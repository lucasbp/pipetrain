use pipetrain::config::{Config, Rules};

fn example_text() -> String {
    let path = format!("{}/pipetrain.example.toml", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).expect("example config readable")
}

#[test]
fn example_config_parses() {
    let cfg = Config::parse(&example_text()).expect("example config parses");
    assert!(cfg.layer.len() >= 4);
    assert_eq!(cfg.api.max_requests, 300);
    assert_eq!(cfg.api.max_per_minute, 100);
    assert_eq!(cfg.api.min_remaining, 200);
}

#[test]
fn example_config_builds_rules() {
    let cfg = Config::parse(&example_text()).expect("example config parses");
    let rules = Rules::new(&cfg).expect("rules build from example config");
    assert!(rules.layer_of("acme/api-core").is_some());
}
