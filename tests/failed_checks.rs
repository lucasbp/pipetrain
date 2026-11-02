use pipetrain::github::parse_pr;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use serde_json::{Value, json};

fn node(contexts: Value) -> Value {
    json!({
        "number": 7,
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "commits": { "nodes": [ { "commit": { "statusCheckRollup": {
            "state": "FAILURE",
            "contexts": { "nodes": contexts }
        } } } ] },
    })
}

fn pr_with(failed: &[&str]) -> Pr {
    Pr {
        key: PrKey::new("acme", "api", 7),
        state: PrState::Open,
        title: String::new(),
        url: String::new(),
        body: String::new(),
        head_ref: "feat/a".to_string(),
        base_ref: "master".to_string(),
        head_sha: String::new(),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Failure,
        author: String::new(),
        failed_checks: failed.iter().map(|n| n.to_string()).collect(),
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

#[test]
fn failing_check_runs_and_status_contexts_are_named_sorted_and_deduplicated() {
    let pr = parse_pr(&node(json!([
        { "__typename": "CheckRun", "name": "test", "conclusion": "FAILURE" },
        { "__typename": "CheckRun", "name": "build", "conclusion": "TIMED_OUT" },
        { "__typename": "CheckRun", "name": "build", "conclusion": "FAILURE" },
        { "__typename": "CheckRun", "name": "lint", "conclusion": "SUCCESS" },
        { "__typename": "StatusContext", "context": "ci/legacy", "state": "ERROR" },
        { "__typename": "StatusContext", "context": "ci/ok", "state": "SUCCESS" },
    ])))
    .unwrap();
    assert_eq!(pr.checks, Checks::Failure);
    assert_eq!(pr.failed_checks, vec!["build", "ci/legacy", "test"]);
}

#[test]
fn a_node_without_contexts_has_no_failed_check_names() {
    let mut raw = node(json!([]));
    raw["commits"]["nodes"][0]["commit"]["statusCheckRollup"]
        .as_object_mut()
        .unwrap()
        .remove("contexts");
    assert!(parse_pr(&raw).unwrap().failed_checks.is_empty());
    assert!(
        parse_pr(&json!({
            "number": 7,
            "repository": { "name": "api", "owner": { "login": "acme" } },
        }))
        .unwrap()
        .failed_checks
        .is_empty()
    );
}

#[test]
fn the_reason_names_the_failing_checks() {
    assert_eq!(pr_with(&[]).blockers(false), vec!["checks failing"]);
    assert_eq!(
        pr_with(&["build", "test"]).blockers(false),
        vec!["checks failing: build, test"]
    );
}

#[test]
fn the_reason_shows_five_names_and_counts_the_rest() {
    let names = ["a", "b", "c", "d", "e", "f", "g"];
    assert_eq!(
        pr_with(&names).blockers(false),
        vec!["checks failing: a, b, c, d, e, +2 more"]
    );
    assert_eq!(
        pr_with(&names[..5]).blockers(false),
        vec!["checks failing: a, b, c, d, e"]
    );
}
