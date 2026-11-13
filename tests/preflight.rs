mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use common::{Server, serve_requests};
use serde_json::{Value, json};

fn node(number: u64, state: &str, base: &str) -> Value {
    json!({
        "number": number, "title": "change", "body": "",
        "url": format!("https://github.com/acme/api/pull/{number}"),
        "headRefName": format!("feature/{number}"), "baseRefName": base,
        "headRefOid": format!("{number:040x}"), "baseRef": {"target": {"oid": "b".repeat(40)}},
        "isDraft": false, "state": "OPEN", "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE", "mergeStateStatus": state,
        "repository": {"name": "api", "owner": {"login": "acme"}, "defaultBranchRef": {"name": "main"}},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {"nodes": []}}}}]}
    })
}

struct Fixture {
    directory: PathBuf,
    server: Server,
}

impl Fixture {
    fn new(nodes: Vec<Value>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "pipetrain-preflight-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("config.toml"), "orgs = [\"acme\"]\n[api]\nmax_requests = 200\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\nstate_file = \"state.json\"\nno_deploy_repos = [\"acme/api\"]\n").unwrap();
        fs::write(
            directory.join("state.json"),
            "{\"schema\":2,\"pending\":[]}",
        )
        .unwrap();
        let server = serve_requests(move |line, payload| {
            if line == "POST /graphql" {
                let body: Value = serde_json::from_str(payload).unwrap();
                let query = body["query"].as_str().unwrap();
                if query.contains("pullRequest(number:") {
                    let number = body["variables"]["n"].as_u64().unwrap();
                    return (
                        200,
                        json!({"data": {"repository": {"pullRequest": nodes.iter().find(|node| node["number"] == number)}}}),
                    );
                }
                if query.contains("search(query:") {
                    return (
                        200,
                        json!({"data": {"search": {"nodes": nodes, "pageInfo": {"hasNextPage": false}, "issueCount": nodes.len()}}}),
                    );
                }
                return (200, json!({"data": {}}));
            }
            if line.starts_with("GET ") && line.contains("/files?") {
                return (
                    200,
                    json!([{ "filename": "src/app.rs", "status": "modified" }]),
                );
            }
            if line.starts_with("GET ") && line.contains("/git/trees/") {
                return (200, json!({"tree": [], "truncated": false}));
            }
            (404, json!({"message": "unexpected request"}))
        });
        Self { directory, server }
    }

    fn run(&self, command: &str, ids: &[u64], extra: &[&str]) -> Output {
        let mut process = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
        process.arg(command);
        for id in ids {
            process.arg(format!("acme/api#{id}"));
        }
        if !extra.contains(&"--json") {
            process.args(["--color", "never"]);
        }
        let output = process
            .args(["--config", "config.toml", "--no-cache", "--no-progress"])
            .args(extra)
            .current_dir(&self.directory)
            .env("GH_TOKEN", "ghp_FAKE0123456789")
            .env_remove("GITHUB_TOKEN")
            .env("PIPETRAIN_API_URL", &self.server.base)
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(self.server.seen.lock().unwrap().iter().all(|request| request.starts_with("GET ") || request.starts_with("POST /graphql ")));
        output
    }

    fn json(&self, command: &str, ids: &[u64], extra: &[&str]) -> Value {
        let mut flags = vec!["--json"];
        flags.extend(extra);
        let output = self.run(command, ids, &flags);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn blocked_dirty_unstable_draft_and_behind_are_explained_and_excluded_from_queue() {
    for state in ["BLOCKED", "DIRTY", "UNSTABLE", "DRAFT", "BEHIND"] {
        let fixture = Fixture::new(vec![node(1, state, "main")]);
        let report = fixture.json("plan", &[1], &[]);
        assert!(report["items"].as_array().unwrap().is_empty());
        let preflight = &report["preflight"][0];
        assert_eq!(preflight["status"], "blocked");
        assert_eq!(preflight["merge_state_status"], state);
        assert!(!preflight["causes"].as_array().unwrap().is_empty());
        assert!(!preflight["next_step"].as_str().unwrap().is_empty());
        let queue = fixture.run("plan", &[1], &["--emit", "queue"]);
        let text = String::from_utf8(queue.stdout).unwrap();
        assert!(text.lines().all(|line| line.starts_with('#')), "{text}");
        assert!(text.contains("blocked") && text.contains("next:"), "{text}");
        let merge = fixture.json("merge", &[1], &[]);
        assert_eq!(merge["preflight"], report["preflight"]);
        assert!(merge["steps"].as_array().unwrap().is_empty());
    }
}

#[test]
fn unknown_warns_without_blocking_and_clean_and_hooks_pass_preflight() {
    for state in ["UNKNOWN", "CLEAN", "HAS_HOOKS"] {
        let fixture = Fixture::new(vec![node(1, state, "main")]);
        for command in ["plan", "merge"] {
            let report = fixture.json(command, &[1], &["--strict"]);
            let preflight = &report["preflight"][0];
            assert_eq!(preflight["status"], "eligible");
            let warnings = preflight["warnings"].to_string();
            assert_eq!(
                warnings.contains("GitHub still computing"),
                state == "UNKNOWN"
            );
        }
    }
}

#[test]
fn feature_base_warns_without_a_selected_parent_and_train_base_does_not() {
    let fixture = Fixture::new(vec![
        node(1, "CLEAN", "feature/2"),
        node(2, "CLEAN", "main"),
        node(3, "CLEAN", "deploy/old"),
    ]);
    for command in ["plan", "merge"] {
        let orphan = fixture.json(command, &[1], &[]);
        assert_eq!(orphan["preflight"][0]["base"], "feature/2");
        assert_eq!(orphan["preflight"][0]["default_branch"], "main");
        assert!(
            orphan["preflight"][0]["warnings"]
                .to_string()
                .contains("base not landed")
        );
        let stacked = fixture.json(command, &[1, 2, 3], &[]);
        assert!(!stacked["preflight"].to_string().contains("base not landed"));
        assert_eq!(stacked["preflight"][0]["status"], "eligible");
        assert_eq!(stacked["preflight"][1]["status"], "waiting");
        assert!(
            stacked["preflight"][1]["causes"]
                .to_string()
                .contains("acme/api#1")
        );
    }
}

#[test]
fn failed_checks_keep_names_states_and_both_kinds_of_urls_in_reports_and_text() {
    let mut raw = node(1, "UNSTABLE", "main");
    raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = json!({"state": "FAILURE", "contexts": {"nodes": [
        {"__typename": "CheckRun", "name": "package-version", "conclusion": "FAILURE", "detailsUrl": "https://example.test/acme/api/check/1"},
        {"__typename": "StatusContext", "context": "ci/build", "state": "ERROR", "targetUrl": "https://example.test/acme/api/status/2"}
    ]}});
    let fixture = Fixture::new(vec![raw]);
    for command in ["plan", "merge"] {
        let report = fixture.json(command, &[1], &[]);
        let checks = &report["preflight"][0]["checks"];
        assert_eq!(
            checks[0],
            json!({"name": "ci/build", "state": "ERROR", "url": "https://example.test/acme/api/status/2"})
        );
        assert_eq!(
            checks[1],
            json!({"name": "package-version", "state": "FAILURE", "url": "https://example.test/acme/api/check/1"})
        );
        let output = fixture.run(command, &[1], &[]);
        let text = String::from_utf8(output.stdout).unwrap();
        for value in [
            "ci/build",
            "ERROR",
            "package-version",
            "FAILURE",
            "https://example.test/acme/api/check/1",
            "https://example.test/acme/api/status/2",
        ] {
            assert!(text.contains(value), "{text}");
        }
    }
}

#[test]
fn strict_returns_four_for_fully_blocked_plan_and_merge_dry_run_only_when_requested() {
    let fixture = Fixture::new(vec![node(1, "BLOCKED", "main")]);
    for command in ["plan", "merge"] {
        for json in [false, true] {
            for strict in [false, true] {
                let mut flags = Vec::new();
                if json {
                    flags.push("--json");
                }
                if strict {
                    flags.push("--strict");
                }
                let output = fixture.run(command, &[1], &flags);
                assert_eq!(
                    output.status.code(),
                    Some(if strict { 4 } else { 0 }),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
    for strict in [false, true] {
        let flags = if strict {
            vec!["--emit", "queue", "--strict"]
        } else {
            vec!["--emit", "queue"]
        };
        assert_eq!(
            fixture.run("plan", &[1], &flags).status.code(),
            Some(if strict { 4 } else { 0 })
        );
    }
}

#[test]
fn search_and_direct_queries_request_default_branch_and_check_links() {
    let fixture = Fixture::new(vec![node(1, "BLOCKED", "main")]);
    fixture.json("plan", &[1], &[]);
    fixture.json("plan", &[], &["--all"]);
    let requests = fixture.server.seen.lock().unwrap();
    let queries: Vec<Value> = requests
        .iter()
        .filter_map(|request| request.split_once("\r\n\r\n"))
        .filter_map(|(_, body)| serde_json::from_str::<Value>(body).ok())
        .collect();
    assert_eq!(queries.len(), 2);
    for query in queries {
        let text = query["query"].as_str().unwrap();
        for field in ["defaultBranchRef { name }", "detailsUrl", "targetUrl"] {
            assert!(text.contains(field));
        }
    }
}

#[test]
fn strict_distinguishes_empty_waiting_and_mixed_selections() {
    let empty = Fixture::new(Vec::new());
    let mut waiting = node(1, "CLEAN", "main");
    waiting["body"] = json!("Depends-On: acme/api#2");
    let fixture = Fixture::new(vec![
        waiting,
        node(2, "CLEAN", "main"),
        node(3, "BLOCKED", "main"),
    ]);
    for command in ["plan", "merge"] {
        assert_eq!(
            empty
                .run(command, &[], &["--all", "--strict", "--json"])
                .status
                .code(),
            Some(4)
        );
        assert_eq!(
            empty.run(command, &[], &["--all", "--json"]).status.code(),
            Some(0)
        );
        let output = fixture.run(command, &[1], &["--strict", "--json"]);
        assert_eq!(output.status.code(), Some(4));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["preflight"][0]["status"], "waiting");
        assert!(
            report["preflight"][0]["causes"]
                .to_string()
                .contains("acme/api#2")
        );
        let mixed = fixture.json(command, &[2, 3], &["--strict"]);
        assert_eq!(mixed["preflight"][0]["status"], "eligible");
        assert_eq!(mixed["preflight"][1]["status"], "blocked");
    }
}

#[test]
fn strict_apply_with_no_eligible_pr_preserves_default_exit_and_does_not_merge() {
    let fixture = Fixture::new(vec![node(1, "BLOCKED", "main")]);
    let before = fs::read(fixture.directory.join("state.json")).unwrap();
    for strict in [false, true] {
        let mut flags = vec!["--apply", "--yes", "--json"];
        if strict {
            flags.push("--strict");
        }
        assert_eq!(
            fixture.run("merge", &[1], &flags).status.code(),
            Some(if strict { 4 } else { 0 })
        );
        assert_eq!(
            fs::read(fixture.directory.join("state.json")).unwrap(),
            before
        );
    }
}

#[test]
fn strict_merge_dry_run_with_only_inherited_resume_reports_four_without_writes() {
    let fixture = Fixture::new(Vec::new());
    fs::write(fixture.directory.join("state.json"), serde_json::to_vec(&json!({
        "schema": 2,
        "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": "a".repeat(40)}]
    })).unwrap()).unwrap();
    let before = fs::read(fixture.directory.join("state.json")).unwrap();
    for strict in [false, true] {
        let mut flags = vec!["--all", "--json"];
        if strict {
            flags.push("--strict");
        }
        let output = fixture.run("merge", &[], &flags);
        assert_eq!(output.status.code(), Some(if strict { 4 } else { 0 }));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["steps"].as_array().unwrap().len(), 1);
        assert_eq!(report["steps"][0]["kind"], "resume");
        assert_eq!(
            fs::read(fixture.directory.join("state.json")).unwrap(),
            before
        );
    }
}

#[test]
fn check_without_conclusion_reports_runtime_status_and_null_url_without_policy_inference() {
    let mut raw = node(1, "CLEAN", "main");
    raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = json!({"state": "PENDING", "contexts": {"nodes": [
        {"__typename": "CheckRun", "name": "package-version", "conclusion": null, "status": "IN_PROGRESS", "detailsUrl": null}
    ]}});
    let fixture = Fixture::new(vec![raw]);
    for command in ["plan", "merge"] {
        let report = fixture.json(command, &[1], &[]);
        assert_eq!(
            report["preflight"][0]["checks"][0],
            json!({"name": "package-version", "state": "IN_PROGRESS", "url": null})
        );
        assert_eq!(report["preflight"][0]["status"], "blocked");
        assert!(
            report["preflight"][0]["causes"]
                .to_string()
                .contains("checks pending")
        );
    }
}
