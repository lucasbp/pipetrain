use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(args)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap()
}

fn assert_conflict(args: &[&str]) {
    let out = run(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{args:?} should fail");
    assert!(
        stderr.contains("cannot be used with"),
        "{args:?} stderr: {stderr}"
    );
}

fn assert_help_lists_flags(args: &[&str]) {
    let out = run(args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{args:?} should succeed");
    assert!(stdout.contains("--no-cache"), "{args:?} stdout: {stdout}");
    assert!(stdout.contains("--refresh"), "{args:?} stdout: {stdout}");
}

#[test]
fn plan_rejects_no_cache_with_refresh() {
    assert_conflict(&["plan", "--no-cache", "--refresh"]);
}

#[test]
fn order_suggest_rejects_no_cache_with_refresh() {
    assert_conflict(&["order", "suggest", "--no-cache", "--refresh"]);
}

#[test]
fn plan_help_lists_the_cache_flags() {
    assert_help_lists_flags(&["plan", "--help"]);
}

#[test]
fn order_suggest_help_lists_the_cache_flags() {
    assert_help_lists_flags(&["order", "suggest", "--help"]);
}
