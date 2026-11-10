use anyhow::{Result, bail};
use regex::Regex;
use serde::Serialize;
pub mod backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherExecution {
    pub repository: String,
    pub merge_sha: String,
    pub run_id: u64,
    pub published_this_execution: bool,
}

#[derive(Debug, Clone)]
pub struct PublisherRun {
    pub repository: String,
    pub head_sha: String,
    pub id: u64,
    pub succeeded: bool,
    pub push_step_succeeded: bool,
    pub push_step_log: String,
}

pub trait ExactFeed {
    fn contains(&mut self, package: &str, version: &str) -> Result<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicationEvidence {
    repository: String,
    merge_sha: String,
    run_id: u64,
    package: String,
    version: String,
}

impl PublicationEvidence {
    pub fn package(&self) -> &str {
        &self.package
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn merge_sha(&self) -> &str {
        &self.merge_sha
    }
    pub fn run_id(&self) -> u64 {
        self.run_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PublicationResult {
    Published { evidence: PublicationEvidence },
    Skipped { reason: String },
}

pub fn verify(
    execution: &PublisherExecution,
    run: &PublisherRun,
    package: &str,
    feed: &mut impl ExactFeed,
) -> Result<PublicationResult> {
    if !execution.published_this_execution {
        return Ok(PublicationResult::Skipped {
            reason: "publisher did not publish in this execution".into(),
        });
    }
    if execution.run_id == 0
        || execution.run_id != run.id
        || !execution.repository.eq_ignore_ascii_case(&run.repository)
        || execution.merge_sha.len() != 40
        || !execution.merge_sha.bytes().all(|b| b.is_ascii_hexdigit())
        || execution.merge_sha != run.head_sha
    {
        bail!("publisher workflow does not match this execution's repository, run and merge SHA");
    }
    if !run.succeeded || !run.push_step_succeeded {
        return Ok(PublicationResult::Skipped {
            reason: "publisher push step did not succeed".into(),
        });
    }
    let pushes = new_nuget_pushes(&run.push_step_log, package)?;
    let Some(version) = pushes.first() else {
        return Ok(PublicationResult::Skipped {
            reason: "no new package push proved by the publisher step".into(),
        });
    };
    if pushes.len() != 1 {
        bail!("publisher pushed multiple versions of the requested package");
    }
    if !feed.contains(package, version)? {
        bail!("the exact newly published package version is not in the feed yet");
    }
    Ok(PublicationResult::Published {
        evidence: PublicationEvidence {
            repository: run.repository.clone(),
            merge_sha: run.head_sha.clone(),
            run_id: run.id,
            package: package.into(),
            version: version.clone(),
        },
    })
}

pub fn new_nuget_pushes(log: &str, package: &str) -> Result<Vec<String>> {
    if package.is_empty()
        || !package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        bail!("invalid NuGet package id");
    }
    let pushing = Regex::new(r"(?i)Pushing\s+([^\s/\\]+)\.nupkg\s+to\s")?;
    let named = Regex::new(&format!(
        r"(?i)^{}\.([0-9]+(?:\.[0-9]+){{1,3}}(?:-[0-9A-Za-z.-]+)?)$",
        regex::escape(package)
    ))?;
    let mut pending = None;
    let mut versions = Vec::new();
    let mut saw_push = false;
    for line in log.lines() {
        if let Some(found) = pushing.captures(line) {
            if pending.is_some() {
                bail!("publisher log has an incomplete push for the requested package");
            }
            saw_push = true;
            pending = named.captures(&found[1]).map(|m| m[1].to_string());
        } else if line.contains("Your package was pushed.") {
            if let Some(version) = pending.take() {
                versions.push(version);
            }
        } else if line.to_ascii_lowercase().contains("conflict")
            || line.to_ascii_lowercase().contains("already exists")
            || line.to_ascii_lowercase().contains("already been pushed")
            || line.to_ascii_lowercase().contains("skip")
        {
            pending = None;
        }
    }
    if !saw_push || pending.is_some() {
        bail!("publisher logs do not prove a complete push or duplicate outcome");
    }
    versions.sort();
    versions.dedup();
    Ok(versions)
}
