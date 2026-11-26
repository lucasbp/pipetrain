use anyhow::{Context, Result, ensure};

use super::{Env, behind, bump};
use crate::github::Client;
use crate::merge::{Kind, Report, Status, StepReport, Store};
use crate::model::PrKey;
use crate::planner::{Item, behind::QueuedBump};

pub(super) fn candidate_draft(item: &Item, contracts: &[QueuedBump], store: &Store) -> bool {
    let slug = format!("{}/{}", item.org, item.repo);
    behind::is_published_bump(item, &store.state)
        || contracts.iter().any(|contract| {
            if contract.base != item.base
                || !contract
                    .id
                    .split_once('#')
                    .is_some_and(|(repo, _)| repo.eq_ignore_ascii_case(&slug))
            {
                return false;
            }
            let prefix = crate::consumer_bump::apply::branch_name(&contract.package, "");
            item.head.strip_prefix(&prefix).is_some_and(|suffix| {
                let version = suffix
                    .split_once('-')
                    .map_or(suffix, |(version, _)| version);
                version.split('.').all(|part| part.parse::<u64>().is_ok())
                    && crate::planner::behind::stable_in_major(
                        &contract.from,
                        &[version.to_owned()],
                    )
                    .is_some()
            })
        })
}

pub(super) fn summary(report: &Report, command: &str) -> String {
    let mut out = String::new();
    let mut waiting = false;
    for step in report.steps.iter().filter(|step| {
        step.bump
            .as_ref()
            .is_some_and(|data| data["source"] == "queue_publication")
    }) {
        out.push_str(&format!("  {}: {}\n", step.target, step.detail));
        waiting |= step.status != Status::Done;
    }
    if waiting {
        out.push_str(&format!("  After the reported blocker clears or the bump is merged, resume with the same selection and state: {command}\n"));
    }
    out
}

pub(super) fn prepare(
    env: &Env,
    contract: &QueuedBump,
    client: &mut Client,
    store: &mut Store,
    stopped_by: Option<&str>,
) -> (StepReport, bool) {
    let mut version = None;
    let result = (|| -> Result<_> {
        if let Some(cause) = stopped_by {
            anyhow::bail!("the queue stopped before bump preparation: {cause}");
        }
        ensure!(
            env.settings.bump,
            "resume with --bump and the same merge state"
        );
        let options = &env.settings.bump_options;
        let evidence = bump::queued::publication(contract, client, options, store)?;
        version = Some(evidence.version().to_owned());
        let key = PrKey::parse(&contract.id).context("invalid consumer PR id")?;
        let pr = client.pr(&key)?.context("consumer PR is unavailable")?;
        ensure!(
            pr.key == key && pr.base_ref == contract.base && pr.head_ref == contract.head,
            "consumer branch or base changed, inspect the saved bump before continuing"
        );
        let planned_sha = env
            .plan
            .items
            .iter()
            .find(|item| item.id == contract.id)
            .map(|item| &item.sha)
            .or_else(|| {
                env.plan
                    .queued_bumps
                    .iter()
                    .find(|planned| planned.id == contract.id)
                    .map(|planned| &planned.sha)
            });
        ensure!(
            planned_sha.is_none_or(|sha| sha == &pr.head_sha),
            "consumer head changed since planning"
        );
        let blockers: Vec<_> = env
            .cfg
            .blockers(&pr)
            .into_iter()
            .filter(|reason| reason != "branch behind its base, update it")
            .collect();
        ensure!(
            blockers.is_empty(),
            "consumer is not ready: {}",
            blockers.join(", ")
        );
        if let Some(skipped) = env
            .plan
            .skipped
            .iter()
            .find(|entry| entry.id == contract.id)
        {
            let others: Vec<_> = skipped
                .reasons
                .iter()
                .filter(|reason| {
                    !reason.starts_with(crate::planner::behind::PREFIX)
                        && !reason.starts_with(crate::merge::category::DEPENDS_PREFIX)
                        && reason.as_str() != "branch behind its base, update it"
                })
                .cloned()
                .collect();
            ensure!(
                others.is_empty(),
                "consumer held by the plan: {}",
                others.join(", ")
            );
        }
        let mut gateway = behind::GithubGateway {
            client,
            cfg: env.cfg,
            options: &options.behind,
        };
        Ok(behind::published(
            contract,
            &evidence,
            &pr,
            store,
            &mut gateway,
        ))
    })();
    let (mut step, ready) = result.unwrap_or_else(|error| {
        (
            StepReport::new(
                Kind::Bump,
                &contract.id,
                Status::Stopped,
                &format!("{error:#}"),
            ),
            false,
        )
    });
    let saved = store.state.bump_behind.iter().find(|entry| {
        entry.id == contract.id
            && entry.package.eq_ignore_ascii_case(&contract.package)
            && entry
                .publication
                .as_ref()
                .is_some_and(|scope| scope.base == contract.base)
    });
    let target = version
        .clone()
        .unwrap_or_else(|| format!("version unproved from {}", contract.publishers.join(", ")));
    step.detail = format!(
        "{} {target} into {}: {}",
        contract.package, contract.base, step.detail
    );
    let bump = step
        .bump
        .get_or_insert_with(|| serde_json::json!({"outcome":"attention"}));
    bump["source"] = "queue_publication".into();
    bump["package"] = contract.package.clone().into();
    bump["version"] = version.into();
    bump["base"] = contract.base.clone().into();
    bump["ready"] = ready.into();
    bump["publishers"] = serde_json::json!(&contract.publishers);
    if let Some(entry) = saved.filter(|entry| !entry.pull_request_url.is_empty()) {
        if !step.detail.contains(&entry.pull_request_url) {
            step.detail.push_str(&format!(
                " (bump for {}: {})",
                entry.to, entry.pull_request_url
            ));
        }
        bump["pull_request_url"] = entry.pull_request_url.clone().into();
    }
    (step, ready)
}
