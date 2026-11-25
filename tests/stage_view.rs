use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use pipetrain::config::{Config, Rules};
use pipetrain::discovery::{self, Discovered};
use pipetrain::github;
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers;
use serde_json::Value;

const TRAIN: &str = "deploy/28-10-2026";

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

fn build() -> Plan {
    let cfg = Config::parse(&fixture("config.toml")).expect("config");
    let rules = Rules::new(&cfg).expect("rules");
    let all = github::parse_search(&json("search.json"));
    assert_eq!(all.len(), 26);

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

    let manifests: Vec<(String, String, String)> = json("manifests.json")
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
        .collect();

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
            publishers: publishers::collect(&manifests),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn stage(plan: &Plan, id: &str) -> usize {
    plan.items.iter().find(|i| i.id == id).unwrap().stage
}

fn crowded_stage(plan: &Plan) -> (usize, Vec<String>) {
    let mut by_stage: BTreeMap<usize, Vec<&pipetrain::planner::Item>> = BTreeMap::new();
    for item in plan.items.iter().filter(|i| i.gate.is_none() && !i.hidden) {
        by_stage.entry(item.stage).or_default().push(item);
    }
    let (stage, items) = by_stage
        .into_iter()
        .find(|(_, items)| items.len() >= 3)
        .expect("a stage with three PRs");
    let repos = items
        .iter()
        .map(|i| format!("{}/{}", i.org, i.repo).to_lowercase())
        .collect();
    (stage, repos)
}

#[test]
fn every_running_pr_appears_once_per_stage_as_together_or_held() {
    let plan = build();
    let running = plan
        .items
        .iter()
        .filter(|i| i.gate.is_none() && !i.hidden)
        .count();
    let listed: usize = plan
        .stages
        .iter()
        .map(|view| view.together.len() + view.held.len())
        .sum();
    assert_eq!(listed, running);
    for view in &plan.stages {
        for id in view.together.iter().chain(view.held.iter().map(|w| &w.id)) {
            assert_eq!(stage(&plan, id), view.stage, "{id}");
        }
    }
}

#[test]
fn without_detected_resources_nothing_is_held_and_the_peak_matches() {
    let plan = build();
    assert!(plan.stages.iter().all(|view| view.held.is_empty()));
    let widest = plan.stages.iter().map(|v| v.together.len()).max().unwrap();
    assert_eq!(widest, plan.stats.peak_parallel);
    assert_eq!(
        plan.stats.effective_parallel,
        plan.stats.peak_parallel.min(plan.stats.max_parallel)
    );
}

#[test]
fn waits_name_the_gate_the_package_and_the_repo() {
    let plan = build();
    let waits: Vec<_> = plan.stages.iter().flat_map(|v| v.waits.iter()).collect();
    let package = waits
        .iter()
        .find(|w| w.id == "acme/bff-appointments#44" && w.reason == "package")
        .expect("package wait");
    assert!(package.behind.contains(&"acme/svc-auth#39".to_string()));
    assert!(!package.detail.is_empty());
    let same_repo = waits
        .iter()
        .find(|w| w.id == "acme/connector#88" && w.reason == "same-repo")
        .expect("same repo wait");
    assert_eq!(same_repo.behind, vec!["acme/connector#87".to_string()]);
    assert_eq!(same_repo.detail, "acme/connector");
}

#[test]
fn a_shared_deploy_resource_serializes_a_stage_and_is_named() {
    let mut plan = build();
    let before = plan.stats.effective_parallel;
    let (stage, repos) = crowded_stage(&plan);
    for repo in &repos {
        plan.deploy_resources
            .insert(repo.clone(), vec!["runner:shared".to_string()]);
    }
    plan.refresh_stages(&BTreeMap::new());
    let view = plan.stages.iter().find(|v| v.stage == stage).unwrap();
    assert_eq!(view.together.len(), 1, "{view:?}");
    assert!(view.held.len() >= 2);
    for held in &view.held {
        assert_eq!(held.reason, "deploy-resource");
        assert_eq!(held.detail, "runner:shared");
        assert_eq!(held.behind, view.together);
    }
    assert!(plan.stats.effective_parallel <= before);
    assert!(plan.stats.effective_parallel <= plan.stats.peak_parallel);

    let text = pipetrain::render::text(&plan);
    assert!(
        text.contains("shared deploy resource runner:shared"),
        "{text}"
    );
    assert!(text.contains("serial:"), "{text}");
}

#[test]
fn configured_slots_let_the_resource_users_run_together() {
    let mut plan = build();
    let (stage, repos) = crowded_stage(&plan);
    for repo in &repos {
        plan.deploy_resources
            .insert(repo.clone(), vec!["runner:shared".to_string()]);
    }
    let slots = BTreeMap::from([("shared".to_string(), repos.len())]);
    plan.refresh_stages(&slots);
    let view = plan.stages.iter().find(|v| v.stage == stage).unwrap();
    assert!(view.held.is_empty(), "{view:?}");
}

#[test]
fn json_only_adds_fields_and_keeps_the_peak_coherent() {
    let mut plan = build();
    let (_, repos) = crowded_stage(&plan);
    for repo in &repos {
        plan.deploy_resources
            .insert(repo.clone(), vec!["runner:shared".to_string()]);
    }
    plan.refresh_stages(&BTreeMap::new());
    let value = serde_json::to_value(&plan).unwrap();
    let stages = value["stages"].as_array().expect("stages");
    assert!(!stages.is_empty());
    for view in stages {
        for key in ["stage", "together", "waits", "held"] {
            assert!(view.get(key).is_some(), "stage misses {key}");
        }
    }
    let stats = &value["stats"];
    assert!(stats["effective_parallel"].as_u64() <= stats["peak_parallel"].as_u64());
    assert!(stats["peak_parallel"].is_u64());
    assert!(stats["max_parallel"].is_u64());
}

#[test]
fn a_gate_pr_is_listed_in_its_stage_apart_and_counted_only_if_confirmed() {
    let plan = build();
    let gate = plan
        .items
        .iter()
        .find(|i| i.id == "acme/svc-billing#21")
        .unwrap();
    let view = plan.stages.iter().find(|v| v.stage == gate.stage).unwrap();
    let note = view
        .gates
        .iter()
        .find(|g| g.id == gate.id)
        .expect("gate note");
    assert_eq!(note.kind, "DDL");
    assert_eq!(
        note.note,
        "runs with this stage once confirmed at --apply start"
    );
    assert!(plan.stages.iter().all(|v| !v.together.contains(&gate.id)));

    let widest = plan.stages.iter().map(|v| v.together.len()).max().unwrap();
    assert_eq!(
        plan.stats.effective_parallel,
        widest.min(plan.stats.max_parallel)
    );
    let with_gates = plan
        .stages
        .iter()
        .map(|v| v.together.len() + v.gates.len())
        .max()
        .unwrap();
    assert_eq!(
        plan.stats.effective_parallel_if_gates_confirmed,
        with_gates.min(plan.stats.max_parallel)
    );
    assert!(plan.stats.effective_parallel_if_gates_confirmed >= plan.stats.effective_parallel);

    let value = serde_json::to_value(&plan).unwrap();
    assert!(value["stats"]["effective_parallel_if_gates_confirmed"].is_u64());
    let text = pipetrain::render::text(&plan);
    assert!(text.contains("gate DDL: "), "{text}");
}
