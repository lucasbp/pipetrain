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
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        author: String::new(),
        failed_checks: Vec::new(),
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
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
    run_with("", prs)
}

fn bump(mut consumer: Pr, package: &str) -> Pr {
    consumer.files.push(ChangedFile {
        path: "package.json".to_string(),
        patch: Some(format!(
            "-    \"{package}\": \"^1.0.0\",\n+    \"{package}\": \"^1.1.0\","
        )),
    });
    consumer
}

fn run_with(extra: &str, prs: Vec<Pr>) -> Planned {
    let cfg = Config::parse(&format!("orgs = [\"acme\"]\n{extra}\n")).unwrap();
    let rules = Rules::new(&cfg).unwrap();
    planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external: BTreeMap::new(),
        publishers: Publishers::new(),
        references: Default::default(),
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
fn gate_without_a_predecessor_is_early_and_leaves_no_gate_late_in_json() {
    let planned = run(vec![
        with_sql(pr("acme/api-core", 1, "feat/a", "master")),
        pr("acme/api-core", 2, "feat/b", "master"),
    ]);
    let gate = planned
        .items
        .iter()
        .find(|i| i.id == "acme/api-core#1")
        .unwrap();
    assert!(!gate.gate_late);
    assert!(
        serde_json::to_value(gate)
            .unwrap()
            .get("gate_late")
            .is_none()
    );
    assert!(planned.questions.is_empty(), "{:?}", planned.questions);
}

#[test]
fn gate_that_bumps_a_package_published_in_the_plan_is_a_late_gate() {
    let publisher = pr("acme/kit", 6, "feat/a", "master");
    let gate = bump(with_sql(pr("acme/web", 5, "feat/b", "master")), "@acme/kit");
    let other = pr("acme/other", 7, "feat/c", "master");
    let planned = run_with(
        "[packages]\n\"@acme/kit\" = \"acme/kit\"\n",
        vec![publisher, gate, other],
    );
    assert!(planned.questions.is_empty(), "{:?}", planned.questions);
    assert!(edge(&planned, "acme/kit#6", "acme/web#5", "package"));
    assert_eq!(planned.gate, vec!["acme/web#5".to_string()]);
    let item = planned.items.iter().find(|i| i.id == "acme/web#5").unwrap();
    assert!(item.gate_late);
    assert!(item.manual_pause);
    assert!(item.stage > 0);
    assert_eq!(
        serde_json::to_value(item).unwrap()["gate_late"],
        serde_json::Value::Bool(true)
    );
    for other in planned.items.iter().filter(|i| i.id != "acme/web#5") {
        assert!(
            other.after.iter().all(|d| d.id != "acme/web#5"),
            "{} waits for the late gate",
            other.id
        );
        assert!(!other.gate_late);
    }
}

#[test]
fn gate_with_a_package_and_a_stacked_predecessor_is_still_a_question() {
    let publisher = pr("acme/kit", 6, "feat/a", "master");
    let train = bump(
        with_sql(pr("acme/web", 10, "deploy/29-10-2026", "master")),
        "@acme/kit",
    );
    let feature = pr("acme/web", 9, "feat/x", "deploy/29-10-2026");
    let planned = run_with(
        "[packages]\n\"@acme/kit\" = \"acme/kit\"\n",
        vec![publisher, train, feature],
    );
    let codes: Vec<&str> = planned.questions.iter().map(|q| q.code.as_str()).collect();
    assert_eq!(codes, vec!["gate-has-predecessor"]);
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
