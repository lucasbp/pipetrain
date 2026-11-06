use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

mod common;
use common::{Server, pr, serve_requests};
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan, render};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::emit;
use pipetrain::github::Client;
use pipetrain::merge::{EXIT_STOPPED, Kind, STOPPED, Status, Step, StepReport, Store};
use pipetrain::model::{ChangedFile, Pr, Question};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const TRAIN: &str = "deploy/05-11-2026";

struct Script {
    base: String,
    review: BTreeMap<u64, String>,
    live_state: BTreeMap<u64, String>,
    live_head: BTreeMap<u64, String>,
    put_status: u16,
    put_body: Option<Value>,
    put_merges: bool,
    merged: BTreeSet<u64>,
    runs: Vec<Value>,
    workflows: Vec<(String, String)>,
    tree_status: u16,
    branches: Vec<String>,
    open_prs: Vec<Value>,
    into_train: Vec<Value>,
    branch_missing: bool,
    post_status: u16,
    post_message: String,
}

fn merge_sha(number: u64) -> String {
    format!("{:040x}", 0x1000 + number)
}

fn run_json(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "html_url": format!("https://example.test/acme/api/actions/runs/{name}"),
    })
}

fn green() -> Vec<Value> {
    vec![run_json("deploy", "completed", Some("success"))]
}

impl Script {
    fn new() -> Self {
        Self {
            base: "main".to_string(),
            review: BTreeMap::new(),
            live_state: BTreeMap::new(),
            live_head: BTreeMap::new(),
            put_status: 200,
            put_body: None,
            put_merges: true,
            merged: BTreeSet::new(),
            runs: green(),
            workflows: Vec::new(),
            tree_status: 200,
            branches: vec!["main".to_string(), "feat/x".to_string()],
            open_prs: Vec::new(),
            into_train: Vec::new(),
            branch_missing: false,
            post_status: 201,
            post_message: String::new(),
        }
    }

    fn tree(&self) -> Value {
        let mut entries: Vec<Value> = self
            .workflows
            .iter()
            .map(|(name, _)| json!({"type": "blob", "path": format!(".github/workflows/{name}")}))
            .collect();
        entries.push(json!({"type": "blob", "path": "README.md"}));
        json!({"tree": entries, "truncated": false})
    }

    fn blobs(&self) -> Value {
        let mut repository = serde_json::Map::new();
        for (index, (_, text)) in self.workflows.iter().enumerate() {
            repository.insert(format!("f{index}"), json!({"text": text}));
        }
        json!({"data": {"repository": repository}})
    }

    fn node(&self, number: u64) -> Value {
        let merged = self.merged.contains(&number);
        let decision = self.review.get(&number).map_or("APPROVED", String::as_str);
        json!({
            "number": number,
            "title": format!("change {number}"),
            "url": format!("https://github.com/acme/api/pull/{number}"),
            "body": "",
            "headRefName": format!("feat/{number}"),
            "baseRefName": self.base,
            "baseRef": {"target": {"oid": "b".repeat(40)}},
            "headRefOid": format!("{number:040x}"),
            "isDraft": false,
            "state": if merged { "MERGED" } else { "OPEN" },
            "reviewDecision": decision,
            "mergeable": "MERGEABLE",
            "author": {"login": "octocat"},
            "labels": {"nodes": []},
            "repository": {"name": "api", "owner": {"login": "acme"}},
            "latestOpinionatedReviews": {"nodes": [{"state": decision}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "SUCCESS",
                "contexts": {"nodes": []}
            }}}]},
        })
    }

    fn live(&self, number: u64) -> Value {
        let merged = self.merged.contains(&number);
        let head = self
            .live_head
            .get(&number)
            .cloned()
            .unwrap_or_else(|| format!("{number:040x}"));
        let state = self.live_state.get(&number).map_or("clean", String::as_str);
        let mut body = json!({
            "state": if merged { "closed" } else { "open" },
            "merged": merged,
            "head": {"sha": head},
            "base": {"ref": self.base},
            "mergeable_state": state,
        });
        if merged {
            body["merge_commit_sha"] = json!(merge_sha(number));
        }
        body
    }
}

fn respond(script: &Mutex<Script>, line: &str, payload: &str) -> (u16, Value) {
    let mut script = script.lock().unwrap();
    if line == "POST /graphql" {
        let body: Value = serde_json::from_str(payload).unwrap();
        if body.pointer("/variables/e0").is_some() {
            return (200, script.blobs());
        }
        let number = body
            .pointer("/variables/n")
            .and_then(Value::as_u64)
            .unwrap();
        return (
            200,
            json!({"data": {"repository": {"pullRequest": script.node(number)}}}),
        );
    }
    if let Some(rest) = line.strip_prefix("PUT /repos/acme/api/pulls/") {
        let number: u64 = rest.trim_end_matches("/merge").parse().unwrap();
        if script.put_merges {
            script.merged.insert(number);
        }
        let body = script
            .put_body
            .clone()
            .unwrap_or_else(|| json!({"sha": merge_sha(number), "merged": true}));
        return (script.put_status, body);
    }
    if line.starts_with("GET /repos/acme/api/git/trees/") {
        if script.tree_status != 200 {
            return (script.tree_status, json!({"message": "unavailable"}));
        }
        return (200, script.tree());
    }
    if line.starts_with("GET /repos/acme/api/branches") {
        let names: Vec<Value> = script
            .branches
            .iter()
            .map(|name| json!({"name": name}))
            .collect();
        return (200, Value::Array(names));
    }
    if line.starts_with("GET /repos/acme/api/git/ref/heads/") {
        if script.branch_missing {
            return (404, json!({"message": "Not Found"}));
        }
        return (200, json!({"object": {"sha": "c".repeat(40)}}));
    }
    if line.starts_with("GET /repos/acme/api/pulls?") {
        if line.contains("&head=") {
            return (200, Value::Array(script.open_prs.clone()));
        }
        return (200, Value::Array(script.into_train.clone()));
    }
    if line == "POST /repos/acme/api/pulls" {
        return (
            script.post_status,
            json!({"message": script.post_message, "number": 99}),
        );
    }
    if let Some(rest) = line.strip_prefix("GET /repos/acme/api/pulls/") {
        return (200, script.live(rest.parse().unwrap()));
    }
    if line.starts_with("GET /repos/acme/api/actions/runs") {
        return (200, json!({"workflow_runs": script.runs}));
    }
    (404, json!({"message": "not found"}))
}

struct Run {
    outcome: Outcome,
    confirms: usize,
    gates: usize,
    slept: Duration,
    observed: usize,
}

impl Run {
    fn statuses(&self) -> Vec<Status> {
        self.outcome.report.steps.iter().map(|s| s.status).collect()
    }

    fn step(&self, index: usize) -> &StepReport {
        &self.outcome.report.steps[index]
    }

    fn said(&self, index: usize) -> String {
        let step = self.step(index);
        format!(
            "{} {}",
            step.detail,
            step.deploy.clone().unwrap_or_default()
        )
    }
}

struct Rig {
    script: Arc<Mutex<Script>>,
    server: Server,
    store: Store,
    cfg: Config,
}

impl Rig {
    fn new(extra: &str) -> Self {
        Self::with_top("", extra)
    }

    fn with_top(top: &str, extra: &str) -> Self {
        let script = Arc::new(Mutex::new(Script::new()));
        let shared = Arc::clone(&script);
        let server = serve_requests(move |line, payload| respond(&shared, line, payload));
        let cfg = Config::parse(&format!(
            "orgs = [\"acme\"]\n{top}[merge]\npoll_seconds = 1\ntimeout_minutes = 1\nno_run_grace_seconds = 3\n{extra}"
        ))
        .unwrap();
        Self {
            script,
            server,
            store: Store::in_memory(),
            cfg,
        }
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        self.script.lock().unwrap()
    }

    fn run(
        &mut self,
        plan: &Plan,
        settings: Settings,
        confirm: bool,
        gate: bool,
    ) -> anyhow::Result<Run> {
        let api = ApiConfig {
            max_requests: 50,
            max_per_minute: 1000,
            min_remaining: 0,
        };
        let mut client = Client::with_base_url(
            "ghp_FAKE0123456789".to_string(),
            &api,
            false,
            &self.server.base,
        );
        let mut confirms = 0;
        let mut gates = 0;
        let mut slept = Duration::ZERO;
        let mut observed = 0;
        let outcome = {
            let mut confirm_hook = |_: &str| {
                confirms += 1;
                confirm
            };
            let mut gate_hook = |_: &str| {
                gates += 1;
                gate
            };
            let mut sleep_hook = |pause: Duration| slept += pause;
            let mut observe_hook = |_: &StepReport| observed += 1;
            let mut hooks = Hooks {
                confirm: &mut confirm_hook,
                gate: &mut gate_hook,
                sleep: &mut sleep_hook,
                observe: &mut observe_hook,
            };
            merge_plan(
                plan,
                &self.cfg,
                &mut client,
                &settings,
                &mut self.store,
                &mut hooks,
            )?
        };
        Ok(Run {
            outcome,
            confirms,
            gates,
            slept,
            observed,
        })
    }

    fn go(&mut self, plan: &Plan) -> Run {
        self.run(plan, applied(), true, true).unwrap()
    }

    fn lines(&self) -> Vec<String> {
        self.server
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(" HTTP/1.1")
                    .to_string()
            })
            .collect()
    }

    fn puts(&self) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter(|line| line.starts_with("PUT "))
            .collect()
    }

    fn polls(&self) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.starts_with("GET /repos/acme/api/actions/runs"))
            .count()
    }

    fn body(&self, line: &str) -> Value {
        let seen = self.server.seen.lock().unwrap();
        let request = seen
            .iter()
            .find(|request| request.starts_with(&format!("{line} ")))
            .unwrap();
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap()
    }
}

fn applied() -> Settings {
    Settings {
        apply: true,
        gates_confirmed: false,
        allow_questions: false,
        method: None,
        timeout: None,
        open_pr: false,
    }
}

fn dry() -> Settings {
    Settings {
        apply: false,
        ..applied()
    }
}

fn prs(numbers: &[u64], base: &str) -> Vec<Pr> {
    numbers
        .iter()
        .map(|number| pr("acme/api", *number, &format!("feat/{number}"), base))
        .collect()
}

fn plan_of(ready: Vec<Pr>, questions: Vec<Question>) -> Plan {
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
            questions,
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn plan_on(numbers: &[u64]) -> Plan {
    plan_of(prs(numbers, "main"), Vec::new())
}

fn gated_plan() -> Plan {
    let mut ddl = pr("acme/api", 1, "feat/1", "main");
    ddl.files = vec![ChangedFile {
        path: "db/migrations/001_add.sql".to_string(),
        patch: None,
    }];
    plan_of(vec![ddl, pr("acme/api", 2, "feat/2", "main")], Vec::new())
}

fn expected(plan: &Plan) -> Vec<Step> {
    pipetrain::merge::steps(&emit::queue_lines(plan))
}

fn target_of(step: &Step) -> &str {
    match step {
        Step::Merge(text) | Step::Gate(text) | Step::Wait(text) => text,
    }
}

fn merge_numbers(plan: &Plan) -> Vec<u64> {
    expected(plan)
        .iter()
        .filter_map(|step| match step {
            Step::Merge(id) => id.rsplit('#').next().unwrap().parse().ok(),
            _ => None,
        })
        .collect()
}

fn position(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no request containing {needle} in {lines:?}"))
}

fn last_position(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .rposition(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no request containing {needle} in {lines:?}"))
}

#[test]
fn a_dry_run_lists_the_steps_in_queue_order_and_writes_nothing() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);

    let run = rig.run(&plan, dry(), true, true).unwrap();

    let targets: Vec<String> = run
        .outcome
        .report
        .steps
        .iter()
        .map(|step| step.target.clone())
        .collect();
    let queue: Vec<String> = expected(&plan)
        .iter()
        .map(|step| target_of(step).to_string())
        .collect();
    assert_eq!(targets, queue);
    assert_eq!(targets.len(), 2);
    assert!(
        run.statuses()
            .iter()
            .all(|status| *status == Status::Planned)
    );
    assert!(!run.outcome.report.applied);
    assert!(!run.outcome.aborted);
    assert_eq!(run.outcome.code, 0);
    assert_eq!(run.confirms, 0);
    assert!(rig.puts().is_empty());
    let text = render(&run.outcome.report, false);
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("pass --apply"), "{text}");
}

#[test]
fn two_ready_prs_merge_one_at_a_time_and_each_deploy_ends_before_the_next_merge() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let order = merge_numbers(&plan);

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert_eq!(run.outcome.code, 0);
    assert!(run.outcome.report.applied);
    assert_eq!(run.observed, 2);
    assert_eq!(run.confirms, 1);
    assert!(rig.store.state.pending.is_empty());
    let (first, second) = (order[0], order[1]);
    assert_eq!(run.step(0).merge_sha, Some(merge_sha(first)));
    assert_eq!(run.step(1).merge_sha, Some(merge_sha(second)));
    assert!(
        run.step(0)
            .deploy
            .as_deref()
            .unwrap()
            .contains("deploy passed")
    );
    let lines = rig.lines();
    let put_first = position(&lines, &format!("PUT /repos/acme/api/pulls/{first}/merge"));
    let put_second = position(&lines, &format!("PUT /repos/acme/api/pulls/{second}/merge"));
    let polls_first = format!("head_sha={}", merge_sha(first));
    assert!(put_first < position(&lines, &polls_first));
    assert!(last_position(&lines, &polls_first) < put_second);
    assert_eq!(
        rig.puts(),
        vec![
            format!("PUT /repos/acme/api/pulls/{first}/merge"),
            format!("PUT /repos/acme/api/pulls/{second}/merge"),
        ]
    );
}

#[test]
fn a_pr_that_lost_its_approval_is_not_merged_and_the_rest_are_skipped() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let first = merge_numbers(&plan)[0];
    rig.script()
        .review
        .insert(first, "CHANGES_REQUESTED".to_string());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed, Status::Skipped]);
    assert!(
        run.step(0).detail.contains("no longer ready"),
        "{}",
        run.said(0)
    );
    assert_eq!(run.step(1).detail, STOPPED);
    assert_eq!(run.outcome.code, 1);
    assert!(rig.puts().is_empty());
    assert!(rig.store.state.pending.is_empty());
}

#[test]
fn a_pr_without_any_review_is_not_merged_by_default() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    let first = merge_numbers(&plan)[0];
    rig.script().review.insert(first, String::new());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed]);
    assert!(
        run.step(0).detail.contains("not approved"),
        "{}",
        run.said(0)
    );
    assert!(rig.puts().is_empty());
}

#[test]
fn a_pr_without_any_review_is_merged_when_approval_is_not_required() {
    let mut rig = Rig::with_top("require_approval = false\n", "");
    let plan = plan_on(&[1]);
    let first = merge_numbers(&plan)[0];
    rig.script().review.insert(first, String::new());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done]);
    assert_eq!(rig.puts().len(), 1);
}

#[test]
fn changes_requested_still_blocks_the_merge_when_approval_is_not_required() {
    let mut rig = Rig::with_top("require_approval = false\n", "");
    let plan = plan_on(&[1]);
    let first = merge_numbers(&plan)[0];
    rig.script()
        .review
        .insert(first, "CHANGES_REQUESTED".to_string());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed]);
    assert!(
        run.step(0).detail.contains("changes requested"),
        "{}",
        run.said(0)
    );
    assert!(rig.puts().is_empty());
}

#[test]
fn a_branch_that_github_no_longer_calls_clean_is_not_merged() {
    for (state, words) in [
        ("blocked", "branch protection"),
        ("behind", "behind its base"),
        ("dirty", "merge conflict"),
    ] {
        let mut rig = Rig::new("");
        let plan = plan_on(&[1, 2]);
        let first = merge_numbers(&plan)[0];
        rig.script().live_state.insert(first, state.to_string());

        let run = rig.go(&plan);

        assert_eq!(
            run.statuses(),
            vec![Status::Failed, Status::Skipped],
            "{state}"
        );
        assert!(
            run.step(0).detail.contains(words),
            "{state}: {}",
            run.said(0)
        );
        assert_eq!(run.step(1).detail, STOPPED);
        assert_eq!(run.outcome.code, 1);
        assert!(rig.puts().is_empty(), "{state}");
        assert!(rig.store.state.pending.is_empty(), "{state}");
    }
}

#[test]
fn unknown_mergeability_is_asked_again_a_few_times_and_then_fails() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().live_state.insert(1, "unknown".to_string());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed]);
    assert!(
        run.step(0).detail.contains("mergeability"),
        "{}",
        run.said(0)
    );
    assert!(run.slept > Duration::ZERO);
    let reads = rig
        .lines()
        .iter()
        .filter(|line| line.as_str() == "GET /repos/acme/api/pulls/1")
        .count();
    assert_eq!(reads, 3);
    assert!(rig.puts().is_empty());
}

#[test]
fn a_head_that_moved_after_the_plan_is_refused() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let first = merge_numbers(&plan)[0];
    rig.script().live_head.insert(first, "f".repeat(40));

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed, Status::Skipped]);
    assert!(run.step(0).detail.contains("moved"), "{}", run.said(0));
    assert_eq!(run.outcome.code, 1);
    assert!(rig.puts().is_empty());
}

#[test]
fn a_failed_deploy_fails_the_step_keeps_the_pending_and_stops_the_queue() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let order = merge_numbers(&plan);
    rig.script().runs = vec![
        run_json("build", "completed", Some("success")),
        run_json("deploy", "completed", Some("failure")),
    ];

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed, Status::Skipped]);
    assert!(
        run.step(0)
            .deploy
            .as_deref()
            .unwrap()
            .contains("deploy ended as failure"),
        "{}",
        run.said(0)
    );
    assert_eq!(run.step(1).detail, STOPPED);
    assert_eq!(run.outcome.code, 1);
    assert_eq!(rig.puts().len(), 1);
    assert_eq!(rig.store.state.pending.len(), 1);
    assert_eq!(
        rig.store.state.pending[0].id,
        format!("acme/api#{}", order[0])
    );
    assert_eq!(
        rig.store.state.pending[0].sha.as_deref(),
        Some(merge_sha(order[0]).as_str())
    );
}

#[test]
fn a_deploy_that_never_ends_stops_and_the_next_run_resumes_without_merging_again() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().runs = vec![run_json("deploy", "in_progress", None)];
    let settings = Settings {
        timeout: Some(Duration::from_secs(5)),
        open_pr: false,
        ..applied()
    };

    let first = rig.run(&plan, settings, true, true).unwrap();

    assert_eq!(first.statuses(), vec![Status::Stopped]);
    assert_eq!(first.outcome.code, EXIT_STOPPED);
    assert!(first.slept >= Duration::from_secs(5));
    assert!(
        first.said(0).contains("still deploying"),
        "{}",
        first.said(0)
    );
    assert_eq!(rig.puts().len(), 1);
    assert_eq!(rig.store.state.pending.len(), 1);
    assert_eq!(
        rig.store.state.pending[0].sha.as_deref(),
        Some(merge_sha(1).as_str())
    );

    rig.script().runs = green();
    let second = rig.go(&plan);

    assert_eq!(second.step(0).kind, Kind::Resume);
    assert_eq!(second.step(0).status, Status::Done);
    assert_eq!(second.step(0).merge_sha, Some(merge_sha(1)));
    assert_eq!(second.step(1).kind, Kind::Merge);
    assert_eq!(second.step(1).status, Status::Skipped);
    assert_eq!(second.outcome.code, 0);
    assert_eq!(rig.puts().len(), 1);
    assert!(rig.store.state.pending.is_empty());
}

#[test]
fn no_run_for_the_push_stops_with_a_pointer_to_no_deploy_repos() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    rig.script().runs = Vec::new();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Stopped, Status::Skipped]);
    assert!(
        run.said(0).contains("merge.no_deploy_repos"),
        "{}",
        run.said(0)
    );
    assert_eq!(run.slept, Duration::from_secs(3));
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.puts().len(), 1);
    assert_eq!(rig.store.state.pending.len(), 1);
}

#[test]
fn a_repo_listed_in_no_deploy_repos_is_not_waited_for() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/api\"]");
    let plan = plan_on(&[1, 2]);
    rig.script().runs = Vec::new();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(run.said(0).contains("not waited"), "{}", run.said(0));
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.polls(), 0);
    assert_eq!(rig.puts().len(), 2);
    assert_eq!(run.slept, Duration::ZERO);
    assert!(rig.store.state.pending.is_empty());
}

const BUNDLE: &str = "deploy/05-11-2026-main";
const PULLS: &str = "POST /repos/acme/api/pulls";

fn bundle_rig(extra: &str) -> (Rig, Plan) {
    let rig = Rig::new(extra);
    rig.script().base = BUNDLE.to_string();
    rig.script().runs = Vec::new();
    (rig, plan_of(prs(&[1, 2], BUNDLE), Vec::new()))
}

fn pushing(branches: &str) -> (String, String) {
    (
        "deploy.yml".to_string(),
        format!("on:\n  push:\n    branches: [{branches}]\n"),
    )
}

fn open_pr_settings() -> Settings {
    Settings {
        open_pr: true,
        ..applied()
    }
}

#[test]
fn a_train_branch_is_not_waited_for_when_no_workflow_runs_on_a_push_to_it() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().workflows = vec![pushing("main")];

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(
        run.said(0)
            .contains("not waited: workflows read, none runs on push to deploy/05-11-2026-main"),
        "{}",
        run.said(0)
    );
    assert_eq!(rig.polls(), 0);
    assert_eq!(rig.puts().len(), 2);
}

#[test]
fn a_repo_without_workflow_files_is_not_waited_for_on_a_train_branch() {
    let (mut rig, plan) = bundle_rig("");

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(run.said(0).contains("none runs on push"), "{}", run.said(0));
    assert_eq!(rig.polls(), 0);
}

#[test]
fn the_workflows_are_read_once_per_repo_and_branch_in_a_run() {
    let (mut rig, plan) = bundle_rig("");

    rig.go(&plan);

    let trees = rig
        .lines()
        .iter()
        .filter(|line| line.starts_with("GET /repos/acme/api/git/trees/"))
        .count();
    assert_eq!(trees, 1);
}

#[test]
fn a_train_branch_is_waited_for_when_a_workflow_runs_on_a_push_to_it() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().workflows = vec![pushing("'deploy/**'")];
    rig.script().runs = green();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(
        run.said(0)
            .contains("workflow deploy.yml runs on push to deploy/05-11-2026-main"),
        "{}",
        run.said(0)
    );
    assert!(run.said(0).contains("deploy passed"), "{}", run.said(0));
    assert!(rig.polls() > 0);
}

#[test]
fn a_train_branch_whose_workflows_cannot_be_read_is_waited_for() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().tree_status = 403;
    rig.script().runs = green();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(
        run.said(0).contains("could not read the workflows"),
        "{}",
        run.said(0)
    );
    assert!(rig.polls() > 0);
}

#[test]
fn a_train_branch_with_a_workflow_that_is_not_valid_yaml_is_waited_for() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().workflows = vec![("ci.yml".to_string(), "on: [".to_string())];
    rig.script().runs = green();

    let run = rig.go(&plan);

    assert!(
        run.said(0).contains("could not read the workflows"),
        "{}",
        run.said(0)
    );
    assert!(rig.polls() > 0);
}

fn run_with(id: u64, workflow: u64, status: &str, conclusion: Option<&str>) -> Value {
    let mut run = run_json("ci", status, conclusion);
    run["id"] = json!(id);
    run["workflow_id"] = json!(workflow);
    run
}

#[test]
fn a_cancelled_run_replaced_by_a_newer_one_does_not_fail_the_deploy() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().runs = vec![
        run_with(10, 7, "completed", Some("cancelled")),
        run_with(11, 7, "completed", Some("success")),
    ];

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done]);
    assert!(
        run.said(0).contains("deploy passed (1 workflow runs)"),
        "{}",
        run.said(0)
    );
}

#[test]
fn a_cancelled_run_with_no_newer_run_of_its_workflow_still_fails_the_deploy() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().runs = vec![
        run_with(10, 7, "completed", Some("cancelled")),
        run_with(11, 8, "completed", Some("success")),
    ];

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed]);
    assert!(
        run.said(0).contains("ended as cancelled"),
        "{}",
        run.said(0)
    );
}

#[test]
fn open_pr_adds_one_step_after_the_last_merge_of_the_bundle() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().branches = vec!["main".to_string(), BUNDLE.to_string()];

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    let kinds: Vec<Kind> = run.outcome.report.steps.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec![Kind::Merge, Kind::Merge, Kind::OpenPr]);
    assert_eq!(run.statuses(), vec![Status::Done; 3]);
    assert_eq!(run.step(2).target, format!("acme/api:{BUNDLE}"));
    assert_eq!(run.step(2).detail, format!("opened {BUNDLE} into main"));
    let lines = rig.lines();
    let last_put = last_position(&lines, "PUT /repos/acme/api/pulls/");
    assert!(last_put < position(&lines, PULLS));
    let body = rig.body(PULLS);
    assert_eq!(body["title"], BUNDLE);
    assert_eq!(body["head"], BUNDLE);
    assert_eq!(body["base"], "main");
    let text = body["body"].as_str().unwrap();
    assert!(
        text.contains("acme/api#1") && text.contains("acme/api#2"),
        "{text}"
    );
    assert_eq!(
        serde_json::to_value(&run.outcome.report.steps[2]).unwrap()["kind"],
        "open-pr"
    );
}

#[test]
fn a_dry_run_with_open_pr_plans_the_step_and_writes_nothing() {
    let (mut rig, plan) = bundle_rig("");
    let settings = Settings {
        open_pr: true,
        ..dry()
    };

    let run = rig.run(&plan, settings, true, true).unwrap();

    let kinds: Vec<Kind> = run.outcome.report.steps.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec![Kind::Merge, Kind::Merge, Kind::OpenPr]);
    assert!(run.statuses().iter().all(|s| *s == Status::Planned));
    assert!(rig.puts().is_empty());
    assert!(!rig.lines().iter().any(|line| line == PULLS));
}

#[test]
fn without_open_pr_no_pull_request_is_ever_opened() {
    let (mut rig, plan) = bundle_rig("");

    let run = rig.go(&plan);

    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .all(|step| step.kind == Kind::Merge)
    );
    assert!(!rig.lines().iter().any(|line| line == PULLS));
}

#[test]
fn open_pr_does_not_open_when_a_merge_before_it_did_not_finish() {
    let (mut rig, plan) = bundle_rig("");
    let first = merge_numbers(&plan)[0];
    rig.script()
        .review
        .insert(first, "CHANGES_REQUESTED".to_string());

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(
        run.statuses(),
        vec![Status::Failed, Status::Skipped, Status::Skipped]
    );
    assert_eq!(run.step(2).detail, STOPPED);
    assert!(!rig.lines().iter().any(|line| line == PULLS));
}

#[test]
fn open_pr_does_not_duplicate_a_deploy_pr_that_is_already_open() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().branches = vec!["main".to_string(), BUNDLE.to_string()];
    rig.script().open_prs = vec![json!({"number": 9})];

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(2).status, Status::Skipped);
    assert_eq!(run.step(2).detail, "deploy PR already open: acme/api#9");
    assert!(!rig.lines().iter().any(|line| line == PULLS));
    assert_eq!(run.outcome.code, 0);
}

#[test]
fn open_pr_treats_a_422_already_exists_as_skipped() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().branches = vec!["main".to_string(), BUNDLE.to_string()];
    rig.script().post_status = 422;
    rig.script().post_message = "A pull request already exists for acme:deploy".to_string();

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(2).status, Status::Skipped);
    assert_eq!(run.outcome.code, 0);
}

#[test]
fn open_pr_fails_loudly_when_the_base_cannot_be_told() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().branches = vec!["develop".to_string()];

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(2).status, Status::Failed);
    assert!(
        run.step(2).detail.contains("open the PR by hand"),
        "{}",
        run.step(2).detail
    );
    assert_eq!(run.outcome.code, 1);
    assert!(!rig.lines().iter().any(|line| line == PULLS));
}

#[test]
fn open_pr_skips_prs_that_do_not_target_a_train_branch() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .all(|s| s.kind == Kind::Merge)
    );
}

fn recorded_rig() -> (Rig, Plan) {
    let mut rig = Rig::new("");
    rig.script().base = BUNDLE.to_string();
    rig.script().runs = Vec::new();
    rig.script().branches = vec!["main".to_string(), BUNDLE.to_string()];
    rig.store
        .add_deploy_pr("acme/api", BUNDLE, "acme/api#1")
        .unwrap();
    rig.store
        .add_deploy_pr("acme/api", BUNDLE, "acme/api#2")
        .unwrap();
    (rig, plan_of(Vec::new(), Vec::new()))
}

fn pulls_posted(rig: &Rig) -> usize {
    rig.lines().iter().filter(|line| *line == PULLS).count()
}

#[test]
fn a_run_cut_off_before_open_pr_opens_it_once_on_the_next_run() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().branches = vec!["main".to_string(), BUNDLE.to_string()];
    rig.script().post_status = 500;

    let first = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(
        first.statuses(),
        vec![Status::Done, Status::Done, Status::Failed]
    );
    assert_eq!(rig.store.state.deploy_prs.len(), 1);

    let empty = plan_of(Vec::new(), Vec::new());
    rig.script().post_status = 201;
    let second = rig.run(&empty, open_pr_settings(), true, true).unwrap();

    let kinds: Vec<Kind> = second.outcome.report.steps.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec![Kind::OpenPr]);
    assert_eq!(second.step(0).status, Status::Done);
    assert_eq!(second.step(0).detail, format!("opened {BUNDLE} into main"));
    let text = rig.body(PULLS)["body"].as_str().unwrap().to_string();
    assert!(
        text.contains("acme/api#1") && text.contains("acme/api#2"),
        "{text}"
    );
    assert!(rig.store.state.deploy_prs.is_empty());

    let third = rig.run(&empty, open_pr_settings(), true, true).unwrap();
    assert!(third.outcome.report.steps.is_empty());
    assert_eq!(pulls_posted(&rig), 2);
}

#[test]
fn a_recorded_bundle_whose_deploy_pr_is_already_open_is_not_duplicated() {
    let (mut rig, plan) = recorded_rig();
    rig.script().open_prs = vec![json!({"number": 9})];

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(0).status, Status::Skipped);
    assert_eq!(run.step(0).detail, "deploy PR already open: acme/api#9");
    assert_eq!(pulls_posted(&rig), 0);
    assert!(rig.store.state.deploy_prs.is_empty());
}

#[test]
fn a_recorded_bundle_with_a_pr_still_on_the_train_branch_does_not_open() {
    let (mut rig, plan) = recorded_rig();
    rig.script().into_train = vec![json!({"number": 5})];

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(0).status, Status::Skipped);
    assert!(
        run.step(0).detail.contains("acme/api#5 still targets"),
        "{}",
        run.step(0).detail
    );
    assert_eq!(pulls_posted(&rig), 0);
    assert_eq!(rig.store.state.deploy_prs.len(), 1);
    assert_eq!(run.outcome.code, 0);
}

#[test]
fn a_recorded_bundle_whose_branch_is_gone_is_dropped_without_opening() {
    let (mut rig, plan) = recorded_rig();
    rig.script().branch_missing = true;

    let run = rig.run(&plan, open_pr_settings(), true, true).unwrap();

    assert_eq!(run.step(0).status, Status::Skipped);
    assert!(
        run.step(0).detail.contains("no longer exists"),
        "{}",
        run.step(0).detail
    );
    assert_eq!(pulls_posted(&rig), 0);
    assert!(rig.store.state.deploy_prs.is_empty());
}

#[test]
fn a_dry_run_plans_the_recorded_bundle_and_writes_nothing() {
    let (mut rig, plan) = recorded_rig();
    let settings = Settings {
        open_pr: true,
        ..dry()
    };

    let run = rig.run(&plan, settings, true, true).unwrap();

    assert_eq!(run.outcome.report.steps.len(), 1);
    assert_eq!(run.step(0).kind, Kind::OpenPr);
    assert_eq!(run.step(0).status, Status::Planned);
    assert_eq!(pulls_posted(&rig), 0);
    assert_eq!(rig.store.state.deploy_prs.len(), 1);
}

#[test]
fn without_open_pr_a_recorded_bundle_is_left_alone() {
    let (mut rig, plan) = recorded_rig();

    let run = rig.go(&plan);

    assert!(run.outcome.report.steps.is_empty());
    assert_eq!(rig.store.state.deploy_prs.len(), 1);
}

#[test]
fn a_ddl_pr_adds_a_gate_that_stops_the_queue_until_it_is_confirmed() {
    let mut rig = Rig::new("");
    let plan = gated_plan();

    let run = rig.run(&plan, applied(), true, false).unwrap();

    let kinds: Vec<Kind> = run.outcome.report.steps.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec![Kind::Merge, Kind::Gate, Kind::Merge]);
    assert_eq!(
        run.statuses(),
        vec![Status::Done, Status::Stopped, Status::Skipped]
    );
    assert_eq!(run.step(2).detail, STOPPED);
    assert_eq!(run.gates, 1);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.puts().len(), 1);
}

#[test]
fn a_gate_the_hook_confirms_lets_the_queue_continue() {
    let mut rig = Rig::new("");
    let plan = gated_plan();

    let run = rig.run(&plan, applied(), true, true).unwrap();

    assert_eq!(
        run.statuses(),
        vec![Status::Done, Status::Done, Status::Done]
    );
    assert_eq!(run.step(1).kind, Kind::Gate);
    assert_eq!(run.gates, 1);
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.puts().len(), 2);
}

#[test]
fn gates_confirmed_passes_every_gate_without_asking() {
    let mut rig = Rig::new("");
    let plan = gated_plan();
    let settings = Settings {
        gates_confirmed: true,
        ..applied()
    };

    let run = rig.run(&plan, settings, true, false).unwrap();

    assert_eq!(
        run.statuses(),
        vec![Status::Done, Status::Done, Status::Done]
    );
    assert_eq!(run.gates, 0);
    assert!(run.step(1).detail.contains("--gates-confirmed"));
    assert_eq!(rig.puts().len(), 2);
}

#[test]
fn open_questions_stop_an_apply_before_any_merge() {
    let mut rig = Rig::new("");
    let question = Question::new(
        None,
        "unclear-order",
        "two PRs could go either way".to_string(),
        "say which goes first",
    );
    let plan = plan_of(prs(&[1], "main"), vec![question]);

    let run = rig.go(&plan);

    assert!(run.outcome.report.stopped.is_some());
    assert!(!run.outcome.report.questions.is_empty());
    assert_eq!(run.outcome.code, emit::EXIT_QUESTIONS);
    assert_eq!(run.confirms, 0);
    assert!(rig.puts().is_empty());
}

#[test]
fn allow_questions_lets_the_apply_go_on() {
    let mut rig = Rig::new("");
    let question = Question::new(
        None,
        "unclear-order",
        "two PRs could go either way".to_string(),
        "say which goes first",
    );
    let plan = plan_of(prs(&[1], "main"), vec![question]);
    let settings = Settings {
        allow_questions: true,
        ..applied()
    };

    let run = rig.run(&plan, settings, true, true).unwrap();

    assert_eq!(run.statuses(), vec![Status::Done]);
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.puts().len(), 1);
}

#[test]
fn declining_the_confirmation_merges_nothing() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);

    let run = rig.run(&plan, applied(), false, true).unwrap();

    assert!(run.outcome.aborted);
    assert_eq!(run.outcome.code, 1);
    assert_eq!(run.confirms, 1);
    assert!(rig.puts().is_empty());
}

#[test]
fn the_method_from_the_settings_goes_in_the_merge_request() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    let settings = Settings {
        method: Some("Squash".to_string()),
        ..applied()
    };

    let run = rig.run(&plan, settings, true, true).unwrap();

    assert_eq!(run.statuses(), vec![Status::Done]);
    let body = rig.body("PUT /repos/acme/api/pulls/1/merge");
    assert_eq!(body["merge_method"], "squash");
    assert_eq!(body["sha"], format!("{:040x}", 1));
}

#[test]
fn the_method_defaults_to_the_config_and_falls_back_to_merge() {
    let mut rig = Rig::new("method = \"rebase\"");
    rig.go(&plan_on(&[1]));
    assert_eq!(
        rig.body("PUT /repos/acme/api/pulls/1/merge")["merge_method"],
        "rebase"
    );

    let mut plain = Rig::new("");
    plain.go(&plan_on(&[1]));
    assert_eq!(
        plain.body("PUT /repos/acme/api/pulls/1/merge")["merge_method"],
        "merge"
    );
}

#[test]
fn an_unknown_method_is_an_error_before_anything_is_asked_or_merged() {
    let mut rig = Rig::new("");
    let settings = Settings {
        method: Some("fast-forward".to_string()),
        ..applied()
    };

    let error = rig
        .run(&plan_on(&[1]), settings, true, true)
        .err()
        .expect("an error");

    assert!(format!("{error:#}").contains("fast-forward"));
    assert!(rig.lines().is_empty());
}

#[test]
fn a_rejected_merge_fails_the_step_and_leaves_nothing_pending() {
    let mut rig = Rig::new("");
    {
        let mut script = rig.script();
        script.put_status = 405;
        script.put_body = Some(json!({"message": "Pull Request is not mergeable"}));
        script.put_merges = false;
    }

    let run = rig.go(&plan_on(&[1, 2]));

    assert_eq!(run.statuses(), vec![Status::Failed, Status::Skipped]);
    assert!(
        run.step(0).detail.contains("not mergeable"),
        "{}",
        run.said(0)
    );
    assert_eq!(run.outcome.code, 1);
    assert_eq!(rig.puts().len(), 1);
    assert!(rig.store.state.pending.is_empty());
}

#[test]
fn a_gateway_error_on_a_merge_that_actually_happened_goes_on_as_merged() {
    let mut rig = Rig::new("");
    {
        let mut script = rig.script();
        script.put_status = 502;
        script.put_body = Some(json!({"message": "Bad Gateway"}));
        script.put_merges = true;
    }

    let run = rig.go(&plan_on(&[1]));

    assert_eq!(run.statuses(), vec![Status::Done]);
    assert_eq!(run.step(0).merge_sha, Some(merge_sha(1)));
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.puts().len(), 1);
    assert!(rig.store.state.pending.is_empty());
}
