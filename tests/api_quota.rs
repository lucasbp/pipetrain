mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use common::Headers;
use pipetrain::cache::{Cache, CacheMode};
use pipetrain::config::ApiConfig;
use pipetrain::github::Client;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKEQUOTA0123456789";

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-api-quota-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_file() -> PathBuf {
    let path = scratch("config").join("pipetrain.toml");
    fs::write(&path, "orgs = [\"acme\"]\n").unwrap();
    path
}

fn run(args: &[&str], api: &str, cache: &Path, quota_check: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
    command
        .args(args)
        .arg("--config")
        .arg(config_file())
        .env("GH_TOKEN", FAKE)
        .env("PIPETRAIN_API_URL", api)
        .env("PIPETRAIN_CACHE_DIR", cache)
        .env("PIPETRAIN_LOG_DIR", scratch("log"))
        .env_remove("PIPETRAIN_LOG")
        .env("PATH", scratch("empty-path"))
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .current_dir(scratch("home"));
    if quota_check {
        command.env("PIPETRAIN_QUOTA_CHECK", "1");
    }
    command.output().unwrap()
}

fn window(remaining: u64) -> Value {
    let one = json!({ "remaining": remaining, "limit": 5000, "reset": 4_000_000_000_i64 });
    json!({ "resources": { "core": one, "graphql": one, "search": one } })
}

fn pr_node(number: u64) -> Value {
    json!({
        "number": number,
        "title": "Add a thing",
        "url": format!("https://example.test/acme/api/pull/{number}"),
        "body": "",
        "headRefName": "feature/thing",
        "baseRefName": "main",
        "baseRef": { "target": { "oid": "b".repeat(40) } },
        "headRefOid": "a".repeat(40),
        "isDraft": false,
        "state": "OPEN",
        "reviewDecision": "APPROVED",
        "mergeable": "MERGEABLE",
        "author": { "login": "octocat" },
        "labels": { "nodes": [] },
        "repository": { "name": "api", "owner": { "login": "acme" } },
        "latestOpinionatedReviews": { "nodes": [{ "state": "APPROVED" }] },
        "commits": { "nodes": [{ "commit": { "statusCheckRollup": {
            "state": "SUCCESS", "contexts": { "nodes": [] }
        } } }] }
    })
}

fn number_of(body: &str) -> u64 {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.pointer("/variables/n").and_then(Value::as_u64))
        .unwrap_or(7)
}

fn answer(line: &str, body: &str) -> (u16, Value) {
    if line.starts_with("POST /graphql") {
        if body.contains("pullRequest(number") {
            return (
                200,
                json!({ "data": { "repository": { "pullRequest": pr_node(number_of(body)) } } }),
            );
        }
        if body.contains("compare(") {
            let ahead =
                json!({ "ref": { "compare": { "aheadBy": 1, "behindBy": 0, "status": "ahead" } } });
            return (200, json!({ "data": { "t0": ahead, "t1": ahead } }));
        }
        if body.contains("viewer") {
            return (200, json!({ "data": { "viewer": { "login": "octocat" } } }));
        }
        return (404, json!({ "message": "not faked" }));
    }
    if line.starts_with("GET /rate_limit") {
        return (200, window(4900));
    }
    if line.contains("/pulls/") && line.contains("/files") {
        return (200, json!([]));
    }
    (404, json!({ "message": "not faked" }))
}

fn every_file_text(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned());
            }
        }
    }
    found
}

fn network_requests(fake: &common::Server) -> usize {
    fake.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|request| !request.starts_with("GET /rate_limit"))
        .count()
}

#[test]
fn a_start_below_the_reserve_is_refused_with_exit_6_and_the_message() {
    let fake = common::serve_requests(|line, _| {
        if line.starts_with("GET /rate_limit") {
            (200, window(100))
        } else {
            (500, json!({ "message": "must not be reached" }))
        }
    });
    let cache = scratch("cache");
    let out = run(
        &["plan", "acme/api#7", "--json", "--no-progress"],
        &fake.base,
        &cache,
        true,
    );
    let errors = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(6), "{errors}");
    assert!(errors.contains("not enough GitHub API quota"), "{errors}");
    assert!(errors.contains("core: needs about"), "{errors}");
    assert!(errors.contains("UTC ("), "{errors}");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].starts_with("GET /rate_limit"), "{seen:?}");
}

#[test]
fn a_start_above_the_reserve_goes_ahead() {
    let fake = common::serve_requests(answer);
    let out = run(
        &["plan", "acme/api#7", "--json", "--no-progress"],
        &fake.base,
        &scratch("cache"),
        true,
    );
    let errors = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{errors}");
}

#[test]
fn a_second_plan_over_the_same_data_makes_at_most_a_tenth_of_the_requests() {
    let fake = common::serve_requests(answer);
    let cache = scratch("cache");
    let args = [
        "plan",
        "acme/api#1",
        "acme/api#2",
        "acme/api#3",
        "acme/api#4",
        "acme/api#5",
        "acme/api#6",
        "--json",
        "--no-progress",
    ];
    let first = run(&args, &fake.base, &cache, false);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let after_first = network_requests(&fake);
    assert!(after_first >= 10, "{after_first}");
    let second = run(&args, &fake.base, &cache, false);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_requests = network_requests(&fake) - after_first;
    assert!(
        second_requests * 10 <= after_first,
        "first {after_first}, second {second_requests}"
    );
    let plan: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert!(plan["stats"]["cache_hits"].as_u64().unwrap() > 0, "{plan}");
}

#[test]
fn the_token_never_reaches_the_cache_on_disk() {
    let fake = common::serve_requests(answer);
    let cache = scratch("cache");
    let out = run(
        &["plan", "acme/api#7", "--json", "--no-progress"],
        &fake.base,
        &cache,
        false,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let files = every_file_text(&cache);
    assert!(!files.is_empty(), "nothing was cached");
    for text in files {
        assert!(!text.contains(FAKE), "{text}");
        assert!(!text.to_lowercase().contains("authorization"), "{text}");
    }
}

#[test]
fn a_304_is_not_counted_as_spent() {
    let fake = common::serve_with_headers(|line, _, request| {
        assert!(line.starts_with("GET /repos/acme/api/pulls/7"), "{line}");
        let headers: Headers = vec![("ETag".to_string(), "\"v1\"".to_string())];
        if request.to_lowercase().contains("if-none-match: \"v1\"") {
            (304, headers, Value::Null)
        } else {
            (200, headers, json!({ "number": 7 }))
        }
    });
    let cache = Cache::open(CacheMode::Use, Some(scratch("cache")));
    let mut client = Client::with_base_url(
        FAKE.to_string(),
        &ApiConfig {
            max_per_second: 1000,
            ..ApiConfig::default()
        },
        false,
        &fake.base,
    )
    .with_cache(cache)
    .with_http_cache();
    client.set_live(true);
    let first = client.rest("/repos/acme/api/pulls/7").unwrap();
    assert_eq!(client.usage().core, 1);
    let second = client.rest("/repos/acme/api/pulls/7").unwrap();
    assert_eq!(first, second);
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
    assert_eq!(client.usage().core, 1, "a 304 must not stay spent");
    assert_eq!(client.usage().not_modified, 1);
}
