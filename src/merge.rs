use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::emit::PAUSE_LINE;
use crate::github::Run;

mod storage;

pub use storage::resolve_state_path;

pub const SCHEMA: u32 = 1;
pub const STOPPED: &str = "stopped after an earlier failure";
pub const EXIT_STOPPED: i32 = 5;

const GENERIC_GATE: &str = "confirm DDL/infra before continuing";
const GOOD_CONCLUSIONS: [&str; 3] = ["success", "skipped", "neutral"];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    NoRuns,
    Waiting(String),
    Failed(String),
    Passed(usize),
    Skipped(usize),
}

fn same_workflow(a: &Run, b: &Run) -> bool {
    if a.workflow_id != 0 || b.workflow_id != 0 {
        a.workflow_id == b.workflow_id
    } else {
        a.name == b.name
    }
}

fn superseded(run: &Run, all: &[Run]) -> bool {
    run.status == "completed"
        && run.conclusion.as_deref() == Some("cancelled")
        && all
            .iter()
            .any(|other| other.id > run.id && same_workflow(other, run))
}

pub fn judge(all: &[Run]) -> Verdict {
    if all.is_empty() {
        return Verdict::NoRuns;
    }
    let runs: Vec<&Run> = all.iter().filter(|run| !superseded(run, all)).collect();
    for run in runs.iter().filter(|run| run.status == "completed") {
        let conclusion = run.conclusion.as_deref().unwrap_or("unknown");
        if !GOOD_CONCLUSIONS.contains(&conclusion) {
            return Verdict::Failed(format!(
                "workflow {} ended as {conclusion}: {}",
                run.name, run.url
            ));
        }
    }
    let running = runs.iter().filter(|run| run.status != "completed").count();
    if running > 0 {
        return Verdict::Waiting(format!(
            "{running} of {} workflow runs still going",
            runs.len()
        ));
    }
    if runs
        .iter()
        .all(|run| run.conclusion.as_deref() == Some("skipped"))
    {
        Verdict::Skipped(runs.len())
    } else {
        Verdict::Passed(runs.len())
    }
}

pub fn judge_expected(all: &[Run], prediction: &crate::workflows::PushRuns) -> Verdict {
    let crate::workflows::PushRuns::Expected {
        workflows,
        chained_workflows,
        untracked_chains,
    } = prediction
    else {
        return judge(all);
    };
    let paths: Vec<_> = workflows.iter().chain(chained_workflows).collect();
    let runs: Vec<_> = all
        .iter()
        .filter(|run| {
            paths.contains(&&run.path)
                && !(run.status == "completed"
                    && run.conclusion.as_deref() == Some("cancelled")
                    && all.iter().any(|other| {
                        other.path == run.path && other.id > run.id && same_workflow(other, run)
                    }))
        })
        .collect();
    for run in &runs {
        if run.status == "completed"
            && !matches!(run.conclusion.as_deref(), Some("success" | "skipped"))
        {
            return Verdict::Failed(format!(
                "workflow {} ended as {}: {}",
                run.name,
                run.conclusion.as_deref().unwrap_or("unknown"),
                run.url
            ));
        }
    }
    if !untracked_chains.is_empty() {
        return Verdict::Waiting(
            untracked_chains
                .iter()
                .map(|path| format!("chained workflow {path} cannot be tracked"))
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    if all.is_empty() {
        return Verdict::NoRuns;
    }
    let missing: Vec<_> = paths
        .iter()
        .filter(|path| !runs.iter().any(|run| &run.path == **path))
        .map(|path| path.as_str())
        .collect();
    if !missing.is_empty() || paths.is_empty() {
        return Verdict::Waiting(format!(
            "waiting for expected workflows: {}",
            missing.join(", ")
        ));
    }
    let running = runs.iter().filter(|run| run.status != "completed").count();
    if running > 0 {
        return Verdict::Waiting(format!(
            "{running} of {} expected workflow runs still going",
            runs.len()
        ));
    }
    if runs
        .iter()
        .all(|run| run.conclusion.as_deref() == Some("skipped"))
    {
        Verdict::Skipped(runs.len())
    } else {
        Verdict::Passed(runs.len())
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub independent: BTreeMap<String, String>,
    pub dependents: Vec<String>,
    pub has_consumers: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum DependencyFailure {
    #[default]
    Stop,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    pub repo: String,
    pub base: String,
    pub sha: Option<String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoreOperation {
    Add(Pending),
    SetSha {
        id: String,
        sha: String,
    },
    Remove(String),
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
            state: State::default(),
            remote: None,
        }
    }

    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self::in_memory());
        };
        let state = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("the merge state file {} is not valid", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => {
                return Err(anyhow!(
                    "cannot read the merge state file {}: {error}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            path: Some(path),
            state,
            remote: None,
        })
    }

    pub fn remote(
        state: State,
        forward: impl FnMut(StoreOperation) -> Result<()> + Send + 'static,
    ) -> Self {
        Self {
            path: None,
            state,
            remote: Some(Box::new(forward)),
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
            StoreOperation::SetSha { id, sha } => self.set_sha(&id, &sha),
            StoreOperation::Remove(id) => self.remove(&id),
            StoreOperation::AddDeployPr { slug, branch, id } => {
                self.add_deploy_pr(&slug, &branch, &id)
            }
            StoreOperation::RemoveDeployPr { slug, branch } => {
                self.remove_deploy_pr(&slug, &branch)
            }
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.state.schema = SCHEMA;
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, serde_json::to_string_pretty(&self.state)?)
            .with_context(|| format!("cannot write {}", temp.display()))?;
        fs::rename(&temp, path).with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn add(&mut self, pending: Pending) -> Result<()> {
        self.forward(StoreOperation::Add(pending.clone()))?;
        self.state.pending.retain(|p| p.id != pending.id);
        self.state.pending.push(pending);
        self.save()
    }

    pub fn set_sha(&mut self, id: &str, sha: &str) -> Result<()> {
        self.forward(StoreOperation::SetSha {
            id: id.to_string(),
            sha: sha.to_string(),
        })?;
        if let Some(found) = self.state.pending.iter_mut().find(|p| p.id == id) {
            found.sha = Some(sha.to_string());
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.forward(StoreOperation::Remove(id.to_string()))?;
        self.state.pending.retain(|p| p.id != id);
        self.save()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub kind: Kind,
    pub target: String,
    pub status: Status,
    pub detail: String,
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
}

impl StepReport {
    pub fn new(kind: Kind, target: &str, status: Status, detail: &str) -> Self {
        Self {
            kind,
            target: target.to_string(),
            status,
            detail: detail.to_string(),
            merge_sha: None,
            deploy: None,
            blocked_by: None,
            bump: None,
            deploy_prediction: None,
            expected_runs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NotMerging {
    pub id: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
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
}

impl Report {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id: 0,
            workflow_id: 0,
            name: name.to_string(),
            path: format!(".github/workflows/{name}.yml"),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            url: format!("https://example.test/{name}"),
        }
    }

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
    fn no_runs_means_nothing_to_judge_yet() {
        assert_eq!(judge(&[]), Verdict::NoRuns);
    }

    #[test]
    fn only_all_skipped_runs_are_a_normal_skipped_deploy() {
        assert_eq!(
            judge(&[run("publish", "completed", Some("skipped"))]),
            Verdict::Skipped(1)
        );
        assert_eq!(
            judge(&[
                numbered(1, 1, "completed", Some("success")),
                numbered(2, 2, "completed", Some("skipped"))
            ]),
            Verdict::Passed(2)
        );
        assert_eq!(
            judge(&[run("audit", "completed", Some("neutral"))]),
            Verdict::Passed(1)
        );
        assert!(matches!(
            judge(&[
                numbered(1, 1, "completed", Some("failure")),
                numbered(2, 2, "completed", Some("skipped"))
            ]),
            Verdict::Failed(_)
        ));
    }

    #[test]
    fn a_failed_run_wins_even_while_others_still_run() {
        let runs = [
            run("build", "in_progress", None),
            run("deploy", "completed", Some("failure")),
        ];

        assert!(
            matches!(judge(&runs), Verdict::Failed(text) if text.contains("deploy ended as failure"))
        );
    }

    #[test]
    fn cancelled_and_timed_out_runs_are_failures() {
        for conclusion in [
            "cancelled",
            "timed_out",
            "action_required",
            "startup_failure",
        ] {
            let runs = [run("deploy", "completed", Some(conclusion))];
            assert!(matches!(judge(&runs), Verdict::Failed(_)), "{conclusion}");
        }
    }

    fn numbered(id: u64, workflow: u64, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            workflow_id: workflow,
            ..run("ci", status, conclusion)
        }
    }

    #[test]
    fn a_cancelled_run_that_a_newer_run_of_the_same_workflow_replaced_is_ignored() {
        let runs = [
            numbered(1, 7, "completed", Some("cancelled")),
            numbered(2, 7, "completed", Some("success")),
        ];

        assert_eq!(judge(&runs), Verdict::Passed(1));
    }

    #[test]
    fn a_replacement_still_running_is_waited_for() {
        let runs = [
            numbered(1, 7, "completed", Some("cancelled")),
            numbered(2, 7, "in_progress", None),
        ];

        assert_eq!(
            judge(&runs),
            Verdict::Waiting("1 of 1 workflow runs still going".to_string())
        );
    }

    #[test]
    fn a_cancelled_run_nobody_replaced_still_fails() {
        let runs = [
            numbered(1, 7, "completed", Some("cancelled")),
            numbered(2, 8, "completed", Some("success")),
        ];

        assert!(matches!(judge(&runs), Verdict::Failed(_)));
        let alone = [numbered(3, 7, "completed", Some("cancelled"))];
        assert!(matches!(judge(&alone), Verdict::Failed(_)));
    }

    #[test]
    fn a_newer_cancelled_run_is_not_replaced_by_an_older_one() {
        let runs = [
            numbered(5, 7, "completed", Some("success")),
            numbered(6, 7, "completed", Some("cancelled")),
        ];

        assert!(matches!(judge(&runs), Verdict::Failed(_)));
    }

    #[test]
    fn running_runs_wait_and_finished_good_runs_pass() {
        let running = [
            run("build", "completed", Some("success")),
            run("deploy", "queued", None),
        ];
        assert_eq!(
            judge(&running),
            Verdict::Waiting("1 of 2 workflow runs still going".to_string())
        );
        let done = [
            run("build", "completed", Some("success")),
            run("lint", "completed", Some("skipped")),
        ];
        assert_eq!(judge(&done), Verdict::Passed(2));
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
                id: "acme/api#1".to_string(),
                repo: "acme/api".to_string(),
                base: "main".to_string(),
                sha: None,
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
