use crate::publication::{PublicationEvidence, PublicationResult};
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
pub mod apply;
pub mod backend;
pub mod journal;
pub(crate) mod pin;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum RestoreMode {
    #[default]
    Locked,
    Unlocked,
    Skip,
}

impl RestoreMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locked => "locked",
            Self::Unlocked => "unlocked",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreSettings {
    pub mode: RestoreMode,
    pub solution: Option<String>,
}

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
    fn restore_plain(&mut self, _solution: &str) -> Result<()> {
        bail!("this workspace cannot run a plain restore")
    }
    fn build(&mut self, _solution: &str) -> Result<()> {
        bail!("this workspace cannot run a build")
    }
    fn changed_files(&mut self) -> Result<Vec<String>>;
    fn commit(&mut self, paths: &[String], message: &str) -> Result<String>;
    fn push_new_branch(&mut self, branch: &str, commit: &str) -> Result<()>;
    fn open_review_pr(&mut self, consumer: &Consumer, commit: &str, body: &str) -> Result<String>;
}

pub(crate) fn safe_path(path: &str) -> bool {
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

pub(crate) fn ensure_head(gateway: &mut impl BumpGateway, consumer: &Consumer) -> Result<()> {
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

pub(crate) struct Pinned {
    pub commit: String,
    pub packages: BTreeSet<String>,
}

pub(crate) fn pin_commit_push(
    pins: &BTreeMap<String, (String, String)>,
    consumer: &Consumer,
    mode: RestoreMode,
    gateway: &mut impl BumpGateway,
) -> Result<Option<Pinned>> {
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
        for (key, (name, version)) in pins {
            if crate::manifest::package_versions(path, &patched, name).is_empty() {
                continue;
            }
            let next = pin_manifest(&patched, name, version)?;
            if next != patched {
                changed_packages.insert(key.clone());
            }
            patched = next;
        }
        if original != patched {
            patches.insert(path.clone(), patched);
        }
    }
    if patches.is_empty() {
        return Ok(None);
    }
    for (path, text) in &patches {
        gateway.write(path, text)?;
    }
    match mode {
        RestoreMode::Locked => {
            gateway.restore(&consumer.solution, false)?;
            gateway.restore(&consumer.solution, true)?;
        }
        RestoreMode::Unlocked => {
            gateway.restore_plain(&consumer.solution)?;
            gateway.build(&consumer.solution)?;
        }
        RestoreMode::Skip => {}
    }
    let changed = gateway.changed_files()?;
    let locks_allowed = mode != RestoreMode::Skip;
    if changed.is_empty()
        || changed.iter().any(|p| {
            !safe_path(p)
                || (!patches.contains_key(p)
                    && !(locks_allowed
                        && (p.ends_with("/packages.lock.json") || p == "packages.lock.json")))
        })
    {
        bail!("restore changed files outside the selected pins and NuGet lock files");
    }
    for path in patches.keys() {
        let current = gateway.read(path)?;
        for key in &changed_packages {
            let (name, version) = &pins[key];
            if !crate::manifest::package_versions(path, &current, name).is_empty()
                && pin_manifest(&current, name, version)? != current
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
                    if dependency["resolved"] != pins[&key].1.as_str() {
                        bail!("lock file resolves a different package version");
                    }
                    locked.insert(key);
                }
            }
        }
    }
    if mode == RestoreMode::Locked && locked != changed_packages {
        bail!("restore did not produce the exact published package in a changed lock file");
    }
    ensure_head(gateway, consumer)?;
    let described = changed_packages
        .iter()
        .map(|key| format!("{} to {}", pins[key].0, pins[key].1))
        .collect::<Vec<_>>()
        .join(", ");
    let commit = gateway.commit(&changed, &format!("bump {described}"))?;
    ensure_head(gateway, consumer)?;
    gateway.push_new_branch(&consumer.bump_branch, &commit)?;
    ensure_head(gateway, consumer)?;
    Ok(Some(Pinned {
        commit,
        packages: changed_packages,
    }))
}

pub(crate) fn published_packages(
    publications: &[PublicationResult],
) -> Result<BTreeMap<String, &PublicationEvidence>> {
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
    Ok(evidence_by_package)
}

pub fn prepare_many(
    publications: &[PublicationResult],
    consumer: &Consumer,
    gateway: &mut impl BumpGateway,
) -> Result<PrepareResult> {
    let evidence_by_package = published_packages(publications)?;
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
    let pins = evidence_by_package
        .iter()
        .map(|(key, evidence)| {
            (
                key.clone(),
                (
                    evidence.package().to_string(),
                    evidence.version().to_string(),
                ),
            )
        })
        .collect();
    let Some(pinned) = pin_commit_push(&pins, consumer, RestoreMode::Locked, gateway)? else {
        return Ok(PrepareResult::Skipped {
            reason: "consumer already pins the published version".into(),
        });
    };
    let commit = pinned.commit;
    let evidence = pinned
        .packages
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
