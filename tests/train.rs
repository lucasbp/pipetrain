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

fn has_edge(plan: &Plan, from: &str, to: &str, rule: &str) -> bool {
    let item = plan
        .items
        .iter()
        .find(|i| i.id == to)
        .unwrap_or_else(|| panic!("{to} is not in the plan"));
    item.after
        .iter()
        .any(|d| d.id == from && d.rule.as_str() == rule)
}

fn stage(plan: &Plan, id: &str) -> usize {
    plan.items.iter().find(|i| i.id == id).unwrap().stage
}

#[test]
fn discovers_the_train_and_its_stacked_prs() {
    let plan = build();
    assert_eq!(plan.items.len(), 21);
    let in_train = plan.items.iter().filter(|i| i.scope == "train").count();
    assert_eq!(in_train, 13);
    let repos: std::collections::BTreeSet<&str> = plan
        .items
        .iter()
        .filter(|i| i.scope == "train")
        .map(|i| i.repo.as_str())
        .collect();
    assert_eq!(repos.len(), 13);
    let shared = plan
        .items
        .iter()
        .filter(|i| i.scope == "shared-branch")
        .count();
    assert_eq!(shared, 4);
    let stacked = plan
        .items
        .iter()
        .filter(|i| i.scope == "into-train")
        .count();
    assert_eq!(stacked, 4);
}

#[test]
fn leaves_out_noise_and_explains_the_one_skipped() {
    let plan = build();
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].id, "acme/svc-legacy#12");
    assert!(
        plan.skipped[0]
            .reasons
            .contains(&"changes requested".to_string())
    );
    assert!(
        plan.skipped[0]
            .reasons
            .contains(&"checks failing".to_string())
    );
    for ignored in [
        "acme/front-hub#41",
        "acme/front-portal#40",
        "acme/svc-auth#40",
        "acme/bff-hub#28",
    ] {
        assert!(
            plan.items.iter().all(|i| i.id != ignored),
            "{ignored} leaked in"
        );
    }
}

#[test]
fn library_goes_before_the_services_that_bump_it() {
    let plan = build();
    assert!(has_edge(
        &plan,
        "acme/svc-auth#39",
        "acme/bff-appointments#44",
        "package"
    ));
    assert!(has_edge(
        &plan,
        "acme/svc-auth#39",
        "acme/bff-hub#27",
        "package"
    ));
    assert!(has_edge(
        &plan,
        "acme/front-portal#39",
        "acme/front-hub#40",
        "package"
    ));
    assert!(has_edge(
        &plan,
        "acme/front-portal#39",
        "acme/front-appointments#35",
        "package"
    ));
    assert!(!has_edge(
        &plan,
        "acme/front-portal#39",
        "acme/front-reports#44",
        "package"
    ));
}

#[test]
fn feature_prs_go_into_the_train_branch_before_the_train_pr() {
    let plan = build();
    for n in [87, 88, 89, 90] {
        let from = format!("acme/connector#{n}");
        assert!(
            has_edge(&plan, &from, "acme/connector#91", "stacked"),
            "{from}"
        );
    }
    assert!(has_edge(
        &plan,
        "acme/svc-confirm#20",
        "acme/svc-confirm#22",
        "stacked"
    ));
    assert!(has_edge(
        &plan,
        "acme/svc-confirm#21",
        "acme/svc-confirm#22",
        "stacked"
    ));
    assert!(has_edge(
        &plan,
        "acme/bff-appointments#42",
        "acme/bff-appointments#44",
        "stacked"
    ));
    assert!(has_edge(
        &plan,
        "acme/front-portal#38",
        "acme/front-portal#39",
        "stacked"
    ));
}

#[test]
fn same_target_goes_in_series_by_number() {
    let plan = build();
    assert!(has_edge(
        &plan,
        "acme/connector#87",
        "acme/connector#88",
        "target"
    ));
    assert!(has_edge(
        &plan,
        "acme/connector#88",
        "acme/connector#89",
        "target"
    ));
    assert!(has_edge(
        &plan,
        "acme/connector#89",
        "acme/connector#90",
        "target"
    ));
    assert!(has_edge(
        &plan,
        "acme/svc-confirm#20",
        "acme/svc-confirm#21",
        "target"
    ));
}

#[test]
fn ddl_is_a_manual_pause_before_everything() {
    let plan = build();
    assert_eq!(plan.gate, vec!["acme/svc-billing#21".to_string()]);
    let gate = plan
        .items
        .iter()
        .find(|i| i.id == "acme/svc-billing#21")
        .unwrap();
    assert!(gate.manual_pause);
    assert!(gate.lane.is_none());
    for item in plan.items.iter().filter(|i| !i.manual_pause) {
        assert!(
            has_edge(&plan, "acme/svc-billing#21", &item.id, "gate"),
            "{} does not wait for the DDL",
            item.id
        );
    }
}

#[test]
fn layers_order_the_train_from_client_to_front() {
    let plan = build();
    assert!(has_edge(
        &plan,
        "acme/connector#91",
        "acme/svc-auth#39",
        "layer"
    ));
    assert!(has_edge(
        &plan,
        "acme/svc-confirm#22",
        "acme/bff-hub#27",
        "layer"
    ));
    assert!(has_edge(
        &plan,
        "acme/bff-hub#27",
        "acme/front-hub#40",
        "layer"
    ));
    assert!(has_edge(
        &plan,
        "acme/connector#90",
        "acme/bff-appointments#42",
        "layer"
    ));
    assert!(has_edge(
        &plan,
        "acme/svc-confirm#20",
        "acme/front-portal#38",
        "layer"
    ));
}

#[test]
fn nothing_is_left_undecided_and_every_edge_goes_forward() {
    let plan = build();
    assert!(plan.questions.is_empty(), "{:?}", plan.questions);
    for item in &plan.items {
        for dep in item.after.iter().filter(|d| d.rule.as_str() != "gate") {
            assert!(
                stage(&plan, &dep.id) < item.stage,
                "{} must come after {}",
                item.id,
                dep.id
            );
        }
        if !item.manual_pause {
            assert!(item.lane.is_some(), "{} has no lane", item.id);
        }
    }
    let in_lanes: usize = plan.lanes.iter().map(|l| l.len()).sum();
    assert_eq!(in_lanes + plan.gate.len(), plan.items.len());
}

#[test]
fn stacked_train_has_nothing_to_bundle() {
    let plan = build();
    assert!(plan.bundles.is_empty(), "{:?}", plan.bundles);
    assert_eq!(plan.stats.deploys_saved, 0);
    assert_eq!(plan.stats.deploys_now, plan.stats.deploys_after);
}

#[test]
fn json_output_keeps_the_documented_shape() {
    let plan = build();
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(value["schema"], 1);
    for key in [
        "train",
        "deploy_branch",
        "orgs",
        "items",
        "gate",
        "lanes",
        "bundles",
        "skipped",
        "questions",
        "notes",
        "stats",
    ] {
        assert!(value.get(key).is_some(), "missing {key}");
    }
    for key in ["deploys_saved", "deploys_now", "deploys_after"] {
        assert!(value["stats"].get(key).is_some(), "stats miss {key}");
    }
    let item = &value["items"][0];
    for key in [
        "id",
        "org",
        "repo",
        "number",
        "title",
        "url",
        "head",
        "base",
        "sha",
        "layer",
        "scope",
        "gate",
        "gate_reason",
        "manual_pause",
        "stage",
        "lane",
        "after",
    ] {
        assert!(item.get(key).is_some(), "item misses {key}");
    }
}
