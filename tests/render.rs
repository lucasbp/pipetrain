use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered};
use pipetrain::publishers::Publishers;
use pipetrain::render;

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

fn sample() -> String {
    sample_with(0)
}

fn sample_with(elapsed_ms: u64) -> String {
    let cfg = Config::parse(
        "orgs = [\"acme\"]\n[[layer]]\nname = \"api\"\nrepos = [\"acme/api-*\"]\n[[layer]]\nname = \"front\"\nrepos = [\"acme/front-*\"]\n",
    )
    .unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let mut migration = pr("acme/api-core", 1, "deploy/30-10-2026", "master");
    migration.files = vec![ChangedFile {
        path: "db/migrations/001.sql".to_string(),
        patch: None,
    }];
    let front = pr("acme/front-web", 2, "deploy/30-10-2026", "master");
    let feature_a = pr("acme/api-other", 3, "feat/shared", "master");
    let feature_b = pr("acme/front-other", 4, "feat/shared", "master");
    let mut built = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: vec![
                (migration, "train".to_string()),
                (front, "train".to_string()),
                (feature_a, "shared-branch".to_string()),
                (feature_b, "shared-branch".to_string()),
            ],
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
    built.stats.elapsed_ms = elapsed_ms;
    render::text(&built)
}

#[test]
fn output_is_colored_and_plain_text_is_intact_after_stripping() {
    let colored = sample();
    assert!(colored.contains("\x1b["));
    let plain = anstream::adapter::strip_str(&colored).to_string();
    assert!(!plain.contains('\x1b'));
    for needle in [
        "pipetrain plan",
        "WHERE IT CAME FROM",
        "ORDER",
        "GATE",
        "LANE 1",
        "api-core#1",
        "feat/shared",
    ] {
        assert!(plain.contains(needle), "missing `{needle}` in:\n{plain}");
    }
    assert!(!plain.contains("acme/api-core#1"));
}

#[test]
fn the_time_line_follows_the_api_line_with_the_gathering_time() {
    let plain = anstream::adapter::strip_str(&sample_with(14_600)).to_string();
    let lines: Vec<&str> = plain.lines().collect();
    let api = lines
        .iter()
        .position(|line| line.trim_start().starts_with("api "))
        .unwrap();
    assert_eq!(lines[api + 1], "  time      14.6 s", "{plain}");
    assert!(plain.contains("time      14.6 s"));
}

#[test]
fn the_time_line_switches_to_minutes_after_a_minute() {
    let plain = anstream::adapter::strip_str(&sample_with(65_000)).to_string();
    assert!(plain.contains("time      1m 05s"), "{plain}");
}

fn late_gate_text() -> String {
    let cfg =
        Config::parse("orgs = [\"acme\"]\n[packages]\n\"@acme/kit\" = \"acme/kit\"\n").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let publisher = pr("acme/kit", 6, "feat/kit", "master");
    let mut gate = pr("acme/web", 5, "feat/gate", "master");
    gate.files = vec![
        ChangedFile {
            path: "db/migrations/001.sql".to_string(),
            patch: None,
        },
        ChangedFile {
            path: "package.json".to_string(),
            patch: Some(
                "-    \"@acme/kit\": \"^1.0.0\",\n+    \"@acme/kit\": \"^1.1.0\",".to_string(),
            ),
        },
    ];
    let built = plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T09:00:00-03:00".to_string(),
            train: "deploy/30-10-2026".to_string(),
            deploy_branch: "deploy/30-10-2026".to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: vec!["acme".to_string()],
            ready: vec![
                (publisher, "explicit".to_string()),
                (gate, "explicit".to_string()),
            ],
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: 4,
            requests: 0,
            rate_remaining: None,
        },
    );
    anstream::adapter::strip_str(&render::text(&built)).to_string()
}

#[test]
fn a_late_gate_section_says_it_runs_after_its_predecessors() {
    let plain = late_gate_text();
    let gate_at = plain.find("GATE").unwrap();
    assert!(plain[gate_at..].contains("runs after"), "{plain}");
    assert!(plain[gate_at..].contains("kit#6"), "{plain}");
    assert!(!plain.contains("runs first, pauses"), "{plain}");
}

#[test]
fn a_late_gate_sits_in_a_step_of_the_order_map_and_leaves_no_top_pause_line() {
    let plain = late_gate_text();
    let order = plain.find("ORDER").unwrap();
    let section: Vec<&str> = plain[order..].lines().collect();
    assert!(
        !section
            .iter()
            .any(|line| line.trim_start().starts_with("pause ")),
        "{plain}"
    );
    let step = section
        .iter()
        .find(|line| line.contains("step") && line.contains("web#5"))
        .unwrap_or_else(|| panic!("no step with the gate in:\n{plain}"));
    assert!(step.contains("(pause)"), "{step}");
    assert!(step.contains("DDL"), "{step}");
}

#[test]
fn an_early_gate_keeps_its_pause_line_in_the_order_map() {
    let plain = anstream::adapter::strip_str(&sample()).to_string();
    let order = plain.find("ORDER").unwrap();
    let section: Vec<&str> = plain[order..].lines().collect();
    assert!(
        section
            .iter()
            .any(|line| line.trim_start().starts_with("pause ")),
        "{plain}"
    );
    assert!(!plain[order..].contains("(pause)"), "{plain}");
}

#[test]
fn order_map_lists_the_pause_before_the_steps() {
    let plain = anstream::adapter::strip_str(&sample()).to_string();
    let order = plain.find("ORDER").unwrap();
    let pause = plain[order..].find("pause").unwrap();
    let step = plain[order..].find("step 1").unwrap();
    assert!(pause < step);
}
