use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

fn empty_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pipetrain-doctor-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn doctor(args: &[&str]) -> Output {
    let dir = empty_dir("cli");
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .arg("doctor")
        .args(args)
        .env_clear()
        .env("PATH", &dir)
        .current_dir(&dir)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn without_a_token_the_exit_code_is_one() {
    let output = doctor(&["--color", "never"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn color_never_prints_no_ansi_codes() {
    let text = stdout(&doctor(&["--color", "never"]));
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("[fail] token"));
    assert!(text.contains("[warn] api"));
}

#[test]
fn color_always_prints_ansi_codes() {
    let text = stdout(&doctor(&["--color", "always"]));
    assert!(text.contains('\u{1b}'));
}

#[test]
fn json_prints_the_documented_shape_without_ansi() {
    let output = doctor(&["--json"]);
    let text = stdout(&output);
    assert!(!text.contains('\u{1b}'));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["checks"][0]["id"], "token");
    assert_eq!(value["checks"][0]["status"], "fail");
    assert!(value["checks"][0]["fix"].is_string());
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn json_and_color_cannot_be_combined() {
    let output = doctor(&["--json", "--color", "always"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn an_org_flag_adds_a_skipped_line_when_there_is_no_token() {
    let text = stdout(&doctor(&["--color", "never", "--org", "acme"]));
    assert!(text.contains("org:acme"));
    assert!(text.contains("skipped"));
}
