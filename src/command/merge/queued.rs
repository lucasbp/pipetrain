use anyhow::{Context, Result, ensure};

use super::{Env, behind, bump};
use crate::github::Client;
use crate::merge::{BumpWait, Kind, Report, Status, StepReport, Store};
use crate::model::{Checks, Pr, PrKey, PrState, Review};
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

pub(super) fn readiness_after_bump(pr: &Pr, check_head: Option<&str>) -> Result<Option<String>> {
    ensure!(pr.state == PrState::Open, "consumer PR is no longer open");
    ensure!(!pr.is_draft, "consumer PR is a draft");
    ensure!(
        !pr.conflicting && pr.merge_state_status != "DIRTY",
        "consumer has a merge conflict"
    );
    ensure!(
        !pr.behind && pr.merge_state_status != "BEHIND",
        "consumer is behind its base after incorporation"
    );
    ensure!(
        pr.review != Review::ChangesRequested,
        "consumer has changes requested"
    );
    let current = check_head == Some(pr.head_sha.as_str());
    ensure!(
        !current || pr.checks != Checks::Failure,
        "consumer checks failing on {}",
        pr.head_sha
    );
    let mut waiting = Vec::new();
    if !current || pr.checks != Checks::Success {
        waiting.push("green checks");
    }
    if pr.review != Review::Approved {
        waiting.push("approval");
    }
    match pr.merge_state_status.as_str() {
        "BLOCKED" | "UNSTABLE" if !waiting.is_empty() => {}
        "CLEAN" | "HAS_HOOKS" => {}
        "UNKNOWN" | "" => waiting.push("GitHub mergeability"),
        other => anyhow::bail!("GitHub blocks the consumer ({other})"),
    }
    if pr.mergeable_unknown && !waiting.contains(&"GitHub mergeability") {
        waiting.push("GitHub mergeability");
    }
    Ok((!waiting.is_empty()).then(|| {
        format!(
            "waiting for {} on consumer head {}",
            waiting.join(" and "),
            pr.head_sha
        )
    }))
}

pub(super) fn pending_signal(reason: &str) -> bool {
    matches!(
        reason,
        "checks pending" | "no checks reported" | "not approved" | "review required"
    )
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
        let (pr, check_head) = client
            .pr_with_check_head(&key)?
            .context("consumer PR is unavailable")?;
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
        let updating = behind::requested_update(store, contract).is_some();
        let new_head = pr.head_sha != contract.sha;
        ensure!(
            !new_head || updating,
            "consumer head changed outside the requested base update"
        );
        let stale_plan = planned_sha.is_some_and(|sha| sha != &pr.head_sha);
        ensure!(
            !stale_plan || (updating && new_head),
            "consumer head changed since planning"
        );
        let blockers: Vec<_> = env
            .cfg
            .blockers(&pr)
            .into_iter()
            .filter(|reason| reason != "branch behind its base, update it")
            .collect();
        if new_head {
            readiness_after_bump(&pr, check_head.as_deref())?;
        } else {
            ensure!(
                blockers.is_empty(),
                "consumer is not ready: {}",
                blockers.join(", ")
            );
        }
        let mut replan_readiness = false;
        if let Some(skipped) = env
            .plan
            .skipped
            .iter()
            .find(|entry| entry.id == contract.id)
        {
            replan_readiness =
                new_head && skipped.reasons.iter().any(|reason| pending_signal(reason));
            let others: Vec<_> = skipped
                .reasons
                .iter()
                .filter(|reason| {
                    !reason.starts_with(crate::planner::behind::PREFIX)
                        && !reason.starts_with(crate::merge::category::DEPENDS_PREFIX)
                        && reason.as_str() != "branch behind its base, update it"
                        && !(new_head && blockers.contains(reason))
                        && !(replan_readiness
                            && (pending_signal(reason)
                                || matches!(
                                    reason.as_str(),
                                    "branch protection blocks merge (BLOCKED)"
                                        | "checks are not passing (UNSTABLE)"
                                )))
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
        let (mut step, incorporated) =
            behind::published(contract, &evidence, &pr, store, &mut gateway);
        if !incorporated {
            return Ok((step, false));
        }
        let readiness = readiness_after_bump(&pr, check_head.as_deref())?.or_else(|| {
            replan_readiness.then(|| {
                "consumer approval and checks cleared since planning; replan the verified head"
                    .into()
            })
        });
        let pending = if let Some(reason) = readiness {
            Some((
                BumpWait::Readiness {
                    key,
                    sha: pr.head_sha.clone(),
                },
                reason,
            ))
        } else {
            planned_sha.filter(|_| stale_plan).map(|old| {
                (
                    BumpWait::Update {
                        key,
                        sha: old.clone(),
                    },
                    "replan for the verified updated consumer head".into(),
                )
            })
        };
        if let Some((wait, reason)) = pending {
            step.status = Status::Stopped;
            step.detail = reason;
            step.wait = Some(wait);
            if let Some(data) = &mut step.bump {
                data["outcome"] = "attention".into();
            }
        }
        let ready = step.status == Status::Done;
        Ok((step, ready))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incorporated_consumers_distinguish_pending_signals_from_terminal_blockers() {
        let mut pr = crate::github::parse_pr(&serde_json::json!({
            "number":2, "repository":{"name":"app","owner":{"login":"acme"}},
            "state":"OPEN", "headRefOid":"updated", "reviewDecision":"APPROVED",
            "mergeStateStatus":"CLEAN", "mergeable":"MERGEABLE",
            "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"SUCCESS"}}}]}
        }))
        .unwrap();
        let current = Some("updated");
        assert!(readiness_after_bump(&pr, Some("old")).unwrap().is_some());
        assert!(readiness_after_bump(&pr, current).unwrap().is_none());
        pr.review = Review::ReviewRequired;
        assert!(readiness_after_bump(&pr, current).unwrap().is_some());
        pr.checks = Checks::Failure;
        assert!(readiness_after_bump(&pr, current).is_err());
        pr.checks = Checks::Missing;
        pr.review = Review::ChangesRequested;
        assert!(readiness_after_bump(&pr, current).is_err());
    }
}
