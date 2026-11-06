mod common;

use common::serve_bodies;
use pipetrain::config::ApiConfig;
use pipetrain::github::Client;
use serde_json::{Value, json};

const FAKE: &str = "fake-value-for-tests";

fn node(number: u64) -> Value {
    json!({
        "number": number,
        "title": "change",
        "url": format!("https://github.com/acme/api/pull/{number}"),
        "headRefName": "feat/a",
        "baseRefName": "main",
        "repository": { "name": "api", "owner": { "login": "acme" } },
    })
}

fn reply(count: Option<u64>, numbers: &[u64], next: Option<&str>) -> String {
    let mut search = json!({
        "pageInfo": { "hasNextPage": next.is_some(), "endCursor": next },
        "nodes": numbers.iter().map(|n| node(*n)).collect::<Vec<_>>(),
    });
    if let Some(count) = count {
        search["issueCount"] = json!(count);
    }
    json!({ "data": { "search": search } }).to_string()
}

fn client(base: &str) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
}

#[test]
fn a_complete_reply_passes_and_asks_for_the_count_in_pages_of_20() {
    let (base, server) = serve_bodies(vec![reply(Some(3), &[1, 2, 3], None)]);
    let prs = client(&base).open_prs("acme").unwrap();
    let seen = server.join().unwrap();
    assert_eq!(prs.len(), 3);
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("issueCount"));
    assert!(seen[0].contains("first: 20"));
}

#[test]
fn a_short_reply_is_searched_again_and_the_complete_one_passes() {
    let (base, server) = serve_bodies(vec![
        reply(Some(3), &[1], None),
        reply(Some(3), &[1, 2, 3], None),
    ]);
    let prs = client(&base).open_prs("acme").unwrap();
    let seen = server.join().unwrap();
    assert_eq!(prs.len(), 3);
    assert_eq!(seen.len(), 2);
}

#[test]
fn a_reply_that_stays_short_fails_with_a_clear_message() {
    let (base, server) = serve_bodies(vec![
        reply(Some(10), &[1, 2], None),
        reply(Some(10), &[1, 2], None),
        reply(Some(10), &[1, 2], None),
    ]);
    let error = client(&base).open_prs("acme").unwrap_err().to_string();
    let seen = server.join().unwrap();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        error,
        "GitHub returned 2 of 10 open PRs in acme (the search was cut short). Try again in a minute, or run with --verbose."
    );
    assert!(!error.contains(FAKE));
}

#[test]
fn a_reply_without_the_count_is_not_checked() {
    let (base, server) = serve_bodies(vec![reply(None, &[1], None)]);
    let prs = client(&base).open_prs("acme").unwrap();
    let seen = server.join().unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(seen.len(), 1);
}

#[test]
fn a_count_that_is_not_a_number_is_not_checked() {
    let mut body: Value = serde_json::from_str(&reply(None, &[1], None)).unwrap();
    body["data"]["search"]["issueCount"] = json!("many");
    let (base, server) = serve_bodies(vec![body.to_string()]);
    let prs = client(&base).open_prs("acme").unwrap();
    let seen = server.join().unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(seen.len(), 1);
}

#[test]
fn nodes_are_summed_across_pages_before_the_count_is_checked() {
    let (base, server) = serve_bodies(vec![
        reply(Some(3), &[1, 2], Some("cursor:2")),
        reply(Some(3), &[3], None),
    ]);
    let prs = client(&base).open_prs("acme").unwrap();
    let seen = server.join().unwrap();
    assert_eq!(prs.len(), 3);
    assert_eq!(seen.len(), 2);
}
