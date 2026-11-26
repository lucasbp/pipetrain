use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::consumer_bump::backend::{ConsumerRemote, GithubRemote, LocalBackend, ProcessRunner};
use crate::consumer_bump::journal::{
    Contract, Journal, Preparation, Prepared, PublicationInput, Publisher,
};
use crate::consumer_bump::{self, Consumer, PrepareResult};
use crate::github::Client;
use crate::merge::{Store, StoreOperation};
use crate::plan::Plan;
use crate::planner::Item;
use crate::publication::backend::{NugetFeed, collect_run};
use crate::publication::{self, PublicationResult, PublisherExecution};

pub mod queued;
mod review;
pub(super) use review::{draft_wait, is_review_draft, waiting_consumer};

pub const DEFAULT_PUSH_STEP: &str = "Push to GitHub Packages";

#[derive(Debug, Clone)]
pub struct Options {
    pub push_step: String,
    pub behind: super::behind::Options,
    pub workspace: Option<PathBuf>,
    pub timeout: Duration,
    pub logs_api_base: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            push_step: DEFAULT_PUSH_STEP.into(),
            behind: Default::default(),
            workspace: None,
            timeout: Duration::from_secs(600),
            logs_api_base: crate::github::debug_loopback_env("PIPETRAIN_API_URL")
                .unwrap_or_else(|| "https://api.github.com".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Skipped {
        reason: String,
    },
    NeedsReview {
        pull_request_url: String,
        branch: String,
        consumer_head: String,
    },
    Attention {
        reason: String,
    },
}

pub trait Gateway {
    fn publication(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
    ) -> Result<Vec<PublicationResult>>;
    fn consumer(&mut self, item: &Item, packages: &[String]) -> Result<Consumer>;
    fn prepare(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult>;
    fn incorporated(&mut self, preparation: &Preparation, item: &Item) -> Result<bool>;
    fn confirm_pins(&mut self, item: &Item, pins: &BTreeMap<String, String>) -> Result<()>;
    fn recover(&mut self, _marker: &Preparation) -> Result<Option<Prepared>> {
        Ok(None)
    }
    fn prepare_recording(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
        _record: &mut dyn FnMut(&str) -> Result<()>,
        _base: &str,
    ) -> Result<PrepareResult> {
        self.prepare(publications, consumer)
    }
}

pub fn applicable(item: &Item, journal: Option<&Journal>) -> bool {
    item.after
        .iter()
        .any(|dependency| dependency.package.is_some())
        || journal.is_some_and(|journal| {
            journal.consumers.contains_key(&item.id) || journal.contracts.contains_key(&item.id)
        })
}

pub fn track(plan: &Plan, store: &mut Store, options: &Options, operation: &StoreOperation) {
    let journal = store.state.bump.get_or_insert_with(Journal::default);
    match operation {
        StoreOperation::Add(pending) => {
            let ready = plan
                .items
                .iter()
                .filter(|item| !item.hidden)
                .map(|item| (&item.id, &item.after));
            let held = plan
                .skipped
                .iter()
                .filter(|entry| !entry.hidden && !entry.blocked_by.is_empty())
                .filter_map(|entry| Some((&entry.id, plan.selected_dependencies.get(&entry.id)?)));
            let mut packages = BTreeSet::new();
            for (id, dependencies) in ready.chain(held) {
                for package in dependencies
                    .iter()
                    .filter(|edge| edge.id == pending.id)
                    .filter_map(|edge| edge.package.as_ref())
                {
                    packages.insert(package.clone());
                    let known = journal.contracts.entry(id.clone()).or_default();
                    if !known.iter().any(|contract| {
                        contract.publisher_id == pending.id
                            && contract.package.eq_ignore_ascii_case(package)
                    }) {
                        known.push(Contract {
                            publisher_id: pending.id.clone(),
                            package: package.clone(),
                        });
                    }
                }
            }
            for contract in journal
                .queued_bumps
                .iter()
                .filter(|contract| contract.publishers.contains(&pending.id))
            {
                packages.insert(contract.package.clone());
            }
            if !packages.is_empty()
                && let Some(publisher) = crate::model::PrKey::parse(&pending.id)
                && publisher.slug().eq_ignore_ascii_case(&pending.repo)
                && !plan.is_hidden(&pending.id)
            {
                let recorded = journal
                    .publishers
                    .entry(pending.id.clone())
                    .or_insert_with(|| Publisher {
                        repository: pending.repo.clone(),
                        org: publisher.org,
                        merge_sha: None,
                        push_step: options.push_step.clone(),
                        packages: Vec::new(),
                        deploy_confirmed: false,
                        publications: Default::default(),
                    });
                if recorded.merge_sha.is_none() {
                    packages.extend(recorded.packages.iter().cloned());
                    recorded.packages = packages.into_iter().collect();
                }
            }
        }
        StoreOperation::SetSha { id, sha, .. } => {
            journal.queued_bumps.retain(|contract| &contract.id != id);
            store
                .state
                .bump_behind
                .retain(|entry| &entry.id != id || entry.publication.is_none());
            if let Some(publisher) = journal.publishers.get_mut(id) {
                publisher.merge_sha = Some(sha.clone());
            }
        }
        StoreOperation::Remove(id) => {
            if let Some(publisher) = journal.publishers.get_mut(id)
                && publisher.merge_sha.is_some()
            {
                publisher.deploy_confirmed = true;
            }
        }
        _ => {}
    }
}

pub fn prepare_with(
    item: &Item,
    journal: &mut Journal,
    gateway: &mut impl Gateway,
    workspace_root: &str,
    mut save: impl FnMut(&Journal) -> Result<()>,
) -> Outcome {
    match prepare_checked(item, journal, gateway, workspace_root, &mut save) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Attention {
            reason: format!("consumer bump requires attention: {error:#}"),
        },
    }
}

fn prepare_checked(
    item: &Item,
    journal: &mut Journal,
    gateway: &mut impl Gateway,
    workspace_root: &str,
    save: &mut impl FnMut(&Journal) -> Result<()>,
) -> Result<Outcome> {
    let fingerprint =
        serde_json::to_string(&(&journal.publishers, &journal.contracts, &item.after))?;
    if let Some(marker) = journal.consumers.get_mut(&item.id)
        && !marker.completed
        && marker.prepared.is_none()
        && let Some(prepared) = gateway.recover(marker)?
    {
        marker.prepared = Some(prepared);
        marker.completed = true;
        save(journal)?;
    }
    let mut reviewed = None;
    if let Some(marker) = journal.consumers.get(&item.id) {
        if marker.head_branch != item.head
            || marker
                .base_branch
                .as_deref()
                .is_some_and(|base| base != item.base)
        {
            bail!("consumer branch or target base changed after bump preparation")
        }
        if marker.verified_head.as_ref() == Some(&item.sha)
            && marker.publication_fingerprint == fingerprint
        {
            return Ok(Outcome::Skipped {
                reason:
                    "consumer already incorporates the reviewed bump and pins its verified versions"
                        .into(),
            });
        }
        if let Some(prepared) = &marker.prepared {
            if !gateway.incorporated(marker, item)? {
                return Ok(Outcome::NeedsReview {
                    pull_request_url: prepared.pull_request_url.clone(),
                    branch: marker.bump_branch.clone(),
                    consumer_head: marker.head_sha.clone(),
                });
            }
            let pins = review::marker_pins(marker)?;
            gateway.confirm_pins(item, &pins)?;
            reviewed = Some(pins);
        }
        if !marker.completed {
            bail!(
                "a prior bump preparation may have remote effects on branch {}; inspect it and workspace {} before retrying",
                marker.bump_branch,
                marker.workspace_root
            );
        }
        if marker.prepared.is_none()
            && marker.head_sha == item.sha
            && marker.publication_fingerprint == fingerprint
        {
            return Ok(Outcome::Skipped {
                reason: "consumer already pins the verified published version".into(),
            });
        }
    }
    let mut results = Vec::new();
    let mut checked = BTreeSet::new();
    let mut historical = std::collections::BTreeMap::new();
    for contract in journal.contracts.get(&item.id).into_iter().flatten() {
        historical.insert(contract.package.to_ascii_lowercase(), contract.clone());
    }
    let current: Vec<_> = item
        .after
        .iter()
        .filter_map(|dependency| {
            dependency.package.as_ref().map(|package| Contract {
                publisher_id: dependency.id.clone(),
                package: package.clone(),
            })
        })
        .collect();
    for contract in &current {
        historical.remove(&contract.package.to_ascii_lowercase());
    }
    let contracts: Vec<_> = historical.into_values().chain(current).collect();
    for dependency in &contracts {
        let package = &dependency.package;
        if !checked.insert((
            dependency.publisher_id.clone(),
            package.to_ascii_lowercase(),
        )) {
            continue;
        }
        let Some(publisher) = journal.publishers.get(&dependency.publisher_id).cloned() else {
            continue;
        };
        if !publisher
            .packages
            .iter()
            .any(|known| known.eq_ignore_ascii_case(package))
        {
            continue;
        }
        if !publisher.deploy_confirmed {
            bail!(
                "recorded publisher deploy is not confirmed; resume it before preparing consumers"
            )
        }
        let known = publisher.publications.get(&package.to_ascii_lowercase());
        let publications = gateway.publication(&publisher, package, known.map(Vec::as_slice))?;
        let inputs = publication_inputs(&publications);
        journal
            .publishers
            .get_mut(&dependency.publisher_id)
            .expect("known publisher")
            .publications
            .insert(package.to_ascii_lowercase(), inputs);
        save(journal)?;
        results.extend(publications);
    }
    let published = consumer_bump::published_packages(&results)?;
    let pins: BTreeMap<String, String> = published
        .iter()
        .map(|(name, evidence)| (name.clone(), evidence.version().to_owned()))
        .collect();
    if reviewed.as_ref() == Some(&pins) {
        let marker = journal
            .consumers
            .get_mut(&item.id)
            .expect("incorporated bump");
        marker.verified_head = Some(item.sha.clone());
        marker.publication_fingerprint =
            serde_json::to_string(&(&journal.publishers, &journal.contracts, &item.after))?;
        save(journal)?;
        return Ok(Outcome::Skipped {
            reason: "consumer incorporates the reviewed bump and pins its verified versions".into(),
        });
    }
    if reviewed.is_some() && pins.is_empty() {
        bail!("recorded publication no longer proves the incorporated consumer bump");
    }
    if pins.is_empty() {
        return Ok(Outcome::Skipped {
            reason: "publisher did not produce a new package in this execution".into(),
        });
    }
    let packages: Vec<_> = published
        .values()
        .map(|evidence| evidence.package().to_owned())
        .collect();
    let consumer = gateway.consumer(item, &packages)?;
    journal.consumers.insert(
        item.id.clone(),
        Preparation {
            repository: consumer.repository.clone(),
            number: consumer.number,
            head_branch: consumer.head_branch.clone(),
            head_sha: consumer.head_sha.clone(),
            base_branch: Some(item.base.clone()),
            pins,
            verified_head: None,
            prepared_commit: None,
            bump_branch: consumer.bump_branch.clone(),
            workspace_root: workspace_root.into(),
            completed: false,
            publication_fingerprint: serde_json::to_string(&(
                &journal.publishers,
                &journal.contracts,
                &item.after,
            ))?,
            prepared: None,
        },
    );
    save(journal)?;
    let prepared = gateway.prepare_recording(
        &results,
        &consumer,
        &mut |commit| {
            journal
                .consumers
                .get_mut(&item.id)
                .expect("saved consumer")
                .prepared_commit = Some(commit.to_owned());
            save(journal)
        },
        &item.base,
    )?;
    let marker = journal
        .consumers
        .get_mut(&item.id)
        .expect("saved preparation");
    marker.completed = true;
    let outcome = match prepared {
        PrepareResult::Skipped { reason } => Outcome::Skipped { reason },
        PrepareResult::Prepared { bump } => {
            marker.prepared = Some(Prepared {
                commit: bump.commit.clone(),
                pull_request_url: bump.pull_request_url.clone(),
            });
            Outcome::NeedsReview {
                pull_request_url: bump.pull_request_url,
                branch: bump.branch,
                consumer_head: bump.consumer_head,
            }
        }
    };
    save(journal)?;
    Ok(outcome)
}

pub fn prepare(item: &Item, client: &mut Client, options: &Options, store: &mut Store) -> Outcome {
    let root = match options
        .workspace
        .clone()
        .or_else(|| crate::cache::default_dir().map(|root| root.join("bump-workspaces")))
    {
        Some(root) => root,
        None => {
            return Outcome::Attention {
                reason: "no safe cache directory for isolated bump workspaces".into(),
            };
        }
    };
    let mut journal = store.state.bump.clone().unwrap_or_default();
    let mut gateway = GithubGateway { client, options };
    prepare_with(
        item,
        &mut journal,
        &mut gateway,
        &root.to_string_lossy(),
        |journal| {
            store.state.bump = Some(journal.clone());
            store.save()
        },
    )
}

pub struct GithubGateway<'a> {
    pub client: &'a mut Client,
    pub options: &'a Options,
}

fn publication_inputs(results: &[PublicationResult]) -> Vec<PublicationInput> {
    results
        .iter()
        .filter_map(|result| match result {
            PublicationResult::Published { evidence } => Some(PublicationInput {
                run_id: evidence.run_id(),
                version: evidence.version().into(),
            }),
            PublicationResult::Skipped { .. } => None,
        })
        .collect()
}

impl GithubGateway<'_> {
    fn verified_runs(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
        queued: bool,
    ) -> Result<Vec<PublicationResult>> {
        let sha = publisher
            .merge_sha
            .as_deref()
            .context("publisher has no confirmed merge SHA")?;
        let run_ids = if let Some(known) = known.filter(|_| !queued) {
            known.iter().map(|input| input.run_id).collect::<Vec<_>>()
        } else {
            let body = self.client.rest(&format!(
                "/repos/{}/actions/runs?head_sha={sha}&event=push&per_page=100",
                publisher.repository
            ))?;
            let total = body["total_count"]
                .as_u64()
                .context("missing publisher run count")?;
            let runs = body["workflow_runs"]
                .as_array()
                .context("missing publisher workflow runs")?;
            if total != runs.len() as u64 || total > 100 {
                bail!("publisher workflow run list is truncated")
            }
            if queued
                && runs.iter().any(|run| {
                    run["status"] != "completed"
                        || !matches!(run["conclusion"].as_str(), Some("success" | "skipped"))
                })
            {
                bail!("publisher workflow has not completed successfully for the merged SHA")
            }
            if known.is_none_or(|inputs| inputs.is_empty())
                && !runs.is_empty()
                && runs
                    .iter()
                    .all(|run| run["status"] == "completed" && run["conclusion"] == "skipped")
            {
                return Ok(vec![PublicationResult::Skipped {
                    reason: "publisher workflows were all skipped".into(),
                }]);
            }
            runs.iter()
                .filter(|run| run["status"] == "completed" && run["conclusion"] == "success")
                .map(|run| run["id"].as_u64().context("missing publisher run ID"))
                .collect::<Result<Vec<_>>>()?
        };
        let mut results = Vec::new();
        for run_id in run_ids.into_iter().collect::<BTreeSet<_>>() {
            let execution = PublisherExecution {
                repository: publisher.repository.clone(),
                merge_sha: sha.into(),
                run_id,
                published_this_execution: true,
            };
            let run = collect_run(
                self.client,
                &execution,
                &publisher.push_step,
                &self.options.logs_api_base,
            )?;
            let mut feed = NugetFeed {
                org: publisher.org.clone(),
                registry: None,
            };
            let result = if queued {
                publication::verify_pushes(&execution, &run, package, |version, destinations| {
                    feed.contains_pushes(package, version, destinations)
                })?
            } else {
                publication::verify(&execution, &run, package, &mut feed)?
            };
            results.push(result);
        }
        if let Some(known) = known {
            let observed = publication_inputs(&results);
            if !known.iter().all(|input| observed.contains(input)) {
                bail!(
                    "saved publication inputs no longer verify against the workflow and exact feed"
                )
            }
        }
        if results.is_empty() && known.is_none() {
            bail!("no successful publisher workflow can prove publication for the merged SHA")
        }
        Ok(results)
    }
}

impl Gateway for GithubGateway<'_> {
    fn publication(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
    ) -> Result<Vec<PublicationResult>> {
        self.verified_runs(publisher, package, known, false)
    }

    fn consumer(&mut self, item: &Item, packages: &[String]) -> Result<Consumer> {
        let repository = format!("{}/{}", item.org, item.repo);
        let key = crate::model::PrKey::new(&item.org, &item.repo, item.number);
        review::current(self.client, &key, &item.base, &item.head, &item.sha)?;
        if self.client.open_pr_into(&repository, &item.head)?.is_some() {
            bail!(
                "an open PR already targets the consumer head; review it before preparing another bump"
            );
        }
        let metadata = self.client.rest(&format!("/repos/{repository}"))?;
        let default_branch = metadata["default_branch"]
            .as_str()
            .context("missing consumer default branch")?
            .to_string();
        let (paths, truncated) = self.client.tree(&repository, &item.sha)?;
        if truncated {
            bail!("consumer tree is truncated; select its solution manually")
        }
        let solutions: Vec<_> = paths
            .iter()
            .filter(|path| path.ends_with(".sln") || path.ends_with(".slnx"))
            .cloned()
            .collect();
        if solutions.len() != 1 {
            bail!("consumer must have exactly one solution at its planned head")
        }
        let manifests: Vec<_> = paths
            .iter()
            .filter(|path| path.ends_with(".csproj") || path.ends_with(".props"))
            .cloned()
            .collect();
        if manifests.len() > 500 {
            bail!("consumer has too many manifests for automatic preparation")
        }
        let mut selected = Vec::new();
        for chunk in manifests.chunks(25) {
            let blobs = self.client.blobs(&repository, &item.sha, chunk)?;
            if blobs.len() != chunk.len() {
                bail!("consumer manifests are unavailable at the planned head")
            }
            for (path, text) in blobs {
                if packages.iter().any(|package| {
                    !crate::manifest::package_versions(&path, &text, package).is_empty()
                }) {
                    selected.push(path)
                }
            }
        }
        if selected.is_empty() {
            bail!("consumer has no supported literal pins for the published packages")
        }
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Consumer {
            repository,
            number: item.number,
            head_branch: item.head.clone(),
            head_sha: item.sha.clone(),
            default_branch,
            bump_branch: format!(
                "pipetrain/bump/{}-{}-{stamp}",
                item.number,
                item.sha.get(..12).context("invalid consumer head")?
            ),
            solution: solutions[0].clone(),
            manifests: selected,
        })
    }

    fn incorporated(&mut self, preparation: &Preparation, item: &Item) -> Result<bool> {
        let key = crate::model::PrKey::new(&item.org, &item.repo, item.number);
        review::incorporated(
            self.client,
            preparation,
            &key,
            &item.base,
            &item.head,
            &item.sha,
        )
    }

    fn confirm_pins(&mut self, item: &Item, pins: &BTreeMap<String, String>) -> Result<()> {
        review::confirm_pins(
            self.client,
            &format!("{}/{}", item.org, item.repo),
            &item.sha,
            pins,
        )
    }

    fn recover(&mut self, marker: &Preparation) -> Result<Option<Prepared>> {
        review::recover(self.client, marker)
    }

    fn prepare_recording(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
        record: &mut dyn FnMut(&str) -> Result<()>,
        base: &str,
    ) -> Result<PrepareResult> {
        let remote = review::RecordingRemote {
            remote: GithubRemote {
                client: self.client,
            },
            record,
            base,
        };
        self.options.prepare(remote, publications, consumer)
    }

    fn prepare(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult> {
        self.options.prepare(
            GithubRemote {
                client: self.client,
            },
            publications,
            consumer,
        )
    }
}

impl Options {
    fn prepare(
        &self,
        remote: impl ConsumerRemote,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult> {
        let root = self
            .workspace
            .clone()
            .or_else(|| crate::cache::default_dir().map(|root| root.join("bump-workspaces")))
            .context("no safe cache directory for isolated bump workspaces")?;
        let mut backend = LocalBackend::new(root, ProcessRunner, remote, self.timeout);
        consumer_bump::prepare_many(publications, consumer, &mut backend)
    }
}
