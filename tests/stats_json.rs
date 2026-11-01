use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::plan::{self, Gathered};
use pipetrain::publishers::Publishers;

fn built_json() -> String {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let built = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: Vec::new(),
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 3,
            rate_remaining: Some(4000),
        },
    );
    serde_json::to_string(&built).unwrap()
}

#[test]
fn cache_counters_start_at_zero() {
    let value: serde_json::Value = serde_json::from_str(&built_json()).unwrap();
    assert_eq!(value["stats"]["cache_hits"], 0);
    assert_eq!(value["stats"]["cache_misses"], 0);
}

#[test]
fn stats_keys_keep_their_order_and_the_new_ones_come_last() {
    let json = built_json();
    let stats = &json[json.find("\"stats\"").unwrap()..];
    let keys = [
        "requests",
        "rate_remaining",
        "peak_parallel",
        "max_parallel",
        "deploys_saved",
        "deploys_now",
        "deploys_after",
        "cache_hits",
        "cache_misses",
    ];
    let positions: Vec<usize> = keys
        .iter()
        .map(|key| {
            stats
                .find(&format!("\"{key}\""))
                .unwrap_or_else(|| panic!("missing {key}"))
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
}
