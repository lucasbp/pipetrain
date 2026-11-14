use std::process::{Command, Output};

mod common;

fn empty_dir() -> common::TempDir {
    common::TempDir::new("progress")
}

fn run(args: &[&str]) -> Output {
    let dir = empty_dir();
    Command::new(env!("CARGO_BIN_EXE_pipetrain"))
        .args(args)
        .env_clear()
        .env("PATH", dir.path())
        .current_dir(dir.path())
        .output()
        .unwrap()
}

fn drew_nothing(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for text in [&stderr, &stdout] {
        assert!(!text.contains('\r'), "{text}");
        assert!(!text.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)));
    }
}

#[test]
fn plan_through_a_pipe_draws_nothing() {
    drew_nothing(&run(&["plan", "--org", "acme"]));
}

#[test]
fn order_suggest_through_a_pipe_draws_nothing() {
    drew_nothing(&run(&["order", "suggest", "acme/api", "acme/web"]));
}

#[test]
fn no_progress_is_accepted_by_plan_and_order_suggest() {
    for args in [
        &["plan", "--org", "acme", "--no-progress"][..],
        &["order", "suggest", "acme/api", "--no-progress"][..],
    ] {
        let output = run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("unexpected argument"),
            "{args:?}: {stderr}"
        );
        drew_nothing(&output);
    }
}

#[test]
fn help_lists_no_progress() {
    for args in [&["plan", "--help"][..], &["order", "suggest", "--help"][..]] {
        let output = run(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("--no-progress"), "{args:?}: {stdout}");
    }
}
