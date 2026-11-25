use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use pipetrain::discovery::Scope;
use pipetrain::model::{Checks, Pr, PrKey, PrState, Review};
use pipetrain::sequence::{self, Member, Remote};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@localhost")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@localhost")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-sequence-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

const TEMPLATE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

struct Hosted {
    root: PathBuf,
    heads: Vec<(u64, String)>,
}

fn hosted(label: &str, changes: &[(u64, &str, &str)]) -> Hosted {
    let root = scratch(label);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["init", "-q", "-b", "master"]);
    std::fs::write(work.join("serverless.template"), TEMPLATE).unwrap();
    run(&work, &["add", "."]);
    run(&work, &["commit", "-q", "-m", "base"]);
    let bare = root.join("remote").join("acme").join("api.git");
    std::fs::create_dir_all(&bare).unwrap();
    run(&bare, &["init", "-q", "--bare"]);
    let target = bare.to_string_lossy().to_string();
    run(&work, &["push", "-q", &target, "master:refs/heads/master"]);
    let mut heads = Vec::new();
    for (number, from, to) in changes {
        run(&work, &["checkout", "-q", "-B", "topic", "master"]);
        let current = std::fs::read_to_string(work.join("serverless.template")).unwrap();
        std::fs::write(work.join("serverless.template"), current.replace(from, to)).unwrap();
        run(&work, &["commit", "-q", "-am", &format!("pr {number}")]);
        let sha = run(&work, &["rev-parse", "HEAD"]);
        run(
            &work,
            &[
                "push",
                "-q",
                &target,
                &format!("HEAD:refs/pull/{number}/head"),
            ],
        );
        heads.push((*number, sha));
    }
    Hosted { root, heads }
}

fn remote_for(hosted: &Hosted) -> Remote {
    Remote {
        base: file_url(&hosted.root.join("remote")),
        token: String::new(),
    }
}

fn pr(number: u64, head: &str) -> (Pr, Scope) {
    (
        Pr {
            key: PrKey::new("acme", "api", number),
            state: PrState::Open,
            title: format!("change {number}"),
            url: format!("https://github.com/acme/api/pull/{number}"),
            author: "ana".to_string(),
            body: String::new(),
            head_ref: format!("feat/{number}"),
            default_branch: "master".to_string(),
            merge_state_status: String::new(),
            check_details: Vec::new(),
            base_ref: "master".to_string(),
            head_sha: head.to_string(),
            base_sha: String::new(),
            is_draft: false,
            review: Review::Approved,
            checks: Checks::Success,
            failed_checks: Vec::new(),
            conflicting: false,
            mergeable_unknown: false,
            behind: false,
            files: Vec::new(),
            files_complete: true,
            labels: Vec::new(),
        },
        Scope::Train,
    )
}

fn selection(hosted: &Hosted) -> Vec<(Pr, Scope)> {
    hosted.heads.iter().map(|(n, sha)| pr(*n, sha)).collect()
}

fn ids(list: &[(Pr, Scope)]) -> Vec<String> {
    list.iter().map(|(pr, _)| pr.key.to_string()).collect()
}

#[test]
fn a_pr_that_conflicts_after_an_earlier_one_merges_is_skipped_with_the_cause() {
    let hosted = hosted(
        "conflict",
        &[
            (47, "two", "two from 47"),
            (51, "two", "two from 51"),
            (52, "nine", "nine from 52"),
        ],
    );
    let cache = scratch("cache");
    let (ready, skipped) = sequence::filter(&remote_for(&hosted), &cache, selection(&hosted));
    assert_eq!(ids(&ready), vec!["acme/api#47", "acme/api#52"]);
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].pr.key.to_string(), "acme/api#51");
    assert_eq!(
        skipped[0].reasons,
        vec!["conflicts with acme/api#47 after it merges: serverless.template"]
    );
}

#[test]
fn prs_that_touch_other_lines_all_stay() {
    let hosted = hosted(
        "clean",
        &[
            (1, "two", "two from 1"),
            (2, "nine", "nine from 2"),
            (3, "five", "five from 3"),
        ],
    );
    let cache = scratch("cache");
    let (ready, skipped) = sequence::filter(&remote_for(&hosted), &cache, selection(&hosted));
    assert_eq!(ready.len(), 3);
    assert!(skipped.is_empty());
}

#[test]
fn without_data_the_selection_is_kept_whole() {
    let hosted = hosted(
        "nodata",
        &[(47, "two", "two from 47"), (51, "two", "two from 51")],
    );
    let cache = scratch("cache");
    let remote = Remote {
        base: file_url(&hosted.root.join("missing")),
        token: String::new(),
    };
    let (ready, skipped) = sequence::filter(&remote, &cache, selection(&hosted));
    assert_eq!(ready.len(), 2);
    assert!(skipped.is_empty());
}

#[test]
fn a_head_that_moved_since_the_plan_read_it_keeps_the_selection() {
    let hosted = hosted(
        "moved",
        &[(47, "two", "two from 47"), (51, "two", "two from 51")],
    );
    let cache = scratch("cache");
    let mut stale = selection(&hosted);
    stale[1].0.head_sha = "0".repeat(40);
    let (ready, skipped) = sequence::filter(&remote_for(&hosted), &cache, stale);
    assert_eq!(ready.len(), 2);
    assert!(skipped.is_empty());
}

#[test]
fn a_single_pr_per_repo_and_base_costs_nothing() {
    let hosted = hosted("single", &[(47, "two", "two from 47")]);
    let cache = scratch("cache");
    let (ready, skipped) = sequence::filter(&remote_for(&hosted), &cache, selection(&hosted));
    assert_eq!(ready.len(), 1);
    assert!(skipped.is_empty());
    assert!(!cache.join("sequence").exists());
}

#[test]
fn the_order_is_by_pr_number_whatever_order_the_selection_arrives_in() {
    let hosted = hosted(
        "order",
        &[(47, "two", "two from 47"), (51, "two", "two from 51")],
    );
    let cache = scratch("cache");
    let mut reversed = selection(&hosted);
    reversed.reverse();
    let (ready, skipped) = sequence::filter(&remote_for(&hosted), &cache, reversed);
    assert_eq!(ids(&ready), vec!["acme/api#47"]);
    assert_eq!(skipped[0].pr.key.to_string(), "acme/api#51");
}

#[test]
fn a_conflict_that_needs_two_earlier_prs_is_blamed_on_one_of_them() {
    let hosted = hosted(
        "chain",
        &[
            (1, "two", "two from 1"),
            (2, "nine", "nine from 2"),
            (3, "nine", "nine from 3"),
        ],
    );
    let dir = hosted.root.join("remote").join("acme").join("api.git");
    let base = run(&dir, &["rev-parse", "refs/heads/master"]);
    let members: Vec<Member> = hosted
        .heads
        .iter()
        .map(|(number, head)| Member {
            id: format!("acme/api#{number}"),
            number: *number,
            head: head.clone(),
        })
        .collect();
    let found = sequence::conflicts_in_order(&dir, &base, &members).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "acme/api#3");
    assert_eq!(found[0].with, "acme/api#2");
}
