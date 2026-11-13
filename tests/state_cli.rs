use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use pipetrain::merge::Store;
use serde_json::{Value, json};

struct Fixture {
    directory: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "pipetrain-state-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let state = directory.join("state.json");
        let config = directory.join("config.toml");
        fs::write(&config, "[profile.acme]\norgs = [\"acme\"]\n[profile.acme.merge]\nstate_file = \"state.json\"\nstale_pending_hours = 1\n").unwrap();
        Self {
            directory,
            state,
            config,
        }
    }

    fn seed(&self) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": 2,
            "profile": "acme",
            "created_at": "2000-01-01T00:00:00Z",
            "updated_at": "2000-01-02T00:00:00Z",
            "queue_id": "queue-original",
            "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": "abcdef123456", "recorded_at": "2000-01-01T00:00:00Z"}],
            "deploy_prs": [{"slug": "acme/api", "branch": "deploy/old", "ids": ["acme/api#1"]}],
            "active_jobs": ["step:0"],
            "audit": [{"id": "acme/api#0", "repo": "acme/api", "base": "main", "sha": null, "action": "discarded", "reason": "cancelled", "recorded_at": "2000-01-01T00:00:00Z"}]
        })).unwrap();
        fs::write(&self.state, &bytes).unwrap();
        bytes
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pipetrain"))
            .arg("state")
            .args(args)
            .arg("--config")
            .arg(&self.config)
            .args(["--profile", "acme"])
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env("PIPETRAIN_LOG", "0")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn read(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn state_show_json_exposes_documented_fields_without_mutating_or_locking() {
    let fixture = Fixture::new();
    let bytes = fixture.seed();
    let out = fixture.run(&["show", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    for field in [
        "path",
        "schema",
        "created_at",
        "updated_at",
        "profile",
        "selected_profile",
        "queue_id",
        "file_age_seconds",
        "file_age",
        "stale_pending_hours",
        "pending",
        "deploy_prs",
        "active_jobs",
        "audit",
    ] {
        assert!(value.get(field).is_some(), "{field}");
    }
    assert_eq!(value["schema"], 2);
    assert_eq!(
        PathBuf::from(value["path"].as_str().unwrap()),
        fixture.state
    );
    assert_eq!(value["profile"], "acme");
    assert_eq!(value["selected_profile"], "acme");
    assert_eq!(value["queue_id"], "queue-original");
    assert_eq!(value["pending"][0]["repo"], "acme/api");
    assert_eq!(value["pending"][0]["sha"], "abcdef123456");
    assert!(value["pending"][0]["age_seconds"].as_u64().unwrap() > 3600);
    assert_eq!(value["pending"][0]["stale"], true);
    assert_eq!(value["deploy_prs"][0]["branch"], "deploy/old");
    assert_eq!(value["active_jobs"], json!(["step:0"]));
    assert_eq!(value["audit"][0]["action"], "discarded");
    assert_eq!(fs::read(&fixture.state).unwrap(), bytes);
    assert!(!fixture.state.with_file_name("state.json.lock").exists());
    let text = fixture.run(&["show"]);
    let text = String::from_utf8(text.stdout).unwrap();
    for evidence in [
        "schema: 2",
        "acme/api#1",
        "abcdef123456",
        "age ",
        "deploy/old",
        "step:0",
        "discarded",
        "created_at",
        "updated_at",
    ] {
        assert!(text.contains(evidence), "{evidence}: {text}");
    }
}

#[test]
fn state_discard_without_terminal_or_yes_refuses_without_changing_file() {
    let fixture = Fixture::new();
    let bytes = fixture.seed();
    let out = fixture.run(&["discard", "acme/api#1", "--reason", "incident closed"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("refusing to discard without a terminal: pass --yes")
    );
    assert_eq!(fs::read(&fixture.state).unwrap(), bytes);
    assert!(!fixture.state.with_file_name("state.json.lock").exists());
}

#[test]
fn state_discard_preserves_metadata_and_appends_audit_exactly_once() {
    let fixture = Fixture::new();
    fixture.seed();
    let before = fixture.read();
    let out = fixture.run(&[
        "discard",
        "acme/api#1",
        "--reason",
        "  incident closed  ",
        "--yes",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = fixture.read();
    assert_eq!(after["pending"], json!([]));
    for field in [
        "profile",
        "queue_id",
        "created_at",
        "deploy_prs",
        "active_jobs",
    ] {
        assert_eq!(after[field], before[field]);
    }
    assert_eq!(after["audit"].as_array().unwrap().len(), 2);
    assert_eq!(after["audit"][0], before["audit"][0]);
    let entry = &after["audit"][1];
    assert_eq!(entry["id"], "acme/api#1");
    assert_eq!(entry["repo"], "acme/api");
    assert_eq!(entry["base"], "main");
    assert_eq!(entry["sha"], "abcdef123456");
    assert_eq!(entry["action"], "discarded");
    assert_eq!(entry["reason"], "incident closed");
    assert!(entry.get("resolution").is_none());
    chrono::DateTime::parse_from_rfc3339(entry["recorded_at"].as_str().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("no deploy confirmed"));
    let bytes = fs::read(&fixture.state).unwrap();
    let again = fixture.run(&["discard", "acme/api#1", "--reason", "duplicate", "--yes"]);
    assert!(!again.status.success());
    assert_eq!(fs::read(&fixture.state).unwrap(), bytes);
}

#[test]
fn state_discard_obeys_merge_lock_but_show_can_read_it() {
    let fixture = Fixture::new();
    let bytes = fixture.seed();
    let mut store = Store::open(Some(fixture.state.clone())).unwrap();
    store.lock().unwrap();
    let out = fixture.run(&["discard", "acme/api#1", "--reason", "closed", "--yes"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("state.json.lock"));
    assert_eq!(fs::read(&fixture.state).unwrap(), bytes);
    assert!(fixture.run(&["show", "--json"]).status.success());
}

#[test]
fn state_discard_rejects_empty_reason_and_unknown_id_without_writes() {
    let fixture = Fixture::new();
    let bytes = fixture.seed();
    for (id, reason) in [("acme/api#1", "   "), ("acme/api#999", "closed")] {
        let out = fixture.run(&["discard", id, "--reason", reason, "--yes"]);
        assert!(!out.status.success());
        assert_eq!(fs::read(&fixture.state).unwrap(), bytes);
    }
}

#[test]
fn state_show_explicit_path_overrides_profile_config_and_missing_state_stays_absent() {
    let fixture = Fixture::new();
    fixture.seed();
    let missing = fixture.directory.join("missing.json");
    let out = fixture.run(&["show", "--json", "--state", missing.to_str().unwrap()]);
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(PathBuf::from(value["path"].as_str().unwrap()), missing);
    assert_eq!(value["pending"], json!([]));
    assert_eq!(value["audit"], json!([]));
    assert_eq!(value["active_jobs"], json!([]));
    assert_eq!(value["created_at"], Value::Null);
    assert!(!missing.exists());
}
