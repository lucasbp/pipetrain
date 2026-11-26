use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::command::bump::{Status as RowStatus, classify, in_ecosystem, refs_of};
use crate::config::Config;
use crate::consumer_bump::RestoreMode;
use crate::consumer_bump::apply::{
    Applied, Candidate, DefaultBranchRemote, apply_repo, branch_name, pull_request_title,
};
use crate::consumer_bump::backend::{LocalBackend, ProcessRunner};
use crate::feed::Ecosystem;
use crate::github::Client;
use crate::merge::{BumpBehind, Kind, Status, StepReport, Store};
use crate::planner::behind::Need;

const WORKSPACE_TIMEOUT: Duration = Duration::from_secs(600);
const BLOB_CHUNK: usize = 25;

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub restore_mode: Option<RestoreMode>,
    pub solution: Option<String>,
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opening {
    Opened {
        number: u64,
        url: String,
        branch: String,
    },
    AlreadyPinned,
    Skipped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Updated,
    Conflict(String),
}

pub trait Gateway {
    fn state(&mut self, repo: &str, number: u64) -> Result<PullState>;
    fn open(&mut self, need: &Need, to: &str) -> Result<Opening>;
    fn update(&mut self, repo: &str, number: u64) -> Result<Update>;
}

fn repo_of(id: &str) -> &str {
    id.split_once('#').map_or(id, |(repo, _)| repo)
}

fn number_of(id: &str) -> Option<u64> {
    id.split_once('#')?.1.parse().ok()
}

fn step(need_id: &str, status: Status, detail: impl AsRef<str>) -> StepReport {
    StepReport::new(Kind::Bump, need_id, status, detail.as_ref())
}

fn remove(store: &mut Store, index: usize) -> Result<BumpBehind> {
    let entry = store.state.bump_behind.remove(index);
    store.save()?;
    Ok(entry)
}

fn settled(store: &mut Store, index: usize, state: PullState) -> StepReport {
    let url = store.state.bump_behind[index].pull_request_url.clone();
    let id = store.state.bump_behind[index].id.clone();
    match remove(store, index) {
        Ok(_) => match state {
            PullState::Merged => step(
                &id,
                Status::Done,
                format!("bump PR {url} was merged, the next plan reads the new pin"),
            ),
            _ => step(
                &id,
                Status::Failed,
                format!("bump PR {url} was closed without merging, the next run opens a new one"),
            ),
        },
        Err(error) => step(
            &id,
            Status::Stopped,
            format!("cannot update the merge state: {error:#}"),
        ),
    }
}

fn release(gateway: &mut dyn Gateway, need: &Need, via: &str) -> StepReport {
    let Some(number) = number_of(&need.id) else {
        return step(&need.id, Status::Failed, "the PR id has no number");
    };
    match gateway.update(repo_of(&need.id), number) {
        Ok(Update::Updated) => step(
            &need.id,
            Status::Done,
            format!(
                "{via}; updated the branch of {} with its base, the next plan reads the new pin and releases it",
                need.id
            ),
        ),
        Ok(Update::Conflict(message)) => {
            let mut blocked = step(
                &need.id,
                Status::Skipped,
                format!(
                    "merge conflict updating the branch of {} with its base: {message}",
                    need.id
                ),
            );
            blocked.blocked_by = Some(need.id.clone());
            blocked
        }
        Err(error) => step(
            &need.id,
            Status::Failed,
            format!("cannot update the branch of {}: {error:#}", need.id),
        ),
    }
}

fn existing(store: &mut Store, gateway: &mut dyn Gateway, need: &Need, index: usize) -> StepReport {
    let entry = store.state.bump_behind[index].clone();
    match gateway.state(repo_of(&need.id), entry.number) {
        Ok(PullState::Open) => step(
            &need.id,
            Status::Skipped,
            format!(
                "bump PR {} is open, waiting for it to be merged",
                entry.pull_request_url
            ),
        ),
        Ok(PullState::Merged) => {
            let released = release(
                gateway,
                need,
                &format!("bump PR {} was merged", entry.pull_request_url),
            );
            if released.status == Status::Failed {
                return released;
            }
            match remove(store, index) {
                Ok(_) => released,
                Err(error) => step(
                    &need.id,
                    Status::Stopped,
                    format!("cannot update the merge state: {error:#}"),
                ),
            }
        }
        Ok(state) => settled(store, index, state),
        Err(error) => step(
            &need.id,
            Status::Failed,
            format!("cannot read bump PR {}: {error:#}", entry.pull_request_url),
        ),
    }
}

fn open_new(store: &mut Store, gateway: &mut dyn Gateway, need: &Need, to: &str) -> StepReport {
    match gateway.open(need, to) {
        Ok(Opening::Opened {
            number,
            url,
            branch,
        }) => {
            store.state.bump_behind.push(BumpBehind {
                id: need.id.clone(),
                package: need.package.clone(),
                from: need.from.clone(),
                to: to.to_string(),
                number,
                pull_request_url: url.clone(),
                branch,
                recorded_at: chrono::Utc::now().to_rfc3339(),
            });
            match store.save() {
                Ok(()) => step(
                    &need.id,
                    Status::Done,
                    format!(
                        "opened bump PR {url} for {} {to}, waiting for it to be merged",
                        need.package
                    ),
                ),
                Err(error) => step(
                    &need.id,
                    Status::Stopped,
                    format!("opened {url} but cannot save the merge state: {error:#}"),
                ),
            }
        }
        Ok(Opening::AlreadyPinned) => release(
            gateway,
            need,
            &format!("the default branch already pins {} {to}", need.package),
        ),
        Ok(Opening::Skipped(reason)) => step(
            &need.id,
            Status::Failed,
            format!("no bump PR opened: {reason}"),
        ),
        Err(error) => step(&need.id, Status::Failed, format!("{error:#}")),
    }
}

fn share(store: &mut Store, need: &Need, to: &str) -> Option<usize> {
    let shared = store
        .state
        .bump_behind
        .iter()
        .find(|entry| {
            entry.package == need.package
                && entry.to == to
                && repo_of(&entry.id) == repo_of(&need.id)
        })?
        .clone();
    store.state.bump_behind.push(BumpBehind {
        id: need.id.clone(),
        from: need.from.clone(),
        ..shared
    });
    store.save().ok()?;
    Some(store.state.bump_behind.len() - 1)
}

pub fn run(needs: &[Need], store: &mut Store, gateway: &mut dyn Gateway) -> Vec<StepReport> {
    let mut steps = Vec::new();
    for need in needs {
        let found = store
            .state
            .bump_behind
            .iter()
            .position(|entry| entry.id == need.id && entry.package == need.package);
        let report = match (found, need.to.as_deref()) {
            (Some(index), _) => existing(store, gateway, need, index),
            (None, Some(to)) => match share(store, need, to) {
                Some(index) => existing(store, gateway, need, index),
                None => open_new(store, gateway, need, to),
            },
            (None, None) => step(
                &need.id,
                Status::Skipped,
                format!(
                    "{} is published by this queue, --bump handles it after the publisher deploys",
                    need.package
                ),
            ),
        };
        steps.push(report);
    }
    let orphans: Vec<BumpBehind> = store
        .state
        .bump_behind
        .iter()
        .filter(|entry| {
            !needs
                .iter()
                .any(|need| need.id == entry.id && need.package == entry.package)
        })
        .cloned()
        .collect();
    for entry in orphans {
        let Ok(state) = gateway.state(repo_of(&entry.id), entry.number) else {
            continue;
        };
        if state == PullState::Open {
            continue;
        }
        if let Some(index) = store
            .state
            .bump_behind
            .iter()
            .position(|saved| saved.id == entry.id && saved.package == entry.package)
        {
            let mut resumed = settled(store, index, state);
            if state == PullState::Closed {
                resumed.status = Status::Skipped;
            }
            steps.push(resumed);
        }
    }
    steps
}

pub struct GithubGateway<'a> {
    pub client: &'a mut Client,
    pub cfg: &'a Config,
    pub options: &'a Options,
}

impl GithubGateway<'_> {
    fn root(&self) -> Result<PathBuf> {
        self.options
            .workspace
            .clone()
            .or_else(|| crate::cache::default_dir().map(|root| root.join("bump-workspaces")))
            .context("no safe cache directory for isolated bump workspaces")
    }
}

fn cleanup(workspace: Option<&Path>) {
    if let Some(path) = workspace
        && path.join("pipetrain-owned").is_file()
    {
        let _ = std::fs::remove_dir_all(path);
    }
}

impl Gateway for GithubGateway<'_> {
    fn state(&mut self, repo: &str, number: u64) -> Result<PullState> {
        let pull = self.client.rest(&format!("/repos/{repo}/pulls/{number}"))?;
        if pull["merged"].as_bool() == Some(true) || !pull["merged_at"].is_null() {
            return Ok(PullState::Merged);
        }
        match pull["state"].as_str() {
            Some("open") => Ok(PullState::Open),
            Some("closed") => Ok(PullState::Closed),
            _ => bail!("GitHub did not report the state of {repo}#{number}"),
        }
    }

    fn update(&mut self, repo: &str, number: u64) -> Result<Update> {
        let path = format!("/repos/{repo}/pulls/{number}/update-branch");
        let reply = self.client.put_json(&path, &serde_json::json!({}))?;
        match reply.status {
            200..=299 => Ok(Update::Updated),
            422 if reply.message().to_ascii_lowercase().contains("conflict") => {
                Ok(Update::Conflict(reply.message()))
            }
            status => bail!(
                "GitHub refused to update the branch of {repo}#{number} (HTTP {status}): {}",
                reply.message()
            ),
        }
    }

    fn open(&mut self, need: &Need, to: &str) -> Result<Opening> {
        let repo = repo_of(&need.id);
        let number = number_of(&need.id).context("the PR id has no number")?;
        let pull = self.client.rest(&format!("/repos/{repo}/pulls/{number}"))?;
        if pull["state"].as_str() != Some("open") {
            return Ok(Opening::Skipped(format!("{} is not open", need.id)));
        }
        let base = pull["base"]["ref"]
            .as_str()
            .context("GitHub did not name the PR base branch")?
            .to_string();
        let head_sha = self
            .client
            .branch_tip(repo, &base)?
            .with_context(|| format!("the base branch {base} of {} does not exist", need.id))?;
        let ecosystem = Ecosystem::default_for(&need.package);
        let (tree, truncated) = self.client.tree(repo, &head_sha)?;
        if truncated {
            return Ok(Opening::Skipped(
                "the file tree is too large to inspect".into(),
            ));
        }
        let wanted: Vec<String> = tree
            .iter()
            .filter(|path| in_ecosystem(path, ecosystem))
            .cloned()
            .collect();
        let mut manifests = Vec::new();
        for chunk in wanted.chunks(BLOB_CHUNK) {
            for (path, text) in self.client.blobs(repo, &head_sha, chunk)? {
                manifests.push((repo.to_string(), path, text));
            }
        }
        let rows: Vec<(String, String)> = refs_of(&manifests, &need.package, ecosystem)
            .into_iter()
            .filter(|(_, spec)| classify(spec, to) == RowStatus::Change)
            .collect();
        if rows.is_empty() {
            return Ok(Opening::AlreadyPinned);
        }
        let bump_branch = branch_name(&need.package, to);
        if self.client.branch_tip(repo, &bump_branch)?.is_some() {
            return Ok(Opening::Skipped(format!(
                "branch {bump_branch} already exists, so an earlier run or someone else got there first"
            )));
        }
        let settings = self.cfg.bump.settings_for(
            repo,
            self.options.restore_mode,
            self.options.solution.as_deref(),
        );
        let root = self.root()?;
        let candidate = Candidate {
            repo,
            default_branch: &base,
            head_sha: &head_sha,
            tree: &tree,
            rows: &rows,
        };
        let remote = DefaultBranchRemote {
            client: &mut *self.client,
            title: pull_request_title(&need.package, to),
        };
        let mut backend = LocalBackend::new(root, ProcessRunner, remote, WORKSPACE_TIMEOUT);
        let applied = apply_repo(
            ecosystem,
            &need.package,
            to,
            &candidate,
            &settings,
            &mut backend,
        );
        let workspace = backend.workspace().map(Path::to_path_buf);
        match applied {
            Ok(Applied::Opened(opened)) => {
                cleanup(workspace.as_deref());
                let created = opened
                    .url
                    .rsplit_once('/')
                    .and_then(|(_, tail)| tail.parse::<u64>().ok())
                    .context("the bump PR URL has no number")?;
                Ok(Opening::Opened {
                    number: created,
                    url: opened.url,
                    branch: opened.branch,
                })
            }
            Ok(Applied::Skipped(reason)) => {
                cleanup(workspace.as_deref());
                Ok(Opening::Skipped(reason))
            }
            Err(error) => {
                let kept = workspace
                    .map(|path| format!("; workspace kept at {}", path.display()))
                    .unwrap_or_default();
                bail!("{error:#}{kept}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct Fake {
        states: BTreeMap<u64, PullState>,
        opened: Vec<String>,
        refuse: Option<String>,
        next: u64,
        updated: Vec<String>,
        conflict: Option<String>,
        pinned: bool,
    }

    impl Gateway for Fake {
        fn state(&mut self, _repo: &str, number: u64) -> Result<PullState> {
            self.states
                .get(&number)
                .copied()
                .context("the PR is unknown")
        }

        fn open(&mut self, need: &Need, to: &str) -> Result<Opening> {
            if let Some(reason) = &self.refuse {
                bail!("{reason}");
            }
            if self.pinned {
                return Ok(Opening::AlreadyPinned);
            }
            self.next += 1;
            let number = 100 + self.next;
            self.states.insert(number, PullState::Open);
            self.opened.push(format!("{}@{to}", need.id));
            Ok(Opening::Opened {
                number,
                url: format!("https://example.test/acme/web/pull/{number}"),
                branch: format!("pipetrain/bump/acme.core-{to}"),
            })
        }

        fn update(&mut self, repo: &str, number: u64) -> Result<Update> {
            self.updated.push(format!("{repo}#{number}"));
            Ok(match &self.conflict {
                Some(message) => Update::Conflict(message.clone()),
                None => Update::Updated,
            })
        }
    }

    fn other_need() -> Need {
        Need {
            id: "acme/web#8".into(),
            ..need(Some("1.4.0"))
        }
    }

    fn need(to: Option<&str>) -> Need {
        Need {
            id: "acme/web#7".into(),
            package: "Acme.Core".into(),
            from: "1.0.0".into(),
            to: to.map(str::to_string),
            behind: 3,
            limit: 2,
            latest: None,
            queue_published: to.is_none(),
        }
    }

    fn store() -> Store {
        Store::open(None).unwrap()
    }

    #[test]
    fn opens_once_and_waits_on_the_next_run() {
        let mut store = store();
        let mut fake = Fake::default();
        let first = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(first[0].status, Status::Done);
        assert!(first[0].detail.contains("waiting for it to be merged"));
        assert_eq!(store.state.bump_behind.len(), 1);
        let second = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(second[0].status, Status::Skipped);
        assert!(second[0].detail.contains("is open"));
        assert_eq!(fake.opened, vec!["acme/web#7@1.4.0".to_string()]);
        assert_eq!(store.state.bump_behind.len(), 1);
    }

    #[test]
    fn resumes_after_the_bump_pr_is_merged() {
        let mut store = store();
        let mut fake = Fake::default();
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        fake.states.insert(101, PullState::Merged);
        let resumed = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(resumed[0].status, Status::Done);
        assert!(resumed[0].detail.contains("was merged"));
        assert!(resumed[0].detail.contains("updated the branch"));
        assert_eq!(fake.updated, vec!["acme/web#7".to_string()]);
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn an_open_bump_pr_does_not_update_the_branch() {
        let mut store = store();
        let mut fake = Fake::default();
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert!(fake.updated.is_empty());
    }

    #[test]
    fn a_conflict_updating_the_branch_blocks_the_pr() {
        let mut store = store();
        let mut fake = Fake {
            conflict: Some("merge conflict between base and head".into()),
            ..Fake::default()
        };
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        fake.states.insert(101, PullState::Merged);
        let found = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(found[0].status, Status::Skipped);
        assert_eq!(found[0].blocked_by.as_deref(), Some("acme/web#7"));
        assert!(found[0].detail.contains("merge conflict"));
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn a_base_that_already_pins_the_version_goes_straight_to_the_update() {
        let mut store = store();
        let mut fake = Fake {
            pinned: true,
            ..Fake::default()
        };
        let found = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(found[0].status, Status::Done);
        assert_eq!(fake.updated, vec!["acme/web#7".to_string()]);
        assert!(fake.opened.is_empty());
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn two_prs_of_one_repo_share_one_bump_pr() {
        let mut store = store();
        let mut fake = Fake::default();
        let found = run(&[need(Some("1.4.0")), other_need()], &mut store, &mut fake);
        assert_eq!(fake.opened.len(), 1);
        assert_eq!(found[1].status, Status::Skipped);
        assert_eq!(store.state.bump_behind.len(), 2);
        fake.states.insert(101, PullState::Merged);
        let resumed = run(&[need(Some("1.4.0")), other_need()], &mut store, &mut fake);
        assert_eq!(resumed[0].status, Status::Done);
        assert_eq!(resumed[1].status, Status::Done);
        assert_eq!(
            fake.updated,
            vec!["acme/web#7".to_string(), "acme/web#8".to_string()]
        );
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn clears_a_merged_entry_when_the_plan_no_longer_needs_it() {
        let mut store = store();
        let mut fake = Fake::default();
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        fake.states.insert(101, PullState::Merged);
        let resumed = run(&[], &mut store, &mut fake);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].status, Status::Done);
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn keeps_waiting_for_an_open_entry_the_plan_no_longer_lists() {
        let mut store = store();
        let mut fake = Fake::default();
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        let resumed = run(&[], &mut store, &mut fake);
        assert!(resumed.is_empty());
        assert_eq!(store.state.bump_behind.len(), 1);
    }

    #[test]
    fn a_closed_bump_pr_fails_and_is_forgotten() {
        let mut store = store();
        let mut fake = Fake::default();
        run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        fake.states.insert(101, PullState::Closed);
        let found = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(found[0].status, Status::Failed);
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn a_failed_open_is_a_failed_step_and_saves_nothing() {
        let mut store = store();
        let mut fake = Fake {
            refuse: Some("restore failed".into()),
            ..Fake::default()
        };
        let found = run(&[need(Some("1.4.0"))], &mut store, &mut fake);
        assert_eq!(found[0].status, Status::Failed);
        assert!(found[0].detail.contains("restore failed"));
        assert!(store.state.bump_behind.is_empty());
    }

    #[test]
    fn a_version_published_by_the_queue_is_left_to_bump() {
        let mut store = store();
        let mut fake = Fake::default();
        let found = run(&[need(None)], &mut store, &mut fake);
        assert_eq!(found[0].status, Status::Skipped);
        assert!(found[0].detail.contains("--bump"));
        assert!(fake.opened.is_empty());
    }
}
