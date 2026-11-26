use std::fs;
use std::path::{Path, PathBuf};

mod common;

use common::serve_bodies as serve;
use pipetrain::cache::{Cache, CacheMode, Stored, http_key};
use pipetrain::config::ApiConfig;
use pipetrain::github::Client;
use pipetrain::model::{ChangedFile, PrKey};
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccc";
const SLUG: &str = "acme/api";

fn temp(name: &str) -> common::TempDir {
    common::TempDir::new(&format!("client-test-{name}"))
}

fn client(base: &str, dir: &Path, mode: CacheMode) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
        max_per_second: 1000,
        ..ApiConfig::default()
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
        .with_cache(Cache::open(mode, Some(dir.to_path_buf())))
}

fn key() -> PrKey {
    PrKey {
        org: "acme".to_string(),
        repo: "api".to_string(),
        number: 7,
    }
}

fn files_reply(names: &[&str]) -> String {
    let items: Vec<Value> = names
        .iter()
        .map(|name| json!({ "filename": name, "patch": "@@ -1 +1 @@\n-a\n+b" }))
        .collect();
    Value::Array(items).to_string()
}

fn shape(files: &[ChangedFile]) -> Vec<(String, Option<String>)> {
    files
        .iter()
        .map(|f| (f.path.clone(), f.patch.clone()))
        .collect()
}

fn blobs_reply(entries: &[Option<&str>]) -> String {
    let mut repository = serde_json::Map::new();
    for (i, text) in entries.iter().enumerate() {
        let value = text.map_or(json!({}), |t| json!({ "text": t }));
        repository.insert(format!("f{i}"), value);
    }
    json!({ "data": { "repository": repository } }).to_string()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn assert_clean(dir: &Path) {
    let mut found = Vec::new();
    walk(dir, &mut found);
    assert!(!found.is_empty());
    for path in found {
        let text = fs::read_to_string(&path).unwrap_or_default();
        assert!(!text.contains(FAKE), "token in {path:?}");
        assert!(!text.contains("Authorization"), "header in {path:?}");
        assert!(!text.contains("\"patch\""), "patch in {path:?}");
    }
}

#[test]
fn files_are_served_from_the_cache_without_a_request() {
    let scratch = temp("files-hit");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![files_reply(&["src/a.rs", "docs/b.md"])]);
    let mut first = client(&base, &dir, CacheMode::Use);
    let (files, complete) = first.files(&key(), HEAD, BASE).unwrap();
    assert_eq!(first.requests(), 1);
    assert_eq!(server.join().unwrap().len(), 1);

    let mut second = client(&base, &dir, CacheMode::Use);
    let (again, again_complete) = second.files(&key(), HEAD, BASE).unwrap();
    assert_eq!(second.requests(), 0);
    assert_eq!(second.cache_hits(), 1);
    assert_eq!(shape(&files), shape(&again));
    assert_eq!(complete, again_complete);
    assert_eq!(again.len(), 2);
    assert!(again.iter().all(|f| f.patch.is_none()));
    assert_clean(&dir);
}

#[test]
fn both_caches_require_the_same_profile_token_and_api_origin() {
    let scratch = temp("scopes");
    let dir = scratch.join("cache");
    let path = format!("/repos/{SLUG}/contents/package.json?ref={HEAD}");
    let replies = |marker: &str| {
        [
            json!({"tree": [{"type": "blob", "path": marker}], "truncated": false}).to_string(),
            json!({"name": marker}).to_string(),
        ]
    };
    let (base, server) = serve(
        ["one", "two", "three"]
            .into_iter()
            .flat_map(replies)
            .collect(),
    );
    let (other_base, other_server) = serve(Vec::from(replies("four")));
    let open = || Cache::open(CacheMode::Use, Some(dir.clone()));
    let mut legacy = open();
    legacy.put(
        SLUG,
        "tree",
        &[HEAD],
        &json!({"paths":["legacy"], "truncated":false}),
    );
    legacy.http().save(
        &http_key("GET", &format!("{base}{path}"), "", FAKE),
        &Stored::new(None, None, json!({"name":"legacy"})),
    );
    let read = |origin: &str, profile: &str, token: &str, marker: &str| {
        let mut api = Client::with_base_url(token.to_owned(), &ApiConfig::default(), false, origin)
            .with_profile_cache(open(), profile);
        let (paths, _) = api.tree(SLUG, HEAD).unwrap();
        assert_eq!(paths, [marker]);
        let mut api = api.with_http_cache();
        assert_eq!(api.rest(&path).unwrap()["name"], marker);
        api.requests()
    };
    assert_eq!(read(&base, "first", FAKE, "one"), 2);
    assert_eq!(read(&base, "first", FAKE, "one"), 0);
    assert_eq!(read(&base, "second", FAKE, "two"), 2);
    assert_eq!(read(&base, "first", "ghp_FAKE2345678901", "three"), 2);
    assert_eq!(read(&other_base, "first", FAKE, "four"), 2);
    assert_eq!(read(&base, "first", FAKE, "one"), 0);
    assert_eq!(server.join().unwrap().len(), 6);
    assert_eq!(other_server.join().unwrap().len(), 2);
}

#[test]
fn a_different_head_or_base_sha_is_a_miss() {
    let scratch = temp("files-miss");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![
        files_reply(&["a.rs"]),
        files_reply(&["a.rs"]),
        files_reply(&["a.rs"]),
    ]);
    let mut api = client(&base, &dir, CacheMode::Use);
    api.files(&key(), HEAD, BASE).unwrap();
    api.files(&key(), OTHER, BASE).unwrap();
    api.files(&key(), HEAD, OTHER).unwrap();
    assert_eq!(api.requests(), 3);
    assert_eq!(api.cache_hits(), 0);
    assert_eq!(api.cache_misses(), 3);
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn an_empty_sha_is_never_cached() {
    let scratch = temp("files-empty");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![
        files_reply(&["a.rs"]),
        files_reply(&["a.rs"]),
        files_reply(&["a.rs"]),
        files_reply(&["a.rs"]),
    ]);
    for (head, sha) in [(HEAD, ""), (HEAD, ""), ("", BASE), ("", BASE)] {
        let mut api = client(&base, &dir, CacheMode::Use);
        api.files(&key(), head, sha).unwrap();
        assert_eq!(api.requests(), 1);
    }
    assert_eq!(server.join().unwrap().len(), 4);
    assert!(!dir.exists());
}

#[test]
fn a_pr_that_needs_its_patch_is_not_cached() {
    let scratch = temp("files-patch");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![
        files_reply(&["package.json", "src/a.rs"]),
        files_reply(&["package.json", "src/a.rs"]),
    ]);
    for _ in 0..2 {
        let mut api = client(&base, &dir, CacheMode::Use).with_http_cache();
        let (files, _) = api.files(&key(), HEAD, BASE).unwrap();
        assert_eq!(api.requests(), 1);
        let manifest = files.iter().find(|f| f.path == "package.json").unwrap();
        assert!(manifest.patch.is_some());
    }
    assert_eq!(server.join().unwrap().len(), 2);
    assert!(!dir.exists());
}

#[test]
fn refresh_requests_and_rewrites_while_off_leaves_nothing() {
    let scratch = temp("files-modes");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![files_reply(&["a.rs"]), files_reply(&["a.rs", "b.rs"])]);
    client(&base, &dir, CacheMode::Use)
        .files(&key(), HEAD, BASE)
        .unwrap();
    let mut refresh = client(&base, &dir, CacheMode::Refresh);
    let (files, _) = refresh.files(&key(), HEAD, BASE).unwrap();
    assert_eq!(refresh.requests(), 1);
    assert_eq!(files.len(), 2);
    assert_eq!(server.join().unwrap().len(), 2);

    let mut after = client(&base, &dir, CacheMode::Use);
    let (rewritten, _) = after.files(&key(), HEAD, BASE).unwrap();
    assert_eq!(after.requests(), 0);
    assert_eq!(rewritten.len(), 2);

    let off_scratch = temp("files-off");
    let off_dir = off_scratch.join("cache");
    let (off_base, off_server) = serve(vec![files_reply(&["a.rs"])]);
    let mut off = client(&off_base, &off_dir, CacheMode::Off);
    off.files(&key(), HEAD, BASE).unwrap();
    assert_eq!(off.requests(), 1);
    assert_eq!(off_server.join().unwrap().len(), 1);
    assert!(!off_dir.exists());
}

#[test]
fn a_tree_round_trips_including_truncation() {
    let scratch = temp("tree");
    let dir = scratch.join("cache");
    let body = json!({
        "tree": [
            { "type": "blob", "path": "a/package.json" },
            { "type": "tree", "path": "a" },
            { "type": "blob", "path": "b/Cargo.toml" }
        ],
        "truncated": true
    })
    .to_string();
    let (base, server) = serve(vec![body]);
    let mut first = client(&base, &dir, CacheMode::Use);
    let fetched = first.tree(SLUG, HEAD).unwrap();
    assert_eq!(first.requests(), 1);
    server.join().unwrap();

    let mut second = client(&base, &dir, CacheMode::Use);
    let cached = second.tree(SLUG, HEAD).unwrap();
    assert_eq!(second.requests(), 0);
    assert_eq!(second.cache_hits(), 1);
    assert_eq!(fetched, cached);
    assert_eq!(cached.0, strings(&["a/package.json", "b/Cargo.toml"]));
    assert!(cached.1);
}

#[test]
fn blobs_fetch_only_the_missing_paths_and_remember_absent_ones() {
    let scratch = temp("blobs");
    let dir = scratch.join("cache");
    let (base, server) = serve(vec![
        blobs_reply(&[Some("one"), Some("two")]),
        blobs_reply(&[Some("three")]),
        blobs_reply(&[None]),
    ]);
    let mut api = client(&base, &dir, CacheMode::Use);
    let first = strings(&["a/package.json", "b/package.json"]);
    let got = api.blobs(SLUG, HEAD, &first).unwrap();
    assert_eq!(
        got,
        pairs(&[("a/package.json", "one"), ("b/package.json", "two")])
    );

    let all = strings(&["a/package.json", "b/package.json", "c/Cargo.toml"]);
    let got = api.blobs(SLUG, HEAD, &all).unwrap();
    let expected = pairs(&[
        ("a/package.json", "one"),
        ("b/package.json", "two"),
        ("c/Cargo.toml", "three"),
    ]);
    assert_eq!(got, expected);

    let gone = strings(&["a/package.json", "d/gone.json"]);
    let got = api.blobs(SLUG, HEAD, &gone).unwrap();
    assert_eq!(got, pairs(&[("a/package.json", "one")]));
    assert_eq!(api.requests(), 3);

    let seen = server.join().unwrap();
    assert!(seen[0].contains(&format!("{HEAD}:a/package.json")));
    assert!(seen[0].contains(&format!("{HEAD}:b/package.json")));
    assert!(seen[1].contains(&format!("{HEAD}:c/Cargo.toml")));
    assert!(!seen[1].contains("a/package.json"));
    assert!(!seen[1].contains("b/package.json"));
    assert!(seen[2].contains(&format!("{HEAD}:d/gone.json")));
    assert!(!seen[2].contains("a/package.json"));

    let mut offline = client(&base, &dir, CacheMode::Use);
    assert_eq!(offline.blobs(SLUG, HEAD, &all).unwrap(), expected);
    let again = offline.blobs(SLUG, HEAD, &gone).unwrap();
    assert_eq!(again, pairs(&[("a/package.json", "one")]));
    assert_eq!(offline.requests(), 0);
    assert_eq!(offline.cache_hits(), 2);
}

#[test]
fn malformed_tree_responses_are_incomplete() {
    let scratch = temp("malformed-tree");
    let (base, server) = serve(vec![
        json!({"tree":[]}).to_string(),
        json!({"truncated":false}).to_string(),
        json!({"truncated":false,"tree":[{"type":"blob"}]}).to_string(),
    ]);
    let mut api = client(&base, &scratch.join("cache"), CacheMode::Off);
    for _ in 0..3 {
        assert!(api.tree(SLUG, HEAD).unwrap().1);
    }
    server.join().unwrap();
}

#[test]
fn the_cache_directory_holds_no_token_header_or_patch() {
    let scratch = temp("clean");
    let dir = scratch.join("cache");
    let tree =
        json!({ "tree": [{ "type": "blob", "path": "a/package.json" }], "truncated": false });
    let (base, server) = serve(vec![
        files_reply(&["src/a.rs"]),
        tree.to_string(),
        blobs_reply(&[Some("{}")]),
    ]);
    let mut api = client(&base, &dir, CacheMode::Use);
    api.files(&key(), HEAD, BASE).unwrap();
    api.tree(SLUG, HEAD).unwrap();
    api.blobs(SLUG, HEAD, &strings(&["a/package.json"]))
        .unwrap();
    assert_eq!(server.join().unwrap().len(), 3);
    assert_clean(&dir);
}
