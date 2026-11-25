use std::collections::BTreeSet;
use std::fs;

mod common;

use chrono::{Duration, Utc};
use common::{TempDir, pr, serve_handler as serve};
use pipetrain::command::authors::{self, AuthorFilter};
use pipetrain::config::{ApiConfig, Config};
use pipetrain::discovery::{self, Discovered, Scope};
use pipetrain::github::Client;
use pipetrain::merge::{Archival, Store, archived_beside};
use pipetrain::model::{Pr, Review};
use serde_json::json;

const TRAIN: &str = "deploy/25-11-2026";

fn cfg() -> Config {
    Config::parse("orgs = [\"acme\"]").unwrap()
}

fn by(slug: &str, number: u64, author: &str) -> Pr {
    let mut item = pr(slug, number, &format!("feat/{number}"), "main");
    item.default_branch = "main".to_string();
    item.author = author.to_string();
    item
}

fn ids(found: &Discovered) -> Vec<String> {
    let mut all: Vec<String> = found
        .ready
        .iter()
        .map(|(p, _)| p.key.to_string())
        .chain(found.skipped.iter().map(|s| s.pr.key.to_string()))
        .collect();
    all.sort();
    all
}

fn scope_of(found: &Discovered, id: &str) -> Scope {
    found
        .ready
        .iter()
        .find(|(p, _)| p.key.to_string() == id)
        .map(|(_, s)| *s)
        .or_else(|| {
            found
                .skipped
                .iter()
                .find(|s| s.pr.key.to_string() == id)
                .map(|s| s.scope)
        })
        .unwrap()
}

fn authors(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn author_selection_takes_open_prs_of_each_repeated_author_with_origin_author() {
    let all = vec![
        by("acme/api", 1, "Lucas"),
        by("acme/web", 2, "ana"),
        by("acme/web", 3, "bob"),
    ];
    let found = discovery::discover_authored(all, &cfg(), TRAIN, &authors(&["lucas", "ANA"]));
    assert_eq!(ids(&found), vec!["acme/api#1", "acme/web#2"]);
    assert_eq!(scope_of(&found, "acme/api#1"), Scope::Author);
    assert_eq!(Scope::Author.as_str(), "author");
}

#[test]
fn author_selection_leaves_drafts_out_and_reports_unready_with_cause() {
    let mut draft = by("acme/api", 1, "lucas");
    draft.is_draft = true;
    let mut unapproved = by("acme/api", 2, "lucas");
    unapproved.review = Review::ReviewRequired;
    let found =
        discovery::discover_authored(vec![draft, unapproved], &cfg(), TRAIN, &authors(&["lucas"]));
    assert_eq!(ids(&found), vec!["acme/api#2"]);
    assert!(found.ready.is_empty());
    assert!(!found.skipped[0].reasons.is_empty());
}

#[test]
fn author_selection_keeps_the_train_origin_and_adds_nothing_twice() {
    let mut train = by("acme/api", 1, "lucas");
    train.head_ref = TRAIN.to_string();
    let found = discovery::discover_authored(
        vec![train, by("acme/api", 2, "lucas")],
        &cfg(),
        TRAIN,
        &authors(&["lucas"]),
    );
    assert_eq!(ids(&found), vec!["acme/api#1", "acme/api#2"]);
    assert_eq!(scope_of(&found, "acme/api#1"), Scope::Train);
    assert_eq!(scope_of(&found, "acme/api#2"), Scope::Author);
}

#[test]
fn authors_join_explicit_prs_without_duplicating() {
    let mut found = discovery::explicit(vec![by("acme/api", 1, "lucas")], &cfg());
    let pool = vec![
        by("acme/api", 1, "lucas"),
        by("acme/api", 2, "lucas"),
        by("acme/api", 3, "bob"),
    ];
    discovery::add_authored(&mut found, &pool, &cfg(), &authors(&["lucas"]));
    assert_eq!(ids(&found), vec!["acme/api#1", "acme/api#2"]);
    assert_eq!(scope_of(&found, "acme/api#1"), Scope::Explicit);
    assert_eq!(scope_of(&found, "acme/api#2"), Scope::Author);
}

#[test]
fn mine_is_the_login_of_the_token_and_only_flags_select() {
    let server = serve(|_| (200, json!({"data": {"viewer": {"login": "Lucas"}}})));
    let api = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
        max_per_second: 1000,
        ..ApiConfig::default()
    };
    let mut client =
        Client::with_base_url("ghp_FAKE0123456789".to_string(), &api, false, &server.base);
    let filter = AuthorFilter::from_flags(true, vec!["Ana".to_string()], false);
    let logins = authors::resolve(&filter.selecting(), &mut client).unwrap();
    assert_eq!(logins, vec!["ana".to_string(), "lucas".to_string()]);
    assert!(AuthorFilter::Config.selecting().is_empty());
    assert!(AuthorFilter::None.selecting().is_empty());
}

fn stacked(number: u64, head: &str, base: &str) -> Pr {
    let mut item = pr("acme/auth", number, head, base);
    item.default_branch = "main".to_string();
    item
}

fn finder(pool: Vec<Pr>) -> impl FnMut(&Pr) -> anyhow::Result<Vec<Pr>> {
    move |child| {
        Ok(pool
            .iter()
            .filter(|p| p.head_ref == child.base_ref)
            .cloned()
            .collect())
    }
}

#[test]
fn the_parent_of_a_selected_stacked_pr_joins_with_origin_stack_parent() {
    let parent = stacked(40, "feat/totp-hardening", "main");
    let child = stacked(41, "feat/totp-ui", "feat/totp-hardening");
    let mut found = discovery::explicit(vec![child], &cfg());
    discovery::add_stack_parents(&mut found, &cfg(), finder(vec![parent])).unwrap();
    assert_eq!(ids(&found), vec!["acme/auth#40", "acme/auth#41"]);
    assert_eq!(scope_of(&found, "acme/auth#40"), Scope::StackParent);
    assert_eq!(scope_of(&found, "acme/auth#41"), Scope::Explicit);
    assert_eq!(Scope::StackParent.as_str(), "stack-parent");
}

#[test]
fn a_stack_of_three_pulls_every_ancestor() {
    let one = stacked(1, "a", "main");
    let two = stacked(2, "b", "a");
    let three = stacked(3, "c", "b");
    let mut found = discovery::explicit(vec![three.clone()], &cfg());
    discovery::add_stack_parents(&mut found, &cfg(), finder(vec![one, two, three])).unwrap();
    assert_eq!(
        ids(&found),
        vec!["acme/auth#1", "acme/auth#2", "acme/auth#3"]
    );
    assert_eq!(scope_of(&found, "acme/auth#2"), Scope::StackParent);
    assert_eq!(scope_of(&found, "acme/auth#1"), Scope::StackParent);
}

#[test]
fn a_cycle_of_bases_terminates_without_duplicates() {
    let a = stacked(1, "a", "b");
    let b = stacked(2, "b", "a");
    let mut found = discovery::explicit(vec![a.clone()], &cfg());
    discovery::add_stack_parents(&mut found, &cfg(), finder(vec![a, b])).unwrap();
    assert_eq!(ids(&found), vec!["acme/auth#1", "acme/auth#2"]);
}

#[test]
fn an_unready_parent_holds_the_child_and_both_are_skipped_with_causes() {
    let mut parent = stacked(40, "feat/totp-hardening", "main");
    parent.is_draft = true;
    let child = stacked(41, "feat/totp-ui", "feat/totp-hardening");
    let mut found = discovery::explicit(vec![child], &cfg());
    discovery::add_stack_parents(&mut found, &cfg(), finder(vec![parent])).unwrap();
    assert!(found.ready.is_empty());
    let held = found
        .skipped
        .iter()
        .find(|s| s.pr.key.number == 41)
        .unwrap();
    assert_eq!(
        held.reasons,
        vec!["stack parent acme/auth#40 not ready".to_string()]
    );
    let mother = found
        .skipped
        .iter()
        .find(|s| s.pr.key.number == 40)
        .unwrap();
    assert!(!mother.reasons.is_empty());
}

#[test]
fn an_unready_grandparent_holds_the_whole_chain() {
    let mut one = stacked(1, "a", "main");
    one.review = Review::ReviewRequired;
    let two = stacked(2, "b", "a");
    let three = stacked(3, "c", "b");
    let mut found = discovery::explicit(vec![three.clone()], &cfg());
    discovery::add_stack_parents(&mut found, &cfg(), finder(vec![one, two, three])).unwrap();
    assert!(found.ready.is_empty());
    let message = |n: u64| {
        found
            .skipped
            .iter()
            .find(|s| s.pr.key.number == n)
            .unwrap()
            .reasons
            .clone()
    };
    assert_eq!(
        message(2),
        vec!["stack parent acme/auth#1 not ready".to_string()]
    );
    assert_eq!(
        message(3),
        vec!["stack parent acme/auth#2 not ready".to_string()]
    );
}

#[test]
fn prs_on_the_default_or_a_train_branch_do_not_look_for_a_parent() {
    let mut calls = 0;
    let mut found = discovery::explicit(
        vec![
            stacked(1, "x", "main"),
            stacked(2, "y", "deploy/25-11-2026"),
        ],
        &cfg(),
    );
    discovery::add_stack_parents(&mut found, &cfg(), |_| {
        calls += 1;
        Ok(Vec::new())
    })
    .unwrap();
    assert_eq!(calls, 0);
}

fn write_state(directory: &TempDir, pending: &[(&str, i64)]) -> std::path::PathBuf {
    let path = directory.join("merge-state.json");
    let records: Vec<_> = pending
        .iter()
        .map(|(id, hours)| {
            json!({
                "id": id,
                "repo": "acme/api",
                "base": "main",
                "sha": "0123456789abcdef0123456789abcdef01234567",
                "recorded_at": (Utc::now() - Duration::hours(*hours)).to_rfc3339(),
            })
        })
        .collect();
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({"schema": 1, "pending": records})).unwrap(),
    )
    .unwrap();
    path
}

fn selection(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_stale_state_of_another_queue_is_archived_intact_on_apply() {
    let directory = TempDir::new("archive");
    let path = write_state(&directory, &[("acme/api#1", 200), ("acme/api#2", 300)]);
    let before = fs::read(&path).unwrap();
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, Utc::now())
        .unwrap();
    let Archival::Archived { pending, to } = outcome else {
        panic!("expected archive");
    };
    assert_eq!(
        pending,
        vec!["acme/api#1".to_string(), "acme/api#2".to_string()]
    );
    assert_eq!(fs::read(&to).unwrap(), before);
    assert!(!path.exists());
    assert!(store.state.pending.is_empty());
    let name = to.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("merge-state.") && name.ends_with("Z.json"),
        "{name}"
    );
    let listed = archived_beside(&path);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].pending, 2);
    assert!(listed[0].archived_at.is_some());
}

#[test]
fn a_dry_run_renames_nothing_and_says_what_apply_would_do() {
    let directory = TempDir::new("archive-dry");
    let path = write_state(&directory, &[("acme/api#1", 200)]);
    let before = fs::read(&path).unwrap();
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, false, Utc::now())
        .unwrap();
    let notice = outcome.notice(&path).unwrap();
    assert!(matches!(outcome, Archival::WouldArchive { .. }));
    assert!(notice.contains("--apply would archive") && notice.contains("acme/api#1"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(archived_beside(&path).is_empty());
    assert_eq!(store.state.pending.len(), 1);
}

#[test]
fn a_fresh_pending_keeps_the_state_as_it_is() {
    let directory = TempDir::new("archive-fresh");
    let path = write_state(&directory, &[("acme/api#1", 200), ("acme/api#2", 1)]);
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, Utc::now())
        .unwrap();
    assert_eq!(outcome, Archival::Kept);
    assert!(path.exists());
}

#[test]
fn the_same_queue_keeps_the_state_as_it_is() {
    let directory = TempDir::new("archive-same");
    let path = write_state(&directory, &[("acme/api#1", 200)]);
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(
            &selection(&["acme/api#1", "acme/web#9"]),
            24,
            true,
            Utc::now(),
        )
        .unwrap();
    assert_eq!(outcome, Archival::Kept);
    assert!(path.exists());
}

#[test]
fn a_held_lock_keeps_the_state_as_it_is() {
    let directory = TempDir::new("archive-lock");
    let path = write_state(&directory, &[("acme/api#1", 200)]);
    let mut holder = Store::open(Some(path.clone())).unwrap();
    holder.lock().unwrap();
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, Utc::now())
        .unwrap();
    assert_eq!(outcome, Archival::Kept);
    assert!(path.exists());
}

#[test]
fn an_active_job_keeps_the_state_as_it_is() {
    let directory = TempDir::new("archive-job");
    let path = write_state(&directory, &[("acme/api#1", 200)]);
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["active_jobs"] = json!(["acme/api#1"]);
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let mut store = Store::open(Some(path.clone())).unwrap();
    let outcome = store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, Utc::now())
        .unwrap();
    assert_eq!(outcome, Archival::Kept);
}

#[test]
fn two_archives_in_the_same_second_get_distinct_names() {
    let directory = TempDir::new("archive-twice");
    let now = Utc::now();
    let path = write_state(&directory, &[("acme/api#1", 200)]);
    let mut first = Store::open(Some(path.clone())).unwrap();
    first
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, now)
        .unwrap();
    drop(first);
    write_state(&directory, &[("acme/api#2", 200)]);
    let mut second = Store::open(Some(path.clone())).unwrap();
    second
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, now)
        .unwrap();
    assert_eq!(archived_beside(&path).len(), 2);
}

#[test]
fn state_show_lists_archived_files_in_json_and_text() {
    let directory = TempDir::new("archive-show");
    let path = write_state(&directory, &[("acme/api#1", 200), ("acme/api#2", 200)]);
    let mut store = Store::open(Some(path.clone())).unwrap();
    store
        .archive_stale_queue(&selection(&["acme/web#9"]), 24, true, Utc::now())
        .unwrap();
    let archived = archived_beside(&path);
    let value = pipetrain::command::state::show(&store, "acme", 24, &archived).unwrap();
    assert_eq!(value["archived"][0]["pending"], 2);
    assert_eq!(value["archived"][0]["path"], archived[0].path);
    assert!(value["archived"][0]["archived_at"].is_string());
}
