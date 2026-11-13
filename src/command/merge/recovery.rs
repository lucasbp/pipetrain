use super::*;
use crate::merge::{AuditAction, AuditEntry, ExpectedRun, Resolution};

fn later(
    env: &Env,
    pending: &Pending,
    client: &mut Client,
    hooks: &mut Hooks,
) -> Result<Resolution> {
    let old = pending
        .sha
        .as_deref()
        .ok_or_else(|| anyhow!("saved SHA unknown"))?;
    let tip = client.branch_sha(&pending.repo, &pending.base)?;
    if tip == old {
        bail!(
            "base {} is still at SHA {old}; no later commit exists; repeating unchanged will not help",
            pending.base
        );
    }
    if !client.contains_commit(&pending.repo, &tip, old)? {
        bail!(
            "base {} at SHA {tip} does not contain saved SHA {old}; pending deploy retained",
            pending.base
        );
    }
    let prediction = pending
        .deploy_prediction
        .as_ref()
        .filter(|reading| matches!(reading, PushRuns::Expected { .. }));
    let mut settled = None;
    for poll in 0..2 {
        let (runs, complete) = client.workflow_runs(&pending.repo, &tip, prediction.is_none())?;
        let verdict = prediction.map_or_else(
            || judge(&runs),
            |prediction| judge_expected(&runs, prediction),
        );
        let evidence: Vec<ExpectedRun> = runs
            .iter()
            .filter(|run| match prediction {
                Some(PushRuns::Expected {
                    workflows,
                    chained_workflows,
                    ..
                }) => workflows.contains(&run.path) || chained_workflows.contains(&run.path),
                _ => true,
            })
            .map(ExpectedRun::from)
            .collect();
        let urls = evidence
            .iter()
            .map(|run| run.html_url.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        (hooks.progress)(&format!(
            "checking later deploy of {}@{} at SHA {tip}: {urls}",
            pending.repo, pending.base
        ));
        let Verdict::Passed(count) = verdict else {
            bail!("later SHA {tip} has not passed its expected deploy ({verdict:?}); runs: {urls}");
        };
        if !complete || evidence.is_empty() || evidence.iter().any(|run| run.html_url.is_empty()) {
            bail!("later SHA {tip} lacks complete workflow run evidence; runs: {urls}");
        }
        if settled == Some(count) {
            return Ok(Resolution {
                status: AuditAction::ResolvedByLaterDeploy,
                sha: tip,
                runs: evidence,
            });
        }
        settled = Some(count);
        if poll == 0 {
            (hooks.sleep)(Duration::from_secs(env.cfg.merge.poll_seconds.max(1)));
        }
    }
    bail!("later deploy did not settle")
}

pub(super) fn resolve(
    env: &Env,
    pending: &Pending,
    client: &mut Client,
    store: &mut Store,
    hooks: &mut Hooks,
    report: &mut StepReport,
) {
    let resolution = match later(env, pending, client, hooks) {
        Ok(resolution) => resolution,
        Err(error) => {
            report
                .detail
                .push_str(&format!("; later deploy check: {error:#}"));
            return;
        }
    };
    let urls = resolution
        .runs
        .iter()
        .map(|run| run.html_url.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let detail = format!(
        "resolved by a later deploy of {} into {}: {urls}",
        short(&resolution.sha),
        pending.base
    );
    let mut entry = AuditEntry::new(pending, AuditAction::ResolvedByLaterDeploy, detail.clone());
    entry.resolution = Some(resolution.clone());
    if let Err(error) = store.archive(entry) {
        stop(
            report,
            format!("cannot audit later deploy resolution: {error:#}"),
        );
        return;
    }
    report.status = Status::Done;
    report.detail = detail;
    report.deploy = Some(format!(
        "saved SHA {} failed; later SHA {} passed: {urls}",
        pending.sha.as_deref().unwrap_or("unknown"),
        resolution.sha
    ));
    report.resolution = Some(resolution);
}
