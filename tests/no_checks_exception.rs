use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::command::merge::compact::preview;
use pipetrain::config::{Config, Rules};
use pipetrain::discovery;
use pipetrain::merge::{
    ApiEstimate, Kind, REPORT_SCHEMA, Report, StatePreview, Status, StepReport, Summary,
};
use pipetrain::model::{Checks, Pr};
use pipetrain::plan::{self, Plan};
use pipetrain::profile::ProfileSet;
use pipetrain::publishers::Publishers;

fn config(extra: &str) -> Config {
    Config::parse(&format!("orgs = [\"acme\"]\n{extra}\n")).unwrap()
}

fn without_checks(slug: &str, number: u64) -> Pr {
    let mut pr = pr(slug, number, "feat/a", "master");
    pr.checks = Checks::Missing;
    pr
}

fn plan_of(cfg: &Config, prs: Vec<Pr>) -> Plan {
    let rules = Rules::new(cfg).unwrap();
    let found = discovery::explicit(prs, cfg);
    plan::build(
        &rules,
        plan::Gathered {
            generated_at: "2026-11-18T09:00:00-03:00".to_string(),
            train: "deploy/18-11-2026".to_string(),
            deploy_branch: "deploy/18-11-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: found
                .ready
                .into_iter()
                .map(|(pr, scope)| (pr, scope.as_str().to_string()))
                .collect(),
            skipped: found.skipped,
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn readiness<'a>(plan: &'a Plan, id: &str) -> &'a pipetrain::model::Readiness {
    plan.preflight.iter().find(|r| r.id == id).unwrap()
}

#[test]
fn strict_by_default_blocks_a_pr_with_no_checks() {
    let plan = plan_of(&config(""), vec![without_checks("acme/api", 1)]);
    let entry = readiness(&plan, "acme/api#1");
    assert_eq!(entry.status, "blocked");
    assert_eq!(entry.causes, vec!["no checks reported".to_string()]);
    assert!(entry.exceptions.is_empty());
}

#[test]
fn a_listed_repo_is_eligible_and_the_exception_is_visible() {
    let cfg = config("checks_optional_repos = [\"acme/infra\"]");
    let plan = plan_of(
        &cfg,
        vec![
            without_checks("acme/infra", 1),
            without_checks("acme/api", 2),
        ],
    );
    let infra = readiness(&plan, "acme/infra#1");
    assert_eq!(infra.status, "eligible");
    assert_eq!(infra.exceptions.len(), 1);
    let lines = infra.lines().join("\n");
    assert!(
        lines.contains("acme/infra#1: exception: no checks reported"),
        "{lines}"
    );
    let api = readiness(&plan, "acme/api#2");
    assert_eq!(api.status, "blocked");
    assert!(api.exceptions.is_empty());
}

#[test]
fn the_exception_is_an_additive_json_field() {
    let cfg = config("allow_missing_checks = true");
    let plan = plan_of(&cfg, vec![without_checks("acme/api", 1)]);
    let value = serde_json::to_value(&plan).unwrap();
    let entry = &value["preflight"][0];
    assert_eq!(entry["status"], "eligible");
    assert_eq!(entry["exceptions"].as_array().unwrap().len(), 1);
    let strict = plan_of(&config(""), vec![pr("acme/api", 3, "feat/a", "master")]);
    let value = serde_json::to_value(&strict).unwrap();
    assert!(value["preflight"][0].get("exceptions").is_none());
}

#[test]
fn pending_or_failing_checks_still_block_when_missing_checks_are_allowed() {
    let cfg = config("allow_missing_checks = true");
    let mut pending = pr("acme/api", 1, "feat/a", "master");
    pending.checks = Checks::Pending;
    let mut failing = pr("acme/api", 2, "feat/b", "master");
    failing.checks = Checks::Failure;
    let plan = plan_of(&cfg, vec![pending, failing]);
    for id in ["acme/api#1", "acme/api#2"] {
        let entry = readiness(&plan, id);
        assert_eq!(entry.status, "blocked", "{id}");
        assert!(entry.exceptions.is_empty(), "{id}");
    }
}

#[test]
fn a_branch_rule_that_requires_a_check_still_blocks() {
    let cfg = config("allow_missing_checks = true");
    let mut required = without_checks("acme/api", 1);
    required.merge_state_status = "BLOCKED".to_string();
    let plan = plan_of(&cfg, vec![required]);
    let entry = readiness(&plan, "acme/api#1");
    assert_eq!(entry.status, "blocked");
    assert!(
        entry
            .causes
            .iter()
            .any(|cause| cause.contains("branch protection blocks merge")),
        "{:?}",
        entry.causes
    );
    assert!(entry.exceptions.is_empty());
}

#[test]
fn the_option_is_inherited_and_overridden_per_profile() {
    let set = ProfileSet::parse(
        "allow_missing_checks = true\n[profile.acme]\norgs = ['acme']\n[profile.globex]\norgs = ['globex']\nallow_missing_checks = false\nchecks_optional_repos = ['globex/infra']\n",
    )
    .unwrap();
    let acme = &set.find("acme").unwrap().config;
    let globex = &set.find("globex").unwrap().config;
    assert!(acme.allows_missing_checks("acme/api"));
    assert!(!globex.allows_missing_checks("globex/api"));
    assert!(globex.allows_missing_checks("globex/infra"));
}

#[test]
fn the_compact_dry_run_marks_the_exception_on_the_row() {
    let cfg = config("allow_missing_checks = true");
    let plan = plan_of(
        &cfg,
        vec![
            without_checks("acme/api", 1),
            pr("acme/web", 2, "feat/w", "master"),
        ],
    );
    let steps = vec![
        StepReport::new(Kind::Merge, "acme/api#1", Status::Planned, "into master"),
        StepReport::new(Kind::Merge, "acme/web#2", Status::Planned, "into master"),
    ];
    let report = Report {
        api_usage: Default::default(),
        api_estimate: ApiEstimate::default(),
        max_parallel: 4,
        summary: Summary::from_steps(&steps),
        preflight: Vec::new(),
        state: StatePreview {
            path: None,
            file_age_seconds: None,
            file_age: "new".to_string(),
            created_at: None,
            updated_at: None,
            profile: None,
            selected_profile: None,
            queue_id: None,
            stale_pending_hours: 24,
            pending: Vec::new(),
        },
        bump_enabled: false,
        bump_candidates: Vec::new(),
        schema: REPORT_SCHEMA,
        applied: false,
        requested: false,
        stopped: None,
        steps,
        not_merging: Vec::new(),
        questions: Vec::new(),
        notices: Vec::new(),
        held: Vec::new(),
    };
    let text = anstream::adapter::strip_str(&preview(&report, &plan, false)).to_string();
    let api = text.lines().find(|line| line.contains("api#1")).unwrap();
    let web = text.lines().find(|line| line.contains("web#2")).unwrap();
    assert!(
        api.contains("no checks reported (allowed by config)"),
        "{text}"
    );
    assert!(!web.contains("no checks"), "{text}");
}
