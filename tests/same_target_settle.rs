use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod common;
use common::{Server, pr, serve_requests};
use pipetrain::command::merge::compact::{Context, enrich, final_report};
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::github::Client;
use pipetrain::merge::{DependencyFailure, Report, Status, StepReport, Store};
use pipetrain::model::Pr;
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const TRAIN: &str = "deploy/05-11-2026";
const ACTIVE: &str = "The Deployment Group acme-api-group already has an active Deployment";

struct Script {
    bases: BTreeMap<u64, String>,
    merged: BTreeSet<u64>,
    runs: Vec<Value>,
    jobs: Value,
    annotations: Value,
}

fn merge_sha(number: u64) -> String {
    format!("{:040x}", 0x1000 + number)
}

fn run_json(conclusion: &str) -> Value {
    json!({
        "name": "deploy",
        "path": ".github/workflows/deploy.yml",
        "status": "completed",
        "conclusion": conclusion,
        "html_url": "https://example.test/acme/api/actions/runs/101",
    })
}

impl Script {
    fn node(&self, number: u64) -> Value {
        let merged = self.merged.contains(&number);
        json!({
            "number": number,
            "title": format!("change {number}"),
            "url": format!("https://github.com/acme/api/pull/{number}"),
            "body": "",
            "headRefName": format!("feat/{number}"),
            "baseRefName": self.bases.get(&number).map_or("main", String::as_str),
            "baseRef": {"target": {"oid": "b".repeat(40)}},
            "headRefOid": format!("{number:040x}"),
            "isDraft": false,
            "state": if merged { "MERGED" } else { "OPEN" },
            "reviewDecision": "APPROVED",
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "author": {"login": "octocat"},
            "labels": {"nodes": []},
            "repository": {"name": "api", "owner": {"login": "acme"}},
            "latestOpinionatedReviews": {"nodes": [{"state": "APPROVED"}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "SUCCESS",
                "contexts": {"nodes": []}
            }}}]},
        })
    }

    fn live(&self, number: u64) -> Value {
        let merged = self.merged.contains(&number);
        let mut body = json!({
            "state": if merged { "closed" } else { "open" },
            "merged": merged,
            "head": {"sha": format!("{number:040x}")},
            "base": {"ref": self.bases.get(&number).map_or("main", String::as_str)},
            "mergeable_state": "clean",
            "changed_files": 1,
        });
        if merged {
            body["merge_commit_sha"] = json!(merge_sha(number));
        }
        body
    }
}

fn respond(script: &Mutex<Script>, line: &str, payload: &str) -> (u16, Value) {
    let mut script = script.lock().unwrap();
    let normalized = line.split_once("/repos/acme/").and_then(|(prefix, path)| {
        path.split_once('/')
            .map(|(_, suffix)| format!("{prefix}/repos/acme/api/{suffix}"))
    });
    let line = normalized.as_deref().unwrap_or(line);
    if line == "POST /graphql" {
        let body: Value = serde_json::from_str(payload).unwrap();
        if body.pointer("/variables/e0").is_some() {
            return (
                200,
                json!({"data": {"repository": {"f0": {"text": "on: push"}}}}),
            );
        }
        let number = body
            .pointer("/variables/n")
            .and_then(Value::as_u64)
            .unwrap();
        let mut node = script.node(number);
        node["repository"]["name"] = body["variables"]["r"].clone();
        node["repository"]["owner"]["login"] = body["variables"]["o"].clone();
        return (200, json!({"data": {"repository": {"pullRequest": node}}}));
    }
    if let Some(rest) = line.strip_prefix("PUT /repos/acme/api/pulls/") {
        let number: u64 = rest.trim_end_matches("/merge").parse().unwrap();
        script.merged.insert(number);
        return (200, json!({"sha": merge_sha(number), "merged": true}));
    }
    if line.starts_with("GET /repos/acme/api/git/trees/") {
        return (
            200,
            json!({"tree": [{"type": "blob", "path": ".github/workflows/deploy.yml"}], "truncated": false}),
        );
    }
    if line.contains("/files?per_page=100&page=") {
        return (
            200,
            json!([{"filename": "src/app.rs", "status": "modified"}]),
        );
    }
    if line.starts_with("GET /repos/acme/api/actions/runs/101/jobs") {
        return (200, script.jobs.clone());
    }
    if line.starts_with("GET /repos/acme/api/check-runs/7001/annotations") {
        return (200, script.annotations.clone());
    }
    if line.starts_with("GET /repos/acme/api/actions/runs") {
        let runs = script.runs.clone();
        return (
            200,
            json!({"total_count": runs.len(), "workflow_runs": runs}),
        );
    }
    if let Some(rest) = line.strip_prefix("GET /repos/acme/api/pulls/") {
        return (200, script.live(rest.parse().unwrap()));
    }
    (404, json!({"message": "not found"}))
}

struct Run {
    phases: Vec<String>,
    outcome: Outcome,
    slept: Duration,
}

struct Rig {
    clock: Arc<Mutex<Instant>>,
    script: Arc<Mutex<Script>>,
    server: Server,
    store: Store,
    cfg: Config,
}

impl Rig {
    fn new(extra: &str) -> Self {
        let script = Arc::new(Mutex::new(Script {
            bases: BTreeMap::new(),
            merged: BTreeSet::new(),
            runs: vec![run_json("success")],
            jobs: json!({"jobs": []}),
            annotations: json!([]),
        }));
        let shared = Arc::clone(&script);
        let server = serve_requests(move |line, payload| respond(&shared, line, payload));
        let mut cfg = Config::parse(&format!(
            "orgs = [\"acme\"]\n[merge]\npoll_seconds = 1\ntimeout_minutes = 30\nno_run_grace_seconds = 3\n{extra}"
        ))
        .unwrap();
        cfg.max_parallel = 1;
        Self {
            clock: Arc::new(Mutex::new(Instant::now())),
            script,
            server,
            store: Store::in_memory(),
            cfg,
        }
    }

    fn client(&self) -> Client {
        let api = ApiConfig {
            max_requests: 100,
            max_per_minute: 1000,
            min_remaining: 0,
        };
        Client::with_timeout(
            "ghp_FAKE0123456789".to_string(),
            &api,
            false,
            &self.server.base,
            Duration::from_secs(40),
        )
    }

    fn run(&mut self, plan: &Plan, settings: Settings) -> Run {
        let mut client = self.client();
        let mut slept = Duration::ZERO;
        let mut phases = Vec::new();
        let outcome = {
            let mut confirm = |_: &str| true;
            let mut gate = |_: &str| true;
            let mut sleep = |pause: Duration| {
                slept += pause;
                *self.clock.lock().unwrap() += pause;
            };
            let mut observe = |_: &StepReport| {};
            let mut progress = |text: &str| phases.push(text.to_string());
            let now = || *self.clock.lock().unwrap();
            let mut hooks = Hooks {
                confirm: &mut confirm,
                gate: &mut gate,
                sleep: &mut sleep,
                now: &now,
                observe: &mut observe,
                progress: &mut progress,
                runner: None,
            };
            merge_plan(
                plan,
                &self.cfg,
                &mut client,
                &settings,
                &mut self.store,
                &mut hooks,
            )
            .unwrap()
        };
        Run {
            phases,
            outcome,
            slept,
        }
    }

    fn go(&mut self, plan: &Plan) -> Run {
        self.run(plan, applied())
    }
}

fn applied() -> Settings {
    Settings {
        apply: true,
        gates_confirmed: false,
        allow_questions: false,
        method: None,
        timeout: None,
        feed_registry: None,
        open_pr: false,
        dependency_failure: DependencyFailure::Stop,
        bump: false,
        bump_options: Default::default(),
        bump_behind: None,
    }
}

fn dry() -> Settings {
    Settings {
        apply: false,
        ..applied()
    }
}

fn plan_of(ready: Vec<Pr>) -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-11-05T10:00:00-03:00".to_string(),
            train: TRAIN.to_string(),
            deploy_branch: TRAIN.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: ready
                .into_iter()
                .map(|item| (item, "explicit".to_string()))
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

fn same_target() -> Plan {
    plan_of(vec![
        pr("acme/api", 1, "feat/1", "main"),
        pr("acme/api", 2, "feat/2", "main"),
    ])
}

fn settling(run: &Run) -> Vec<String> {
    run.phases
        .iter()
        .filter_map(|text| text.find("settling ").map(|at| text[at..].to_string()))
        .collect()
}

fn statuses(run: &Run) -> Vec<Status> {
    run.outcome
        .report
        .steps
        .iter()
        .map(|step| step.status)
        .collect()
}

fn baseline(plan: &Plan) -> Duration {
    let mut rig = Rig::new("");
    let run = rig.go(plan);
    assert!(settling(&run).is_empty());
    run.slept
}

#[test]
fn the_second_pr_on_the_same_target_waits_for_the_window() {
    let plan = same_target();
    let base = baseline(&plan);
    let mut rig = Rig::new("same_target_settle_seconds = 120\n");
    let run = rig.go(&plan);
    assert_eq!(statuses(&run), vec![Status::Done, Status::Done]);
    let waits = settling(&run);
    assert_eq!(waits.len(), 1, "{:?}", run.phases);
    assert!(
        waits[0].starts_with("settling 120s before merging acme/api#2"),
        "{:?}",
        run.phases
    );
    assert_eq!(run.slept, base + Duration::from_secs(120));
    let first = run
        .phases
        .iter()
        .position(|text| text.contains("merged acme/api#1"))
        .unwrap();
    let wait = run
        .phases
        .iter()
        .position(|text| text.contains("settling "))
        .unwrap();
    let second = run
        .phases
        .iter()
        .position(|text| text.contains("merged acme/api#2"))
        .unwrap();
    assert!(first < wait && wait < second, "{:?}", run.phases);
}

#[test]
fn a_different_base_or_repo_does_not_wait() {
    let plan = plan_of(vec![
        pr("acme/api", 1, "feat/1", "main"),
        pr("acme/api", 2, "feat/2", "release"),
        pr("acme/web", 3, "feat/3", "main"),
    ]);
    let mut rig = Rig::new("same_target_settle_seconds = 120\n");
    rig.script.lock().unwrap().bases.insert(2, "release".into());
    let run = rig.go(&plan);
    assert_eq!(
        statuses(&run),
        vec![Status::Done, Status::Done, Status::Done]
    );
    assert!(settling(&run).is_empty(), "{:?}", run.phases);
}

#[test]
fn the_default_of_zero_changes_nothing() {
    let plan = same_target();
    let mut explicit = Rig::new("same_target_settle_seconds = 0\n");
    let mut unset = Rig::new("");
    let first = explicit.go(&plan);
    let second = unset.go(&plan);
    assert!(settling(&first).is_empty());
    assert!(settling(&second).is_empty());
    assert_eq!(first.slept, second.slept);
    let json = serde_json::to_string(&second.outcome.report).unwrap();
    assert!(!json.contains("settle_seconds"));
}

#[test]
fn a_repo_override_replaces_the_default() {
    let plan = same_target();
    let mut rig = Rig::new(
        "same_target_settle_seconds = 120\n[merge.repo_same_target_settle_seconds]\n\"ACME/api\" = 45\n",
    );
    let run = rig.go(&plan);
    assert!(
        settling(&run)[0].starts_with("settling 45s before merging acme/api#2"),
        "{:?}",
        run.phases
    );

    let mut rig = Rig::new(
        "[merge.repo_same_target_settle_seconds]\n\"acme/api\" = 90\n\"acme/api@main\" = 30\n",
    );
    let run = rig.go(&plan);
    assert!(
        settling(&run)[0].starts_with("settling 30s before"),
        "{:?}",
        run.phases
    );

    let mut rig = Rig::new(
        "[merge.repo_same_target_settle_seconds]\n\"acme/api\" = 90\n\"acme/api@release\" = 30\n",
    );
    let run = rig.go(&plan);
    assert!(
        settling(&run)[0].starts_with("settling 90s before"),
        "{:?}",
        run.phases
    );

    let mut rig = Rig::new(
        "same_target_settle_seconds = 120\n[merge.repo_same_target_settle_seconds]\n\"acme/api\" = 0\n",
    );
    let run = rig.go(&plan);
    assert!(settling(&run).is_empty(), "{:?}", run.phases);
}

#[test]
fn the_wait_never_passes_the_deploy_timeout() {
    let plan = same_target();
    let base = baseline(&plan);
    let mut rig = Rig::new("same_target_settle_seconds = 600\n");
    rig.cfg.merge.timeout_minutes = 1;
    let run = rig.go(&plan);
    assert!(
        settling(&run)[0].starts_with("settling 60s before"),
        "{:?}",
        run.phases
    );
    assert_eq!(run.slept, base + Duration::from_secs(60));
    assert_eq!(statuses(&run), vec![Status::Done, Status::Done]);

    let mut rig = Rig::new("same_target_settle_seconds = 600\n");
    let run = rig.run(
        &plan,
        Settings {
            timeout: Some(Duration::from_secs(20)),
            ..applied()
        },
    );
    assert!(
        settling(&run)[0].starts_with("settling 20s before"),
        "{:?}",
        run.phases
    );
}

#[test]
fn a_deploy_that_is_not_confirmed_opens_no_window() {
    let plan = same_target();
    let mut rig = Rig::new("same_target_settle_seconds = 120\nno_deploy_repos = [\"acme/api\"]\n");
    let run = rig.go(&plan);
    assert_eq!(statuses(&run), vec![Status::Done, Status::Done]);
    assert!(settling(&run).is_empty(), "{:?}", run.phases);
}

#[test]
fn the_dry_run_marks_only_the_steps_that_follow_a_merge_on_the_same_target() {
    let plan = plan_of(vec![
        pr("acme/api", 1, "feat/1", "main"),
        pr("acme/api", 2, "feat/2", "main"),
        pr("acme/web", 3, "feat/3", "main"),
    ]);
    let mut on = Rig::new("same_target_settle_seconds = 120\n");
    let mut off = Rig::new("");
    let with = on.run(&plan, dry()).outcome.report;
    let without = off.run(&plan, dry()).outcome.report;
    let seconds: BTreeMap<String, Option<u64>> = with
        .steps
        .iter()
        .map(|step| (step.target.clone(), step.settle_seconds))
        .collect();
    assert_eq!(
        seconds,
        BTreeMap::from([
            ("acme/api#1".to_string(), None),
            ("acme/api#2".to_string(), Some(120)),
            ("acme/web#3".to_string(), None),
        ])
    );
    assert!(
        without
            .steps
            .iter()
            .all(|step| step.settle_seconds.is_none())
    );

    let mut changed = serde_json::to_value(&with).unwrap();
    let plain = serde_json::to_value(&without).unwrap();
    let steps = changed["steps"].as_array_mut().unwrap();
    let marked = steps
        .iter()
        .position(|step| step["target"] == json!("acme/api#2"))
        .unwrap();
    assert_eq!(steps[marked]["settle_seconds"], json!(120));
    assert_eq!(
        steps
            .iter()
            .filter(|step| step.get("settle_seconds").is_some())
            .count(),
        1
    );
    steps[marked]
        .as_object_mut()
        .unwrap()
        .remove("settle_seconds");
    changed["api_estimate"] = plain["api_estimate"].clone();
    assert_eq!(changed, plain);
    assert!(plain.to_string().find("settle_seconds").is_none());
}

#[test]
fn the_dry_run_text_says_settling() {
    let plan = same_target();
    let mut rig = Rig::new("same_target_settle_seconds = 120\n");
    let report = rig.run(&plan, dry()).outcome.report;
    let compact = pipetrain::command::merge::compact::preview(&report, &plan, false);
    let line = compact
        .lines()
        .find(|line| line.contains("api#2"))
        .unwrap()
        .to_string();
    assert!(line.contains("settling 120s"), "{compact}");
    assert!(
        !compact
            .lines()
            .find(|line| line.contains("api#1"))
            .unwrap()
            .contains("settling"),
        "{compact}"
    );
    let verbose = pipetrain::command::merge::render(&report, false);
    assert!(verbose.contains("; settling 120s"), "{verbose}");
}

fn failed_report() -> (Rig, Report) {
    let plan = plan_of(vec![pr("acme/api", 2, "feat/2", "main")]);
    let mut rig = Rig::new("");
    rig.script.lock().unwrap().runs = vec![run_json("failure")];
    let run = rig.go(&plan);
    assert_eq!(statuses(&run), vec![Status::Failed]);
    (rig, run.outcome.report)
}

fn context(detail: Option<&str>) -> Context {
    let mut context = Context {
        command: "pipetrain merge --apply --state \"q.json\"".to_string(),
        state: Some("q.json".to_string()),
        details: BTreeMap::new(),
    };
    if let Some(detail) = detail {
        context
            .details
            .insert("acme/api#2".to_string(), detail.to_string());
    }
    context
}

#[test]
fn an_active_deployment_failure_is_explained_with_the_resume_command() {
    let (_, report) = failed_report();
    let detail = format!("job Deploy, step Start CodeDeploy; {ACTIVE}").to_uppercase();
    let text = final_report(&report, false, &context(Some(&detail)));
    assert!(
        text.contains("the previous deployment on this target was still active"),
        "{text}"
    );
    assert!(text.contains("the merge went in at"), "{text}");
    assert!(
        text.contains("then resume: pipetrain merge --apply --state \"q.json\""),
        "{text}"
    );
    assert!(text.contains("merge.same_target_settle_seconds"), "{text}");
    assert!(text.contains("JOB DEPLOY, STEP START CODEDEPLOY"), "{text}");
    assert!(
        !text.contains("unknown: whether production changed"),
        "{text}"
    );
}

#[test]
fn other_deploy_failures_keep_the_usual_block() {
    let (_, report) = failed_report();
    let text = final_report(&report, false, &context(Some("job Deploy, step Build")));
    assert!(!text.contains("previous deployment"), "{text}");
    assert!(!text.contains("same_target_settle_seconds"), "{text}");
    assert!(
        text.contains("unknown: whether production changed"),
        "{text}"
    );
}

#[test]
fn the_job_annotations_are_read_to_recognise_an_active_deployment() {
    let (rig, report) = failed_report();
    {
        let mut script = rig.script.lock().unwrap();
        script.jobs = json!({"jobs": [{
            "id": 7001,
            "name": "Deploy",
            "conclusion": "failure",
            "steps": [{"name": "Start CodeDeploy", "conclusion": "failure"}],
        }]});
        script.annotations = json!([
            {"message": "Process completed with exit code 1."},
            {"message": ACTIVE},
        ]);
    }
    let mut client = rig.client();
    let details = enrich(&mut client, &report);
    let detail = details.get("acme/api#2").expect("job detail");
    assert!(
        detail.starts_with("job Deploy, step Start CodeDeploy; "),
        "{detail}"
    );
    let text = final_report(
        &report,
        false,
        &Context {
            details,
            ..context(None)
        },
    );
    assert!(
        text.contains("the previous deployment on this target was still active"),
        "{text}"
    );
}

#[test]
fn without_the_marker_in_the_annotations_the_detail_stays_the_job_and_step() {
    let (rig, report) = failed_report();
    {
        let mut script = rig.script.lock().unwrap();
        script.jobs = json!({"jobs": [{
            "id": 7001,
            "name": "Deploy",
            "conclusion": "failure",
            "steps": [{"name": "Build", "conclusion": "failure"}],
        }]});
        script.annotations = json!([{"message": "Process completed with exit code 1."}]);
    }
    let mut client = rig.client();
    let details = enrich(&mut client, &report);
    assert_eq!(details["acme/api#2"], "job Deploy, step Build");
}
