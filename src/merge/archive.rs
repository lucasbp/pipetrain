use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{State, Store};

const STAMP: &str = "%Y-%m-%dT%H%M%SZ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Archival {
    Kept,
    WouldArchive { pending: Vec<String>, to: PathBuf },
    Archived { pending: Vec<String>, to: PathBuf },
}

impl Archival {
    pub fn notice(&self, from: &Path) -> Option<String> {
        let (verb, pending, to) = match self {
            Archival::Kept => return None,
            Archival::WouldArchive { pending, to } => ("--apply would archive", pending, to),
            Archival::Archived { pending, to } => ("archived", pending, to),
        };
        Some(format!(
            "state {} belongs to another queue and every pending record in it is stale; {verb} it intact as {} and start a new state. Pending records: {}. Nothing was discarded or marked as deployed.",
            from.display(),
            to.display(),
            pending.join(", ")
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchivedState {
    pub path: String,
    pub archived_at: Option<String>,
    pub pending: usize,
}

fn stem_and_extension(path: &Path) -> (String, String) {
    let stem = path.file_stem().map_or_else(
        || "merge-state".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let extension = path
        .extension()
        .map_or_else(|| "json".to_string(), |s| s.to_string_lossy().into_owned());
    (stem, extension)
}

fn archive_name(path: &Path, now: DateTime<Utc>, attempt: u32) -> PathBuf {
    let (stem, extension) = stem_and_extension(path);
    let suffix = if attempt == 0 {
        String::new()
    } else {
        format!("-{attempt}")
    };
    path.with_file_name(format!("{stem}.{}{suffix}.{extension}", now.format(STAMP)))
}

fn free_archive_name(path: &Path, now: DateTime<Utc>) -> PathBuf {
    (0u32..)
        .map(|attempt| archive_name(path, now, attempt))
        .find(|candidate| !candidate.exists())
        .expect("an unused archive name exists")
}

fn parse_stamp(name: &str, stem: &str, extension: &str) -> Option<String> {
    let middle = name
        .strip_prefix(stem)?
        .strip_prefix('.')?
        .strip_suffix(extension)?
        .strip_suffix('.')?;
    let (stamp, rest) = middle.split_at_checked(18)?;
    let valid_rest = rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if !valid_rest {
        return None;
    }
    let parsed = chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%dT%H%M%SZ").ok()?;
    Some(
        parsed
            .and_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

pub fn archived_beside(path: &Path) -> Vec<ArchivedState> {
    let (stem, extension) = stem_and_extension(path);
    let directory = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<ArchivedState> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let archived_at = parse_stamp(&name, &stem, &extension)?;
            let pending = fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("pending")
                        .and_then(|p| p.as_array().map(Vec::len))
                })
                .unwrap_or(0);
            Some(ArchivedState {
                path: entry.path().display().to_string(),
                archived_at: Some(archived_at),
                pending,
            })
        })
        .collect();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn carries_only_stale_pending(state: &State, stale: &[bool]) -> bool {
    !state.pending.is_empty()
        && stale.iter().all(|s| *s)
        && state.deploy_prs.is_empty()
        && state.active_jobs.is_empty()
        && state.bump_behind.is_empty()
        && state.bump.as_ref().is_none_or(|journal| {
            journal.publishers.is_empty()
                && journal.contracts.is_empty()
                && journal.consumers.is_empty()
        })
}

fn shares_a_pull_request(state: &State, selection: &BTreeSet<String>) -> bool {
    state
        .pending
        .iter()
        .any(|pending| selection.contains(&pending.id.to_lowercase()))
}

impl Store {
    fn stale_queue_of_another_selection(
        &self,
        selection: &BTreeSet<String>,
        stale_pending_hours: u64,
    ) -> bool {
        let stale: Vec<bool> = self
            .preview(None, stale_pending_hours)
            .pending
            .iter()
            .map(|pending| pending.stale)
            .collect();
        carries_only_stale_pending(&self.state, &stale)
            && !shares_a_pull_request(&self.state, selection)
    }

    pub fn archive_stale_queue(
        &mut self,
        selection: &BTreeSet<String>,
        stale_pending_hours: u64,
        apply: bool,
        now: DateTime<Utc>,
    ) -> Result<Archival> {
        let Some(path) = self.path.clone() else {
            return Ok(Archival::Kept);
        };
        if !self.stale_queue_of_another_selection(selection, stale_pending_hours) {
            return Ok(Archival::Kept);
        }
        if apply {
            if self.lock().is_err() {
                return Ok(Archival::Kept);
            }
            if !self.stale_queue_of_another_selection(selection, stale_pending_hours) {
                return Ok(Archival::Kept);
            }
        }
        let pending: Vec<String> = self.state.pending.iter().map(|p| p.id.clone()).collect();
        let to = free_archive_name(&path, now);
        if !apply {
            return Ok(Archival::WouldArchive { pending, to });
        }
        fs::rename(&path, &to)
            .with_context(|| format!("cannot archive {} as {}", path.display(), to.display()))?;
        self.state = State::default();
        Ok(Archival::Archived { pending, to })
    }
}
