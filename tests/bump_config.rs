use pipetrain::config::Config;
use pipetrain::consumer_bump::RestoreMode;

const TEXT: &str = r#"
orgs = ["acme"]

[bump]
restore_mode = "unlocked"

[bump.repos."acme/api"]
restore_mode = "skip"
solution = "src/Api.slnx"

[bump.repos."acme/worker"]
solution = "Worker.sln"
"#;

fn config() -> Config {
    Config::parse(TEXT).unwrap()
}

#[test]
fn the_default_without_a_bump_table_is_locked_with_no_solution() {
    let cfg = Config::parse("orgs = [\"acme\"]\n").unwrap();
    let settings = cfg.bump.settings_for("acme/api", None, None);
    assert_eq!(settings.mode, RestoreMode::Locked);
    assert_eq!(settings.solution, None);
}

#[test]
fn a_repo_entry_beats_the_global_mode_and_the_global_mode_beats_the_default() {
    let cfg = config();
    let api = cfg.bump.settings_for("acme/api", None, None);
    assert_eq!(api.mode, RestoreMode::Skip);
    assert_eq!(api.solution.as_deref(), Some("src/Api.slnx"));
    let worker = cfg.bump.settings_for("acme/worker", None, None);
    assert_eq!(worker.mode, RestoreMode::Unlocked);
    assert_eq!(worker.solution.as_deref(), Some("Worker.sln"));
    let other = cfg.bump.settings_for("acme/other", None, None);
    assert_eq!(other.mode, RestoreMode::Unlocked);
    assert_eq!(other.solution, None);
}

#[test]
fn the_command_line_beats_the_config_and_the_repo_key_ignores_case() {
    let cfg = config();
    let settings = cfg
        .bump
        .settings_for("ACME/API", Some(RestoreMode::Locked), Some("B.sln"));
    assert_eq!(settings.mode, RestoreMode::Locked);
    assert_eq!(settings.solution.as_deref(), Some("B.sln"));
    let from_config = cfg.bump.settings_for("Acme/Api", None, None);
    assert_eq!(from_config.mode, RestoreMode::Skip);
}

#[test]
fn an_unknown_mode_or_key_in_the_bump_table_is_rejected() {
    assert!(Config::parse("[bump]\nrestore_mode = \"fast\"\n").is_err());
    assert!(Config::parse("[bump]\nmode = \"skip\"\n").is_err());
}
