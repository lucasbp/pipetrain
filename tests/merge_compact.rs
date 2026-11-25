use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::command::merge::compact::{
    Context, command_line, describe_jobs, final_report, finished_line, parse_run, plain_cause,
    preview, short,
};
use pipetrain::config::{Config, Rules};
use pipetrain::merge::{
    ApiEstimate, DeployOutcome, ExpectedRun, Kind, NotMerging, PendingPreview, REPORT_SCHEMA,
    Report, StatePreview, Status, StepReport, Summary,
};
use pipetrain::model::ChangedFile;
use pipetrain::plan::{self, Plan};
use pipetrain::publishers::Publishers;
use serde_json::json;

fn build_plan() -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let mut migration = pr("acme/api", 2, "feat/a", "master");
    migration.files = vec![ChangedFile {
        path: "db/migrations/001.sql".to_string(),
        patch: None,
    }];
    plan::build(
        &rules,
        plan::Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: vec![
                (
                    pr("acme/lib", 1, "feat/lib", "master"),
                    "explicit".to_string(),
                ),
                (migration, "explicit".to_string()),
                (
                    pr("acme/web", 3, "feat/web", "master"),
                    "explicit".to_string(),
                ),
            ],
            skipped: Vec::new(),
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

fn state_preview(pending: Vec<PendingPreview>) -> StatePreview {
    StatePreview {
        path: Some("state.json".to_string()),
        file_age_seconds: None,
        file_age: "new".to_string(),
        created_at: None,
        updated_at: None,
        profile: None,
        selected_profile: None,
        queue_id: None,
        stale_pending_hours: 24,
        pending,
    }
}

fn estimate(exceeds: bool) -> ApiEstimate {
    ApiEstimate {
        discovery: 10,
        checks: 20,
        deploy_polls: 30,
        total: 60,
        budget: if exceeds { 50 } else { 5000 },
        exceeds_budget: exceeds,
        rate_remaining: None,
        warn: exceeds,
    }
}

fn report(steps: Vec<StepReport>, not_merging: Vec<NotMerging>) -> Report {
    Report {
        api_estimate: estimate(false),
        max_parallel: 4,
        summary: Summary::from_steps(&steps),
        preflight: Vec::new(),
        state: state_preview(Vec::new()),
        bump_enabled: false,
        bump_candidates: Vec::new(),
        schema: REPORT_SCHEMA,
        applied: false,
        requested: false,
        stopped: None,
        steps,
        not_merging,
        questions: Vec::new(),
        notices: Vec::new(),
        held: Vec::new(),
    }
}

fn merge_step(id: &str) -> StepReport {
    StepReport::new(Kind::Merge, id, Status::Planned, "into master")
}

fn merged(id: &str, outcome: DeployOutcome) -> StepReport {
    let mut step = StepReport::new(Kind::Merge, id, Status::Done, "into master");
    step.merge_sha = Some("a1b2c3d4e5f6a7b8".to_string());
    step.deploy_outcome = Some(outcome);
    step.deploy = Some("deploy confirmed".to_string());
    step
}

fn deploy_failed(id: &str, url: &str) -> StepReport {
    let mut step = StepReport::new(Kind::Merge, id, Status::Failed, "into master");
    step.merge_sha = Some("9f8e7d6c5b4a3210".to_string());
    step.deploy_outcome = Some(DeployOutcome::Failed);
    step.deploy = Some(format!("workflow Deploy ended as failure: {url}"));
    step.expected_runs = vec![ExpectedRun {
        path: ".github/workflows/deploy.yml".to_string(),
        name: "Deploy".to_string(),
        html_url: url.to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
    }];
    step
}

fn strip(text: &str) -> String {
    anstream::adapter::strip_str(text).to_string()
}

#[test]
fn the_preview_has_one_line_per_pr_and_no_check_lines_or_urls() {
    let plan = build_plan();
    let report = report(
        vec![
            merge_step("acme/lib#1"),
            merge_step("acme/api#2"),
            merge_step("acme/web#3"),
        ],
        vec![NotMerging::new(
            "acme/ui#9",
            vec!["review required".to_string(), "checks pending".to_string()],
        )],
    );
    let text = strip(&preview(&report, &plan, true));
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("- ") || line.starts_with(|c: char| c.is_ascii_digit())
        })
        .filter(|line| !line.contains("enter the queue"))
        .collect();
    assert_eq!(rows.len(), 4, "{text}");
    assert!(
        rows[0].contains("lib#1") && rows[0].contains("ready"),
        "{text}"
    );
    assert!(
        rows[3].contains("ui#9") && rows[3].contains("waiting"),
        "{text}"
    );
    assert!(rows[3].contains("needs human approval"), "{text}");
    assert!(!text.contains("http"), "{text}");
    assert!(!text.contains("check "), "{text}");
    assert!(!text.contains("acme/"), "{text}");
    assert!(text.contains("3 enter the queue"), "{text}");
    assert!(text.contains("1 waiting · 0 blocked"), "{text}");
    assert!(text.contains("dry run, nothing was merged"), "{text}");
    assert!(text.contains("--verbose"), "{text}");
}

#[test]
fn the_preview_names_the_gates_the_questions_and_the_inherited_state() {
    let plan = build_plan();
    let mut report = report(vec![merge_step("acme/api#2")], Vec::new());
    report.questions = vec!["? acme/web#3: which base?".to_string()];
    report.state = state_preview(vec![PendingPreview {
        id: "acme/old#7".to_string(),
        base: "master".to_string(),
        sha: None,
        recorded_at: None,
        age_seconds: None,
        age: "2 days".to_string(),
        stale: true,
    }]);
    let text = strip(&preview(&report, &plan, false));
    assert!(text.contains("gates: 1 DDL"), "{text}");
    assert!(text.contains("1 open questions"), "{text}");
    assert!(text.contains("which base?"), "{text}");
    assert!(
        text.contains("inherited state: 1 pending from earlier runs (old#7, 1 stale)"),
        "{text}"
    );
}

#[test]
fn the_budget_warning_appears_only_over_the_limit() {
    let plan = build_plan();
    let mut report = report(vec![merge_step("acme/api#2")], Vec::new());
    let quiet = strip(&preview(&report, &plan, false));
    assert!(!quiet.contains("API requests"), "{quiet}");
    report.api_estimate = estimate(true);
    let loud = strip(&preview(&report, &plan, false));
    let warning: Vec<&str> = loud
        .lines()
        .filter(|l| l.contains("API requests"))
        .collect();
    assert_eq!(warning.len(), 1, "{loud}");
    assert!(warning[0].contains("over the budget of 50"), "{loud}");
}

#[test]
fn a_dependency_shows_as_ordering_on_the_ready_line() {
    let cfg =
        Config::parse("orgs = [\"acme\"]\n[packages]\n\"@acme/kit\" = \"acme/kit\"\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let mut consumer = pr("acme/web", 5, "feat/gate", "master");
    consumer.files = vec![ChangedFile {
        path: "package.json".to_string(),
        patch: Some("-    \"@acme/kit\": \"^1.0.0\",\n+    \"@acme/kit\": \"^1.1.0\",".to_string()),
    }];
    let plan = plan::build(
        &rules,
        plan::Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: vec![
                (
                    pr("acme/kit", 6, "feat/kit", "master"),
                    "explicit".to_string(),
                ),
                (consumer, "explicit".to_string()),
            ],
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 0,
            rate_remaining: None,
        },
    );
    let report = report(
        vec![merge_step("acme/kit#6"), merge_step("acme/web#5")],
        Vec::new(),
    );
    let text = strip(&preview(&report, &plan, false));
    let web = text.lines().find(|line| line.contains("web#5")).unwrap();
    assert!(web.contains("after kit#6"), "{text}");
}

#[test]
fn causes_are_plain_language() {
    for (raw, plain) in [
        ("review required", "needs human approval"),
        ("not approved", "needs human approval"),
        ("changes requested", "changes requested, address them first"),
        ("draft", "draft, mark it ready for review"),
        ("checks pending", "checks still running"),
        ("checks failing: build", "check build failing"),
        ("checks failing: build, lint", "checks build, lint failing"),
        ("checks failing", "checks failing"),
        ("no checks reported", "no checks configured in the repo"),
        ("merge conflict", "conflict with base"),
        ("merge conflict (DIRTY)", "conflict with base"),
        (
            "branch behind its base, update it",
            "branch behind its base, update it",
        ),
        (
            "acme/web#2 depends on acme/api#1, already merged",
            "depends on api#1 first",
        ),
        (
            "waiting for package acme.kit 1.2.0",
            "waiting for package acme.kit 1.2.0",
        ),
        ("something new", "something new"),
    ] {
        assert_eq!(plain_cause(raw), plain, "{raw}");
    }
    assert!(plain_cause("branch protection blocks merge (BLOCKED)").contains("branch protection"));
}

#[test]
fn package_release_behind_reads_in_plain_words() {
    let raw = "references acme-lib 1.0.0; the queue publishes 2 new versions of it (acme/lib#1, acme/lib#2), so this PR will be 2 behind and the repo check allows 1 (max-behind 1 in .github/workflows/ci.yml)";
    assert_eq!(
        plain_cause(raw),
        "queue publishes 2 versions of acme-lib, repo accepts at most 1 behind"
    );
    let one = "references acme-lib 1.0.0; the queue publishes 1 new version of it (acme/lib#1), so this PR will be 1 behind and the repo check allows 0 (merge.package_max_behind = 0 from the config)";
    assert_eq!(
        plain_cause(one),
        "queue publishes 1 version of acme-lib, repo accepts at most 0 behind"
    );
}

#[test]
fn short_ids_drop_the_org() {
    assert_eq!(short("acme/api#12"), "api#12");
    assert_eq!(short("gate"), "gate");
}

#[test]
fn each_finished_pr_is_one_line_with_its_own_color() {
    let ok = finished_line(&merged("acme/lib#1", DeployOutcome::Confirmed), true);
    let failed = finished_line(
        &deploy_failed("acme/api#2", "https://example.test/run/1"),
        true,
    );
    let mut halted = StepReport::new(Kind::Merge, "acme/web#3", Status::Skipped, "stopped");
    halted.blocked_by = Some("acme/api#2 depends on it".to_string());
    let stopped = finished_line(&halted, true);
    assert!(ok.contains("\x1b[32mmerged + deploy ok"), "{ok:?}");
    assert!(
        failed.contains("\x1b[1;31mmerged, deploy failed"),
        "{failed:?}"
    );
    assert!(stopped.contains("\x1b[33mstopped"), "{stopped:?}");
    assert!(ok.contains("\x1b[36mlib#1"), "{ok:?}");
    for line in [&ok, &failed, &stopped] {
        assert!(!line.contains('\n'), "{line:?}");
    }
    assert_eq!(strip(&ok), "lib#1  merged + deploy ok");
    assert!(strip(&failed).contains("workflow Deploy ended as failure"));
    assert!(!strip(&failed).contains("http"), "{failed}");
}

#[test]
fn without_color_there_are_no_escape_codes_and_the_text_is_the_same() {
    let plan = build_plan();
    let steps = vec![
        merged("acme/lib#1", DeployOutcome::Confirmed),
        deploy_failed("acme/api#2", "https://example.test/run/1"),
    ];
    let mut report = report(steps, Vec::new());
    report.applied = true;
    report.requested = true;
    let colored = final_report(&report, true, &Context::default());
    let plain = final_report(&report, false, &Context::default());
    assert!(colored.contains("\x1b["));
    assert!(!plain.contains('\x1b'), "{plain:?}");
    assert_eq!(strip(&colored), plain);
    let dry = preview(&report, &plan, false);
    assert!(!dry.contains('\x1b'));
    assert!(
        !finished_line(&merged("acme/lib#1", DeployOutcome::Confirmed), false).contains('\x1b')
    );
}

fn five_pr_report() -> Report {
    let url = "https://github.com/acme/api/actions/runs/101";
    let mut merge_failed = StepReport::new(
        Kind::Merge,
        "acme/job#5",
        Status::Failed,
        "merge conflict (DIRTY): resolve conflicts with the base branch, then retry",
    );
    merge_failed.deploy = None;
    let mut halted = StepReport::new(
        Kind::Merge,
        "acme/ui#6",
        Status::Skipped,
        "stopped after an earlier failure",
    );
    halted.blocked_by = Some("acme/api#2 depends on it".to_string());
    let steps = vec![
        merged("acme/lib#1", DeployOutcome::Confirmed),
        deploy_failed("acme/api#2", url),
        merged("acme/web#3", DeployOutcome::Confirmed),
        merged("acme/docs#4", DeployOutcome::Dispensed),
        merge_failed,
        halted,
    ];
    let mut report = report(steps, Vec::new());
    report.applied = true;
    report.requested = true;
    report
}

#[test]
fn the_final_report_leads_with_the_counts_in_bold() {
    let report = five_pr_report();
    let colored = final_report(&report, true, &Context::default());
    let first = colored.lines().next().unwrap();
    assert!(first.starts_with("\x1b[1m"), "{first:?}");
    assert_eq!(
        strip(first),
        "4 merged · 3 deploys ok · 2 failed · 1 stopped"
    );
}

#[test]
fn successful_prs_are_one_line_with_names_only() {
    let report = five_pr_report();
    let text = strip(&final_report(&report, true, &Context::default()));
    let ok: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("merged + deploy ok:"))
        .collect();
    assert_eq!(
        ok,
        vec!["merged + deploy ok: lib#1, web#3, docs#4"],
        "{text}"
    );
    assert!(!text.contains("a1b2c3d"), "{text}");
}

#[test]
fn a_failed_deploy_block_says_what_happened_what_is_unknown_and_the_next_command() {
    let report = five_pr_report();
    let mut context = Context {
        command: "pipetrain merge --apply --yes --state \"C:\\tmp\\q.json\"".to_string(),
        state: Some("C:\\tmp\\q.json".to_string()),
        details: BTreeMap::new(),
    };
    context.details.insert(
        "acme/api#2".to_string(),
        "job migrate, step Run migrations".to_string(),
    );
    let text = strip(&final_report(&report, true, &context));
    let at = text.find("api#2  merged, deploy failed").expect(&text);
    let block = &text[at..];
    assert!(block.contains("the merge went in at 9f8e7d6"), "{text}");
    assert!(block.contains("workflow Deploy ended as failure"), "{text}");
    assert!(block.contains("job migrate, step Run migrations"), "{text}");
    assert!(
        block.contains("run:     https://github.com/acme/api/actions/runs/101"),
        "{text}"
    );
    assert!(
        block.contains("unknown: whether production changed"),
        "{text}"
    );
    assert!(
        block.contains("next:    pipetrain merge --apply --yes --state"),
        "{text}"
    );
    assert!(
        block.contains("pipetrain state discard acme/api#2 --reason \"<why>\" --state"),
        "{text}"
    );
}

#[test]
fn a_merge_that_failed_before_merging_says_so_and_how_to_retry() {
    let report = five_pr_report();
    let context = Context {
        command: "pipetrain merge --apply".to_string(),
        ..Context::default()
    };
    let text = strip(&final_report(&report, true, &context));
    let at = text.find("job#5  failed, not merged").expect(&text);
    let block: Vec<&str> = text[at..].lines().take(3).collect();
    assert!(block[1].contains("merge conflict"), "{text}");
    assert!(
        block[2].contains("then run again: pipetrain merge --apply"),
        "{text}"
    );
}

#[test]
fn the_stopped_ones_are_named_with_the_cause() {
    let report = five_pr_report();
    let text = strip(&final_report(&report, true, &Context::default()));
    assert!(text.contains("stopped, not merged: ui#6"), "{text}");
    assert!(text.contains("api#2 depends on it"), "{text}");
}

#[test]
fn the_long_resume_paragraph_is_one_short_tip() {
    let report = five_pr_report();
    let text = strip(&final_report(&report, true, &Context::default()));
    assert!(!text.contains("Repeat the command"), "{text}");
    let tips: Vec<&str> = text.lines().filter(|l| l.starts_with("tip:")).collect();
    assert_eq!(tips.len(), 1, "{text}");
    assert!(tips[0].len() < 120, "{text}");
}

#[test]
fn a_clean_run_prints_only_the_counts_and_the_names() {
    let mut report = report(
        vec![
            merged("acme/lib#1", DeployOutcome::Confirmed),
            merged("acme/api#2", DeployOutcome::Confirmed),
        ],
        Vec::new(),
    );
    report.applied = true;
    report.requested = true;
    let text = strip(&final_report(&report, true, &Context::default()));
    assert_eq!(
        text,
        "2 merged · 2 deploys ok · 0 failed · 0 stopped\nmerged + deploy ok: lib#1, api#2\n"
    );
}

#[test]
fn the_resume_command_reuses_the_arguments_and_adds_the_state_when_missing() {
    let args: Vec<String> = ["merge", "--apply", "--org", "acme"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        command_line(&args, Some("/tmp/q.json")),
        "pipetrain merge --apply --org acme --state /tmp/q.json"
    );
    let with_state: Vec<String> = ["merge", "--state", "/tmp/q.json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        command_line(&with_state, Some("/tmp/q.json")),
        "pipetrain merge --state /tmp/q.json"
    );
    let spaced: Vec<String> = ["merge", "--state", "C:\\My Files\\q.json"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        command_line(&spaced, None),
        "pipetrain merge --state \"C:\\My Files\\q.json\""
    );
}

#[test]
fn the_run_url_and_the_failed_job_are_read_from_the_api_shapes() {
    assert_eq!(
        parse_run("https://github.com/acme/api/actions/runs/101/attempts/2"),
        Some(("acme/api".to_string(), 101))
    );
    assert_eq!(parse_run("https://example.test/nothing"), None);
    let jobs = json!({"jobs": [
        {"name": "lint", "conclusion": "success", "steps": []},
        {"name": "migrate", "conclusion": "failure", "steps": [
            {"name": "Checkout", "conclusion": "success"},
            {"name": "Run migrations", "conclusion": "failure"}
        ]}
    ]});
    assert_eq!(
        describe_jobs(&jobs).as_deref(),
        Some("job migrate, step Run migrations")
    );
    assert_eq!(
        describe_jobs(&json!({"jobs": [{"name": "x", "conclusion": "success"}]})),
        None
    );
}

#[test]
fn samples_for_the_pull_request_body() {
    let plan = build_plan();
    let mut dry = report(
        vec![
            merge_step("acme/lib#1"),
            merge_step("acme/api#2"),
            merge_step("acme/web#3"),
        ],
        vec![NotMerging::new(
            "acme/ui#9",
            vec!["review required".to_string()],
        )],
    );
    dry.api_estimate = estimate(true);
    println!(
        "--- compact preview ---\n{}",
        strip(&preview(&dry, &plan, false))
    );
    let mut context = Context {
        command: "pipetrain merge --apply --state q.json".to_string(),
        state: Some("q.json".to_string()),
        details: BTreeMap::new(),
    };
    context.details.insert(
        "acme/api#2".to_string(),
        "job migrate, step Run migrations".to_string(),
    );
    println!(
        "--- compact final report ---\n{}",
        strip(&final_report(&five_pr_report(), true, &context))
    );
}

#[test]
fn the_dry_run_preview_says_what_runs_together_by_stage() {
    let mut plan = build_plan();
    plan.deploy_resources
        .insert("acme/web".to_string(), vec!["runner:shared".to_string()]);
    plan.deploy_resources
        .insert("acme/lib".to_string(), vec!["runner:shared".to_string()]);
    plan.refresh_stages(&std::collections::BTreeMap::new());
    let report = report(vec![merge_step("acme/api#2")], Vec::new());
    let text = strip(&preview(&report, &plan, false));
    assert!(text.contains("stage 1: together"), "{text}");
    assert!(
        text.contains("waits for the shared deploy resource runner:shared"),
        "{text}"
    );
    assert!(!text.contains("acme/"), "{text}");
}
