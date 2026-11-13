use std::fmt::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use super::{State, Store};

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
