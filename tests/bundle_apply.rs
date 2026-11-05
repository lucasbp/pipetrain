use std::collections::BTreeMap;
use std::time::Duration;

mod common;

use common::{Server, serve_handler as serve};
use pipetrain::command::apply::{
    Gate, Kind, Outcome, Running, Status, apply_plan, gate, render, render_with,
};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::github::Client;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const DEPLOY: &str = "deploy/30-10-2026";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const REF: &str = "GET /repos/acme/api-core/git/ref/heads/master";
const CREATE: &str = "POST /repos/acme/api-core/git/refs";
const BRANCH: &str = "deploy/30-10-2026-master";
const PROBE: &str = "GET /repos/acme/api-core/git/ref/heads/deploy/30-10-2026-master";
const FIRST: &str = "PATCH /repos/acme/api-core/pulls/1";
const SECOND: &str = "PATCH /repos/acme/api-core/pulls/2";
const STOPPED: &str = "stopped after an earlier failure";

fn pr(number: u64) -> Pr {
    Pr {
        key: PrKey::new("acme", "api-core", number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/acme/api-core/pull/{number}"),
        body: String::new(),
        head_ref: format!("feat/{number}"),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: String::new(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        behind: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn two_prs() -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T10:00:00-03:00".to_string(),
            train: DEPLOY.to_string(),
            deploy_branch: DEPLOY.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: vec![
                (pr(1), "explicit".to_string()),
                (pr(2), "explicit".to_string()),
            ],
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

impl Server {
    fn lines(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                let first = request.lines().next().unwrap_or_default();
                first.trim_end_matches(" HTTP/1.1").to_string()
            })
            .collect()
    }

    fn body(&self, line: &str) -> Value {
        let seen = self.seen.lock().unwrap();
        let request = seen
            .iter()
            .find(|request| request.starts_with(&format!("{line} ")))
            .unwrap();
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }

    fn writes(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| !line.starts_with("GET "))
            .collect()
    }
}

fn happy(line: &str) -> (u16, Value) {
    if line.contains("/git/ref/heads/deploy/") {
        (404, json!({"message": "Not Found"}))
    } else if line.starts_with("GET ") {
        (200, json!({"object": {"sha": SHA}}))
    } else if line.starts_with("POST ") {
        (201, json!({}))
    } else {
        (200, json!({}))
    }
}

fn fail_on(
    target: &'static str,
    status: u16,
    message: &'static str,
) -> impl Fn(&str) -> (u16, Value) + Send + 'static {
    move |line| {
        if line == target {
            (status, json!({"message": message}))
        } else {
            happy(line)
        }
    }
}

fn client(server: &Server) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url("ghp_FAKE0123456789".to_string(), &cfg, false, &server.base)
}

fn run(server: &Server, plan: &Plan, apply: bool, answer: bool) -> Outcome {
    let mut client = client(server);
    apply_plan(
        plan,
        &mut client,
        apply,
        &mut |_: &str| answer,
        &mut |_, _| {},
    )
    .unwrap()
}

fn actions(outcome: &Outcome) -> Vec<(Kind, &str, Status, &str)> {
    outcome.report.bundles[0]
        .actions
        .iter()
        .map(|a| (a.kind, a.target.as_str(), a.status, a.detail.as_str()))
        .collect()
}

#[test]
fn a_dry_run_only_reads_the_base_ref() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);

    assert_eq!(server.lines(), vec![REF, PROBE]);
    assert!(!outcome.report.applied);
    assert!(!outcome.failed && !outcome.aborted);
    assert!(
        outcome.report.bundles[0]
            .actions
            .iter()
            .all(|a| a.status == Status::Planned)
    );
    let text = render(&outcome.report);
    assert!(text.starts_with("dry run, nothing was written"));
    assert!(text.contains(&format!("create branch {BRANCH} from master (0123456)")));
    assert!(text.contains(&format!("retarget acme/api-core#1: master -> {BRANCH}")));
    assert!(text.contains(&format!("retarget acme/api-core#2: master -> {BRANCH}")));
}

#[test]
fn apply_creates_the_branch_then_retargets_each_pr_in_order() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(server.lines(), vec![REF, PROBE, CREATE, FIRST, SECOND]);
    assert_eq!(
        server.body(CREATE),
        json!({"ref": format!("refs/heads/{BRANCH}"), "sha": SHA})
    );
    assert_eq!(server.body(FIRST), json!({"base": BRANCH}));
    assert_eq!(server.body(SECOND), json!({"base": BRANCH}));
    assert!(outcome.report.applied);
    assert!(!outcome.failed && !outcome.aborted);
    assert_eq!(
        actions(&outcome),
        vec![
            (Kind::CreateBranch, BRANCH, Status::Done, ""),
            (Kind::Retarget, "acme/api-core#1", Status::Done, ""),
            (Kind::Retarget, "acme/api-core#2", Status::Done, ""),
        ]
    );
    assert!(!render(&outcome.report).contains("dry run"));
}

#[test]
fn declining_the_confirmation_writes_nothing() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, false);

    assert!(outcome.aborted);
    assert!(!outcome.failed);
    assert!(!outcome.report.applied);
    assert!(server.writes().is_empty());
}

#[test]
fn an_existing_branch_is_skipped_and_the_retargets_still_run() {
    let server = serve(fail_on(CREATE, 422, "Reference already exists"));
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(
        server.lines(),
        vec![REF, PROBE, CREATE, PROBE, FIRST, SECOND]
    );
    assert!(!outcome.failed);
    assert_eq!(
        actions(&outcome),
        vec![
            (
                Kind::CreateBranch,
                BRANCH,
                Status::Skipped,
                "branch already exists"
            ),
            (Kind::Retarget, "acme/api-core#1", Status::Done, ""),
            (Kind::Retarget, "acme/api-core#2", Status::Done, ""),
        ]
    );
}

fn assert_stops_at_first_patch(status: u16, message: &'static str) {
    let server = serve(fail_on(FIRST, status, message));
    let outcome = run(&server, &two_prs(), true, true);

    assert_eq!(server.lines(), vec![REF, PROBE, CREATE, FIRST]);
    assert!(outcome.failed);
    assert!(outcome.report.applied);
    let found = actions(&outcome);
    assert_eq!(found[0].2, Status::Done);
    assert_eq!(found[1].2, Status::Failed);
    assert!(found[1].3.contains(message), "detail: {}", found[1].3);
    assert_eq!(
        found[2],
        (Kind::Retarget, "acme/api-core#2", Status::Skipped, STOPPED)
    );
}

#[test]
fn a_422_on_a_retarget_stops_the_rest() {
    assert_stops_at_first_patch(422, "Validation Failed");
}

#[test]
fn a_403_on_a_retarget_stops_the_rest() {
    assert_stops_at_first_patch(403, "Resource not accessible by integration");
}

type Seen = Vec<(Running, Vec<Status>)>;

fn observed(server: &Server, apply: bool) -> (Outcome, Seen) {
    let mut client = client(server);
    let mut seen: Seen = Vec::new();
    let outcome = apply_plan(
        &two_prs(),
        &mut client,
        apply,
        &mut |_: &str| true,
        &mut |report, running| {
            let statuses = report.bundles[0].actions.iter().map(|a| a.status).collect();
            seen.push((running, statuses));
        },
    )
    .unwrap();
    (outcome, seen)
}

#[test]
fn the_observer_sees_every_step_before_and_after_it_runs() {
    use Status::{Done, Planned};
    let server = serve(happy);
    let (_, seen) = observed(&server, true);

    assert_eq!(
        seen,
        vec![
            (None, vec![Planned, Planned, Planned]),
            (Some((0, 0)), vec![Planned, Planned, Planned]),
            (None, vec![Done, Planned, Planned]),
            (Some((0, 1)), vec![Done, Planned, Planned]),
            (None, vec![Done, Done, Planned]),
            (Some((0, 2)), vec![Done, Done, Planned]),
            (None, vec![Done, Done, Done]),
        ]
    );
}

#[test]
fn the_observer_sees_the_rest_skipped_after_a_failure() {
    use Status::{Done, Failed, Planned, Skipped};
    let server = serve(fail_on(FIRST, 422, "Validation Failed"));
    let (outcome, seen) = observed(&server, true);

    assert!(outcome.failed);
    assert_eq!(
        seen,
        vec![
            (None, vec![Planned, Planned, Planned]),
            (Some((0, 0)), vec![Planned, Planned, Planned]),
            (None, vec![Done, Planned, Planned]),
            (Some((0, 1)), vec![Done, Planned, Planned]),
            (None, vec![Done, Failed, Skipped]),
        ]
    );
}

#[test]
fn the_observer_is_not_called_with_a_step_on_a_dry_run() {
    let server = serve(happy);
    let (_, seen) = observed(&server, false);

    assert!(seen.iter().all(|(running, _)| running.is_none()));
}

#[test]
fn a_pr_already_on_the_branch_is_skipped_without_a_request() {
    let mut plan = two_prs();
    plan.items
        .iter_mut()
        .find(|item| item.id == "acme/api-core#1")
        .unwrap()
        .base = BRANCH.to_string();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(server.lines(), vec![REF, PROBE, CREATE, SECOND]);
    assert!(!outcome.failed);
    assert_eq!(
        actions(&outcome)[1],
        (
            Kind::Retarget,
            "acme/api-core#1",
            Status::Skipped,
            format!("already based on {BRANCH}").as_str()
        )
    );
    assert_eq!(actions(&outcome)[2].2, Status::Done);
}

#[test]
fn an_existing_train_pr_means_no_read_and_no_branch_creation() {
    let mut plan = two_prs();
    plan.bundles[0].existing_train_pr = Some("acme/api-core#9".to_string());
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(server.lines(), vec![FIRST, SECOND]);
    assert!(
        actions(&outcome)
            .iter()
            .all(|a| a.0 == Kind::Retarget && a.2 == Status::Done)
    );
    assert!(render(&outcome.report).contains(&format!(
        "branch exists: {BRANCH} is the head of acme/api-core#9"
    )));
}

fn assert_refused(branch: &str) {
    let mut plan = two_prs();
    plan.bundles[0].branch = branch.to_string();
    let server = serve(happy);
    let mut client = client(&server);
    let error = apply_plan(
        &plan,
        &mut client,
        true,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap_err();

    assert!(error.to_string().contains("refusing to bundle"));
    assert!(server.writes().is_empty());
}

#[test]
fn a_branch_equal_to_the_base_is_refused() {
    assert_refused("master");
}

#[test]
fn an_empty_branch_is_refused() {
    assert_refused("");
}

#[test]
fn the_json_report_has_the_documented_shape() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);
    let value = serde_json::to_value(&outcome.report).unwrap();

    assert_eq!(value["schema"], 1);
    assert_eq!(value["applied"], true);
    let bundle = &value["bundles"][0];
    assert_eq!(bundle["repo"], "acme/api-core");
    assert_eq!(bundle["base"], "master");
    assert_eq!(bundle["branch"], BRANCH);
    assert!(bundle.get("note").is_none());
    let list = bundle["actions"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["kind"], "create-branch");
    assert_eq!(list[0]["target"], BRANCH);
    assert_eq!(list[1]["kind"], "retarget");
    assert_eq!(list[1]["target"], "acme/api-core#1");
    for action in list {
        assert_eq!(action["status"], "done");
        assert_eq!(action["detail"], "");
        assert!(action.get("step").is_none());
        assert!(action.get("note").is_none());
    }
}

#[test]
fn no_bundles_renders_nothing_to_bundle() {
    let mut plan = two_prs();
    plan.bundles.clear();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert!(server.lines().is_empty());
    assert_eq!(render(&outcome.report), "nothing to bundle\n");
}

#[test]
fn the_gate_follows_its_truth_table() {
    assert_eq!(gate(false, false, true, None), Gate::DryRun);
    assert_eq!(gate(false, true, false, None), Gate::DryRun);
    assert_eq!(gate(true, true, false, None), Gate::Proceed);
    assert_eq!(gate(true, false, false, None), Gate::Refuse);
    assert_eq!(gate(true, false, true, None), Gate::Ask);
    assert_eq!(gate(true, false, true, Some("yes")), Gate::Proceed);
    assert_eq!(gate(true, false, true, Some(" yes \n")), Gate::Proceed);
    assert_eq!(gate(true, false, true, Some("y")), Gate::Abort);
    assert_eq!(gate(true, false, true, Some("")), Gate::Abort);
    assert_eq!(gate(true, false, true, Some("no")), Gate::Abort);
}

fn mixed_report() -> Outcome {
    let server = serve(happy);
    let mut outcome = run(&server, &two_prs(), true, true);
    let extra = run(&server, &two_prs(), true, true)
        .report
        .bundles
        .remove(0)
        .actions
        .pop()
        .unwrap();
    let list = &mut outcome.report.bundles[0].actions;
    list.push(extra);
    list[1].status = Status::Skipped;
    list[2].status = Status::Failed;
    outcome
}

#[test]
fn the_summary_counts_the_actions_by_status() {
    let outcome = mixed_report();
    let text = render(&outcome.report);

    assert!(
        text.ends_with("\n\n2 done, 1 skipped, 1 failed\n"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn the_summary_ends_with_the_elapsed_time_when_given() {
    let outcome = mixed_report();
    let text = render_with(&outcome.report, false, Some(Duration::from_millis(1500)));

    assert!(
        text.ends_with("2 done, 1 skipped, 1 failed in 1.5 s\n"),
        "{text}"
    );
}

#[test]
fn a_dry_run_has_no_summary() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);
    let text = render_with(&outcome.report, false, Some(Duration::from_millis(10)));

    assert!(!text.contains(" done"), "{text}");
    assert!(!text.contains("skipped,"), "{text}");
}

#[test]
fn the_plain_render_has_no_escape_codes_and_matches_the_default() {
    let outcome = mixed_report();

    assert_eq!(
        render_with(&outcome.report, false, None),
        render(&outcome.report)
    );
}

#[test]
fn the_colored_dry_run_banner_is_yellow() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), false, true);
    let text = render_with(&outcome.report, true, None);

    assert!(
        text.starts_with(
            "\u{1b}[33mdry run, nothing was written; pass --apply to do it\u{1b}[0m\n\n"
        ),
        "{text}"
    );
    assert!(text.contains("\u{1b}[33mcreate branch\u{1b}[0m"), "{text}");
    assert!(
        text.contains(&format!("\u{1b}[36m{BRANCH}\u{1b}[0m")),
        "{text}"
    );
    assert!(text.contains("\u{1b}[1macme/api-core#1\u{1b}[0m"), "{text}");
}

#[test]
fn the_colored_statuses_use_green_dim_and_red() {
    let outcome = mixed_report();
    let text = render_with(&outcome.report, true, None);

    assert!(text.contains("\u{1b}[32mdone:\u{1b}[0m"), "{text}");
    assert!(text.contains("\u{1b}[2mskipped:\u{1b}[0m"), "{text}");
    assert!(text.contains("\u{1b}[1;31mfailed:\u{1b}[0m"), "{text}");
    assert!(
        text.contains(
            "\u{1b}[32m2 done\u{1b}[0m, \u{1b}[2m1 skipped\u{1b}[0m, \u{1b}[1;31m1 failed\u{1b}[0m"
        ),
        "{text}"
    );
}

#[test]
fn zero_failures_are_dim() {
    let server = serve(happy);
    let outcome = run(&server, &two_prs(), true, true);
    let text = render_with(&outcome.report, true, None);

    assert!(text.contains("\u{1b}[2m0 failed\u{1b}[0m"), "{text}");
}

#[test]
fn the_colored_nothing_to_bundle_is_dim() {
    let mut plan = two_prs();
    plan.bundles.clear();
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert_eq!(
        render_with(&outcome.report, true, None),
        "\u{1b}[2mnothing to bundle\u{1b}[0m\n"
    );
}

fn plan_on(prs: &[(u64, &str)]) -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T10:00:00-03:00".to_string(),
            train: DEPLOY.to_string(),
            deploy_branch: DEPLOY.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: prs
                .iter()
                .map(|(number, base)| {
                    let mut item = pr(*number);
                    item.base_ref = base.to_string();
                    (item, "explicit".to_string())
                })
                .collect(),
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

#[test]
fn two_bases_that_flatten_to_the_same_suffix_are_refused() {
    let plan = plan_on(&[
        (1, "feature/x"),
        (2, "feature/x"),
        (3, "feature-x"),
        (4, "feature-x"),
    ]);
    let server = serve(happy);
    let mut client = client(&server);
    let error = apply_plan(
        &plan,
        &mut client,
        false,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap_err();

    let text = error.to_string();
    assert!(text.contains("refusing to bundle acme/api-core"), "{text}");
    assert!(text.contains("would both use the branch"), "{text}");
    assert!(server.writes().is_empty());
}

#[test]
fn two_bases_in_one_repo_get_their_own_branch_and_a_note() {
    let plan = plan_on(&[
        (1, "master"),
        (2, "master"),
        (3, "feature-x"),
        (4, "feature-x"),
    ]);
    let server = serve(happy);
    let outcome = run(&server, &plan, false, true);

    assert!(!outcome.failed && !outcome.aborted);
    let branches: Vec<&str> = outcome
        .report
        .bundles
        .iter()
        .map(|b| b.branch.as_str())
        .collect();
    assert_eq!(branches.len(), 2);
    assert_ne!(branches[0], branches[1]);
    assert!(
        branches.contains(&"deploy/30-10-2026-master"),
        "{branches:?}"
    );
    assert!(
        branches.contains(&"deploy/30-10-2026-feature-x"),
        "{branches:?}"
    );
    assert!(render(&outcome.report).contains("named after its base"));
    assert!(server.writes().is_empty());
}

const OTHER: &str = "9999999999999999999999999999999999999999";
const PULLS: &str = "POST /repos/acme/api-core/pulls";
const COMPARE: &str = "GET /repos/acme/api-core/compare/";
const LIST_PULLS: &str = "GET /repos/acme/api-core/pulls?";

fn existing_branch(status: &'static str) -> impl Fn(&str) -> (u16, Value) + Send + 'static {
    move |line| {
        if line == PROBE {
            (200, json!({"object": {"sha": OTHER}}))
        } else if line.starts_with(COMPARE) {
            (200, json!({"status": status}))
        } else {
            happy(line)
        }
    }
}

fn refusal(server: &Server, plan: &Plan, apply: bool) -> String {
    let mut client = client(server);
    apply_plan(
        plan,
        &mut client,
        apply,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap_err()
    .to_string()
}

#[test]
fn two_bases_in_one_repo_each_retarget_only_their_own_prs() {
    let plan = plan_on(&[
        (41, "feat/x"),
        (42, "feat/x"),
        (35, "master"),
        (43, "master"),
    ]);
    let server = serve(happy);
    let outcome = run(&server, &plan, true, true);

    assert!(!outcome.failed);
    assert_eq!(outcome.report.bundles.len(), 2);
    let targets = |branch: &str| -> Vec<String> {
        outcome
            .report
            .bundles
            .iter()
            .find(|b| b.branch == branch)
            .unwrap_or_else(|| panic!("no bundle {branch}"))
            .actions
            .iter()
            .filter(|a| a.kind == Kind::Retarget)
            .map(|a| a.target.clone())
            .collect()
    };
    assert_eq!(
        targets("deploy/30-10-2026-feat-x"),
        vec!["acme/api-core#41", "acme/api-core#42"]
    );
    assert_eq!(
        targets("deploy/30-10-2026-master"),
        vec!["acme/api-core#35", "acme/api-core#43"]
    );
    let creates = server
        .writes()
        .iter()
        .filter(|l| l.ends_with("/git/refs"))
        .count();
    assert_eq!(creates, 2);
}

#[test]
fn a_branch_from_another_base_is_refused_on_a_dry_run_and_on_apply() {
    for apply in [false, true] {
        let server = serve(existing_branch("diverged"));
        let text = refusal(&server, &two_prs(), apply);

        assert!(text.contains("refusing to reuse the branch"), "{text}");
        assert!(text.contains(BRANCH), "{text}");
        assert!(server.writes().is_empty());
    }
}

#[test]
fn a_branch_behind_its_base_is_refused_too() {
    let server = serve(existing_branch("behind"));
    let text = refusal(&server, &two_prs(), true);

    assert!(text.contains("refusing to reuse the branch"), "{text}");
    assert!(server.writes().is_empty());
}

#[test]
fn a_branch_that_contains_the_base_tip_is_reused() {
    for status in ["ahead", "identical"] {
        let server = serve(existing_branch(status));
        let outcome = run(&server, &two_prs(), true, true);

        assert!(!outcome.failed, "{status}");
        assert_eq!(server.writes(), vec![FIRST, SECOND]);
        assert_eq!(
            actions(&outcome)[0],
            (
                Kind::CreateBranch,
                BRANCH,
                Status::Skipped,
                "branch already exists"
            )
        );
    }
}

#[test]
fn a_branch_that_appears_between_the_read_and_the_create_is_checked_again() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let probes = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&probes);
    let server = serve(move |line| {
        if line == PROBE {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                (404, json!({"message": "Not Found"}))
            } else {
                (200, json!({"object": {"sha": OTHER}}))
            }
        } else if line == CREATE {
            (422, json!({"message": "Reference already exists"}))
        } else if line.starts_with(COMPARE) {
            (200, json!({"status": "diverged"}))
        } else {
            happy(line)
        }
    });
    let outcome = run(&server, &two_prs(), true, true);

    assert!(outcome.failed);
    let found = actions(&outcome);
    assert_eq!(found[0].2, Status::Failed);
    assert!(found[0].3.contains("refusing to reuse the branch"));
    assert_eq!(found[1].2, Status::Skipped);
    assert_eq!(found[2].2, Status::Skipped);
    assert_eq!(server.writes(), vec![CREATE]);
}

fn run_open(server: &Server, plan: &Plan, apply: bool) -> Outcome {
    let mut client = client(server);
    pipetrain::command::apply::apply_plan_with(
        plan,
        &mut client,
        apply,
        true,
        &mut |_: &str| true,
        &mut |_, _| {},
    )
    .unwrap()
}

#[test]
fn open_pr_is_the_last_write_of_the_bundle_and_never_merges() {
    let server = serve(happy);
    let outcome = run_open(&server, &two_prs(), true);

    assert!(!outcome.failed);
    assert_eq!(server.writes(), vec![CREATE, FIRST, SECOND, PULLS]);
    assert_eq!(
        server.body(PULLS),
        json!({
            "title": BRANCH,
            "head": BRANCH,
            "base": "master",
            "body": "Deploy of 2 PRs into `master`:\n\n- acme/api-core#1\n- acme/api-core#2\n"
        })
    );
    assert_eq!(
        actions(&outcome)[3],
        (Kind::OpenPr, BRANCH, Status::Done, "")
    );
}

#[test]
fn open_pr_on_a_dry_run_writes_nothing_and_is_listed() {
    let server = serve(happy);
    let outcome = run_open(&server, &two_prs(), false);

    assert!(server.writes().is_empty());
    assert!(render(&outcome.report).contains(&format!("open PR {BRANCH} -> master")));
}

#[test]
fn an_already_open_deploy_pr_is_not_duplicated() {
    let server = serve(|line| {
        if line.starts_with(LIST_PULLS) {
            (200, json!([{"number": 77}]))
        } else {
            happy(line)
        }
    });
    let outcome = run_open(&server, &two_prs(), true);

    assert!(!server.writes().contains(&PULLS.to_string()));
    let last = actions(&outcome).pop().unwrap();
    assert_eq!(last.0, Kind::OpenPr);
    assert_eq!(last.2, Status::Skipped);
    assert!(last.3.contains("acme/api-core#77"), "{}", last.3);
}

#[test]
fn an_existing_train_pr_means_no_new_deploy_pr() {
    let mut plan = two_prs();
    plan.bundles[0].existing_train_pr = Some("acme/api-core#9".to_string());
    let server = serve(happy);
    let outcome = run_open(&server, &plan, true);

    assert_eq!(server.writes(), vec![FIRST, SECOND]);
    let last = actions(&outcome).pop().unwrap();
    assert_eq!(last.2, Status::Skipped);
    assert!(last.3.contains("acme/api-core#9"));
}

#[test]
fn a_failed_retarget_skips_the_deploy_pr() {
    let server = serve(fail_on(FIRST, 422, "Validation Failed"));
    let outcome = run_open(&server, &two_prs(), true);

    assert!(outcome.failed);
    assert!(!server.writes().contains(&PULLS.to_string()));
    let last = actions(&outcome).pop().unwrap();
    assert_eq!(last, (Kind::OpenPr, BRANCH, Status::Skipped, STOPPED));
}

#[test]
fn a_422_saying_the_pr_exists_is_skipped_not_failed() {
    let server = serve(fail_on(
        PULLS,
        422,
        "A pull request already exists for acme:deploy/30-10-2026-master.",
    ));
    let outcome = run_open(&server, &two_prs(), true);

    assert!(!outcome.failed);
    let last = actions(&outcome).pop().unwrap();
    assert_eq!(last.2, Status::Skipped);
    assert_eq!(last.3, "deploy PR already open");
}

#[test]
fn without_open_pr_no_pull_request_is_listed_or_created() {
    let server = serve(happy);
    run(&server, &two_prs(), true, true);

    assert!(!server.lines().iter().any(|l| l.contains("/pulls?")));
    assert!(!server.writes().contains(&PULLS.to_string()));
}
