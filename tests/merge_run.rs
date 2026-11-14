use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

mod common;
use common::{Server, pr, serve_requests};
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan, render, render_final};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::emit;
use pipetrain::github::Client;
use pipetrain::merge::{
    DependencyFailure, EXIT_STOPPED, Kind, STOPPED, Status, Step, StepReport, Store,
};
use pipetrain::model::{ChangedFile, Pr, Question};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

const TRAIN: &str = "deploy/05-11-2026";

struct Script {
    http_elapsed: BTreeMap<String, Duration>,
    run_errors: std::collections::VecDeque<u16>,
    run_delays: std::collections::VecDeque<Duration>,
    base: String,
    review: BTreeMap<u64, String>,
    unknown_reads: BTreeMap<u64, usize>,
    graphql_states: BTreeMap<u64, String>,
    live_state: BTreeMap<u64, String>,
    live_head: BTreeMap<u64, String>,
    put_status: u16,
    put_body: Option<Value>,
    put_merges: bool,
    merged: BTreeSet<u64>,
    runs: Vec<Value>,
    run_readings: std::collections::VecDeque<Vec<Value>>,
    run_total: Option<u64>,
    runs_by_sha: BTreeMap<String, Vec<Value>>,
    bases: BTreeMap<u64, String>,
    workflows: Vec<(String, String)>,
    files: Vec<Value>,
    file_total: Option<u64>,
    file_pages: BTreeMap<u64, Vec<Value>>,
    files_by_pr: BTreeMap<u64, Vec<Value>>,
    metadata_missing_total: bool,
    tree_truncated: bool,
    tree_body: Option<Value>,
    tree_status: u16,
    branches: Vec<String>,
    open_prs: Vec<Value>,
    into_train: Vec<Value>,
    branch_missing: bool,
    later_tip: Option<String>,
    compare_status: String,
    post_status: u16,
    post_message: String,
}

fn merge_sha(number: u64) -> String {
    format!("{:040x}", 0x1000 + number)
}

fn stacked_plan(depth: u64) -> Plan {
    plan_of(
        (1..=depth)
            .map(|number| {
                let base = if number == 1 {
                    "main".to_string()
                } else {
                    format!("feat/{}", number - 1)
                };
                let mut item = pr("acme/api", number, &format!("feat/{number}"), &base);
                item.default_branch = "main".into();
                item
            })
            .collect(),
        Vec::new(),
    )
}

fn stacked_rig(mode: &str, depth: u64, parallel: usize) -> Rig {
    let mut rig = Rig::new("no_deploy_repos = ['acme/api']");
    rig.cfg.max_parallel = parallel;
    for number in 2..=depth {
        rig.script()
            .bases
            .insert(number, format!("feat/{}", number - 1));
    }
    let shared = rig.script.clone();
    let mode = mode.to_string();
    rig.server = serve_requests(move |line, payload| {
        if line.starts_with("GET /repos/acme/api/compare/") {
            return (
                200,
                json!({
                    "status": if mode == "missing-ancestor" { "diverged" } else { "ahead" },
                    "ahead_by": if mode == "distant-descendant" { 2 } else { 1 },
                }),
            );
        }
        if line.starts_with("GET /repos/acme/api/git/commits/") {
            return (
                200,
                json!({"parents": [
                    {"sha": format!("{:040x}", 1)},
                    {"sha": if mode == "unrelated" || mode == "wrong-parents" { "e".repeat(40) } else { format!("{:040x}", 2) }},
                ]}),
            );
        }
        let (status, mut body) = respond(&shared, line, payload);
        if line == "PUT /repos/acme/api/pulls/2/merge"
            && matches!(
                mode.as_str(),
                "descendant" | "wrong-parents" | "distant-descendant"
            )
        {
            body["sha"] = json!(format!("{:040x}", 2));
        }
        let number = if line == "POST /graphql" {
            serde_json::from_str::<Value>(payload).unwrap()["variables"]["n"].as_u64()
        } else {
            line.strip_prefix("GET /repos/acme/api/pulls/")
                .and_then(|number| number.parse::<u64>().ok())
        };
        if let Some(number) = number {
            let script = shared.lock().unwrap();
            let child = script
                .merged
                .iter()
                .rev()
                .find(|child| script.bases.get(child) == Some(&format!("feat/{number}")));
            if let Some(child) = child {
                let head = if matches!(
                    mode.as_str(),
                    "unrelated" | "descendant" | "wrong-parents" | "distant-descendant"
                ) {
                    "d".repeat(40)
                } else {
                    merge_sha(if mode == "rollback" { 2 } else { *child })
                };
                if line == "POST /graphql" {
                    let node = &mut body["data"]["repository"]["pullRequest"];
                    if mode != "stale-review-snapshot" {
                        node["headRefOid"] = json!(head);
                    }
                    if mode == "lost-approval" {
                        node["reviewDecision"] = json!("REVIEW_REQUIRED");
                        node["latestOpinionatedReviews"]["nodes"] = json!([]);
                    }
                    if mode == "checks-pending" {
                        node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] =
                            json!("PENDING");
                    }
                } else {
                    body["head"]["sha"] = json!(head);
                    if mode == "base-changed" {
                        body["base"]["ref"] = json!("other");
                    }
                }
            }
        }
        (status, body)
    });
    rig
}

#[test]
fn stacked_child_and_parent_merge_with_revalidation_and_verified_sha() {
    for parallel in [1, 2] {
        let mut rig = stacked_rig("landed", 2, parallel);
        let state = FilterState::attach(&mut rig);
        let run = rig.go(&stacked_plan(2));
        assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
        assert_eq!(
            rig.puts(),
            vec![
                "PUT /repos/acme/api/pulls/2/merge",
                "PUT /repos/acme/api/pulls/1/merge"
            ]
        );
        assert_eq!(
            rig.body("PUT /repos/acme/api/pulls/1/merge")["sha"],
            merge_sha(2)
        );
        let lines = rig.lines();
        let child = position(&lines, "PUT /repos/acme/api/pulls/2/merge");
        let parent = position(&lines, "PUT /repos/acme/api/pulls/1/merge");
        assert!(
            lines[child + 1..parent]
                .iter()
                .any(|line| line == "POST /graphql")
        );
        assert!(
            lines[child + 1..parent]
                .iter()
                .any(|line| line.contains("/compare/"))
        );
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn stacked_parent_lost_approval_stops_with_reapprove_next_step() {
    let mut rig = stacked_rig("lost-approval", 2, 1);
    let state = FilterState::attach(&mut rig);
    let run = rig.go(&stacked_plan(2));
    assert_eq!(run.statuses(), vec![Status::Done, Status::Stopped]);
    assert!(run.said(1).contains("review required"));
    assert!(
        run.said(1)
            .contains("re-approve acme/api#1 after its stacked child landed")
    );
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn stacked_parent_pending_checks_stop_with_cause_and_next_step() {
    let mut rig = stacked_rig("checks-pending", 2, 1);
    let state = FilterState::attach(&mut rig);
    let run = rig.go(&stacked_plan(2));
    assert_eq!(run.step(1).status, Status::Stopped);
    assert!(run.said(1).contains("checks pending"));
    assert!(run.said(1).contains("wait for checks to complete"));
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn stacked_parent_unrelated_push_is_refused() {
    let mut rig = stacked_rig("unrelated", 2, 1);
    let state = FilterState::attach(&mut rig);
    let run = rig.go(&stacked_plan(2));
    assert_eq!(run.step(1).status, Status::Failed);
    assert!(run.said(1).contains("branch moved after the plan"));
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn stacked_parent_requires_planned_ancestor_matching_snapshot_and_unchanged_base() {
    for mode in [
        "missing-ancestor",
        "stale-review-snapshot",
        "base-changed",
        "wrong-parents",
        "distant-descendant",
    ] {
        let mut rig = stacked_rig(mode, 2, 1);
        let state = FilterState::attach(&mut rig);
        let run = rig.go(&stacked_plan(2));
        assert_eq!(run.step(1).status, Status::Failed, "{mode}");
        assert_eq!(rig.puts().len(), 1, "{mode}");
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn stacked_parent_direct_descendant_requires_exact_merge_parents() {
    let mut rig = stacked_rig("descendant", 2, 1);
    let state = FilterState::attach(&mut rig);
    let run = rig.go(&stacked_plan(2));
    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert_eq!(
        rig.body("PUT /repos/acme/api/pulls/1/merge")["sha"],
        "d".repeat(40)
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn three_pr_stack_shows_full_chain_in_plan_and_preview_and_merges_in_order() {
    let plan = stacked_plan(3);
    let chain = "acme/api#3 → feat/2 (#2) → feat/1 (#1) → main";
    let strategy = "children land into the parent branch first; the parent is revalidated after";
    assert!(pipetrain::render::text(&plan).contains(chain));
    assert!(pipetrain::render::text(&plan).contains(strategy));
    let json = serde_json::to_value(&plan).unwrap();
    assert!(
        json["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains(chain))
    );
    let mut rig = stacked_rig("landed", 3, 2);
    let state = FilterState::attach(&mut rig);
    let preview = rig.run(&plan, dry(), true, true).unwrap();
    let text = render(&preview.outcome.report, false);
    assert!(text.contains(chain));
    assert!(text.contains(strategy));
    assert!(rig.puts().is_empty());
    let run = rig.go(&plan);
    assert_eq!(run.statuses(), vec![Status::Done; 3]);
    assert!(run.confirmation_text[0].contains(chain));
    assert_eq!(
        rig.puts(),
        vec![
            "PUT /repos/acme/api/pulls/3/merge",
            "PUT /repos/acme/api/pulls/2/merge",
            "PUT /repos/acme/api/pulls/1/merge"
        ]
    );
    assert_eq!(
        rig.body("PUT /repos/acme/api/pulls/2/merge")["sha"],
        merge_sha(3)
    );
    assert_eq!(
        rig.body("PUT /repos/acme/api/pulls/1/merge")["sha"],
        merge_sha(2)
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn stacked_orphan_feature_base_keeps_preflight_warning_in_plan_and_preview() {
    let mut orphan = pr("acme/api", 2, "feat/2", "feat/1");
    orphan.default_branch = "main".into();
    let plan = plan_of(vec![orphan], Vec::new());
    assert!(pipetrain::render::text(&plan).contains("base not landed"));
    let mut rig = stacked_rig("landed", 2, 1);
    let state = FilterState::attach(&mut rig);
    let preview = rig.run(&plan, dry(), true, true).unwrap();
    assert!(render(&preview.outcome.report, false).contains("base not landed"));
    assert!(rig.puts().is_empty());
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn stacked_siblings_require_the_latest_recorded_child_result() {
    for mode in ["landed", "rollback"] {
        let mut ready = prs(&[1], "main");
        ready.extend(prs(&[2, 3], "feat/1"));
        let plan = plan_of(ready, Vec::new());
        let mut rig = stacked_rig(mode, 3, 2);
        rig.script().bases.insert(3, "feat/1".into());
        let state = FilterState::attach(&mut rig);
        let run = rig.go(&plan);
        assert_eq!(run.step(0).status, Status::Done);
        assert_eq!(run.step(1).status, Status::Done);
        if mode == "landed" {
            assert_eq!(run.step(2).status, Status::Done);
            assert_eq!(
                rig.body("PUT /repos/acme/api/pulls/1/merge")["sha"],
                merge_sha(3)
            );
        } else {
            assert_eq!(run.step(2).status, Status::Failed);
            assert_eq!(rig.puts().len(), 2);
        }
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn stacked_parent_workflow_prediction_uses_verified_head() {
    let mut rig = stacked_rig("landed", 2, 1);
    rig.cfg.merge.no_deploy_repos.clear();
    let state = FilterState::attach(&mut rig);
    let run = rig.go(&stacked_plan(2));
    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(matches!(
        run.step(1).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Expected { .. })
    ));
    assert!(state.reopen().state.pending.is_empty());
}

fn run_json(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({
        "name": name,
        "path": ".github/workflows/deploy.yml",
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
            http_elapsed: BTreeMap::new(),
            run_errors: Default::default(),
            run_delays: Default::default(),
            base: "main".to_string(),
            review: BTreeMap::new(),
            unknown_reads: BTreeMap::new(),
            graphql_states: BTreeMap::new(),
            live_state: BTreeMap::new(),
            live_head: BTreeMap::new(),
            put_status: 200,
            put_body: None,
            put_merges: true,
            merged: BTreeSet::new(),
            runs: green(),
            run_readings: Default::default(),
            run_total: None,
            runs_by_sha: BTreeMap::new(),
            bases: BTreeMap::new(),
            workflows: vec![("deploy.yml".into(), "on: push".into())],
            files: vec![json!({"filename": "src/app.rs", "status": "modified"})],
            file_total: None,
            file_pages: BTreeMap::new(),
            files_by_pr: BTreeMap::new(),
            metadata_missing_total: false,
            tree_truncated: false,
            tree_body: None,
            tree_status: 200,
            branches: vec!["main".to_string(), "feat/x".to_string()],
            open_prs: Vec::new(),
            into_train: Vec::new(),
            branch_missing: false,
            later_tip: None,
            compare_status: "diverged".into(),
            post_status: 201,
            post_message: String::new(),
        }
    }

    fn tree(&self) -> Value {
        if let Some(body) = &self.tree_body {
            return body.clone();
        }
        let mut entries: Vec<Value> = self
            .workflows
            .iter()
            .map(|(name, _)| json!({"type": "blob", "path": format!(".github/workflows/{name}")}))
            .collect();
        entries.push(json!({"type": "blob", "path": "README.md"}));
        json!({"tree": entries, "truncated": self.tree_truncated})
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
            "baseRefName": self.bases.get(&number).unwrap_or(&self.base),
            "baseRef": {"target": {"oid": "b".repeat(40)}},
            "headRefOid": format!("{number:040x}"),
            "isDraft": false,
            "state": if merged { "MERGED" } else { "OPEN" },
            "reviewDecision": decision,
            "mergeable": "MERGEABLE",
            "mergeStateStatus": self.graphql_states.get(&number).map_or("CLEAN", String::as_str),
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
            "base": {"ref": self.bases.get(&number).unwrap_or(&self.base)},
            "mergeable_state": state,
            "changed_files": self.file_total.unwrap_or(self.files_by_pr.get(&number).unwrap_or(&self.files).len() as u64),
        });
        if self.metadata_missing_total {
            body.as_object_mut().unwrap().remove("changed_files");
        }
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
            return (200, script.blobs());
        }
        let number = body
            .pointer("/variables/n")
            .and_then(Value::as_u64)
            .unwrap();
        let mut node = script.node(number);
        if let Some(remaining) = script.unknown_reads.get_mut(&number)
            && *remaining > 0
        {
            *remaining -= 1;
            node["mergeable"] = json!("UNKNOWN");
        }
        node["repository"]["name"] = body["variables"]["r"].clone();
        node["repository"]["owner"]["login"] = body["variables"]["o"].clone();
        return (200, json!({"data": {"repository": {"pullRequest": node}}}));
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
        return (
            200,
            json!({"object": {"sha": script.later_tip.clone().unwrap_or_else(|| "c".repeat(40))}}),
        );
    }
    if line.starts_with("GET /repos/acme/api/compare/") {
        return (200, json!({"status": script.compare_status}));
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
    if let Some((prefix, page)) = line.split_once("/files?per_page=100&page=") {
        let number = prefix.rsplit('/').next().unwrap().parse::<u64>().unwrap();
        let page = page.parse::<u64>().unwrap();
        let files = script.file_pages.get(&page).cloned().unwrap_or_else(|| {
            script
                .files_by_pr
                .get(&number)
                .unwrap_or(&script.files)
                .iter()
                .skip((page as usize - 1) * 100)
                .take(100)
                .cloned()
                .collect()
        });
        return (200, json!(files));
    }
    if let Some(rest) = line.strip_prefix("GET /repos/acme/api/pulls/") {
        return (200, script.live(rest.parse().unwrap()));
    }
    if line.starts_with("GET /repos/acme/api/actions/runs") {
        if let Some(status) = script.run_errors.pop_front()
            && status != 200
        {
            return (status, json!({"message": "temporarily unavailable"}));
        }
        let sha = line
            .split("head_sha=")
            .nth(1)
            .and_then(|query| query.split('&').next())
            .unwrap_or("");
        let runs = script
            .run_readings
            .pop_front()
            .unwrap_or_else(|| script.runs_by_sha.get(sha).unwrap_or(&script.runs).clone());
        return (
            200,
            json!({"total_count": script.run_total.unwrap_or(runs.len() as u64), "workflow_runs": runs}),
        );
    }
    (404, json!({"message": "not found"}))
}

struct Run {
    phases: Vec<String>,
    outcome: Outcome,
    confirms: usize,
    confirmation_text: Vec<String>,
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
    request_timeout: Duration,
    clock: Arc<Mutex<Instant>>,
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
        let clock = Arc::new(Mutex::new(Instant::now()));
        let server_clock = Arc::clone(&clock);
        let server = serve_requests(move |line, payload| {
            let elapsed = shared
                .lock()
                .unwrap()
                .http_elapsed
                .iter()
                .filter(|(route, _)| line.starts_with(route.as_str()))
                .map(|(_, duration)| *duration)
                .sum::<Duration>();
            *server_clock.lock().unwrap() += elapsed;
            let delay = if line.contains("/actions/runs?") {
                shared
                    .lock()
                    .unwrap()
                    .run_delays
                    .pop_front()
                    .unwrap_or_default()
            } else {
                Duration::ZERO
            };
            std::thread::sleep(delay);
            respond(&shared, line, payload)
        });
        let mut cfg = Config::parse(&format!(
            "orgs = [\"acme\"]\n{top}[merge]\npoll_seconds = 1\ntimeout_minutes = 1\nno_run_grace_seconds = 3\n{extra}"
        ))
        .unwrap();
        cfg.max_parallel = 1;
        Self {
            request_timeout: Duration::from_secs(40),
            clock,
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
        let mut client = Client::with_timeout(
            "ghp_FAKE0123456789".to_string(),
            &api,
            false,
            &self.server.base,
            self.request_timeout,
        );
        let mut confirms = 0;
        let mut confirmation_text = Vec::new();
        let mut gates = 0;
        let mut slept = Duration::ZERO;
        let mut observed = 0;
        let mut phases = Vec::new();
        let outcome = {
            let mut confirm_hook = |text: &str| {
                confirms += 1;
                confirmation_text.push(text.to_string());
                confirm
            };
            let mut gate_hook = |_: &str| {
                gates += 1;
                gate
            };
            let mut sleep_hook = |pause: Duration| {
                slept += pause;
                *self.clock.lock().unwrap() += pause;
                if settings.feed_registry.is_some() {
                    std::thread::sleep(pause);
                }
            };
            let mut observe_hook = |_: &StepReport| observed += 1;
            let mut progress_hook = |text: &str| phases.push(text.to_string());
            let run_started = Instant::now();
            let now = || {
                *self.clock.lock().unwrap()
                    + if self.cfg.max_parallel > 1 {
                        run_started.elapsed()
                    } else {
                        Duration::ZERO
                    }
            };
            let mut hooks = Hooks {
                confirm: &mut confirm_hook,
                gate: &mut gate_hook,
                sleep: &mut sleep_hook,
                now: &now,
                observe: &mut observe_hook,
                progress: &mut progress_hook,
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
            phases,
            outcome,
            confirms,
            confirmation_text,
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
        feed_registry: None,
        open_pr: false,
        dependency_failure: DependencyFailure::Stop,
        bump: false,
        bump_options: Default::default(),
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
fn progress_identifies_the_current_pr_and_deploy_confirmation_in_queue_order() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let order = merge_numbers(&plan);
    let run = rig.go(&plan);
    let first = format!("merging acme/api#{} into main", order[0]);
    let second = format!("merging acme/api#{} into main", order[1]);
    let first_at = run
        .phases
        .iter()
        .position(|text| text.ends_with(&first))
        .unwrap();
    let second_at = run
        .phases
        .iter()
        .position(|text| text.ends_with(&second))
        .unwrap();
    let confirmed_at = run
        .phases
        .iter()
        .position(|text| text.contains("confirming completion"))
        .unwrap();
    assert!(first_at < confirmed_at && confirmed_at < second_at);
    assert!(run.phases.iter().all(|text| text.starts_with("waiting: ")));
    assert!(run.phases.iter().any(|text| {
        text.starts_with("waiting: 1, running: 1, merged: 0, deploy confirmed: 0")
            && text.contains("acme/api#1 → main | readiness: starting")
    }));
    assert!(run.phases.iter().any(|text| {
        text.starts_with("waiting: 0, running: 1, merged: 1, deploy confirmed: 1")
            && text.contains("acme/api#2 → main | readiness: starting")
    }));
    assert!(
        run.phases
            .iter()
            .any(|text| text.contains("checking merge readiness"))
    );
    assert!(
        run.phases
            .iter()
            .any(|text| text.ends_with("checking deploy workflows for acme/api@main"))
    );
    assert!(
        run.phases.last().unwrap().starts_with(
            "waiting: 0, running: 0, merged: 2, deploy confirmed: 2, failed/stopped: 0"
        )
    );
}

#[test]
fn progress_shows_running_workflows_and_waiting_for_the_first_run() {
    let plan = plan_on(&[1]);
    let mut running = Rig::new("");
    running.script().runs = vec![run_json("deploy", "in_progress", None)];
    let run = running
        .run(
            &plan,
            Settings {
                timeout: Some(Duration::from_secs(1)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert!(
        run.phases.iter().any(
            |text| text.ends_with("deploy acme/api: 1 of 1 expected workflow runs still going")
        )
    );

    let mut absent = Rig::new("");
    absent.script().runs = Vec::new();
    let run = absent.go(&plan);
    assert!(
        run.phases
            .iter()
            .any(|text| text.ends_with("deploy acme/api: waiting for a workflow to start"))
    );
    assert_eq!(run.step(0).status, Status::Stopped);
}

#[test]
fn declined_confirmation_emits_no_execution_progress() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    let run = rig.run(&plan, applied(), false, true).unwrap();
    assert!(run.phases.is_empty());
    assert!(rig.puts().is_empty());
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
    assert!(run.phases.is_empty());
    let text = render(&run.outcome.report, false);
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("pass --apply"), "{text}");
}

#[test]
fn apply_with_nothing_to_merge_does_not_print_the_dry_run_banner() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[]);

    let run = rig.go(&plan);

    assert!(run.outcome.report.steps.is_empty());
    assert_eq!(run.outcome.code, 0);
    let text = render(&run.outcome.report, false);
    assert!(text.contains("nothing to merge"), "{text}");
    assert!(!text.contains("dry run"), "{text}");
    assert!(!text.contains("pass --apply"), "{text}");
}

#[test]
fn dry_run_with_nothing_to_merge_still_prints_the_banner() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[]);

    let run = rig.run(&plan, dry(), true, true).unwrap();

    let text = render(&run.outcome.report, false);
    assert!(text.contains("dry run"), "{text}");
    assert!(text.contains("nothing to merge"), "{text}");
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
fn unknown_mergeability_is_retried_for_a_minute_and_then_stops_without_merging() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().live_state.insert(1, "unknown".to_string());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Stopped]);
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
    assert_eq!(reads, 12);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert!(rig.puts().is_empty());
}

#[test]
fn graphql_mergeability_recalculation_after_an_earlier_merge_is_retried() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    let second = merge_numbers(&plan)[1];
    rig.script().unknown_reads.insert(second, 3);

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert_eq!(rig.puts().len(), 2);
    assert!(run.slept >= Duration::from_secs(15));
    assert!(
        run.phases
            .iter()
            .any(|phase| phase.contains("recalculating"))
    );
}

#[test]
fn graphql_mergeability_timeout_is_resumable_without_a_merge_request() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().unknown_reads.insert(1, usize::MAX);

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Stopped]);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(run.slept, Duration::from_secs(60));
    assert!(rig.puts().is_empty());
    assert!(rig.store.state.pending.is_empty());
}

#[test]
fn an_old_train_target_is_explicit_in_confirmation_and_final_summary() {
    let mut rig = Rig::new("");
    let mut plan = plan_on(&[1]);
    plan.items[0].base = TRAIN.into();
    rig.script().base = TRAIN.into();
    rig.script().workflows.push((
        "deploy.yml".into(),
        "on:\n  push:\n    branches: [main]\njobs: {}\n".into(),
    ));

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done]);
    let confirmation = &run.confirmation_text[0];
    assert!(confirmation.contains(TRAIN));
    assert!(confirmation.contains("intermediate train branch"));
    assert!(
        run.phases
            .iter()
            .flat_map(|phase| phase.lines().skip(1))
            .all(|row| row.starts_with(&format!("acme/api#1 → {TRAIN} | ")))
    );
    let summary = render_final(&run.outcome.report, true);
    assert!(summary.contains("Promotion requires a separate reviewed PR"));
    assert!(summary.contains("\u{1b}[33m"));
    assert!(run.step(0).deploy.as_ref().unwrap().contains("not waited"));
    assert!(
        !rig.lines()
            .iter()
            .any(|line| line == "POST /repos/acme/api/pulls")
    );
}

#[test]
fn unknown_mergeability_does_not_delay_or_bypass_a_lost_approval() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().unknown_reads.insert(1, usize::MAX);
    rig.script().review.insert(1, "CHANGES_REQUESTED".into());

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Failed]);
    assert_eq!(run.slept, Duration::ZERO);
    assert!(run.step(0).detail.contains("changes requested"));
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
fn a_failed_deploy_keeps_the_pending_and_blocks_the_same_target() {
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
    assert!(run.step(1).detail.contains("blocked by failed deploy"));
    assert!(run.step(1).blocked_by.is_some());
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
    assert!(run.step(0).detail.contains("the deploy failed"));
    let final_text = render_final(&run.outcome.report, false);
    assert!(!final_text.contains("failed: merge"), "{final_text}");
    assert!(!final_text.contains("skipped: merge"), "{final_text}");
    assert!(final_text.contains("1 PRs merged, 0 earlier merges checked"));
    assert!(final_text.contains("0 done, 1 skipped, 1 failed, 0 stopped"));
    assert!(final_text.contains("same --state to resume"));
    assert!(final_text.contains("The merge will not be repeated"));
    let full_text = render(&run.outcome.report, false);
    assert_eq!(full_text.matches("failed: merge").count(), 1);
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(json["steps"][0]["status"], "failed");
    assert_eq!(json["steps"][0]["merge_sha"], merge_sha(order[0]));
}

#[test]
fn a_failed_deploy_can_resume_after_a_successful_rerun_without_another_merge() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    rig.script().runs = vec![run_json("deploy", "completed", Some("failure"))];
    let first = rig.go(&plan);
    assert_eq!(first.statuses(), vec![Status::Failed]);
    assert_eq!(rig.puts().len(), 1);

    rig.script().runs = green();
    let second = rig.go(&plan);
    assert_eq!(second.step(0).kind, Kind::Resume);
    assert_eq!(second.step(0).status, Status::Done);
    assert_eq!(second.outcome.code, 0);
    assert_eq!(rig.puts().len(), 1);
    assert!(rig.store.state.pending.is_empty());
    let text = render_final(&second.outcome.report, false);
    assert!(text.contains("0 PRs merged, 1 earlier merges checked"));
    assert!(!text.contains("same --state to resume"));
}

#[test]
fn final_dry_run_keeps_the_plan_and_final_success_does_not_repeat_steps() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1]);
    let dry_run = rig.run(&plan, dry(), true, true).unwrap();
    assert_eq!(
        render_final(&dry_run.outcome.report, false),
        render(&dry_run.outcome.report, false)
    );

    let applied = rig.go(&plan);
    let text = render_final(&applied.outcome.report, false);
    assert!(!text.contains("done: merge"));
    assert!(text.contains("1 PRs merged, 0 earlier merges checked"));
    assert!(text.contains("1 done, 0 skipped, 0 failed, 0 stopped"));
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
fn no_run_for_a_push_workflow_stops_without_suggesting_no_deploy_repos() {
    let mut rig = Rig::new("");
    let plan = plan_on(&[1, 2]);
    rig.script().runs = Vec::new();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Stopped, Status::Skipped]);
    assert!(
        !run.said(0).contains("merge.no_deploy_repos"),
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
    rig.script().workflows.clear();
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
            .contains("none runs on push to deploy/05-11-2026-main"),
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
    assert!(
        run.said(0).contains("no workflow in the repo runs on push"),
        "{}",
        run.said(0)
    );
    assert_eq!(rig.polls(), 0);
}

#[test]
fn workflow_readings_are_keyed_by_the_merge_sha() {
    let (mut rig, plan) = bundle_rig("");

    rig.go(&plan);

    let trees = rig
        .lines()
        .iter()
        .filter(|line| line.starts_with("GET /repos/acme/api/git/trees/"))
        .count();
    assert_eq!(trees, 2);
}

#[test]
fn a_train_branch_is_waited_for_when_a_workflow_runs_on_a_push_to_it() {
    let (mut rig, plan) = bundle_rig("");
    rig.script().workflows = vec![pushing("'deploy/**'")];
    rig.script().runs = green();

    let run = rig.go(&plan);

    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert!(
        run.said(0).contains(
            "workflow .github/workflows/deploy.yml runs on push to deploy/05-11-2026-main"
        ),
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

fn deployment_plan() -> Plan {
    let mut portal = pr("acme/portal", 2, "feat/2", "main");
    portal.body = "depends-on: acme/sdk#1".to_string();
    let mut auth = pr("acme/auth", 3, "feat/3", "main");
    auth.body = "depends-on: acme/sdk#1".to_string();
    let mut later = pr("acme/auth", 5, "feat/5", "main");
    later.body = "depends-on: acme/auth#3".to_string();
    let mut plan = plan_of(
        vec![
            pr("acme/sdk", 1, "feat/1", "main"),
            portal,
            auth,
            pr("acme/sync", 4, "feat/4", "main"),
            later,
        ],
        Vec::new(),
    );
    for item in &mut plan.items {
        item.stage = item.number as usize;
    }
    plan
}

fn fail_deploy(rig: &Rig, number: u64) {
    rig.script().runs_by_sha.insert(
        merge_sha(number),
        vec![run_json("deploy", "completed", Some("failure"))],
    );
}

#[test]
fn leaf_failure_continues_independent_prs_in_order_and_stays_failed_in_json() {
    let mut rig = Rig::new("");
    let plan = deployment_plan();
    assert_eq!(merge_numbers(&plan), vec![1, 2, 3, 4, 5]);
    fail_deploy(&rig, 2);
    let run = rig.go(&plan);
    assert_eq!(
        run.statuses(),
        vec![
            Status::Done,
            Status::Failed,
            Status::Done,
            Status::Done,
            Status::Done
        ]
    );
    assert_eq!(rig.puts().len(), 5);
    assert_eq!(run.outcome.code, 1);
    assert_eq!(rig.store.state.pending.len(), 1);
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(json["steps"][1]["merge_sha"], merge_sha(2));
    assert_eq!(json["steps"][1]["status"], "failed");
    assert_eq!(json["steps"][4]["status"], "done");
}

#[test]
fn publisher_failure_stops_by_default_but_skip_blocks_transitive_consumers() {
    for policy in [DependencyFailure::Stop, DependencyFailure::Skip] {
        let mut rig = Rig::new("");
        let plan = deployment_plan();
        fail_deploy(&rig, 1);
        let run = rig
            .run(
                &plan,
                Settings {
                    dependency_failure: policy,
                    ..applied()
                },
                true,
                true,
            )
            .unwrap();
        assert_eq!(run.outcome.code, 1);
        assert_eq!(run.step(0).status, Status::Failed);
        assert_eq!(run.step(1).status, Status::Skipped);
        assert_eq!(run.step(2).status, Status::Skipped);
        assert_eq!(run.step(4).status, Status::Skipped);
        assert_eq!(run.step(4).blocked_by.as_deref(), Some("acme/sdk#1"));
        assert_eq!(
            run.step(3).status,
            if policy == DependencyFailure::Stop {
                Status::Skipped
            } else {
                Status::Done
            }
        );
        assert_eq!(
            rig.puts().len(),
            if policy == DependencyFailure::Stop {
                1
            } else {
                2
            }
        );
    }
}

#[test]
fn resumed_leaf_failure_uses_saved_graph_even_when_merged_pr_is_absent() {
    let mut first = Rig::new("");
    let mut plan = deployment_plan();
    fail_deploy(&first, 2);
    first.go(&plan);
    let pending = first.store.state.pending[0].clone();
    assert!(pending.continuation.is_some());
    plan.items.retain(|item| item.number >= 3);
    let mut rig = Rig::new("");
    rig.store.state.pending.push(pending);
    fail_deploy(&rig, 2);
    let run = rig.go(&plan);
    assert_eq!(run.step(0).kind, Kind::Resume);
    assert_eq!(
        run.statuses(),
        vec![Status::Failed, Status::Done, Status::Done, Status::Done]
    );
    assert_eq!(rig.puts().len(), 3);
    assert_eq!(rig.store.state.pending.len(), 1);
}

#[test]
fn legacy_resume_without_independence_metadata_stops_safely() {
    let mut first = Rig::new("");
    fail_deploy(&first, 2);
    first.go(&deployment_plan());
    let mut pending = first.store.state.pending[0].clone();
    pending.continuation = None;
    let mut rig = Rig::new("");
    rig.store.state.pending.push(pending);
    fail_deploy(&rig, 2);
    let plan = plan_of(vec![pr("acme/sync", 4, "feat/4", "main")], Vec::new());
    let run = rig.go(&plan);
    assert_eq!(run.statuses(), vec![Status::Failed, Status::Skipped]);
    assert!(run.step(0).detail.contains("no independence proof"));
    assert!(rig.puts().is_empty());
}

#[test]
fn all_skipped_workflows_are_gray_successful_noops_and_do_not_block() {
    let mut rig = Rig::new("");
    rig.script().runs = vec![run_json("publish", "completed", Some("skipped"))];
    let run = rig.go(&plan_on(&[1, 2]));
    assert_eq!(run.statuses(), vec![Status::Skipped, Status::Skipped]);
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.puts().len(), 2);
    assert!(rig.store.state.pending.is_empty());
    for step in &run.outcome.report.steps {
        assert!(step.merge_sha.is_some());
        assert!(step.blocked_by.is_none());
    }
    let color = render(&run.outcome.report, true);
    assert!(color.contains("\x1b[2mskipped:"));
    assert!(!render(&run.outcome.report, false).contains('\x1b'));
}

#[test]
fn auxiliary_skip_does_not_mask_success_or_failure() {
    for (conclusion, expected) in [("success", Status::Done), ("failure", Status::Failed)] {
        let mut rig = Rig::new("");
        rig.script().runs = vec![
            run_json("audit", "completed", Some(conclusion)),
            run_json("publish", "completed", Some("skipped")),
        ];
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, expected);
    }
}

#[test]
fn resumed_failure_rejects_new_heads_without_independence_proof() {
    let mut first = Rig::new("");
    fail_deploy(&first, 2);
    let mut plan = deployment_plan();
    first.go(&plan);
    let mut rig = Rig::new("");
    rig.store
        .state
        .pending
        .push(first.store.state.pending[0].clone());
    plan.items
        .retain(|item| item.number == 3 || item.number == 4);
    plan.items
        .iter_mut()
        .find(|item| item.number == 3)
        .unwrap()
        .sha = "f".repeat(40);
    fail_deploy(&rig, 2);
    let run = rig.go(&plan);
    assert_eq!(
        run.statuses(),
        vec![Status::Failed, Status::Skipped, Status::Done]
    );
    assert!(run.step(1).detail.contains("no saved independence proof"));
    assert_eq!(rig.puts().len(), 1);
    let color = render(&run.outcome.report, true);
    assert!(color.contains("\x1b[33mskipped:"));
    assert!(color.contains("\x1b[32mdone:"));
    assert!(color.contains("failed:"));
    assert!(!render(&run.outcome.report, false).contains('\x1b'));
}

#[test]
fn resumed_publisher_failure_still_stops_after_publisher_leaves_plan() {
    let mut first = Rig::new("");
    fail_deploy(&first, 1);
    let mut plan = deployment_plan();
    first.go(&plan);
    let mut rig = Rig::new("");
    rig.store
        .state
        .pending
        .push(first.store.state.pending[0].clone());
    plan.items.retain(|item| item.number != 1);
    fail_deploy(&rig, 1);
    let run = rig.go(&plan);
    assert_eq!(run.step(0).kind, Kind::Resume);
    assert_eq!(run.step(0).status, Status::Failed);
    assert!(
        run.outcome.report.steps[1..]
            .iter()
            .all(|step| step.status == Status::Skipped)
    );
    assert!(rig.puts().is_empty());
}

#[test]
fn timeout_stops_even_proven_independent_prs() {
    let mut rig = Rig::new("");
    rig.script()
        .runs_by_sha
        .insert(merge_sha(2), vec![run_json("deploy", "in_progress", None)]);
    let run = rig
        .run(
            &deployment_plan(),
            Settings {
                timeout: Some(Duration::from_secs(5)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        run.statuses(),
        vec![
            Status::Done,
            Status::Stopped,
            Status::Skipped,
            Status::Skipped,
            Status::Skipped
        ]
    );
    assert_eq!(rig.puts().len(), 2);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
}

#[test]
fn parallel_real_workers_merge_independent_repositories_with_coordinated_state() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/api\", \"acme/ui\"]");
    rig.cfg.max_parallel = 2;
    let plan = plan_of(
        vec![
            pr("acme/api", 1, "feat/1", "main"),
            pr("acme/ui", 2, "feat/2", "main"),
        ],
        Vec::new(),
    );
    let run = rig.go(&plan);
    assert_eq!(run.statuses(), [Status::Done, Status::Done]);
    assert_eq!(rig.puts().len(), 2);
    assert!(rig.store.state.pending.is_empty());
    assert!(rig.store.state.active_jobs.is_empty());
    assert_eq!(run.observed, 2);
    assert!(!run.phases.is_empty());
    for phase in &run.phases {
        let rows: Vec<_> = phase.lines().collect();
        assert!(rows.len() <= 3, "{phase}");
        assert!(rows[0].starts_with("waiting: "));
        assert!(rows[0].contains("running: "));
        for row in &rows[1..] {
            assert!(
                row.starts_with("acme/api#1 → main | ") || row.starts_with("acme/ui#2 → main | ")
            );
        }
    }
}

#[test]
fn parallel_gates_stay_on_coordinator_and_refusal_prevents_later_merge() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/api\"]");
    rig.cfg.max_parallel = 2;
    let plan = gated_plan();
    let run = rig.run(&plan, applied(), true, false).unwrap();
    assert_eq!(rig.puts().len(), 1);
    assert_eq!(run.gates, 1);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .any(|step| step.blocked_by.is_some())
    );
    assert!(rig.store.state.active_jobs.is_empty());
}

#[test]
fn parallel_resume_reconciles_saved_sha_without_repeating_merge() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/api\"]");
    rig.cfg.max_parallel = 2;
    let plan = plan_on(&[1]);
    rig.script().merged.insert(1);
    rig.store
        .add(pipetrain::merge::Pending {
            recorded_at: None,
            id: "acme/api#1".to_string(),
            repo: "acme/api".to_string(),
            base: "main".to_string(),
            sha: Some(merge_sha(1)),
            continuation: None,
            deploy_prediction: None,
        })
        .unwrap();
    let run = rig.go(&plan);
    assert_eq!(run.outcome.code, 0);
    assert!(rig.puts().is_empty());
    assert!(rig.store.state.pending.is_empty());
    assert!(rig.store.state.active_jobs.is_empty());
}

#[test]
fn parallel_http_merges_overlap_and_pending_state_is_durable_before_put() {
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::Condvar;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let finished = Arc::new(AtomicBool::new(false));
    let signal = finished.clone();
    let overlap = Arc::new((Mutex::new(0usize), Condvar::new()));
    let server_overlap = overlap.clone();
    let directory = common::TempDir::new("parallel");
    let path = directory.join("state.json");
    let state_path = path.clone();
    let server = std::thread::spawn(move || {
        let script = Arc::new(Mutex::new(Script::new()));
        std::thread::scope(|scope| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !signal.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let script = script.clone();
                        let overlap = server_overlap.clone();
                        let state_path = state_path.clone();
                        scope.spawn(move || {
                            stream.set_nonblocking(false).unwrap();
                            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                            let request = common::read_request(&mut stream);
                            let line = request.lines().next().unwrap().trim_end_matches(" HTTP/1.1");
                            let payload = request.split_once("\r\n\r\n").unwrap().1;
                            if line.starts_with("PUT ") {
                                let state: Value = serde_json::from_str(&std::fs::read_to_string(state_path).unwrap()).unwrap();
                                assert!(!state["pending"].as_array().unwrap().is_empty());
                                assert!(!state["active_jobs"].as_array().unwrap().is_empty());
                                let (lock, wake) = &*overlap;
                                let mut count = lock.lock().unwrap();
                                *count += 1;
                                wake.notify_all();
                                let (count, _) = wake.wait_timeout_while(count, Duration::from_secs(2), |count| *count < 2).unwrap();
                                assert_eq!(*count, 2, "network requests must overlap");
                            }
                            let (status, body) = respond(&script, line, payload);
                            let body = body.to_string();
                            let response = format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            stream.write_all(response.as_bytes()).unwrap();
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
    });
    let mut cfg = Config::parse("orgs = [\"acme\"]\nmax_parallel = 2\n[merge]\nno_deploy_repos = [\"acme/api\", \"acme/ui\"]").unwrap();
    cfg.api.max_requests = 50;
    cfg.api.max_per_minute = 1000;
    let mut client = Client::with_timeout(
        "ghp_FAKE0123456789".to_string(),
        &cfg.api,
        false,
        &base,
        Duration::from_secs(3),
    );
    let mut store = Store::open(Some(path.clone())).unwrap();
    let plan = plan_of(
        vec![
            pr("acme/api", 1, "feat/1", "main"),
            pr("acme/ui", 2, "feat/2", "main"),
        ],
        Vec::new(),
    );
    let mut confirm = |_: &str| true;
    let mut gate = |_: &str| panic!("unexpected gate");
    let mut sleep = |_: Duration| panic!("unexpected coordinator sleep");
    let mut observe = |_: &StepReport| {};
    let mut progress = |_: &str| {};
    let result = merge_plan(
        &plan,
        &cfg,
        &mut client,
        &applied(),
        &mut store,
        &mut Hooks {
            confirm: &mut confirm,
            gate: &mut gate,
            sleep: &mut sleep,
            now: &Instant::now,
            observe: &mut observe,
            progress: &mut progress,
        },
    );
    finished.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let outcome = result.unwrap();
    assert_eq!(outcome.code, 0);
    assert_eq!(*overlap.0.lock().unwrap(), 2);
    assert!(store.state.pending.is_empty());
    assert!(store.state.active_jobs.is_empty());
}

#[test]
fn parallel_failed_publication_blocks_wait_and_consumers_without_cancelling_running_work() {
    for policy in [DependencyFailure::Stop, DependencyFailure::Skip] {
        let mut rig = Rig::new("no_deploy_repos = [\"acme/sync\", \"acme/portal\", \"acme/auth\"]");
        rig.cfg.max_parallel = 2;
        let plan = deployment_plan();
        fail_deploy(&rig, 1);
        let run = rig
            .run(
                &plan,
                Settings {
                    dependency_failure: policy,
                    ..applied()
                },
                true,
                true,
            )
            .unwrap();
        assert_eq!(run.outcome.code, 1);
        assert_eq!(
            run.outcome
                .report
                .steps
                .iter()
                .find(|step| step.target == "acme/sync#4")
                .unwrap()
                .status,
            Status::Done
        );
        assert_eq!(rig.puts().len(), 2);
        assert_eq!(rig.store.state.pending.len(), 1);
        assert!(rig.store.state.active_jobs.is_empty());
        for step in run.outcome.report.steps.iter().filter(|step| {
            ["acme/portal#2", "acme/auth#3", "acme/auth#5"].contains(&step.target.as_str())
        }) {
            assert_eq!(step.status, Status::Skipped);
            assert!(step.blocked_by.is_some());
        }
    }
    let mut rig = Rig::new("no_deploy_repos = [\"acme/sync\"]");
    rig.cfg.max_parallel = 2;
    let mut plan = deployment_plan();
    let dependency = &mut plan
        .items
        .iter_mut()
        .find(|item| item.number == 2)
        .unwrap()
        .after[0];
    dependency.rule = pipetrain::planner::Rule::Package;
    dependency.package = Some("Acme.Sdk".to_string());
    dependency.version = Some("1.2.3".to_string());
    fail_deploy(&rig, 1);
    let run = rig
        .run(
            &plan,
            Settings {
                dependency_failure: DependencyFailure::Skip,
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.gates, 0);
    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .any(|step| step.kind == Kind::Wait && step.blocked_by.is_some())
    );
    assert_eq!(
        run.outcome
            .report
            .steps
            .iter()
            .find(|step| step.target == "acme/sync#4")
            .unwrap()
            .status,
        Status::Done
    );
}

#[test]
fn parallel_legacy_failed_resume_stops_before_dispatching_new_independent_work() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/sync\"]");
    rig.cfg.max_parallel = 2;
    rig.store
        .add(pipetrain::merge::Pending {
            recorded_at: None,
            id: "acme/sdk#1".to_string(),
            repo: "acme/sdk".to_string(),
            base: "main".to_string(),
            sha: Some(merge_sha(1)),
            continuation: None,
            deploy_prediction: None,
        })
        .unwrap();
    fail_deploy(&rig, 1);
    let plan = plan_of(vec![pr("acme/sync", 4, "feat/4", "main")], Vec::new());
    let run = rig.go(&plan);
    assert_eq!(run.outcome.code, 1);
    assert!(rig.puts().is_empty());
    assert_eq!(rig.store.state.pending.len(), 1);
    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .find(|step| step.target == "acme/sync#4")
            .unwrap()
            .blocked_by
            .is_some()
    );
}

#[test]
fn bump_dry_run_does_not_offer_author_hidden_consumers() {
    let mut rig = Rig::new("");
    let mut plan = deployment_plan();
    let consumer = plan.items.iter_mut().find(|item| item.number == 2).unwrap();
    consumer.after[0].package = Some("Acme.SDK".into());
    let hidden = consumer.id.clone();
    consumer.hidden = true;
    let run = rig
        .run(
            &plan,
            Settings {
                bump: true,
                ..dry()
            },
            true,
            true,
        )
        .unwrap();
    assert!(!run.outcome.report.bump_candidates.contains(&hidden));
    assert!(rig.puts().is_empty());
    assert!(rig.store.state.bump.is_none());
}

#[test]
fn bump_dry_run_keeps_state_and_github_untouched() {
    let mut rig = Rig::new("");
    let run = rig
        .run(
            &deployment_plan(),
            Settings {
                bump: true,
                ..dry()
            },
            true,
            true,
        )
        .unwrap();
    assert!(
        run.outcome
            .report
            .steps
            .iter()
            .all(|step| step.status == Status::Planned)
    );
    assert!(rig.store.state.bump.is_none());
    assert!(rig.puts().is_empty());
    let text = render(&run.outcome.report, false);
    assert!(text.contains("Dry runs perform no writes"));
    assert!(text.contains("draft PRs targeting consumer heads"));
    let declined = rig
        .run(
            &deployment_plan(),
            Settings {
                bump: true,
                ..applied()
            },
            false,
            true,
        )
        .unwrap();
    assert!(declined.confirmation_text[0].contains("isolated bump branches and draft PRs"));
    assert!(rig.store.state.bump.is_none());
    assert!(rig.puts().is_empty());
}

#[test]
fn bump_partial_preparation_blocks_its_consumer_but_continues_independent_work() {
    use pipetrain::consumer_bump::journal::{Journal, Preparation};
    let mut rig = Rig::new("no_deploy_repos = [\"acme/auth\", \"acme/sync\"]");
    let mut auth = pr("acme/auth", 3, "feat/3", "main");
    auth.body.clear();
    let mut later = pr("acme/auth", 5, "feat/5", "main");
    later.body = "depends-on: acme/auth#3".into();
    let mut plan = plan_of(
        vec![auth, pr("acme/sync", 4, "feat/4", "main"), later],
        Vec::new(),
    );
    for item in &mut plan.items {
        item.stage = item.number as usize;
    }
    let mut journal = Journal::default();
    journal.consumers.insert(
        "acme/auth#3".into(),
        Preparation {
            repository: "acme/auth".into(),
            number: 3,
            head_branch: "feat/3".into(),
            head_sha: "1".repeat(40),
            bump_branch: "pipetrain/bump/3-nonce".into(),
            workspace_root: "isolated".into(),
            completed: false,
            publication_fingerprint: String::new(),
            prepared: None,
        },
    );
    rig.store.state.bump = Some(journal);
    let run = rig
        .run(
            &plan,
            Settings {
                bump: true,
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Skipped);
    assert_eq!(run.step(1).status, Status::Done);
    assert_eq!(run.step(2).status, Status::Skipped);
    assert_eq!(run.step(3).kind, Kind::Bump);
    assert_eq!(run.step(3).status, Status::Stopped);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.puts().len(), 1);
    assert!(run.step(0).detail.contains("prior bump preparation"));
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(json["steps"][3]["bump"]["outcome"], "attention");
}

#[test]
fn bump_all_skipped_publisher_does_not_skip_its_consumer_or_create_review_work() {
    let mut rig = Rig::new("no_deploy_repos = [\"acme/portal\"]");
    let mut plan = deployment_plan();
    plan.items
        .retain(|item| item.number == 1 || item.number == 2);
    let consumer = plan.items.iter_mut().find(|item| item.number == 2).unwrap();
    consumer.after[0].package = Some("Acme.SDK".into());
    rig.script().runs = vec![run_json("publish", "completed", Some("skipped"))];
    let run = rig
        .run(
            &plan,
            Settings {
                bump: true,
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(
        run.statuses(),
        vec![Status::Skipped, Status::Done, Status::Skipped]
    );
    assert_eq!(run.step(2).kind, Kind::Bump);
    assert_eq!(run.outcome.code, 0);
    assert_eq!(rig.puts().len(), 2);
    let journal = rig.store.state.bump.as_ref().unwrap();
    assert!(journal.publishers["acme/sdk#1"].deploy_confirmed);
    assert_eq!(
        journal.publishers["acme/sdk#1"].merge_sha.as_deref(),
        Some(merge_sha(1).as_str())
    );
    assert!(journal.consumers.is_empty());
    assert!(rig.store.state.pending.is_empty());
}
struct FilterState {
    directory: common::TempDir,
}

fn inherited_state(state: &FilterState) -> Vec<u8> {
    std::fs::create_dir_all(state.directory.path()).unwrap();
    let recorded_at = (chrono::Utc::now() - chrono::Duration::hours(26))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let bytes = serde_json::to_vec_pretty(&json!({
        "schema": 2,
        "created_at": recorded_at,
        "updated_at": recorded_at,
        "profile": "acme",
        "queue_id": "original-queue",
        "pending": [
            {"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": merge_sha(1), "recorded_at": recorded_at},
            {"id": "acme/api#2", "repo": "acme/api", "base": "release", "sha": null}
        ]
    })).unwrap();
    std::fs::write(state.directory.join("state.json"), &bytes).unwrap();
    bytes
}

#[test]
fn dry_run_lists_inherited_pending_age_origin_and_stale_in_text_and_json() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let bytes = inherited_state(&state);
    rig.store = state.reopen();
    let run = rig.run(&plan_on(&[3]), dry(), false, false).unwrap();
    let text = render(&run.outcome.report, false);
    for expected in [
        "State\n",
        "state.json",
        "file age 0h",
        "profile: acme (selected: default)",
        "queue: original-queue",
        "inherited pending: acme/api#1 into main at 0000000 (age 26h",
        "stale (warning only)",
        "inherited pending: acme/api#2 into release at SHA unknown (age unknown)",
    ] {
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
    let value = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(value["schema"], 1);
    let preview = &value["state"];
    assert_eq!(
        preview["path"],
        state.directory.join("state.json").display().to_string()
    );
    assert!(preview["file_age_seconds"].as_u64().unwrap() < 60);
    assert_eq!(preview["profile"], "acme");
    assert_eq!(preview["selected_profile"], "default");
    assert_eq!(preview["queue_id"], "original-queue");
    assert_eq!(preview["stale_pending_hours"], 24);
    assert_eq!(preview["pending"][0]["id"], "acme/api#1");
    assert_eq!(preview["pending"][0]["base"], "main");
    assert_eq!(preview["pending"][0]["sha"], "0000000");
    assert!(preview["pending"][0]["age_seconds"].as_u64().unwrap() >= 26 * 3600);
    assert_eq!(preview["pending"][0]["stale"], true);
    assert_eq!(preview["pending"][1]["age"], "age unknown");
    assert!(preview["pending"][1]["age_seconds"].is_null());
    assert_eq!(preview["pending"][1]["stale"], false);
    assert_eq!(run.step(0).kind, Kind::Resume);
    assert_eq!(run.step(1).kind, Kind::Resume);
    assert!(rig.puts().is_empty());
    assert_eq!(
        std::fs::read(state.directory.join("state.json")).unwrap(),
        bytes
    );
    assert!(!state.directory.join("state.json.lock").exists());
}

#[test]
fn configured_stale_pending_hours_changes_only_the_warning() {
    let mut rig = Rig::new("stale_pending_hours = 48");
    let state = FilterState::attach(&mut rig);
    inherited_state(&state);
    rig.store = state.reopen();
    let plan = plan_on(&[3]);
    let fresh = rig.run(&plan, dry(), false, false).unwrap();
    assert!(!fresh.outcome.report.state.pending[0].stale);
    assert!(!render(&fresh.outcome.report, false).contains("stale (warning only)"));
    rig.cfg = Config::parse("orgs = ['acme']\n[merge]\nstale_pending_hours = 1").unwrap();
    let stale = rig.run(&plan, dry(), false, false).unwrap();
    assert!(stale.outcome.report.state.pending[0].stale);
    assert_eq!(stale.outcome.report.state.stale_pending_hours, 1);
    assert_eq!(fresh.statuses(), stale.statuses());
    assert_eq!(
        fresh
            .outcome
            .report
            .steps
            .iter()
            .map(|step| &step.target)
            .collect::<Vec<_>>(),
        stale
            .outcome
            .report
            .steps
            .iter()
            .map(|step| &step.target)
            .collect::<Vec<_>>()
    );
    assert_eq!(rig.store.state.pending.len(), 2);
}

#[test]
fn apply_confirmation_shows_inherited_state_without_rewriting_on_refusal() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let bytes = inherited_state(&state);
    rig.store = state.reopen();
    let run = rig.run(&plan_on(&[3]), applied(), false, false).unwrap();
    assert!(run.outcome.aborted);
    assert_eq!(run.confirms, 1);
    for expected in [
        "State\n",
        "profile: acme",
        "acme/api#1",
        "age 26h",
        "stale (warning only)",
        "age unknown",
    ] {
        assert!(
            run.confirmation_text[0].contains(expected),
            "{}",
            run.confirmation_text[0]
        );
    }
    assert!(rig.puts().is_empty());
    assert_eq!(
        std::fs::read(state.directory.join("state.json")).unwrap(),
        bytes
    );
    let mut second = state.reopen();
    assert!(second.lock().is_err());
    rig.store = Store::in_memory();
    second.lock().unwrap();
    drop(second);
}

#[test]
fn apply_fails_before_confirmation_when_state_is_locked_but_dry_run_can_read_it() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let bytes = inherited_state(&state);
    let mut other = state.reopen();
    other.lock().unwrap();
    rig.store = state.reopen();
    let plan = plan_on(&[3]);
    let error = rig
        .run(&plan, applied(), true, true)
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("another merge execution is active"),
        "{error}"
    );
    assert!(error.contains("state.json.lock"), "{error}");
    assert!(rig.lines().is_empty());
    let run = rig.run(&plan, dry(), false, false).unwrap();
    assert_eq!(run.outcome.report.state.pending.len(), 2);
    assert_eq!(
        std::fs::read(state.directory.join("state.json")).unwrap(),
        bytes
    );
    drop(other);
}

#[test]
fn apply_records_utc_pending_timestamp_and_keeps_stale_resume_behavior() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().runs = vec![run_json("deploy", "in_progress", None)];
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    let saved = state.reopen();
    let timestamp = saved.state.pending[0].recorded_at.as_deref().unwrap();
    assert!(timestamp.ends_with('Z'));
    chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
    assert_eq!(saved.state.profile.as_deref(), Some("default"));
    assert_eq!(saved.state.schema, 2);
    let identity = saved.state.queue_id.clone();
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(state.directory.join("state.json")).unwrap())
            .unwrap();
    value["pending"][0]["recorded_at"] = json!("2000-01-01T00:00:00Z");
    rig.store = Store::in_memory();
    std::fs::write(
        state.directory.join("state.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    rig.store = state.reopen();
    rig.script().runs = green();
    let resumed = rig.go(&plan_on(&[]));
    assert!(resumed.outcome.report.state.pending[0].stale);
    assert_eq!(resumed.step(0).kind, Kind::Resume);
    assert_eq!(resumed.step(0).status, Status::Done);
    assert_eq!(rig.puts().len(), 1);
    let saved = state.reopen();
    assert!(saved.state.pending.is_empty());
    assert_eq!(saved.state.queue_id, identity);
    rig.store = Store::in_memory();
}

impl FilterState {
    fn attach(rig: &mut Rig) -> Self {
        let directory = common::TempDir::new("deploy-filters");
        rig.store = Store::open(Some(directory.join("state.json"))).unwrap();
        Self { directory }
    }

    fn reopen(&self) -> Store {
        Store::open(Some(self.directory.join("state.json"))).unwrap()
    }
}

fn path_workflow(filters: &str) -> Vec<(String, String)> {
    vec![("deploy.yml".into(), format!("on:\n  push:\n{filters}"))]
}

#[test]
fn main_paths_miss_dispenses_without_polling() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    branches: main\n    paths: ['service/**']");
    rig.script().runs.clear();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 0);
    assert_eq!(run.slept, Duration::ZERO);
    assert!(run.said(0).contains("paths"));
    assert!(run.said(0).contains(".github/workflows/deploy.yml"));
    assert!(run.said(0).contains("src/app.rs"));
    assert!(!run.said(0).contains("merge.no_deploy_repos"));
    let value = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(
        value["steps"][0]["deploy_prediction"]["status"],
        "dispensed"
    );
    assert_eq!(
        value["steps"][0]["deploy_prediction"]["workflows"][0]["filter"],
        "paths"
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn main_paths_match_waits_for_deploy_and_passes() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    branches: [main]\n    paths: 'src/**'");
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(run.said(0).contains("deploy passed"));
    assert!(
        matches!(run.step(0).deploy_prediction.as_ref(), Some(pipetrain::workflows::PushRuns::Expected { workflows, .. }) if workflows == &[".github/workflows/deploy.yml"])
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn paths_ignore_excludes_every_changed_file_without_polling() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths-ignore: ['docs/**', '*.md']");
    rig.script().files = vec![
        json!({"filename": "docs/guide.md"}),
        json!({"filename": "README.md"}),
    ];
    rig.script().runs.clear();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 0);
    assert!(run.said(0).contains("paths-ignore"));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn negated_path_patterns_exclude_and_reinclude_files() {
    for (file, polls) in [("src/generated/a.rs", 0), ("src/generated/keep.rs", 2)] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows =
            path_workflow("    paths: ['src/**', '!src/generated/**', 'src/generated/keep.rs']");
        rig.script().files = vec![json!({"filename": file})];
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Done);
        assert_eq!(rig.polls(), polls);
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn renamed_file_previous_name_keeps_deploy_expected() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: ['src/**']");
    rig.script().files = vec![
        json!({"filename": "docs/app.md", "previous_filename": "src/app.rs", "status": "renamed"}),
    ];
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(matches!(
        run.step(0).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Expected { .. })
    ));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn incomplete_pr_files_remain_indeterminate_and_wait() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script().file_total = Some(2);
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(run.said(0).contains("incomplete: read 1 of 2"));
    assert!(matches!(
        run.step(0).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Indeterminate { .. })
    ));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn file_pagination_must_match_the_pr_total() {
    for mode in ["duplicate", "short", "excess"] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows = path_workflow("    paths: 'service/**'");
        rig.script().files = (0..100)
            .map(|n| json!({"filename": format!("docs/{n}.md")}))
            .collect();
        match mode {
            "duplicate" => {
                rig.script().file_total = Some(101);
                rig.script()
                    .file_pages
                    .insert(2, vec![json!({"filename": "docs/0.md"})]);
            }
            "short" => rig.script().file_total = Some(101),
            _ => rig.script().file_total = Some(99),
        }
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Done, "{mode}");
        assert_eq!(rig.polls(), 2, "{mode}");
        assert!(
            matches!(
                run.step(0).deploy_prediction,
                Some(pipetrain::workflows::PushRuns::Indeterminate { .. })
            ),
            "{mode}"
        );
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn more_than_300_changed_files_wait_without_using_filters() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script().file_total = Some(301);
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(run.said(0).contains("more than 300 changed files"));
    assert!(rig.lines().iter().all(|line| !line.contains("/files?")));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn exactly_300_changed_files_are_read_through_the_last_page() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script().files = (0..300)
        .map(|n| json!({"filename": format!("docs/{n}.md")}))
        .collect();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 0);
    assert!(
        rig.lines()
            .iter()
            .any(|line| line.ends_with("/files?per_page=100&page=4"))
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn truncated_workflow_tree_remains_indeterminate_and_waits() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script().tree_truncated = true;
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(run.said(0).contains("truncated"));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn unreadable_workflow_and_unsupported_glob_remain_indeterminate() {
    for workflow in [
        "on: [",
        "on:\n  push:\n    branches: other\n    paths: 'src/[ab].rs'",
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows = vec![("deploy.yml".into(), workflow.into())];
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Done);
        assert_eq!(rig.polls(), 2);
        assert!(matches!(
            run.step(0).deploy_prediction,
            Some(pipetrain::workflows::PushRuns::Indeterminate { .. })
        ));
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn dry_run_reports_each_workflow_prediction_in_text_and_json() {
    for (status, filters) in [
        ("expected", "    paths: 'src/**'"),
        ("dispensed", "    paths: ['service/**']"),
        ("indeterminate", "    paths: 'src/?'"),
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.store.save().unwrap();
        let before = std::fs::read(state.directory.join("state.json")).unwrap();
        rig.script().workflows = path_workflow(filters);
        let run = rig.run(&plan_on(&[1, 2]), dry(), false, false).unwrap();
        let value = serde_json::to_value(&run.outcome.report).unwrap();
        let text = render(&run.outcome.report, false);
        assert_eq!(value["schema"], 1);
        for step in value["steps"].as_array().unwrap() {
            assert_eq!(step["status"], "planned");
            assert_eq!(step["deploy_prediction"]["status"], status);
        }
        assert_eq!(text.matches(&format!("{status}:")).count(), 2, "{text}");
        assert!(text.contains(".github/workflows/deploy.yml"));
        assert_eq!(rig.polls(), 0);
        assert!(rig.puts().is_empty());
        assert_eq!(run.confirms, 0);
        assert_eq!(
            std::fs::read(state.directory.join("state.json")).unwrap(),
            before
        );
    }
}

#[test]
fn pending_prediction_is_saved_and_resumed_without_reading_workflows() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'src/**'");
    rig.script()
        .workflows
        .push(("audit.yml".into(), "on: push".into()));
    rig.script().runs.clear();
    let first = rig.go(&plan_on(&[1]));
    assert_eq!(first.step(0).status, Status::Stopped);
    let saved = state.reopen();
    assert!(
        matches!(saved.state.pending[0].deploy_prediction.as_ref(), Some(pipetrain::workflows::PushRuns::Expected { workflows, .. }) if workflows.len() == 2)
    );
    rig.store = saved;
    rig.script().workflows.clear();
    rig.script().files.clear();
    let mut audit = run_json("audit", "completed", Some("success"));
    audit["path"] = json!(".github/workflows/audit.yml");
    rig.script().runs = vec![green().remove(0), audit];
    let trees = rig
        .lines()
        .iter()
        .filter(|line| line.contains("/git/trees/"))
        .count();
    let second = rig.go(&plan_on(&[]));
    assert_eq!(second.step(0).status, Status::Done);
    assert!(matches!(
        second.step(0).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Expected { .. })
    ));
    assert_eq!(rig.puts().len(), 1);
    assert_eq!(
        rig.lines()
            .iter()
            .filter(|line| line.contains("/git/trees/"))
            .count(),
        trees
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn legacy_pending_without_prediction_keeps_polling_on_resume() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    std::fs::create_dir_all(state.directory.path()).unwrap();
    std::fs::write(state.directory.join("state.json"), serde_json::to_vec(&json!({"schema": 1, "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": merge_sha(1)}]})).unwrap()).unwrap();
    rig.store = state.reopen();
    rig.script().workflows.clear();
    let run = rig.go(&plan_on(&[]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(run.step(0).deploy_prediction.is_none());
    assert!(rig.puts().is_empty());
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn same_repo_and_base_keep_each_pr_file_prediction_separate() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script()
        .files_by_pr
        .insert(2, vec![json!({"filename": "service/app.rs"})]);
    rig.script().put_body = Some(json!({"sha": merge_sha(1), "merged": true}));
    let run = rig.go(&plan_on(&[1, 2]));
    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert_eq!(rig.polls(), 2);
    assert!(matches!(
        run.step(0).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Dispensed { .. })
    ));
    assert!(matches!(
        run.step(1).deploy_prediction,
        Some(pipetrain::workflows::PushRuns::Expected { .. })
    ));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn missing_totals_and_rename_names_remain_indeterminate() {
    for mode in ["total", "filename", "previous_filename", "head"] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows = path_workflow("    paths: 'service/**'");
        match mode {
            "total" => rig.script().metadata_missing_total = true,
            "filename" => rig.script().files = vec![json!({"status": "modified"})],
            "previous_filename" => {
                rig.script().files = vec![json!({"filename": "docs/a.md", "status": "renamed"})]
            }
            _ => {}
        }
        let run = if mode == "head" {
            let mut plan = plan_on(&[1]);
            plan.items[0].sha = "c".repeat(40);
            rig.run(&plan, dry(), false, false).unwrap()
        } else {
            rig.go(&plan_on(&[1]))
        };
        assert!(
            matches!(
                run.step(0).deploy_prediction,
                Some(pipetrain::workflows::PushRuns::Indeterminate { .. })
            ),
            "{mode}"
        );
        assert_eq!(rig.polls(), if mode == "head" { 0 } else { 2 });
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn malformed_workflow_trees_never_dispense_the_wait() {
    for tree in [
        json!({}),
        json!({"tree": [], "truncated": "unknown"}),
        json!({"tree": "missing", "truncated": false}),
        json!({"tree": [{"type": "blob"}], "truncated": false}),
        json!({"tree": [{"path": ".github/workflows/deploy.yml"}], "truncated": false}),
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().tree_body = Some(tree);
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Done);
        assert_eq!(rig.polls(), 2);
        assert!(matches!(
            run.step(0).deploy_prediction,
            Some(pipetrain::workflows::PushRuns::Indeterminate { .. })
        ));
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn workflow_readings_are_reused_for_the_same_sha_and_files() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    rig.script().put_body = Some(json!({"sha": merge_sha(1), "merged": true}));
    let run = rig.go(&plan_on(&[1, 2]));
    assert_eq!(run.statuses(), vec![Status::Done, Status::Done]);
    assert_eq!(rig.polls(), 0);
    assert_eq!(
        rig.lines()
            .iter()
            .filter(|line| line.contains("/git/trees/"))
            .count(),
        1
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn legacy_train_pending_keeps_branch_only_workflow_behavior() {
    for (workflow, polls) in [
        ("on: pull_request", 0),
        (
            "on:\n  push:\n    branches: ['deploy/**']\n    paths: ['service/**']",
            2,
        ),
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        std::fs::create_dir_all(state.directory.path()).unwrap();
        std::fs::write(state.directory.join("state.json"), serde_json::to_vec(&json!({"schema": 1, "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": BUNDLE, "sha": merge_sha(1)}]})).unwrap()).unwrap();
        rig.store = state.reopen();
        rig.script().workflows = vec![("deploy.yml".into(), workflow.into())];
        let run = rig.go(&plan_on(&[]));
        assert_eq!(run.step(0).status, Status::Done);
        assert_eq!(rig.polls(), polls);
        assert!(run.step(0).deploy_prediction.is_none());
        assert!(rig.puts().is_empty());
        assert!(state.reopen().state.pending.is_empty());
    }
}

fn workflow_run(path: &str, name: &str, status: &str, conclusion: Option<&str>) -> Value {
    let mut run = run_json(name, status, conclusion);
    run["path"] = json!(format!(".github/workflows/{path}"));
    run
}

fn short_deploy(rig: &mut Rig) -> Run {
    rig.run(
        &plan_on(&[1]),
        Settings {
            timeout: Some(Duration::from_secs(3)),
            ..applied()
        },
        true,
        true,
    )
    .unwrap()
}

fn chained_workflows(name: &str, trigger: &str) -> Vec<(String, String)> {
    vec![
        ("deploy.yml".into(), format!("name: '{name}'\non: push")),
        (
            "release.yml".into(),
            format!(
                "name: Release\non:\n  workflow_run:\n    workflows: ['{trigger}']\n    types: [completed]"
            ),
        ),
    ]
}

#[test]
fn auxiliary_success_waits_until_the_expected_deploy_finishes() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let auxiliary = workflow_run("lint.yml", "Lint", "completed", Some("success"));
    let running = workflow_run("deploy.yml", "Deploy", "in_progress", None);
    rig.script().run_readings = [
        vec![auxiliary.clone(), running.clone()],
        vec![auxiliary.clone(), running],
    ]
    .into();
    rig.script().runs = vec![
        auxiliary,
        workflow_run("deploy.yml", "Deploy", "completed", Some("success")),
    ];
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_secs(4)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 4);
    assert!(
        run.phases
            .iter()
            .filter(|text| text.contains("expected workflow runs still going"))
            .count()
            >= 2
    );
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(
        json["steps"][0]["expected_runs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(json["steps"][0]["expected_runs"][0]["name"], "Deploy");
    assert_eq!(
        json["steps"][0]["expected_runs"][0]["html_url"],
        "https://example.test/acme/api/actions/runs/Deploy"
    );
    assert!(run.said(0).contains("expected workflow Deploy (.github/workflows/deploy.yml): https://example.test/acme/api/actions/runs/Deploy"));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn auxiliary_success_without_any_expected_run_cannot_confirm_deploy() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().runs = vec![workflow_run(
        "lint.yml",
        "Deploy",
        "completed",
        Some("success"),
    )];
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert!(
        run.said(0)
            .contains("waiting for expected workflows: .github/workflows/deploy.yml")
    );
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn expected_deploy_failure_wins_over_green_auxiliary() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().runs = vec![
        workflow_run("lint.yml", "Lint", "completed", Some("success")),
        workflow_run("deploy.yml", "Deploy", "completed", Some("failure")),
    ];
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Failed);
    assert_eq!(run.outcome.code, 1);
    assert_eq!(rig.polls(), 1);
    assert!(run.said(0).contains("Deploy ended as failure"));
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn auxiliary_failure_is_reported_without_deciding_expected_deploy() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().runs = vec![
        workflow_run("lint.yml", "Lint", "completed", Some("failure")),
        workflow_run("deploy.yml", "Deploy", "completed", Some("success")),
    ];
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Done);
    assert!(run.said(0).contains("auxiliary workflow Lint ended as failure: https://example.test/acme/api/actions/runs/Lint (does not decide deploy)"));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn expected_neutral_is_failure_but_skipped_is_a_successful_noop() {
    for (conclusion, status) in [("neutral", Status::Failed), ("skipped", Status::Skipped)] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().runs = vec![workflow_run(
            "deploy.yml",
            "Deploy",
            "completed",
            Some(conclusion),
        )];
        let run = short_deploy(&mut rig);
        assert_eq!(run.step(0).status, status);
        assert_eq!(
            state.reopen().state.pending.is_empty(),
            status == Status::Skipped
        );
    }
}

#[test]
fn workflow_run_chain_is_followed_by_path_and_same_sha_until_success() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = chained_workflows("Deploy", "Deploy");
    let parent = workflow_run("deploy.yml", "Deploy", "completed", Some("success"));
    let mut child = workflow_run("release.yml", "Release", "in_progress", None);
    child["event"] = json!("workflow_run");
    rig.script().run_readings = [vec![parent.clone()], vec![parent.clone(), child.clone()]].into();
    child["status"] = json!("completed");
    child["conclusion"] = json!("success");
    rig.script().runs = vec![parent, child];
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_secs(4)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 4);
    for request in rig
        .lines()
        .iter()
        .filter(|line| line.contains("/actions/runs?"))
    {
        assert!(request.contains(&format!("head_sha={}", merge_sha(1))));
        assert!(!request.contains("event="));
    }
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(
        json["steps"][0]["deploy_prediction"]["workflows"],
        json!([".github/workflows/deploy.yml"])
    );
    assert_eq!(
        json["steps"][0]["deploy_prediction"]["chained_workflows"],
        json!([".github/workflows/release.yml"])
    );
    assert_eq!(run.step(0).expected_runs.len(), 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn unresolved_chain_times_out_and_is_preserved_for_resume() {
    for name in ["Unknown", "Deploy"] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows = chained_workflows("Deploy", name);
        if name == "Deploy" {
            rig.script().workflows.push((
                "duplicate.yml".into(),
                "name: Deploy\non: workflow_dispatch".into(),
            ));
        }
        let run = short_deploy(&mut rig);
        assert_eq!(run.step(0).status, Status::Stopped);
        assert_eq!(rig.polls(), 3);
        assert!(
            run.said(0)
                .contains("chained workflow .github/workflows/release.yml cannot be tracked")
        );
        assert_eq!(state.reopen().state.pending.len(), 1);
        rig.store = state.reopen();
        rig.script().workflows.clear();
        let resumed = rig
            .run(
                &plan_on(&[]),
                Settings {
                    timeout: Some(Duration::from_secs(1)),
                    ..applied()
                },
                true,
                true,
            )
            .unwrap();
        assert_eq!(resumed.step(0).status, Status::Stopped);
        assert!(
            resumed
                .said(0)
                .contains("chained workflow .github/workflows/release.yml cannot be tracked")
        );
        assert_eq!(rig.puts().len(), 1);
    }
}

#[test]
fn transitive_chain_with_literal_names_is_saved_and_resumed() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = chained_workflows("!Deploy", "!Deploy");
    rig.script().workflows.push((
        "final.yml".into(),
        "on:\n  workflow_run:\n    workflows: Release".into(),
    ));
    let first = short_deploy(&mut rig);
    assert_eq!(first.step(0).status, Status::Stopped);
    rig.store = state.reopen();
    let saved = serde_json::to_value(&rig.store.state).unwrap();
    assert_eq!(
        saved["pending"][0]["deploy_prediction"]["chained_workflows"],
        json!([
            ".github/workflows/final.yml",
            ".github/workflows/release.yml"
        ])
    );
    rig.script().workflows.clear();
    rig.script().runs = vec![
        workflow_run("deploy.yml", "!Deploy", "completed", Some("success")),
        workflow_run("release.yml", "Release", "completed", Some("success")),
        workflow_run("final.yml", "Final", "completed", Some("success")),
    ];
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Done);
    assert_eq!(resumed.step(0).expected_runs.len(), 3);
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn chain_of_known_but_excluded_workflow_is_not_expected() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = vec![
        ("deploy.yml".into(), "name: Deploy\non: push".into()),
        (
            "other.yml".into(),
            "name: Other\non:\n  push:\n    branches: other".into(),
        ),
        (
            "release.yml".into(),
            "on:\n  workflow_run:\n    workflows: [Other]".into(),
        ),
    ];
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(run.step(0).expected_runs.len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn incomplete_run_response_never_confirms_even_with_expected_green() {
    for read in [1, 100] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().runs = (0..read)
            .map(|_| workflow_run("deploy.yml", "Deploy", "completed", Some("success")))
            .collect();
        rig.script().run_total = Some(read + 1);
        let run = short_deploy(&mut rig);
        assert_eq!(run.step(0).status, Status::Stopped);
        assert!(
            run.said(0)
                .contains("workflow run response is incomplete; cannot confirm deploy")
        );
        assert_eq!(state.reopen().state.pending.len(), 1);
    }
}

#[test]
fn legacy_pending_json_keeps_neutral_and_missing_path_compatibility() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    std::fs::create_dir_all(state.directory.path()).unwrap();
    std::fs::write(state.directory.join("state.json"), serde_json::to_vec(&json!({"schema": 1, "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": merge_sha(1)}]})).unwrap()).unwrap();
    rig.store = state.reopen();
    let mut auxiliary = workflow_run("lint.yml", "Lint", "completed", Some("neutral"));
    auxiliary.as_object_mut().unwrap().remove("path");
    rig.script().runs = vec![auxiliary];
    let run = rig.go(&plan_on(&[]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 2);
    assert!(
        rig.lines()
            .iter()
            .filter(|line| line.contains("/actions/runs?"))
            .all(|line| line.contains("event=push"))
    );
    assert!(run.step(0).expected_runs.is_empty());
    assert!(state.reopen().state.pending.is_empty());
}

fn failed_deploy_on_disk(rig: &mut Rig) -> FilterState {
    let state = FilterState::attach(rig);
    rig.script().runs = vec![run_json("deploy", "completed", Some("failure"))];
    let failed = rig.go(&plan_on(&[1]));
    assert_eq!(failed.step(0).status, Status::Failed);
    rig.store = state.reopen();
    state
}

#[test]
fn parallel_progress_renders_both_active_prs_with_their_phases() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.max_parallel = 2;
    let shared = rig.script.clone();
    rig.server = serve_requests(move |line, payload| {
        if line.contains("/actions/runs?") {
            let ready = shared.lock().unwrap().merged.len() == 2;
            if !ready {
                return (
                    200,
                    json!({"total_count": 1, "workflow_runs": [run_json("deploy", "in_progress", None)]}),
                );
            }
        }
        respond(&shared, line, payload)
    });
    let plan = plan_of(
        vec![
            pr("acme/api", 1, "feat/1", "main"),
            pr("acme/ui", 2, "feat/2", "main"),
        ],
        Vec::new(),
    );
    let run = rig.go(&plan);
    let frame =
        run.phases
            .iter()
            .find(|frame| {
                frame.starts_with("waiting: 0, running: 2, merged: 2, deploy confirmed: 0")
                    && frame.lines().skip(1).count() == 2
                    && frame.lines().skip(1).all(|line| {
                        line.contains("waiting for deploy: .github/workflows/deploy.yml")
                    })
            })
            .unwrap_or_else(|| panic!("both deploy waits must be visible: {:?}", run.phases));
    let sink = ProgressSink::default();
    let mut painter = pipetrain::progress::Painter::new(sink.clone(), true, Duration::ZERO);
    painter.set_phase(frame);
    painter.tick(Duration::from_secs(1));
    let rendered = sink.text();
    assert!(rendered.contains("acme/api#1 → main | waiting for deploy:"));
    assert!(rendered.contains("acme/ui#2 → main | waiting for deploy:"));
    assert_eq!(rendered.matches(" · ").count(), 3);
    assert_eq!(run.outcome.report.summary.merged, 2);
    assert_eq!(run.outcome.report.summary.deploy_confirmed, 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[derive(Clone, Default)]
struct ProgressSink(Arc<Mutex<Vec<u8>>>);

impl ProgressSink {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for ProgressSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn merge_summary_separates_merge_and_deploy_outcomes_in_text_and_json() {
    for case in [
        "confirmed",
        "dispensed",
        "indeterminate",
        "failed",
        "skipped",
        "timeout",
        "readiness",
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        match case {
            "dispensed" => rig.script().workflows = path_workflow("    paths: 'service/**'"),
            "indeterminate" => rig.script().tree_status = 500,
            "failed" => rig.script().runs = vec![run_json("deploy", "completed", Some("failure"))],
            "skipped" => rig.script().runs = vec![run_json("deploy", "completed", Some("skipped"))],
            "timeout" => rig.script().runs.clear(),
            "readiness" => {
                rig.script().review.insert(1, "CHANGES_REQUESTED".into());
            }
            _ => {}
        }
        let run = rig.go(&plan_on(&[1]));
        let summary = &run.outcome.report.summary;
        let value = serde_json::to_value(&run.outcome.report).unwrap();
        assert_eq!(summary.merged, usize::from(case != "readiness"), "{case}");
        assert_eq!(
            summary.deploy_confirmed,
            usize::from(case == "confirmed"),
            "{case}"
        );
        assert_eq!(
            summary.deploy_dispensed,
            usize::from(case == "dispensed"),
            "{case}"
        );
        assert_eq!(
            summary.deploy_indeterminate,
            usize::from(matches!(case, "indeterminate" | "timeout")),
            "{case}"
        );
        assert_eq!(
            summary.deploy_skipped,
            usize::from(case == "skipped"),
            "{case}"
        );
        assert_eq!(
            summary.deploy_failed,
            usize::from(case == "failed"),
            "{case}"
        );
        assert_eq!(
            summary.failed,
            usize::from(matches!(case, "failed" | "readiness")),
            "{case}"
        );
        assert_eq!(summary.stopped, usize::from(case == "timeout"), "{case}");
        assert_eq!(summary.deploy_resolved, 0);
        assert_eq!(value["schema"], 1);
        assert_eq!(value["summary"]["merged"], summary.merged);
        assert_eq!(
            value["summary"]["deploy_confirmed"],
            summary.deploy_confirmed
        );
        let text = render_final(&run.outcome.report, false);
        assert!(text.contains(&format!("{} PRs merged", summary.merged)));
        assert!(text.contains(&format!("deploys: {} confirmed, {} dispensed, {} indeterminate, 0 resolved by later deploy, {} skipped, {} failed", summary.deploy_confirmed, summary.deploy_dispensed, summary.deploy_indeterminate, summary.deploy_skipped, summary.deploy_failed)));
        assert_eq!(
            state.reopen().state.pending.len(),
            usize::from(matches!(case, "failed" | "timeout"))
        );
    }
}

#[test]
fn merge_summary_counts_later_resolution_separately_from_confirmation() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    {
        let mut script = rig.script();
        script.later_tip = Some("d".repeat(40));
        script.compare_status = "ahead".into();
        script.runs_by_sha.insert("d".repeat(40), green());
    }
    let run = rig.go(&plan_on(&[]));
    let value = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(value["summary"]["merged"], 0);
    assert_eq!(value["summary"]["earlier_merges_checked"], 1);
    assert_eq!(value["summary"]["deploy_confirmed"], 0);
    assert_eq!(value["summary"]["deploy_resolved"], 1);
    assert_eq!(value["summary"]["deploy_failed"], 0);
    assert!(render_final(&run.outcome.report, false).contains("1 resolved by later deploy"));
    assert!(
        run.phases
            .iter()
            .any(|frame| frame.contains("| resolving:"))
    );
    assert_eq!(state.reopen().state.audit.len(), 1);
}

#[test]
fn merge_preview_shows_configured_parallelism_in_dry_run_and_confirmation() {
    for limit in [1, 4] {
        let mut rig = Rig::new("");
        let _state = FilterState::attach(&mut rig);
        rig.cfg.max_parallel = limit;
        let plan = plan_on(&[1]);
        let dry = rig
            .run(
                &plan,
                Settings {
                    apply: false,
                    ..applied()
                },
                true,
                true,
            )
            .unwrap();
        let line =
            format!("up to {limit} PRs at a time, one per repository (including different bases)");
        assert!(render(&dry.outcome.report, false).contains(&line));
        let value = serde_json::to_value(&dry.outcome.report).unwrap();
        assert_eq!(value["max_parallel"], limit);
        assert_eq!(value["summary"]["merged"], 0);
        assert_eq!(value["summary"]["deploy_confirmed"], 0);
        let declined = rig.run(&plan, applied(), false, true).unwrap();
        assert!(declined.confirmation_text[0].contains(&line));
        assert!(rig.puts().is_empty());
    }
}

#[test]
fn later_descendant_deploy_resolves_with_audit_and_unblocks_same_target() {
    for parallelism in [1, 2] {
        let mut rig = Rig::new("");
        let state = failed_deploy_on_disk(&mut rig);
        rig.cfg.max_parallel = parallelism;
        let tip = "d".repeat(40);
        {
            let mut script = rig.script();
            script.later_tip = Some(tip.clone());
            script.compare_status = "ahead".into();
            let failed_runs = script.runs.clone();
            script.runs_by_sha.insert(merge_sha(1), failed_runs);
            script.runs_by_sha.insert(tip.clone(), green());
            script.runs = green();
        }
        let resumed = rig.go(&plan_on(&[2]));
        assert_eq!(
            resumed.outcome.code,
            0,
            "{}",
            render(&resumed.outcome.report, false)
        );
        assert_eq!(resumed.step(0).status, Status::Done);
        assert_eq!(
            resumed.step(0).merge_sha.as_deref(),
            Some(merge_sha(1).as_str())
        );
        assert!(
            resumed
                .said(0)
                .contains("resolved by a later deploy of ddddddd")
        );
        assert_eq!(resumed.step(1).status, Status::Done);
        let report = serde_json::to_value(resumed.step(0)).unwrap();
        assert_eq!(report["resolution"]["status"], "resolved_by_later_deploy");
        assert_eq!(report["resolution"]["sha"], tip);
        assert_eq!(
            report["resolution"]["runs"][0]["html_url"],
            "https://example.test/acme/api/actions/runs/deploy"
        );
        let saved = state.reopen();
        assert!(saved.state.pending.is_empty());
        let audit = serde_json::to_value(&saved.state.audit).unwrap();
        assert_eq!(audit.as_array().unwrap().len(), 1);
        assert_eq!(audit[0]["action"], "resolved_by_later_deploy");
        assert_eq!(audit[0]["sha"], merge_sha(1));
        assert_eq!(audit[0]["resolution"], report["resolution"]);
        chrono::DateTime::parse_from_rfc3339(audit[0]["recorded_at"].as_str().unwrap()).unwrap();
    }
}

#[test]
fn unrelated_later_commit_does_not_resolve_or_poll_its_green_deploy() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    let tip = "d".repeat(40);
    rig.script().later_tip = Some(tip.clone());
    rig.script().runs_by_sha.insert(tip.clone(), green());
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Failed);
    assert!(resumed.said(0).contains("does not contain saved SHA"));
    assert!(
        !rig.lines()
            .iter()
            .any(|line| line.contains(&format!("head_sha={tip}")))
    );
    assert_eq!(state.reopen().state.pending.len(), 1);
    assert!(state.reopen().state.audit.is_empty());
}

#[test]
fn unchanged_failed_deploy_names_sha_run_and_explains_repeating_will_not_help() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    rig.script().later_tip = Some(merge_sha(1));
    let resumed = rig.go(&plan_on(&[]));
    let text = resumed.said(0);
    assert!(text.contains(&merge_sha(1)));
    assert!(text.contains("https://example.test/acme/api/actions/runs/deploy"));
    assert!(text.contains("no later commit exists; repeating unchanged will not help"));
    assert_eq!(state.reopen().state.pending.len(), 1);
    assert!(state.reopen().state.audit.is_empty());
}

#[test]
fn later_deploy_needs_complete_expected_success_and_run_evidence() {
    for case in [
        "auxiliary",
        "failed",
        "running",
        "skipped",
        "incomplete",
        "missing_url",
        "chain",
    ] {
        let mut rig = Rig::new("");
        let state = failed_deploy_on_disk(&mut rig);
        let tip = "d".repeat(40);
        let mut runs = green();
        match case {
            "auxiliary" => runs[0]["path"] = json!(".github/workflows/lint.yml"),
            "failed" => runs[0]["conclusion"] = json!("failure"),
            "running" => {
                runs[0]["status"] = json!("in_progress");
                runs[0]["conclusion"] = Value::Null;
            }
            "skipped" => runs[0]["conclusion"] = json!("skipped"),
            "incomplete" => rig.script().run_total = Some(101),
            "missing_url" => runs[0]["html_url"] = json!(""),
            "chain" => {
                rig.store.state.pending[0].deploy_prediction =
                    Some(pipetrain::workflows::PushRuns::Expected {
                        workflows: vec![".github/workflows/deploy.yml".into()],
                        chained_workflows: vec![".github/workflows/promote.yml".into()],
                        untracked_chains: Vec::new(),
                    });
                rig.store.save().unwrap();
            }
            _ => unreachable!(),
        }
        rig.script().later_tip = Some(tip.clone());
        rig.script().compare_status = "ahead".into();
        rig.script().runs_by_sha.insert(tip, runs);
        let resumed = rig.go(&plan_on(&[]));
        assert_eq!(resumed.step(0).status, Status::Failed, "{case}");
        assert!(resumed.step(0).resolution.is_none(), "{case}");
        assert_eq!(state.reopen().state.pending.len(), 1, "{case}");
        assert!(state.reopen().state.audit.is_empty(), "{case}");
    }
}

#[test]
fn legacy_pending_resolves_later_deploy_using_all_push_runs() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    rig.store.state.pending[0].deploy_prediction = None;
    rig.store.save().unwrap();
    let tip = "d".repeat(40);
    rig.script().later_tip = Some(tip.clone());
    rig.script().compare_status = "ahead".into();
    rig.script().runs_by_sha.insert(tip.clone(), green());
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Done);
    assert!(
        rig.lines()
            .iter()
            .filter(|line| line.contains(&format!("head_sha={tip}")))
            .all(|line| line.contains("event=push"))
    );
    assert_eq!(state.reopen().state.audit.len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn later_resolution_requires_two_passed_readings() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    rig.script().later_tip = Some("d".repeat(40));
    rig.script().compare_status = "ahead".into();
    rig.script().run_readings = [
        vec![run_json("deploy", "completed", Some("failure"))],
        green(),
        vec![run_json("deploy", "in_progress", None)],
    ]
    .into();
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Failed);
    assert!(resumed.step(0).resolution.is_none());
    assert_eq!(state.reopen().state.pending.len(), 1);
    assert!(state.reopen().state.audit.is_empty());
}

#[test]
fn later_resolution_tracks_saved_chains_after_recovering_unknown_merge_sha() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    rig.store.state.pending[0].sha = None;
    rig.store.state.pending[0].deploy_prediction = Some(pipetrain::workflows::PushRuns::Expected {
        workflows: vec![".github/workflows/deploy.yml".into()],
        chained_workflows: vec![".github/workflows/promote.yml".into()],
        untracked_chains: Vec::new(),
    });
    rig.store.save().unwrap();
    let tip = "d".repeat(40);
    let mut promote = run_json("promote", "completed", Some("success"));
    promote["path"] = json!(".github/workflows/promote.yml");
    let mut lint = run_json("lint", "completed", Some("failure"));
    lint["path"] = json!(".github/workflows/lint.yml");
    rig.script().later_tip = Some(tip.clone());
    rig.script().compare_status = "ahead".into();
    let mut runs = green();
    runs.extend([promote, lint]);
    rig.script().runs_by_sha.insert(tip.clone(), runs);
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Done);
    assert_eq!(resumed.step(0).resolution.as_ref().unwrap().runs.len(), 2);
    let saved = state.reopen();
    assert_eq!(
        saved.state.audit[0].sha.as_deref(),
        Some(merge_sha(1).as_str())
    );
    assert!(saved.state.pending.is_empty());
    assert!(
        rig.lines()
            .iter()
            .filter(|line| line.contains(&format!("head_sha={tip}")))
            .all(|line| !line.contains("event=push"))
    );
}

#[test]
fn discarded_record_is_audited_and_never_resumed_or_reported_as_confirmed() {
    let mut rig = Rig::new("");
    let state = failed_deploy_on_disk(&mut rig);
    rig.store = Store::in_memory();
    let config = state.directory.join("config.toml");
    std::fs::write(&config, "orgs = [\"acme\"]").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args([
            "state",
            "discard",
            "acme/api#1",
            "--reason",
            "superseded incident",
            "--yes",
            "--state",
        ])
        .arg(state.directory.join("state.json"))
        .arg("--config")
        .arg(config)
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .env("PIPETRAIN_LOG", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    rig.store = state.reopen();
    assert!(rig.store.state.pending.is_empty());
    let audit = serde_json::to_value(&rig.store.state.audit).unwrap();
    assert_eq!(audit[0]["action"], "discarded");
    assert_eq!(audit[0]["reason"], "superseded incident");
    assert_eq!(audit[0]["sha"], merge_sha(1));
    assert!(audit[0].get("resolution").is_none());
    let before = rig.lines().len();
    let resumed = rig.go(&plan_on(&[]));
    assert!(resumed.outcome.report.steps.is_empty());
    assert_eq!(rig.lines().len(), before);
    assert!(!render(&resumed.outcome.report, false).contains("deploy passed"));
}

#[test]
fn auxiliary_with_same_name_cannot_replace_cancelled_expected_run() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let mut cancelled = workflow_run("deploy.yml", "Deploy", "completed", Some("cancelled"));
    cancelled["id"] = json!(1);
    let mut auxiliary = workflow_run("lint.yml", "Deploy", "completed", Some("success"));
    auxiliary["id"] = json!(2);
    rig.script().runs = vec![cancelled, auxiliary];
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Failed);
    assert!(run.said(0).contains("ended as cancelled"));
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn unreadable_chain_reference_cannot_confirm_green_push() {
    for trigger in [
        "on: workflow_run",
        "on: [workflow_run]",
        "on:\n  workflow_run:",
        "on:\n  workflow_run:\n    workflows: [Deploy, 5]",
    ] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script().workflows = vec![
            ("deploy.yml".into(), "name: Deploy\non: push".into()),
            ("release.yml".into(), trigger.into()),
        ];
        let run = short_deploy(&mut rig);
        assert_eq!(run.step(0).status, Status::Stopped, "{trigger}");
        assert!(
            run.said(0)
                .contains("chained workflow .github/workflows/release.yml cannot be tracked")
        );
        assert_eq!(state.reopen().state.pending.len(), 1);
    }
}

#[test]
fn untrackable_chain_uses_timeout_even_without_runs() {
    let mut rig = Rig::new("");
    rig.cfg.merge.no_run_grace_seconds = 0;
    let state = FilterState::attach(&mut rig);
    rig.script().workflows = vec![
        ("deploy.yml".into(), "on: push".into()),
        (
            "release.yml".into(),
            "on:\n  workflow_run:\n    workflows: [Deploy]".into(),
        ),
    ];
    rig.script().runs.clear();
    let run = short_deploy(&mut rig);
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(rig.polls(), 3);
    assert_eq!(run.slept, Duration::from_secs(3));
    assert!(
        run.said(0)
            .contains("chained workflow .github/workflows/release.yml cannot be tracked")
    );
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn saved_push_prediction_without_chain_fields_still_tracks_expected_paths() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    std::fs::create_dir_all(state.directory.path()).unwrap();
    std::fs::write(state.directory.join("state.json"), serde_json::to_vec(&json!({"schema": 1, "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": merge_sha(1), "deploy_prediction": {"status": "expected", "workflows": [".github/workflows/deploy.yml"]}}]})).unwrap()).unwrap();
    rig.store = state.reopen();
    rig.script().runs = vec![workflow_run(
        "lint.yml",
        "Lint",
        "completed",
        Some("success"),
    )];
    let first = rig
        .run(
            &plan_on(&[]),
            Settings {
                timeout: Some(Duration::from_secs(1)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(first.step(0).status, Status::Stopped);
    assert_eq!(state.reopen().state.pending.len(), 1);
    rig.store = state.reopen();
    rig.script().runs = green();
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Done);
    assert_eq!(resumed.step(0).expected_runs.len(), 1);
    assert!(rig.puts().is_empty());
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn shared_preflight_rejects_new_graphql_blockers_before_any_merge_write() {
    for state in ["BLOCKED", "DIRTY", "UNSTABLE", "DRAFT", "BEHIND"] {
        let mut rig = Rig::new("");
        let state_file = FilterState::attach(&mut rig);
        rig.script().graphql_states.insert(1, state.to_string());
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Failed);
        assert!(run.step(0).detail.contains("no longer ready"));
        assert!(!run.step(0).detail.trim().is_empty());
        assert!(rig.puts().is_empty());
        assert!(state_file.reopen().state.pending.is_empty());
    }
}

#[test]
fn graphql_unknown_merge_state_alone_retries_for_a_minute_without_writes() {
    let mut rig = Rig::new("");
    let state_file = FilterState::attach(&mut rig);
    rig.script().graphql_states.insert(1, "UNKNOWN".to_string());
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(run.slept, Duration::from_secs(60));
    assert!(rig.puts().is_empty());
    assert!(state_file.reopen().state.pending.is_empty());
}

#[test]
fn hooks_pass_preflight_but_executor_still_requires_clean_from_rest() {
    let mut rig = Rig::new("");
    let state_file = FilterState::attach(&mut rig);
    rig.script()
        .graphql_states
        .insert(1, "HAS_HOOKS".to_string());
    rig.script().live_state.insert(1, "has_hooks".to_string());
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Failed);
    assert!(run.step(0).detail.contains("not clean"));
    assert!(rig.puts().is_empty());
    assert!(state_file.reopen().state.pending.is_empty());
}

fn package_plan(package: &str, version: Option<&str>, published: Option<bool>) -> Plan {
    let mut consumer = pr("acme/api", 2, "feat/2", "main");
    consumer.body = "depends-on: acme/api#1".into();
    let mut plan = plan_of(
        vec![pr("acme/api", 1, "feat/1", "main"), consumer],
        Vec::new(),
    );
    let dependency = &mut plan
        .items
        .iter_mut()
        .find(|item| item.number == 2)
        .unwrap()
        .after[0];
    dependency.rule = pipetrain::planner::Rule::Package;
    dependency.package = Some(package.into());
    dependency.version = version.map(str::to_string);
    dependency.published = published;
    plan
}

fn package_feed_rig(
    handler: impl Fn(usize) -> (u16, Value) + Send + 'static,
) -> (Rig, Settings, Arc<std::sync::atomic::AtomicUsize>) {
    let mut rig = Rig::new(
        "no_deploy_repos = ['acme/api']\npackage_poll_seconds = 1\npackage_wait_seconds = 5",
    );
    let shared = rig.script.clone();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = reads.clone();
    rig.server = serve_requests(move |line, payload| {
        if line == "GET /@acme%2fkit" {
            let merged = &shared.lock().unwrap().merged;
            assert!(merged.contains(&1));
            assert!(!merged.contains(&2));
            return handler(count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1);
        }
        respond(&shared, line, payload)
    });
    let settings = Settings {
        feed_registry: Some(rig.server.base.clone()),
        ..applied()
    };
    (rig, settings, reads)
}

#[test]
fn package_wait_rechecks_after_publisher_and_releases_consumer_without_prompt() {
    for parallel in [1, 2] {
        let (mut rig, settings, reads) = package_feed_rig(|attempt| {
            if attempt < 3 {
                (200, json!({"versions": {"1.0.0": {}}}))
            } else {
                (200, json!({"versions": {"1.0.0": {}, "1.1.0": {}}}))
            }
        });
        rig.cfg.max_parallel = parallel;
        let state = FilterState::attach(&mut rig);
        let run = rig
            .run(
                &package_plan("@acme/kit", Some("1.1.0"), Some(false)),
                settings,
                true,
                false,
            )
            .unwrap();
        assert_eq!(run.outcome.code, 0);
        assert_eq!(
            run.statuses(),
            vec![Status::Done, Status::Skipped, Status::Done]
        );
        assert_eq!(run.gates, 0);
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(run.step(1).detail.contains("@acme/kit 1.1.0: published in"));
        assert!(run.step(1).detail.contains(&rig.server.base));
        assert_eq!(rig.puts().len(), 2);
        assert!(state.reopen().state.pending.is_empty());
        assert!(
            rig.lines()
                .iter()
                .filter(|line| line.contains("@acme%2fkit"))
                .all(|line| line.starts_with("GET "))
        );
    }
}

#[test]
fn package_wait_timeout_keeps_manual_confirmation_and_gates_confirmed() {
    for (gate, confirmed) in [(false, false), (true, false), (false, true)] {
        let (mut rig, mut settings, reads) = package_feed_rig(|_| (200, json!({"versions": {}})));
        rig.cfg.merge.package_wait_seconds = 1;
        settings.gates_confirmed = confirmed;
        let state = FilterState::attach(&mut rig);
        let run = rig
            .run(
                &package_plan("@acme/kit", Some("1.1.0"), None),
                settings,
                true,
                gate,
            )
            .unwrap();
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(run.gates, usize::from(!confirmed));
        if gate || confirmed {
            assert_eq!(
                run.statuses(),
                vec![Status::Done, Status::Done, Status::Done]
            );
            assert_eq!(run.outcome.code, 0);
            assert_eq!(rig.puts().len(), 2);
            if confirmed {
                assert!(run.step(1).detail.contains("--gates-confirmed"));
            }
        } else {
            assert_eq!(
                run.statuses(),
                vec![Status::Done, Status::Stopped, Status::Skipped]
            );
            assert_eq!(run.outcome.code, EXIT_STOPPED);
            assert!(run.step(1).detail.contains("not confirmed"));
            assert_eq!(rig.puts().len(), 1);
        }
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn package_wait_refused_feed_stops_with_cause_even_with_gates_confirmed() {
    for parallel in [1, 2] {
        for status in [401, 403] {
            let (mut rig, mut settings, reads) = package_feed_rig(move |_| (status, json!({})));
            rig.cfg.max_parallel = parallel;
            settings.gates_confirmed = true;
            let state = FilterState::attach(&mut rig);
            let run = rig
                .run(
                    &package_plan("@acme/kit", Some("1.1.0"), Some(true)),
                    settings,
                    true,
                    true,
                )
                .unwrap();
            assert_eq!(run.step(1).status, Status::Stopped);
            assert_eq!(run.step(2).status, Status::Skipped);
            assert_eq!(run.outcome.code, EXIT_STOPPED);
            assert_eq!(run.gates, 0);
            assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(run.step(1).detail.contains("401/403"));
            assert!(run.step(1).detail.contains("read:packages"));
            assert_eq!(rig.puts().len(), 1);
            assert!(state.reopen().state.pending.is_empty());
        }
    }
}

#[test]
fn package_wait_without_version_keeps_manual_gate_without_reading_feed() {
    let (mut rig, settings, reads) = package_feed_rig(|_| panic!("no version to check"));
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", None, None),
            settings,
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.step(1).status, Status::Stopped);
    assert_eq!(run.gates, 1);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_dry_run_does_not_read_feed() {
    let (mut rig, settings, reads) = package_feed_rig(|_| panic!("dry run must not check"));
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), None),
            Settings {
                apply: false,
                ..settings
            },
            true,
            true,
        )
        .unwrap();
    assert!(
        run.statuses()
            .iter()
            .all(|status| *status == Status::Planned)
    );
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(run.gates, 0);
    assert!(rig.puts().is_empty());
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_retries_first_release_not_found() {
    let (mut rig, settings, reads) = package_feed_rig(|attempt| {
        if attempt == 1 {
            (404, json!({}))
        } else {
            (200, json!({"versions": {"1.1.0": {}}}))
        }
    });
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), None),
            settings,
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.outcome.code, 0);
    assert_eq!(run.gates, 0);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(rig.puts().len(), 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_failed_feed_stops_with_cause_at_timeout() {
    let (mut rig, settings, reads) = package_feed_rig(|_| (500, json!({})));
    rig.cfg.merge.package_wait_seconds = 0;
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), None),
            settings,
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(1).status, Status::Stopped);
    assert_eq!(run.gates, 0);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(run.step(1).detail.contains("status 500"));
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_config_defaults_to_ten_minutes_and_ten_second_polls() {
    let cfg = Config::parse("orgs = ['acme']").unwrap();
    assert_eq!(cfg.merge.package_wait_seconds, 600);
    assert_eq!(cfg.merge.package_poll_seconds, 10);
}

#[test]
fn package_wait_published_snapshot_is_rechecked_and_cannot_release_absent_version() {
    let (mut rig, settings, reads) = package_feed_rig(|_| (200, json!({"versions": {}})));
    rig.cfg.merge.package_wait_seconds = 0;
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), Some(true)),
            settings,
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.step(1).status, Status::Stopped);
    assert_eq!(run.gates, 1);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_retries_transient_errors_and_normalizes_zero_interval() {
    let (mut rig, settings, reads) = package_feed_rig(|attempt| {
        if attempt == 1 {
            (500, json!({}))
        } else {
            (200, json!({"versions": {"1.1.0": {}}}))
        }
    });
    rig.cfg.merge.package_poll_seconds = 0;
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), None),
            settings,
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.outcome.code, 0);
    assert_eq!(run.gates, 0);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(run.slept, Duration::from_secs(1));
    assert!(run.phases.iter().any(|text| text.contains("status 500")));
    assert_eq!(rig.puts().len(), 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_zero_timeout_still_checks_a_published_version() {
    let (mut rig, settings, reads) =
        package_feed_rig(|_| (200, json!({"versions": {"1.1.0": {}}})));
    rig.cfg.merge.package_wait_seconds = 0;
    let state = FilterState::attach(&mut rig);
    let run = rig
        .run(
            &package_plan("@acme/kit", Some("1.1.0"), None),
            settings,
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.outcome.code, 0);
    assert_eq!(run.gates, 0);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(run.slept, Duration::ZERO);
    assert_eq!(rig.puts().len(), 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn package_wait_uses_nuget_service_index_and_records_feed_evidence() {
    let mut rig = Rig::new(
        "no_deploy_repos = ['acme/api']\npackage_poll_seconds = 1\npackage_wait_seconds = 5",
    );
    let state = FilterState::attach(&mut rig);
    let shared = rig.script.clone();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = reads.clone();
    let versions = common::serve_handler(move |line| {
        assert_eq!(line, "GET /flat/acme.kit/index.json");
        assert!(shared.lock().unwrap().merged.contains(&1));
        if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            (200, json!({"versions": ["1.0.0"]}))
        } else {
            (200, json!({"versions": ["1.0.0", "1.1.0"]}))
        }
    });
    let address = format!("{}/flat/", versions.base);
    let index = common::serve_handler(move |line| {
        assert_eq!(line, "GET /index.json");
        (
            200,
            json!({"resources": [{"@id": address, "@type": "PackageBaseAddress/3.0.0"}]}),
        )
    });
    let registry = format!("{}/index.json", index.base);
    let run = rig
        .run(
            &package_plan("Acme.Kit", Some("1.1.0"), None),
            Settings {
                feed_registry: Some(registry.clone()),
                ..applied()
            },
            true,
            false,
        )
        .unwrap();
    assert_eq!(run.outcome.code, 0);
    assert_eq!(run.gates, 0);
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(index.seen.lock().unwrap().len(), 2);
    assert!(run.step(1).detail.contains("Acme.Kit 1.1.0: published in"));
    assert!(run.step(1).detail.contains(&registry));
    assert_eq!(rig.puts().len(), 2);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn deploy_deadline_counts_time_spent_in_http_and_preserves_pending() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().http_elapsed.insert(
        "GET /repos/acme/api/actions/runs".into(),
        Duration::from_secs(6),
    );
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_secs(5)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.polls(), 1);
    assert_eq!(run.slept, Duration::ZERO);
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn deploy_sleep_is_capped_by_remaining_deadline() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.merge.poll_seconds = 15;
    rig.script().runs = vec![run_json("deploy", "in_progress", None)];
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_millis(2500)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(rig.polls(), 1);
    assert_eq!(run.slept, Duration::from_millis(2500));
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn no_run_grace_counts_http_and_caps_the_sleep() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.merge.poll_seconds = 15;
    rig.script().http_elapsed.insert(
        "GET /repos/acme/api/actions/runs".into(),
        Duration::from_secs(2),
    );
    rig.script().runs.clear();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Stopped);
    assert!(run.said(0).contains("no workflow run appeared"));
    assert_eq!(run.slept, Duration::from_secs(1));
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn both_mergeability_deadlines_count_slow_http_and_cap_sleeps() {
    for route in ["POST /graphql", "GET /repos/acme/api/pulls/1"] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script()
            .http_elapsed
            .insert(route.into(), Duration::from_secs(28));
        if route == "POST /graphql" {
            rig.script().graphql_states.insert(1, "UNKNOWN".into());
        } else {
            rig.script().live_state.insert(1, "unknown".into());
        }
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Stopped, "{route}");
        assert_eq!(run.outcome.code, EXIT_STOPPED);
        assert_eq!(run.slept, Duration::from_secs(5));
        assert!(rig.puts().is_empty());
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn mergeability_sleep_uses_only_the_remaining_real_time() {
    for route in ["POST /graphql", "GET /repos/acme/api/pulls/1"] {
        let mut rig = Rig::new("");
        let state = FilterState::attach(&mut rig);
        rig.script()
            .http_elapsed
            .insert(route.into(), Duration::from_secs(58));
        if route == "POST /graphql" {
            rig.script().graphql_states.insert(1, "UNKNOWN".into());
        } else {
            rig.script().live_state.insert(1, "unknown".into());
        }
        let run = rig.go(&plan_on(&[1]));
        assert_eq!(run.step(0).status, Status::Stopped);
        assert_eq!(run.slept, Duration::from_secs(2));
        assert!(rig.puts().is_empty());
        assert!(state.reopen().state.pending.is_empty());
    }
}

#[test]
fn deploy_read_retries_three_server_failures_then_recovers() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().run_errors = [500, 502, 503].into();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 5);
    assert_eq!(run.slept, Duration::from_secs(4));
    assert!(run.phases.iter().any(|phase| phase.contains("retry 3/3")));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn deploy_read_stops_resumably_after_three_failed_retries() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().run_errors = [503, 503, 503, 503, 503].into();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(run.outcome.code, EXIT_STOPPED);
    assert_eq!(rig.polls(), 4);
    assert_eq!(run.slept, Duration::from_secs(3));
    assert!(run.said(0).contains("exhausted 3 consecutive retries"));
    assert_eq!(state.reopen().state.pending.len(), 1);
    rig.script().run_errors.clear();
    rig.store = state.reopen();
    let resumed = rig.go(&plan_on(&[]));
    assert_eq!(resumed.step(0).status, Status::Done);
    assert_eq!(rig.puts().len(), 1);
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn deploy_read_transport_timeout_is_retried_and_recovers() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.request_timeout = Duration::from_millis(100);
    rig.script()
        .run_delays
        .push_back(Duration::from_millis(250));
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                feed_registry: Some(rig.server.base.clone()),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Done, "{}", run.said(0));
    assert!(
        run.phases
            .iter()
            .any(|phase| phase.contains("transient workflow read error"))
    );
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn deploy_authentication_failure_is_not_retried() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().run_errors = [401].into();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(rig.polls(), 1);
    assert_eq!(run.slept, Duration::ZERO);
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn deploy_read_retry_sleep_is_capped_by_deadline() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.merge.poll_seconds = 15;
    rig.script().run_errors = [503, 503].into();
    let run = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_secs(2)),
                ..applied()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(rig.polls(), 1);
    assert_eq!(run.slept, Duration::from_secs(2));
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn api_budget_preview_warns_for_repo_timeouts_without_blocking_confirmation() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.merge.poll_seconds = 15;
    rig.cfg.merge.timeout_minutes = 30;
    rig.cfg
        .merge
        .repo_timeout_minutes
        .insert("acme/web".into(), 60);
    let plan = plan_of(
        vec![
            pr("acme/api", 1, "feat/1", "main"),
            pr("acme/web", 2, "feat/2", "main"),
        ],
        Vec::new(),
    );
    let run = rig.run(&plan, dry(), true, true).unwrap();
    let estimate = &run.outcome.report.api_estimate;
    assert_eq!(estimate.deploy_polls, 362);
    assert_eq!(estimate.checks, 6);
    assert_eq!(estimate.discovery, u64::from(plan.stats.requests) + 14);
    assert!(estimate.exceeds_budget);
    let text = render(&run.outcome.report, false);
    assert!(text.contains("raise api.max_requests"));
    let json = serde_json::to_value(&run.outcome.report).unwrap();
    assert_eq!(json["api_estimate"]["exceeds_budget"], true);
    assert_eq!(run.outcome.code, 0);
    assert!(rig.puts().is_empty());
    assert!(state.reopen().state.pending.is_empty());
    let confirmed = rig.run(&plan, applied(), false, false).unwrap();
    assert!(confirmed.outcome.aborted);
    assert!(confirmed.confirmation_text[0].contains("raise api.max_requests"));
    assert!(rig.puts().is_empty());
}

#[test]
fn api_budget_preview_honors_cli_timeout_and_excludes_dispensed_waits() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.merge.poll_seconds = 15;
    rig.cfg.api.max_requests = 1000;
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    let dispensed = rig.run(&plan_on(&[1]), dry(), true, true).unwrap();
    assert_eq!(dispensed.outcome.report.api_estimate.deploy_polls, 0);
    rig.script().workflows = vec![("deploy.yml".into(), "on: push".into())];
    let overridden = rig
        .run(
            &plan_on(&[1]),
            Settings {
                timeout: Some(Duration::from_secs(31)),
                ..dry()
            },
            true,
            true,
        )
        .unwrap();
    assert_eq!(overridden.outcome.report.api_estimate.deploy_polls, 4);
    assert!(!overridden.outcome.report.api_estimate.exceeds_budget);
    assert!(!render(&overridden.outcome.report, false).contains("raise api.max_requests"));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn parallel_deploy_worker_uses_the_injected_http_clock() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.cfg.max_parallel = 2;
    rig.script().http_elapsed.insert(
        "GET /repos/acme/api/actions/runs".into(),
        Duration::from_secs(61),
    );
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Stopped);
    assert_eq!(rig.polls(), 1);
    assert_eq!(state.reopen().state.pending.len(), 1);
}

#[test]
fn deploy_read_recovery_resets_retry_and_confirmation_counters() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    rig.script().run_errors = [503, 503, 503, 200, 503, 503, 503, 200].into();
    let run = rig.go(&plan_on(&[1]));
    assert_eq!(run.step(0).status, Status::Done);
    assert_eq!(rig.polls(), 9);
    assert_eq!(run.slept, Duration::from_secs(8));
    assert!(state.reopen().state.pending.is_empty());
}

#[test]
fn api_budget_saturates_large_waits_and_recomputes_dispensed_polls() {
    let mut rig = Rig::new("");
    let state = FilterState::attach(&mut rig);
    let settings = Settings {
        timeout: Some(Duration::MAX),
        ..dry()
    };
    let expected = rig.run(&plan_on(&[1, 2]), settings, true, true).unwrap();
    assert_eq!(expected.outcome.report.api_estimate.deploy_polls, u64::MAX);
    assert_eq!(expected.outcome.report.api_estimate.total, u64::MAX);
    rig.script().workflows = path_workflow("    paths: 'service/**'");
    let dispensed = rig
        .run(
            &plan_on(&[1, 2]),
            Settings {
                timeout: Some(Duration::MAX),
                ..dry()
            },
            true,
            true,
        )
        .unwrap();
    let estimate = dispensed.outcome.report.api_estimate;
    assert_eq!(estimate.deploy_polls, 0);
    assert_eq!(estimate.total, estimate.discovery + estimate.checks);
    assert!(state.reopen().state.pending.is_empty());
}
