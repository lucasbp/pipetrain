use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use yaml_rust2::{Yaml, YamlLoader};

use crate::github::Client;
use crate::merge_scheduler::Capacity;
use crate::workflows::{self, PushRuns};

pub const DYNAMIC: &str = "dynamic";
const MAX_DEPTH: usize = 5;
const GENERIC_LABELS: [&str; 7] = ["linux", "windows", "macos", "x64", "x86", "arm", "arm64"];

pub trait Source {
    fn workflows(&mut self, slug: &str, git_ref: &str) -> Result<Vec<(String, String)>>;
    fn file(&mut self, slug: &str, git_ref: &str, path: &str) -> Result<Option<String>>;
}

impl Source for Client {
    fn workflows(&mut self, slug: &str, git_ref: &str) -> Result<Vec<(String, String)>> {
        let tree = root_tree(self, slug, git_ref)?;
        workflows::read(self, slug, &tree)
    }

    fn file(&mut self, slug: &str, git_ref: &str, path: &str) -> Result<Option<String>> {
        let tree = root_tree(self, slug, git_ref)?;
        let found = self.blobs(slug, &tree, &[path.to_string()])?;
        Ok(found.into_iter().next().map(|(_, text)| text))
    }
}

fn root_tree(client: &mut Client, slug: &str, git_ref: &str) -> Result<String> {
    let body = client.rest(&format!("/repos/{slug}/git/trees/{git_ref}"))?;
    body.get("sha")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("{slug}@{git_ref} has no root tree"))
}

pub fn detect(source: &mut impl Source, slug: &str, branch: &str) -> BTreeSet<String> {
    let files = match source.workflows(slug, branch) {
        Ok(files) => files,
        Err(_) => return BTreeSet::from([DYNAMIC.to_string()]),
    };
    let relevant: Vec<String> = match workflows::branch_push_runs(&files, branch) {
        PushRuns::Expected {
            workflows,
            chained_workflows,
            ..
        } => workflows.into_iter().chain(chained_workflows).collect(),
        PushRuns::Dispensed { .. } => return BTreeSet::new(),
        PushRuns::Indeterminate { .. } => return BTreeSet::from([DYNAMIC.to_string()]),
    };
    let mut found = BTreeSet::new();
    let mut walk = Walk {
        source,
        origin: slug,
        branch,
        files: &files,
        found: &mut found,
    };
    for path in relevant {
        let Some((_, text)) = files.iter().find(|(name, _)| *name == path) else {
            walk.found.insert(DYNAMIC.to_string());
            continue;
        };
        walk.workflow(text, slug, branch, 0);
    }
    found
}

struct Walk<'a, S: Source> {
    source: &'a mut S,
    origin: &'a str,
    branch: &'a str,
    files: &'a [(String, String)],
    found: &'a mut BTreeSet<String>,
}

impl<S: Source> Walk<'_, S> {
    fn workflow(&mut self, text: &str, home: &str, git_ref: &str, depth: usize) {
        let Some(document) = YamlLoader::load_from_str(text)
            .ok()
            .and_then(|mut documents| (!documents.is_empty()).then(|| documents.remove(0)))
        else {
            self.found.insert(DYNAMIC.to_string());
            return;
        };
        let Some(map) = document.as_hash() else {
            self.found.insert(DYNAMIC.to_string());
            return;
        };
        if let Some(concurrency) = field(map, "concurrency") {
            self.concurrency(concurrency, self.origin);
        }
        let Some(jobs) = field(map, "jobs").and_then(Yaml::as_hash) else {
            return;
        };
        for (_, job) in jobs {
            let Some(job) = job.as_hash() else {
                self.found.insert(DYNAMIC.to_string());
                continue;
            };
            if let Some(concurrency) = field(job, "concurrency") {
                self.concurrency(concurrency, self.origin);
            }
            if let Some(uses) = field(job, "uses") {
                self.reusable(uses, home, git_ref, depth);
            } else if let Some(runs_on) = field(job, "runs-on") {
                self.runner(runs_on);
            }
        }
    }

    fn concurrency(&mut self, value: &Yaml, repo: &str) {
        let group = match value {
            Yaml::String(group) => Some(group.as_str()),
            Yaml::Hash(map) => field(map, "group").and_then(Yaml::as_str),
            _ => None,
        };
        let key = match group {
            Some(group) if !group.contains("${{") => {
                format!("group:{}#{}", repo.to_lowercase(), group.to_lowercase())
            }
            _ => format!("group:{}#*", repo.to_lowercase()),
        };
        self.found.insert(key);
    }

    fn runner(&mut self, value: &Yaml) {
        let mut labels = Vec::new();
        match value {
            Yaml::String(label) => labels.push(label.clone()),
            Yaml::Array(items) => {
                for item in items {
                    match item.as_str() {
                        Some(label) => labels.push(label.to_string()),
                        None => {
                            self.found.insert(DYNAMIC.to_string());
                        }
                    }
                }
            }
            Yaml::Hash(map) => {
                match field(map, "group") {
                    Some(Yaml::String(group)) if !group.contains("${{") => {
                        self.found
                            .insert(format!("runner-group:{}", group.to_lowercase()));
                    }
                    Some(_) => {
                        self.found.insert(DYNAMIC.to_string());
                    }
                    None => {}
                }
                match field(map, "labels") {
                    Some(Yaml::String(label)) => labels.push(label.clone()),
                    Some(Yaml::Array(items)) => {
                        for item in items {
                            match item.as_str() {
                                Some(label) => labels.push(label.to_string()),
                                None => {
                                    self.found.insert(DYNAMIC.to_string());
                                }
                            }
                        }
                    }
                    Some(_) => {
                        self.found.insert(DYNAMIC.to_string());
                    }
                    None => {}
                }
            }
            _ => {
                self.found.insert(DYNAMIC.to_string());
            }
        }
        self.labels(&labels);
    }

    fn labels(&mut self, labels: &[String]) {
        let labels: Vec<String> = labels
            .iter()
            .map(|label| label.trim().to_lowercase())
            .filter(|label| !label.is_empty())
            .collect();
        if labels.iter().any(|label| label.contains("${{")) {
            self.found.insert(DYNAMIC.to_string());
            return;
        }
        if !labels.iter().any(|label| label == "self-hosted") {
            return;
        }
        let specific: Vec<&String> = labels
            .iter()
            .filter(|label| {
                label.as_str() != "self-hosted" && !GENERIC_LABELS.contains(&label.as_str())
            })
            .collect();
        if !specific.is_empty() {
            for label in specific {
                self.found.insert(format!("runner:{label}"));
            }
            return;
        }
        let generic: Vec<&String> = labels
            .iter()
            .filter(|label| label.as_str() != "self-hosted")
            .collect();
        if generic.is_empty() {
            self.found.insert("runner:self-hosted".to_string());
        }
        for label in generic {
            self.found.insert(format!("runner:{label}"));
        }
    }

    fn reusable(&mut self, uses: &Yaml, home: &str, git_ref: &str, depth: usize) {
        let Some(uses) = uses.as_str() else {
            self.found.insert(DYNAMIC.to_string());
            return;
        };
        if depth >= MAX_DEPTH || uses.contains("${{") {
            self.found.insert(DYNAMIC.to_string());
            return;
        }
        if let Some(path) = uses.strip_prefix("./") {
            let local = if home.eq_ignore_ascii_case(self.origin) && git_ref == self.branch {
                self.files
                    .iter()
                    .find(|(name, _)| name == path)
                    .map(|(_, text)| text.clone())
            } else {
                None
            };
            let text = match local {
                Some(text) => Some(text),
                None => self.source.file(home, git_ref, path).ok().flatten(),
            };
            match text {
                Some(text) => self.workflow(&text, home, git_ref, depth + 1),
                None => {
                    self.found.insert(DYNAMIC.to_string());
                }
            }
            return;
        }
        let Some((location, target)) = uses.split_once('@') else {
            self.found.insert(DYNAMIC.to_string());
            return;
        };
        let parts: Vec<&str> = location.splitn(3, '/').collect();
        let [owner, repo, path] = parts[..] else {
            self.found.insert(DYNAMIC.to_string());
            return;
        };
        let slug = format!("{owner}/{repo}");
        match self.source.file(&slug, target, path) {
            Ok(Some(text)) => self.workflow(&text, &slug, target, depth + 1),
            _ => {
                self.found.insert(DYNAMIC.to_string());
            }
        }
    }
}

pub fn with_reusables(
    source: &mut impl Source,
    slug: &str,
    git_ref: &str,
    files: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut all = files.clone();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<(String, String, String, usize)> = files
        .into_iter()
        .map(|(_, text)| (text, slug.to_string(), git_ref.to_string(), 0))
        .collect();
    while let Some((text, home, reference, depth)) = pending.pop() {
        if depth >= MAX_DEPTH {
            continue;
        }
        for uses in job_uses(&text) {
            let Some((target_slug, target_ref, path)) = locate(&uses, &home, &reference) else {
                continue;
            };
            let label = format!("{target_slug}/{path}@{target_ref}");
            if !seen.insert(label.to_lowercase()) {
                continue;
            }
            if target_slug.eq_ignore_ascii_case(slug)
                && target_ref == git_ref
                && all.iter().any(|(name, _)| *name == path)
            {
                continue;
            }
            if let Ok(Some(found)) = source.file(&target_slug, &target_ref, &path) {
                all.push((label, found.clone()));
                pending.push((found, target_slug, target_ref, depth + 1));
            }
        }
    }
    all
}

fn job_uses(text: &str) -> Vec<String> {
    let Some(document) = YamlLoader::load_from_str(text)
        .ok()
        .and_then(|mut documents| (!documents.is_empty()).then(|| documents.remove(0)))
    else {
        return Vec::new();
    };
    let Some(jobs) = document
        .as_hash()
        .and_then(|map| field(map, "jobs"))
        .and_then(Yaml::as_hash)
    else {
        return Vec::new();
    };
    jobs.iter()
        .filter_map(|(_, job)| job.as_hash())
        .filter_map(|job| field(job, "uses"))
        .filter_map(Yaml::as_str)
        .filter(|uses| !uses.contains("${{"))
        .map(str::to_string)
        .collect()
}

fn locate(uses: &str, home: &str, git_ref: &str) -> Option<(String, String, String)> {
    if let Some(path) = uses.strip_prefix("./") {
        return Some((home.to_string(), git_ref.to_string(), path.to_string()));
    }
    let (location, target) = uses.split_once('@')?;
    let parts: Vec<&str> = location.splitn(3, '/').collect();
    let [owner, repo, path] = parts[..] else {
        return None;
    };
    Some((
        format!("{owner}/{repo}"),
        target.to_string(),
        path.to_string(),
    ))
}

fn field<'a>(map: &'a yaml_rust2::yaml::Hash, name: &str) -> Option<&'a Yaml> {
    map.iter()
        .find(|(key, _)| matches!(key, Yaml::String(text) if text == name))
        .map(|(_, value)| value)
}

pub fn detect_all(
    client: &mut Client,
    repos: &BTreeSet<(String, String)>,
) -> BTreeMap<String, Vec<String>> {
    let mut found = BTreeMap::new();
    for (slug, branch) in repos {
        let key = slug.to_lowercase();
        let resources = detect(client, slug, branch);
        found
            .entry(key)
            .or_insert_with(BTreeSet::new)
            .extend(resources);
    }
    found
        .into_iter()
        .map(|(slug, resources)| (slug, resources.into_iter().collect()))
        .collect()
}

pub fn slots(resource: &str, configured: &BTreeMap<String, usize>) -> Option<usize> {
    let bare = resource
        .strip_prefix("runner:")
        .or_else(|| resource.strip_prefix("runner-group:"))
        .or_else(|| {
            resource
                .strip_prefix("group:")
                .and_then(|rest| rest.split_once('#').map(|(_, group)| group))
        })
        .unwrap_or(resource);
    configured.iter().find_map(|(key, value)| {
        let key = key.to_lowercase();
        (key == resource || key == bare).then_some(*value)
    })
}

pub fn capacity(
    jobs: &[(String, String)],
    detected: &BTreeMap<String, Vec<String>>,
    configured: &BTreeMap<String, usize>,
) -> Capacity {
    let mut capacity = Capacity::default();
    for (id, repo) in jobs {
        let needs = if detected.is_empty() {
            Vec::new()
        } else {
            detected
                .get(&repo.to_lowercase())
                .cloned()
                .unwrap_or_else(|| vec![DYNAMIC.to_string()])
        };
        for resource in &needs {
            if let Some(limit) = slots(resource, configured) {
                capacity.slots.insert(resource.clone(), limit);
            }
        }
        if !needs.is_empty() {
            capacity.resources.insert(id.clone(), needs);
        }
    }
    capacity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        repos: BTreeMap<String, Vec<(String, String)>>,
        remote: BTreeMap<String, String>,
    }

    impl Source for Fake {
        fn workflows(&mut self, slug: &str, _: &str) -> Result<Vec<(String, String)>> {
            self.repos
                .get(slug)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such repo"))
        }

        fn file(&mut self, slug: &str, git_ref: &str, path: &str) -> Result<Option<String>> {
            Ok(self
                .remote
                .get(&format!("{slug}/{path}@{git_ref}"))
                .cloned())
        }
    }

    fn deploy(runs_on: &str, extra: &str) -> (String, String) {
        (
            ".github/workflows/deploy.yml".to_string(),
            format!(
                "name: deploy\non:\n  push:\n    branches: [main]\n{extra}jobs:\n  ship:\n    runs-on: {runs_on}\n    steps:\n      - run: echo\n"
            ),
        )
    }

    fn one(slug: &str, file: (String, String)) -> Fake {
        let mut fake = Fake::default();
        fake.repos.insert(slug.to_string(), vec![file]);
        fake
    }

    fn found(fake: &mut Fake, slug: &str) -> Vec<String> {
        detect(fake, slug, "main").into_iter().collect()
    }

    #[test]
    fn a_self_hosted_label_is_a_resource() {
        let mut fake = one("o/a", deploy("[self-hosted, linux, deploy-box]", ""));
        assert_eq!(found(&mut fake, "o/a"), ["runner:deploy-box"]);
    }

    #[test]
    fn hosted_runners_are_not_resources() {
        for label in ["ubuntu-latest", "windows-2022", "macos-14"] {
            let mut fake = one("o/a", deploy(label, ""));
            assert!(found(&mut fake, "o/a").is_empty(), "{label}");
        }
    }

    #[test]
    fn a_dynamic_label_becomes_the_shared_resource() {
        let mut fake = one("o/a", deploy("${{ matrix.os }}", ""));
        assert_eq!(found(&mut fake, "o/a"), [DYNAMIC]);
        let mut fake = one("o/a", deploy("[self-hosted, \"${{ vars.POOL }}\"]", ""));
        assert!(found(&mut fake, "o/a").contains(&DYNAMIC.to_string()));
    }

    #[test]
    fn an_unreadable_repo_is_dynamic() {
        let mut fake = Fake::default();
        assert_eq!(found(&mut fake, "o/missing"), [DYNAMIC]);
    }

    #[test]
    fn a_runner_group_is_a_resource() {
        let mut fake = one(
            "o/a",
            deploy("\n      group: Prod-Pool\n      labels: [self-hosted]", ""),
        );
        let resources = found(&mut fake, "o/a");
        assert!(resources.contains(&"runner-group:prod-pool".to_string()));
    }

    #[test]
    fn a_concurrency_group_is_scoped_to_its_repository() {
        let extra = "concurrency:\n  group: Prod\n";
        let mut a = one("o/a", deploy("ubuntu-latest", extra));
        let mut b = one("o/b", deploy("ubuntu-latest", extra));
        let first = found(&mut a, "o/a");
        let second = found(&mut b, "o/b");
        assert_eq!(first, ["group:o/a#prod"]);
        assert_eq!(second, ["group:o/b#prod"]);
        assert_ne!(first, second);
        let again = found(&mut a, "o/a");
        assert_eq!(first, again);
    }

    #[test]
    fn a_reusable_workflow_from_another_repo_is_followed() {
        let caller = (
            ".github/workflows/deploy.yml".to_string(),
            "name: deploy\non:\n  push:\n    branches: [main]\njobs:\n  ship:\n    uses: org/pipelines/.github/workflows/ship.yml@v2\n"
                .to_string(),
        );
        let mut fake = one("o/a", caller);
        fake.remote.insert(
            "org/pipelines/.github/workflows/ship.yml@v2".to_string(),
            "on: workflow_call\njobs:\n  go:\n    runs-on: [self-hosted, ship-box]\n".to_string(),
        );
        assert_eq!(found(&mut fake, "o/a"), ["runner:ship-box"]);
        fake.remote.clear();
        assert_eq!(found(&mut fake, "o/a"), [DYNAMIC]);
    }

    #[test]
    fn a_local_reusable_workflow_is_followed() {
        let caller = (
            ".github/workflows/deploy.yml".to_string(),
            "name: deploy\non:\n  push:\n    branches: [main]\njobs:\n  ship:\n    uses: ./.github/workflows/ship.yml\n"
                .to_string(),
        );
        let ship = (
            ".github/workflows/ship.yml".to_string(),
            "on: workflow_call\njobs:\n  go:\n    runs-on: [self-hosted, local-box]\n".to_string(),
        );
        let mut fake = Fake::default();
        fake.repos.insert("o/a".to_string(), vec![caller, ship]);
        assert_eq!(found(&mut fake, "o/a"), ["runner:local-box"]);
    }

    #[test]
    fn a_workflow_that_does_not_run_on_push_needs_nothing() {
        let file = (
            ".github/workflows/ci.yml".to_string(),
            "on: pull_request\njobs:\n  t:\n    runs-on: [self-hosted, box]\n".to_string(),
        );
        let mut fake = one("o/a", file);
        assert!(found(&mut fake, "o/a").is_empty());
    }

    fn table(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), *value))
            .collect()
    }

    #[test]
    fn slots_match_by_label_group_or_full_id() {
        let configured = table(&[("Deploy-Box", 2), ("prod", 3)]);
        assert_eq!(slots("runner:deploy-box", &configured), Some(2));
        assert_eq!(slots("group:o/a#prod", &configured), Some(3));
        assert_eq!(slots("runner:other", &configured), None);
    }

    #[test]
    fn capacity_gives_every_job_the_resources_of_its_repo() {
        let mut detected = BTreeMap::new();
        detected.insert("o/a".to_string(), vec!["runner:box".to_string()]);
        detected.insert("o/b".to_string(), Vec::new());
        let jobs = vec![
            ("s0".to_string(), "o/a".to_string()),
            ("s1".to_string(), "O/B".to_string()),
            ("s2".to_string(), "o/unscanned".to_string()),
        ];
        let capacity = capacity(&jobs, &detected, &table(&[("box", 2)]));
        assert_eq!(capacity.resources["s0"], ["runner:box"]);
        assert!(!capacity.resources.contains_key("s1"));
        assert_eq!(capacity.resources["s2"], [DYNAMIC]);
        assert_eq!(capacity.slots["runner:box"], 2);
        assert!(
            super::capacity(&jobs, &BTreeMap::new(), &table(&[]))
                .resources
                .is_empty()
        );
    }

    #[test]
    fn the_limit_in_a_called_reusable_workflow_is_reachable_from_the_deploy_workflow() {
        let caller = (
            ".github/workflows/deploy.yml".to_string(),
            "name: deploy
jobs:
  check:
    uses: acme/shared/.github/workflows/versions.yml@main
  local:
    uses: ./.github/workflows/own.yml
"
            .to_string(),
        );
        let mut source = Fake {
            repos: BTreeMap::new(),
            remote: BTreeMap::from([
                (
                    "acme/shared/.github/workflows/versions.yml@main".to_string(),
                    "name: versions
jobs:
  verify:
    steps:
      - run: check-versions --max-behind 1
"
                    .to_string(),
                ),
                (
                    "acme/send/.github/workflows/own.yml@abc".to_string(),
                    "name: own
jobs:
  x:
    steps:
      - run: echo
"
                    .to_string(),
                ),
            ]),
        };
        let files = with_reusables(&mut source, "acme/send", "abc", vec![caller]);
        assert_eq!(files.len(), 3);
        assert!(matches!(
            workflows::max_behind(&files),
            workflows::MaxBehind::Found { max: 1, .. }
        ));
    }

    #[test]
    fn a_reusable_workflow_that_cannot_be_read_leaves_the_files_untouched() {
        let caller = (
            ".github/workflows/deploy.yml".to_string(),
            "name: deploy
jobs:
  check:
    uses: acme/shared/.github/workflows/versions.yml@main
"
            .to_string(),
        );
        let mut source = Fake {
            repos: BTreeMap::new(),
            remote: BTreeMap::new(),
        };
        let files = with_reusables(&mut source, "acme/send", "abc", vec![caller.clone()]);
        assert_eq!(files, vec![caller]);
    }
}
