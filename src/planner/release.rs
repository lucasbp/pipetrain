use std::collections::{BTreeMap, BTreeSet};

use crate::config::Rules;
use crate::manifest;
use crate::model::{Pr, PrKey, Question};
use crate::plan::{Plan, WAITING_NEXT_STEP};
use crate::planner::{Item, Rule};
use crate::publishers::{Publishers, References};
use crate::workflows::MaxBehind;

pub const CODE: &str = "package-release-behind";

pub type Manifest = (String, String, String);

pub type Versions = BTreeMap<(String, String), BTreeSet<String>>;

#[derive(Debug, Default)]
pub struct Context {
    pub publishers: Publishers,
    pub references: References,
    pub versions: Versions,
    pub releases: BTreeMap<String, BTreeSet<String>>,
    pub limits: BTreeMap<String, MaxBehind>,
    pub default_max: Option<u32>,
    pub repos: BTreeSet<String>,
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn inside(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn referenced_versions(manifests: &[Manifest], references: &References) -> Versions {
    let mut found = Versions::new();
    for (slug, path, text) in manifests {
        let repo = slug.to_lowercase();
        let Some(packages) = references.get(&repo) else {
            continue;
        };
        for package in packages {
            for version in manifest::package_versions(path, text, package) {
                found
                    .entry((repo.clone(), package.clone()))
                    .or_default()
                    .insert(version);
            }
        }
    }
    found
}

fn queue_releases(
    publishers: &Publishers,
    manifests: &[Manifest],
    prs: &[&Pr],
) -> BTreeMap<String, BTreeSet<String>> {
    let mut roots: BTreeMap<(String, String), Vec<&str>> = BTreeMap::new();
    for (slug, path, text) in manifests {
        if let Some(package) = manifest::published(path, text) {
            roots
                .entry((package, slug.to_lowercase()))
                .or_default()
                .push(directory(path));
        }
    }
    let mut releases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (package, owners) in publishers {
        for pr in prs {
            let repo = pr.key.slug().to_lowercase();
            if !owners.contains(&repo) {
                continue;
            }
            let publishes = match roots.get(&(package.clone(), repo)) {
                Some(dirs) => {
                    !pr.files_complete
                        || pr
                            .files
                            .iter()
                            .any(|file| dirs.iter().any(|dir| inside(&file.path, dir)))
                }
                None => true,
            };
            if publishes {
                releases
                    .entry(package.clone())
                    .or_default()
                    .insert(pr.key.to_string());
            }
        }
    }
    releases
}

impl Context {
    pub fn new(
        rules: &Rules,
        scanned: &Publishers,
        references: &References,
        manifests: &[Manifest],
        prs: &[&Pr],
        default_max: Option<u32>,
    ) -> Self {
        let mut publishers = scanned.clone();
        for (package, slug) in &rules.packages {
            publishers.insert(package.clone(), BTreeSet::from([slug.clone()]));
        }
        Self {
            releases: queue_releases(&publishers, manifests, prs),
            versions: referenced_versions(manifests, references),
            publishers,
            references: references.clone(),
            limits: BTreeMap::new(),
            default_max,
            repos: prs.iter().map(|pr| pr.key.slug().to_lowercase()).collect(),
        }
    }

    pub fn limit_candidates(&self) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for (consumer, packages) in &self.references {
            if !self.repos.contains(consumer) {
                continue;
            }
            let released = packages.iter().any(|package| {
                self.publishers
                    .get(package)
                    .is_some_and(|owners| !owners.contains(consumer))
                    && self
                        .releases
                        .get(package)
                        .is_some_and(|ids| !ids.is_empty())
            });
            if released {
                found.insert(consumer.clone());
            }
        }
        found
    }
}

fn bumps(item: &Item, package: &str) -> bool {
    item.after.iter().any(|dep| {
        dep.rule == Rule::Package
            && dep
                .package
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(package))
    })
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

pub fn annotate(plan: &mut Plan, context: &Context) {
    let mut questions = Vec::new();
    let mut causes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut warnings: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in &plan.items {
        let repo = format!("{}/{}", item.org, item.repo).to_lowercase();
        let Some(packages) = context.references.get(&repo) else {
            continue;
        };
        for package in packages {
            let owned = context
                .publishers
                .get(package)
                .is_some_and(|owners| owners.contains(&repo));
            let Some(releasing) = context.releases.get(package) else {
                continue;
            };
            if owned || releasing.is_empty() || bumps(item, package) {
                continue;
            }
            let count = releasing.len();
            let releasing_ids = releasing.iter().cloned().collect::<Vec<_>>().join(", ");
            let referenced = context
                .versions
                .get(&(repo.clone(), package.clone()))
                .filter(|versions| !versions.is_empty())
                .map(|versions| versions.iter().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| "(version not read)".to_string());
            let (limit, origin) = match context.limits.get(&repo) {
                Some(MaxBehind::Found { max, source }) => {
                    (Some(*max), format!("max-behind {max} in {source}"))
                }
                found => match context.default_max {
                    Some(max) => (
                        Some(max),
                        format!("merge.package_max_behind = {max} from the config"),
                    ),
                    None => (
                        None,
                        match found {
                            Some(MaxBehind::Unknown { reason }) => reason.clone(),
                            _ => "the workflows were not read".to_string(),
                        },
                    ),
                },
            };
            match limit {
                Some(max) if u32::try_from(count).map_or(true, |behind| behind >= max) => {
                    let message = format!(
                        "references {package} {referenced}; the queue publishes {count} new version{} of it ({releasing_ids}), so this PR will be {count} behind and the repo check fails at {max} behind ({origin})",
                        plural(count)
                    );
                    questions.push(Question::new(
                        Some(&PrKey::new(&item.org, &item.repo, item.number)),
                        CODE,
                        message.clone(),
                        "bump the reference in this PR to the version the queue publishes after the publishers deploy, or raise the limit",
                    ));
                    causes.entry(item.id.clone()).or_default().push(message);
                }
                Some(_) => {}
                None => warnings.entry(item.id.clone()).or_default().push(format!(
                    "(not confirmed) references {package} {referenced}; the queue publishes {count} new version{} of it ({releasing_ids}) and the max-behind limit of {repo} could not be determined ({origin}), so its version check may fail after the publishers merge",
                    plural(count)
                )),
            }
        }
    }
    plan.questions.extend(questions);
    for readiness in &mut plan.preflight {
        if let Some(found) = warnings.remove(&readiness.id) {
            readiness.warnings.extend(found);
        }
        let Some(found) = causes.remove(&readiness.id) else {
            continue;
        };
        if readiness.status == "blocked" {
            continue;
        }
        if readiness.status == "eligible" {
            readiness.causes.clear();
            readiness.next_step = WAITING_NEXT_STEP.to_string();
        }
        readiness.status = "waiting".to_string();
        readiness.causes.extend(found);
    }
}
