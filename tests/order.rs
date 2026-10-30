use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
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

fn config(extra: &str) -> Config {
    Config::parse(&format!(
        "orgs = [\"acme\"]\n{extra}\n[[layer]]\nname = \"api\"\nrepos = [\"acme/api-*\"]\n[[layer]]\nname = \"front\"\nrepos = [\"acme/front-*\"]\n"
    ))
    .unwrap()
}

fn run(cfg: &Config, prs: Vec<Pr>) -> Planned {
    let rules = Rules::new(cfg).unwrap();
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

fn edges_of(planned: &Planned, rule: &str) -> usize {
    planned
        .items
        .iter()
        .flat_map(|i| i.after.iter())
        .filter(|d| d.rule.as_str() == rule)
        .count()
}

fn codes(planned: &Planned) -> Vec<&str> {
    planned.questions.iter().map(|q| q.code.as_str()).collect()
}

fn queue(cfg: &Config, prs: Vec<Pr>) -> Vec<String> {
    let rules = Rules::new(cfg).unwrap();
    let built = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: prs
                .into_iter()
                .map(|p| (p, "explicit".to_string()))
                .collect(),
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 1,
            rate_remaining: Some(4000),
        },
    );
    emit::queue_lines(&built)
}

fn position(lines: &[String], id: &str) -> usize {
    lines
        .iter()
        .position(|line| line == &format!("merge:{id}"))
        .unwrap()
}

const ORDER_FRONT_FIRST: &str = "[[order]]\nrepos = [\"acme/front-web\", \"acme/api-core\"]\n";

#[test]
fn the_earlier_repo_goes_first_without_a_shared_branch() {
    let api = pr("acme/api-core", 1, "feat/a", "master");
    let front = pr("acme/front-web", 2, "feat/b", "master");
    let planned = run(&config(ORDER_FRONT_FIRST), vec![api, front]);
    assert!(edge(
        &planned,
        "acme/front-web#2",
        "acme/api-core#1",
        "order"
    ));
    assert!(!edge(
        &planned,
        "acme/api-core#1",
        "acme/front-web#2",
        "order"
    ));
    assert!(codes(&planned).is_empty());
}

#[test]
fn the_edge_carries_the_config_order_reason() {
    let api = pr("acme/api-core", 1, "feat/a", "master");
    let front = pr("acme/front-web", 2, "feat/b", "master");
    let planned = run(&config(ORDER_FRONT_FIRST), vec![api, front]);
    let item = planned
        .items
        .iter()
        .find(|i| i.id == "acme/api-core#1")
        .unwrap();
    let dep = item
        .after
        .iter()
        .find(|d| d.rule.as_str() == "order")
        .unwrap();
    assert_eq!(dep.reason, "config order");
    assert_eq!(dep.id, "acme/front-web#2");
}

#[test]
fn glob_entries_match_many_repos() {
    let extra = "[[order]]\nrepos = [\"acme/*-sdk\", \"acme/*-service\"]\n";
    let sdk_a = pr("acme/auth-sdk", 1, "feat/a", "master");
    let sdk_b = pr("acme/billing-sdk", 2, "feat/b", "master");
    let service = pr("acme/auth-service", 3, "feat/c", "master");
    let planned = run(&config(extra), vec![service, sdk_a, sdk_b]);
    assert!(edge(
        &planned,
        "acme/auth-sdk#1",
        "acme/auth-service#3",
        "order"
    ));
    assert!(edge(
        &planned,
        "acme/billing-sdk#2",
        "acme/auth-service#3",
        "order"
    ));
    assert!(!edge(
        &planned,
        "acme/auth-sdk#1",
        "acme/billing-sdk#2",
        "order"
    ));
    assert!(!edge(
        &planned,
        "acme/billing-sdk#2",
        "acme/auth-sdk#1",
        "order"
    ));
}

#[test]
fn glob_matching_ignores_case() {
    let extra = "[[order]]\nrepos = [\"ACME/Front-Web\", \"acme/API-core\"]\n";
    let api = pr("acme/api-core", 1, "feat/a", "master");
    let front = pr("acme/front-web", 2, "feat/b", "master");
    let planned = run(&config(extra), vec![api, front]);
    assert_eq!(edges_of(&planned, "order"), 1);
}

#[test]
fn the_same_entry_gives_no_edge() {
    let extra = "[[order]]\nrepos = [\"acme/*-sdk\", \"acme/app\"]\n";
    let a = pr("acme/auth-sdk", 1, "feat/a", "master");
    let b = pr("acme/billing-sdk", 2, "feat/b", "master");
    let planned = run(&config(extra), vec![a, b]);
    assert_eq!(edges_of(&planned, "order"), 0);
}

#[test]
fn two_prs_in_the_same_repo_give_no_edge() {
    let extra = "[[order]]\nrepos = [\"acme/api-core\", \"acme/front-web\"]\n";
    let a = pr("acme/api-core", 1, "feat/a", "master");
    let b = pr("acme/api-core", 2, "feat/b", "master");
    let planned = run(&config(extra), vec![a, b]);
    assert_eq!(edges_of(&planned, "order"), 0);
}

#[test]
fn repos_outside_the_list_are_left_alone() {
    let a = pr("acme/api-core", 1, "feat/a", "master");
    let b = pr("acme/mystery", 2, "feat/b", "master");
    let planned = run(&config(ORDER_FRONT_FIRST), vec![a, b]);
    assert_eq!(edges_of(&planned, "order"), 0);
}

#[test]
fn several_blocks_each_apply() {
    let extra = "[[order]]\nrepos = [\"acme/front-web\", \"acme/api-core\"]\n[[order]]\nrepos = [\"acme/api-core\", \"acme/api-other\"]\n";
    let front = pr("acme/front-web", 1, "feat/a", "master");
    let core = pr("acme/api-core", 2, "feat/b", "master");
    let other = pr("acme/api-other", 3, "feat/c", "master");
    let planned = run(&config(extra), vec![front, core, other]);
    assert!(edge(
        &planned,
        "acme/front-web#1",
        "acme/api-core#2",
        "order"
    ));
    assert!(edge(
        &planned,
        "acme/api-core#2",
        "acme/api-other#3",
        "order"
    ));
}

#[test]
fn order_beats_the_layer_rule_on_a_shared_branch() {
    let api = pr("acme/api-core", 1, "feat/x", "master");
    let front = pr("acme/front-web", 2, "feat/x", "master");
    let planned = run(&config(ORDER_FRONT_FIRST), vec![api, front]);
    assert!(edge(
        &planned,
        "acme/front-web#2",
        "acme/api-core#1",
        "order"
    ));
    assert!(!edge(
        &planned,
        "acme/api-core#1",
        "acme/front-web#2",
        "layer"
    ));
    assert_eq!(codes(&planned), vec!["edge-conflict"]);
}

#[test]
fn depends_on_beats_order_and_the_conflict_is_asked() {
    let mut api = pr("acme/api-core", 1, "feat/a", "master");
    api.body = "Depends-On: acme/front-web#2".to_string();
    let front = pr("acme/front-web", 2, "feat/b", "master");
    let extra = "[[order]]\nrepos = [\"acme/api-core\", \"acme/front-web\"]\n";
    let planned = run(&config(extra), vec![api, front]);
    assert!(edge(
        &planned,
        "acme/front-web#2",
        "acme/api-core#1",
        "depends-on"
    ));
    assert!(!edge(
        &planned,
        "acme/api-core#1",
        "acme/front-web#2",
        "order"
    ));
    assert_eq!(codes(&planned), vec!["edge-conflict"]);
}

#[test]
fn the_queue_follows_the_order() {
    let api = pr("acme/api-core", 1, "feat/a", "master");
    let front = pr("acme/front-web", 2, "feat/b", "master");
    let lines = queue(&config(ORDER_FRONT_FIRST), vec![api, front]);
    assert!(position(&lines, "acme/front-web#2") < position(&lines, "acme/api-core#1"));
}

#[test]
fn without_order_the_queue_and_edges_are_unchanged() {
    let build = || {
        vec![
            pr("acme/api-core", 1, "feat/a", "master"),
            pr("acme/front-web", 2, "feat/b", "master"),
        ]
    };
    let planned = run(&config(""), build());
    assert_eq!(edges_of(&planned, "order"), 0);
    let lines = queue(&config(""), build());
    assert!(position(&lines, "acme/api-core#1") < position(&lines, "acme/front-web#2"));
}

#[test]
fn an_empty_order_block_is_rejected() {
    let cfg = config("[[order]]\nrepos = []\n");
    let message = format!("{:#}", Rules::new(&cfg).err().unwrap());
    assert!(message.contains("[[order]] number 1 has no repos"));
}

#[test]
fn an_invalid_glob_names_the_block() {
    let cfg = config("[[order]]\nrepos = [\"acme/[\"]\n");
    let message = format!("{:#}", Rules::new(&cfg).err().unwrap());
    assert!(message.contains("[[order]] number 1"));
}

#[test]
fn an_unknown_key_in_an_order_block_is_rejected() {
    let text = "[[order]]\nrepos = [\"acme/a\"]\nname = \"x\"\n";
    assert!(Config::parse(text).is_err());
}
