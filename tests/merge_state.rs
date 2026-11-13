use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pipetrain::merge::{SCHEMA, Store};
use serde_json::{Value, json};

struct StateFile(PathBuf);

impl StateFile {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "pipetrain-state-safety-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        Self(directory.join("state.json"))
    }

    fn open(&self) -> Store {
        Store::open(Some(self.0.clone())).unwrap()
    }

    fn write(&self, value: &Value) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        fs::write(&self.0, &bytes).unwrap();
        bytes
    }

    fn read(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.0).unwrap()).unwrap()
    }

    fn lock_path(&self) -> PathBuf {
        self.0.with_file_name("state.json.lock")
    }
}

impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.0.parent().unwrap());
    }
}

#[test]
fn newer_state_schema_is_refused_without_touching_the_file() {
    let file = StateFile::new();
    for schema in [3, u64::MAX] {
        let bytes = file.write(&json!({"schema": schema, "pending": "future format"}));
        let modified = fs::metadata(&file.0).unwrap().modified().unwrap();
        let error = Store::open(Some(file.0.clone())).err().unwrap().to_string();
        assert!(error.contains("written by a newer pipetrain"), "{error}");
        assert!(error.contains(&file.0.display().to_string()));
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
        assert_eq!(fs::metadata(&file.0).unwrap().modified().unwrap(), modified);
        assert!(!file.lock_path().exists());
    }
}

#[test]
fn legacy_state_schemas_upgrade_only_on_save_and_preserve_pending() {
    for schema in [None, Some(0), Some(1), Some(2)] {
        let file = StateFile::new();
        let mut value = json!({
            "pending": [{"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": "abcdef123456"}],
            "deploy_prs": [{"slug": "acme/api", "branch": "deploy/old", "ids": ["acme/api#1"]}],
            "active_jobs": ["step:1"]
        });
        if let Some(schema) = schema {
            value["schema"] = json!(schema);
        }
        let bytes = file.write(&value);
        let mut store = file.open();
        assert_eq!(store.state.schema, SCHEMA);
        assert_eq!(fs::read(&file.0).unwrap(), bytes);
        assert!(store.state.pending[0].recorded_at.is_none());
        let pending = store.state.pending.clone();
        store.save().unwrap();
        assert_eq!(file.read()["schema"], 2);
        let reopened = file.open();
        assert_eq!(reopened.state.pending, pending);
        assert_eq!(reopened.state.deploy_prs, store.state.deploy_prs);
        assert_eq!(reopened.state.active_jobs, ["step:1"]);
        assert!(reopened.state.pending[0].recorded_at.is_none());
    }
}

#[test]
fn state_lock_excludes_another_store_and_releases_on_drop() {
    let file = StateFile::new();
    let mut first = file.open();
    let mut second = file.open();
    first.lock().unwrap();
    first.lock().unwrap();
    let error = second.lock().unwrap_err().to_string();
    assert!(
        error.contains("another merge execution is active"),
        "{error}"
    );
    assert!(
        error.contains(&file.lock_path().display().to_string()),
        "{error}"
    );
    assert!(!file.0.exists());
    drop(first);
    second.lock().unwrap();
    assert!(file.lock_path().exists());
}

#[test]
fn state_lock_is_exclusive_between_processes() {
    let file = StateFile::new();
    let mut store = file.open();
    store.lock().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "state_lock_child_probe", "--nocapture"])
        .env("PIPETRAIN_TEST_STATE_LOCK", &file.0)
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

#[test]
fn acquiring_state_lock_reloads_changes_since_open() {
    let file = StateFile::new();
    let mut waiting = file.open();
    {
        let mut first = file.open();
        first.lock().unwrap();
        first.state.queue_id = Some("original-queue".into());
        first.save().unwrap();
    }
    waiting.lock().unwrap();
    assert_eq!(waiting.state.queue_id.as_deref(), Some("original-queue"));
    drop(waiting);
    let mut stale = file.open();
    let bytes = file.write(&json!({"schema": 3}));
    assert!(
        stale
            .lock()
            .unwrap_err()
            .to_string()
            .contains("written by a newer pipetrain")
    );
    assert_eq!(fs::read(&file.0).unwrap(), bytes);
}

#[test]
fn saved_state_identity_and_creation_time_survive_later_writes() {
    let file = StateFile::new();
    let mut store = file.open();
    store.state.profile = Some("acme".into());
    store.save().unwrap();
    let first = file.read();
    for field in ["created_at", "updated_at"] {
        let text = first[field].as_str().unwrap();
        assert!(text.ends_with('Z'));
        chrono::DateTime::parse_from_rfc3339(text).unwrap();
    }
    assert!(!first["queue_id"].as_str().unwrap().is_empty());
    store.state.updated_at = Some("2000-01-01T00:00:00Z".into());
    store.save().unwrap();
    let second = file.read();
    assert_eq!(first["created_at"], second["created_at"]);
    assert_eq!(first["queue_id"], second["queue_id"]);
    assert_eq!(second["profile"], "acme");
    assert!(second["updated_at"].as_str().unwrap() >= first["updated_at"].as_str().unwrap());
    let other = StateFile::new();
    other.open().save().unwrap();
    assert_ne!(first["queue_id"], other.read()["queue_id"]);
}

#[test]
fn unique_state_temporary_files_preserve_existing_temporary_files() {
    let file = StateFile::new();
    let old_temp = file.0.with_extension("json.tmp");
    fs::write(&old_temp, "previous interruption").unwrap();
    for number in 0..64 {
        let path = file
            .0
            .with_file_name(format!("state.json.{}.{number}.tmp", std::process::id()));
        fs::write(path, "previous interruption").unwrap();
    }
    let mut store = file.open();
    store.save().unwrap();
    store.save().unwrap();
    assert_eq!(file.read()["schema"], 2);
    assert_eq!(
        fs::read_to_string(old_temp).unwrap(),
        "previous interruption"
    );
    for number in 0..64 {
        let path = file
            .0
            .with_file_name(format!("state.json.{}.{number}.tmp", std::process::id()));
        assert_eq!(fs::read_to_string(path).unwrap(), "previous interruption");
    }
    assert_eq!(fs::read_dir(file.0.parent().unwrap()).unwrap().count(), 66);
}

#[test]
fn pending_age_threshold_is_strict_and_legacy_or_invalid_dates_stay_unknown() {
    let file = StateFile::new();
    file.write(&json!({"schema": 2, "pending": [
        {"id": "acme/api#1", "repo": "acme/api", "base": "main", "sha": null, "recorded_at": "2000-01-01T00:00:00Z"},
        {"id": "acme/api#2", "repo": "acme/api", "base": "main", "sha": null},
        {"id": "acme/api#3", "repo": "acme/api", "base": "main", "sha": null, "recorded_at": "invalid"},
        {"id": "acme/api#4", "repo": "acme/api", "base": "main", "sha": null, "recorded_at": "2999-01-01T00:00:00Z"}
    ]}));
    let store = file.open();
    let preview = store.preview(Some("acme"), 24);
    assert!(preview.pending[0].stale);
    for pending in &preview.pending[1..3] {
        assert_eq!(pending.age, "age unknown");
        assert!(pending.age_seconds.is_none());
        assert!(!pending.stale);
    }
    assert_eq!(preview.pending[3].age_seconds, Some(0));
    assert!(!store.preview(None, 0).pending[3].stale);
    assert!(!store.preview(None, u64::MAX).pending[0].stale);
}
