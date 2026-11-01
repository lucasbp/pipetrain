use std::collections::BTreeSet;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Conflict {
    pub a: String,
    pub b: String,
    pub repo: String,
    pub files: Vec<String>,
    pub kind: String,
}

impl Conflict {
    pub fn file_count(&self) -> String {
        match self.files.len() {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: String,
    pub repo: String,
    pub paths: BTreeSet<String>,
}

const LOCKFILES: [&str; 4] = [
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "packages.lock.json",
];

pub fn kind_of(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    let segments: Vec<&str> = lower.split('/').collect();
    let (name, dirs) = match segments.split_last() {
        Some((name, dirs)) => (*name, dirs),
        None => return "other",
    };
    if LOCKFILES.contains(&name) {
        return "lockfile";
    }
    if name.ends_with(".sql") && dirs.iter().any(|d| matches!(*d, "db" | "migrations")) {
        return "sql";
    }
    if dirs.iter().any(|d| matches!(*d, "obj" | "bin")) {
        return "generated";
    }
    "other"
}

fn shared_kind(files: &[String]) -> &'static str {
    let mut kinds = files.iter().map(|f| kind_of(f));
    match kinds.next() {
        Some(first) if kinds.all(|k| k == first) => first,
        _ => "other",
    }
}

pub fn find(snapshots: &[Snapshot], stacked: &BTreeSet<(String, String)>) -> Vec<Conflict> {
    let mut ordered: Vec<&Snapshot> = snapshots.iter().collect();
    ordered.sort_by(|x, y| x.id.cmp(&y.id));
    let mut out = Vec::new();
    for (i, first) in ordered.iter().enumerate() {
        for second in &ordered[i + 1..] {
            if first.repo != second.repo {
                continue;
            }
            if stacked.contains(&(first.id.clone(), second.id.clone())) {
                continue;
            }
            let files: Vec<String> = first.paths.intersection(&second.paths).cloned().collect();
            if files.is_empty() {
                continue;
            }
            out.push(Conflict {
                a: first.id.clone(),
                b: second.id.clone(),
                repo: first.repo.clone(),
                kind: shared_kind(&files).to_string(),
                files,
            });
        }
    }
    out.sort_by(|x, y| (&x.a, &x.b).cmp(&(&y.a, &y.b)));
    out
}
