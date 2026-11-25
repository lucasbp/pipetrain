use pipetrain::config::Config;
use pipetrain::discovery::{self, Scope};
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};

const TRAIN: &str = "deploy/30-10-2026";

fn pr(org: &str, number: u64, review: Review) -> Pr {
    Pr {
        key: PrKey::new(org, "api", number),
        state: PrState::Open,
        title: format!("change {number}"),
        url: format!("https://github.com/{org}/api/pull/{number}"),
        author: "ana".to_string(),
        body: String::new(),
        head_ref: format!("feat/{number}"),
        default_branch: "master".to_string(),
        merge_state_status: String::new(),
        check_details: Vec::new(),
        base_ref: "master".to_string(),
        head_sha: format!("{number:040x}"),
        base_sha: String::new(),
        is_draft: false,
        review,
        checks: Checks::Success,
        failed_checks: Vec::new(),
        conflicting: false,
        mergeable_unknown: false,
        behind: false,
        files: Vec::new(),
        files_complete: true,
        labels: Vec::new(),
    }
}

fn cfg() -> Config {
    Config::parse("orgs = [\"acme\"]\n").unwrap()
}

fn ids(out: &discovery::Discovered) -> Vec<String> {
    out.ready
        .iter()
        .map(|(pr, _)| pr.key.to_string())
        .chain(out.skipped.iter().map(|s| s.pr.key.to_string()))
        .collect()
}

#[test]
fn a_pr_returned_twice_by_the_search_is_listed_once() {
    let pool = vec![
        pr("acme", 45, Review::ReviewRequired),
        pr("acme", 46, Review::Approved),
        pr("acme", 45, Review::ReviewRequired),
    ];
    let out = discovery::discover_authored(pool, &cfg(), TRAIN, &["ana".to_string()]);
    let mut listed = ids(&out);
    listed.sort();
    assert_eq!(listed, vec!["acme/api#45", "acme/api#46"]);
}

#[test]
fn the_same_pr_under_another_letter_case_is_listed_once() {
    let pool = vec![
        pr("acme", 45, Review::ReviewRequired),
        pr("Acme", 45, Review::ReviewRequired),
    ];
    let out = discovery::discover_authored(pool, &cfg(), TRAIN, &["ana".to_string()]);
    assert_eq!(ids(&out).len(), 1);
}

#[test]
fn the_author_pass_does_not_add_what_discovery_already_listed() {
    let pool = vec![pr("acme", 45, Review::ReviewRequired)];
    let mut out = discovery::explicit(pool.clone(), &cfg());
    discovery::add_authored(&mut out, &pool, &cfg(), &["ana".to_string()]);
    assert_eq!(ids(&out), vec!["acme/api#45"]);
    assert!(out.ready.iter().all(|(_, scope)| *scope == Scope::Explicit));
}

#[test]
fn explicit_and_released_drop_repeated_prs() {
    let twice = vec![
        pr("acme", 7, Review::Approved),
        pr("acme", 7, Review::Approved),
    ];
    assert_eq!(ids(&discovery::explicit(twice.clone(), &cfg())).len(), 1);
    assert_eq!(ids(&discovery::released(twice, &cfg(), TRAIN)).len(), 1);
}
