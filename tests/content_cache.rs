use std::fs;
use std::path::{Path, PathBuf};

mod common;

use common::serve_bodies as serve;
use pipetrain::cache::{Cache, CacheMode};
use pipetrain::config::ApiConfig;
use pipetrain::content::{self, Check};
use pipetrain::github::Client;
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SLUG: &str = "acme/api";

fn temp(name: &str) -> common::TempDir {
    common::TempDir::new(&format!("content-test-{name}"))
}

fn client(base: &str, dir: &Path, mode: CacheMode) -> Client {
    let cfg = ApiConfig {
        max_requests: 50,
        max_per_minute: 1000,
        min_remaining: 0,
    };
    Client::with_base_url(FAKE.to_string(), &cfg, false, base)
        .with_cache(Cache::open(mode, Some(dir.to_path_buf())))
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

fn wanted(paths: &[&str]) -> Vec<Check> {
    vec![Check {
        slug: SLUG.to_string(),
        reference: "main".to_string(),
        sha: HEAD.to_string(),
        paths: paths.iter().map(|p| p.to_string()).collect(),
    }]
}

fn reply(entries: &[Option<&str>]) -> String {
    let mut data = serde_json::Map::new();
    for (i, oid) in entries.iter().enumerate() {
        let object = oid.map_or(Value::Null, |o| json!({ "oid": o }));
        data.insert(format!("p{i}"), json!({ "object": object }));
    }
    json!({ "data": data }).to_string()
}

fn run(
    dir: &Path,
    mode: CacheMode,
    replies: Vec<String>,
    checks: &[Check],
) -> (Vec<bool>, Vec<String>) {
    let (base, handle) = serve(replies);
    let mut github = client(&base, dir, mode);
    let verdicts = content::check(&mut github, checks).unwrap();
    (verdicts, handle.join().unwrap())
}

fn probe_count(request: &str) -> usize {
    request.matches("object(expression").count()
}

fn total_probes(requests: &[String]) -> usize {
    requests.iter().map(|r| probe_count(r)).sum()
}

fn asks_fixed(request: &str, path: &str) -> bool {
    request.contains(&format!("{HEAD}:{path}"))
}

fn asks_live(request: &str, path: &str) -> bool {
    request.contains(&format!("main:{path}"))
}

#[test]
fn second_run_skips_the_fixed_probe() {
    let scratch = temp("second");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/a.rs"]);
    let full = reply(&[Some("c1"), Some("aaa"), Some("aaa")]);
    let (first, seen) = run(&dir, CacheMode::Use, vec![full], &checks);
    assert_eq!(first, vec![true]);
    assert_eq!(probe_count(&seen[0]), 3);
    let live = reply(&[Some("c1"), Some("aaa")]);
    let (second, seen) = run(&dir, CacheMode::Use, vec![live], &checks);
    assert_eq!(second, first);
    assert_eq!(probe_count(&seen[0]), 2);
    assert!(!asks_fixed(&seen[0], "src/a.rs"));
    assert!(asks_live(&seen[0], "src/a.rs"));
}

#[test]
fn deleted_path_is_cached_as_null() {
    let scratch = temp("deleted");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/gone.rs"]);
    let full = reply(&[Some("c1"), Some("aaa"), None]);
    let (first, seen) = run(&dir, CacheMode::Use, vec![full], &checks);
    assert_eq!(first, vec![false]);
    assert_eq!(probe_count(&seen[0]), 3);
    let live = reply(&[Some("c1"), Some("aaa")]);
    let (second, seen) = run(&dir, CacheMode::Use, vec![live], &checks);
    assert_eq!(second, vec![false]);
    assert_eq!(probe_count(&seen[0]), 2);
    assert!(!asks_fixed(&seen[0], "src/gone.rs"));
}

#[test]
fn cached_oid_never_gives_a_stale_true() {
    let scratch = temp("stale");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/a.rs"]);
    let full = reply(&[Some("c1"), Some("aaa"), Some("aaa")]);
    let (first, _) = run(&dir, CacheMode::Use, vec![full], &checks);
    assert_eq!(first, vec![true]);
    let moved = reply(&[Some("c2"), Some("bbb")]);
    let (second, seen) = run(&dir, CacheMode::Use, vec![moved], &checks);
    assert_eq!(second, vec![false]);
    assert_eq!(probe_count(&seen[0]), 2);
}

#[test]
fn off_mode_always_sends_every_probe() {
    let scratch = temp("off");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/a.rs"]);
    for _ in 0..2 {
        let full = reply(&[Some("c1"), Some("aaa"), Some("aaa")]);
        let (verdicts, seen) = run(&dir, CacheMode::Off, vec![full], &checks);
        assert_eq!(verdicts, vec![true]);
        assert_eq!(probe_count(&seen[0]), 3);
        assert!(asks_fixed(&seen[0], "src/a.rs"));
    }
}

#[test]
fn refresh_mode_asks_everything_and_rewrites() {
    let scratch = temp("refresh");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/a.rs"]);
    let seed = reply(&[Some("c1"), Some("aaa"), Some("aaa")]);
    run(&dir, CacheMode::Use, vec![seed], &checks);
    let fresh = reply(&[Some("c1"), Some("ddd"), Some("ddd")]);
    let (refreshed, seen) = run(&dir, CacheMode::Refresh, vec![fresh], &checks);
    assert_eq!(refreshed, vec![true]);
    assert_eq!(probe_count(&seen[0]), 3);
    let again = reply(&[Some("c1"), Some("eee"), Some("eee")]);
    let (second, seen) = run(&dir, CacheMode::Refresh, vec![again], &checks);
    assert_eq!(second, vec![true]);
    assert_eq!(probe_count(&seen[0]), 3);
    let live = reply(&[Some("c1"), Some("eee")]);
    let (used, seen) = run(&dir, CacheMode::Use, vec![live], &checks);
    assert_eq!(used, vec![true]);
    assert_eq!(probe_count(&seen[0]), 2);
}

fn paths_of(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("src/f{i}.rs")).collect()
}

fn oid_for(index: usize, bad: Option<usize>, live: bool) -> String {
    if live && Some(index) == bad {
        format!("x{index}")
    } else {
        format!("o{index}")
    }
}

fn first_run_replies(count: usize, bad: Option<usize>) -> Vec<String> {
    let all: Vec<String> = (0..1 + 2 * count)
        .map(|g| match g {
            0 => "c1".to_string(),
            g => oid_for((g - 1) / 2, bad, g % 2 == 1),
        })
        .collect();
    all.chunks(50)
        .map(|chunk| {
            let entries: Vec<Option<&str>> = chunk.iter().map(|s| Some(s.as_str())).collect();
            reply(&entries)
        })
        .collect()
}

fn second_run_reply(count: usize, bad: Option<usize>) -> String {
    let all: Vec<String> = (0..1 + count)
        .map(|g| match g {
            0 => "c1".to_string(),
            g => oid_for(g - 1, bad, true),
        })
        .collect();
    let entries: Vec<Option<&str>> = all.iter().map(|s| Some(s.as_str())).collect();
    reply(&entries)
}

fn many(bad: Option<usize>, name: &str) {
    let scratch = temp(name);
    let dir = scratch.join("cache");
    let owned = paths_of(30);
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let checks = wanted(&names);
    let (first, seen) = run(&dir, CacheMode::Use, first_run_replies(30, bad), &checks);
    assert_eq!(seen.len(), 2);
    assert_eq!(total_probes(&seen), 61);
    let (second, seen) = run(
        &dir,
        CacheMode::Use,
        vec![second_run_reply(30, bad)],
        &checks,
    );
    assert_eq!(seen.len(), 1);
    assert_eq!(total_probes(&seen), 31);
    assert_eq!(second, first);
    assert_eq!(first, vec![bad.is_none()]);
}

#[test]
fn many_paths_ask_fewer_probes_with_the_same_verdict() {
    many(None, "many");
}

#[test]
fn many_paths_with_a_changed_file_stay_false() {
    many(Some(7), "many-bad");
}

#[test]
fn cache_files_hold_no_token() {
    let scratch = temp("secret");
    let dir = scratch.join("cache");
    let checks = wanted(&["src/a.rs"]);
    let full = reply(&[Some("c1"), Some("aaa"), Some("aaa")]);
    run(&dir, CacheMode::Use, vec![full], &checks);
    let mut files = Vec::new();
    walk(&dir, &mut files);
    assert!(!files.is_empty());
    for file in files {
        let text = fs::read_to_string(&file).unwrap().to_lowercase();
        assert!(!text.contains(&FAKE.to_lowercase()));
        assert!(!text.contains("authorization"));
    }
}
