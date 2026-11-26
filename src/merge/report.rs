use serde::{Deserialize, Serialize};

use super::{
    AdminOverride, ApiEstimate, EXIT_STOPPED, Pending, Resolution, StatePreview, category,
};
use crate::github::Run;

pub const REPORT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedRun {
    pub path: String,
    pub name: String,
    pub html_url: String,
    pub status: String,
    pub conclusion: Option<String>,
}

impl From<&Run> for ExpectedRun {
    fn from(run: &Run) -> Self {
        Self {
            path: run.path.clone(),
            name: run.name.clone(),
            html_url: run.url.clone(),
            status: run.status.clone(),
            conclusion: run.conclusion.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Resume,
    Bump,
    Merge,
    Gate,
    Wait,
    #[serde(rename = "open-pr")]
    OpenPr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Planned,
    Done,
    Skipped,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BumpWait {
    Draft {
        key: crate::model::PrKey,
        base: String,
        head: String,
        sha: String,
    },
    Update {
        key: crate::model::PrKey,
        sha: String,
    },
    Readiness {
        key: crate::model::PrKey,
        sha: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub kind: Kind,
    pub target: String,
    pub status: Status,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_override: Option<AdminOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bump: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_prediction: Option<crate::workflows::PushRuns>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_runs: Vec<ExpectedRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_outcome: Option<DeployOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_seconds: Option<u64>,
    #[serde(skip)]
    pub inherited: bool,
    #[serde(skip)]
    pub wait: Option<BumpWait>,
}

impl StepReport {
    pub fn new(kind: Kind, target: &str, status: Status, detail: &str) -> Self {
        Self {
            kind,
            target: target.to_string(),
            status,
            detail: detail.to_string(),
            admin_override: None,
            merge_sha: None,
            deploy: None,
            blocked_by: None,
            bump: None,
            deploy_prediction: None,
            expected_runs: Vec::new(),
            resolution: None,
            deploy_outcome: None,
            settle_seconds: None,
            inherited: false,
            wait: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployOutcome {
    Confirmed,
    Dispensed,
    Indeterminate,
    Skipped,
    Failed,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Summary {
    pub merged: usize,
    pub earlier_merges_checked: usize,
    pub deploy_confirmed: usize,
    pub deploy_dispensed: usize,
    pub deploy_indeterminate: usize,
    pub deploy_resolved: usize,
    pub deploy_skipped: usize,
    pub deploy_failed: usize,
    pub failed: usize,
    pub stopped: usize,
}

impl Summary {
    pub fn from_steps<'a>(steps: impl IntoIterator<Item = &'a StepReport>) -> Self {
        let mut summary = Self::default();
        for step in steps {
            summary.failed += usize::from(step.status == Status::Failed);
            summary.stopped += usize::from(step.status == Status::Stopped);
            if !matches!(step.kind, Kind::Merge | Kind::Resume) || step.merge_sha.is_none() {
                continue;
            }
            summary.merged += usize::from(step.kind == Kind::Merge);
            summary.earlier_merges_checked += usize::from(step.kind == Kind::Resume);
            if step.resolution.is_some() {
                summary.deploy_resolved += 1;
                continue;
            }
            match step.deploy_outcome {
                Some(DeployOutcome::Confirmed) => summary.deploy_confirmed += 1,
                Some(DeployOutcome::Dispensed) => summary.deploy_dispensed += 1,
                Some(DeployOutcome::Indeterminate) | None => summary.deploy_indeterminate += 1,
                Some(DeployOutcome::Skipped) => summary.deploy_skipped += 1,
                Some(DeployOutcome::Failed) => summary.deploy_failed += 1,
            }
        }
        summary
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NotMerging {
    pub id: String,
    pub reasons: Vec<String>,
    pub state: category::Category,
}

impl NotMerging {
    pub fn new(id: &str, reasons: Vec<String>) -> Self {
        Self {
            id: id.to_string(),
            state: category::classify(&reasons),
            reasons,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub api_estimate: ApiEstimate,
    pub api_usage: crate::github::Usage,
    pub max_parallel: usize,
    pub summary: Summary,
    pub preflight: Vec<crate::model::Readiness>,
    pub state: StatePreview,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bump_enabled: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bump_candidates: Vec<String>,
    pub schema: u32,
    pub applied: bool,
    #[serde(skip)]
    pub requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    pub steps: Vec<StepReport>,
    pub not_merging: Vec<NotMerging>,
    pub questions: Vec<String>,
    #[serde(skip)]
    pub notices: Vec<String>,
    #[serde(skip)]
    pub held: Vec<Pending>,
}

impl Report {
    pub fn count_state(&self, state: category::Category) -> usize {
        self.not_merging
            .iter()
            .filter(|skipped| skipped.state == state)
            .count()
    }

    pub fn count(&self, status: Status) -> usize {
        self.steps.iter().filter(|s| s.status == status).count()
    }

    pub fn exit_code(&self) -> i32 {
        if self.count(Status::Failed) > 0 {
            1
        } else if self.count(Status::Stopped) > 0 {
            EXIT_STOPPED
        } else {
            0
        }
    }
}
