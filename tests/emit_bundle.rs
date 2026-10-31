use std::collections::BTreeMap;

use pipetrain::config::{Config, Rules};
use pipetrain::emit;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::plan::{self, Gathered, Plan};
use pipetrain::publishers::Publishers;

const DEPLOY: &str = "deploy/30-10-2026";

fn pr(slug: &str, number: u64, head: &str, base: &str) -> Pr {
    let (org, repo) = slug.split_once('/').unwrap();
    Pr {
        key: PrKey::new(org, repo, number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("github.com/{slug}/pull/{number}"),
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

fn without_bundles(mut plan: Plan) -> Plan {
    plan.bundles.clear();
    plan
}

fn merges(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.starts_with("merge:")).collect()
}

fn bundles(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.starts_with("# bundle "))
        .collect()
}

#[test]
fn a_bundle_line_comes_before_the_first_merge_and_merges_are_unchanged() {
    let prs = || {
        vec![
            pr("acme/api-core", 1, "feat/a", "master"),
            pr("acme/api-core", 2, "feat/b", "master"),
            pr("acme/api-auth", 7, "feat/c", "master"),
        ]
    };
    let with = emit::queue_lines(&build(prs()));
    let plain = emit::queue_lines(&without_bundles(build(prs())));

    assert_eq!(
        with[0],
        "# bundle acme/api-core: 2 PRs into master could be 1 deploy (saves 1): acme/api-core#1 acme/api-core#2"
    );
    assert_eq!(bundles(&with).len(), 1);
    let first_merge = with.iter().position(|l| l.starts_with("merge:")).unwrap();
    assert_eq!(first_merge, 1);
    assert_eq!(merges(&with), merges(&plain));
    assert_eq!(with[1..], plain[..]);
}

#[test]
fn without_a_bundle_the_queue_is_identical() {
    let prs = || {
        vec![
            pr("acme/api-core", 1, "feat/a", "master"),
            pr("acme/api-auth", 7, "feat/c", "master"),
        ]
    };
    let plan = build(prs());
    assert!(plan.bundles.is_empty());
    let lines = emit::queue_lines(&plan);
    assert_eq!(lines, emit::queue_lines(&without_bundles(build(prs()))));
    assert!(bundles(&lines).is_empty());
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.starts_with("merge:")));
}

#[test]
fn two_bundles_come_out_in_a_stable_order() {
    let prs = || {
        vec![
            pr("acme/api-core", 1, "feat/a", "master"),
            pr("acme/api-core", 2, "feat/b", "master"),
            pr("acme/api-auth", 3, "feat/c", "master"),
            pr("acme/api-auth", 4, "feat/d", "master"),
            pr("acme/api-auth", 5, DEPLOY, "master"),
        ]
    };
    let plan = build(prs());
    let lines = emit::queue_lines(&plan);
    let found = bundles(&lines);
    assert_eq!(found.len(), 2);
    assert_eq!(
        *found[0],
        "# bundle acme/api-auth: 3 PRs into master could be 1 deploy (saves 2): acme/api-auth#5 acme/api-auth#3 acme/api-auth#4"
    );
    assert_eq!(
        *found[1],
        "# bundle acme/api-core: 2 PRs into master could be 1 deploy (saves 1): acme/api-core#1 acme/api-core#2"
    );
    assert!(lines[..2].iter().all(|l| l.starts_with("# bundle ")));
    assert_eq!(lines, emit::queue_lines(&build(prs())));
}
