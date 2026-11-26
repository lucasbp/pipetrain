use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::store;

const VERSION: u64 = 1;
const MAX_ENTRY_BYTES: u64 = 4 * 1024 * 1024;

pub const PR_TTL: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Ttl(u64),
    Immutable,
    Revalidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stored {
    pub v: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub stored_at: u64,
    pub body: Value,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl Stored {
    pub fn new(etag: Option<String>, last_modified: Option<String>, body: Value) -> Self {
        Self {
            v: VERSION,
            etag,
            last_modified,
            stored_at: now(),
            body,
        }
    }

    pub fn is_fresh(&self, freshness: Freshness, live: bool) -> bool {
        match freshness {
            Freshness::Immutable => true,
            Freshness::Revalidate => false,
            Freshness::Ttl(seconds) => !live && now().saturating_sub(self.stored_at) < seconds,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HttpCache {
    dir: Option<PathBuf>,
    refresh: bool,
}

pub fn key(method: &str, url: &str, body: &str, token: &str) -> String {
    let who = hex(&Sha256::digest(token.as_bytes()));
    let mut hasher = Sha256::new();
    for part in [method, url, body, &who] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl HttpCache {
    pub fn off() -> Self {
        Self::default()
    }

    pub(super) fn at(dir: Option<PathBuf>, refresh: bool) -> Self {
        Self {
            dir: dir.map(|dir| dir.join("http")),
            refresh,
        }
    }

    pub fn enabled(&self) -> bool {
        self.dir.is_some()
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        Some(
            self.dir
                .as_ref()?
                .join(&key[..2])
                .join(format!("{key}.json")),
        )
    }

    pub fn load(&self, key: &str) -> Option<Stored> {
        if self.refresh {
            return None;
        }
        let path = self.path(key)?;
        if fs::metadata(&path).ok()?.len() > MAX_ENTRY_BYTES {
            return None;
        }
        let stored: Stored = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        (stored.v == VERSION).then_some(stored)
    }

    pub fn save(&self, key: &str, stored: &Stored) {
        let Some(path) = self.path(key) else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(stored) else {
            return;
        };
        if bytes.len() as u64 <= MAX_ENTRY_BYTES {
            let _ = store::write_atomic(&path, &bytes);
        }
    }
}

pub fn is_sha(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn freshness_of(path: &str, live: bool) -> Freshness {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let mut segments = route.strip_prefix("/repos/").unwrap_or("").split('/');
    let resource = segments.nth(2);
    let by_sha = resource == Some("git")
        && matches!(segments.next(), Some("trees" | "blobs"))
        && segments.next().is_some_and(is_sha)
        && segments.next().is_none();
    let ref_is_sha = resource == Some("contents")
        && query
            .split('&')
            .any(|pair| pair.strip_prefix("ref=").is_some_and(is_sha));
    if by_sha || ref_is_sha {
        Freshness::Immutable
    } else if live {
        Freshness::Revalidate
    } else {
        Freshness::Ttl(PR_TTL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_keys_distinguish_variables_and_accounts() {
        let query = "query($n: Int!) { x }";
        let one = key(
            "POST",
            "u/graphql",
            &format!("{query}|{}", r#"{"n":1}"#),
            "t",
        );
        let two = key(
            "POST",
            "u/graphql",
            &format!("{query}|{}", r#"{"n":2}"#),
            "t",
        );
        let other = key(
            "POST",
            "u/graphql",
            &format!("{query}|{}", r#"{"n":1}"#),
            "u",
        );
        assert_ne!(one, two);
        assert_ne!(one, other);
        assert_eq!(one.len(), 64);
        assert!(one.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn ttl_expires_and_immutable_does_not() {
        let mut stored = Stored::new(None, None, Value::Null);
        stored.stored_at = now() - 120;
        assert!(!stored.is_fresh(Freshness::Ttl(60), false));
        assert!(stored.is_fresh(Freshness::Ttl(300), false));
        assert!(!stored.is_fresh(Freshness::Ttl(300), true));
        assert!(stored.is_fresh(Freshness::Immutable, true));
        assert!(!stored.is_fresh(Freshness::Revalidate, false));
    }

    #[test]
    fn paths_by_sha_are_immutable() {
        let sha = "a".repeat(40);
        assert_eq!(
            freshness_of(&format!("/repos/o/r/git/trees/{sha}?recursive=1"), false),
            Freshness::Immutable
        );
        assert_eq!(
            freshness_of(&format!("/repos/o/r/contents/a.yml?ref={sha}"), true),
            Freshness::Immutable
        );
        assert_eq!(
            freshness_of("/repos/o/r/pulls/7", false),
            Freshness::Ttl(PR_TTL)
        );
        assert_eq!(
            freshness_of("/repos/o/r/actions/runs?head_sha=x", true),
            Freshness::Revalidate
        );
        assert_eq!(
            freshness_of(&format!("/repos/o/{sha}/git/trees/main"), true),
            Freshness::Revalidate
        );
    }
}
