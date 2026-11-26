use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::discovery::{Scope, Skipped};
use crate::feed;
use crate::model::Pr;
use crate::planner::release::Context;
use crate::workflows::MaxBehind;

pub const PREFIX: &str = "needs a bump of ";

pub use crate::publishers::Feeds;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedBump {
    pub id: String,
    pub base: String,
    pub head: String,
    pub sha: String,
    pub package: String,
    pub from: String,
    pub owners: Vec<String>,
    pub publishers: Vec<String>,
}

pub fn queued_bumps(
    needs: &[Need],
    context: &Context,
    pr: &Pr,
    manifests: &[crate::planner::release::Manifest],
) -> Vec<QueuedBump> {
    let mut contracts = Vec::new();
    let id = pr.key.to_string();
    for need in needs.iter().filter(|need| {
        need.id == id && need.queue_published && context.nuget_releases.contains(&need.package)
    }) {
        let Some(publishers) = context.releases.get(&need.package) else {
            continue;
        };
        if need.behind.saturating_sub(publishers.len()) >= need.limit as usize {
            continue;
        }
        let repo = pr.key.slug();
        let nuget_pin = manifests.iter().any(|(slug, path, text)| {
            slug.eq_ignore_ascii_case(&repo)
                && (path.to_ascii_lowercase().ends_with("proj")
                    || path.to_ascii_lowercase().ends_with(".props"))
                && crate::manifest::package_versions(path, text, &need.package).contains(&need.from)
        });
        if nuget_pin {
            contracts.push(QueuedBump {
                id: need.id.clone(),
                base: pr.base_ref.clone(),
                head: pr.head_ref.clone(),
                sha: pr.head_sha.clone(),
                package: need.package.clone(),
                from: need.from.clone(),
                owners: context
                    .publishers
                    .get(&need.package)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect(),
                publishers: publishers.iter().cloned().collect(),
            });
        }
    }
    contracts
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Need {
    pub id: String,
    pub package: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub behind: usize,
    pub limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    pub queue_published: bool,
}

impl Need {
    pub fn cause(&self) -> String {
        let latest = self
            .latest
            .as_deref()
            .map(|version| format!(", latest stable {version}"))
            .unwrap_or_default();
        let detail = format!(
            "package {} behind: installed {}{latest}, limit {}",
            self.package, self.from, self.limit
        );
        match &self.to {
            Some(version) => format!("{PREFIX}{} to {version} ({detail})", self.package),
            None => format!(
                "{PREFIX}{} to the version the queue publishes ({detail})",
                self.package
            ),
        }
    }

    pub fn step_detail(&self) -> String {
        let target = self
            .to
            .as_deref()
            .unwrap_or("the version the queue publishes");
        format!(
            "bump {} from {} to {target} in {}, the repo check fails at {} behind and this would be {} behind",
            self.package,
            self.from,
            repo_of(&self.id),
            self.limit,
            self.behind
        )
    }
}

fn repo_of(id: &str) -> &str {
    id.split_once('#').map_or(id, |(repo, _)| repo)
}

pub fn is_pin(spec: &str) -> bool {
    spec.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !spec.contains(['*', '[', '(', ',', '^', '~', '>', '<', ' '])
}

pub fn stable_in_major(current: &str, versions: &[String]) -> Option<String> {
    let major = feed::Version::parse(current).numbers().first().copied();
    versions
        .iter()
        .filter(|version| {
            let parsed = feed::Version::parse(version);
            !parsed.is_prerelease()
                && parsed.numbers().first().copied() == major
                && feed::compare(version, current).is_gt()
        })
        .max_by(|a, b| feed::compare(a, b))
        .cloned()
}

pub fn versions_behind(current: &str, versions: &[String]) -> usize {
    feed::newest_first(versions)
        .iter()
        .filter(|version| !feed::is_prerelease(version) && feed::compare(version, current).is_gt())
        .count()
}

fn limit_of(context: &Context, repo: &str) -> Option<u32> {
    match context.limits.get(repo) {
        Some(MaxBehind::Found { max, .. }) => Some(*max),
        _ => context.default_max,
    }
}

fn oldest_pin(context: &Context, repo: &str, package: &str) -> Option<String> {
    context
        .versions
        .get(&(repo.to_string(), package.to_string()))?
        .iter()
        .filter(|spec| is_pin(spec))
        .min_by(|a, b| feed::compare(a, b))
        .cloned()
}

fn consumed(context: &Context, repo: &str) -> Vec<String> {
    let Some(packages) = context.references.get(repo) else {
        return Vec::new();
    };
    packages
        .iter()
        .filter(|package| {
            context
                .publishers
                .get(*package)
                .is_some_and(|owners| !owners.is_empty() && !owners.contains(repo))
        })
        .cloned()
        .collect()
}

pub fn consumers(context: &Context) -> BTreeSet<String> {
    context
        .repos
        .iter()
        .filter(|repo| !consumed(context, repo).is_empty())
        .cloned()
        .collect()
}

pub fn candidates(context: &Context) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for repo in &context.repos {
        if limit_of(context, repo).is_none() {
            continue;
        }
        for package in consumed(context, repo) {
            if oldest_pin(context, repo, &package).is_none() {
                continue;
            }
            let orgs = context
                .publishers
                .get(&package)
                .map(|owners| {
                    owners
                        .iter()
                        .filter_map(|slug| slug.split_once('/').map(|(org, _)| org.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            found.insert(package, orgs);
        }
    }
    found
}

fn lookup<'a>(feeds: &'a Feeds, package: &str) -> Option<&'a Vec<String>> {
    feeds
        .get(package)
        .or_else(|| feeds.get(&package.to_lowercase()))
}

pub fn find(context: &Context, ready: &[(Pr, Scope)], feeds: &Feeds) -> Vec<Need> {
    let mut needs = Vec::new();
    for (pr, _) in ready {
        let repo = pr.key.slug().to_lowercase();
        let Some(limit) = limit_of(context, &repo) else {
            continue;
        };
        for package in consumed(context, &repo) {
            let Some(pin) = oldest_pin(context, &repo, &package) else {
                continue;
            };
            let known = lookup(feeds, &package);
            if known.is_none() && !context.releases.contains_key(&package) {
                continue;
            }
            let published = known.map(Vec::as_slice).unwrap_or_default();
            let ahead_of_feed = !published.is_empty()
                && published
                    .iter()
                    .all(|version| feed::compare(&pin, version).is_gt());
            let queued = if ahead_of_feed {
                0
            } else {
                context.releases.get(&package).map_or(0, BTreeSet::len)
            };
            let behind = versions_behind(&pin, published) + queued;
            if limit == 0 || u32::try_from(behind).is_ok_and(|count| count < limit) {
                continue;
            }
            let to = stable_in_major(&pin, published);
            let latest = feed::newest_first(published).into_iter().find(|version| {
                !feed::is_prerelease(version) && feed::compare(version, &pin).is_gt()
            });
            if to.is_none() && queued == 0 {
                continue;
            }
            needs.push(Need {
                id: pr.key.to_string(),
                package,
                from: pin,
                to,
                behind,
                limit,
                latest,
                queue_published: queued > 0,
            });
        }
    }
    needs
}

pub fn hold(
    context: &Context,
    feeds: &Feeds,
    ready: &mut Vec<(Pr, Scope)>,
    skipped: &mut Vec<Skipped>,
) -> Vec<Need> {
    let needs = find(context, ready, feeds);
    hold_needs(&needs, ready, skipped);
    needs
}

pub(crate) fn hold_needs(needs: &[Need], ready: &mut Vec<(Pr, Scope)>, skipped: &mut Vec<Skipped>) {
    if needs.is_empty() {
        return;
    }
    let mut causes: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for need in needs {
        causes.entry(&need.id).or_default().push(need.cause());
    }
    let taken = std::mem::take(ready);
    for (pr, scope) in taken {
        match causes.remove(pr.key.to_string().as_str()) {
            Some(reasons) => skipped.push(Skipped { pr, scope, reasons }),
            None => ready.push((pr, scope)),
        }
    }
}
