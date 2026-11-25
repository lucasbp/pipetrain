use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use pipetrain::deploy_resources::{self, Source};
use pipetrain::merge_scheduler::{Completion, Coordinator, FailurePolicy, Job, Status, run_with};

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

struct Quiet;

impl Coordinator for Quiet {
    fn persist(&mut self, _: &str, _: &Status) -> Result<(), String> {
        Ok(())
    }
    fn confirm(&mut self, _: &Job) -> Result<(), String> {
        Ok(())
    }
    fn checkpoint(&mut self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn progress(&mut self, _: &str, _: &str) {}
}

fn workflow(runs_on: &str, group: Option<&str>) -> Vec<(String, String)> {
    let concurrency = group
        .map(|group| format!("concurrency:\n  group: {group}\n"))
        .unwrap_or_default();
    vec![(
        ".github/workflows/deploy.yml".to_string(),
        format!(
            "name: deploy\non:\n  push:\n    branches: [main]\n{concurrency}jobs:\n  ship:\n    runs-on: {runs_on}\n    steps:\n      - run: echo\n"
        ),
    )]
}

fn world(repos: &[(&str, &str, Option<&str>)]) -> Fake {
    let mut fake = Fake::default();
    for (slug, runs_on, group) in repos {
        fake.repos
            .insert(slug.to_string(), workflow(runs_on, *group));
    }
    fake
}

fn peak(fake: &mut Fake, repos: &[&str], slots: &[(&str, usize)]) -> usize {
    let mut detected = BTreeMap::new();
    for slug in repos {
        let found = deploy_resources::detect(fake, slug, "main");
        detected.insert(slug.to_lowercase(), found.into_iter().collect::<Vec<_>>());
    }
    let configured: BTreeMap<String, usize> = slots
        .iter()
        .map(|(key, value)| (key.to_string(), *value))
        .collect();
    let needing: Vec<(String, String)> = repos
        .iter()
        .enumerate()
        .map(|(index, slug)| (format!("step:{index}"), slug.to_string()))
        .collect();
    let capacity = deploy_resources::capacity(&needing, &detected, &configured);
    let jobs: Vec<Job> = needing
        .iter()
        .map(|(id, slug)| Job {
            id: id.clone(),
            repo: slug.to_lowercase(),
            base: "main".to_string(),
            after: Vec::new(),
            publisher: false,
            barrier: false,
        })
        .collect();
    let current = AtomicUsize::new(0);
    let top = AtomicUsize::new(0);
    let report = run_with(
        &jobs,
        BTreeMap::new(),
        jobs.len(),
        &capacity,
        FailurePolicy::StopOnPublisher,
        &mut Quiet,
        |_, _, _| {
            let active = current.fetch_add(1, Ordering::SeqCst) + 1;
            top.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            current.fetch_sub(1, Ordering::SeqCst);
            Ok(Completion::Done)
        },
    )
    .unwrap();
    assert!(report.states.values().all(|state| *state == Status::Done));
    top.load(Ordering::SeqCst)
}

#[test]
fn the_same_self_hosted_label_serializes_deploys() {
    let mut fake = world(&[
        ("o/a", "[self-hosted, deploy-box]", None),
        ("o/b", "[self-hosted, deploy-box]", None),
        ("o/c", "[self-hosted, deploy-box]", None),
    ]);
    assert_eq!(peak(&mut fake, &["o/a", "o/b", "o/c"], &[]), 1);
}

#[test]
fn different_self_hosted_labels_do_not_serialize() {
    let mut fake = world(&[
        ("o/a", "[self-hosted, box-a]", None),
        ("o/b", "[self-hosted, box-b]", None),
    ]);
    assert_eq!(peak(&mut fake, &["o/a", "o/b"], &[]), 2);
}

#[test]
fn github_hosted_runners_do_not_serialize() {
    let mut fake = world(&[
        ("o/a", "ubuntu-latest", None),
        ("o/b", "ubuntu-latest", None),
        ("o/c", "windows-2022", None),
    ]);
    assert_eq!(peak(&mut fake, &["o/a", "o/b", "o/c"], &[]), 3);
}

#[test]
fn a_dynamic_label_becomes_a_shared_resource() {
    let mut fake = world(&[
        ("o/a", "${{ matrix.os }}", None),
        ("o/b", "${{ inputs.runner }}", None),
        ("o/c", "ubuntu-latest", None),
    ]);
    assert_eq!(peak(&mut fake, &["o/a", "o/b", "o/c"], &[]), 2);
}

#[test]
fn the_same_group_name_in_two_repositories_does_not_serialize() {
    let mut fake = world(&[
        ("o/a", "ubuntu-latest", Some("production")),
        ("o/b", "ubuntu-latest", Some("production")),
    ]);
    assert_eq!(peak(&mut fake, &["o/a", "o/b"], &[]), 2);
}

#[test]
fn the_same_group_name_in_the_same_repository_is_one_resource() {
    let mut fake = world(&[("o/a", "ubuntu-latest", Some("production"))]);
    let found = deploy_resources::detect(&mut fake, "o/a", "main");
    assert_eq!(
        found.into_iter().collect::<Vec<_>>(),
        ["group:o/a#production"]
    );
}

#[test]
fn deploy_slots_allow_that_many_deploys_together() {
    let repos = ["o/a", "o/b", "o/c", "o/d"];
    let mut fake = world(&[
        ("o/a", "[self-hosted, deploy-box]", None),
        ("o/b", "[self-hosted, deploy-box]", None),
        ("o/c", "[self-hosted, deploy-box]", None),
        ("o/d", "[self-hosted, deploy-box]", None),
    ]);
    assert_eq!(peak(&mut fake, &repos, &[("deploy-box", 2)]), 2);
    assert_eq!(peak(&mut fake, &repos, &[]), 1);
}

#[test]
fn a_reusable_workflow_from_another_repository_is_followed() {
    let caller = (
        ".github/workflows/deploy.yml".to_string(),
        "name: deploy\non:\n  push:\n    branches: [main]\njobs:\n  ship:\n    uses: org/pipelines/.github/workflows/ship.yml@v2\n"
            .to_string(),
    );
    let mut fake = Fake::default();
    for slug in ["o/a", "o/b"] {
        fake.repos.insert(slug.to_string(), vec![caller.clone()]);
    }
    fake.remote.insert(
        "org/pipelines/.github/workflows/ship.yml@v2".to_string(),
        "on: workflow_call\njobs:\n  go:\n    runs-on: [self-hosted, ship-box]\n".to_string(),
    );
    assert_eq!(peak(&mut fake, &["o/a", "o/b"], &[]), 1);
}
