mod common;

use pipetrain::consumer_bump::backend::{Invocation, ProcessRunner, Runner};
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn workspace(mode: &str) -> common::TempDir {
    let root = common::TempDir::new("process");
    fs::write(root.join("fixture-mode"), mode).unwrap();
    root
}

#[test]
fn owned_process_fixture() {
    let root = std::env::current_dir().unwrap();
    let Ok(mode) = fs::read_to_string(root.join("fixture-mode")) else {
        return;
    };
    if root.join("child-role").exists() {
        std::thread::sleep(Duration::from_millis(1500));
        fs::write(root.join("late-marker"), "descendant survived timeout").unwrap();
        return;
    }
    if mode == "normal" {
        println!("normal operation");
        return;
    }
    fs::write(root.join("child-role"), "owned descendant").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "owned_process_fixture", "--nocapture"])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    fs::write(root.join("descendant.pid"), child.id().to_string()).unwrap();
    let _waiter = std::thread::spawn(move || child.wait());
    if mode == "parent_waits" {
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn timeout_case(mode: &str) {
    let root = workspace(mode);
    fs::write(root.join("preserved-workspace"), "keep").unwrap();
    let started = Instant::now();
    let result = ProcessRunner.run(&Invocation {
        program: std::env::current_exe().unwrap().display().to_string(),
        args: vec![
            "--exact".into(),
            "owned_process_fixture".into(),
            "--nocapture".into(),
        ],
        directory: root.path().to_path_buf(),
        timeout: Duration::from_millis(600),
    });
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        root.join("descendant.pid").exists(),
        "fixture must spawn descendant"
    );
    std::thread::sleep(Duration::from_millis(1700));
    let survived = root.join("late-marker").exists();
    assert_eq!(
        fs::read_to_string(root.join("preserved-workspace")).unwrap(),
        "keep"
    );
    assert!(!survived, "descendant continued after runner timeout");
}

#[test]
fn timeout_after_parent_exit_terminates_descendant_holding_output_pipes() {
    timeout_case("parent_exits");
}

#[test]
fn timeout_while_parent_runs_terminates_descendants() {
    timeout_case("parent_waits");
}

#[test]
fn normal_owned_process_returns_output() {
    let root = workspace("normal");
    let result = ProcessRunner
        .run(&Invocation {
            program: std::env::current_exe().unwrap().display().to_string(),
            args: vec![
                "--exact".into(),
                "owned_process_fixture".into(),
                "--nocapture".into(),
            ],
            directory: root.path().to_path_buf(),
            timeout: Duration::from_secs(3),
        })
        .unwrap();
    assert!(result.contains("normal operation"));
}
