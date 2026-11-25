mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Server, TempDir, pr, serve_requests};
use pipetrain::command::merge::compact::{footer_counts, preview};
use pipetrain::command::merge::{Hooks, Outcome, Settings, merge_plan};
use pipetrain::config::{ApiConfig, Config, Rules};
use pipetrain::discovery::{Scope, Skipped};
use pipetrain::github::Client;
use pipetrain::merge::category::{Category, classify};
use pipetrain::merge::{DependencyFailure, Kind, Status, Store};
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

#[derive(Default)]
struct Script {
    failed: bool,
    tip_is_merged_commit: bool,
    blocked: bool,
}

impl Script {
    fn node(&self, number: u64, repo: &str) -> Value {
        json!({
            "number": number, "title": "change", "body": "",
            "url": format!("https://github.com/acme/{repo}/pull/{number}"),
            "headRefName": format!("feat/{number}"), "baseRefName": "main",
            "headRefOid": sha(number), "baseRef": {"target": {"oid": "b".repeat(40)}},
            "state": "OPEN",
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
                return (
                    200,
                    json!({"data": {"repository": {"f0": {"text": "on: push"}}}}),
                );
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
            let tip = if self.tip_is_merged_commit {
                merged_sha(1)
            } else {
                "d".repeat(40)
            };
            return (200, json!({"object": {"sha": tip}}));
        }
        if line.contains("/compare/") {
            return (200, json!({"status": "ahead", "ahead_by": 1}));
        }
        if line.contains("/actions/runs?") {
            let conclusion = if self.failed { "failure" } else { "success" };
            return (
                200,
                json!({"total_count": 1, "workflow_runs": [{
                    "name": "deploy", "path": ".github/workflows/deploy.yml",
                    "status": "completed", "conclusion": conclusion,
                    "html_url": "https://example.test/acme/api/actions/runs/1"
                }]}),
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
                    "state": "open", "merged": false, "head": {"sha": sha(number)},
                    "base": {"ref": "main"}, "mergeable_state": "clean", "changed_files": 1,
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

impl Fixture {
    fn new(script: Script) -> Self {
        let directory = TempDir::new("waiting-vs-blocked");
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

    fn plan(&self, ready: Vec<Pr>, skipped: Vec<(Pr, Vec<&str>)>) -> Plan {
        build(&self.cfg, ready, skipped)
    }

    fn run(&self, plan: &Plan, apply: bool) -> Outcome {
        let api = ApiConfig {
            max_requests: 300,
            max_per_minute: 10000,
            min_remaining: 0,
            max_per_second: 1000,
            ..ApiConfig::default()
        };
        let mut client =
            Client::with_base_url("ghp_FAKE0123456789".into(), &api, false, &self.server.base);
        let mut store = self.store();
        let clock = Mutex::new(Instant::now());
        let slept = Cell::new(Duration::ZERO);
        merge_plan(
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
                bump_behind: None,
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
                progress: &mut |_| {},
            },
        )
        .unwrap()
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

fn build(cfg: &Config, ready: Vec<Pr>, skipped: Vec<(Pr, Vec<&str>)>) -> Plan {
    plan::build(
        &Rules::new(cfg).unwrap(),
        Gathered {
            generated_at: "2026-11-18T12:00:00Z".into(),
            train: "deploy/18-11-2026".into(),
            deploy_branch: "deploy/18-11-2026".into(),
            train_prefix: "deploy/".into(),
            orgs: cfg.orgs.clone(),
            ready: ready
                .into_iter()
                .map(|pr| (pr, "explicit".into()))
                .collect(),
            skipped: skipped
                .into_iter()
                .map(|(pr, reasons)| Skipped {
                    pr,
                    scope: Scope::Explicit,
                    reasons: reasons.into_iter().map(str::to_string).collect(),
                })
                .collect(),
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

fn failed_first_run() -> Fixture {
    let fixture = Fixture::new(Script {
        failed: true,
        ..Default::default()
    });
    let outcome = fixture.run(
        &fixture.plan(vec![pr("acme/api", 1, "feat/1", "main")], Vec::new()),
        true,
    );
    assert_eq!(outcome.report.steps[0].status, Status::Failed);
    assert_eq!(fixture.store().state.pending.len(), 1);
    fixture
}

fn left_out(plan: &Plan, id: &str) -> Vec<String> {
    plan.skipped
        .iter()
        .find(|skipped| skipped.id == id)
        .unwrap_or_else(|| panic!("{id} was not left out: {:?}", plan.skipped))
        .reasons
        .clone()
}

#[test]
fn resume_with_an_unchanged_failed_run_is_waiting_and_not_queued() {
    let fixture = failed_first_run();
    fixture.script.lock().unwrap().tip_is_merged_commit = true;
    let outcome = fixture.run(&fixture.plan(Vec::new(), Vec::new()), false);
    assert_eq!(outcome.code, 0);
    assert!(
        outcome
            .report
            .steps
            .iter()
            .all(|step| step.kind != Kind::Resume),
        "{:?}",
        outcome.report.steps
    );
    let value = serde_json::to_value(&outcome.report).unwrap();
    let held = &value["not_merging"][0];
    assert_eq!(held["id"], "acme/api#1");
    assert_eq!(held["state"], "waiting");
    let cause = held["reasons"][0].as_str().unwrap();
    assert!(cause.starts_with("deploy failed at "), "{cause}");
    assert!(cause.ends_with("needs a new commit"), "{cause}");
    assert_eq!(fixture.store().state.pending.len(), 1);
}

#[test]
fn resume_with_a_later_deploy_containing_the_saved_sha_stays_queued() {
    let fixture = failed_first_run();
    let outcome = fixture.run(&fixture.plan(Vec::new(), Vec::new()), false);
    let resume: Vec<_> = outcome
        .report
        .steps
        .iter()
        .filter(|step| step.kind == Kind::Resume)
        .collect();
    assert_eq!(resume.len(), 1);
    assert!(outcome.report.not_merging.is_empty());
}

#[test]
fn every_existing_cause_maps_to_a_category() {
    let waiting: &[&[&str]] = &[
        &["review required"],
        &["not approved"],
        &["changes requested"],
        &["checks pending"],
        &["draft"],
        &["draft (DRAFT)"],
        &[
            "review required",
            "branch protection blocks merge (BLOCKED)",
        ],
        &["depends on acme/api#1, which is blocked"],
        &["deploy failed at workflow deploy (failure), needs a new commit"],
    ];
    let blocked: &[&[&str]] = &[
        &["checks failing"],
        &["checks failing: build"],
        &["merge conflict"],
        &["merge conflict (DIRTY)"],
        &["branch behind its base, update it"],
        &["no checks reported"],
        &["checks are not passing (UNSTABLE)"],
        &["branch protection blocks merge (BLOCKED)"],
        &["already merged"],
        &["closed"],
        &["already in main"],
        &["review required", "merge conflict"],
        &["something nobody has seen before"],
        &[],
    ];
    for reasons in waiting {
        assert_eq!(classify(&owned(reasons)), Category::Waiting, "{reasons:?}");
    }
    for reasons in blocked {
        assert_eq!(classify(&owned(reasons)), Category::Blocked, "{reasons:?}");
    }
}

fn owned(reasons: &[&str]) -> Vec<String> {
    reasons.iter().map(|reason| (*reason).to_string()).collect()
}

#[test]
fn json_gains_state_and_keeps_schema_one() {
    let fixture = Fixture::new(Script::default());
    let plan = fixture.plan(
        vec![pr("acme/api", 1, "feat/1", "main")],
        vec![
            (pr("acme/api", 2, "feat/2", "main"), vec!["review required"]),
            (pr("acme/web", 3, "feat/3", "main"), vec!["merge conflict"]),
        ],
    );
    let outcome = fixture.run(&plan, false);
    let value = serde_json::to_value(&outcome.report).unwrap();
    assert_eq!(value["schema"], 1);
    let states: BTreeMap<String, String> = value["not_merging"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["id"].as_str().unwrap().to_string(),
                entry["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(states["acme/api#2"], "waiting");
    assert_eq!(states["acme/web#3"], "blocked");
    assert!(value["not_merging"][0].get("reasons").is_some());
    assert!(value["not_merging"][0].get("id").is_some());
}

#[test]
fn apply_exits_zero_when_only_waiting_and_blocked_prs_are_left_out() {
    let fixture = Fixture::new(Script::default());
    let plan = fixture.plan(
        vec![pr("acme/api", 1, "feat/1", "main")],
        vec![
            (pr("acme/api", 2, "feat/2", "main"), vec!["draft"]),
            (pr("acme/web", 3, "feat/3", "main"), vec!["merge conflict"]),
        ],
    );
    let outcome = fixture.run(&plan, true);
    assert_eq!(outcome.code, 0);
    assert_eq!(outcome.report.count_state(Category::Waiting), 1);
    assert_eq!(outcome.report.count_state(Category::Blocked), 1);
}

#[test]
fn apply_exits_non_zero_only_for_the_pr_that_really_failed() {
    let fixture = Fixture::new(Script {
        failed: true,
        ..Default::default()
    });
    let plan = fixture.plan(
        vec![pr("acme/api", 1, "feat/1", "main")],
        vec![
            (pr("acme/api", 2, "feat/2", "main"), vec!["draft"]),
            (pr("acme/web", 3, "feat/3", "main"), vec!["merge conflict"]),
        ],
    );
    let outcome = fixture.run(&plan, true);
    assert_ne!(outcome.code, 0);
    assert_eq!(outcome.report.steps[0].status, Status::Failed);
    assert_eq!(outcome.report.count_state(Category::Waiting), 1);
    assert_eq!(outcome.report.count_state(Category::Blocked), 1);
}

#[test]
fn strict_still_exits_four_when_nothing_is_eligible_and_default_stays_zero() {
    let fixture = Fixture::new(Script {
        blocked: true,
        ..Default::default()
    });
    assert_eq!(fixture.cli(&["merge", "acme/api#1"]).status.code(), Some(0));
    assert_eq!(
        fixture
            .cli(&["merge", "acme/api#1", "--strict"])
            .status
            .code(),
        Some(4)
    );
}

#[test]
fn compact_footer_counts_each_category_and_colors_follow_it() {
    let fixture = Fixture::new(Script::default());
    let plan = fixture.plan(
        vec![pr("acme/api", 1, "feat/1", "main")],
        vec![
            (pr("acme/api", 2, "feat/2", "main"), vec!["review required"]),
            (pr("acme/web", 3, "feat/3", "main"), vec!["merge conflict"]),
            (pr("acme/web", 4, "feat/4", "main"), vec!["checks failing"]),
        ],
    );
    let outcome = fixture.run(&plan, false);
    let plain = preview(&outcome.report, &plan, false);
    assert!(!plain.contains('\u{1b}'), "{plain}");
    assert!(
        plain.contains("1 enter the queue · 1 waiting · 2 blocked"),
        "{plain}"
    );
    let waiting = plain.lines().find(|line| line.contains("api#2")).unwrap();
    let blocked = plain.lines().find(|line| line.contains("web#3")).unwrap();
    assert!(waiting.contains("waiting"), "{waiting}");
    assert!(blocked.contains("blocked"), "{blocked}");
    let colored = preview(&outcome.report, &plan, true);
    let waiting = colored.lines().find(|line| line.contains("api#2")).unwrap();
    let blocked = colored.lines().find(|line| line.contains("web#3")).unwrap();
    assert!(waiting.contains("33mwaiting"), "{waiting:?}");
    assert!(blocked.contains("31mblocked"), "{blocked:?}");
    let counts = anstream::adapter::strip_str(&footer_counts(&outcome.report, true)).to_string();
    assert!(counts.contains("1 waiting · 2 blocked"), "{counts}");
}

#[test]
fn no_color_and_non_tty_output_carry_no_escape_codes() {
    let fixture = Fixture::new(Script {
        blocked: true,
        ..Default::default()
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
    let output = command
        .args(["merge", "acme/api#1", "--config", "config.toml"])
        .args(["--no-cache", "--no-progress"])
        .current_dir(fixture.directory.path())
        .env("GH_TOKEN", "ghp_FAKE0123456789")
        .env_remove("GITHUB_TOKEN")
        .env("PIPETRAIN_API_URL", &fixture.server.base)
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .env("PIPETRAIN_LOG", "0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("blocked"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
}

#[test]
fn a_dependent_of_a_blocked_pr_is_left_out_transitively_as_waiting() {
    let cfg = Config::parse("orgs = ['acme']\n").unwrap();
    let mut second = pr("acme/api", 2, "feat/2", "main");
    second.body = "Depends-On: acme/api#1".into();
    let mut third = pr("acme/web", 3, "feat/3", "main");
    third.body = "Depends-On: acme/api#2".into();
    let free = pr("acme/docs", 4, "feat/4", "main");
    let plan = build(
        &cfg,
        vec![second, third, free],
        vec![(pr("acme/api", 1, "feat/1", "main"), vec!["merge conflict"])],
    );
    let second = left_out(&plan, "acme/api#2");
    assert_eq!(second[0], "depends on acme/api#1, which is blocked");
    assert!(second[1].contains("merge conflict"), "{second:?}");
    let third = left_out(&plan, "acme/web#3");
    assert_eq!(third[0], "depends on acme/api#2, which is waiting");
    assert_eq!(
        plan.items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec!["acme/docs#4"]
    );
}

#[test]
fn a_dependent_of_a_waiting_pr_waits_and_the_cause_shows_in_verbose() {
    let fixture = Fixture::new(Script::default());
    let mut second = pr("acme/api", 2, "feat/2", "main");
    second.body = "Depends-On: acme/api#1".into();
    let plan = fixture.plan(
        vec![second],
        vec![(pr("acme/api", 1, "feat/1", "main"), vec!["review required"])],
    );
    let held = left_out(&plan, "acme/api#2");
    assert_eq!(held[0], "depends on acme/api#1, which is waiting");
    let outcome = fixture.run(&plan, false);
    let value = serde_json::to_value(&outcome.report).unwrap();
    let states: Vec<&str> = value["not_merging"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["waiting", "waiting"]);
    assert_eq!(outcome.report.count_state(Category::Waiting), 2);
}

#[test]
fn unprovable_or_ordering_only_edges_do_not_drop_the_pr() {
    let cfg = Config::parse("orgs = ['acme']\n").unwrap();
    let sibling = pr("acme/api", 2, "feat/2", "main");
    let plan = build(
        &cfg,
        vec![sibling],
        vec![(pr("acme/api", 1, "feat/1", "main"), vec!["merge conflict"])],
    );
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].id, "acme/api#2");
}

#[test]
fn finished_work_is_never_a_parent() {
    let cfg = Config::parse("orgs = ['acme']\n").unwrap();
    let mut second = pr("acme/api", 2, "feat/2", "main");
    second.body = "Depends-On: acme/api#1".into();
    let plan = build(
        &cfg,
        vec![second],
        vec![(pr("acme/api", 1, "feat/1", "main"), vec!["already merged"])],
    );
    assert_eq!(plan.items.len(), 1);
}
