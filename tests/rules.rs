use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::discovery::{self, Scope};
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::planner::{self, DepState, PlanInput, Planned};
use pipetrain::publishers::Publishers;

fn pr(slug: &str, number: u64, head: &str, base: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        base_ref: base.to_string(),
        head_sha: format!("{number:040x}"),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
    }
}

fn config(extra: &str) -> Config {
    Config::parse(&format!(
        "orgs = [\"acme\"]\n{extra}\n[[layer]]\nname = \"api\"\nrepos = [\"acme/api-*\"]\n[[layer]]\nname = \"front\"\nrepos = [\"acme/front-*\"]\n"
    ))
    .unwrap()
}

fn run(
    cfg: &Config,
    prs: Vec<Pr>,
    external: BTreeMap<String, DepState>,
    publishers: Publishers,
) -> Planned {
    let rules = Rules::new(cfg).unwrap();
    planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external,
        publishers,
    })
}

fn edge(planned: &Planned, from: &str, to: &str, rule: &str) -> bool {
    planned
        .items
        .iter()
        .find(|i| i.id == to)
        .unwrap()
        .after
        .iter()
        .any(|d| d.id == from && d.rule.as_str() == rule)
}

fn codes(planned: &Planned) -> Vec<&str> {
    planned.questions.iter().map(|q| q.code.as_str()).collect()
}

#[test]
fn depends_on_orders_prs_with_no_other_link() {
    let mut front = pr("acme/front-web", 3, "feat/x", "master");
    front.body = "Depends-On: acme/api-core#9".to_string();
    let api = pr("acme/api-core", 9, "feat/y", "master");
    let planned = run(
        &config(""),
        vec![front, api],
        BTreeMap::new(),
        Publishers::new(),
    );
    assert!(edge(
        &planned,
        "acme/api-core#9",
        "acme/front-web#3",
        "depends-on"
    ));
    assert_eq!(planned.lanes.len(), 1);
}

#[test]
fn depends_on_merged_is_a_note_and_open_outside_the_plan_is_a_question() {
    let mut a = pr("acme/front-web", 3, "feat/x", "master");
    a.body = "Depends-On: acme/api-core#1\nDepends-On: acme/api-core#2\nDepends-On: acme/api-core#3\nDepends-On: acme/api-core#4".to_string();
    let external = BTreeMap::from([
        ("acme/api-core#1".to_string(), DepState::Merged),
        ("acme/api-core#2".to_string(), DepState::Open),
        ("acme/api-core#3".to_string(), DepState::Closed),
        ("acme/api-core#4".to_string(), DepState::Missing),
    ]);
    let planned = run(&config(""), vec![a], external, Publishers::new());
    assert_eq!(planned.notes.len(), 1);
    assert_eq!(
        codes(&planned),
        vec!["depends-on-open", "depends-on-closed", "depends-on-missing"]
    );
}

#[test]
fn conflicting_edges_keep_the_earlier_rule_and_ask() {
    let mut a = pr("acme/api-core", 1, "feat/x", "master");
    a.body = "Depends-On: acme/front-web#2".to_string();
    let b = pr("acme/front-web", 2, "feat/x", "master");
    let planned = run(&config(""), vec![a, b], BTreeMap::new(), Publishers::new());
    assert!(edge(
        &planned,
        "acme/front-web#2",
        "acme/api-core#1",
        "depends-on"
    ));
    assert!(!edge(
        &planned,
        "acme/api-core#1",
        "acme/front-web#2",
        "layer"
    ));
    assert_eq!(codes(&planned), vec!["edge-conflict"]);
}

#[test]
fn unmapped_repo_on_a_shared_branch_is_a_question() {
    let a = pr("acme/api-core", 1, "feat/x", "master");
    let b = pr("acme/mystery", 2, "feat/x", "master");
    let planned = run(&config(""), vec![a, b], BTreeMap::new(), Publishers::new());
    assert_eq!(codes(&planned), vec!["layer-unknown"]);
    assert_eq!(planned.questions[0].pr.as_deref(), Some("acme/mystery#2"));
}

#[test]
fn different_branches_in_different_repos_run_in_parallel() {
    let a = pr("acme/api-core", 1, "feat/a", "master");
    let b = pr("acme/front-web", 2, "feat/b", "master");
    let planned = run(&config(""), vec![a, b], BTreeMap::new(), Publishers::new());
    assert_eq!(planned.lanes.len(), 2);
    assert_eq!(planned.peak_parallel, 2);
    assert!(planned.items.iter().all(|i| i.after.is_empty()));
}

#[test]
fn package_pin_in_config_beats_scanning() {
    let mut consumer = pr("acme/front-web", 5, "feat/a", "master");
    consumer.files = vec![ChangedFile {
        path: "package.json".to_string(),
        patch: Some("-    \"@acme/kit\": \"^1.0.0\",\n+    \"@acme/kit\": \"^1.1.0\",".to_string()),
    }];
    let publisher = pr("acme/front-kit", 6, "feat/b", "master");
    let cfg = config("[packages]\n\"@acme/kit\" = \"acme/front-kit\"\n");
    let planned = run(
        &cfg,
        vec![consumer, publisher],
        BTreeMap::new(),
        Publishers::new(),
    );
    assert!(edge(
        &planned,
        "acme/front-kit#6",
        "acme/front-web#5",
        "package"
    ));
}

#[test]
fn package_with_two_publishers_is_ambiguous_but_stays_serial() {
    let mut consumer = pr("acme/front-web", 5, "feat/a", "master");
    consumer.files = vec![ChangedFile {
        path: "package.json".to_string(),
        patch: Some("-    \"@acme/kit\": \"^1.0.0\",\n+    \"@acme/kit\": \"^1.1.0\",".to_string()),
    }];
    let one = pr("acme/front-kit", 6, "feat/b", "master");
    let two = pr("acme/front-kit-fork", 7, "feat/c", "master");
    let mut publishers = Publishers::new();
    publishers.insert(
        "@acme/kit".to_string(),
        [
            "acme/front-kit".to_string(),
            "acme/front-kit-fork".to_string(),
        ]
        .into(),
    );
    let planned = run(
        &config(""),
        vec![consumer, one, two],
        BTreeMap::new(),
        publishers,
    );
    assert!(edge(
        &planned,
        "acme/front-kit#6",
        "acme/front-web#5",
        "package"
    ));
    assert!(edge(
        &planned,
        "acme/front-kit-fork#7",
        "acme/front-web#5",
        "package"
    ));
    assert_eq!(codes(&planned), vec!["package-ambiguous"]);
}

#[test]
fn infra_repo_is_a_gate_and_waits_for_nothing_else() {
    let infra = pr("acme/infra", 1, "feat/a", "master");
    let api = pr("acme/api-core", 2, "feat/b", "master");
    let cfg = config("[deploy]\ninfra_repos = [\"acme/infra\"]\n");
    let planned = run(&cfg, vec![infra, api], BTreeMap::new(), Publishers::new());
    assert_eq!(planned.gate, vec!["acme/infra#1".to_string()]);
    assert!(edge(&planned, "acme/infra#1", "acme/api-core#2", "gate"));
}

#[test]
fn custom_target_group_serializes_two_repos() {
    let a = pr("acme/api-one", 1, "feat/a", "master");
    let b = pr("acme/api-two", 2, "feat/b", "master");
    let cfg = config("[targets]\n\"acme/api-one\" = \"shared\"\n\"acme/api-two\" = \"shared\"\n");
    let planned = run(&cfg, vec![b, a], BTreeMap::new(), Publishers::new());
    assert!(edge(&planned, "acme/api-one#1", "acme/api-two#2", "target"));
}

#[test]
fn truncated_file_list_is_a_question() {
    let mut a = pr("acme/api-core", 1, "feat/a", "master");
    a.files_complete = false;
    let planned = run(&config(""), vec![a], BTreeMap::new(), Publishers::new());
    assert_eq!(codes(&planned), vec!["files-truncated"]);
}

#[test]
fn discovery_requires_ready_prs_and_warns_about_unready_siblings() {
    let cfg = config("");
    let ready = pr("acme/api-core", 1, "feat/shared", "master");
    let mut draft = pr("acme/front-web", 2, "feat/shared", "master");
    draft.is_draft = true;
    draft.review = Review::None;
    let mut red = pr("acme/api-core", 3, "deploy/29-10-2026", "master");
    red.checks = Checks::Failure;
    let lonely = pr("acme/api-core", 4, "feat/lonely", "master");
    let found = discovery::discover(vec![ready, draft, red, lonely], &cfg, "deploy/29-10-2026");
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.ready[0].1, Scope::SharedBranch);
    assert_eq!(found.skipped.len(), 2);
    assert_eq!(found.questions.len(), 1);
    assert_eq!(found.questions[0].code, "sibling-not-ready");
}

#[test]
fn unready_pr_into_the_train_warns_the_train_pr_of_its_repo() {
    let cfg = config("");
    let train = pr("acme/api-core", 10, "deploy/29-10-2026", "master");
    let mut feature = pr("acme/api-core", 9, "feat/x", "deploy/29-10-2026");
    feature.review = Review::ReviewRequired;
    let found = discovery::discover(vec![train, feature], &cfg, "deploy/29-10-2026");
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.questions.len(), 1);
    assert_eq!(found.questions[0].code, "stacked-not-ready");
    assert_eq!(found.questions[0].pr.as_deref(), Some("acme/api-core#10"));
}

#[test]
fn bot_branches_and_other_trains_are_not_shared_branches() {
    let cfg = config("");
    let all = vec![
        pr("acme/api-core", 1, "dependabot/npm/x", "master"),
        pr("acme/front-web", 2, "dependabot/npm/x", "master"),
        pr("acme/api-core", 3, "deploy/03-11-2026", "master"),
        pr("acme/front-web", 4, "deploy/03-11-2026", "master"),
    ];
    let found = discovery::discover(all, &cfg, "deploy/29-10-2026");
    assert!(found.ready.is_empty());
    assert!(found.skipped.is_empty());
}

#[test]
fn explicit_prs_that_are_not_ready_are_skipped_with_the_reason() {
    let cfg = config("");
    let mut merged = pr("acme/api-core", 1, "feat/a", "master");
    merged.state = PrState::Merged;
    let mut no_checks = pr("acme/api-core", 2, "feat/b", "master");
    no_checks.checks = Checks::Missing;
    let found = discovery::explicit(vec![merged, no_checks], &cfg);
    assert!(found.ready.is_empty());
    assert_eq!(found.skipped[0].reasons, vec!["already merged".to_string()]);
    assert_eq!(
        found.skipped[1].reasons,
        vec!["no checks reported".to_string()]
    );
}

#[test]
fn missing_checks_are_accepted_for_listed_repos_only() {
    let cfg = config("checks_optional_repos = [\"acme/infra\"]");
    let mut infra = pr("acme/infra", 1, "feat/a", "master");
    infra.checks = Checks::Missing;
    let mut api = pr("acme/api-core", 2, "feat/b", "master");
    api.checks = Checks::Missing;
    let found = discovery::explicit(vec![infra, api], &cfg);
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.ready[0].0.key.repo, "infra");
    assert_eq!(
        found.skipped[0].reasons,
        vec!["no checks reported".to_string()]
    );
}

#[test]
fn gate_reports_the_file_that_triggered_it() {
    let mut a = pr("acme/api-core", 1, "feat/a", "master");
    a.files = vec![ChangedFile {
        path: "db/migrations/001_add.sql".to_string(),
        patch: None,
    }];
    let planned = run(&config(""), vec![a], BTreeMap::new(), Publishers::new());
    assert_eq!(
        planned.items[0].gate_reason.as_deref(),
        Some("db/migrations/001_add.sql")
    );
}

#[test]
fn unknown_mergeability_is_skipped_with_the_reason() {
    let cfg = config("");
    let mut unknown = pr("acme/api-core", 1, "feat/a", "master");
    unknown.mergeable_unknown = true;
    let found = discovery::explicit(vec![unknown], &cfg);
    assert!(found.ready.is_empty());
    assert_eq!(
        found.skipped[0].reasons,
        vec!["mergeability unknown, retry in a minute".to_string()]
    );
}

#[test]
fn missing_checks_are_accepted_when_the_config_says_so() {
    let cfg = config("allow_missing_checks = true");
    let mut no_checks = pr("acme/api-core", 2, "feat/b", "master");
    no_checks.checks = Checks::Missing;
    let found = discovery::explicit(vec![no_checks], &cfg);
    assert_eq!(found.ready.len(), 1);
}
