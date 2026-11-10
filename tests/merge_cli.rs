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
fn merge_help_lists_apply_gates_and_timeout() {
    let out = run(&["merge", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("--apply"), "stdout: {stdout}");
    assert!(stdout.contains("--gates-confirmed"), "stdout: {stdout}");
    assert!(stdout.contains("--timeout-minutes"), "stdout: {stdout}");
}

#[test]
fn yes_without_apply_is_a_usage_error() {
    let out = run(&["merge", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("--apply"), "stderr: {stderr}");
}

#[test]
fn gates_confirmed_without_apply_is_a_usage_error() {
    let out = run(&["merge", "--gates-confirmed"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("--apply"), "stderr: {stderr}");
}

#[test]
fn an_unknown_merge_method_is_a_usage_error() {
    let out = run(&["merge", "--apply", "--method", "fast-forward"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("--method"), "stderr: {stderr}");
}

#[test]
fn plan_help_does_not_offer_the_merge_flags() {
    let out = run(&["plan", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(!stdout.contains("--gates-confirmed"), "stdout: {stdout}");
}

#[test]
fn dependency_failure_accepts_only_stop_or_skip() {
    for value in ["stop", "skip"] {
        let out = run(&["merge", "--dependency-failure", value, "--help"]);
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("--dependency-failure"));
    }
    let out = run(&["merge", "--dependency-failure", "continue"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn merge_bump_is_explicit_opt_in_and_conflicts_with_no_bump() {
    for option in ["--bump", "--no-bump"] {
        assert!(run(&["merge", option, "--help"]).status.success());
    }
    let out = run(&["merge", "--bump", "--no-bump"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used"));
}
