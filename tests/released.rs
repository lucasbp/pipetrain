use pipetrain::config::Config;
use pipetrain::discovery::{self, Discovered};
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};

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

fn run(prs: Vec<Pr>) -> Discovered {
    let cfg = Config::parse("orgs = [\"acme\"]").unwrap();
    discovery::released(prs, &cfg, TRAIN)
}

#[test]
fn approved_green_pr_on_any_branch_is_released() {
    let found = run(vec![pr("acme/api", 1, "fix/typo", "main")]);
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.ready[0].1.as_str(), "released");
    assert!(found.skipped.is_empty());
    assert!(found.questions.is_empty());
}

#[test]
fn approved_pr_with_failing_checks_is_skipped_with_the_reason() {
    let mut red = pr("acme/api", 2, "fix/typo", "main");
    red.checks = Checks::Failure;
    let found = run(vec![red]);
    assert!(found.ready.is_empty());
    assert_eq!(found.skipped.len(), 1);
    assert_eq!(found.skipped[0].scope.as_str(), "released");
    assert_eq!(found.skipped[0].reasons, vec!["checks failing".to_string()]);
}

#[test]
fn unapproved_pr_is_ignored_in_silence() {
    let mut none = pr("acme/api", 3, "feat/a", "main");
    none.review = Review::None;
    let mut required = pr("acme/api", 4, "feat/b", "main");
    required.review = Review::ReviewRequired;
    let mut changes = pr("acme/api", 5, "feat/c", "main");
    changes.review = Review::ChangesRequested;
    let found = run(vec![none, required, changes]);
    assert!(found.ready.is_empty());
    assert!(found.skipped.is_empty());
}

#[test]
fn train_and_into_train_scopes_take_precedence() {
    let on_train = pr("acme/api", 6, TRAIN, "main");
    let into_train = pr("acme/web", 7, "feat/x", TRAIN);
    let plain = pr("acme/lib", 8, "feat/y", "main");
    let found = run(vec![on_train, into_train, plain]);
    let scope_of = |n: u64| {
        found
            .ready
            .iter()
            .find(|(p, _)| p.key.number == n)
            .map(|(_, s)| s.as_str())
            .unwrap()
    };
    assert_eq!(scope_of(6), "train");
    assert_eq!(scope_of(7), "into-train");
    assert_eq!(scope_of(8), "released");
}

#[test]
fn approved_draft_is_skipped_as_draft() {
    let mut draft = pr("acme/api", 9, "wip/a", "main");
    draft.is_draft = true;
    let found = run(vec![draft]);
    assert!(found.ready.is_empty());
    assert_eq!(found.skipped.len(), 1);
    assert_eq!(found.skipped[0].reasons, vec!["draft".to_string()]);
}

#[test]
fn shared_branches_do_not_raise_sibling_questions() {
    let ready = pr("acme/api", 10, "feat/shared", "main");
    let mut blocked = pr("acme/web", 11, "feat/shared", "main");
    blocked.checks = Checks::Failure;
    let found = run(vec![ready, blocked]);
    assert_eq!(found.ready.len(), 1);
    assert_eq!(found.ready[0].1.as_str(), "released");
    assert_eq!(found.skipped.len(), 1);
    assert!(found.questions.is_empty());
}
