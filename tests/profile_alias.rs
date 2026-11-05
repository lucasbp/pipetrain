mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use pipetrain::command::profiles;
use pipetrain::profile::{self, Inputs, ProfileSet, ProfileSource};

const FAKE: &str = "ghp_FAKE0123456789";

const ALIASED: &str = r#"
default_profile = "a"

[profile.acme]
aliases = ["a", "Ac"]
orgs = ["acme"]
folders = ["/work/acme"]

[profile.globex]
aliases = ["gx"]
orgs = ["globex"]
folders = ["/work/globex"]
"#;

const PLAIN: &str = r#"
default_profile = "acme"

[profile.acme]
orgs = ["acme"]
folders = ["/work/acme"]

[profile.globex]
orgs = ["globex"]
folders = ["/work/globex"]
"#;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-profile-alias-{label}-{}-{}",
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

fn pick(set: &ProfileSet, flag: Option<&str>, cwd: Option<&str>) -> (String, ProfileSource) {
    let inputs = Inputs::new(flag, &[], cwd);
    let found = profile::select(set, &inputs).unwrap();
    (found.profile.name.clone(), found.source)
}

fn pick_error(set: &ProfileSet, flag: Option<&str>, cwd: Option<&str>) -> String {
    let inputs = Inputs::new(flag, &[], cwd);
    format!("{:#}", profile::select(set, &inputs).unwrap_err())
}

fn run(args: &[&str], config: &Path, api: &str) -> Output {
    let home = scratch("home");
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(args)
        .arg("--config")
        .arg(config)
        .env("GH_TOKEN", FAKE)
        .env("PIPETRAIN_API_URL", api)
        .env("PIPETRAIN_CACHE_DIR", scratch("cache"))
        .env("PIPETRAIN_LOG_DIR", scratch("log"))
        .env_remove("PIPETRAIN_LOG")
        .env("PATH", scratch("empty-path"))
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .current_dir(&home)
        .output()
        .unwrap()
}

fn config_file(text: &str) -> PathBuf {
    let path = scratch("config").join("pipetrain.toml");
    fs::write(&path, text).unwrap();
    path
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_profile_flag_accepts_an_alias() {
    let set = set(ALIASED);
    assert_eq!(
        pick(&set, Some("gx"), None),
        ("globex".to_string(), ProfileSource::Flag)
    );
}

#[test]
fn an_alias_is_case_insensitive() {
    let set = set(ALIASED);
    assert_eq!(pick(&set, Some("GX"), None).0, "globex");
    assert_eq!(pick(&set, Some("ac"), None).0, "acme");
    assert_eq!(pick(&set, Some("AC"), None).0, "acme");
}

#[test]
fn the_real_name_still_works_next_to_its_aliases() {
    let set = set(ALIASED);
    assert_eq!(pick(&set, Some("globex"), None).0, "globex");
    assert_eq!(pick(&set, Some("GLOBEX"), None).0, "globex");
}

#[test]
fn the_default_profile_can_be_an_alias() {
    let set = set(ALIASED);
    assert_eq!(set.default_profile.as_deref(), Some("a"));
    assert_eq!(
        pick(&set, None, Some("/elsewhere")),
        ("acme".to_string(), ProfileSource::Default)
    );
}

#[test]
fn a_default_profile_that_is_neither_a_name_nor_an_alias_is_rejected() {
    let text = ALIASED.replace("default_profile = \"a\"", "default_profile = \"zz\"");
    let message = error_of(&text);
    assert!(
        message.contains("`default_profile` names `zz`, which is not a profile"),
        "{message}"
    );
}

#[test]
fn an_alias_matching_another_profile_name_is_rejected() {
    let text = r#"
[profile.acme]
aliases = ["globex"]
orgs = ["acme"]

[profile.globex]
orgs = ["globex"]
"#;
    let message = error_of(text);
    assert!(
        message
            .contains("alias `globex` in profile `acme` is already the name of profile `globex`"),
        "{message}"
    );
}

#[test]
fn an_alias_matching_the_name_of_an_earlier_profile_names_both() {
    let text = r#"
[profile.acme]
orgs = ["acme"]

[profile.globex]
aliases = ["Acme"]
orgs = ["globex"]
"#;
    let message = error_of(text);
    assert!(
        message.contains("alias `Acme` in profile `globex` is already the name of profile `acme`"),
        "{message}"
    );
}

#[test]
fn an_alias_matching_an_alias_of_another_profile_names_both() {
    let text = r#"
[profile.acme]
aliases = ["x"]
orgs = ["acme"]

[profile.globex]
aliases = ["X"]
orgs = ["globex"]
"#;
    let message = error_of(text);
    assert!(
        message.contains("alias `X` in profile `globex` is already an alias of profile `acme`"),
        "{message}"
    );
}

#[test]
fn an_alias_listed_twice_in_one_profile_is_rejected() {
    let text = r#"
[profile.acme]
aliases = ["a", "A"]
orgs = ["acme"]

[profile.globex]
orgs = ["globex"]
"#;
    let message = error_of(text);
    assert!(
        message.contains("is listed twice in profile `acme`"),
        "{message}"
    );
}

#[test]
fn an_alias_equal_to_the_profile_own_name_is_rejected() {
    let text = r#"
[profile.acme]
aliases = ["ACME"]
orgs = ["acme"]

[profile.globex]
orgs = ["globex"]
"#;
    let message = error_of(text);
    assert!(
        message.contains("alias `ACME` in profile `acme` repeats the profile's own name"),
        "{message}"
    );
}

#[test]
fn aliases_outside_a_profile_table_are_rejected() {
    let flat = error_of("aliases = [\"a\"]\norgs = [\"acme\"]\n");
    assert!(
        flat.contains("`aliases` is only allowed inside a [profile.NAME] table"),
        "{flat}"
    );
    let shared = error_of(&format!("aliases = [\"a\"]\n{PLAIN}"));
    assert!(
        shared.contains("`aliases` is only allowed inside a [profile.NAME] table"),
        "{shared}"
    );
}

#[test]
fn aliases_must_be_a_list_of_non_empty_names() {
    let not_a_list = error_of("[profile.acme]\naliases = \"a\"\norgs = [\"acme\"]\n");
    assert!(
        not_a_list.contains("`profile.acme.aliases` must be a list of names"),
        "{not_a_list}"
    );
    let empty = error_of("[profile.acme]\naliases = [\"\"]\norgs = [\"acme\"]\n");
    assert!(
        empty.contains("`profile.acme.aliases` must hold non-empty names"),
        "{empty}"
    );
    let number = error_of("[profile.acme]\naliases = [1]\norgs = [\"acme\"]\n");
    assert!(
        number.contains("`profile.acme.aliases` must hold non-empty names"),
        "{number}"
    );
}

#[test]
fn the_listing_shows_the_aliases_of_each_profile() {
    let listing = set(ALIASED).listing();
    assert!(listing.contains("acme  orgs: acme"), "{listing}");
    assert!(listing.contains("aliases: a, Ac"), "{listing}");
    assert!(listing.contains("aliases: gx"), "{listing}");
}

#[test]
fn the_listing_without_aliases_has_no_aliases_line() {
    let listing = set(PLAIN).listing();
    assert!(!listing.contains("aliases"), "{listing}");
}

#[test]
fn the_unknown_profile_error_shows_the_aliases() {
    let message = pick_error(&set(ALIASED), Some("nope"), None);
    assert!(message.contains("unknown profile `nope`"), "{message}");
    assert!(message.contains("aliases: a, Ac"), "{message}");
    assert!(message.contains("aliases: gx"), "{message}");
}

#[test]
fn the_no_profile_chosen_error_shows_the_aliases() {
    let text = ALIASED.replace("default_profile = \"a\"\n", "");
    let message = pick_error(&set(&text), None, Some("/elsewhere"));
    assert!(message.contains("no profile could be chosen"), "{message}");
    assert!(message.contains("aliases: gx"), "{message}");
}

#[test]
fn the_table_has_an_aliases_column_when_a_profile_has_aliases() {
    let set = set(ALIASED);
    let picked = Some(("acme".to_string(), ProfileSource::Default));
    let table = profiles::table(&set, picked.as_ref());
    let lines: Vec<&str> = table.lines().collect();
    let header: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(header, ["NAME", "ALIASES", "ORGS", "FOLDERS"]);
    assert!(lines[1].starts_with("* acme"), "{table}");
    assert!(lines[1].contains("a, Ac"), "{table}");
    assert!(lines[2].contains("gx"), "{table}");
    assert!(!table.contains("org aliases:"), "{table}");
}

#[test]
fn a_profile_without_aliases_shows_none_in_the_aliases_column() {
    let text = r#"
[profile.acme]
aliases = ["a"]
orgs = ["acme"]
folders = ["/work/acme"]

[profile.globex]
orgs = ["globex"]
folders = ["/work/globex"]
"#;
    let table = profiles::table(&set(text), None);
    let globex = table.lines().find(|l| l.contains("globex")).unwrap();
    assert!(globex.contains("(none)"), "{table}");
}

#[test]
fn the_table_is_unchanged_when_no_profile_has_aliases() {
    let table = profiles::table(&set(PLAIN), None);
    let lines: Vec<&str> = table.lines().collect();
    let header: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(header, ["NAME", "ORGS", "FOLDERS"]);
    assert_eq!(lines.len(), 3);
    assert!(!table.contains("ALIASES"), "{table}");
    assert!(!table.contains("org aliases:"), "{table}");
}

#[test]
fn the_profiles_command_shows_the_aliases_and_picks_the_alias_default() {
    let config = config_file(ALIASED);
    let out = run(&["profiles"], &config, "http://127.0.0.1:1");
    let text = stdout(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("ALIASES"), "{text}");
    assert!(text.contains("a, Ac"), "{text}");
    assert!(text.contains("from here: acme ("), "{text}");
}

#[test]
fn the_profiles_command_without_aliases_has_no_aliases_column() {
    let config = config_file(PLAIN);
    let out = run(&["profiles"], &config, "http://127.0.0.1:1");
    let text = stdout(&out);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("ALIASES"), "{text}");
    assert!(text.contains("from here: acme ("), "{text}");
}

#[test]
fn doctor_accepts_an_alias_in_the_profile_flag() {
    let config = config_file(ALIASED);
    let out = run(
        &["doctor", "--color", "never", "--profile", "GX"],
        &config,
        &common::closed_base(),
    );
    let text = stdout(&out);
    assert!(
        text.lines()
            .any(|l| l.contains("profile globex (from --profile)")),
        "{text}"
    );
}

#[test]
fn doctor_lists_the_aliases_when_the_profile_is_unknown() {
    let config = config_file(ALIASED);
    let out = run(
        &["doctor", "--color", "never", "--profile", "nope"],
        &config,
        &common::closed_base(),
    );
    let text = stdout(&out);
    assert!(text.contains("unknown profile `nope`"), "{text}");
    assert!(text.contains("aliases: a, Ac"), "{text}");
    assert!(text.contains("aliases: gx"), "{text}");
}
