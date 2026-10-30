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
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        conflicting: false,
        mergeable_unknown: false,
        files: Vec::new(),
        files_complete: true,
    }
}

fn sample() -> String {
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
    let built = plan::build(
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
fn order_map_lists_the_pause_before_the_steps() {
    let plain = anstream::adapter::strip_str(&sample()).to_string();
    let order = plain.find("ORDER").unwrap();
    let pause = plain[order..].find("pause").unwrap();
    let step = plain[order..].find("step 1").unwrap();
    assert!(pause < step);
}
