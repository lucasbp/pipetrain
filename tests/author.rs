use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::discovery::{Scope, Skipped};
use pipetrain::github::parse_pr;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
use pipetrain::publishers::Publishers;
use pipetrain::render;
use serde_json::{Value, json};

fn pr(number: u64, author: &str) -> Pr {
    Pr {
        key: PrKey::new("acme", "api", number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/acme/api/pull/{number}"),
        author: author.to_string(),
        body: String::new(),
        head_ref: "deploy/30-10-2026".to_string(),
        default_branch: String::new(),
        merge_state_status: String::new(),
        check_details: Vec::new(),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        failed_checks: Vec::new(),
        conflicting: false,
        mergeable_unknown: false,
        behind: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn built(ready: Vec<Pr>, skipped: Vec<Pr>) -> plan::Plan {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: ready
                .into_iter()
                .map(|p| (p, "train".to_string()))
                .collect(),
            skipped: skipped
                .into_iter()
                .map(|pr| Skipped {
                    pr,
                    scope: Scope::Train,
                    reasons: vec!["draft".to_string()],
                })
                .collect(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 1,
            rate_remaining: Some(4000),
        },
    )
}

fn plain(plan: &plan::Plan) -> String {
    anstream::adapter::strip_str(&render::text(plan)).to_string()
}

fn node(author: Value) -> Value {
    let mut raw = json!({
        "number": 7,
        "repository": { "name": "api", "owner": { "login": "acme" } },
    });
    if !author.is_null() {
        raw["author"] = author;
    }
    raw
}

#[test]
fn the_author_login_is_read_from_the_node() {
    let parsed = parse_pr(&node(json!({ "login": "alice" }))).unwrap();
    assert_eq!(parsed.author, "alice");
}

#[test]
fn a_missing_or_null_author_is_empty() {
    assert_eq!(parse_pr(&node(Value::Null)).unwrap().author, "");
    let mut raw = node(Value::Null);
    raw["author"] = Value::Null;
    assert_eq!(parse_pr(&raw).unwrap().author, "");
}

#[test]
fn json_carries_the_author_on_items_and_skipped() {
    let plan = built(vec![pr(1, "alice")], vec![pr(2, "bob")]);
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(value["items"][0]["author"], "alice");
    assert_eq!(value["skipped"][0]["author"], "bob");
}

#[test]
fn json_has_no_author_key_when_nobody_is_known() {
    let plan = built(vec![pr(1, "")], vec![pr(2, "")]);
    let value = serde_json::to_value(&plan).unwrap();
    assert!(value["items"][0].get("author").is_none());
    assert!(value["skipped"][0].get("author").is_none());
}

#[test]
fn text_shows_the_handle_only_for_prs_with_an_author() {
    let with = plain(&built(vec![pr(1, "alice")], vec![pr(2, "bob")]));
    assert!(with.contains("api#1 @alice"), "{with}");
    assert!(with.contains("api#2 @bob"), "{with}");
    let without = plain(&built(vec![pr(1, "")], vec![pr(2, "")]));
    assert!(!without.contains('@'), "{without}");
}
