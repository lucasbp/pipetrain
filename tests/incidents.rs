mod common;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Server, TempDir, pr, serve_requests};
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan, render};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::github::Client;
use pipetrain::merge::{DependencyFailure, Status, Store};
use pipetrain::model::Pr;
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use serde_json::{Value, json};

fn sha(number: u64) -> String {
    format!("{number:040x}")
}

fn merged_sha(number: u64) -> String {
    sha(0x1000 + number)
}

fn workflow_run(conclusion: &str) -> Value {
    json!({
        "name": "deploy", "path": ".github/workflows/deploy.yml",
        "status": "completed", "conclusion": conclusion,
        "html_url": "https://example.test/acme/api/actions/runs/1"
    })
}

#[derive(Default)]
struct Script {
    merged: BTreeSet<u64>,
    stack: bool,
    blocked: bool,
    filtered: bool,
    failed: bool,
    later: bool,
}

impl Script {
    fn base(&self, number: u64) -> &str {
        if self.stack && number == 2 {
            "feat/1"
        } else {
            "main"
        }
    }

    fn head(&self, number: u64) -> String {
        if self.stack && number == 1 && self.merged.contains(&2) {
            merged_sha(2)
        } else {
            sha(number)
        }
    }

    fn node(&self, number: u64, repo: &str) -> Value {
        json!({
            "number": number, "title": "change", "body": "",
            "url": format!("https://github.com/acme/{repo}/pull/{number}"),
            "headRefName": format!("feat/{number}"), "baseRefName": self.base(number),
            "headRefOid": self.head(number), "baseRef": {"target": {"oid": "b".repeat(40)}},
            "state": if self.merged.contains(&number) { "MERGED" } else { "OPEN" },
            "isDraft": false, "reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
            "mergeStateStatus": if self.blocked { "BLOCKED" } else { "CLEAN" },
            "repository": {"name": repo, "owner": {"login": "acme"}, "defaultBranchRef": {"name": "main"}},
            "latestOpinionatedReviews": {"nodes": [{"state": "APPROVED"}]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {"nodes": []}}}}]}
        })
    }

    fn respond(&mut self, line: &str, payload: &str) -> (u16, Value) {
        if line == "POST /graphql" {
            let body: Value = serde_json::from_str(payload).unwrap();
            if body.pointer("/variables/e0").is_some() {
                let text = if self.filtered {
                    "on:\n  push:\n    branches: main\n    paths: ['service/**']"
                } else {
                    "on: push"
                };
                return (200, json!({"data": {"repository": {"f0": {"text": text}}}}));
            }
            if body["query"].as_str().unwrap().contains("search(query:") {
                return (
                    200,
                    json!({"data": {"search": {"nodes": [], "issueCount": 0, "pageInfo": {"hasNextPage": false}}}}),
                );
            }
            if body.pointer("/variables/q0").is_some() {
                return (
                    200,
                    json!({"data": {"t0": {"ref": null}, "t1": {"ref": null}}}),
                );
            }
            let number = body["variables"]["n"]
                .as_u64()
                .unwrap_or_else(|| panic!("{body}"));
            let repo = body["variables"]["r"].as_str().unwrap();
            return (
                200,
                json!({"data": {"repository": {"pullRequest": self.node(number, repo)}}}),
            );
        }
        if line.starts_with("PUT ") {
            let number = line.split('/').nth(5).unwrap().parse().unwrap();
            self.merged.insert(number);
            return (200, json!({"merged": true, "sha": merged_sha(number)}));
        }
        if line.contains("/git/trees/") {
            return (
                200,
                json!({"truncated": false, "tree": [{"type": "blob", "path": ".github/workflows/deploy.yml"}]}),
            );
        }
        if line.contains("/files?") {
            return (
                200,
                json!([{"filename": "src/app.rs", "status": "modified"}]),
            );
        }
        if line.contains("/git/ref/heads/") {
            return (200, json!({"object": {"sha": "d".repeat(40)}}));
        }
        if line.contains("/compare/") {
            return (200, json!({"status": "ahead", "ahead_by": 1}));
        }
        if line.contains("/actions/runs?") {
            let conclusion = if self.failed
                && !(self.later && line.contains(&format!("head_sha={}", "d".repeat(40))))
            {
                "failure"
            } else {
                "success"
            };
            return (
                200,
                json!({"total_count": 1, "workflow_runs": [workflow_run(conclusion)]}),
            );
        }
        if let Some(number) = line
            .rsplit('/')
            .next()
            .and_then(|value| value.parse::<u64>().ok())
        {
            return (
                200,
                json!({
                    "state": if self.merged.contains(&number) { "closed" } else { "open" },
                    "merged": self.merged.contains(&number), "head": {"sha": self.head(number)},
                    "base": {"ref": self.base(number)}, "mergeable_state": "clean", "changed_files": 1,
                    "merge_commit_sha": merged_sha(number)
                }),
            );
        }
        (404, json!({"message": "unexpected request"}))
    }
}

struct Fixture {
    directory: TempDir,
    script: Arc<Mutex<Script>>,
    server: Server,
    cfg: Config,
}

fn unique_directory() -> TempDir {
    TempDir::new("incidents")
}

impl Fixture {
    fn new(script: Script) -> Self {
        let directory = unique_directory();
        let text = "orgs = ['acme']\n[api]\nmax_requests = 300\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\nstate_file = 'state.json'\npoll_seconds = 1\ntimeout_minutes = 1\nno_run_grace_seconds = 120\n";
        fs::write(directory.join("config.toml"), text).unwrap();
        fs::write(
            directory.join("state.json"),
            "{\"schema\":2,\"pending\":[]}",
        )
        .unwrap();
        let script = Arc::new(Mutex::new(script));
        let shared = script.clone();
        let server =
            serve_requests(move |line, payload| shared.lock().unwrap().respond(line, payload));
        Self {
            directory,
            script,
            server,
            cfg: Config::parse(text).unwrap(),
        }
    }

    fn store(&self) -> Store {
        Store::open(Some(self.directory.join("state.json"))).unwrap()
    }

    fn plan(&self, prs: Vec<Pr>) -> Plan {
        plan::build(
            &Rules::new(&self.cfg).unwrap(),
            Gathered {
                generated_at: "2026-11-13T12:00:00Z".into(),
                train: "deploy/13-11-2026".into(),
                deploy_branch: "deploy/13-11-2026".into(),
                train_prefix: "deploy/".into(),
                orgs: self.cfg.orgs.clone(),
                ready: prs.into_iter().map(|pr| (pr, "explicit".into())).collect(),
                skipped: Vec::new(),
                questions: Vec::new(),
                external: BTreeMap::new(),
                publishers: Publishers::new(),
                references: Default::default(),
                max_parallel: self.cfg.max_parallel,
                requests: 0,
                rate_remaining: None,
            },
        )
    }

    fn run(&self, plan: &Plan, apply: bool) -> (Outcome, Vec<String>, Duration) {
        let api = ApiConfig {
            max_requests: 300,
            max_per_minute: 10000,
            min_remaining: 0,
        };
        let mut client =
            Client::with_base_url("ghp_FAKE0123456789".into(), &api, false, &self.server.base);
        let mut store = self.store();
        let clock = Mutex::new(Instant::now());
        let slept = Cell::new(Duration::ZERO);
        let mut phases = Vec::new();
        let outcome = merge_plan(
            plan,
            &self.cfg,
            &mut client,
            &Settings {
                apply,
                gates_confirmed: false,
                allow_questions: false,
                method: None,
                timeout: None,
                feed_registry: None,
                open_pr: false,
                dependency_failure: DependencyFailure::Stop,
                bump: false,
                bump_options: Default::default(),
            },
            &mut store,
            &mut Hooks {
                confirm: &mut |_| true,
                gate: &mut |_| true,
                sleep: &mut |pause| {
                    *clock.lock().unwrap() += pause;
                    slept.set(slept.get() + pause);
                },
                now: &|| *clock.lock().unwrap(),
                observe: &mut |_| {},
                progress: &mut |phase| phases.push(phase.to_string()),
            },
        )
        .unwrap();
        (outcome, phases, slept.get())
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pipetrain"))
            .args(args)
            .args(["--config", "config.toml", "--no-cache", "--no-progress"])
            .current_dir(self.directory.path())
            .env("GH_TOKEN", "ghp_FAKE0123456789")
            .env_remove("GITHUB_TOKEN")
            .env("PIPETRAIN_API_URL", &self.server.base)
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

#[test]
fn fixture_directories_never_collide_across_threads() {
    let handles: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| (0..32).map(|_| unique_directory()).collect::<Vec<_>>()))
        .collect();
    let mut all = BTreeSet::new();
    let mut directories = Vec::new();
    let mut total = 0;
    for handle in handles {
        for directory in handle.join().unwrap() {
            total += 1;
            all.insert(directory.path().to_path_buf());
            directories.push(directory);
        }
    }
    assert_eq!(all.len(), total);
}

#[test]
fn incident_1_main_outside_paths_is_dispensed_without_grace_wait() {
    let fixture = Fixture::new(Script {
        filtered: true,
        ..Default::default()
    });
    let plan = fixture.plan(vec![pr("acme/api", 1, "feat/1", "main")]);
    let (outcome, _, slept) = fixture.run(&plan, true);
    assert_eq!(outcome.report.steps[0].status, Status::Done);
    assert_eq!(slept, Duration::ZERO);
    assert_eq!(
        serde_json::to_value(&outcome.report).unwrap()["steps"][0]["deploy_prediction"]["status"],
        "dispensed"
    );
    assert!(
        !fixture
            .server
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains("/actions/runs?"))
    );
    assert!(fixture.store().state.pending.is_empty());
}

#[test]
fn incident_2_parallel_prs_keep_both_active_phases_visible() {
    let mut fixture = Fixture::new(Script::default());
    fixture.cfg.max_parallel = 2;
    let plan = fixture.plan(vec![
        pr("acme/api", 1, "feat/1", "main"),
        pr("acme/ui", 2, "feat/2", "main"),
    ]);
    let (outcome, phases, _) = fixture.run(&plan, true);
    let frame = phases
        .iter()
        .find(|frame| {
            frame.contains("acme/api#1 → main | waiting for deploy:")
                && frame.contains("acme/ui#2 → main | waiting for deploy:")
        })
        .unwrap_or_else(|| panic!("missing concurrent phases: {phases:?}"));
    assert!(frame.starts_with("waiting: 0, running: 2"));
    assert_eq!(outcome.report.summary.merged, 2);
    assert_eq!(outcome.report.summary.deploy_confirmed, 2);
    assert!(fixture.store().state.pending.is_empty());
}

#[test]
fn incident_3_failed_deploy_is_resolved_by_later_descendant_deploy() {
    let fixture = Fixture::new(Script {
        failed: true,
        ..Default::default()
    });
    let (failed, _, _) = fixture.run(
        &fixture.plan(vec![pr("acme/api", 1, "feat/1", "main")]),
        true,
    );
    assert_eq!(failed.report.steps[0].status, Status::Failed);
    assert_eq!(fixture.store().state.pending.len(), 1);
    fixture.script.lock().unwrap().later = true;
    let (resumed, _, _) = fixture.run(&fixture.plan(Vec::new()), true);
    assert_eq!(resumed.code, 0);
    assert_eq!(resumed.report.steps[0].status, Status::Done);
    let resolution =
        serde_json::to_value(&resumed.report).unwrap()["steps"][0]["resolution"].clone();
    assert_eq!(resolution["status"], "resolved_by_later_deploy");
    assert_eq!(resolution["sha"], "d".repeat(40));
    assert!(
        resolution["runs"][0]["html_url"]
            .as_str()
            .unwrap()
            .contains("actions/runs/1")
    );
    let saved = fixture.store();
    assert!(saved.state.pending.is_empty());
    assert_eq!(
        serde_json::to_value(&saved.state.audit).unwrap()[0]["resolution"],
        resolution
    );
}

#[test]
fn incident_4_blocked_pr_never_appears_ready_in_plan() {
    let fixture = Fixture::new(Script {
        blocked: true,
        ..Default::default()
    });
    let before = fs::read(fixture.directory.join("state.json")).unwrap();
    let output = fixture.cli(&["plan", "acme/api#1", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["items"].as_array().unwrap().is_empty());
    assert_eq!(report["preflight"][0]["status"], "blocked");
    assert_eq!(report["preflight"][0]["merge_state_status"], "BLOCKED");
    assert!(
        !report["preflight"][0]["next_step"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read(fixture.directory.join("state.json")).unwrap(),
        before
    );
}

#[test]
fn incident_5_stacked_child_then_parent_merge_with_verified_new_head() {
    let mut fixture = Fixture::new(Script {
        stack: true,
        ..Default::default()
    });
    fixture.cfg.merge.no_deploy_repos = vec!["acme/api".into()];
    fixture.cfg.max_parallel = 1;
    let plan = fixture.plan(vec![
        pr("acme/api", 1, "feat/1", "main"),
        pr("acme/api", 2, "feat/2", "feat/1"),
    ]);
    let (outcome, _, _) = fixture.run(&plan, true);
    assert_eq!(outcome.code, 0, "{}", render(&outcome.report, false));
    assert!(
        outcome
            .report
            .steps
            .iter()
            .all(|step| step.status == Status::Done)
    );
    let seen = fixture.server.seen.lock().unwrap();
    let puts: Vec<_> = seen
        .iter()
        .filter(|request| request.starts_with("PUT "))
        .collect();
    assert_eq!(puts.len(), 2);
    assert!(puts[0].starts_with("PUT /repos/acme/api/pulls/2/merge "));
    assert!(puts[1].starts_with("PUT /repos/acme/api/pulls/1/merge "));
    let body: Value = serde_json::from_str(puts[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["sha"], merged_sha(2));
    assert!(seen.iter().any(|request| request.contains("/compare/")));
    assert!(fixture.store().state.pending.is_empty());
}

#[test]
fn incident_6_old_pending_shows_age_and_stale_and_lock_excludes_second_process() {
    let fixture = Fixture::new(Script::default());
    let recorded_at = (chrono::Utc::now() - chrono::Duration::hours(26)).to_rfc3339();
    let bytes = serde_json::to_vec(&json!({
        "schema": 2, "created_at": recorded_at, "updated_at": recorded_at,
        "queue_id": "old-queue", "pending": [{"id": "acme/api#1", "repo": "acme/api",
            "base": "main", "sha": merged_sha(1), "recorded_at": recorded_at}]
    }))
    .unwrap();
    fs::write(fixture.directory.join("state.json"), &bytes).unwrap();
    let (preview, _, _) = fixture.run(&fixture.plan(Vec::new()), false);
    assert!(preview.report.state.pending[0].stale);
    assert!(preview.report.state.pending[0].age_seconds.unwrap() >= 26 * 3600);
    let text = render(&preview.report, false);
    assert!(text.contains("age 26h"));
    assert!(text.contains("stale (warning only)"));
    let mut first = fixture.store();
    first.lock().unwrap();
    let output = fixture.cli(&["merge", "acme/api#2", "--apply", "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("state.json.lock"));
    assert!(
        fixture
            .server
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|request| !request.starts_with("PUT "))
    );
    assert_eq!(
        fs::read(fixture.directory.join("state.json")).unwrap(),
        bytes
    );
    drop(first);
    fixture.store().lock().unwrap();
}
