use crate::github::Run;

const GOOD_CONCLUSIONS: [&str; 3] = ["success", "skipped", "neutral"];

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
}
