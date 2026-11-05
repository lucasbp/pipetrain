mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use pipetrain::command::profiles;
use pipetrain::model::PrKey;
use pipetrain::profile::{self, Inputs, ProfileSet, ProfileSource};
use serde_json::{Value, json};

const FAKE: &str = "ghp_FAKE0123456789";

const MULTI: &str = r#"
default_profile = "acme"

[profile.acme]
orgs = ["acme"]
folders = ["/work/acme"]

[profile.labs]
orgs = ["acme-labs"]
folders = ["/work/labs"]

[org_alias]
lab = "acme-labs"
Ac = "acme"
"#;

const FLAT: &str = r#"
orgs = ["acme"]

[org_alias]
zeta = "acme"
"#;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-org-alias-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn set(text: &str) -> ProfileSet {
    ProfileSet::parse(text).unwrap()
}

fn error_of(text: &str) -> String {
    format!("{:#}", ProfileSet::parse(text).unwrap_err())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn config_file(text: &str) -> PathBuf {
    let path = scratch("config").join("pipetrain.toml");
    fs::write(&path, text).unwrap();
    path
}

fn run(args: &[&str], config: &Path, api: &str, cache: &Path) -> Output {
    let home = scratch("home");
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(args)
        .arg("--config")
        .arg(config)
        .env("GH_TOKEN", FAKE)
        .env("PIPETRAIN_API_URL", api)
        .env("PIPETRAIN_CACHE_DIR", cache)
        .env("PIPETRAIN_LOG_DIR", scratch("log"))
        .env_remove("PIPETRAIN_LOG")
        .env("PATH", scratch("empty-path"))
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .current_dir(&home)
        .output()
        .unwrap()
}

fn pr_node() -> Value {
    json!({
        "number": 7,
        "title": "Add a thing",
        "url": "https://example.test/acme/api/pull/7",
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

fn answer(line: &str, body: &str) -> (u16, Value) {
    if line.starts_with("POST /graphql") {
        if body.contains("pullRequest(number") {
            return (
                200,
                json!({ "data": { "repository": { "pullRequest": pr_node() } } }),
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
        return (
            200,
            json!({ "resources": { "core": { "remaining": 4900, "limit": 5000, "reset": 4_000_000_000_i64 } } }),
        );
    }
    if line.contains("/pulls/7/files") {
        return (200, json!([]));
    }
    (404, json!({ "message": "not faked" }))
}

fn requests(fake: &common::Server) -> String {
    fake.seen.lock().unwrap().join("\n")
}

fn every_file_text(dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                found.push((path.display().to_string(), text));
            }
        }
    }
    found
}

#[test]
fn an_org_alias_resolves_case_insensitively() {
    let aliases = set(MULTI).org_aliases;
    assert_eq!(aliases.resolve("lab"), "acme-labs");
    assert_eq!(aliases.resolve("LAB"), "acme-labs");
    assert_eq!(aliases.resolve("ac"), "acme");
}

#[test]
fn a_name_that_is_not_an_alias_is_returned_as_given() {
    let aliases = set(MULTI).org_aliases;
    assert_eq!(aliases.resolve("acme"), "acme");
    assert_eq!(aliases.resolve("Globex"), "Globex");
}

#[test]
fn resolve_all_maps_every_org() {
    let aliases = set(MULTI).org_aliases;
    assert_eq!(
        aliases.resolve_all(&strings(&["lab", "globex", "AC"])),
        strings(&["acme-labs", "globex", "acme"])
    );
}

#[test]
fn an_alias_resolves_in_the_org_repo_number_form() {
    let aliases = set(MULTI).org_aliases;
    let resolved = aliases.resolve_ref("lab/api#1");
    assert_eq!(resolved, "acme-labs/api#1");
    let key = PrKey::parse(&resolved).unwrap();
    assert_eq!(
        (key.org.as_str(), key.repo.as_str(), key.number),
        ("acme-labs", "api", 1)
    );
}

#[test]
fn an_alias_resolves_in_the_github_url() {
    let aliases = set(MULTI).org_aliases;
    let resolved = aliases.resolve_ref("https://github.com/LAB/api/pull/1");
    assert_eq!(resolved, "https://github.com/acme-labs/api/pull/1");
    let key = PrKey::parse(&resolved).unwrap();
    assert_eq!(
        (key.org.as_str(), key.repo.as_str(), key.number),
        ("acme-labs", "api", 1)
    );
}

#[test]
fn a_reference_without_an_org_or_with_a_real_org_is_left_alone() {
    let aliases = set(MULTI).org_aliases;
    assert_eq!(aliases.resolve_ref("api#1"), "api#1");
    assert_eq!(aliases.resolve_ref("acme/api#1"), "acme/api#1");
    assert_eq!(
        aliases.resolve_refs(&strings(&["lab/api#1", "api#2"])),
        strings(&["acme-labs/api#1", "api#2"])
    );
}

#[test]
fn the_listing_shows_every_org_alias() {
    let aliases = set(MULTI).org_aliases;
    assert_eq!(
        aliases.listing().as_deref(),
        Some("Ac = acme, lab = acme-labs")
    );
    assert!(set("orgs = [\"acme\"]\n").org_aliases.listing().is_none());
    assert!(set("orgs = [\"acme\"]\n").org_aliases.is_empty());
}

#[test]
fn profile_selection_by_argument_uses_the_real_org_of_an_alias() {
    let set = set(MULTI);
    let orgs = set.org_aliases.resolve_all(&strings(&["lab"]));
    let inputs = Inputs {
        flag: None,
        arg_orgs: &orgs,
        cwd: Some("/elsewhere"),
        case_insensitive: false,
    };
    let found = profile::select(&set, &inputs).unwrap();
    assert_eq!(found.profile.name, "labs");
    assert_eq!(found.source, ProfileSource::Args);
}

#[test]
fn profile_selection_by_argument_does_not_know_an_unresolved_alias() {
    let set = set(MULTI);
    let orgs = strings(&["lab"]);
    let inputs = Inputs {
        flag: None,
        arg_orgs: &orgs,
        cwd: Some("/elsewhere"),
        case_insensitive: false,
    };
    let found = profile::select(&set, &inputs).unwrap();
    assert_eq!(found.source, ProfileSource::Default);
}

#[test]
fn an_org_alias_is_accepted_in_a_flat_config() {
    let aliases = set(FLAT).org_aliases;
    assert_eq!(aliases.resolve("zeta"), "acme");
}

#[test]
fn an_org_alias_that_shadows_a_real_org_of_a_profile_is_rejected() {
    let text = MULTI.replace("Ac = \"acme\"", "acme = \"acme-labs\"");
    let message = error_of(&text);
    assert!(
        message.contains(
            "org alias `acme` is also an org name in profile `acme`; an alias must not shadow a real org"
        ),
        "{message}"
    );
}

#[test]
fn an_org_alias_that_shadows_the_org_of_a_flat_config_is_rejected() {
    let message = error_of("orgs = [\"acme\"]\n\n[org_alias]\nACME = \"acme-labs\"\n");
    assert!(
        message.contains("org alias `ACME` is also an org name in profile `default`"),
        "{message}"
    );
}

#[test]
fn two_org_aliases_that_differ_only_by_case_are_rejected() {
    let message =
        error_of("orgs = [\"acme\"]\n\n[org_alias]\nlab = \"acme-labs\"\nLAB = \"globex\"\n");
    assert!(message.contains("is defined twice"), "{message}");
    assert!(
        message.contains("aliases are case-insensitive"),
        "{message}"
    );
}

#[test]
fn an_org_alias_pointing_to_another_alias_is_rejected() {
    let message = error_of("orgs = [\"acme\"]\n\n[org_alias]\na = \"b\"\nb = \"acme\"\n");
    assert!(
        message.contains("org alias `a` points to `b`, which is itself an alias (`b`)"),
        "{message}"
    );
}

#[test]
fn an_org_alias_pointing_to_itself_is_rejected() {
    let message = error_of("orgs = [\"acme\"]\n\n[org_alias]\nlab = \"LAB\"\n");
    assert!(
        message.contains("org alias `lab` points to itself"),
        "{message}"
    );
}

#[test]
fn malformed_org_alias_tables_are_rejected() {
    let not_a_table = error_of("orgs = [\"acme\"]\norg_alias = [\"lab\"]\n");
    assert!(
        not_a_table.contains("`org_alias` must be a table of alias = \"real-org-name\" entries"),
        "{not_a_table}"
    );
    let not_a_string = error_of("orgs = [\"acme\"]\n\n[org_alias]\nlab = 3\n");
    assert!(
        not_a_string.contains("`org_alias.lab` must be a string with the real org name"),
        "{not_a_string}"
    );
    let empty_real = error_of("orgs = [\"acme\"]\n\n[org_alias]\nlab = \"\"\n");
    assert!(
        empty_real.contains("`org_alias.lab` is empty; it must name the real org"),
        "{empty_real}"
    );
    let empty_alias = error_of("orgs = [\"acme\"]\n\n[org_alias]\n\"\" = \"acme\"\n");
    assert!(
        empty_alias.contains("`org_alias` has an empty alias"),
        "{empty_alias}"
    );
}

#[test]
fn the_table_prints_the_org_aliases_line_only_when_they_exist() {
    let with = profiles::table(&set(MULTI), None);
    assert_eq!(
        with.lines().last(),
        Some("org aliases: Ac = acme, lab = acme-labs"),
        "{with}"
    );
    let without = profiles::table(&set("orgs = [\"acme\"]\n"), None);
    assert!(!without.contains("org aliases"), "{without}");
}

#[test]
fn the_profiles_command_prints_the_org_aliases_line() {
    let fake = common::serve_requests(answer);
    let out = run(
        &["profiles"],
        &config_file(MULTI),
        &fake.base,
        &scratch("cache"),
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("org aliases: Ac = acme, lab = acme-labs"),
        "{text}"
    );
}

#[test]
fn plan_resolves_an_org_alias_and_keeps_the_real_name_in_the_json_and_the_cache() {
    let fake = common::serve_requests(answer);
    let cache = scratch("cache");
    let out = run(
        &["plan", "zeta/api#7", "--json", "--no-progress"],
        &config_file(FLAT),
        &fake.base,
        &cache,
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let errors = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{text}\n{errors}");
    let plan: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(plan["items"][0]["id"], "acme/api#7", "{text}");
    assert!(!text.contains("zeta"), "{text}");
    let seen = requests(&fake);
    assert!(seen.contains("acme"), "{seen}");
    assert!(!seen.contains("zeta"), "{seen}");
    let cached = every_file_text(&cache);
    assert!(!cached.is_empty(), "the cache was never written");
    for (path, content) in cached {
        assert!(!path.contains("zeta"), "{path}");
        assert!(!content.contains("zeta"), "{path}");
    }
}

#[test]
fn plan_resolves_an_org_alias_in_a_github_url() {
    let fake = common::serve_requests(answer);
    let out = run(
        &[
            "plan",
            "https://github.com/ZETA/api/pull/7",
            "--json",
            "--no-progress",
        ],
        &config_file(FLAT),
        &fake.base,
        &scratch("cache"),
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let errors = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{text}\n{errors}");
    let plan: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(plan["items"][0]["id"], "acme/api#7", "{text}");
    assert!(!requests(&fake).to_lowercase().contains("zeta"));
}

#[test]
fn doctor_resolves_an_org_alias_given_in_org() {
    let fake = common::serve_requests(answer);
    let out = run(
        &["doctor", "--color", "never", "--org", "zeta"],
        &config_file(FLAT),
        &fake.base,
        &scratch("cache"),
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let seen = requests(&fake);
    assert!(!text.to_lowercase().contains("zeta"), "{text}");
    assert!(!seen.to_lowercase().contains("zeta"), "{seen}");
    assert!(seen.contains("acme"), "{seen}\n{text}");
}
