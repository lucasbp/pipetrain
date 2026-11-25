use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::discovery::{Scope, Skipped};
use crate::model::Pr;

pub const GIT_BASE_ENV: &str = "PIPETRAIL_SEQUENCE_GIT_BASE";
pub const TOKEN_ENV: &str = "PIPETRAIL_SEQUENCE_GIT_TOKEN";

const DEFAULT_GIT_BASE: &str = "https://github.com";
const REMOTE: &str = "origin";
const BASE_REF: &str = "refs/pipetrain/base";

#[derive(Debug, Clone)]
pub struct Remote {
    pub base: String,
    pub token: String,
}

impl Remote {
    pub fn resolve(token: &str, api_is_loopback: bool) -> Option<Remote> {
        let from_env = std::env::var(GIT_BASE_ENV).ok().filter(|v| !v.is_empty());
        if from_env.is_none() && api_is_loopback {
            return None;
        }
        Some(Remote {
            base: from_env.unwrap_or_else(|| DEFAULT_GIT_BASE.to_string()),
            token: token.to_string(),
        })
    }

    fn url(&self, slug: &str) -> String {
        format!("{}/{slug}.git", self.base.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub number: u64,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub with: String,
    pub files: Vec<String>,
}

pub fn reason(found: &Found) -> String {
    format!(
        "conflicts with {} after it merges: {}",
        found.with,
        found.files.join(", ")
    )
}

fn git(dir: &Path, args: &[&str], token: &str) -> Option<Output> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "pipetrain")
        .env("GIT_AUTHOR_EMAIL", "pipetrain@localhost")
        .env("GIT_COMMITTER_NAME", "pipetrain")
        .env("GIT_COMMITTER_EMAIL", "pipetrain@localhost")
        .stdin(Stdio::null());
    if !token.is_empty() {
        command
            .env(TOKEN_ENV, token)
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "credential.helper")
            .env("GIT_CONFIG_VALUE_0", "")
            .env("GIT_CONFIG_KEY_1", "credential.helper")
            .env(
                "GIT_CONFIG_VALUE_1",
                format!(
                    "!f() {{ echo username=x-access-token; echo \"password=${TOKEN_ENV}\"; }}; f"
                ),
            );
    }
    command.output().ok()
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn succeeded(output: Option<Output>) -> Option<String> {
    output
        .filter(|out| out.status.success())
        .map(|out| text(&out))
}

enum Merge {
    Clean(String),
    Conflict(Vec<String>),
}

fn merge_tree(dir: &Path, left: &str, right: &str) -> Option<Merge> {
    let out = git(
        dir,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            left,
            right,
        ],
        "",
    )?;
    let body = text(&out);
    match out.status.code()? {
        0 => Some(Merge::Clean(body.lines().next()?.to_string())),
        1 => Some(Merge::Conflict(
            body.lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        )),
        _ => None,
    }
}

pub fn conflicts_in_order(dir: &Path, base: &str, members: &[Member]) -> Option<Vec<Found>> {
    let mut current = base.to_string();
    let mut merged: Vec<&Member> = Vec::new();
    let mut found = Vec::new();
    for member in members {
        match merge_tree(dir, &current, &member.head)? {
            Merge::Clean(tree) => {
                current = succeeded(git(
                    dir,
                    &[
                        "commit-tree",
                        &tree,
                        "-p",
                        &current,
                        "-p",
                        &member.head,
                        "-m",
                        "sequence",
                    ],
                    "",
                ))?;
                merged.push(member);
            }
            Merge::Conflict(files) => {
                let mut with = merged.last().map(|m| m.id.clone());
                for earlier in &merged {
                    if let Some(Merge::Conflict(_)) = merge_tree(dir, &earlier.head, &member.head) {
                        with = Some(earlier.id.clone());
                        break;
                    }
                }
                found.push(Found {
                    id: member.id.clone(),
                    with: with?,
                    files,
                });
            }
        }
    }
    Some(found)
}

fn repo_dir(cache: &Path, slug: &str) -> PathBuf {
    cache
        .join("sequence")
        .join(format!("{}.git", slug.to_lowercase().replace('/', "__")))
}

fn prepare(
    remote: &Remote,
    cache: &Path,
    slug: &str,
    base_ref: &str,
    members: &[Member],
) -> Option<(PathBuf, String)> {
    let dir = repo_dir(cache, slug);
    std::fs::create_dir_all(&dir).ok()?;
    if !dir.join("HEAD").exists() {
        succeeded(git(&dir, &["init", "--bare", "-q"], ""))?;
    }
    let url = remote.url(slug);
    git(&dir, &["remote", "remove", REMOTE], "");
    succeeded(git(&dir, &["remote", "add", REMOTE, &url], ""))?;
    let mut specs = vec![format!("+refs/heads/{base_ref}:{BASE_REF}")];
    for member in members {
        specs.push(format!(
            "+refs/pull/{n}/head:refs/pipetrain/pull/{n}",
            n = member.number
        ));
    }
    let mut filtered = vec!["fetch", "-q", "--no-tags", "--filter=blob:none", REMOTE];
    filtered.extend(specs.iter().map(String::as_str));
    let mut plain = vec!["fetch", "-q", "--no-tags", REMOTE];
    plain.extend(specs.iter().map(String::as_str));
    succeeded(git(&dir, &filtered, &remote.token))
        .or_else(|| succeeded(git(&dir, &plain, &remote.token)))?;
    for member in members {
        let reference = format!("refs/pipetrain/pull/{}", member.number);
        let have = succeeded(git(&dir, &["rev-parse", &reference], ""))?;
        if !have.eq_ignore_ascii_case(&member.head) {
            return None;
        }
    }
    let base = succeeded(git(&dir, &["rev-parse", BASE_REF], ""))?;
    Some((dir, base))
}

pub type Filtered = (Vec<(Pr, Scope)>, Vec<Skipped>);

pub fn filter(remote: &Remote, cache: &Path, ready: Vec<(Pr, Scope)>) -> Filtered {
    let mut groups: BTreeMap<(String, String), Vec<Member>> = BTreeMap::new();
    for (pr, _) in &ready {
        if pr.head_sha.is_empty() || pr.base_ref.is_empty() {
            continue;
        }
        groups
            .entry((pr.key.slug().to_lowercase(), pr.base_ref.clone()))
            .or_default()
            .push(Member {
                id: pr.key.to_string(),
                number: pr.key.number,
                head: pr.head_sha.clone(),
            });
    }
    let mut verdicts: BTreeMap<String, Found> = BTreeMap::new();
    for ((slug, base_ref), mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|m| m.number);
        let Some((dir, base)) = prepare(remote, cache, &slug, &base_ref, &members) else {
            continue;
        };
        for found in conflicts_in_order(&dir, &base, &members).unwrap_or_default() {
            verdicts.insert(found.id.clone(), found);
        }
    }
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for (pr, scope) in ready {
        match verdicts.get(&pr.key.to_string()) {
            Some(found) => skipped.push(Skipped {
                pr,
                scope,
                reasons: vec![reason(found)],
            }),
            None => kept.push((pr, scope)),
        }
    }
    (kept, skipped)
}
