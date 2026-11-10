use crate::publication::{PublicationEvidence, PublicationResult};
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
pub mod backend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consumer {
    pub repository: String,
    pub number: u64,
    pub head_branch: String,
    pub head_sha: String,
    pub default_branch: String,
    pub bump_branch: String,
    pub solution: String,
    pub manifests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreparedBump {
    pub consumer_repository: String,
    pub consumer_number: u64,
    pub consumer_head: String,
    pub branch: String,
    pub base: String,
    pub commit: String,
    pub pull_request_url: String,
    pub publications: Vec<PublicationEvidence>,
    pub needs_review: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrepareResult {
    Skipped { reason: String },
    Prepared { bump: Box<PreparedBump> },
}

pub trait BumpGateway {
    fn consumer_head(&mut self, consumer: &Consumer) -> Result<String>;
    fn create_worktree(&mut self, consumer: &Consumer) -> Result<()>;
    fn read(&mut self, path: &str) -> Result<String>;
    fn write(&mut self, path: &str, text: &str) -> Result<()>;
    fn restore(&mut self, solution: &str, locked: bool) -> Result<()>;
    fn changed_files(&mut self) -> Result<Vec<String>>;
    fn commit(&mut self, paths: &[String], message: &str) -> Result<String>;
    fn push_new_branch(&mut self, branch: &str, commit: &str) -> Result<()>;
    fn open_review_pr(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String>;
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub fn pin_manifest(text: &str, package: &str, version: &str) -> Result<String> {
    if Regex::new(r"(?is)<(?:ItemGroup\b[^>]*\bCondition\s*=|Choose\b|When\b)")?.is_match(text) {
        bail!("conditional package groups require manual review");
    }
    let comments = Regex::new(r"(?s)<!--.*?-->")?;
    let mut visible = text.to_string();
    for comment in comments
        .find_iter(text)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        visible.replace_range(comment.range(), &" ".repeat(comment.len()));
    }
    let tag = Regex::new(r"(?is)<Package(?:Reference|Version)\b[^>]*>")?;
    let id = Regex::new(r#"(?i)\b(?:Include|Update)\s*=\s*"([^"]+)""#)?;
    let ver = Regex::new(r#"(?i)\bVersion\s*=\s*"([^"]+)""#)?;
    let mut edits = Vec::new();
    let conditional = Regex::new(r"(?i)\b(?:Condition|VersionOverride)\s*=")?;
    for found in tag.find_iter(&visible) {
        let value = found.as_str();
        if !id
            .captures(value)
            .is_some_and(|m| m[1].eq_ignore_ascii_case(package))
        {
            continue;
        }
        if conditional.is_match(value) {
            bail!("conditional NuGet pins require manual review");
        }
        let Some(capture) = ver.captures(value) else {
            bail!("NuGet reference has no literal Version attribute");
        };
        let current = capture.get(1).expect("version capture");
        if !current
            .as_str()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
        {
            bail!("NuGet ranges or property versions require manual review");
        }
        if crate::feed::compare(current.as_str(), version).is_gt() {
            bail!("refusing to downgrade consumer dependency");
        }
        edits.push((
            found.start() + current.start(),
            found.start() + current.end(),
        ));
    }
    if edits.is_empty() {
        bail!("requested package not found in selected manifest");
    }
    let mut patched = text.to_string();
    for (start, end) in edits.into_iter().rev() {
        patched.replace_range(start..end, version);
    }
    Ok(patched)
}

fn ensure_head(gateway: &mut impl BumpGateway, consumer: &Consumer) -> Result<()> {
    if gateway.consumer_head(consumer)? != consumer.head_sha {
        bail!("consumer head moved; replan before preparing a bump");
    }
    Ok(())
}

pub fn prepare(
    publication: &PublicationResult,
    consumer: &Consumer,
    gateway: &mut impl BumpGateway,
) -> Result<PrepareResult> {
    prepare_many(std::slice::from_ref(publication), consumer, gateway)
}

pub fn prepare_many(
    publications: &[PublicationResult],
    consumer: &Consumer,
    gateway: &mut impl BumpGateway,
) -> Result<PrepareResult> {
    let mut evidence_by_package: BTreeMap<String, &PublicationEvidence> = BTreeMap::new();
    for result in publications {
        if let PublicationResult::Published { evidence } = result {
            let package = evidence.package().to_ascii_lowercase();
            if let Some(previous) = evidence_by_package.insert(package, evidence)
                && previous != evidence
            {
                bail!("conflicting publication evidence for the same package");
            }
        }
    }
    if evidence_by_package.is_empty() {
        return Ok(PrepareResult::Skipped {
            reason: "publisher did not produce a new package in this execution".into(),
        });
    }
    if consumer.head_branch == consumer.default_branch
        || consumer.bump_branch == consumer.default_branch
        || consumer.bump_branch == consumer.head_branch
        || !consumer.bump_branch.starts_with("pipetrain/bump/")
        || consumer.number == 0
        || consumer.head_sha.len() != 40
        || !consumer.head_sha.bytes().all(|b| b.is_ascii_hexdigit())
        || consumer.manifests.is_empty()
        || !safe_path(&consumer.solution)
        || consumer
            .manifests
            .iter()
            .any(|p| !safe_path(p) || !(p.ends_with(".csproj") || p.ends_with(".props")))
    {
        bail!("consumer must name a PR head, isolated bump branch and safe NuGet project paths");
    }
    ensure_head(gateway, consumer)?;
    gateway.create_worktree(consumer)?;
    if !gateway.changed_files()?.is_empty() {
        bail!("new bump workspace is not clean");
    }
    let mut patches = BTreeMap::new();
    let mut changed_packages = BTreeSet::new();
    for path in &consumer.manifests {
        let original = gateway.read(path)?;
        let mut patched = original.clone();
        for (package, evidence) in &evidence_by_package {
            if crate::manifest::package_versions(path, &patched, evidence.package()).is_empty() {
                continue;
            }
            let next = pin_manifest(&patched, evidence.package(), evidence.version())?;
            if next != patched {
                changed_packages.insert(package.clone());
            }
            patched = next;
        }
        if original != patched {
            patches.insert(path.clone(), patched);
        }
    }
    if patches.is_empty() {
        return Ok(PrepareResult::Skipped {
            reason: "consumer already pins the published version".into(),
        });
    }
    for (path, text) in &patches {
        gateway.write(path, text)?;
    }
    gateway.restore(&consumer.solution, false)?;
    gateway.restore(&consumer.solution, true)?;
    let changed = gateway.changed_files()?;
    if changed.is_empty()
        || changed.iter().any(|p| {
            !safe_path(p)
                || (!patches.contains_key(p)
                    && !p.ends_with("/packages.lock.json")
                    && p != "packages.lock.json")
        })
    {
        bail!("restore changed files outside the selected pins and NuGet lock files");
    }
    for path in patches.keys() {
        let current = gateway.read(path)?;
        for package in &changed_packages {
            let evidence = evidence_by_package[package];
            if !crate::manifest::package_versions(path, &current, evidence.package()).is_empty()
                && pin_manifest(&current, evidence.package(), evidence.version())? != current
            {
                bail!("restore changed the requested package pin");
            }
        }
    }
    let mut locked = BTreeSet::new();
    for path in changed.iter().filter(|p| p.ends_with("packages.lock.json")) {
        let value: serde_json::Value = serde_json::from_str(&gateway.read(path)?)?;
        for framework in value["dependencies"]
            .as_object()
            .context("invalid NuGet lock dependency graph")?
            .values()
        {
            for (package, dependency) in framework
                .as_object()
                .context("invalid NuGet target graph")?
            {
                let key = package.to_ascii_lowercase();
                if changed_packages.contains(&key) {
                    let evidence = evidence_by_package[&key];
                    if dependency["resolved"] != evidence.version() {
                        bail!("lock file resolves a different package version");
                    }
                    locked.insert(key);
                }
            }
        }
    }
    if locked != changed_packages {
        bail!("restore did not produce the exact published package in a changed lock file");
    }
    ensure_head(gateway, consumer)?;
    let pins = changed_packages
        .iter()
        .map(|package| {
            let e = evidence_by_package[package];
            format!("{} to {}", e.package(), e.version())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let commit = gateway.commit(&changed, &format!("bump {pins}"))?;
    ensure_head(gateway, consumer)?;
    gateway.push_new_branch(&consumer.bump_branch, &commit)?;
    ensure_head(gateway, consumer)?;
    let evidence = changed_packages
        .iter()
        .map(|package| evidence_by_package[package].clone())
        .collect::<Vec<_>>();
    let provenance = evidence
        .iter()
        .map(|e| {
            format!(
                "{} {} published by {} at {} in workflow run {}",
                e.package(),
                e.version(),
                e.repository(),
                e.merge_sha(),
                e.run_id()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let body = format!(
        "Pins {provenance}. Restore and locked-mode restore passed. Requires new CI and human review before the consumer PR can proceed. Existing feature dependencies remain required."
    );
    let url = gateway.open_review_pr(consumer, &commit, &body)?;
    Ok(PrepareResult::Prepared {
        bump: Box::new(PreparedBump {
            consumer_repository: consumer.repository.clone(),
            consumer_number: consumer.number,
            consumer_head: consumer.head_sha.clone(),
            branch: consumer.bump_branch.clone(),
            base: consumer.head_branch.clone(),
            commit,
            pull_request_url: url,
            publications: evidence,
            needs_review: true,
        }),
    })
}
