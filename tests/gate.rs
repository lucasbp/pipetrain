use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::planner::{self, PlanInput, Planned};
use pipetrain::publishers::Publishers;

fn pr(slug: &str, number: u64, head: &str, base: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        base_ref: base.to_string(),
        head_sha: format!("{number:040x}"),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
    }
}

fn with_sql(mut pr: Pr) -> Pr {
    pr.files = vec![ChangedFile {
        path: "db/migrations/001_add.sql".to_string(),
        patch: None,
    }];
    pr
}

fn run(prs: Vec<Pr>) -> Planned {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external: BTreeMap::new(),
        publishers: Publishers::new(),
    })
}

fn edge(planned: &Planned, from: &str, to: &str, rule: &str) -> bool {
    planned
        .items
        .iter()
        .find(|i| i.id == to)
        .unwrap()
        .after
        .iter()
        .any(|d| d.id == from && d.rule.as_str() == rule)
}

#[test]
fn gate_inside_a_target_group_goes_first_in_the_group() {
    let planned = run(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        with_sql(pr("acme/api-core", 3, "feat/c", "master")),
    ]);
    assert_eq!(planned.gate, vec!["acme/api-core#3".to_string()]);
    assert!(edge(
        &planned,
        "acme/api-core#3",
        "acme/api-core#1",
        "target"
    ));
    assert!(edge(
        &planned,
        "acme/api-core#1",
        "acme/api-core#2",
        "target"
    ));
    assert!(planned.questions.is_empty(), "{:?}", planned.questions);
}

#[test]
fn gate_that_must_wait_for_a_stacked_pr_is_a_question() {
    let train = with_sql(pr("acme/api-core", 10, "deploy/29-10-2026", "master"));
    let feature = pr("acme/api-core", 9, "feat/x", "deploy/29-10-2026");
    let planned = run(vec![train, feature]);
    assert!(edge(
        &planned,
        "acme/api-core#9",
        "acme/api-core#10",
        "stacked"
    ));
    let codes: Vec<&str> = planned.questions.iter().map(|q| q.code.as_str()).collect();
    assert_eq!(codes, vec!["gate-has-predecessor"]);
    assert_eq!(planned.questions[0].pr.as_deref(), Some("acme/api-core#10"));
}

#[test]
fn two_gates_come_out_in_graph_order() {
    let mut late = with_sql(pr("acme/api-a", 1, "feat/a", "master"));
    late.body = "Depends-On: acme/api-b#2".to_string();
    let early = with_sql(pr("acme/api-b", 2, "feat/b", "master"));
    let planned = run(vec![late, early]);
    assert_eq!(
        planned.gate,
        vec!["acme/api-b#2".to_string(), "acme/api-a#1".to_string()]
    );
}
