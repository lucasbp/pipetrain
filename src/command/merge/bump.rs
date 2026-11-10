use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::consumer_bump::backend::{GithubRemote, LocalBackend, ProcessRunner};
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

pub const DEFAULT_PUSH_STEP: &str = "Push to GitHub Packages";

#[derive(Debug, Clone)]
pub struct Options {
    pub push_step: String,
    pub workspace: Option<PathBuf>,
    pub timeout: Duration,
    pub logs_api_base: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            push_step: DEFAULT_PUSH_STEP.into(),
            workspace: None,
            timeout: Duration::from_secs(600),
            logs_api_base: "https://api.github.com".into(),
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
            let packages: BTreeSet<_> = plan
                .items
                .iter()
                .flat_map(|item| &item.after)
                .filter(|dependency| dependency.id == pending.id)
                .filter_map(|dependency| dependency.package.clone())
                .collect();
            if !packages.is_empty() {
                for consumer in &plan.items {
                    let contracts: Vec<_> = consumer
                        .after
                        .iter()
                        .filter(|dependency| dependency.id == pending.id)
                        .filter_map(|dependency| {
                            dependency.package.as_ref().map(|package| Contract {
                                publisher_id: pending.id.clone(),
                                package: package.clone(),
                            })
                        })
                        .collect();
                    if contracts.is_empty() {
                        continue;
                    }
                    let known = journal.contracts.entry(consumer.id.clone()).or_default();
                    for contract in contracts {
                        if !known.iter().any(|found| {
                            found.publisher_id == contract.publisher_id
                                && found.package.eq_ignore_ascii_case(&contract.package)
                        }) {
                            known.push(contract);
                        }
                    }
                }
                if let Some(publisher) = plan.items.iter().find(|item| item.id == pending.id) {
                    journal
                        .publishers
                        .entry(pending.id.clone())
                        .or_insert_with(|| Publisher {
                            repository: pending.repo.clone(),
                            org: publisher.org.clone(),
                            merge_sha: None,
                            push_step: options.push_step.clone(),
                            packages: packages.into_iter().collect(),
                            deploy_confirmed: false,
                            publications: Default::default(),
                        });
                }
            }
        }
        StoreOperation::SetSha { id, sha } => {
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
    if let Some(marker) = journal.consumers.get(&item.id) {
        if let Some(prepared) = &marker.prepared {
            if !gateway.incorporated(marker, item)? {
                return Ok(Outcome::NeedsReview {
                    pull_request_url: prepared.pull_request_url.clone(),
                    branch: marker.bump_branch.clone(),
                    consumer_head: marker.head_sha.clone(),
                });
            }
            if marker.publication_fingerprint.is_empty() {
                bail!(
                    "saved bump has no publication snapshot; inspect it before preparing another bump"
                )
            }
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
        let inputs: Vec<_> = publications
            .iter()
            .filter_map(|result| match result {
                PublicationResult::Published { evidence } => Some(PublicationInput {
                    run_id: evidence.run_id(),
                    version: evidence.version().into(),
                }),
                _ => None,
            })
            .collect();
        journal
            .publishers
            .get_mut(&dependency.publisher_id)
            .expect("known publisher")
            .publications
            .insert(package.to_ascii_lowercase(), inputs);
        save(journal)?;
        results.extend(publications);
    }
    let packages: BTreeSet<String> = results
        .iter()
        .filter_map(|result| match result {
            PublicationResult::Published { evidence } => Some(evidence.package().to_string()),
            PublicationResult::Skipped { .. } => None,
        })
        .collect();
    if packages.is_empty() {
        return Ok(Outcome::Skipped {
            reason: "publisher did not produce a new package in this execution".into(),
        });
    }
    let consumer = gateway.consumer(item, &packages.into_iter().collect::<Vec<_>>())?;
    journal.consumers.insert(
        item.id.clone(),
        Preparation {
            repository: consumer.repository.clone(),
            number: consumer.number,
            head_branch: consumer.head_branch.clone(),
            head_sha: consumer.head_sha.clone(),
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
    let prepared = gateway.prepare(&results, &consumer)?;
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

impl Gateway for GithubGateway<'_> {
    fn publication(
        &mut self,
        publisher: &Publisher,
        package: &str,
        known: Option<&[PublicationInput]>,
    ) -> Result<Vec<PublicationResult>> {
        let sha = publisher
            .merge_sha
            .as_deref()
            .context("publisher has no confirmed merge SHA")?;
        let run_ids = if let Some(known) = known {
            known.iter().map(|input| input.run_id).collect::<Vec<_>>()
        } else {
            let body = self.client.rest(&format!(
                "/repos/{}/actions/runs?head_sha={sha}&event=push&per_page=100",
                publisher.repository
            ))?;
            if body["total_count"]
                .as_u64()
                .context("missing publisher run count")?
                > 100
            {
                bail!("publisher workflow run list is truncated")
            }
            let runs = body["workflow_runs"]
                .as_array()
                .context("missing publisher workflow runs")?;
            if !runs.is_empty()
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
            let result = publication::verify(&execution, &run, package, &mut feed)?;
            if let Some(known) = known {
                match &result {
                    PublicationResult::Published { evidence }
                        if known.iter().any(|input| {
                            input.run_id == evidence.run_id() && input.version == evidence.version()
                        }) => {}
                    _ => bail!(
                        "saved publication inputs no longer verify against the workflow and exact feed"
                    ),
                }
            }
            results.push(result);
        }
        if results.is_empty() && known.is_none() {
            bail!("no successful publisher workflow can prove publication for the merged SHA")
        }
        Ok(results)
    }

    fn consumer(&mut self, item: &Item, packages: &[String]) -> Result<Consumer> {
        let repository = format!("{}/{}", item.org, item.repo);
        let pr = self
            .client
            .rest(&format!("/repos/{repository}/pulls/{}", item.number))?;
        if pr["state"] != "open"
            || pr["head"]["sha"] != item.sha
            || pr["head"]["ref"] != item.head
            || pr["head"]["repo"]["full_name"]
                .as_str()
                .is_none_or(|slug| !slug.eq_ignore_ascii_case(&repository))
        {
            bail!("consumer PR head changed; replan before preparing a bump");
        }
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
        let Some(prepared) = &preparation.prepared else {
            return Ok(false);
        };
        let number = prepared
            .pull_request_url
            .rsplit('/')
            .next()
            .context("invalid saved bump PR URL")?
            .parse::<u64>()?;
        let pr = self
            .client
            .rest(&format!("/repos/{}/pulls/{number}", preparation.repository))?;
        if pr["merged"] != true
            || pr["base"]["ref"] != item.head
            || pr["head"]["sha"] != prepared.commit
        {
            return Ok(false);
        }
        let sha = pr["merge_commit_sha"]
            .as_str()
            .context("merged bump PR has no merge commit")?;
        self.client
            .contains_commit(&preparation.repository, &item.sha, sha)
    }

    fn prepare(
        &mut self,
        publications: &[PublicationResult],
        consumer: &Consumer,
    ) -> Result<PrepareResult> {
        let root = self
            .options
            .workspace
            .clone()
            .or_else(|| crate::cache::default_dir().map(|root| root.join("bump-workspaces")))
            .context("no safe cache directory for isolated bump workspaces")?;
        let mut backend = LocalBackend::new(
            root,
            ProcessRunner,
            GithubRemote {
                client: self.client,
            },
            self.options.timeout,
        );
        consumer_bump::prepare_many(publications, consumer, &mut backend)
    }
}
