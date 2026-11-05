use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use super::checklist::Checklist;
use super::order::requests_line;
use super::{PlanOptions, build_plan};
use crate::bundle::Bundle;
use crate::github::{BranchWrite, Client};
use crate::live::{Row, State};
use crate::plan::Plan;
use crate::planner::Item;
use crate::style::{bold, dim, green, paint, red, yellow};

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
    fn line(&self, color: bool) -> String {
        match &self.step {
            Step::Create {
                base, branch, sha, ..
            } => format!(
                "{} {} from {} ({})",
                tint(color, yellow, "create branch"),
                tint(color, cyan, branch),
                tint(color, cyan, base),
                short(sha)
            ),
            Step::Retarget {
                branch, current, ..
            } => format!(
                "{} {}: {} -> {}",
                tint(color, yellow, "retarget"),
                tint(color, bold, &self.target),
                tint(color, cyan, current),
                tint(color, cyan, branch)
            ),
        }
    }

    fn display(&self, color: bool) -> String {
        let line = self.line(color);
        let suffix = |style: fn(&str) -> String| {
            if self.detail.is_empty() {
                String::new()
            } else {
                format!(" {}", tint(color, style, &format!("({})", self.detail)))
            }
        };
        match self.status {
            Status::Planned => line,
            Status::Done => format!("{} {line}", tint(color, green, "done:")),
            Status::Skipped => format!("{} {line}{}", tint(color, dim, "skipped:"), suffix(dim)),
            Status::Failed => format!(
                "{} {line}{}",
                tint(color, red, "failed:"),
                suffix(plain_red)
            ),
        }
    }
}

pub(crate) fn tint(color: bool, style: fn(&str) -> String, text: &str) -> String {
    if color { style(text) } else { text.to_string() }
}

fn cyan(text: &str) -> String {
    paint("36", text)
}

fn plain_red(text: &str) -> String {
    paint("31", text)
}

impl Report {
    fn has_planned(&self) -> bool {
        self.bundles
            .iter()
            .flat_map(|bundle| &bundle.actions)
            .any(|action| action.status == Status::Planned)
    }

    fn count(&self, status: Status) -> usize {
        self.bundles
            .iter()
            .flat_map(|bundle| &bundle.actions)
            .filter(|action| action.status == status)
            .count()
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

struct Retargets {
    actions: Vec<Action>,
    left_alone: Vec<String>,
}

fn retargets(plan: &Plan, bundle: &Bundle) -> Result<Retargets> {
    if bundle.branch.is_empty() || bundle.branch == bundle.base {
        bail!(
            "refusing to bundle {}: the deploy branch `{}` must be set and differ from the base `{}`",
            bundle.repo,
            bundle.branch,
            bundle.base
        );
    }
    let mut actions = Vec::new();
    let mut left_alone = Vec::new();
    for id in &bundle.retarget {
        let item = plan
            .items
            .iter()
            .find(|item| &item.id == id)
            .ok_or_else(|| anyhow!("{id} is in the bundle but not in the plan"))?;
        if item.hidden {
            left_alone.push(id.clone());
        } else {
            actions.push(retarget_action(bundle, item));
        }
    }
    Ok(Retargets {
        actions,
        left_alone,
    })
}

fn left_alone_note(ids: &[String]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    Some(format!(
        "{} {} by others left alone: {}",
        ids.len(),
        if ids.len() == 1 { "PR" } else { "PRs" },
        ids.join(", ")
    ))
}

fn reject_collisions(plan: &Plan) -> Result<()> {
    let mut seen: BTreeMap<(String, String), &Bundle> = BTreeMap::new();
    for bundle in &plan.bundles {
        let key = (bundle.repo.to_lowercase(), bundle.branch.to_lowercase());
        if let Some(other) = seen.insert(key, bundle) {
            bail!(
                "refusing to bundle {}: the bases `{}` and `{}` would both use the branch `{}`; rename one base or bundle them on separate days with --date",
                bundle.repo,
                other.base,
                bundle.base,
                bundle.branch
            );
        }
    }
    Ok(())
}

fn per_base_note(plan: &Plan, bundle: &Bundle) -> Option<String> {
    let siblings = plan
        .bundles
        .iter()
        .filter(|other| other.repo.eq_ignore_ascii_case(&bundle.repo))
        .count();
    (siblings > 1).then(|| {
        format!(
            "{siblings} bundles in this repo on different bases, so each branch is named after its base"
        )
    })
}

fn assemble(
    bundle: &Bundle,
    found: Retargets,
    client: &mut Client,
    per_base: Option<String>,
) -> Result<BundleReport> {
    let mut actions = Vec::new();
    let mut notes: Vec<String> = per_base.into_iter().collect();
    match &bundle.existing_train_pr {
        Some(pr) => {
            notes.push(format!(
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
    actions.extend(found.actions);
    notes.extend(left_alone_note(&found.left_alone));
    Ok(BundleReport {
        repo: bundle.repo.clone(),
        base: bundle.base.clone(),
        branch: bundle.branch.clone(),
        actions,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("; "))
        },
    })
}

fn prepare(plan: &Plan, client: &mut Client) -> Result<Report> {
    reject_collisions(plan)?;
    let mut drafts = Vec::new();
    for bundle in &plan.bundles {
        let found = retargets(plan, bundle)?;
        let nothing_yours = found.actions.is_empty()
            && !found.left_alone.is_empty()
            && bundle.existing_train_pr.is_none();
        if nothing_yours {
            eprintln!("skipped bundle {}: none of its PRs is yours", bundle.repo);
            continue;
        }
        drafts.push((bundle, found));
    }
    let mut bundles = Vec::new();
    for (bundle, found) in drafts {
        bundles.push(assemble(
            bundle,
            found,
            client,
            per_base_note(plan, bundle),
        )?);
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

pub type Running = Option<(usize, usize)>;

fn skip_the_rest(report: &mut Report) {
    for action in report
        .bundles
        .iter_mut()
        .flat_map(|bundle| bundle.actions.iter_mut())
        .filter(|action| action.status == Status::Planned)
    {
        action.status = Status::Skipped;
        action.detail = STOPPED.to_string();
    }
}

fn execute(
    report: &mut Report,
    client: &mut Client,
    observe: &mut dyn FnMut(&Report, Running),
) -> bool {
    observe(report, None);
    for bundle in 0..report.bundles.len() {
        for index in 0..report.bundles[bundle].actions.len() {
            if report.bundles[bundle].actions[index].status != Status::Planned {
                continue;
            }
            observe(report, Some((bundle, index)));
            let action = &mut report.bundles[bundle].actions[index];
            run_step(action, client);
            let failed = action.status == Status::Failed;
            if failed {
                skip_the_rest(report);
            }
            observe(report, None);
            if failed {
                return true;
            }
        }
    }
    false
}

pub fn apply_plan(
    plan: &Plan,
    client: &mut Client,
    apply: bool,
    confirm: &mut dyn FnMut(&str) -> bool,
    observe: &mut dyn FnMut(&Report, Running),
) -> Result<Outcome> {
    let mut report = prepare(plan, client)?;
    if !apply {
        return Ok(Outcome {
            report,
            failed: false,
            aborted: false,
        });
    }
    if report.has_planned() && !confirm(&render_actions(&report, false)) {
        return Ok(Outcome {
            report,
            failed: false,
            aborted: true,
        });
    }
    report.applied = true;
    let failed = execute(&mut report, client, observe);
    Ok(Outcome {
        report,
        failed,
        aborted: false,
    })
}

fn render_actions(report: &Report, color: bool) -> String {
    let mut out = String::new();
    for (index, bundle) in report.bundles.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}  base {}  branch {}\n",
            tint(color, bold, &bundle.repo),
            tint(color, cyan, &bundle.base),
            tint(color, cyan, &bundle.branch)
        ));
        if let Some(note) = &bundle.note {
            out.push_str(&format!("  {}\n", tint(color, dim, note)));
        }
        for action in &bundle.actions {
            out.push_str(&format!("  {}\n", action.display(color)));
        }
    }
    out
}

fn live_state(action: &Action, running: bool, took: Option<Duration>) -> State {
    match action.status {
        Status::Planned if running => State::Running,
        Status::Planned => State::Pending,
        Status::Done => State::Done(took.unwrap_or_default()),
        Status::Skipped => State::Skipped(action.detail.clone()),
        Status::Failed => State::Failed(action.detail.clone()),
    }
}

pub(super) fn live_rows(report: &Report, running: Running, took: &[Option<Duration>]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut flat = 0;
    for (bundle_index, bundle) in report.bundles.iter().enumerate() {
        rows.push(Row {
            text: format!(
                "{}  base {}  branch {}",
                bundle.repo, bundle.base, bundle.branch
            ),
            state: State::Pending,
            header: true,
        });
        for (action_index, action) in bundle.actions.iter().enumerate() {
            let is_running = running == Some((bundle_index, action_index));
            rows.push(Row {
                text: action.line(false),
                state: live_state(action, is_running, took.get(flat).copied().flatten()),
                header: false,
            });
            flat += 1;
        }
    }
    rows
}

pub(super) fn flat_index(report: &Report, running: (usize, usize)) -> usize {
    let before: usize = report
        .bundles
        .iter()
        .take(running.0)
        .map(|bundle| bundle.actions.len())
        .sum();
    before + running.1
}

pub(super) fn action_count(report: &Report) -> usize {
    report
        .bundles
        .iter()
        .map(|bundle| bundle.actions.len())
        .sum()
}

pub(super) fn statuses(report: &Report) -> Vec<Status> {
    report
        .bundles
        .iter()
        .flat_map(|bundle| bundle.actions.iter().map(|action| action.status))
        .collect()
}

pub fn use_live(
    apply: bool,
    json: bool,
    no_progress: bool,
    stderr_is_terminal: bool,
    ci: Option<&str>,
) -> bool {
    apply && crate::progress::enabled(no_progress, json, false, true, stderr_is_terminal, ci)
}

fn summary_line(report: &Report, color: bool, elapsed: Option<Duration>) -> String {
    let done = report.count(Status::Done);
    let skipped = report.count(Status::Skipped);
    let failed = report.count(Status::Failed);
    let done_text = format!("{done} done");
    let failed_text = format!("{failed} failed");
    let done_part = if done > 0 {
        tint(color, green, &done_text)
    } else {
        done_text
    };
    let failed_part = if failed > 0 {
        tint(color, red, &failed_text)
    } else {
        tint(color, dim, &failed_text)
    };
    let suffix = elapsed
        .map(|elapsed| format!(" in {}", crate::elapsed::format(elapsed)))
        .unwrap_or_default();
    format!(
        "{done_part}, {}, {failed_part}{suffix}\n",
        tint(color, dim, &format!("{skipped} skipped"))
    )
}

pub fn render_with(report: &Report, color: bool, elapsed: Option<Duration>) -> String {
    if report.bundles.is_empty() {
        return format!("{}\n", tint(color, dim, "nothing to bundle"));
    }
    let body = render_actions(report, color);
    if report.applied {
        format!("{body}\n{}", summary_line(report, color, elapsed))
    } else {
        let banner = "dry run, nothing was written; pass --apply to do it";
        format!("{}\n\n{body}", tint(color, yellow, banner))
    }
}

pub fn render(report: &Report) -> String {
    render_with(report, false, None)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Output {
    Json,
    Summary,
    Full,
}

fn output_form(json: bool, live_shown: bool, stdout_is_terminal: bool) -> Output {
    if json {
        Output::Json
    } else if live_shown && stdout_is_terminal {
        Output::Summary
    } else {
        Output::Full
    }
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
    let apply_started = Cell::new(Instant::now());
    let mut confirm = |actions: &str| {
        let answer = if yes { None } else { Some(ask(actions)) };
        apply_started.set(Instant::now());
        gate(apply, yes, interactive, answer.as_deref()) == Gate::Proceed
    };
    let live = use_live(
        opts.apply,
        json,
        !opts.plan.progress,
        std::io::stderr().is_terminal(),
        std::env::var("CI").ok().as_deref(),
    );
    let mut checklist: Option<Checklist> = None;
    let mut tried = false;
    let mut observe = |report: &Report, running: Running| {
        if !live {
            return;
        }
        if let Some(shown) = checklist.as_mut() {
            shown.update(report, running);
        } else if !tried {
            tried = true;
            if report.has_planned() {
                checklist = Checklist::start(report);
            }
        }
    };
    let outcome = apply_plan(&built, &mut client, opts.apply, &mut confirm, &mut observe)?;
    let applying = apply_started.get().elapsed();
    let shown = checklist.take().map(Checklist::finish).is_some();
    if outcome.aborted {
        eprintln!("aborted, nothing was written");
        return Ok(1);
    }
    match output_form(json, shown, std::io::stdout().is_terminal()) {
        Output::Json => println!("{}", serde_json::to_string_pretty(&outcome.report)?),
        Output::Summary => {
            anstream::print!("{}", summary_line(&outcome.report, true, Some(applying)));
        }
        Output::Full => {
            anstream::print!("{}", render_with(&outcome.report, true, Some(applying)));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create() -> Action {
        Action {
            kind: Kind::CreateBranch,
            target: "deploy/30-10-2026".to_string(),
            status: Status::Planned,
            detail: String::new(),
            step: Step::Create {
                slug: "acme/api-core".to_string(),
                base: "master".to_string(),
                branch: "deploy/30-10-2026".to_string(),
                sha: "0123456789abcdef".to_string(),
            },
        }
    }

    fn retarget(number: u64) -> Action {
        Action {
            kind: Kind::Retarget,
            target: format!("acme/api-core#{number}"),
            status: Status::Planned,
            detail: String::new(),
            step: Step::Retarget {
                slug: "acme/api-core".to_string(),
                number,
                branch: "deploy/30-10-2026".to_string(),
                current: "master".to_string(),
            },
        }
    }

    fn report(actions: Vec<Action>) -> Report {
        Report {
            schema: 1,
            applied: true,
            bundles: vec![BundleReport {
                repo: "acme/api-core".to_string(),
                base: "master".to_string(),
                branch: "deploy/30-10-2026".to_string(),
                actions,
                note: Some("left alone: acme/api-core#9".to_string()),
            }],
        }
    }

    fn three() -> Report {
        report(vec![create(), retarget(1), retarget(2)])
    }

    fn states(rows: &[Row]) -> Vec<State> {
        rows.iter().map(|row| row.state.clone()).collect()
    }

    const SECOND: Duration = Duration::from_millis(1500);

    #[test]
    fn planned_rows_are_pending_under_a_header() {
        let rows = live_rows(&three(), None, &[]);

        assert_eq!(rows.len(), 4);
        assert!(rows[0].header);
        assert_eq!(
            rows[0].text,
            "acme/api-core  base master  branch deploy/30-10-2026"
        );
        assert!(rows[1..].iter().all(|row| !row.header));
        assert_eq!(states(&rows[1..]), vec![State::Pending; 3]);
    }

    #[test]
    fn the_action_text_has_no_status_prefix_and_the_note_is_skipped() {
        let rows = live_rows(&three(), None, &[]);

        assert_eq!(
            rows[1].text,
            "create branch deploy/30-10-2026 from master (0123456)"
        );
        assert_eq!(
            rows[2].text,
            "retarget acme/api-core#1: master -> deploy/30-10-2026"
        );
        assert!(rows.iter().all(|row| !row.text.contains("left alone")));
    }

    #[test]
    fn the_running_action_is_the_only_one_marked_running() {
        let mut found = three();
        found.bundles[0].actions[0].status = Status::Done;
        let rows = live_rows(&found, Some((0, 1)), &[Some(SECOND), None, None]);

        assert_eq!(
            states(&rows[1..]),
            vec![State::Done(SECOND), State::Running, State::Pending]
        );
    }

    #[test]
    fn a_done_action_carries_its_duration() {
        let mut found = three();
        found.bundles[0].actions[0].status = Status::Done;
        found.bundles[0].actions[1].status = Status::Done;
        let rows = live_rows(&found, None, &[Some(SECOND), Some(Duration::ZERO)]);

        assert_eq!(
            states(&rows[1..]),
            vec![
                State::Done(SECOND),
                State::Done(Duration::ZERO),
                State::Pending
            ]
        );
    }

    #[test]
    fn a_done_action_without_a_measured_time_shows_zero() {
        let mut found = three();
        found.bundles[0].actions[0].status = Status::Done;
        let rows = live_rows(&found, None, &[]);

        assert_eq!(rows[1].state, State::Done(Duration::ZERO));
    }

    #[test]
    fn a_skipped_action_carries_its_reason() {
        let mut found = three();
        found.bundles[0].actions[0].status = Status::Skipped;
        found.bundles[0].actions[0].detail = "branch already exists".to_string();
        let rows = live_rows(&found, None, &[]);

        assert_eq!(
            rows[1].state,
            State::Skipped("branch already exists".to_string())
        );
    }

    #[test]
    fn a_failure_shows_the_message_and_the_rest_is_stopped() {
        let mut found = three();
        found.bundles[0].actions[0].status = Status::Done;
        found.bundles[0].actions[1].status = Status::Failed;
        found.bundles[0].actions[1].detail = "Validation Failed".to_string();
        skip_the_rest(&mut found);
        let rows = live_rows(&found, None, &[Some(SECOND)]);

        assert_eq!(
            states(&rows[1..]),
            vec![
                State::Done(SECOND),
                State::Failed("Validation Failed".to_string()),
                State::Skipped(STOPPED.to_string()),
            ]
        );
    }

    #[test]
    fn every_bundle_gets_its_own_header_and_a_running_position_is_per_bundle() {
        let mut found = three();
        let mut other = report(vec![retarget(7)]);
        other.bundles[0].repo = "acme/web".to_string();
        found.bundles.push(other.bundles.remove(0));
        let rows = live_rows(&found, Some((1, 0)), &[]);

        assert_eq!(rows.len(), 6);
        assert!(rows[4].header);
        assert!(rows[4].text.starts_with("acme/web"));
        assert_eq!(rows[5].state, State::Running);
        assert_eq!(rows[3].state, State::Pending);
        assert_eq!(flat_index(&found, (1, 0)), 3);
        assert_eq!(flat_index(&found, (0, 2)), 2);
        assert_eq!(action_count(&found), 4);
        assert_eq!(statuses(&found), vec![Status::Planned; 4]);
    }

    #[test]
    fn stdout_gets_only_the_summary_when_the_live_list_and_both_terminals_are_there() {
        assert_eq!(output_form(false, true, true), Output::Summary);
        assert_eq!(output_form(false, true, false), Output::Full);
        assert_eq!(output_form(false, false, true), Output::Full);
        assert_eq!(output_form(false, false, false), Output::Full);
        assert_eq!(output_form(true, true, true), Output::Json);
        assert_eq!(output_form(true, false, false), Output::Json);
    }

    #[test]
    fn the_live_list_needs_apply_a_terminal_and_no_opt_out() {
        let cases = [
            (true, false, false, true, None, true),
            (false, false, false, true, None, false),
            (true, true, false, true, None, false),
            (true, false, true, true, None, false),
            (true, false, false, false, None, false),
            (true, false, false, true, Some("true"), false),
            (true, false, false, true, Some("1"), false),
            (true, false, false, true, Some("false"), true),
            (true, false, false, true, Some(""), true),
        ];
        for (apply, json, no_progress, stderr_tty, ci, want) in cases {
            assert_eq!(
                use_live(apply, json, no_progress, stderr_tty, ci),
                want,
                "apply={apply} json={json} no_progress={no_progress} stderr_tty={stderr_tty} ci={ci:?}"
            );
        }
    }
}
