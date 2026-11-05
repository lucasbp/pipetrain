use pipetrain::github::parse_pr;
use serde_json::{Value, json};

fn node(merge_state: Option<&str>) -> Value {
    let mut node = json!({
        "number": 7,
        "title": "change",
        "url": "https://example.test/acme/api-core/pull/7",
        "body": "",
        "headRefName": "feat/a",
        "baseRefName": "master",
        "headRefOid": "abc",
        "isDraft": false,
        "state": "OPEN",
        "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE",
        "repository": {"name": "api-core", "owner": {"login": "acme"}},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}
    });
    if let Some(state) = merge_state {
        node["mergeStateStatus"] = json!(state);
    }
    node
}

#[test]
fn a_behind_pr_has_a_blocker() {
    let pr = parse_pr(&node(Some("BEHIND"))).unwrap();

    assert!(pr.behind);
    assert_eq!(
        pr.blockers(false),
        vec!["branch behind its base, update it"]
    );
}

#[test]
fn other_merge_states_and_a_missing_field_keep_the_pr_ready() {
    for state in [
        None,
        Some("CLEAN"),
        Some("BLOCKED"),
        Some("UNSTABLE"),
        Some("HAS_HOOKS"),
    ] {
        let pr = parse_pr(&node(state)).unwrap();

        assert!(!pr.behind, "{state:?}");
        assert!(pr.blockers(false).is_empty(), "{state:?}");
    }
}

#[test]
fn a_conflicting_pr_reports_the_conflict_not_the_behind_state() {
    let mut raw = node(Some("BEHIND"));
    raw["mergeable"] = json!("CONFLICTING");
    let pr = parse_pr(&raw).unwrap();

    assert_eq!(pr.blockers(false), vec!["merge conflict"]);
}
