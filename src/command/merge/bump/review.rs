use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::command::merge::queued::readiness_after_bump;
use crate::config::Config;
use crate::consumer_bump::Consumer;
use crate::consumer_bump::backend::{ConsumerRemote, GithubRemote};
use crate::consumer_bump::journal::{Preparation, Prepared};
use crate::github::Client;
use crate::merge::{BumpWait, Kind, Status, StepReport, Store};
use crate::model::{PrKey, PrState};
use crate::planner::Item;

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) struct RecordingRemote<'a, 'b> {
    pub remote: GithubRemote<'a>,
    pub record: &'b mut dyn FnMut(&str) -> Result<()>,
    pub base: &'b str,
}

impl ConsumerRemote for RecordingRemote<'_, '_> {
    fn record_prepared(&mut self, commit: &str) -> Result<()> {
        (self.record)(commit)
    }
    fn head(&mut self, consumer: &Consumer) -> Result<String> {
        self.remote.head_on(consumer, Some(self.base))
    }
    fn reserve_branch(&mut self, repo: &str, branch: &str, source: &str) -> Result<()> {
        self.remote.reserve_branch(repo, branch, source)
    }
    fn open(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String> {
        self.remote.open(consumer, commit, body)
    }
}

fn key(marker: &Preparation) -> Option<PrKey> {
    let prepared = marker.prepared.as_ref()?;
    let key = PrKey::parse(&prepared.pull_request_url)?;
    (key.number != 0
        && key.slug().eq_ignore_ascii_case(&marker.repository)
        && valid_sha(&prepared.commit))
    .then_some(key)
}

fn wait(marker: &Preparation) -> Option<BumpWait> {
    Some(BumpWait::Draft {
        key: key(marker)?,
        base: marker.head_branch.clone(),
        head: marker.bump_branch.clone(),
        sha: marker.prepared.as_ref()?.commit.clone(),
    })
}

pub(in crate::command::merge) fn draft_wait(item: &Item, store: &Store) -> Option<BumpWait> {
    wait(store.state.bump.as_ref()?.consumers.get(&item.id)?)
}

pub(in crate::command::merge) fn is_review_draft(item: &Item, store: &Store) -> bool {
    let repo = format!("{}/{}", item.org, item.repo);
    store
        .state
        .bump
        .iter()
        .flat_map(|journal| journal.consumers.values())
        .any(|marker| {
            marker.repository.eq_ignore_ascii_case(&repo)
                && (item.head == marker.bump_branch
                    || key(marker).is_some_and(|key| key.number == item.number))
        })
}

fn scoped(marker: &Preparation, pr: &PrKey, base: &str, head: &str) -> Result<()> {
    ensure!(
        marker.repository.eq_ignore_ascii_case(&pr.slug())
            && marker.number == pr.number
            && marker.head_branch == head
            && marker
                .base_branch
                .as_deref()
                .is_none_or(|expected| expected == base),
        "consumer identity or target branch changed since bump preparation"
    );
    Ok(())
}

pub(super) fn current(
    client: &mut Client,
    key: &PrKey,
    base: &str,
    head: &str,
    sha: &str,
) -> Result<()> {
    let repo = key.slug();
    let pull = client.rest(&format!("/repos/{repo}/pulls/{}", key.number))?;
    ensure!(
        pull["state"] == "open"
            && pull["number"] == key.number
            && pull["base"]["ref"] == base
            && pull["head"]["ref"] == head
            && pull["head"]["sha"] == sha
            && ["base", "head"]
                .iter()
                .all(|side| pull[side]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|found| found.eq_ignore_ascii_case(&repo))),
        "consumer PR head changed or target branch is no longer authorized"
    );
    Ok(())
}

fn checked_draft(pull: &Value, marker: &Preparation, commit: &str, number: u64) -> Result<()> {
    ensure!(
        pull["number"] == number
            && pull["base"]["ref"] == marker.head_branch
            && pull["head"]["ref"] == marker.bump_branch
            && pull["head"]["sha"] == commit
            && ["head", "base"]
                .iter()
                .all(|side| pull[side]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&marker.repository))),
        "bump PR #{number} changed its repository, branch or prepared commit"
    );
    Ok(())
}

pub(super) fn recover(client: &mut Client, marker: &Preparation) -> Result<Option<Prepared>> {
    let Some(commit) = marker
        .prepared_commit
        .as_deref()
        .filter(|sha| valid_sha(sha))
    else {
        return Ok(None);
    };
    let Some(number) =
        client.bump_pr_from(&marker.repository, &marker.bump_branch, &marker.head_branch)?
    else {
        return Ok(None);
    };
    let pull = client.rest(&format!("/repos/{}/pulls/{number}", marker.repository))?;
    checked_draft(&pull, marker, commit, number)?;
    let pull_request_url = pull["html_url"]
        .as_str()
        .context("recovered bump PR has no URL")?
        .to_owned();
    ensure!(
        PrKey::parse(&pull_request_url).is_some_and(
            |key| key.number == number && key.slug().eq_ignore_ascii_case(&marker.repository)
        ),
        "recovered bump PR URL does not match its repository and number"
    );
    Ok(Some(Prepared {
        commit: commit.into(),
        pull_request_url,
    }))
}

pub(super) fn incorporated(
    client: &mut Client,
    marker: &Preparation,
    consumer: &PrKey,
    base: &str,
    head: &str,
    sha: &str,
) -> Result<bool> {
    scoped(marker, consumer, base, head)?;
    let draft = key(marker).context("invalid saved bump PR identity")?;
    let prepared = marker.prepared.as_ref().context("missing saved bump PR")?;
    let pull = client.rest(&format!("/repos/{}/pulls/{}", draft.slug(), draft.number))?;
    checked_draft(&pull, marker, &prepared.commit, draft.number)?;
    current(client, consumer, base, head, sha)?;
    match (pull["state"].as_str(), pull["merged"].as_bool()) {
        (Some("open"), Some(false)) => {
            ensure!(
                sha == marker.head_sha,
                "consumer head moved before the bump PR was merged"
            );
            Ok(false)
        }
        (Some("closed"), Some(true)) => {
            let merge = pull["merge_commit_sha"]
                .as_str()
                .filter(|sha| valid_sha(sha))
                .context("merged bump has no valid merge commit")?;
            ensure!(
                sha == merge,
                "consumer head differs from the exact merge commit of bump PR {}",
                prepared.pull_request_url
            );
            Ok(true)
        }
        (Some("closed"), Some(false)) => bail!(
            "bump PR {} was closed without merge",
            prepared.pull_request_url
        ),
        _ => bail!(
            "GitHub did not confirm the state of bump PR {}",
            prepared.pull_request_url
        ),
    }
}

pub(super) fn confirm_pins(
    client: &mut Client,
    repo: &str,
    sha: &str,
    pins: &BTreeMap<String, String>,
) -> Result<()> {
    ensure!(
        !pins.is_empty(),
        "saved bump has no verified package versions"
    );
    let (tree, truncated) = client.tree(repo, sha)?;
    ensure!(!truncated, "consumer manifest tree is incomplete at {sha}");
    let paths: Vec<_> = tree
        .into_iter()
        .filter(|path| path.ends_with(".csproj") || path.ends_with(".props"))
        .collect();
    ensure!(
        paths.len() <= 500,
        "too many manifests for consumer pin verification"
    );
    let mut found = std::collections::BTreeSet::new();
    for chunk in paths.chunks(25) {
        let blobs = client.blobs(repo, sha, chunk)?;
        ensure!(
            blobs.len() == chunk.len(),
            "consumer manifests unavailable at {sha}"
        );
        for (path, text) in blobs {
            for (package, version) in pins {
                if crate::manifest::package_versions(&path, &text, package).is_empty() {
                    continue;
                }
                ensure!(
                    crate::consumer_bump::pin_manifest(&text, package, version)? == text,
                    "consumer no longer pins {package} {version} in {path} at {sha}"
                );
                found.insert(package);
            }
        }
    }
    ensure!(
        found.len() == pins.len(),
        "consumer is missing a reviewed package pin at {sha}"
    );
    Ok(())
}

pub(in crate::command::merge) fn waiting_consumer(
    id: &str,
    store: &Store,
    client: &mut Client,
    cfg: &Config,
) -> Result<Option<StepReport>> {
    let Some(marker) = store
        .state
        .bump
        .as_ref()
        .and_then(|journal| journal.consumers.get(id))
    else {
        return Ok(None);
    };
    let Some(prepared) = &marker.prepared else {
        return Ok(None);
    };
    let key = PrKey::parse(id).context("invalid saved consumer identity")?;
    let (pr, checks) = client
        .pr_with_check_head(&key)?
        .context("saved consumer PR was not found")?;
    if pr.state == PrState::Merged {
        return Ok(None);
    }
    let mut step = StepReport::new(Kind::Bump, id, Status::Stopped, "");
    step.bump = Some(
        json!({"outcome":"needs_review", "pull_request_url":prepared.pull_request_url, "branch":marker.bump_branch,"consumer_head":pr.head_sha}),
    );
    let checked = (|| -> Result<(Option<BumpWait>, String)> {
        ensure!(pr.state == PrState::Open, "consumer PR is closed");
        if !incorporated(
            client,
            marker,
            &key,
            &pr.base_ref,
            &pr.head_ref,
            &pr.head_sha,
        )? {
            return Ok((
                wait(marker),
                format!("waiting for human merge of {}", prepared.pull_request_url),
            ));
        }
        let pins = marker_pins(marker)?;
        confirm_pins(client, &key.slug(), &pr.head_sha, &pins)?;
        let reason = match readiness_after_bump(&pr, checks.as_deref())? {
            Some(reason) => reason,
            None => {
                let blockers = cfg.blockers(&pr);
                ensure!(
                    blockers.is_empty(),
                    "consumer is blocked: {}",
                    blockers.join(", ")
                );
                "consumer became ready; revalidate the queue".into()
            }
        };
        Ok((
            Some(BumpWait::Readiness {
                key,
                sha: pr.head_sha,
            }),
            reason,
        ))
    })();
    match checked {
        Ok((wait, reason)) => {
            step.wait = wait;
            step.detail = reason;
        }
        Err(error) => {
            step.detail = format!(
                "consumer bump {} requires attention: {error:#}",
                prepared.pull_request_url
            );
            step.bump = Some(
                json!({"outcome":"attention", "reason":step.detail, "pull_request_url":prepared.pull_request_url}),
            );
        }
    }
    Ok(Some(step))
}

pub(super) fn marker_pins(marker: &Preparation) -> Result<BTreeMap<String, String>> {
    if !marker.pins.is_empty() {
        return Ok(marker.pins.clone());
    }
    let snapshot: Value = serde_json::from_str(&marker.publication_fingerprint)?;
    let id = format!("{}#{}", marker.repository, marker.number);
    let mut pins = BTreeMap::new();
    for contract in snapshot[1][&id]
        .as_array()
        .into_iter()
        .flatten()
        .chain(snapshot[2].as_array().into_iter().flatten())
    {
        let Some(package) = contract["package"].as_str().map(str::to_ascii_lowercase) else {
            continue;
        };
        let publisher = contract["publisher_id"]
            .as_str()
            .or(contract["id"].as_str())
            .context("saved package has no publisher")?;
        for entry in snapshot[0][publisher]["publications"][&package]
            .as_array()
            .into_iter()
            .flatten()
        {
            let version = entry["version"]
                .as_str()
                .context("saved publication has no version")?;
            ensure!(
                pins.insert(package.clone(), version.to_owned())
                    .is_none_or(|old| old == version),
                "ambiguous saved publication of {package}"
            );
        }
    }
    ensure!(
        !pins.is_empty(),
        "saved bump has no verified package versions"
    );
    Ok(pins)
}
