mod common;

use pipetrain::merge::Store;

use common::TempDir;

#[test]
fn state_lock_is_exclusive_between_processes() {
    let directory = TempDir::new("state-process");
    let path = directory.join("state.json");
    let mut store = Store::open(Some(path.clone())).unwrap();
    store.lock().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "state_lock_child_probe", "--nocapture"])
        .env("PIPETRAIN_TEST_STATE_LOCK", &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
fn state_lock_child_probe() {
    let Some(path) = std::env::var_os("PIPETRAIN_TEST_STATE_LOCK") else {
        return;
    };
    let mut store = Store::open(Some(path.into())).unwrap();
    let error = store.lock().unwrap_err().to_string();
    assert!(
        error.contains("another merge execution is active"),
        "{error}"
    );
    assert!(error.contains("state.json.lock"), "{error}");
}
