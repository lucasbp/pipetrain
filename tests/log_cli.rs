use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pipetrain-logcli-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn base(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pipetrain"));
    command
        .env_remove("PIPETRAIN_LOG")
        .env("PIPETRAIN_LOG_DIR", dir.join("logs"))
        .env("PIPETRAIN_NO_UPDATE_CHECK", "1")
        .env("GH_TOKEN", "ghp_FAKE0123456789");
    command
}

fn config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("sentinel-secret-folder.toml");
    fs::write(&path, body).unwrap();
    path
}

fn profiles(command: &mut Command, config: &Path) -> Output {
    command.arg("profiles").arg("--config").arg(config);
    command.output().unwrap()
}

fn lines(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("logs").join("log.jsonl"))
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn the_log_is_off_by_default() {
    let dir = scratch();
    let cfg = config(&dir, "orgs = [\"acme\"]\n");
    let out = profiles(&mut base(&dir), &cfg);
    assert!(out.status.success());
    assert!(!dir.join("logs").exists());
}

#[test]
fn the_env_turns_it_on_and_writes_only_the_allowed_fields() {
    let dir = scratch();
    let cfg = config(&dir, "orgs = [\"acme\"]\n");
    let out = profiles(base(&dir).env("PIPETRAIN_LOG", "1"), &cfg);
    assert!(out.status.success());
    let written = lines(&dir);
    assert_eq!(written.len(), 1);
    let value: Value = serde_json::from_str(&written[0]).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "cache_hit",
            "command",
            "duration_ms",
            "error",
            "exit",
            "requests",
            "time",
            "variant",
            "version"
        ]
    );
    assert_eq!(value["command"], "profiles");
    assert_eq!(value["exit"], 0);
    assert_eq!(value["error"], Value::Null);
}

#[test]
fn nothing_from_the_arguments_or_the_environment_reaches_the_line() {
    let dir = scratch();
    let out = base(&dir)
        .env("PIPETRAIN_LOG", "1")
        .args(["feed", "Sentinel.Package", "--version", "9.9.9-sentinel"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let written = lines(&dir).join("\n");
    assert!(!written.is_empty());
    for secret in [
        "Sentinel",
        "sentinel",
        "ghp_FAKE",
        "9.9.9",
        "--version",
        "required",
    ] {
        assert!(!written.contains(secret), "{secret} leaked: {written}");
    }
    let value: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(value["exit"], 1);
    assert_eq!(value["error"], "Other");
}

#[test]
fn a_typed_error_logs_only_its_type_and_variant() {
    let dir = scratch();
    let out = base(&dir)
        .env("PIPETRAIN_LOG", "1")
        .args(["update", "--check", "--api-url", "http://127.0.0.1:1/x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let value: Value = serde_json::from_str(&lines(&dir)[0]).unwrap();
    assert_eq!(value["command"], "update");
    assert_eq!(value["error"], "UpdateError");
    assert_eq!(value["variant"], "Network");
    assert!(!lines(&dir)[0].contains("127.0.0.1"));
}

#[test]
fn no_log_and_env_zero_win_over_everything() {
    let dir = scratch();
    let cfg = config(&dir, "log = true\n");
    let out = profiles(base(&dir).env("PIPETRAIN_LOG", "1").arg("--no-log"), &cfg);
    assert!(out.status.success());
    assert!(!dir.join("logs").exists());
    let out = profiles(base(&dir).env("PIPETRAIN_LOG", "0"), &cfg);
    assert!(out.status.success());
    assert!(!dir.join("logs").exists());
}

#[test]
fn log_true_in_the_config_turns_it_on() {
    let dir = scratch();
    let cfg = config(&dir, "log = true\n");
    let out = profiles(&mut base(&dir), &cfg);
    assert!(out.status.success());
    assert_eq!(lines(&dir).len(), 1);
}

#[test]
fn an_unwritable_log_dir_does_not_fail_the_command() {
    let dir = scratch();
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"x").unwrap();
    let cfg = config(&dir, "orgs = [\"acme\"]\n");
    let mut command = base(&dir);
    command
        .env("PIPETRAIN_LOG", "1")
        .env("PIPETRAIN_LOG_DIR", blocker.join("inner"));
    let out = profiles(&mut command, &cfg);
    assert!(out.status.success());
}

#[test]
fn the_log_command_is_not_logged_and_summarizes() {
    let dir = scratch();
    let cfg = config(&dir, "orgs = [\"acme\"]\n");
    for _ in 0..2 {
        profiles(base(&dir).env("PIPETRAIN_LOG", "1"), &cfg);
    }
    let text = base(&dir)
        .env("PIPETRAIN_LOG", "1")
        .args(["log", "--summary"])
        .output()
        .unwrap();
    assert!(text.status.success());
    let shown = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(shown.starts_with("2 runs"), "{shown}");
    assert!(shown.contains("profiles"));
    assert_eq!(lines(&dir).len(), 2);
    let json = base(&dir)
        .args(["log", "--summary", "--json"])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["runs"], 2);
    assert_eq!(value["commands"][0]["command"], "profiles");
}

#[test]
fn summarizing_without_a_log_says_how_to_turn_it_on() {
    let dir = scratch();
    let out = base(&dir).args(["log", "--summary"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PIPETRAIN_LOG=1"));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("0 runs"));
}
