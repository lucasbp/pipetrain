use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

pub fn interrupt_deploy(
    command: &mut Command,
    state: &Path,
    target: &str,
    timeout: Duration,
) -> String {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let sha = loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("merge exited {status} before the expected deploy was pending for {target}");
        }
        if let Some(pending) = fs::read(state)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|state| {
                state["pending"]
                    .as_array()?
                    .iter()
                    .find(|p| p["id"] == target)
                    .cloned()
            })
            && pending["deploy_prediction"]["status"] == "expected"
            && let Some(sha) = pending["sha"].as_str()
        {
            break Some(sha.to_owned());
        }
        if started.elapsed() >= timeout {
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    sha.unwrap_or_else(|| panic!("no durable expected deploy for {target} within {timeout:?}"))
}
