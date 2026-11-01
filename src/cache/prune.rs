use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

struct Found {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

fn is_json(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

fn is_temp(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(".tmp-"))
}

fn inspect(path: PathBuf, found: &mut Vec<Found>) {
    if is_temp(&path) {
        let _ = fs::remove_file(&path);
        return;
    }
    if !is_json(&path) {
        return;
    }
    if let Ok(meta) = fs::metadata(&path)
        && let Ok(modified) = meta.modified()
    {
        found.push(Found {
            path,
            modified,
            len: meta.len(),
        });
    }
}

fn collect(dir: &Path, found: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(&entry.path(), found);
        } else if kind.is_file() {
            inspect(entry.path(), found);
        }
    }
}

fn is_expired(found: &Found, now: SystemTime, max_age: Duration) -> bool {
    now.duration_since(found.modified)
        .is_ok_and(|age| age > max_age)
}

fn remove_empty_dirs(dir: &Path, is_root: bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|k| k.is_dir()) {
            remove_empty_dirs(&entry.path(), false);
        }
    }
    let empty = fs::read_dir(dir).is_ok_and(|mut e| e.next().is_none());
    if empty && !is_root {
        let _ = fs::remove_dir(dir);
    }
}

fn trim_to_cap(mut files: Vec<Found>, max_total: u64) {
    files.sort_by_key(|f| f.modified);
    let mut total: u64 = files.iter().map(|f| f.len).sum();
    for file in files {
        if total <= max_total {
            break;
        }
        if fs::remove_file(&file.path).is_ok() {
            total = total.saturating_sub(file.len);
        }
    }
}

pub fn prune(dir: &Path, max_age: Duration, max_total: u64) {
    let mut found = Vec::new();
    collect(dir, &mut found);
    let now = SystemTime::now();
    let (old, fresh): (Vec<Found>, Vec<Found>) =
        found.into_iter().partition(|f| is_expired(f, now, max_age));
    for file in &old {
        let _ = fs::remove_file(&file.path);
    }
    trim_to_cap(fresh, max_total);
    remove_empty_dirs(dir, true);
}
