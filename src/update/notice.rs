use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::release::{REPO, Release};
use super::version::Version;
use crate::cache;
use crate::config::ApiConfig;
use crate::github::Client;

pub const WINDOW_SECS: i64 = 24 * 60 * 60;
pub const TIMEOUT: Duration = Duration::from_secs(3);
const API: &str = "https://api.github.com";
const FILE: &str = "update-check.json";
const MAX_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub version: String,
    pub checked: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Suppress {
    pub flag: bool,
    pub json: bool,
    pub emit_queue: bool,
    pub is_update: bool,
}

fn truthy(value: Option<&str>) -> bool {
    value.is_some_and(|text| {
        let text = text.trim();
        !text.is_empty() && text != "0" && !text.eq_ignore_ascii_case("false")
    })
}

pub fn enabled(
    suppress: Suppress,
    env_off: Option<&str>,
    ci: Option<&str>,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    let in_ci = ci.is_some_and(|value| {
        let value = value.trim();
        value.eq_ignore_ascii_case("true") || value == "1"
    });
    !(suppress.flag
        || suppress.json
        || suppress.emit_queue
        || suppress.is_update
        || truthy(env_off)
        || in_ci
        || !stdout_is_terminal
        || !stderr_is_terminal)
}

pub fn due(stamp: Option<&Stamp>, now: i64) -> bool {
    match stamp {
        None => true,
        Some(stamp) => stamp.checked > now || now - stamp.checked >= WINDOW_SECS,
    }
}

pub fn message(current: &str, latest: &str) -> Option<String> {
    let newer = Version::parse(latest)? > Version::parse(current)?;
    newer.then(|| format!("pipetrain {latest} available, run pipetrain update --check"))
}

pub fn read(path: &Path) -> Option<Stamp> {
    if fs::metadata(path).ok()?.len() > MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub fn write(path: &Path, stamp: &Stamp) {
    let Ok(bytes) = serde_json::to_vec(stamp) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, bytes);
}

pub fn fetch_latest(base: &str, token: String, timeout: Duration) -> Option<String> {
    let mut client = Client::with_timeout(token, &ApiConfig::default(), false, base, timeout);
    let path = format!("/repos/{REPO}/releases/latest");
    let body = client
        .get(&path)
        .and_then(|reply| reply.into_body(&path))
        .ok()?;
    Some(Release::parse(&body).ok()?.version)
}

pub fn check(
    path: &Path,
    now: i64,
    current: &str,
    fetch: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let mut stamp = read(path);
    if due(stamp.as_ref(), now) {
        let version = fetch()
            .or_else(|| stamp.as_ref().map(|old| old.version.clone()))
            .unwrap_or_default();
        let fresh = Stamp {
            version,
            checked: now,
        };
        write(path, &fresh);
        stamp = Some(fresh);
    }
    message(current, &stamp?.version)
}

fn stamp_path() -> Option<PathBuf> {
    Some(cache::default_dir()?.join(FILE))
}

pub fn announce(suppress: Suppress) {
    let off = std::env::var("PIPETRAIN_NO_UPDATE_CHECK").ok();
    let ci = std::env::var("CI").ok();
    let stdout = std::io::stdout().is_terminal();
    let stderr = std::io::stderr().is_terminal();
    if !enabled(suppress, off.as_deref(), ci.as_deref(), stdout, stderr) {
        return;
    }
    let Some(path) = stamp_path() else {
        return;
    };
    let base = std::env::var("PIPETRAIN_UPDATE_API_URL").unwrap_or_else(|_| API.to_string());
    let token = std::env::var("GH_TOKEN").unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let current = env!("CARGO_PKG_VERSION");
    if let Some(line) = check(&path, now, current, || {
        fetch_latest(base.trim(), token.trim().to_string(), TIMEOUT)
    }) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pipetrain-notice-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    fn stamp(version: &str, checked: i64) -> Stamp {
        Stamp {
            version: version.to_string(),
            checked,
        }
    }

    fn on(suppress: Suppress) -> bool {
        enabled(suppress, None, None, true, true)
    }

    #[test]
    fn shown_only_on_an_interactive_terminal() {
        assert!(on(Suppress::default()));
        assert!(!enabled(Suppress::default(), None, None, false, true));
        assert!(!enabled(Suppress::default(), None, None, true, false));
    }

    #[test]
    fn every_suppression_turns_it_off() {
        for suppress in [
            Suppress {
                flag: true,
                ..Suppress::default()
            },
            Suppress {
                json: true,
                ..Suppress::default()
            },
            Suppress {
                emit_queue: true,
                ..Suppress::default()
            },
            Suppress {
                is_update: true,
                ..Suppress::default()
            },
        ] {
            assert!(!on(suppress));
        }
        let plain = Suppress::default();
        assert!(!enabled(plain, Some("1"), None, true, true));
        assert!(!enabled(plain, Some("true"), None, true, true));
        assert!(enabled(plain, Some("0"), None, true, true));
        assert!(enabled(plain, Some(""), None, true, true));
        assert!(!enabled(plain, None, Some("true"), true, true));
        assert!(!enabled(plain, None, Some(" 1 "), true, true));
        assert!(enabled(plain, None, Some("false"), true, true));
    }

    #[test]
    fn due_after_a_day_or_without_a_stamp() {
        assert!(due(None, 1000));
        assert!(!due(Some(&stamp("1.0.0", 1000)), 1000 + WINDOW_SECS - 1));
        assert!(due(Some(&stamp("1.0.0", 1000)), 1000 + WINDOW_SECS));
        assert!(due(Some(&stamp("1.0.0", 5000)), 1000));
    }

    #[test]
    fn the_message_needs_a_newer_version() {
        assert_eq!(
            message("0.1.0", "0.2.0").as_deref(),
            Some("pipetrain 0.2.0 available, run pipetrain update --check")
        );
        assert_eq!(message("0.2.0", "0.2.0"), None);
        assert_eq!(message("0.3.0", "0.2.0"), None);
        assert_eq!(message("0.1.0", ""), None);
    }

    #[test]
    fn a_fresh_stamp_skips_the_network() {
        let path = scratch();
        write(&path, &stamp("0.2.0", 1000));
        let line = check(&path, 2000, "0.1.0", || panic!("must not fetch"));
        assert!(line.unwrap().contains("0.2.0"));
    }

    #[test]
    fn a_stale_stamp_fetches_once_and_is_rewritten() {
        let path = scratch();
        write(&path, &stamp("0.1.0", 0));
        let now = WINDOW_SECS + 5;
        let line = check(&path, now, "0.1.0", || Some("0.3.0".to_string()));
        assert!(line.unwrap().contains("0.3.0"));
        assert_eq!(read(&path), Some(stamp("0.3.0", now)));
        assert!(check(&path, now + 1, "0.1.0", || panic!("again")).is_some());
    }

    #[test]
    fn a_failed_fetch_is_recorded_and_stays_silent() {
        let path = scratch();
        assert_eq!(check(&path, 100, "0.1.0", || None), None);
        assert_eq!(read(&path), Some(stamp("", 100)));
        assert_eq!(check(&path, 101, "0.1.0", || panic!("retry")), None);
    }

    #[test]
    fn a_failed_fetch_keeps_the_known_version() {
        let path = scratch();
        write(&path, &stamp("0.2.0", 0));
        let line = check(&path, WINDOW_SECS, "0.1.0", || None);
        assert!(line.unwrap().contains("0.2.0"));
    }

    #[test]
    fn a_corrupt_stamp_is_ignored() {
        let path = scratch();
        fs::write(&path, b"{not json").unwrap();
        assert_eq!(read(&path), None);
        let line = check(&path, 10, "0.1.0", || Some("0.2.0".to_string()));
        assert!(line.is_some());
    }
}
