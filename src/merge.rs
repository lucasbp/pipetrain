use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::emit::PAUSE_LINE;

mod archive;
pub mod category;
mod estimate;
mod report;
mod storage;
mod verdict;

pub use archive::{Archival, ArchivedState, archived_beside};
pub use estimate::{ApiEstimate, POLL_FLAT_STEPS, estimated_polls, poll_delay};
pub use report::{
    DeployOutcome, ExpectedRun, Kind, NotMerging, REPORT_SCHEMA, Report, Status, StepReport,
    Summary,
};
pub use storage::{PendingPreview, StatePreview, resolve_state_path};
pub(crate) use verdict::settled_ids;
pub use verdict::{Verdict, judge, judge_expected};

pub const SCHEMA: u32 = 2;
pub const STOPPED: &str = "stopped after an earlier failure";
pub const EXIT_STOPPED: i32 = 5;

const GENERIC_GATE: &str = "confirm DDL/infra before continuing";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Merge(String),
    Gate(String),
    Wait(String),
}

pub fn steps(lines: &[String]) -> Vec<Step> {
    let mut out = Vec::new();
    let mut reason: Option<String> = None;
    for line in lines {
        if let Some(id) = line.strip_prefix("merge:") {
            out.push(Step::Merge(id.trim().to_string()));
            reason = None;
        } else if line == PAUSE_LINE {
            out.push(Step::Gate(
                reason.take().unwrap_or_else(|| GENERIC_GATE.to_string()),
            ));
        } else if let Some(text) = line.strip_prefix("# gate: ") {
            reason = Some(text.to_string());
        } else if let Some(text) = line.strip_prefix("# wait: ") {
            out.push(Step::Wait(text.to_string()));
        }
    }
    out
}

pub fn wait_is_published(text: &str) -> bool {
    text.ends_with("(published)")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub independent: BTreeMap<String, String>,
    pub dependents: Vec<String>,
    pub has_consumers: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DependencyFailure {
    Stop,
    #[default]
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    #[serde(default)]
    pub recorded_at: Option<String>,
    pub id: String,
    pub repo: String,
    pub base: String,
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_override: Option<AdminOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_prediction: Option<crate::workflows::PushRuns>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPr {
    pub slug: String,
    pub branch: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    Discarded,
    ResolvedByLaterDeploy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub status: AuditAction,
    pub sha: String,
    pub runs: Vec<ExpectedRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub repo: String,
    pub base: String,
    pub sha: Option<String>,
    pub action: AuditAction,
    pub reason: String,
    pub recorded_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

impl AuditEntry {
    pub fn new(pending: &Pending, action: AuditAction, reason: String) -> Self {
        Self {
            id: pending.id.clone(),
            repo: pending.repo.clone(),
            base: pending.base.clone(),
            sha: pending.sha.clone(),
            action,
            reason,
            recorded_at: chrono::Utc::now().to_rfc3339(),
            resolution: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BumpBehind {
    pub id: String,
    pub package: String,
    pub from: String,
    pub to: String,
    pub number: u64,
    pub pull_request_url: String,
    pub branch: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub queue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bump: Option<crate::consumer_bump::journal::Journal>,
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub pending: Vec<Pending>,
    #[serde(default)]
    pub deploy_prs: Vec<DeployPr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_jobs: Vec<String>,
    #[serde(default)]
    pub audit: Vec<AuditEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admin_overrides: Vec<AdminOverrideRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bump_behind: Vec<BumpBehind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoreOperation {
    Add(Pending),
    SetSha {
        id: String,
        sha: String,
        #[serde(default)]
        admin_override: Option<AdminOverride>,
    },
    Remove(String),
    Archive(AuditEntry),
    AddDeployPr {
        slug: String,
        branch: String,
        id: String,
    },
    RemoveDeployPr {
        slug: String,
        branch: String,
    },
}

type RemoteStore = Box<dyn FnMut(StoreOperation) -> Result<()> + Send>;

pub struct Store {
    path: Option<PathBuf>,
    pub state: State,
    remote: Option<RemoteStore>,
    lock: Option<std::fs::File>,
}

pub fn default_state_path() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|dir| dir.join("pipetrain").join("merge-state.json"))
}

impl Store {
    pub fn in_memory() -> Self {
        Self {
            path: None,
            state: State {
                schema: SCHEMA,
                ..State::default()
            },
            remote: None,
            lock: None,
        }
    }

    pub fn remote(
        state: State,
        forward: impl FnMut(StoreOperation) -> Result<()> + Send + 'static,
    ) -> Self {
        Self {
            path: None,
            state,
            remote: Some(Box::new(forward)),
            lock: None,
        }
    }

    fn forward(&mut self, operation: StoreOperation) -> Result<()> {
        if let Some(remote) = &mut self.remote {
            remote(operation)?;
        }
        Ok(())
    }

    pub fn apply(&mut self, operation: StoreOperation) -> Result<()> {
        match operation {
            StoreOperation::Add(pending) => self.add(pending),
            StoreOperation::SetSha {
                id,
                sha,
                admin_override,
            } => self.set_sha_record(&id, &sha, admin_override),
            StoreOperation::Remove(id) => self.remove(&id),
            StoreOperation::Archive(entry) => self.archive(entry),
            StoreOperation::AddDeployPr { slug, branch, id } => {
                self.add_deploy_pr(&slug, &branch, &id)
            }
            StoreOperation::RemoveDeployPr { slug, branch } => {
                self.remove_deploy_pr(&slug, &branch)
            }
        }
    }

    pub fn add(&mut self, pending: Pending) -> Result<()> {
        self.forward(StoreOperation::Add(pending.clone()))?;
        self.state.pending.retain(|p| p.id != pending.id);
        self.state.pending.push(pending);
        self.save()
    }

    pub fn set_sha(&mut self, id: &str, sha: &str) -> Result<()> {
        let mut admin_override = self
            .state
            .pending
            .iter()
            .find(|pending| pending.id == id)
            .and_then(|pending| pending.admin_override.clone());
        if let Some(admin) = &mut admin_override {
            admin.at.get_or_insert_with(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
            });
        }
        self.set_sha_record(id, sha, admin_override)
    }

    fn set_sha_record(
        &mut self,
        id: &str,
        sha: &str,
        admin_override: Option<AdminOverride>,
    ) -> Result<()> {
        self.forward(StoreOperation::SetSha {
            id: id.to_string(),
            sha: sha.to_string(),
            admin_override: admin_override.clone(),
        })?;
        if let Some(found) = self.state.pending.iter_mut().find(|p| p.id == id) {
            found.sha = Some(sha.to_string());
            found.admin_override = admin_override;
            if let Some(admin) = &found.admin_override
                && let Some(at) = &admin.at
                && !self.state.admin_overrides.iter().any(|record| {
                    record.id == found.id
                        && record.repo == found.repo
                        && record.base == found.base
                        && record.sha == sha
                })
            {
                self.state.admin_overrides.push(AdminOverrideRecord {
                    id: found.id.clone(),
                    repo: found.repo.clone(),
                    base: found.base.clone(),
                    sha: sha.to_string(),
                    reason: admin.reason.clone(),
                    requested_by: admin.requested_by.clone(),
                    at: at.clone(),
                });
            }
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.forward(StoreOperation::Remove(id.to_string()))?;
        self.state.pending.retain(|p| p.id != id);
        self.save()
    }

    pub fn archive(&mut self, entry: AuditEntry) -> Result<()> {
        if !self.state.pending.iter().any(|pending| {
            pending.id == entry.id
                && pending.repo == entry.repo
                && pending.base == entry.base
                && pending.sha == entry.sha
        }) {
            anyhow::bail!(
                "pending record {} no longer matches the audit entry",
                entry.id
            );
        }
        self.forward(StoreOperation::Archive(entry.clone()))?;
        let previous = self.state.clone();
        self.state.pending.retain(|pending| pending.id != entry.id);
        self.state.audit.push(entry);
        if let Err(error) = self.save() {
            self.state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn add_deploy_pr(&mut self, slug: &str, branch: &str, id: &str) -> Result<()> {
        self.forward(StoreOperation::AddDeployPr {
            slug: slug.to_string(),
            branch: branch.to_string(),
            id: id.to_string(),
        })?;
        match self
            .state
            .deploy_prs
            .iter_mut()
            .find(|found| found.slug == slug && found.branch == branch)
        {
            Some(found) => {
                if !found.ids.iter().any(|known| known == id) {
                    found.ids.push(id.to_string());
                }
            }
            None => self.state.deploy_prs.push(DeployPr {
                slug: slug.to_string(),
                branch: branch.to_string(),
                ids: vec![id.to_string()],
            }),
        }
        self.save()
    }

    pub fn remove_deploy_pr(&mut self, slug: &str, branch: &str) -> Result<()> {
        self.forward(StoreOperation::RemoveDeployPr {
            slug: slug.to_string(),
            branch: branch.to_string(),
        })?;
        self.state
            .deploy_prs
            .retain(|found| !(found.slug == slug && found.branch == branch));
        self.save()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminOverride {
    pub reason: String,
    pub requested_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminOverrideRecord {
    pub id: String,
    pub repo: String,
    pub base: String,
    pub sha: String,
    pub reason: String,
    pub requested_by: String,
    pub at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_queue_lines_become_steps_and_comments_are_dropped() {
        let queue = lines(&[
            "# conflict: acme/api#1 and acme/web#2 both change 1 (same-file)",
            "# bundle acme/api: 2 PRs into main could be 1 deploy (saves 1): acme/api#1",
            "merge:acme/api#1",
            "# gate: acme/web#2 waits for acme/api#1 (gate: db/001.sql)",
            PAUSE_LINE,
            "# wait: package acme.core 1.2.0 from acme/api#1 must be published before acme/web#2 (not yet)",
            "merge:acme/web#2",
        ]);

        assert_eq!(
            steps(&queue),
            vec![
                Step::Merge("acme/api#1".to_string()),
                Step::Gate("acme/web#2 waits for acme/api#1 (gate: db/001.sql)".to_string()),
                Step::Wait(
                    "package acme.core 1.2.0 from acme/api#1 must be published before acme/web#2 (not yet)"
                        .to_string()
                ),
                Step::Merge("acme/web#2".to_string()),
            ]
        );
    }

    #[test]
    fn a_pause_without_a_gate_line_gets_the_generic_reason() {
        let queue = lines(&["merge:acme/api#1", PAUSE_LINE, "merge:acme/web#2"]);

        assert_eq!(
            steps(&queue)[1],
            Step::Gate("confirm DDL/infra before continuing".to_string())
        );
    }

    #[test]
    fn only_a_published_wait_passes_by_itself() {
        assert!(wait_is_published(
            "package a 1.0.0 from x#1 must be published before y#2 (published)"
        ));
        assert!(!wait_is_published(
            "package a 1.0.0 from x#1 must be published before y#2 (not yet)"
        ));
        assert!(!wait_is_published(
            "package a from x#1 must be published before y#2"
        ));
    }

    #[test]
    fn the_state_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("pipetrain-state-{}", std::process::id()));
        let path = dir.join("merge-state.json");
        let _ = fs::remove_dir_all(&dir);

        let mut store = Store::open(Some(path.clone())).unwrap();
        assert!(store.state.pending.is_empty());
        store
            .add(Pending {
                recorded_at: None,
                id: "acme/api#1".to_string(),
                repo: "acme/api".to_string(),
                base: "main".to_string(),
                sha: None,
                admin_override: None,
                continuation: None,
                deploy_prediction: None,
            })
            .unwrap();
        store.set_sha("acme/api#1", "abc123").unwrap();

        let reopened = Store::open(Some(path.clone())).unwrap();
        assert_eq!(reopened.state.pending.len(), 1);
        assert_eq!(reopened.state.pending[0].sha.as_deref(), Some("abc123"));

        let mut again = reopened;
        again.remove("acme/api#1").unwrap();
        assert!(Store::open(Some(path)).unwrap().state.pending.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_state_file_is_an_error_not_a_reset() {
        let dir = std::env::temp_dir().join(format!("pipetrain-broken-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("merge-state.json");
        fs::write(&path, "{ not json").unwrap();

        let error = Store::open(Some(path)).err().expect("an error");

        assert!(format!("{error:#}").contains("not valid"));
        let _ = fs::remove_dir_all(&dir);
    }
}
