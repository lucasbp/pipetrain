use super::{ExactFeed, PublisherExecution, PublisherRun};
use crate::command::feed::{Check, Checker};
use crate::github::Client;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration};

pub struct NugetFeed {
    pub org: String,
    pub registry: Option<String>,
}

impl ExactFeed for NugetFeed {
    fn contains(&mut self, package: &str, version: &str) -> Result<bool> {
        match Checker::with_registry(self.registry.clone()).check(package, version, Some(&self.org))
        {
            Check::Published => Ok(true),
            Check::Absent => Ok(false),
            Check::Failed(reason) => bail!("{reason}"),
        }
    }
}

pub fn collect_run(
    client: &mut Client,
    execution: &PublisherExecution,
    push_step: &str,
    logs_api_base: &str,
) -> Result<PublisherRun> {
    let root = format!(
        "/repos/{}/actions/runs/{}",
        execution.repository, execution.run_id
    );
    let run = client.rest(&root)?;
    let jobs = client.rest(&format!("{root}/jobs?per_page=100&filter=latest"))?;
    if jobs["total_count"].as_u64().unwrap_or(u64::MAX) > 100 {
        bail!("publisher job list is truncated");
    }
    let mut candidates = Vec::new();
    for job in jobs["jobs"].as_array().context("missing publisher jobs")? {
        for step in job["steps"].as_array().context("missing publisher steps")? {
            if step["name"] == push_step
                && step["conclusion"] == "success"
                && job["conclusion"] == "success"
            {
                candidates.push((job, step));
            }
        }
    }
    let mut log = String::new();
    if candidates.len() > 1 {
        bail!("multiple publisher push steps match; select an unambiguous publisher workflow");
    }
    if let Some((job, step)) = candidates.first() {
        let id = job["id"].as_u64().context("missing publisher job id")?;
        let start = DateTime::parse_from_rfc3339(
            step["started_at"].as_str().context("missing step start")?,
        )?;
        let end = DateTime::parse_from_rfc3339(
            step["completed_at"].as_str().context("missing step end")?,
        )? + Duration::seconds(1);
        let url = format!(
            "{}/repos/{}/actions/jobs/{id}/logs",
            logs_api_base.trim_end_matches('/'),
            execution.repository
        );
        let bytes = client
            .download(&url, 8 * 1024 * 1024)
            .map_err(|_| anyhow::anyhow!("cannot read publisher job logs"))?;
        let all = String::from_utf8(bytes).context("publisher logs are not UTF-8")?;
        for line in all.lines() {
            let Some(timestamp) = line
                .split_whitespace()
                .next()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            else {
                continue;
            };
            if timestamp >= start && timestamp < end {
                log.push_str(line);
                log.push('\n');
            }
        }
    }
    Ok(PublisherRun {
        repository: run["repository"]["full_name"]
            .as_str()
            .context("missing publisher repository")?
            .into(),
        head_sha: run["head_sha"]
            .as_str()
            .context("missing publisher SHA")?
            .into(),
        id: run["id"].as_u64().context("missing workflow run id")?,
        succeeded: run["status"] == "completed" && run["conclusion"] == "success",
        push_step_succeeded: candidates.len() == 1,
        push_step_log: log,
    })
}
