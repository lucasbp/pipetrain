mod key;
mod prune;
mod store;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

pub use prune::prune;

const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Use,
    Refresh,
    Off,
}

impl CacheMode {
    pub fn from_flags(no_cache: bool, refresh: bool) -> Self {
        if no_cache {
            Self::Off
        } else if refresh {
            Self::Refresh
        } else {
            Self::Use
        }
    }
}

pub struct Cache {
    dir: Option<PathBuf>,
    mode: CacheMode,
    hits: u32,
    misses: u32,
}

impl Cache {
    pub fn disabled() -> Self {
        Self {
            dir: None,
            mode: CacheMode::Off,
            hits: 0,
            misses: 0,
        }
    }

    pub fn open(mode: CacheMode, dir: Option<PathBuf>) -> Self {
        let Some(dir) = dir.filter(|_| mode != CacheMode::Off) else {
            return Self::disabled();
        };
        prune(&dir, MAX_AGE, MAX_TOTAL_BYTES);
        Self {
            dir: Some(dir),
            mode,
            hits: 0,
            misses: 0,
        }
    }

    pub fn at_default(mode: CacheMode) -> Self {
        Self::open(mode, default_dir())
    }

    fn entry_path(&self, slug: &str, endpoint: &str, parts: &[&str]) -> Option<PathBuf> {
        if self.mode == CacheMode::Off {
            return None;
        }
        key::entry_path(self.dir.as_deref()?, slug, endpoint, parts)
    }

    pub fn get(&mut self, slug: &str, endpoint: &str, parts: &[&str]) -> Option<Value> {
        let path = self.entry_path(slug, endpoint, parts)?;
        let found = match self.mode {
            CacheMode::Use => store::read(&path),
            _ => None,
        };
        match &found {
            Some(_) => self.hits = self.hits.saturating_add(1),
            None => self.misses = self.misses.saturating_add(1),
        }
        found
    }

    pub fn put(&mut self, slug: &str, endpoint: &str, parts: &[&str], data: &Value) {
        if let Some(path) = self.entry_path(slug, endpoint, parts) {
            store::write(&path, data);
        }
    }

    pub fn hits(&self) -> u32 {
        self.hits
    }

    pub fn misses(&self) -> u32 {
        self.misses
    }
}

pub fn resolve_dir(env: Option<String>, cache_dir: Option<PathBuf>) -> Option<PathBuf> {
    match env {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => cache_dir.map(|dir| dir.join("pipetrain")),
    }
}

pub fn default_dir() -> Option<PathBuf> {
    resolve_dir(std::env::var("PIPETRAIN_CACHE_DIR").ok(), dirs::cache_dir())
}
