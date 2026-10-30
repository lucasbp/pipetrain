use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use pipetrain::config::{Config, Rules};
use pipetrain::discovery::{self, Discovered};
use pipetrain::emit::{self, PAUSE_LINE};
use pipetrain::github;
use pipetrain::model::Question;
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers;
use regex::Regex;
use serde_json::Value;

const TRAIN: &str = "deploy/28-10-2026";
const GATE: &str = "acme/svc-billing#21";
const SKIPPED: &str = "acme/svc-legacy#12";

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("train-2026-10-28")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).expect("valid json")
}

fn manifests() -> Vec<(String, String, String)> {
    json("manifests.json")
        .as_array()
        .expect("array")
        .iter()
        .map(|m| {
            (
                m["repo"].as_str().unwrap().to_string(),
                m["path"].as_str().unwrap().to_string(),
                m["text"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn build() -> Plan {
    let cfg = Config::parse(&fixture("config.toml")).expect("config");
    let rules = Rules::new(&cfg).expect("rules");
    let all = github::parse_search(&json("search.json"));
    let Discovered {
        mut ready,
        skipped,
        questions,
    } = discovery::discover(all, &cfg, TRAIN);
    let files = json("files.json");
    for (pr, _) in ready.iter_mut() {
        let key = pr.key.lookup_id();
        let entry = files
            .get(&key)
            .unwrap_or_else(|| panic!("no files fixture for {key}"));
        pr.files = github::parse_files(entry);
    }
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-28T10:00:00-03:00".to_string(),
            train: TRAIN.to_string(),
            deploy_branch: TRAIN.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: ready
                .into_iter()
                .map(|(pr, scope)| (pr, scope.as_str().to_string()))
                .collect(),
            skipped,
            questions,
            external: BTreeMap::new(),
            publishers: publishers::collect(&manifests()),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn without_gate(mut plan: Plan) -> Plan {
    plan.gate.clear();
    plan.items.retain(|i| i.id != GATE);
    plan
}

fn items_only(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| !l.starts_with('#')).collect()
}

fn position(lines: &[String], id: &str) -> usize {
    let wanted = format!("merge:{id}");
    lines
        .iter()
        .position(|l| *l == wanted)
        .unwrap_or_else(|| panic!("{id} not in the queue"))
}

#[test]
fn every_ready_pr_appears_once_and_none_skipped() {
    let plan = build();
    let lines = emit::queue_lines(&plan);
    let items = items_only(&lines);
    assert_eq!(plan.items.len(), 21);
    assert_eq!(items.len(), plan.items.len());
    for item in &plan.items {
        let wanted = format!("merge:{}", item.id);
        assert_eq!(items.iter().filter(|l| ***l == wanted).count(), 1);
    }
    assert!(plan.skipped.iter().any(|s| s.id == SKIPPED));
    assert!(!lines.iter().any(|l| l.contains(SKIPPED)));
}

#[test]
fn order_is_the_flattened_stages_and_respects_every_edge() {
    let plan = build();
    let lines = emit::queue_lines(&plan);
    let mut edges = 0;
    for item in &plan.items {
        for dep in &item.after {
            assert!(
                position(&lines, &dep.id) < position(&lines, &item.id),
                "{} must come before {}",
                dep.id,
                item.id
            );
            edges += 1;
        }
    }
    assert!(edges > 0);

    let rest: Vec<&str> = lines
        .iter()
        .skip_while(|l| !l.starts_with('#'))
        .skip(1)
        .map(|l| l.strip_prefix("merge:").expect("an item"))
        .collect();
    let stages: Vec<usize> = rest
        .iter()
        .map(|id| plan.items.iter().find(|i| i.id == *id).unwrap().stage)
        .collect();
    assert!(stages.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn a_gate_comes_first_then_the_pause_then_the_rest() {
    let plan = build();
    assert_eq!(plan.gate, vec![GATE.to_string()]);
    let lines = emit::queue_lines(&plan);
    assert_eq!(lines[0], format!("merge:{GATE}"));
    assert_eq!(lines[1], PAUSE_LINE);
    assert!(lines[2..].iter().all(|l| !l.starts_with('#')));
    assert_eq!(lines.len(), plan.items.len() + 1);
}

#[test]
fn without_a_gate_there_is_no_pause_line() {
    let plan = without_gate(build());
    let lines = emit::queue_lines(&plan);
    assert_eq!(lines.len(), 20);
    assert!(lines.iter().all(|l| !l.starts_with('#')));
}

#[test]
fn open_questions_give_exit_code_3_and_question_lines() {
    let mut plan = build();
    assert_eq!(emit::exit_code(&plan), 0);
    assert!(emit::question_lines(&plan).is_empty());

    plan.questions.push(Question {
        pr: Some("acme/svc-api#7".to_string()),
        code: "gate-has-predecessor".to_string(),
        message: "needs a decision".to_string(),
        hint: "decide".to_string(),
    });
    plan.questions.push(Question {
        pr: None,
        code: "tree-truncated".to_string(),
        message: "tree too large".to_string(),
        hint: "pin it".to_string(),
    });
    assert_eq!(emit::exit_code(&plan), 3);
    assert_eq!(
        emit::question_lines(&plan),
        vec![
            "? acme/svc-api#7: needs a decision".to_string(),
            "? (repo): tree too large".to_string(),
        ]
    );
    assert!(!emit::queue_lines(&plan).is_empty());
}

#[test]
fn an_empty_plan_has_no_queue_lines() {
    let mut plan = build();
    plan.items.clear();
    plan.gate.clear();
    assert!(emit::queue_lines(&plan).is_empty());
    assert_eq!(emit::exit_code(&plan), 0);
}

#[test]
fn a_gate_alone_has_no_trailing_pause() {
    let mut plan = build();
    plan.items.retain(|i| i.id == GATE);
    assert_eq!(emit::queue_lines(&plan), vec![format!("merge:{GATE}")]);
}

#[test]
fn every_item_matches_the_format() {
    let shape = Regex::new(r"^merge:[^/\s]+/[^#\s]+#\d+$").unwrap();
    let lines = emit::queue_lines(&build());
    for line in items_only(&lines) {
        assert!(shape.is_match(line), "{line}");
    }
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("merge:") || l.starts_with('#'))
    );
}
