use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

mod common;

use pipetrain::cache::{Cache, CacheMode, prune, resolve_dir};
use serde_json::{Value, json};

struct Scratch {
    base: common::TempDir,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let base = common::TempDir::new(&format!("cache-test-{name}"));
        Self { base }
    }

    fn dir(&self) -> PathBuf {
        self.base.join("cache")
    }

    fn entry(&self, file: &str) -> PathBuf {
        self.dir().join("acme").join("api").join(file)
    }
}

fn open(scratch: &Scratch, mode: CacheMode) -> Cache {
    Cache::open(mode, Some(scratch.dir()))
}

fn age(path: &Path, secs: u64) {
    let when = SystemTime::now() - Duration::from_secs(secs);
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(when).unwrap();
}

fn write_file(path: &Path, bytes: usize, age_secs: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; bytes]).unwrap();
    age(path, age_secs);
}

#[test]
fn round_trip_hit() {
    let s = Scratch::new("round-trip");
    let mut cache = open(&s, CacheMode::Use);
    let data = json!({"number": 7, "title": "x"});
    cache.put("acme/api", "pulls", &["7", "abc123"], &data);
    assert_eq!(cache.get("acme/api", "pulls", &["7", "abc123"]), Some(data));
    assert_eq!((cache.hits(), cache.misses()), (1, 0));
    assert!(s.entry("pulls-7-abc123.json").is_file());
}

#[test]
fn slug_is_case_insensitive() {
    let s = Scratch::new("case");
    let mut cache = open(&s, CacheMode::Use);
    cache.put("Acme/API", "pulls", &["1"], &json!(1));
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), Some(json!(1)));
}

#[test]
fn miss_counts() {
    let s = Scratch::new("miss");
    let mut cache = open(&s, CacheMode::Use);
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
    assert_eq!(cache.get("acme/api", "pulls", &["2"]), None);
    assert_eq!((cache.hits(), cache.misses()), (0, 2));
}

#[test]
fn refresh_never_reads_but_writes() {
    let s = Scratch::new("refresh");
    let mut seed = open(&s, CacheMode::Use);
    seed.put("acme/api", "pulls", &["1"], &json!("old"));
    let mut refresh = open(&s, CacheMode::Refresh);
    assert_eq!(refresh.get("acme/api", "pulls", &["1"]), None);
    assert_eq!((refresh.hits(), refresh.misses()), (0, 1));
    refresh.put("acme/api", "pulls", &["1"], &json!("new"));
    let mut next = open(&s, CacheMode::Use);
    assert_eq!(next.get("acme/api", "pulls", &["1"]), Some(json!("new")));
    assert_eq!((next.hits(), next.misses()), (1, 0));
}

#[test]
fn off_never_touches_disk() {
    let s = Scratch::new("off");
    let mut cache = open(&s, CacheMode::Off);
    cache.put("acme/api", "pulls", &["1"], &json!(1));
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
    assert_eq!((cache.hits(), cache.misses()), (0, 0));
    assert!(!s.dir().exists());
}

#[test]
fn disabled_never_touches_disk() {
    let mut cache = Cache::disabled();
    cache.put("acme/api", "pulls", &["1"], &json!(1));
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
    assert_eq!((cache.hits(), cache.misses()), (0, 0));
    let mut no_dir = Cache::open(CacheMode::Use, None);
    no_dir.put("acme/api", "pulls", &["1"], &json!(1));
    assert_eq!(no_dir.get("acme/api", "pulls", &["1"]), None);
    assert_eq!((no_dir.hits(), no_dir.misses()), (0, 0));
}

#[test]
fn corrupt_file_is_miss_then_overwritten() {
    let s = Scratch::new("corrupt");
    let path = s.entry("pulls-1.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"{not json").unwrap();
    let mut cache = open(&s, CacheMode::Use);
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
    assert_eq!(cache.misses(), 1);
    cache.put("acme/api", "pulls", &["1"], &json!([1, 2]));
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), Some(json!([1, 2])));
}

#[test]
fn wrong_version_is_miss() {
    let s = Scratch::new("version");
    let path = s.entry("pulls-1.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, br#"{"v":2,"data":{"a":1}}"#).unwrap();
    let mut cache = open(&s, CacheMode::Use);
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
    assert_eq!((cache.hits(), cache.misses()), (0, 1));
}

#[test]
fn entry_format_is_versioned_envelope() {
    let s = Scratch::new("format");
    let mut cache = open(&s, CacheMode::Use);
    cache.put("acme/api", "pulls", &[], &json!({"a": 1}));
    let text = fs::read_to_string(s.entry("pulls.json")).unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, json!({"v": 1, "data": {"a": 1}}));
}

fn assert_ignored(s: &Scratch, slug: &str, endpoint: &str, parts: &[&str]) {
    let mut cache = open(s, CacheMode::Use);
    cache.put(slug, endpoint, parts, &json!(1));
    assert_eq!(cache.get(slug, endpoint, parts), None);
    assert_eq!((cache.hits(), cache.misses()), (0, 0));
    assert!(!s.dir().exists());
    assert!(!s.base.join("x").exists());
}

#[test]
fn invalid_slug_is_ignored() {
    let s = Scratch::new("bad-slug");
    for slug in [
        "../x/y",
        "acme",
        "acme/",
        "/api",
        "acme/a/b",
        "./api",
        "acme/..",
        "acme/...",
        "ac me/api",
        "acme/api\\x",
        "acme/ap\u{e9}",
        "",
    ] {
        assert_ignored(&s, slug, "pulls", &["1"]);
    }
}

#[test]
fn invalid_endpoint_is_ignored() {
    let s = Scratch::new("bad-endpoint");
    for endpoint in ["", "../pulls", "Pulls", "pul-ls", "pulls.json", "pul/ls"] {
        assert_ignored(&s, "acme/api", endpoint, &["1"]);
    }
}

#[test]
fn invalid_part_is_ignored() {
    let s = Scratch::new("bad-part");
    for part in ["a/b", "a..b", "", "a-b", "a\\b", "a:b", "..", "a b"] {
        assert_ignored(&s, "acme/api", "pulls", &["1", part]);
    }
}

#[test]
fn oversize_entry_is_not_written() {
    let s = Scratch::new("oversize");
    let mut cache = open(&s, CacheMode::Use);
    cache.put(
        "acme/api",
        "pulls",
        &["1"],
        &json!("x".repeat(2 * 1024 * 1024 + 1)),
    );
    assert!(!s.entry("pulls-1.json").exists());
    assert_eq!(cache.get("acme/api", "pulls", &["1"]), None);
}

#[test]
fn no_temp_file_is_left_after_put() {
    let s = Scratch::new("temp");
    let mut cache = open(&s, CacheMode::Use);
    for n in 0..5 {
        cache.put("acme/api", "pulls", &[&n.to_string()], &json!(n));
    }
    cache.put("acme/api", "pulls", &["0"], &json!("again"));
    let names: Vec<String> = fs::read_dir(s.dir().join("acme").join("api"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 5);
    assert!(names.iter().all(|n| !n.starts_with(".tmp-")));
}

#[test]
fn prune_removes_old_files_and_keeps_fresh() {
    let s = Scratch::new("prune-age");
    let old = s.entry("pulls-old.json");
    let fresh = s.entry("pulls-fresh.json");
    write_file(&old, 10, 3 * 86_400);
    write_file(&fresh, 10, 60);
    prune(&s.dir(), Duration::from_secs(86_400), u64::MAX);
    assert!(!old.exists());
    assert!(fresh.exists());
}

#[test]
fn prune_trims_to_cap_oldest_first() {
    let s = Scratch::new("prune-cap");
    let names = ["a", "b", "c", "d"];
    for (i, name) in names.iter().enumerate() {
        let file = format!("pulls-{name}.json");
        write_file(&s.entry(&file), 100, 1000 - (i as u64) * 100);
    }
    prune(&s.dir(), Duration::from_secs(86_400), 250);
    assert!(!s.entry("pulls-a.json").exists());
    assert!(!s.entry("pulls-b.json").exists());
    assert!(s.entry("pulls-c.json").exists());
    assert!(s.entry("pulls-d.json").exists());
}

#[test]
fn prune_removes_temp_files_and_empty_dirs() {
    let s = Scratch::new("prune-tmp");
    let temp = s.entry(".tmp-1-1");
    write_file(&temp, 5, 1);
    let lone = s.dir().join("acme").join("other").join("pulls-1.json");
    write_file(&lone, 5, 3 * 86_400);
    prune(&s.dir(), Duration::from_secs(86_400), u64::MAX);
    assert!(!temp.exists());
    assert!(!s.dir().join("acme").exists());
    assert!(s.dir().exists());
}

#[test]
fn prune_ignores_missing_dir() {
    let s = Scratch::new("prune-missing");
    prune(&s.dir(), Duration::from_secs(1), 1);
    assert!(!s.dir().exists());
}

#[test]
fn open_prunes_old_entries() {
    let s = Scratch::new("open-prune");
    let old = s.entry("pulls-old.json");
    write_file(&old, 10, 30 * 86_400);
    let _cache = open(&s, CacheMode::Use);
    assert!(!old.exists());
}

#[test]
fn open_off_does_not_prune() {
    let s = Scratch::new("off-prune");
    let old = s.entry("pulls-old.json");
    write_file(&old, 10, 30 * 86_400);
    let _cache = open(&s, CacheMode::Off);
    assert!(old.exists());
}

#[test]
fn resolve_dir_env_wins() {
    let got = resolve_dir(Some("custom".into()), Some(PathBuf::from("base")));
    assert_eq!(got, Some(PathBuf::from("custom")));
}

#[test]
fn resolve_dir_empty_env_falls_back() {
    let got = resolve_dir(Some(String::new()), Some(PathBuf::from("base")));
    assert_eq!(got, Some(PathBuf::from("base").join("pipetrain")));
}

#[test]
fn resolve_dir_without_cache_dir_is_none() {
    assert_eq!(resolve_dir(None, None), None);
    assert_eq!(resolve_dir(Some(String::new()), None), None);
}

#[test]
fn resolve_dir_fallback_appends_pipetrain() {
    let got = resolve_dir(None, Some(PathBuf::from("base")));
    assert_eq!(got, Some(PathBuf::from("base").join("pipetrain")));
}

#[test]
fn from_flags_truth_table() {
    assert_eq!(CacheMode::from_flags(false, false), CacheMode::Use);
    assert_eq!(CacheMode::from_flags(false, true), CacheMode::Refresh);
    assert_eq!(CacheMode::from_flags(true, false), CacheMode::Off);
    assert_eq!(CacheMode::from_flags(true, true), CacheMode::Off);
}
