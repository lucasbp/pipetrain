use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(args)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn yes_without_apply_is_a_usage_error() {
    let out = run(&["bundle", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("--apply"), "stderr: {stderr}");
}

#[test]
fn apply_without_a_terminal_is_refused_before_the_token() {
    let out = run(&["bundle", "--apply"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr.contains("refusing to write without a terminal: pass --yes"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("no GitHub token"), "stderr: {stderr}");
}

#[test]
fn bundle_help_lists_apply_and_yes() {
    let out = run(&["bundle", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("--apply"), "stdout: {stdout}");
    assert!(stdout.contains("--yes"), "stdout: {stdout}");
}

#[test]
fn plan_help_does_not_offer_apply() {
    let out = run(&["plan", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(!stdout.contains("--apply"), "stdout: {stdout}");
}
