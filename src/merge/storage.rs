use std::fmt::Write;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use super::{SCHEMA, State, Store};

static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    Ok(())
}

fn age_seconds(timestamp: Option<&str>, now: DateTime<Utc>) -> Option<u64> {
    let timestamp = DateTime::parse_from_rfc3339(timestamp?).ok()?;
    Some(now.signed_duration_since(timestamp).num_seconds().max(0) as u64)
}

fn age_text(seconds: Option<u64>) -> String {
    match seconds {
        Some(seconds) => format!(
            "age {}h {}m {}s",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        ),
        None => "age unknown".to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingPreview {
    pub id: String,
    pub base: String,
    pub sha: Option<String>,
    pub recorded_at: Option<String>,
    pub age_seconds: Option<u64>,
    pub age: String,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatePreview {
    pub path: Option<String>,
    pub file_age_seconds: Option<u64>,
    pub file_age: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub profile: Option<String>,
    pub selected_profile: Option<String>,
    pub queue_id: Option<String>,
    pub stale_pending_hours: u64,
    pub pending: Vec<PendingPreview>,
}

impl StatePreview {
    pub fn render(&self) -> String {
        let mut out = format!(
            "State\n  path: {}\n  file {}\n  profile: {} (selected: {})\n  queue: {}\n",
            self.path.as_deref().unwrap_or("memory only"),
            self.file_age,
            self.profile.as_deref().unwrap_or("unknown"),
            self.selected_profile.as_deref().unwrap_or("unknown"),
            self.queue_id.as_deref().unwrap_or("unknown"),
        );
        for pending in &self.pending {
            writeln!(
                out,
                "  inherited pending: {} into {} at {} ({}){}",
                pending.id,
                pending.base,
                pending.sha.as_deref().unwrap_or("SHA unknown"),
                pending.age,
                if pending.stale {
                    "; stale (warning only)"
                } else {
                    ""
                },
            )
            .expect("writing to a String cannot fail");
        }
        out.push('\n');
        out
    }
}

impl Store {
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::in_memory());
        };
        let mut state: State = match fs::read_to_string(&path) {
            Ok(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).with_context(|| {
                    format!("the merge state file {} is not valid", path.display())
                })?;
                if value
                    .get("schema")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|schema| schema > u64::from(SCHEMA))
                {
                    bail!(
                        "the merge state file {} was written by a newer pipetrain (schema {}, supported up to {SCHEMA}); upgrade pipetrain before using it",
                        path.display(),
                        value["schema"]
                    );
                }
                serde_json::from_value(value).with_context(|| {
                    format!("the merge state file {} is not valid", path.display())
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => bail!(
                "cannot read the merge state file {}: {error}",
                path.display()
            ),
        };
        state.schema = SCHEMA;
        Ok(Self {
            path: Some(path),
            state,
            remote: None,
            lock: None,
        })
    }

    pub fn lock(&mut self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if self.lock.is_some() {
            return Ok(());
        }
        let lock_path = sibling(path, ".lock");
        create_parent(&lock_path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("cannot open the merge state lock {}", lock_path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => bail!(
                "another merge execution is active; cannot acquire the merge state lock {}",
                lock_path.display()
            ),
            Err(error) => bail!(
                "cannot acquire the merge state lock {}: {error}",
                lock_path.display()
            ),
        }
        self.state = Self::open(Some(path.clone()))?.state;
        self.lock = Some(file);
        Ok(())
    }

    pub fn preview(&self, profile: Option<&str>, stale_pending_hours: u64) -> StatePreview {
        let now = Utc::now();
        let file_age_seconds = self
            .path
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
            .map(|modified| {
                now.signed_duration_since(DateTime::<Utc>::from(modified))
                    .num_seconds()
                    .max(0) as u64
            });
        StatePreview {
            path: self.path.as_ref().map(|path| path.display().to_string()),
            file_age_seconds,
            file_age: age_text(file_age_seconds),
            created_at: self.state.created_at.clone(),
            updated_at: self.state.updated_at.clone(),
            profile: self.state.profile.clone(),
            selected_profile: profile.map(str::to_string),
            queue_id: self.state.queue_id.clone(),
            stale_pending_hours,
            pending: self
                .state
                .pending
                .iter()
                .map(|pending| {
                    let age_seconds = age_seconds(pending.recorded_at.as_deref(), now);
                    PendingPreview {
                        id: pending.id.clone(),
                        base: pending.base.clone(),
                        sha: pending
                            .sha
                            .as_ref()
                            .map(|sha| sha.chars().take(7).collect()),
                        recorded_at: pending.recorded_at.clone(),
                        age_seconds,
                        age: age_text(age_seconds),
                        stale: age_seconds
                            .is_some_and(|age| age > stale_pending_hours.saturating_mul(3600)),
                    }
                })
                .collect(),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let now = Utc::now();
        let timestamp = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
        self.state.schema = SCHEMA;
        self.state
            .created_at
            .get_or_insert_with(|| timestamp.clone());
        self.state.updated_at = Some(timestamp);
        self.state.queue_id.get_or_insert_with(|| {
            format!(
                "{}-{}-{}",
                now.format("%Y%m%dT%H%M%S%.9fZ"),
                std::process::id(),
                NEXT_WRITE.fetch_add(1, Ordering::Relaxed)
            )
        });
        let Some(path) = &self.path else {
            return Ok(());
        };
        create_parent(path)?;
        let data = serde_json::to_vec_pretty(&self.state)?;
        let (temp, mut file) = loop {
            let temp = sibling(
                path,
                &format!(
                    ".{}.{}.tmp",
                    std::process::id(),
                    NEXT_WRITE.fetch_add(1, Ordering::Relaxed)
                ),
            );
            match File::create_new(&temp) {
                Ok(file) => break (temp, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot write {}", temp.display()));
                }
            }
        };
        let result = file.write_all(&data).and_then(|()| file.sync_all());
        drop(file);
        let result = result
            .with_context(|| format!("cannot write {}", temp.display()))
            .and_then(|()| {
                fs::rename(&temp, path).with_context(|| format!("cannot write {}", path.display()))
            });
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

pub fn resolve_state_path(
    explicit: Option<&Path>,
    configured: Option<&Path>,
    profile: Option<&str>,
) -> Result<PathBuf> {
    let directory = dirs::state_dir().or_else(dirs::data_local_dir);
    resolve_in(explicit, configured, profile, directory.as_deref())
}

fn profile_key(profile: &str) -> String {
    let normalized = profile.to_ascii_lowercase();
    let reserved = matches!(normalized.as_str(), "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            normalized.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
            })
        });
    let mut key = String::new();
    for (index, byte) in normalized.bytes().enumerate() {
        if (byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
            && !(reserved && index == 0)
        {
            key.push(char::from(byte));
        } else {
            write!(key, "%{byte:02x}").expect("writing to a String cannot fail");
        }
    }
    key
}

fn has_work(state: &State) -> bool {
    !state.pending.is_empty()
        || !state.deploy_prs.is_empty()
        || !state.active_jobs.is_empty()
        || state.bump.as_ref().is_some_and(|journal| {
            !journal.publishers.is_empty()
                || !journal.contracts.is_empty()
                || !journal.consumers.is_empty()
        })
}

fn resolve_in(
    explicit: Option<&Path>,
    configured: Option<&Path>,
    profile: Option<&str>,
    directory: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(path) = explicit.or(configured) {
        if path.as_os_str().is_empty() {
            bail!("the merge state path is empty: choose --state FILE or set merge.state_file");
        }
        return Ok(path.to_path_buf());
    }
    let root = directory
        .ok_or_else(|| {
            anyhow!("cannot locate the user state directory: choose --state FILE or set merge.state_file; merging without durable state is not allowed")
        })?
        .join("pipetrain");
    let legacy = root.join("merge-state.json");
    let Some(profile) = profile.filter(|name| !name.eq_ignore_ascii_case(crate::profile::IMPLICIT))
    else {
        return Ok(legacy);
    };
    if profile.trim().is_empty() {
        bail!("cannot select a merge state file for an empty profile name");
    }
    if has_work(&Store::open(Some(legacy.clone()))?.state) {
        bail!(
            "the legacy merge state {} contains unfinished work; choose that file with --state or set merge.state_file explicitly before using automatic state for profile `{profile}`; no state was moved or discarded",
            legacy.display()
        );
    }
    Ok(root
        .join("queues")
        .join(profile_key(profile))
        .join("merge-state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "pipetrain-profile-state-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("pipetrain")).unwrap();
            Self(path)
        }

        fn resolve(&self, profile: Option<&str>) -> Result<PathBuf> {
            resolve_in(None, None, profile, Some(&self.0))
        }

        fn legacy(&self) -> PathBuf {
            self.0.join("pipetrain/merge-state.json")
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn explicit_then_configured_paths_override_automatic_storage_without_a_user_directory() {
        let explicit = Path::new("explicit.json");
        let configured = Path::new("configured.json");
        assert_eq!(
            resolve_in(Some(explicit), Some(configured), Some("acme"), None).unwrap(),
            explicit
        );
        assert_eq!(
            resolve_in(None, Some(configured), Some("acme"), None).unwrap(),
            configured
        );
        assert!(resolve_in(None, None, Some("acme"), None).is_err());
        assert!(resolve_in(None, None, None, None).is_err());
        assert!(resolve_in(None, Some(Path::new("")), Some("acme"), None).is_err());
    }

    #[test]
    fn automatic_paths_are_stable_per_profile_and_keep_the_implicit_legacy_path() {
        let directory = Directory::new();
        assert_eq!(directory.resolve(None).unwrap(), directory.legacy());
        assert_eq!(
            directory.resolve(Some("default")).unwrap(),
            directory.legacy()
        );
        let first = directory.resolve(Some("Acme")).unwrap();
        assert_eq!(first, directory.resolve(Some("acme")).unwrap());
        assert_eq!(
            first,
            directory.0.join("pipetrain/queues/acme/merge-state.json")
        );
        assert_ne!(first, directory.resolve(Some("other")).unwrap());
        assert!(!first.exists());
    }

    #[test]
    fn special_names_cannot_escape_the_queue_directory_or_collide_on_windows() {
        let names = [
            "a/b", "a\\b", "a.b", "a-b", "a_b", "a%2fb", "..", "con", "nul", "com1", "lpt9",
            "équipe", "Équipe", "a ", "a:",
        ];
        let keys: BTreeSet<String> = names.iter().map(|name| profile_key(name)).collect();
        assert_eq!(keys.len(), names.len());
        for name in names {
            let key = profile_key(name);
            assert_eq!(key, profile_key(&name.to_ascii_uppercase()));
            assert_eq!(Path::new(&key).components().count(), 1);
            assert!(!key.contains(['/', '\\', ':', '.']));
        }
        assert_eq!(profile_key("con"), "%63on");
        assert_eq!(profile_key("a/b"), "a%2fb");
        assert_eq!(profile_key("a%2fb"), "a%252fb");
        assert_ne!(profile_key("Équipe"), profile_key("équipe"));
    }

    #[test]
    fn profile_storage_creates_directories_and_resumes_only_its_own_work() {
        let directory = Directory::new();
        let first = directory.resolve(Some("acme")).unwrap();
        let second = directory.resolve(Some("globex")).unwrap();
        let pending = super::super::Pending {
            recorded_at: None,
            id: "acme/api#1".to_string(),
            repo: "acme/api".to_string(),
            base: "main".to_string(),
            sha: Some("abc123".to_string()),
            continuation: None,
            deploy_prediction: None,
        };
        Store::open(Some(first.clone()))
            .unwrap()
            .add(pending.clone())
            .unwrap();
        assert_eq!(
            Store::open(Some(first)).unwrap().state.pending,
            vec![pending]
        );
        assert!(
            Store::open(Some(second.clone()))
                .unwrap()
                .state
                .pending
                .is_empty()
        );
        assert!(!second.exists());
        assert!(!directory.legacy().exists());
    }

    #[test]
    fn legacy_work_blocks_automatic_profiles_without_moving_or_ignoring_any_state() {
        let directory = Directory::new();
        let fresh = directory.resolve(Some("acme")).unwrap();
        fs::create_dir_all(fresh.parent().unwrap()).unwrap();
        fs::write(&fresh, r#"{"schema":1,"pending":[],"deploy_prs":[]}"#).unwrap();
        let cases = [
            r#"{"pending":[{"id":"acme/api#1","repo":"acme/api","base":"main","sha":null}],"deploy_prs":[]}"#,
            r#"{"pending":[],"deploy_prs":[{"slug":"acme/api","branch":"deploy/old","ids":[] }]}"#,
            r#"{"active_jobs":["step:1"]}"#,
            r#"{"bump":{"publishers":{},"contracts":{"acme/api#1":[]},"consumers":{}}}"#,
        ];
        for text in cases {
            fs::write(directory.legacy(), text).unwrap();
            let error = directory.resolve(Some("acme")).unwrap_err().to_string();
            assert!(error.contains("unfinished work"));
            assert!(error.contains("merge.state_file"));
            assert_eq!(fs::read_to_string(directory.legacy()).unwrap(), text);
            assert!(fresh.exists());
            assert_eq!(
                resolve_in(
                    None,
                    Some(&directory.legacy()),
                    Some("acme"),
                    Some(&directory.0)
                )
                .unwrap(),
                directory.legacy()
            );
        }
    }

    #[test]
    fn empty_legacy_state_is_left_in_place_and_corrupt_legacy_state_is_an_error() {
        let directory = Directory::new();
        fs::write(
            directory.legacy(),
            r#"{"schema":1,"pending":[],"deploy_prs":[]}"#,
        )
        .unwrap();
        assert_ne!(directory.resolve(Some("acme")).unwrap(), directory.legacy());
        assert!(directory.legacy().exists());
        fs::write(directory.legacy(), "broken-json").unwrap();
        assert!(
            directory
                .resolve(Some("acme"))
                .unwrap_err()
                .to_string()
                .contains("not valid")
        );
    }
}
