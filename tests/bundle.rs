use std::collections::BTreeMap;

use pipetrain::bundle::{self, Bundle};
use pipetrain::config::{Config, Rules};
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::planner::{self, PlanInput};
use pipetrain::publishers::Publishers;

const DEPLOY: &str = "deploy/30-10-2026";

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

fn suggest(prs: Vec<Pr>) -> Vec<Bundle> {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    let rules = Rules::new(&cfg).unwrap();
    let planned = planner::plan(PlanInput {
        rules: &rules,
        prs: prs
            .into_iter()
            .map(|p| (p, "explicit".to_string()))
            .collect(),
        external: BTreeMap::new(),
        publishers: Publishers::new(),
    });
    bundle::suggest(&planned.items, DEPLOY, "deploy/")
}

#[test]
fn three_prs_into_master_become_one_deploy() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "master"),
    ]);
    assert_eq!(bundles.len(), 1);
    let b = &bundles[0];
    assert_eq!(b.repo, "acme/api-core");
    assert_eq!(b.base, "master");
    assert_eq!(b.branch, DEPLOY);
    assert_eq!(b.existing_train_pr, None);
    assert_eq!(b.retarget.len(), 3);
    assert_eq!(b.deploys_now, 3);
    assert_eq!(b.deploys_after, 1);
    assert_eq!(b.saved, 2);
}

#[test]
fn existing_train_pr_carries_the_others() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "master"),
        pr("acme/api-core", 4, DEPLOY, "master"),
    ]);
    assert_eq!(bundles.len(), 1);
    let b = &bundles[0];
    assert_eq!(b.existing_train_pr.as_deref(), Some("acme/api-core#4"));
    assert_eq!(b.retarget.len(), 3);
    assert!(!b.retarget.contains(&"acme/api-core#4".to_string()));
    assert_eq!(b.deploys_now, 4);
    assert_eq!(b.saved, 3);
}

#[test]
fn prs_already_stacked_on_the_train_are_left_alone() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", DEPLOY),
        pr("acme/api-core", 2, "feat/b", DEPLOY),
    ]);
    assert!(bundles.is_empty());
}

#[test]
fn a_lone_pr_has_nothing_to_bundle() {
    let bundles = suggest(vec![pr("acme/api-core", 1, "feat/a", "master")]);
    assert!(bundles.is_empty());
}

#[test]
fn each_repo_gets_its_own_bundle() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-auth", 1, "feat/a", "master"),
        pr("acme/api-auth", 2, "feat/b", "master"),
    ]);
    assert_eq!(bundles.len(), 2);
    let repos: Vec<&str> = bundles.iter().map(|b| b.repo.as_str()).collect();
    assert!(repos.contains(&"acme/api-core"));
    assert!(repos.contains(&"acme/api-auth"));
    assert!(bundles.iter().all(|b| b.saved == 1));
}
