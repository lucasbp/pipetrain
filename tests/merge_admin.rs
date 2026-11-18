mod common;

#[path = "common/merge_admin_apply.rs"]
mod apply;

use std::fs;
use std::process::{Command, Output, Stdio};

use common::{Server, TempDir, serve_requests};
use serde_json::{Value, json};

fn node(number: u64) -> Value {
    json!({
        "number": number, "title": "change", "body": "",
        "url": format!("https://github.com/acme/api/pull/{number}"),
        "headRefName": format!("feature/{number}"), "baseRefName": "main",
        "headRefOid": format!("{number:040x}"), "baseRef": {"target": {"oid": "b".repeat(40)}},
        "isDraft": false, "state": "OPEN", "reviewDecision": "REVIEW_REQUIRED",
        "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED",
        "repository": {"name": "api", "owner": {"login": "acme"}, "defaultBranchRef": {"name": "main"}},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS", "contexts": {"nodes": []}}}}]}
    })
}

struct Fixture {
    directory: TempDir,
    server: Server,
}

impl Fixture {
    fn new(
        nodes: Vec<Value>,
        admin: bool,
        bypass: bool,
        protection: Option<Value>,
        rules: Value,
    ) -> Self {
        Self::with_compare(
            nodes,
            admin,
            bypass,
            protection,
            rules,
            json!({"behind_by": 0}),
        )
    }

    fn with_compare(
        nodes: Vec<Value>,
        admin: bool,
        bypass: bool,
        protection: Option<Value>,
        rules: Value,
        comparison: Value,
    ) -> Self {
        let directory = TempDir::new("merge-admin");
        fs::write(directory.join("config.toml"), "orgs = ['acme']\nallow_missing_checks = true\n[api]\nmax_requests = 200\nmax_per_minute = 10000\nmin_remaining = 0\n[merge]\nstate_file = 'state.json'\nno_deploy_repos = ['acme/api']\n").unwrap();
        fs::write(
            directory.join("state.json"),
            "{\"schema\":2,\"pending\":[]}",
        )
        .unwrap();
        let server = serve_requests(move |line, payload| {
            if line == "POST /graphql" {
                let body: Value = serde_json::from_str(payload).unwrap();
                let query = body["query"].as_str().unwrap();
                if query.contains("viewerCanMergeAsAdmin") {
                    return (
                        200,
                        json!({"data": {"repository": {"pullRequest": {"viewerCanMergeAsAdmin": bypass}}}}),
                    );
                }
                if query.contains("viewer { login }") {
                    return (200, json!({"data": {"viewer": {"login": "octoadmin"}}}));
                }
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
                        json!({"data": {"search": {"nodes": nodes, "issueCount": nodes.len(), "pageInfo": {"hasNextPage": false}}}}),
                    );
                }
                return (200, json!({"data": {}}));
            }
            if line == "GET /repos/acme/api" {
                return (200, json!({"permissions": {"admin": admin}}));
            }
            if line.contains("/protection") {
                return protection
                    .clone()
                    .map_or((404, json!({"message": "Branch not protected"})), |body| {
                        (200, body)
                    });
            }
            if line.contains("/rules/branches/") {
                return (200, rules.clone());
            }
            if line.contains("/compare/") {
                return (200, comparison.clone());
            }
            if line.starts_with("GET ") && line.contains("/files?") {
                let number = line
                    .split('/')
                    .nth(5)
                    .and_then(|number| number.parse::<u64>().ok());
                let file = nodes
                    .iter()
                    .find(|node| node["number"].as_u64() == number)
                    .and_then(|node| node["fixtureFile"].as_str())
                    .unwrap_or("src/app.rs");
                return (200, json!([{"filename": file, "status": "modified"}]));
            }
            if line.starts_with("GET ") && line.contains("/git/trees/") {
                return (200, json!({"tree": [], "truncated": false}));
            }
            (404, json!({"message": "unexpected request"}))
        });
        Self { directory, server }
    }

    fn review_only() -> Self {
        Self::new(vec![node(1)], true, true, None, json!([]))
    }

    fn run(&self, ids: &[u64], extra: &[&str]) -> Output {
        let mut process = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
        process.arg("merge");
        for id in ids {
            process.arg(format!("acme/api#{id}"));
        }
        if !extra.contains(&"--json") {
            process.args(["--color", "never"]);
        }
        let output = process
            .args(["--config", "config.toml", "--no-cache", "--no-progress"])
            .args(extra)
            .current_dir(self.directory.path())
            .env("GH_TOKEN", "ghp_FAKE0123456789")
            .env_remove("GITHUB_TOKEN")
            .env("PIPETRAIN_API_URL", &self.server.base)
            .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
            .env("PIPETRAIN_LOG", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(self.server.seen.lock().unwrap().iter().all(|request| request.starts_with("GET ") || request.starts_with("POST /graphql ")));
        output
    }

    fn json(&self, ids: &[u64], flags: &[&str]) -> Value {
        let mut extra = vec!["--json"];
        extra.extend(flags);
        let output = self.run(ids, &extra);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn admin_reads(&self) -> Vec<String> {
        self.server
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|request| {
                request.starts_with("GET /repos/acme/api HTTP")
                    || request.contains("/protection")
                    || request.contains("/rules/branches/")
                    || request.contains("viewerCanMergeAsAdmin")
                    || request.contains("viewer { login }")
            })
            .cloned()
            .collect()
    }
}

#[test]
fn without_admin_review_required_keeps_legacy_report_and_text() {
    let fixture = Fixture::review_only();
    let report = fixture.json(&[1], &[]);
    assert_eq!(report["schema"], 1);
    assert_eq!(report["steps"], json!([]));
    assert_eq!(
        report["not_merging"],
        json!([{"id": "acme/api#1", "reasons": ["review required", "branch protection blocks merge (BLOCKED)"]}])
    );
    assert!(!report.to_string().contains("admin_override"));
    let output = fixture.run(&[1], &["--verbose"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("nothing to merge\n\nnot merging, the plan marks these as not ready:\n  acme/api#1: review required, branch protection blocks merge (BLOCKED)\n"), "{text}");
    assert!(!text.contains("--admin") && !text.contains("admin override"));
    assert!(fixture.admin_reads().is_empty());
}

#[test]
fn admin_review_only_dry_run_marks_json_and_text_without_timestamp_or_writes() {
    let fixture = Fixture::review_only();
    let before = fs::read(fixture.directory.join("state.json")).unwrap();
    let report = fixture.json(&[1], &["--admin", "--strict"]);
    assert_eq!(report["steps"][0]["target"], "acme/api#1");
    assert_eq!(
        report["steps"][0]["admin_override"],
        json!({"reason": "REVIEW_REQUIRED", "requested_by": "octoadmin"})
    );
    assert_eq!(report["steps"][0]["status"], "planned");
    assert_eq!(report["not_merging"], json!([]));
    let output = fixture.run(&[1], &["--admin", "--verbose"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("--admin: 1 PR(s) would be merged past a missing review:\n  acme/api#1\n"),
        "{text}"
    );
    assert!(
        text.contains("admin override: merges past REVIEW_REQUIRED as octoadmin"),
        "{text}"
    );
    assert!(text.find("--admin: 1").unwrap() < text.find("merge acme/api#1").unwrap());
    assert_eq!(
        fs::read(fixture.directory.join("state.json")).unwrap(),
        before
    );
}

#[test]
fn other_blockers_make_no_admin_reads() {
    let mut cases = Vec::new();
    for state in ["FAILURE", "PENDING"] {
        let mut raw = node(1);
        raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"] = json!(state);
        cases.push(raw);
    }
    for (field, value) in [
        ("mergeable", json!("CONFLICTING")),
        ("reviewDecision", json!("CHANGES_REQUESTED")),
        ("isDraft", json!(true)),
        ("mergeStateStatus", json!("BEHIND")),
    ] {
        let mut raw = node(1);
        raw[field] = value;
        cases.push(raw);
    }
    for raw in cases {
        let fixture = Fixture::new(vec![raw], true, true, None, json!([]));
        let report = fixture.json(&[1], &["--admin"]);
        assert_eq!(report["steps"], json!([]));
        assert_eq!(report["not_merging"][0]["id"], "acme/api#1");
        assert!(fixture.admin_reads().is_empty());
    }
}

#[test]
fn admin_requires_repository_permission_and_keeps_empty_plan_exit_code() {
    let fixture = Fixture::new(vec![node(1)], false, true, None, json!([]));
    let report = fixture.json(&[1], &["--admin"]);
    assert_eq!(report["steps"], json!([]));
    assert_eq!(
        report["not_merging"][0]["reasons"],
        json!([
            "review required",
            "branch protection blocks merge (BLOCKED)",
            "--admin needs admin permission on acme/api; octoadmin does not have it"
        ])
    );
    assert_eq!(
        fixture.run(&[1], &["--admin", "--strict"]).status.code(),
        Some(4)
    );
    assert!(!fixture.admin_reads().iter().any(
        |request| request.contains("viewerCanMergeAsAdmin") || request.contains("/protection")
    ));
}

#[test]
fn admin_requires_github_viewer_bypass_permission() {
    let fixture = Fixture::new(vec![node(1)], true, false, None, json!([]));
    let report = fixture.json(&[1], &["--admin"]);
    assert_eq!(report["steps"], json!([]));
    assert_eq!(
        report["not_merging"][0]["reasons"][2],
        "GitHub does not let octoadmin bypass the protection of acme/api@main (administrators are included in the rule or not in the ruleset bypass list)"
    );
}

#[test]
fn missing_checks_refuse_classic_contexts_checks_and_ruleset_requirements() {
    for (protection, rules) in [
        (
            Some(json!({"required_status_checks": {"contexts": ["ci"], "checks": []}})),
            json!([]),
        ),
        (
            Some(
                json!({"required_status_checks": {"contexts": [], "checks": [{"context": "ci"}]}}),
            ),
            json!([]),
        ),
        (
            None,
            json!([{"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "ci"}]}}]),
        ),
    ] {
        let mut raw = node(1);
        raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = Value::Null;
        let fixture = Fixture::new(vec![raw], true, true, protection, rules);
        let report = fixture.json(&[1], &["--admin"]);
        assert_eq!(report["steps"], json!([]));
        let refusal = report["not_merging"][0]["reasons"][2].as_str().unwrap();
        assert!(
            refusal.contains("required_status_checks") && refusal.contains("ci"),
            "{refusal}"
        );
    }
}

#[test]
fn missing_checks_pass_only_with_existing_allowance_and_no_required_checks() {
    let mut raw = node(1);
    raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = Value::Null;
    let fixture = Fixture::new(vec![raw], true, true, None, json!([]));
    assert!(fixture.json(&[1], &["--admin"])["steps"][0]["admin_override"].is_object());
    let config = fixture.directory.join("config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config).unwrap().replace(
            "allow_missing_checks = true",
            "allow_missing_checks = false",
        ),
    )
    .unwrap();
    fixture.server.seen.lock().unwrap().clear();
    let report = fixture.json(&[1], &["--admin"]);
    assert_eq!(report["steps"], json!([]));
    assert!(fixture.admin_reads().is_empty());
}

#[test]
fn already_ready_pr_is_unchanged_and_makes_no_admin_reads() {
    let mut raw = node(1);
    raw["reviewDecision"] = json!("APPROVED");
    raw["mergeStateStatus"] = json!("CLEAN");
    let fixture = Fixture::new(vec![raw], true, true, None, json!([]));
    fs::remove_file(fixture.directory.join("state.json")).unwrap();
    let baseline = fixture.json(&[1], &[]);
    let with_admin = fixture.json(&[1], &["--admin"]);
    assert_eq!(baseline, with_admin);
    assert!(with_admin["steps"][0].get("admin_override").is_none());
    assert_eq!(
        fixture.run(&[1], &[]).stdout,
        fixture.run(&[1], &["--admin"]).stdout
    );
    assert!(fixture.admin_reads().is_empty());
}

#[test]
fn non_review_classic_and_unknown_ruleset_requirements_refuse() {
    for name in [
        "required_signatures",
        "required_conversation_resolution",
        "lock_branch",
        "unknown_future_rule",
    ] {
        let protection = (name != "unknown_future_rule").then(|| json!({name: {"enabled": true}}));
        let rules = if protection.is_none() {
            json!([{"type": name}])
        } else {
            json!([])
        };
        let fixture = Fixture::new(vec![node(1)], true, true, protection, rules);
        let report = fixture.json(&[1], &["--admin"]);
        assert_eq!(report["steps"], json!([]));
        assert_eq!(
            report["not_merging"][0]["reasons"][2],
            format!(
                "--admin only bypasses a missing review; acme/api@main also requires {name}, which pipetrain cannot verify"
            )
        );
    }
}

#[test]
fn verified_prs_keep_dependency_order_and_verify_each_override() {
    let mut first = node(1);
    first["body"] = json!("Depends-On: acme/api#2");
    let fixture = Fixture::new(
        vec![first, node(2)],
        true,
        true,
        None,
        json!([
            {"type": "pull_request"}, {"type": "required_status_checks", "parameters": {"required_status_checks": []}}, {"type": "deletion"},
            {"type": "non_fast_forward"}, {"type": "creation"}, {"type": "update"}, {"type": "required_linear_history"}
        ]),
    );
    let report = fixture.json(&[1, 2], &["--admin", "--method", "squash"]);
    assert_eq!(report["steps"][0]["target"], "acme/api#2");
    assert_eq!(report["steps"][1]["target"], "acme/api#1");
    let reads = fixture.admin_reads();
    assert_eq!(
        reads
            .iter()
            .filter(|request| request.starts_with("GET /repos/acme/api HTTP"))
            .count(),
        2
    );
    assert_eq!(
        reads
            .iter()
            .filter(|request| request.contains("/protection"))
            .count(),
        2
    );
    assert_eq!(
        reads
            .iter()
            .filter(|request| request.contains("/rules/branches/"))
            .count(),
        2
    );
    assert_eq!(
        reads
            .iter()
            .filter(|request| request.contains("viewerCanMergeAsAdmin"))
            .count(),
        2
    );
    assert_eq!(
        reads
            .iter()
            .filter(|request| request.contains("viewer { login }"))
            .count(),
        1
    );
}

#[test]
fn apply_review_required_merges_with_admin_override() {
    apply::review_only_apply();
}

#[test]
fn admin_flag_is_only_offered_by_merge() {
    for (command, expected) in [("merge", true), ("plan", false)] {
        let output = Command::new(env!("CARGO_BIN_EXE_pipetrain"))
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).contains("--admin"),
            expected
        );
    }
}

#[test]
fn mixed_selection_marks_only_the_review_override() {
    let mut ready = node(2);
    ready["reviewDecision"] = json!("APPROVED");
    ready["mergeStateStatus"] = json!("CLEAN");
    let fixture = Fixture::new(vec![node(1), ready], true, true, None, json!([]));
    let report = fixture.json(&[1, 2], &["--admin"]);
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert!(
        steps
            .iter()
            .find(|step| step["target"] == "acme/api#1")
            .unwrap()["admin_override"]
            .is_object()
    );
    assert!(
        steps
            .iter()
            .find(|step| step["target"] == "acme/api#2")
            .unwrap()
            .get("admin_override")
            .is_none()
    );
    assert_eq!(
        fixture
            .admin_reads()
            .iter()
            .filter(|request| request.contains("viewerCanMergeAsAdmin"))
            .count(),
        1
    );
}

#[test]
fn admin_override_preserves_manual_gates() {
    let mut ddl = node(1);
    ddl["fixtureFile"] = json!("db/migrations/001_add.sql");
    let mut ready = node(2);
    ready["reviewDecision"] = json!("APPROVED");
    ready["mergeStateStatus"] = json!("CLEAN");
    let fixture = Fixture::new(
        vec![ddl.clone(), ready.clone()],
        true,
        true,
        None,
        json!([]),
    );
    let mut report = fixture.json(&[1, 2], &["--admin"]);
    assert_eq!(report["steps"][0]["target"], "acme/api#1");
    assert!(report["steps"][0]["admin_override"].is_object());
    assert_eq!(report["steps"][1]["kind"], "gate");
    assert!(report["steps"][1].get("admin_override").is_none());
    assert_eq!(report["steps"][2]["target"], "acme/api#2");
    ddl["reviewDecision"] = json!("APPROVED");
    ddl["mergeStateStatus"] = json!("CLEAN");
    let baseline = Fixture::new(vec![ddl, ready], true, true, None, json!([])).json(&[1, 2], &[]);
    report["steps"][0]
        .as_object_mut()
        .unwrap()
        .remove("admin_override");
    assert_eq!(report["steps"], baseline["steps"]);
}

#[test]
fn legacy_step_reports_default_to_no_admin_override() {
    let report: pipetrain::merge::StepReport = serde_json::from_value(json!({
        "kind": "merge", "target": "acme/api#1", "status": "planned", "detail": "into main"
    }))
    .unwrap();
    assert!(report.admin_override.is_none());
    assert!(
        serde_json::to_value(report)
            .unwrap()
            .get("admin_override")
            .is_none()
    );
}

fn check(raw: &mut Value, name: &str, state: &str) {
    raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"]
        .as_array_mut().unwrap().push(json!({"__typename": "CheckRun", "name": name, "conclusion": state, "status": "COMPLETED"}));
}

fn required_checks_sources() -> Vec<(Option<Value>, Value)> {
    vec![
        (
            Some(json!({"required_status_checks": {"contexts": ["required-ci"]}})),
            json!([]),
        ),
        (
            Some(json!({"required_status_checks": {"checks": [{"context": "required-ci"}]}})),
            json!([]),
        ),
        (
            None,
            json!([{"type": "required_status_checks", "parameters": {"required_status_checks": [{"context": "required-ci"}]}}]),
        ),
    ]
}

#[test]
fn admin_refuses_required_check_missing_while_another_is_green() {
    for (protection, rules) in required_checks_sources() {
        let mut raw = node(1);
        check(&mut raw, "other-ci", "SUCCESS");
        let fixture = Fixture::new(vec![raw], true, true, protection, rules);
        let report = fixture.json(&[1], &["--admin"]);
        assert_eq!(report["steps"], json!([]));
        assert!(
            report["not_merging"][0]["reasons"]
                .to_string()
                .contains("missing or non-passing required checks: required-ci"),
            "{report}"
        );
    }
}

#[test]
fn admin_refuses_nonpassing_required_check_even_with_green_rollup() {
    for state in ["FAILURE", "PENDING", "CANCELLED", "TIMED_OUT", "UNKNOWN"] {
        for (protection, rules) in required_checks_sources() {
            let mut raw = node(1);
            check(&mut raw, "other-ci", "SUCCESS");
            check(&mut raw, "required-ci", state);
            let fixture = Fixture::new(vec![raw], true, true, protection, rules);
            let report = fixture.json(&[1], &["--admin"]);
            assert_eq!(report["steps"], json!([]));
            assert!(
                report["not_merging"][0]["reasons"]
                    .to_string()
                    .contains("missing or non-passing required checks: required-ci"),
                "{report}"
            );
        }
    }
}

#[test]
fn admin_accepts_required_checks_success_neutral_skipped_case_insensitively() {
    for state in ["SUCCESS", "neutral", "sKiPpEd"] {
        for (protection, rules) in required_checks_sources() {
            let mut raw = node(1);
            check(&mut raw, "required-ci", state);
            let fixture = Fixture::new(vec![raw], true, true, protection, rules);
            let report = fixture.json(&[1], &["--admin"]);
            assert!(report["steps"][0]["admin_override"].is_object(), "{report}");
        }
    }
}

#[test]
fn admin_strict_requires_zero_behind_by_for_classic_and_rulesets() {
    for ruleset in [false, true] {
        for comparison in [json!({"behind_by": 1}), json!({"behind_by": 0}), json!({})] {
            let protection = (!ruleset).then(
                || json!({"required_status_checks": {"strict": true, "contexts": ["required-ci"]}}),
            );
            let rules = if ruleset {
                json!([{"type": "required_status_checks", "parameters": {"strict_required_status_checks_policy": true, "required_status_checks": [{"context": "required-ci"}]}}])
            } else {
                json!([])
            };
            let mut raw = node(1);
            check(&mut raw, "required-ci", "SUCCESS");
            let fixture =
                Fixture::with_compare(vec![raw], true, true, protection, rules, comparison.clone());
            let report = fixture.json(&[1], &["--admin"]);
            if comparison["behind_by"] == 0 {
                assert!(report["steps"][0]["admin_override"].is_object(), "{report}");
            } else {
                assert_eq!(report["steps"], json!([]));
                let refusal = report["not_merging"][0]["reasons"].to_string();
                if comparison["behind_by"] == 1 {
                    assert!(
                        refusal.contains(
                            "acme/api#1 is behind main and the branch requires it to be up to date"
                        ),
                        "{report}"
                    );
                } else {
                    assert!(refusal.contains("did not return behind_by"), "{report}");
                }
            }
            assert!(
                fixture
                    .server
                    .seen
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.starts_with(&format!(
                        "GET /repos/acme/api/compare/main...{:040x} HTTP",
                        1
                    )))
            );
        }
    }
}

#[test]
fn admin_refuses_ruleset_review_thread_resolution() {
    let fixture = Fixture::new(
        vec![node(1)],
        true,
        true,
        None,
        json!([{"type": "pull_request", "parameters": {"required_review_thread_resolution": true}}]),
    );
    let report = fixture.json(&[1], &["--admin"]);
    assert_eq!(report["steps"], json!([]));
    assert!(
        report["not_merging"][0]["reasons"]
            .to_string()
            .contains("required_review_thread_resolution"),
        "{report}"
    );
}

#[test]
fn admin_requires_configured_method_in_every_ruleset() {
    for method in ["merge", "squash", "rebase"] {
        let rules = json!([
            {"type": "pull_request", "parameters": {"allowed_merge_methods": ["merge", "squash", "rebase"]}},
            {"type": "pull_request", "parameters": {"allowed_merge_methods": ["squash"]}}
        ]);
        let fixture = Fixture::new(vec![node(1)], true, true, None, rules);
        let report = fixture.json(&[1], &["--admin", "--method", method]);
        if method == "squash" {
            assert!(report["steps"][0]["admin_override"].is_object(), "{report}");
        } else {
            assert_eq!(report["steps"], json!([]));
            assert!(
                report["not_merging"][0]["reasons"]
                    .to_string()
                    .contains("allowed_merge_methods"),
                "{report}"
            );
        }
    }
    let fixture = Fixture::new(
        vec![node(1)],
        true,
        true,
        None,
        json!([{"type": "pull_request", "parameters": {"allowed_merge_methods": ["squash"]}}]),
    );
    let config = fixture.directory.join("config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace("[merge]", "[merge]\nmethod = 'squash'"),
    )
    .unwrap();
    assert!(fixture.json(&[1], &["--admin"])["steps"][0]["admin_override"].is_object());
}

#[test]
fn admin_linear_history_refuses_merge_and_accepts_squash_or_rebase() {
    for ruleset in [false, true] {
        for method in ["merge", "squash", "rebase"] {
            let protection =
                (!ruleset).then(|| json!({"required_linear_history": {"enabled": true}}));
            let rules = if ruleset {
                json!([{"type": "required_linear_history"}])
            } else {
                json!([])
            };
            let fixture = Fixture::new(vec![node(1)], true, true, protection, rules);
            let report = fixture.json(&[1], &["--admin", "--method", method]);
            if method == "merge" {
                assert_eq!(report["steps"], json!([]));
                assert!(
                    report["not_merging"][0]["reasons"]
                        .to_string()
                        .contains("required_linear_history"),
                    "{report}"
                );
            } else {
                assert!(report["steps"][0]["admin_override"].is_object(), "{report}");
            }
        }
    }
}

#[test]
fn admin_refuses_unreadable_required_check_lists() {
    for (protection, rules) in [
        (Some(json!({"required_status_checks": {}})), json!([])),
        (Some(json!({"required_status_checks": true})), json!([])),
        (
            Some(json!({"required_status_checks": {"contexts": null}})),
            json!([]),
        ),
        (
            Some(json!({"required_status_checks": {"checks": [{}]}})),
            json!([]),
        ),
        (None, json!([{"type": "required_status_checks"}])),
        (
            None,
            json!([{"type": "required_status_checks", "parameters": {"required_status_checks": [{}]}}]),
        ),
    ] {
        let fixture = Fixture::new(vec![node(1)], true, true, protection, rules);
        let report = fixture.json(&[1], &["--admin"]);
        assert_eq!(report["steps"], json!([]), "{report}");
        assert!(
            report["not_merging"][0]["reasons"]
                .as_array()
                .unwrap()
                .len()
                > 2,
            "{report}"
        );
    }
}
