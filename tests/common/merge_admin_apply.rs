use std::fs;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use crate::common::{Server, TempDir, serve_requests};
use serde_json::{Value, json};

use super::node;

const MERGE_SHA: &str = "abcdef0123456789012345678901234567890123";
const NOTICE: &str =
    "--admin: these PRs will be merged past a missing review as octoadmin:\n  acme/api#1\n";

struct Script {
    planned: Value,
    live: Value,
    unsettled_graph: Option<Value>,
    after_rest: Option<Value>,
    live_protection: Option<Value>,
    live_rules: Value,
    live_repo_admin: bool,
    rest_reads: usize,
    protection_reads: usize,
    repo_reads: usize,
    rest_state: String,
    unsettled_rest: Option<String>,
    rest_head: String,
    rest_base: String,
    live_admin: bool,
    merge_status: u16,
    merge_attempted: bool,
    confirm_unreadable: bool,
    merged: bool,
    graph_reads: usize,
    admin_reads: usize,
    no_runs: bool,
    saved_while_waiting: Vec<Value>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            planned: node(1),
            live: node(1),
            unsettled_graph: None,
            after_rest: None,
            live_protection: None,
            live_rules: json!([]),
            live_repo_admin: true,
            rest_reads: 0,
            protection_reads: 0,
            repo_reads: 0,
            rest_state: "blocked".into(),
            unsettled_rest: None,
            rest_head: format!("{:040x}", 1),
            rest_base: "main".into(),
            live_admin: true,
            merge_status: 200,
            merge_attempted: false,
            confirm_unreadable: false,
            merged: false,
            graph_reads: 0,
            admin_reads: 0,
            no_runs: false,
            saved_while_waiting: Vec::new(),
        }
    }
}

struct Fixture {
    directory: TempDir,
    server: Server,
    script: Arc<Mutex<Script>>,
}

impl Fixture {
    fn new(script: Script, parallel: usize, wait: bool) -> Self {
        let directory = TempDir::new("merge-admin-apply");
        fs::write(directory.join("config.toml"), format!("orgs = ['acme']\nmax_parallel = {parallel}\nallow_missing_checks = true\n[api]\nmax_requests = 200\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\nstate_file = 'state.json'\npoll_seconds = 1\nno_run_grace_seconds = 0\nno_deploy_repos = {}\n", if wait { "[]" } else { "['acme/api']" })).unwrap();
        fs::write(
            directory.join("state.json"),
            "{\"schema\":2,\"pending\":[]}",
        )
        .unwrap();
        let path = directory.join("state.json");
        let script = Arc::new(Mutex::new(script));
        let shared = script.clone();
        let server = serve_requests(move |line, payload| {
            let mut script = shared.lock().unwrap();
            if line == "POST /graphql" {
                let body: Value = serde_json::from_str(payload).unwrap();
                let query = body["query"].as_str().unwrap();
                if query.contains("viewerCanMergeAsAdmin") {
                    script.admin_reads += 1;
                    let allowed = script.admin_reads == 1 || script.live_admin;
                    return (
                        200,
                        json!({"data": {"repository": {"pullRequest": {"viewerCanMergeAsAdmin": allowed}}}}),
                    );
                }
                if query.contains("viewer { login }") {
                    return (200, json!({"data": {"viewer": {"login": "octoadmin"}}}));
                }
                if query.contains("pullRequest(number:") {
                    script.graph_reads += 1;
                    let mut raw = if script.graph_reads == 1 {
                        script.planned.clone()
                    } else if script.rest_reads > 0 {
                        script
                            .after_rest
                            .clone()
                            .unwrap_or_else(|| script.live.clone())
                    } else if let Some(unsettled) = script.unsettled_graph.take() {
                        unsettled
                    } else {
                        script.live.clone()
                    };
                    if script.merged {
                        raw["state"] = json!("MERGED");
                    }
                    return (200, json!({"data": {"repository": {"pullRequest": raw}}}));
                }
                return (200, json!({"data": {}}));
            }
            if line == "GET /repos/acme/api" {
                script.repo_reads += 1;
                return (
                    200,
                    json!({"permissions": {"admin": script.repo_reads == 1 || script.live_repo_admin}}),
                );
            }
            if line.contains("/protection") {
                script.protection_reads += 1;
                if script.protection_reads > 1
                    && let Some(body) = &script.live_protection
                {
                    return (200, body.clone());
                }
                return (404, json!({"message": "Branch not protected"}));
            }
            if line.contains("/rules/branches/") {
                return (
                    200,
                    if script.protection_reads > 1 {
                        script.live_rules.clone()
                    } else {
                        json!([])
                    },
                );
            }
            if line.contains("/compare/") {
                return (200, json!({"behind_by": 1}));
            }
            if line.contains("/files?") {
                return (
                    200,
                    json!([{"filename": "src/app.rs", "status": "modified"}]),
                );
            }
            if line == "GET /repos/acme/api/pulls/1" {
                if script.merge_attempted && script.confirm_unreadable {
                    return (403, json!({"message": "Confirmation unavailable"}));
                }
                script.rest_reads += 1;
                let mergeable_state = script
                    .unsettled_rest
                    .take()
                    .unwrap_or_else(|| script.rest_state.clone());
                return (
                    200,
                    json!({
                        "state": if script.merged { "closed" } else { "open" },
                        "merged": script.merged, "merge_commit_sha": if script.merged { json!(MERGE_SHA) } else { Value::Null },
                        "head": {"sha": script.rest_head}, "base": {"ref": script.rest_base},
                        "mergeable_state": mergeable_state, "changed_files": 1
                    }),
                );
            }
            if line == "PUT /repos/acme/api/pulls/1/merge" {
                script.merge_attempted = true;
                if script.merge_status == 405 {
                    return (
                        405,
                        json!({"message": "At least 1 approving review is required by reviewers with write access."}),
                    );
                }
                script.merged = true;
                return (200, json!({"merged": true, "sha": MERGE_SHA}));
            }
            if line.contains("/actions/runs?") {
                script
                    .saved_while_waiting
                    .push(serde_json::from_slice(&fs::read(&path).unwrap()).unwrap());
                let runs = if script.no_runs {
                    json!([])
                } else {
                    json!([{
                        "id": 1, "workflow_id": 1, "name": "deploy", "path": ".github/workflows/deploy.yml",
                        "status": "completed", "conclusion": "success", "html_url": "https://example.test/run/1"
                    }])
                };
                return (
                    200,
                    json!({"total_count": runs.as_array().unwrap().len(), "workflow_runs": runs}),
                );
            }
            (404, json!({"message": "unexpected request"}))
        });
        Self {
            directory,
            server,
            script,
        }
    }

    fn command(&self) -> Command {
        let mut process = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
        process
            .current_dir(self.directory.path())
            .env("GH_TOKEN", "ghp_FAKE0123456789")
            .env_remove("GITHUB_TOKEN")
            .env("PIPETRAIN_API_URL", &self.server.base)
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .stdin(Stdio::null());
        process
    }

    fn run(&self, flags: &[&str]) -> Output {
        let mut process = self.command();
        process.args([
            "merge",
            "acme/api#1",
            "--config",
            "config.toml",
            "--no-cache",
            "--no-progress",
            "--apply",
            "--yes",
        ]);
        if !flags.contains(&"--json") {
            process.args(["--color", "never"]);
        }
        process.args(flags).output().unwrap()
    }

    fn report(&self, flags: &[&str], code: i32) -> Value {
        let mut args = vec!["--json"];
        args.extend(flags);
        let output = self.run(&args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.directory.join("state.json")).unwrap()).unwrap()
    }

    fn puts(&self) -> Vec<Value> {
        self.server
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("PUT "))
            .map(|request| serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap())
            .collect()
    }

    fn show(&self, json: bool) -> Output {
        let mut process = self.command();
        process.args(["state", "show", "--config", "config.toml"]);
        if json {
            process.arg("--json");
        }
        let output = process.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

fn approved() -> Value {
    let mut raw = node(1);
    raw["reviewDecision"] = json!("APPROVED");
    raw["mergeStateStatus"] = json!("CLEAN");
    raw
}

fn assert_override(value: &Value) {
    assert_eq!(value["reason"], "REVIEW_REQUIRED");
    assert_eq!(value["requested_by"], "octoadmin");
    let at = value["at"].as_str().unwrap();
    assert!(at.ends_with('Z'));
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .offset()
            .local_minus_utc(),
        0
    );
}

pub(super) fn review_only_apply() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(Script::default(), parallel, true);
        let report = fixture.report(&["--admin"], 0);
        assert_eq!(report["schema"], 1);
        assert_eq!(report["steps"][0]["status"], "done");
        assert_override(&report["steps"][0]["admin_override"]);
        assert_eq!(
            fixture.puts(),
            vec![json!({"sha": format!("{:040x}", 1), "merge_method": "merge"})]
        );
        let state = fixture.state();
        assert_eq!(state["schema"], 2);
        assert_eq!(state["pending"], json!([]));
        assert_eq!(state["admin_overrides"].as_array().unwrap().len(), 1);
        let record = &state["admin_overrides"][0];
        assert_override(record);
        assert_eq!(record["id"], "acme/api#1");
        assert_eq!(record["repo"], "acme/api");
        assert_eq!(record["base"], "main");
        assert_eq!(record["sha"], MERGE_SHA);
        assert_eq!(record["at"], report["steps"][0]["admin_override"]["at"]);
        let script = fixture.script.lock().unwrap();
        assert!(!script.saved_while_waiting.is_empty());
        for saved in &script.saved_while_waiting {
            assert_eq!(saved["pending"][0]["sha"], MERGE_SHA);
            assert_eq!(
                saved["pending"][0]["admin_override"],
                report["steps"][0]["admin_override"]
            );
            assert_eq!(saved["admin_overrides"], state["admin_overrides"]);
        }
        drop(script);
        let show: Value = serde_json::from_slice(&fixture.show(true).stdout).unwrap();
        assert_eq!(show["admin_overrides"], state["admin_overrides"]);
        let text = String::from_utf8(fixture.show(false).stdout).unwrap();
        assert!(text.contains(&format!("admin override: acme/api#1 merged past REVIEW_REQUIRED by octoadmin at {} (abcdef0)", record["at"].as_str().unwrap())), "{text}");
    }
}

#[test]
fn without_admin_apply_refuses_review_and_keeps_legacy_state_shape() {
    let fixture = Fixture::new(Script::default(), 1, false);
    let before = fs::read(fixture.directory.join("state.json")).unwrap();
    let report = fixture.report(&[], 0);
    assert_eq!(report["steps"], json!([]));
    assert!(fixture.puts().is_empty());
    assert_eq!(
        fs::read(fixture.directory.join("state.json")).unwrap(),
        before
    );
    let mut store =
        pipetrain::merge::Store::open(Some(fixture.directory.join("state.json"))).unwrap();
    store.save().unwrap();
    assert!(fixture.state().get("admin_overrides").is_none());
}

#[test]
fn admin_apply_refuses_checks_that_fail_after_planning() {
    for parallel in [1, 2] {
        let mut script = Script::default();
        script.live["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] =
            json!("FAILURE");
        let fixture = Fixture::new(script, parallel, false);
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("checks failing"),
            "{report}"
        );
        assert!(fixture.puts().is_empty());
        assert!(fixture.state().get("admin_overrides").is_none());
    }
}

#[test]
fn admin_apply_refuses_rest_dirty_unstable_and_other_states() {
    for parallel in [1, 2] {
        for state in [
            "dirty",
            "unstable",
            "behind",
            "draft",
            "has_hooks",
            "clean",
            "BLOCKED",
        ] {
            let fixture = Fixture::new(
                Script {
                    rest_state: state.into(),
                    ..Script::default()
                },
                parallel,
                false,
            );
            let report = fixture.report(&["--admin"], 1);
            assert_eq!(report["steps"][0]["status"], "failed", "{state}: {report}");
            assert!(fixture.puts().is_empty(), "{state}");
        }
    }
}

#[test]
fn admin_apply_now_approved_and_clean_merges_without_override() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                live: approved(),
                rest_state: "clean".into(),
                ..Script::default()
            },
            parallel,
            false,
        );
        let report = fixture.report(&["--admin"], 0);
        assert_eq!(report["steps"][0]["status"], "done");
        assert!(report["steps"][0].get("admin_override").is_none());
        assert!(fixture.state().get("admin_overrides").is_none());
        assert_eq!(fixture.puts().len(), 1);
        assert_eq!(fixture.script.lock().unwrap().admin_reads, 1);
    }
}

#[test]
fn admin_override_resume_retains_pending_record_and_single_history_entry() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                no_runs: true,
                ..Script::default()
            },
            parallel,
            true,
        );
        let first = fixture.report(&["--admin"], 5);
        assert_eq!(first["steps"][0]["status"], "stopped");
        let admin = first["steps"][0]["admin_override"].clone();
        assert_override(&admin);
        let state = fixture.state();
        assert_eq!(state["pending"][0]["admin_override"], admin);
        assert_eq!(state["admin_overrides"].as_array().unwrap().len(), 1);
        let preview = fixture
            .command()
            .args([
                "merge",
                "acme/api#1",
                "--config",
                "config.toml",
                "--no-cache",
                "--no-progress",
                "--color",
                "never",
            ])
            .output()
            .unwrap();
        assert!(preview.status.success());
        assert!(
            !String::from_utf8_lossy(&preview.stdout)
                .contains("would be merged past a missing review")
        );
        assert_eq!(fixture.state(), state);
        fixture.script.lock().unwrap().no_runs = false;
        let second = fixture.report(&[], 0);
        assert_eq!(second["steps"][0]["kind"], "resume");
        assert_eq!(second["steps"][0]["status"], "done");
        assert_eq!(second["steps"][0]["admin_override"], admin);
        assert_eq!(fixture.state()["admin_overrides"], state["admin_overrides"]);
        assert_eq!(fixture.state()["pending"], json!([]));
        assert_eq!(fixture.puts().len(), 1);
    }
}

#[test]
fn admin_apply_405_preserves_github_message_without_history() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                merge_status: 405,
                ..Script::default()
            },
            parallel,
            false,
        );
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"].as_str().unwrap().contains(
                "At least 1 approving review is required by reviewers with write access."
            )
        );
        assert_eq!(fixture.puts().len(), 1);
        assert!(fixture.state().get("admin_overrides").is_none());
    }
}

#[test]
fn admin_apply_requires_fresh_bypass_permission() {
    let fixture = Fixture::new(
        Script {
            live_admin: false,
            ..Script::default()
        },
        2,
        false,
    );
    let report = fixture.report(&["--admin"], 1);
    assert_eq!(report["steps"][0]["status"], "failed");
    assert!(
        report["steps"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("does not let")
    );
    assert!(fixture.puts().is_empty());
}

#[test]
fn admin_apply_never_grants_override_to_unmarked_step() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                planned: approved(),
                ..Script::default()
            },
            parallel,
            false,
        );
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("review required")
        );
        assert!(fixture.puts().is_empty());
        assert_eq!(fixture.script.lock().unwrap().admin_reads, 0);
    }
}

#[test]
fn admin_apply_preserves_head_and_base_checks() {
    for moved_head in [false, true] {
        let mut script = Script::default();
        if moved_head {
            script.rest_head = "c".repeat(40);
        } else {
            script.rest_base = "other".into();
        }
        let fixture = Fixture::new(script, 1, false);
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains(if moved_head {
                    "branch moved"
                } else {
                    "base changed"
                })
        );
        assert!(fixture.puts().is_empty());
    }
}

#[test]
fn admin_yes_text_announces_override_and_final_summary() {
    let fixture = Fixture::new(Script::default(), 1, false);
    let output = fixture.run(&["--admin"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(NOTICE));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("merged by admin override: acme/api#1 (REVIEW_REQUIRED)")
    );
    let fixture = Fixture::new(Script::default(), 1, false);
    let output = fixture.run(&["--admin", "--json"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(NOTICE));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(NOTICE));
    serde_json::from_slice::<Value>(&output.stdout).unwrap();
}

#[test]
fn admin_apply_settles_unknown_and_empty_rest_before_merging() {
    for parallel in [1, 2] {
        for unsettled in ["unknown", ""] {
            let fixture = Fixture::new(
                Script {
                    unsettled_rest: Some(unsettled.into()),
                    ..Script::default()
                },
                parallel,
                false,
            );
            let report = fixture.report(&["--admin"], 0);
            assert_eq!(report["steps"][0]["status"], "done");
            assert_override(&report["steps"][0]["admin_override"]);
            assert_eq!(fixture.puts().len(), 1);
            assert_eq!(
                fixture
                    .server
                    .seen
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| request.starts_with("GET /repos/acme/api/pulls/1 HTTP"))
                    .count(),
                2
            );
        }
    }
}

#[test]
fn unmarked_step_with_clean_graphql_still_refuses_rest_blocked() {
    let fixture = Fixture::new(
        Script {
            planned: approved(),
            live: approved(),
            ..Script::default()
        },
        2,
        false,
    );
    let report = fixture.report(&["--admin"], 1);
    assert_eq!(report["steps"][0]["status"], "failed");
    assert!(
        report["steps"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("branch protection blocks merge")
    );
    assert!(fixture.puts().is_empty());
}

#[test]
fn resume_confirms_unknown_sha_and_records_override_once() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                merged: true,
                ..Script::default()
            },
            parallel,
            true,
        );
        let admin = json!({"reason": "REVIEW_REQUIRED", "requested_by": "octoadmin"});
        fs::write(
            fixture.directory.join("state.json"),
            json!({
                "schema": 2,
                "pending": [{
                    "recorded_at": "2026-01-01T00:00:00Z", "id": "acme/api#1", "repo": "acme/api",
                    "base": "main", "sha": null, "admin_override": admin
                }]
            })
            .to_string(),
        )
        .unwrap();
        let report = fixture.report(&[], 0);
        assert_eq!(report["steps"][0]["kind"], "resume");
        assert_eq!(report["steps"][0]["status"], "done");
        assert_eq!(report["steps"][0]["merge_sha"], MERGE_SHA);
        assert_override(&report["steps"][0]["admin_override"]);
        let state = fixture.state();
        assert_eq!(state["admin_overrides"].as_array().unwrap().len(), 1);
        assert_eq!(
            state["admin_overrides"][0]["at"],
            report["steps"][0]["admin_override"]["at"]
        );
        assert!(fixture.puts().is_empty());
    }
}

#[test]
fn legacy_pending_round_trip_omits_all_new_fields() {
    let fixture = Fixture::new(Script::default(), 1, false);
    let original = json!({
        "schema": 2, "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z",
        "profile": "default", "queue_id": "legacy-queue", "deploy_prs": [], "audit": [],
        "pending": [{"recorded_at": null, "id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": MERGE_SHA}]
    });
    let path = fixture.directory.join("state.json");
    fs::write(&path, original.to_string()).unwrap();
    let mut store = pipetrain::merge::Store::open(Some(path)).unwrap();
    assert!(store.state.pending[0].admin_override.is_none());
    assert!(store.state.admin_overrides.is_empty());
    store.save().unwrap();
    let mut saved = fixture.state();
    saved["updated_at"] = original["updated_at"].clone();
    assert_eq!(saved, original);
}

#[test]
fn admin_apply_rechecks_fresh_repository_permission_and_protection() {
    for parallel in [1, 2] {
        for revoke_permission in [false, true] {
            let script = Script {
                live_repo_admin: !revoke_permission,
                live_protection: (!revoke_permission)
                    .then(|| json!({"required_signatures": {"enabled": true}})),
                ..Script::default()
            };
            let fixture = Fixture::new(script, parallel, false);
            let report = fixture.report(&["--admin"], 1);
            assert_eq!(report["steps"][0]["status"], "failed", "{report}");
            assert!(
                report["steps"][0].get("admin_override").is_none(),
                "{report}"
            );
            let refusal = report["steps"][0]["detail"].as_str().unwrap();
            assert!(
                refusal.contains(if revoke_permission {
                    "admin permission"
                } else {
                    "required_signatures"
                }),
                "{refusal}"
            );
            assert!(fixture.puts().is_empty());
            assert!(fixture.state().get("admin_overrides").is_none());
        }
    }
}

#[test]
fn admin_apply_rechecks_graphql_after_rest_settles() {
    for parallel in [1, 2] {
        for changes_requested in [false, true] {
            let mut after_rest = node(1);
            if changes_requested {
                after_rest["reviewDecision"] = json!("CHANGES_REQUESTED");
            } else {
                after_rest["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] =
                    json!("FAILURE");
            }
            let fixture = Fixture::new(
                Script {
                    unsettled_rest: Some("unknown".into()),
                    after_rest: Some(after_rest),
                    ..Script::default()
                },
                parallel,
                false,
            );
            let report = fixture.report(&["--admin"], 1);
            assert_eq!(report["steps"][0]["status"], "failed", "{report}");
            assert!(
                report["steps"][0].get("admin_override").is_none(),
                "{report}"
            );
            assert!(
                report["steps"][0]["detail"]
                    .as_str()
                    .unwrap()
                    .contains(if changes_requested {
                        "changes requested"
                    } else {
                        "checks failing"
                    }),
                "{report}"
            );
            assert_eq!(fixture.script.lock().unwrap().rest_reads, 2);
            assert!(fixture.puts().is_empty());
        }
    }
}

#[test]
fn admin_apply_waits_for_unknown_graphql_before_review_override() {
    for parallel in [1, 2] {
        for unknown_state in [false, true] {
            let mut unknown = node(1);
            unknown["mergeable"] = json!("UNKNOWN");
            if unknown_state {
                unknown["mergeStateStatus"] = json!("UNKNOWN");
            }
            let fixture = Fixture::new(
                Script {
                    unsettled_graph: Some(unknown),
                    ..Script::default()
                },
                parallel,
                false,
            );
            let report = fixture.report(&["--admin"], 0);
            assert_eq!(report["steps"][0]["status"], "done", "{report}");
            assert_override(&report["steps"][0]["admin_override"]);
            assert_eq!(fixture.script.lock().unwrap().graph_reads, 4);
            assert_eq!(fixture.puts().len(), 1);
        }
    }
}

#[test]
fn admin_apply_requires_fresh_graphql_head_and_base_to_match_rest() {
    for moved_head in [false, true] {
        let mut fresh = node(1);
        if moved_head {
            fresh["headRefOid"] = json!("d".repeat(40));
        } else {
            fresh["baseRefName"] = json!("other");
        }
        let fixture = Fixture::new(
            Script {
                after_rest: Some(fresh),
                ..Script::default()
            },
            1,
            false,
        );
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed", "{report}");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains(if moved_head {
                    "branch moved between REST and GraphQL"
                } else {
                    "base changed between REST and GraphQL"
                }),
            "{report}"
        );
        assert!(fixture.puts().is_empty());
    }
}

#[test]
fn admin_apply_rechecks_new_check_strict_method_and_thread_requirements() {
    for (protection, rules, name) in [
        (
            Some(json!({"required_status_checks": {"contexts": ["new-ci"]}})),
            json!([]),
            "new-ci",
        ),
        (
            Some(json!({"required_status_checks": {"strict": true, "contexts": []}})),
            json!([]),
            "is behind main",
        ),
        (
            None,
            json!([{"type": "pull_request", "parameters": {"required_review_thread_resolution": true}}]),
            "required_review_thread_resolution",
        ),
        (
            None,
            json!([{"type": "pull_request", "parameters": {"allowed_merge_methods": ["squash"]}}]),
            "allowed_merge_methods",
        ),
        (
            None,
            json!([{"type": "required_linear_history"}]),
            "required_linear_history",
        ),
    ] {
        let fixture = Fixture::new(
            Script {
                live_protection: protection,
                live_rules: rules,
                ..Script::default()
            },
            1,
            false,
        );
        let report = fixture.report(&["--admin"], 1);
        assert_eq!(report["steps"][0]["status"], "failed", "{report}");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains(name),
            "{report}"
        );
        assert!(fixture.puts().is_empty());
    }
}

#[test]
fn admin_stopped_override_without_confirmed_merge_omits_report_marker() {
    for parallel in [1, 2] {
        let fixture = Fixture::new(
            Script {
                merge_status: 405,
                confirm_unreadable: true,
                ..Script::default()
            },
            parallel,
            false,
        );
        let report = fixture.report(&["--admin"], 5);
        assert_eq!(report["steps"][0]["status"], "stopped", "{report}");
        assert!(report["steps"][0].get("merge_sha").is_none(), "{report}");
        assert!(
            report["steps"][0].get("admin_override").is_none(),
            "{report}"
        );
        assert!(
            report["steps"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("could not confirm whether it merged"),
            "{report}"
        );
        assert_eq!(fixture.puts().len(), 1);
        assert!(fixture.state().get("admin_overrides").is_none());
        assert_eq!(
            fixture.state()["pending"][0]["admin_override"]["reason"],
            "REVIEW_REQUIRED"
        );
    }
}
