use std::collections::BTreeMap;

mod common;

use common::pr;
use pipetrain::bundle::{self, Bundle};
use pipetrain::config::{Config, Rules};
use pipetrain::model::Pr;
use pipetrain::planner::{self, PlanInput};
use pipetrain::publishers::Publishers;

const DEPLOY: &str = "deploy/05-11-2026";

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
        references: Default::default(),
    });
    bundle::suggest(&planned.items, DEPLOY, "deploy/")
}

fn of<'a>(bundles: &'a [Bundle], repo: &str, base: &str) -> &'a Bundle {
    bundles
        .iter()
        .find(|b| b.repo == repo && b.base == base)
        .unwrap_or_else(|| panic!("no bundle for {repo} on {base}: {bundles:?}"))
}

#[test]
fn a_repo_with_two_bases_names_each_branch_after_its_base() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "feature-x"),
        pr("acme/api-core", 4, "feat/d", "feature-x"),
    ]);
    assert_eq!(bundles.len(), 2);
    assert_eq!(
        of(&bundles, "acme/api-core", "master").branch,
        "deploy/05-11-2026-master"
    );
    assert_eq!(
        of(&bundles, "acme/api-core", "feature-x").branch,
        "deploy/05-11-2026-feature-x"
    );
}

#[test]
fn a_repo_with_one_base_keeps_the_plain_branch_next_to_a_split_repo() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "feature-x"),
        pr("acme/api-core", 4, "feat/d", "feature-x"),
        pr("acme/web", 1, "feat/a", "main"),
        pr("acme/web", 2, "feat/b", "main"),
    ]);
    assert_eq!(bundles.len(), 3);
    assert_eq!(of(&bundles, "acme/web", "main").branch, DEPLOY);
    assert_ne!(of(&bundles, "acme/api-core", "master").branch, DEPLOY);
    assert_ne!(of(&bundles, "acme/api-core", "feature-x").branch, DEPLOY);
}

#[test]
fn a_group_of_one_pr_does_not_force_a_suffix() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "feature-x"),
    ]);
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].base, "master");
    assert_eq!(bundles[0].branch, DEPLOY);
}

#[test]
fn the_base_suffix_is_flat_and_lowercase() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "Feature/New_Thing"),
        pr("acme/api-core", 4, "feat/d", "Feature/New_Thing"),
    ]);
    assert_eq!(
        of(&bundles, "acme/api-core", "Feature/New_Thing").branch,
        "deploy/05-11-2026-feature-new_thing"
    );
}

#[test]
fn an_open_pr_from_the_suffixed_branch_is_the_existing_train_pr() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "feature-x"),
        pr("acme/api-core", 4, "feat/d", "feature-x"),
        pr(
            "acme/api-core",
            5,
            "deploy/05-11-2026-feature-x",
            "feature-x",
        ),
    ]);
    let split = of(&bundles, "acme/api-core", "feature-x");
    assert_eq!(split.branch, "deploy/05-11-2026-feature-x");
    assert_eq!(split.existing_train_pr.as_deref(), Some("acme/api-core#5"));
    assert_eq!(split.retarget.len(), 2);
    assert!(!split.retarget.contains(&"acme/api-core#5".to_string()));
    let other = of(&bundles, "acme/api-core", "master");
    assert_eq!(other.existing_train_pr, None);
    assert_eq!(other.retarget.len(), 2);
}

#[test]
fn a_pr_from_the_plain_train_branch_is_not_the_train_pr_of_a_split_repo() {
    let bundles = suggest(vec![
        pr("acme/api-core", 1, "feat/a", "master"),
        pr("acme/api-core", 2, "feat/b", "master"),
        pr("acme/api-core", 3, "feat/c", "feature-x"),
        pr("acme/api-core", 4, "feat/d", "feature-x"),
        pr("acme/api-core", 5, DEPLOY, "master"),
    ]);
    let master = of(&bundles, "acme/api-core", "master");
    assert_eq!(master.branch, "deploy/05-11-2026-master");
    assert_eq!(master.existing_train_pr, None);
    assert_eq!(master.retarget.len(), 3);
}

#[test]
fn train_branches_are_the_date_branch_and_its_suffixed_siblings() {
    assert!(bundle::is_train_branch(DEPLOY, DEPLOY));
    assert!(bundle::is_train_branch("deploy/05-11-2026-x", DEPLOY));
    assert!(bundle::is_train_branch(
        "deploy/05-11-2026-feature-x",
        DEPLOY
    ));
    assert!(bundle::is_train_branch(
        "Deploy/05-11-2026-Feature-X",
        DEPLOY
    ));
    assert!(!bundle::is_train_branch("deploy/05-11-2026x", DEPLOY));
    assert!(!bundle::is_train_branch("deploy/06-11-2026", DEPLOY));
    assert!(!bundle::is_train_branch("feature-x", DEPLOY));
}
