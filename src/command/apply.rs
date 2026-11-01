use std::io::{BufRead, IsTerminal, Write};
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use super::order::requests_line;
use super::{PlanOptions, build_plan};
use crate::bundle::Bundle;
use crate::github::{BranchWrite, Client};
use crate::plan::Plan;
use crate::planner::Item;

const STOPPED: &str = "stopped after an earlier failure";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    CreateBranch,
    Retarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Planned,
    Done,
    Skipped,
    Failed,
}

#[derive(Debug)]
enum Step {
    Create {
        slug: String,
        base: String,
        branch: String,
        sha: String,
    },
    Retarget {
        slug: String,
        number: u64,
        branch: String,
        current: String,
    },
}

#[derive(Debug, Serialize)]
pub struct Action {
    pub kind: Kind,
    pub target: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip)]
    step: Step,
}

#[derive(Debug, Serialize)]
pub struct BundleReport {
    pub repo: String,
    pub base: String,
    pub branch: String,
    pub actions: Vec<Action>,
    #[serde(skip)]
    note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub applied: bool,
    pub bundles: Vec<BundleReport>,
}

#[derive(Debug)]
pub struct Outcome {
    pub report: Report,
    pub failed: bool,
    pub aborted: bool,
}

pub struct ApplyOptions {
    pub plan: PlanOptions,
    pub apply: bool,
    pub yes: bool,
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

impl Action {
    fn line(&self) -> String {
        match &self.step {
            Step::Create {
                base, branch, sha, ..
            } => format!("create branch {branch} from {base} ({})", short(sha)),
            Step::Retarget {
                branch, current, ..
            } => format!("retarget {}: {current} -> {branch}", self.target),
        }
    }

    fn display(&self) -> String {
        let line = self.line();
        let suffix = if self.detail.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.detail)
        };
        match self.status {
            Status::Planned => line,
            Status::Done => format!("done: {line}"),
            Status::Skipped => format!("skipped: {line}{suffix}"),
            Status::Failed => format!("failed: {line}{suffix}"),
        }
    }
}

impl Report {
    fn has_planned(&self) -> bool {
        self.bundles
            .iter()
            .flat_map(|bundle| &bundle.actions)
            .any(|action| action.status == Status::Planned)
    }
}

fn retarget_action(bundle: &Bundle, item: &Item) -> Action {
    let (status, detail) = if item.base == bundle.branch {
        (
            Status::Skipped,
            format!("already based on {}", bundle.branch),
        )
    } else {
        (Status::Planned, String::new())
    };
    Action {
        kind: Kind::Retarget,
        target: item.id.clone(),
        status,
        detail,
        step: Step::Retarget {
            slug: bundle.repo.clone(),
            number: item.number,
            branch: bundle.branch.clone(),
            current: item.base.clone(),
        },
    }
}

fn retargets(plan: &Plan, bundle: &Bundle) -> Result<Vec<Action>> {
    if bundle.branch.is_empty() || bundle.branch == bundle.base {
        bail!(
            "refusing to bundle {}: the deploy branch `{}` must be set and differ from the base `{}`",
            bundle.repo,
            bundle.branch,
            bundle.base
        );
    }
    bundle
        .retarget
        .iter()
        .map(|id| {
            let item = plan
                .items
                .iter()
                .find(|item| &item.id == id)
                .ok_or_else(|| anyhow!("{id} is in the bundle but not in the plan"))?;
            Ok(retarget_action(bundle, item))
        })
        .collect()
}

fn assemble(bundle: &Bundle, retargets: Vec<Action>, client: &mut Client) -> Result<BundleReport> {
    let mut actions = Vec::new();
    let mut note = None;
    match &bundle.existing_train_pr {
        Some(pr) => {
            note = Some(format!(
                "branch exists: {} is the head of {pr}",
                bundle.branch
            ));
        }
        None => {
            let sha = client.branch_sha(&bundle.repo, &bundle.base)?;
            actions.push(Action {
                kind: Kind::CreateBranch,
                target: bundle.branch.clone(),
                status: Status::Planned,
                detail: String::new(),
                step: Step::Create {
                    slug: bundle.repo.clone(),
                    base: bundle.base.clone(),
                    branch: bundle.branch.clone(),
                    sha,
                },
            });
        }
    }
    actions.extend(retargets);
    Ok(BundleReport {
        repo: bundle.repo.clone(),
        base: bundle.base.clone(),
        branch: bundle.branch.clone(),
        actions,
        note,
    })
}

fn prepare(plan: &Plan, client: &mut Client) -> Result<Report> {
    let mut drafts = Vec::new();
    for bundle in &plan.bundles {
        drafts.push((bundle, retargets(plan, bundle)?));
    }
    let mut bundles = Vec::new();
    for (bundle, actions) in drafts {
        bundles.push(assemble(bundle, actions, client)?);
    }
    Ok(Report {
        schema: 1,
        applied: false,
        bundles,
    })
}

fn run_step(action: &mut Action, client: &mut Client) {
    let result = match &action.step {
        Step::Create {
            slug, branch, sha, ..
        } => client
            .create_branch(slug, branch, sha)
            .map(|written| match written {
                BranchWrite::Created => (Status::Done, String::new()),
                BranchWrite::AlreadyExists => (Status::Skipped, "branch already exists".into()),
            }),
        Step::Retarget {
            slug,
            number,
            branch,
            ..
        } => client
            .retarget_pr(slug, *number, branch)
            .map(|()| (Status::Done, String::new())),
    };
    match result {
        Ok((status, detail)) => {
            action.status = status;
            action.detail = detail;
        }
        Err(error) => {
            action.status = Status::Failed;
            action.detail = error.to_string();
        }
    }
}

fn execute(report: &mut Report, client: &mut Client) -> bool {
    let mut failed = false;
    for action in report
        .bundles
        .iter_mut()
        .flat_map(|bundle| bundle.actions.iter_mut())
    {
        if action.status != Status::Planned {
            continue;
        }
        if failed {
            action.status = Status::Skipped;
            action.detail = STOPPED.to_string();
            continue;
        }
        run_step(action, client);
        failed = action.status == Status::Failed;
    }
    failed
}

pub fn apply_plan(
    plan: &Plan,
    client: &mut Client,
    apply: bool,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<Outcome> {
    let mut report = prepare(plan, client)?;
    if !apply {
        return Ok(Outcome {
            report,
            failed: false,
            aborted: false,
        });
    }
    if report.has_planned() && !confirm(&render_actions(&report)) {
        return Ok(Outcome {
            report,
            failed: false,
            aborted: true,
        });
    }
    report.applied = true;
    let failed = execute(&mut report, client);
    Ok(Outcome {
        report,
        failed,
        aborted: false,
    })
}

fn render_actions(report: &Report) -> String {
    let mut out = String::new();
    for (index, bundle) in report.bundles.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}  base {}  branch {}\n",
            bundle.repo, bundle.base, bundle.branch
        ));
        if let Some(note) = &bundle.note {
            out.push_str(&format!("  {note}\n"));
        }
        for action in &bundle.actions {
            out.push_str(&format!("  {}\n", action.display()));
        }
    }
    out
}

pub fn render(report: &Report) -> String {
    if report.bundles.is_empty() {
        return "nothing to bundle\n".to_string();
    }
    let body = render_actions(report);
    if report.applied {
        body
    } else {
        format!("dry run, nothing was written; pass --apply to do it\n\n{body}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    DryRun,
    Proceed,
    Ask,
    Refuse,
    Abort,
}

pub fn gate(apply: bool, yes: bool, stdin_is_terminal: bool, answer: Option<&str>) -> Gate {
    if !apply {
        return Gate::DryRun;
    }
    if yes {
        return Gate::Proceed;
    }
    if !stdin_is_terminal {
        return Gate::Refuse;
    }
    match answer {
        None => Gate::Ask,
        Some(text) if text.trim() == "yes" => Gate::Proceed,
        Some(_) => Gate::Abort,
    }
}

fn ask(actions: &str) -> String {
    eprintln!("{actions}");
    eprint!("Apply these changes to GitHub? Type yes: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}

pub fn run(opts: ApplyOptions) -> Result<i32> {
    let interactive = std::io::stdin().is_terminal();
    if gate(opts.apply, opts.yes, interactive, None) == Gate::Refuse {
        bail!("refusing to write without a terminal: pass --yes");
    }
    let started = Instant::now();
    let json = opts.plan.json;
    let (built, mut client) = build_plan(&opts.plan)?;
    let (apply, yes) = (opts.apply, opts.yes);
    let mut confirm = |actions: &str| {
        let answer = if yes { None } else { Some(ask(actions)) };
        gate(apply, yes, interactive, answer.as_deref()) == Gate::Proceed
    };
    let outcome = apply_plan(&built, &mut client, opts.apply, &mut confirm)?;
    if outcome.aborted {
        eprintln!("aborted, nothing was written");
        return Ok(1);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome.report)?);
    } else {
        anstream::print!("{}", render(&outcome.report));
    }
    if opts.apply {
        eprintln!(
            "{}",
            requests_line(client.requests(), client.cache_hits(), started.elapsed())
        );
        if !json {
            eprintln!("now run: pipetrain plan");
        }
    }
    Ok(i32::from(outcome.failed))
}
