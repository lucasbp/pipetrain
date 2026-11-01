use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{ChangedFile, Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;
use pipetrain::render;

const DEPLOY: &str = "deploy/30-10-2026";

fn pr(slug: &str, number: u64, head: &str, paths: &[&str]) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{slug}/pull/{number}"),
        body: String::new(),
        head_ref: head.to_string(),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review: Review::Approved,
        checks: Checks::Success,
        failed_checks: Vec::new(),
        conflicting: false,
        mergeable_unknown: false,
        files: paths
            .iter()
            .map(|p| ChangedFile {
                path: p.to_string(),
                patch: None,
            })
            .collect(),
        files_complete: true,
    }
}

fn build(prs: Vec<Pr>) -> Plan {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    plan::build(
        &rules,
        Gathered {
            generated_at: "2026-10-30T10:00:00-03:00".to_string(),
            train: DEPLOY.to_string(),
            deploy_branch: DEPLOY.to_string(),
            train_prefix: "deploy/".to_string(),
            orgs: cfg.orgs.clone(),
            ready: prs
                .into_iter()
                .map(|p| (p, "explicit".to_string()))
                .collect(),
            skipped: Vec::new(),
            questions: Vec::new(),
            external: BTreeMap::new(),
            publishers: Publishers::new(),
            references: Default::default(),
            max_parallel: cfg.max_parallel,
            requests: 0,
            rate_remaining: None,
        },
    )
}

fn overlapping() -> Vec<Pr> {
    vec![
        pr(
            "acme/api-core",
            1,
            "feat/a",
            &["src/a.rs", "src/b.rs", "x.rs", "y.rs"],
        ),
        pr(
            "acme/api-core",
            2,
            "feat/b",
            &["src/a.rs", "src/b.rs", "x.rs", "y.rs", "z.rs"],
        ),
        pr("acme/api-auth", 7, "feat/c", &["src/a.rs"]),
    ]
}

fn disjoint() -> Vec<Pr> {
    vec![
        pr("acme/api-core", 1, "feat/a", &["src/a.rs"]),
        pr("acme/api-core", 2, "feat/b", &["src/b.rs"]),
        pr("acme/api-auth", 7, "feat/c", &["src/a.rs"]),
    ]
}

fn without_conflicts(mut plan: Plan) -> Plan {
    plan.conflicts.clear();
    plan
}

fn merges(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("merge:")).collect()
}

#[test]
fn plan_json_always_carries_conflicts_as_an_array() {
    let empty = serde_json::to_value(build(disjoint())).unwrap();
    assert_eq!(empty["conflicts"], serde_json::json!([]));
    assert_eq!(empty["schema"], 1);

    let filled = serde_json::to_value(build(overlapping())).unwrap();
    let conflicts = filled["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["a"], "acme/api-core#1");
    assert_eq!(conflicts[0]["b"], "acme/api-core#2");
    assert_eq!(conflicts[0]["repo"], "acme/api-core");
    assert_eq!(conflicts[0]["kind"], "other");
    assert_eq!(
        conflicts[0]["files"],
        serde_json::json!(["src/a.rs", "src/b.rs", "x.rs", "y.rs"])
    );
}

#[test]
fn a_conflict_line_comes_before_the_merges_and_the_merges_are_unchanged() {
    let with = emit::queue_lines(&build(overlapping()));
    let plain = emit::queue_lines(&without_conflicts(build(overlapping())));

    assert_eq!(
        with[0],
        "# conflict: acme/api-core#1 and acme/api-core#2 both change 4 files (other)"
    );
    let first_merge = with.iter().position(|l| l.starts_with("merge:")).unwrap();
    let first_bundle = with
        .iter()
        .position(|l| l.starts_with("# bundle "))
        .unwrap();
    assert!(first_merge > 0);
    assert!(0 < first_bundle);
    assert_eq!(merges(&with), merges(&plain));
    assert_eq!(with[1..], plain[..]);
}

#[test]
fn without_conflicts_the_queue_is_identical() {
    let plan = build(disjoint());
    assert!(plan.conflicts.is_empty());
    let lines = emit::queue_lines(&plan);
    assert!(lines.iter().all(|l| !l.starts_with("# conflict:")));
    assert_eq!(
        lines,
        emit::queue_lines(&without_conflicts(build(disjoint())))
    );
}

#[test]
fn render_shows_conflicts_only_when_there_are_some() {
    let with = anstream::adapter::strip_str(&render::text(&build(overlapping()))).to_string();
    assert!(with.contains("CONFLICTS"), "{with}");
    assert!(
        with.contains("api-core#1 and api-core#2  4 files (other)"),
        "{with}"
    );
    assert!(with.contains("src/a.rs, src/b.rs, x.rs, +1 more"), "{with}");

    let without = anstream::adapter::strip_str(&render::text(&build(disjoint()))).to_string();
    assert!(!without.contains("CONFLICTS"), "{without}");
}
