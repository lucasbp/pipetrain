use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{
    BLOB_CHUNK, GithubGateway, WORKSPACE_TIMEOUT, branch_name, cleanup, in_ecosystem, repo_of, step,
};
use crate::consumer_bump::apply::{
    Applied, Candidate, DefaultBranchRemote, apply_to_branch, pull_request_title,
};
use crate::consumer_bump::backend::{ConsumerRemote, LocalBackend, ProcessRunner};
use crate::consumer_bump::{Consumer, pin_manifest};
use crate::feed::{Ecosystem, Version};
use crate::github::Client;
use crate::merge::{BumpBehind, PublishedTarget, Status, StepReport, Store};
use crate::model::{Pr, PrKey};
use crate::planner::behind::QueuedBump;

struct Pins {
    tree: Vec<String>,
    changes: Vec<(String, String)>,
}

pub fn is_published_bump(item: &crate::planner::Item, state: &crate::merge::State) -> bool {
    state.bump_behind.iter().any(|entry| {
        entry.publication.is_some()
            && repo_of(&entry.id).eq_ignore_ascii_case(repo_of(&item.id))
            && (entry.number == item.number || entry.branch == item.head)
    })
}

impl Pins {
    fn at(
        client: &mut Client,
        repo: &str,
        sha: &str,
        target: &QueuedBump,
        to: &str,
        expected_from: Option<&str>,
    ) -> Result<Self> {
        let (tree, truncated) = client.tree(repo, sha)?;
        ensure!(!truncated, "incomplete manifest tree at {sha}");
        let paths: Vec<_> = tree
            .iter()
            .filter(|p| in_ecosystem(p, Ecosystem::Nuget))
            .cloned()
            .collect();
        let mut found = false;
        let mut changes = Vec::new();
        for chunk in paths.chunks(BLOB_CHUNK) {
            let blobs = client.blobs(repo, sha, chunk)?;
            ensure!(blobs.len() == chunk.len(), "incomplete manifests at {sha}");
            for (path, text) in blobs {
                let versions = crate::manifest::package_versions(&path, &text, &target.package);
                if versions.is_empty() {
                    continue;
                }
                let patched = pin_manifest(&text, &target.package, to)
                    .with_context(|| format!("unverified pin in {path} at {sha}"))?;
                ensure!(
                    versions.iter().all(|v| eligible_pin(v, expected_from, to)),
                    "unexpected pin in {path} at {sha}"
                );
                found = true;
                if patched != text {
                    changes.extend(
                        versions
                            .into_iter()
                            .filter(|v| v != to)
                            .map(|v| (path.clone(), v)),
                    );
                }
            }
        }
        ensure!(found, "no literal pin of {} at {sha}", target.package);
        Ok(Self { tree, changes })
    }
}

fn eligible_pin(version: &str, expected_from: Option<&str>, to: &str) -> bool {
    use super::{RowStatus, classify};
    version == to
        || (expected_from.is_none_or(|from| version == from)
            && classify(version, to) == RowStatus::Change
            && Version::parse(version).numbers().first() == Version::parse(to).numbers().first())
}

fn check_consumer(client: &mut Client, pr: &Pr) -> Result<()> {
    let repo = pr.key.slug();
    let pull = client.rest(&format!("/repos/{repo}/pulls/{}", pr.key.number))?;
    ensure!(
        pull["state"] == "open"
            && pull["base"]["ref"] == pr.base_ref
            && pull["head"]["ref"] == pr.head_ref
            && pull["head"]["sha"] == pr.head_sha
            && same_repository(&pull, &repo),
        "consumer head, base, or repository changed; replan"
    );
    Ok(())
}

fn same_repository(pull: &Value, repo: &str) -> bool {
    pull["head"]["repo"]["full_name"]
        .as_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(repo))
}

fn verify_existing(
    client: &mut Client,
    key: &PrKey,
    pull: &Value,
    tip: &str,
    target: &QueuedBump,
    to: &str,
) -> Result<()> {
    let repo = key.slug();
    ensure!(
        pull["commits"].as_u64() == Some(1),
        "existing bump PR does not contain exactly one commit"
    );
    let base = pull["base"]["sha"]
        .as_str()
        .context("missing bump base SHA")?;
    let commit = client.rest(&format!("/repos/{repo}/git/commits/{tip}"))?;
    let Some([parent]) = commit["parents"].as_array().map(Vec::as_slice) else {
        bail!("existing bump commit has no unique parent");
    };
    let parent = parent["sha"].as_str().context("missing bump parent SHA")?;
    ensure!(
        client.contains_commit(&repo, base, parent)?,
        "existing bump parent is outside the base branch"
    );
    let files = client.push_files(key, tip)?;
    let manifests: Vec<_> = files
        .into_iter()
        .filter(|path| path.rsplit('/').next() != Some("packages.lock.json"))
        .collect();
    ensure!(
        !manifests.is_empty() && manifests.iter().all(|p| in_ecosystem(p, Ecosystem::Nuget)),
        "existing bump PR has unrelated changes"
    );
    for chunk in manifests.chunks(BLOB_CHUNK) {
        let before = client.blobs(&repo, parent, chunk)?;
        let after: std::collections::BTreeMap<_, _> =
            client.blobs(&repo, tip, chunk)?.into_iter().collect();
        ensure!(
            before.len() == chunk.len() && after.len() == chunk.len(),
            "existing bump changes the manifest layout"
        );
        for (path, text) in before {
            let expected = pin_manifest(&text, &target.package, to)?;
            ensure!(
                expected != text && after.get(&path) == Some(&expected),
                "unverified changes to {path} in existing bump PR"
            );
        }
    }
    ensure!(
        Pins::at(client, &repo, tip, target, to, None)?
            .changes
            .is_empty(),
        "existing bump PR has a different pin"
    );
    Ok(())
}

struct Intent<'a> {
    store: &'a mut Store,
    index: usize,
}

impl<'a> Intent<'a> {
    fn track(store: &'a mut Store, target: &QueuedBump, to: &str) -> Result<Self> {
        let entries = &mut store.state.bump_behind;
        if let Some(index) = entries.iter().position(|e| {
            e.publication.is_some()
                && e.id == target.id
                && e.package.eq_ignore_ascii_case(&target.package)
        }) {
            let entry = &entries[index];
            let scope = entry.publication.as_ref().unwrap();
            ensure!(
                entry.to == to && scope.base == target.base && scope.head == target.head,
                "saved bump targets a different version or branch"
            );
            return Ok(Self { store, index });
        }
        let digest = crate::update::checksum::sha256_hex(
            format!(
                "{}\0{}\0{to}",
                target.base,
                target.package.to_ascii_lowercase()
            )
            .as_bytes(),
        );
        let branch = format!("{}-{}", branch_name(&target.package, to), &digest[..16]);
        let shared = entries.iter().find(|e| {
            e.to == to
                && e.package.eq_ignore_ascii_case(&target.package)
                && repo_of(&e.id).eq_ignore_ascii_case(repo_of(&target.id))
                && e.publication
                    .as_ref()
                    .is_some_and(|p| p.base == target.base)
        });
        let entry = BumpBehind {
            id: target.id.clone(),
            from: target.from.clone(),
            package: target.package.clone(),
            to: to.into(),
            branch: shared.map_or(branch, |e| e.branch.clone()),
            number: shared.map_or(0, |e| e.number),
            pull_request_url: shared.map_or_else(String::new, |e| e.pull_request_url.clone()),
            recorded_at: chrono::Utc::now().to_rfc3339(),
            publication: Some(PublishedTarget {
                base: target.base.clone(),
                head: target.head.clone(),
                commit: shared.and_then(|e| e.publication.as_ref()?.commit.clone()),
                update_requested_at: None,
            }),
        };
        let index = entries.len();
        entries.push(entry);
        store.save()?;
        Ok(Self { store, index })
    }

    fn entry(&self) -> &BumpBehind {
        &self.store.state.bump_behind[self.index]
    }

    fn scope(&mut self) -> &mut PublishedTarget {
        self.store.state.bump_behind[self.index]
            .publication
            .as_mut()
            .expect("publication intent")
    }

    fn remember(&mut self, number: u64, url: &str) -> Result<()> {
        let entry = &mut self.store.state.bump_behind[self.index];
        if entry.number != number || entry.pull_request_url != url {
            entry.number = number;
            entry.pull_request_url = url.into();
            self.store.save()?;
        }
        Ok(())
    }
}

struct Remote<'a, 'b> {
    base: DefaultBranchRemote<'a>,
    intent: &'a mut Intent<'b>,
    pr: &'a Pr,
}

impl ConsumerRemote for Remote<'_, '_> {
    fn record_prepared(&mut self, commit: &str) -> Result<()> {
        self.intent.scope().commit = Some(commit.into());
        self.intent.store.save()
    }

    fn head(&mut self, consumer: &Consumer) -> Result<String> {
        check_consumer(self.base.client, self.pr)?;
        self.base.head(consumer)
    }

    fn reserve_branch(&mut self, repo: &str, branch: &str, sha: &str) -> Result<()> {
        self.base.reserve_branch(repo, branch, sha)
    }

    fn open(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String> {
        let body = format!(
            "{body}\n\nAfter merge, resume the original `pipetrain merge --bump --apply` command with the same selection and state to update the waiting consumer branches."
        );
        self.base.open(consumer, commit, &body)
    }
}

fn open(
    gateway: &mut GithubGateway<'_>,
    intent: &mut Intent<'_>,
    pr: &Pr,
    target: &QueuedBump,
    to: &str,
    base_sha: &str,
    pins: &Pins,
) -> Result<String> {
    let repo = pr.key.slug();
    let branch = intent.entry().branch.clone();
    let tip = gateway.client.branch_tip(&repo, &branch)?;
    let root = gateway.root()?;
    let settings = gateway.cfg.bump.settings_for(
        &repo,
        gateway.options.restore_mode,
        gateway.options.solution.as_deref(),
    );
    let mut remote = Remote {
        base: DefaultBranchRemote {
            client: gateway.client,
            title: pull_request_title(&target.package, to),
        },
        intent,
        pr,
    };
    if let Some(tip) = tip {
        ensure!(
            remote.intent.scope().commit.as_ref() == Some(&tip),
            "existing branch {branch} does not match a durably prepared commit; inspect it before resuming"
        );
        let consumer = Consumer {
            repository: repo,
            number: 0,
            head_branch: pr.base_ref.clone(),
            head_sha: base_sha.into(),
            default_branch: pr.base_ref.clone(),
            bump_branch: branch,
            solution: String::new(),
            manifests: Vec::new(),
        };
        check_consumer(remote.base.client, pr)?;
        let body = format!(
            "Resume the prepared bump of {} to {to}. Review the changes and wait for CI before merging.",
            target.package
        );
        return remote.open(&consumer, &tip, &body);
    }
    let candidate = Candidate {
        repo: &repo,
        default_branch: &pr.base_ref,
        head_sha: base_sha,
        tree: &pins.tree,
        rows: &pins.changes,
    };
    let mut backend =
        LocalBackend::new(root, ProcessRunner, remote, WORKSPACE_TIMEOUT).with_create_only_push();
    let result = apply_to_branch(
        Ecosystem::Nuget,
        &target.package,
        to,
        &candidate,
        &settings,
        &mut backend,
        branch,
    );
    match result {
        Ok(Applied::Opened(opened)) => {
            cleanup(backend.workspace());
            Ok(opened.url)
        }
        Ok(Applied::Skipped(reason)) => {
            cleanup(backend.workspace());
            bail!("{reason}")
        }
        Err(error) => Err(error.context(format!("preparing bump in {:?}", backend.workspace()))),
    }
}

fn incorporate(
    client: &mut Client,
    intent: &mut Intent<'_>,
    pr: &Pr,
    commit: &str,
    pinned: bool,
) -> Result<(bool, String)> {
    let repo = pr.key.slug();
    if client
        .commit_inclusion(&repo, &pr.head_sha, commit)?
        .context("consumer ancestry was not reported")?
    {
        ensure!(
            pinned,
            "consumer includes the bump but its pin has changed; inspect its manifests"
        );
        return Ok((
            true,
            "verified the incorporated pin at the planned head".into(),
        ));
    }
    if intent.scope().update_requested_at.as_deref() == Some(&pr.head_sha) {
        return Ok((
            false,
            "base update already requested for this head; inspect the branch update".into(),
        ));
    }
    check_consumer(client, pr)?;
    intent.scope().update_requested_at = Some(pr.head_sha.clone());
    intent.store.save()?;
    let reply = client.put_json(
        &format!("/repos/{repo}/pulls/{}/update-branch", pr.key.number),
        &json!({"expected_head_sha":pr.head_sha}),
    )?;
    if !(200..300).contains(&reply.status) {
        if (400..500).contains(&reply.status) {
            intent.scope().update_requested_at = None;
            intent.store.save()?;
        }
        bail!(
            "cannot incorporate the base (HTTP {}): {}",
            reply.status,
            reply.message()
        );
    }
    Ok((
        false,
        "requested a branch update; wait for checks on the updated head".into(),
    ))
}

fn reconcile(
    gateway: &mut GithubGateway<'_>,
    intent: &mut Intent<'_>,
    pr: &Pr,
    target: &QueuedBump,
    to: &str,
) -> Result<(bool, String)> {
    check_consumer(gateway.client, pr)?;
    let repo = pr.key.slug();
    let head_pins = Pins::at(
        gateway.client,
        &repo,
        &pr.head_sha,
        target,
        to,
        Some(&target.from),
    )?;
    let pinned = head_pins.changes.is_empty();
    let mut entry = intent.entry().clone();
    let saved_commit = intent.scope().commit.clone();
    let number = if entry.number != 0 {
        Some(entry.number)
    } else {
        let scoped = gateway
            .client
            .bump_pr_from(&repo, &entry.branch, &pr.base_ref)?;
        let native = branch_name(&target.package, to);
        let other =
            if entry.branch != native && gateway.client.branch_tip(&repo, &native)?.is_some() {
                gateway.client.bump_pr_from(&repo, &native, &pr.base_ref)?
            } else {
                None
            };
        if let Some(number) = other {
            ensure!(
                scoped.is_none() && saved_commit.is_none(),
                "multiple bump candidates; inspect both branches"
            );
            entry.branch = native;
            Some(number)
        } else {
            scoped
        }
    };
    let mut merged = None;
    if let Some(number) = number {
        let pull = gateway
            .client
            .rest(&format!("/repos/{repo}/pulls/{number}"))?;
        let url = pull["html_url"].as_str().context("bump PR URL missing")?;
        ensure!(
            pull["head"]["ref"] == entry.branch
                && pull["base"]["ref"] == pr.base_ref
                && same_repository(&pull, &repo),
            "bump PR {url} changed head, base or repository"
        );
        let tip = pull["head"]["sha"]
            .as_str()
            .context("bump head SHA missing")?;
        if let Some(commit) = saved_commit {
            ensure!(
                commit == tip,
                "bump PR {url} no longer has the prepared commit"
            );
        } else {
            let key = PrKey {
                number,
                ..pr.key.clone()
            };
            verify_existing(gateway.client, &key, &pull, tip, target, to)
                .with_context(|| format!("cannot reconcile bump PR {url}"))?;
            intent.scope().commit = Some(tip.to_owned());
            intent.store.state.bump_behind[intent.index]
                .branch
                .clone_from(&entry.branch);
        }
        intent.remember(number, url)?;
        if pull["merged"].as_bool() == Some(true) {
            merged = Some(
                pull["merge_commit_sha"]
                    .as_str()
                    .context("merged bump PR lacks its merge SHA")?
                    .to_owned(),
            );
        } else {
            ensure!(
                pull["state"] == "open",
                "bump PR closed without merging; inspect it"
            );
            return Ok((false, "waiting for the bump PR to merge".into()));
        }
    }
    let base_sha = gateway
        .client
        .branch_tip(&repo, &pr.base_ref)?
        .context("consumer base no longer exists")?;
    let base = Pins::at(gateway.client, &repo, &base_sha, target, to, None)?;
    if let Some(commit) = merged {
        ensure!(
            base.changes.is_empty() && gateway.client.contains_commit(&repo, &base_sha, &commit)?,
            "current base lacks the merged bump or the exact pin"
        );
        return incorporate(gateway.client, intent, pr, &commit, pinned);
    }
    if base.changes.is_empty() {
        return incorporate(gateway.client, intent, pr, &base_sha, pinned);
    }
    ensure!(
        !pinned && target.sha == pr.head_sha,
        "consumer head or pin changed before bump preparation; inspect the new head"
    );
    let url = open(gateway, intent, pr, target, to, &base_sha, &base)?;
    let number = url
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .context("bump PR URL lacks its number")?;
    intent.remember(number, &url)?;
    Ok((false, "waiting for the bump PR to merge".into()))
}

pub fn published(
    target: &QueuedBump,
    evidence: &crate::publication::PublicationEvidence,
    pr: &Pr,
    store: &mut Store,
    gateway: &mut GithubGateway<'_>,
) -> (StepReport, bool) {
    let to = evidence.version();
    let mut url = String::new();
    let result = (|| {
        ensure!(
            evidence.package().eq_ignore_ascii_case(&target.package)
                && matches!(target.owners.as_slice(), [owner] if owner.eq_ignore_ascii_case(evidence.repository())),
            "publication does not match the selected package and publisher"
        );
        ensure!(
            pr.key.to_string().eq_ignore_ascii_case(&target.id)
                && pr.base_ref == target.base
                && pr.head_ref == target.head,
            "consumer changed repository or branch since publication was planned"
        );
        let mut intent = Intent::track(store, target, to)?;
        let result = reconcile(gateway, &mut intent, pr, target, to);
        url.clone_from(&intent.entry().pull_request_url);
        result
    })();
    let (ready, reason) = result.unwrap_or_else(|e| (false, format!("{e:#}")));
    let link = if url.is_empty() {
        String::new()
    } else {
        format!(" ({url})")
    };
    let mut report = step(
        &target.id,
        if ready { Status::Done } else { Status::Stopped },
        format!("{reason}{link}"),
    );
    let mut outcome = json!({"outcome":if ready {"incorporated"} else {"attention"}, "package":target.package, "version":to, "base":target.base});
    if !url.is_empty() {
        outcome["pull_request_url"] = url.into();
    }
    report.bump = Some(outcome);
    (report, ready)
}

#[cfg(test)]
mod tests {
    use super::eligible_pin;

    #[test]
    fn consumer_requires_its_observed_pin_and_base_accepts_other_exact_pins_in_the_major() {
        assert!(eligible_pin("1.1.0", Some("1.1.0"), "1.4.0"));
        assert!(!eligible_pin("1.0.0", Some("1.1.0"), "1.4.0"));
        assert!(eligible_pin("1.0.0", None, "1.4.0"));
        for unexpected in ["1.*", "0.9.0", "1.5.0", "2.0.0"] {
            assert!(!eligible_pin(unexpected, None, "1.4.0"));
        }
    }
}
