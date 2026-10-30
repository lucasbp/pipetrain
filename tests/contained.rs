use pipetrain::contained::{self, Target};
use pipetrain::discovery::Scope;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use serde_json::json;

const TRAIN: &str = "deploy/29-10-2026";

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

fn target(slug: &str, reference: &str, sha: &str) -> Target {
    Target {
        slug: slug.to_string(),
        reference: reference.to_string(),
        sha: sha.to_string(),
    }
}

fn compare(ahead: u64, behind: u64, status: &str) -> serde_json::Value {
    json!({ "ref": { "compare": { "aheadBy": ahead, "behindBy": behind, "status": status } } })
}

#[test]
fn parse_reads_ahead_by_per_alias() {
    let response = json!({
        "data": {
            "t0": compare(0, 4, "BEHIND"),
            "t1": compare(3, 0, "AHEAD"),
            "t2": { "ref": null }
        }
    });
    assert_eq!(
        contained::parse(&response, 4),
        vec![true, false, false, false]
    );
}

#[test]
fn parse_treats_identical_as_contained() {
    let response = json!({ "data": { "t0": compare(0, 0, "IDENTICAL") } });
    assert_eq!(contained::parse(&response, 1), vec![true]);
}

#[test]
fn parse_without_data_is_all_false() {
    assert_eq!(contained::parse(&json!({}), 2), vec![false, false]);
}

#[test]
fn query_has_one_alias_per_target_and_variables() {
    let targets = vec![
        target("acme/api", "master", "aaaa1111"),
        target("acme/web", "deploy/29-10-2026", "bbbb2222"),
    ];
    let (query, variables) = contained::build_query(&targets);
    assert!(query.contains("t0: repository(owner: $o0, name: $r0)"));
    assert!(query.contains("t1: repository(owner: $o1, name: $r1)"));
    assert!(!query.contains("t2:"));
    assert!(query.contains("ref(qualifiedName: $q1)"));
    assert!(query.contains("compare(headRef: $h1)"));
    assert_eq!(variables["o0"], "acme");
    assert_eq!(variables["r0"], "api");
    assert_eq!(variables["q0"], "refs/heads/master");
    assert_eq!(variables["h0"], "aaaa1111");
    assert_eq!(variables["o1"], "acme");
    assert_eq!(variables["r1"], "web");
    assert_eq!(variables["q1"], "refs/heads/deploy/29-10-2026");
    assert_eq!(variables["h1"], "bbbb2222");
}

#[test]
fn query_never_contains_user_text() {
    let targets = vec![target("acme/secret-repo", "feature-x", "cafebabe")];
    let (query, _) = contained::build_query(&targets);
    for text in ["acme", "secret-repo", "feature-x", "cafebabe"] {
        assert!(!query.contains(text), "{text} leaked into the query");
    }
}

#[test]
fn pr_contained_in_base_is_skipped() {
    let ready = vec![(pr("acme/api", 1, "feature", "master"), Scope::SharedBranch)];
    let wanted = contained::targets(&ready, TRAIN);
    assert_eq!(wanted.len(), 2);
    let (kept, skipped) = contained::apply(ready, TRAIN, &[true, false]);
    assert!(kept.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].reasons, vec!["already in master".to_string()]);
    assert_eq!(skipped[0].scope, Scope::SharedBranch);
}

#[test]
fn pr_contained_only_in_train_is_skipped_with_train_name() {
    let ready = vec![(pr("acme/api", 1, "feature", "master"), Scope::SharedBranch)];
    let (kept, skipped) = contained::apply(ready, TRAIN, &[false, true]);
    assert!(kept.is_empty());
    assert_eq!(
        skipped[0].reasons,
        vec!["already in deploy/29-10-2026".to_string()]
    );
}

#[test]
fn train_scope_checks_only_the_base() {
    let ready = vec![(pr("acme/api", 1, TRAIN, "master"), Scope::Train)];
    let wanted = contained::targets(&ready, TRAIN);
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].reference, "master");
    let (kept, skipped) = contained::apply(ready, TRAIN, &[false]);
    assert_eq!(kept.len(), 1);
    assert!(skipped.is_empty());
}

#[test]
fn into_train_pr_checks_only_the_train_once() {
    let ready = vec![(pr("acme/api", 1, "feature", TRAIN), Scope::IntoTrain)];
    let wanted = contained::targets(&ready, TRAIN);
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].reference, TRAIN);
}

#[test]
fn pr_not_contained_stays_ready() {
    let ready = vec![(pr("acme/api", 1, "feature", "master"), Scope::SharedBranch)];
    let (kept, skipped) = contained::apply(ready, TRAIN, &[false, false]);
    assert_eq!(kept.len(), 1);
    assert!(skipped.is_empty());
}

#[test]
fn verdicts_stay_aligned_across_several_prs() {
    let ready = vec![
        (pr("acme/a", 1, "feat", "master"), Scope::SharedBranch),
        (pr("acme/b", 2, TRAIN, "master"), Scope::Train),
        (pr("acme/c", 3, "feat", "master"), Scope::SharedBranch),
        (pr("acme/d", 4, "feat", TRAIN), Scope::IntoTrain),
    ];
    let wanted = contained::targets(&ready, TRAIN);
    assert_eq!(wanted.len(), 6);
    let verdicts = [false, false, false, false, true, false];
    let (kept, skipped) = contained::apply(ready, TRAIN, &verdicts);
    let kept_numbers: Vec<u64> = kept.iter().map(|(p, _)| p.key.number).collect();
    let skipped_numbers: Vec<u64> = skipped.iter().map(|s| s.pr.key.number).collect();
    assert_eq!(kept_numbers, vec![1, 2, 4]);
    assert_eq!(skipped_numbers, vec![3]);
    assert_eq!(
        skipped[0].reasons,
        vec!["already in deploy/29-10-2026".to_string()]
    );
}

#[test]
fn pr_without_head_sha_is_not_checked() {
    let mut blank = pr("acme/api", 1, "feature", "master");
    blank.head_sha = String::new();
    let ready = vec![(blank, Scope::SharedBranch)];
    assert!(contained::targets(&ready, TRAIN).is_empty());
    let (kept, skipped) = contained::apply(ready, TRAIN, &[]);
    assert_eq!(kept.len(), 1);
    assert!(skipped.is_empty());
}
