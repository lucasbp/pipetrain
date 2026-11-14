use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};

use crate::config::Config;
use crate::discovery::Discovered;
use crate::github::Client;
use crate::merge::{AdminOverride, Kind, StepReport};
use crate::model::Pr;

pub(super) fn notice(steps: &[StepReport]) -> String {
    let mut by_login: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for step in steps.iter().filter(|step| step.kind == Kind::Merge) {
        if let Some(admin) = &step.admin_override {
            by_login
                .entry(&admin.requested_by)
                .or_default()
                .push(&step.target);
        }
    }
    let mut out = String::new();
    for (login, ids) in by_login {
        out.push_str(&format!(
            "--admin: these PRs will be merged past a missing review as {login}:\n"
        ));
        for id in ids {
            out.push_str(&format!("  {id}\n"));
        }
    }
    out
}

pub(super) fn verify(
    client: &mut Client,
    cfg: &Config,
    pr: &Pr,
    method: &str,
    login: &str,
) -> Result<&'static str> {
    let slug = pr.key.slug();
    let base = &pr.base_ref;
    if !client.repo_admin_permission(&slug)? {
        bail!("--admin needs admin permission on {slug}; {login} does not have it");
    }
    if !client.viewer_can_merge_as_admin(&pr.key)? {
        bail!(
            "GitHub does not let {login} bypass the protection of {slug}@{base} (administrators are included in the rule or not in the ruleset bypass list)"
        );
    }
    let requirements = client.branch_requirements(&slug, base)?;
    let mut names = requirements.unverifiable;
    if requirements.linear_history && method == "merge" {
        names.push("required_linear_history".to_string());
    }
    if requirements
        .allowed_merge_methods
        .iter()
        .any(|methods| !methods.iter().any(|allowed| allowed == method))
    {
        names.push(format!(
            "allowed_merge_methods (configured method: {method})"
        ));
    }
    names.sort();
    names.dedup();
    if !names.is_empty() {
        bail!(
            "--admin only bypasses a missing review; {slug}@{base} also requires {}, which pipetrain cannot verify",
            names.join(", ")
        );
    }
    let missing: Vec<_> = requirements
        .required_checks
        .iter()
        .filter(|name| {
            let mut matching = pr
                .check_details
                .iter()
                .filter(|check| &check.name == *name)
                .peekable();
            matching.peek().is_none()
                || matching.any(|check| {
                    !matches!(
                        check.state.to_ascii_uppercase().as_str(),
                        "SUCCESS" | "NEUTRAL" | "SKIPPED"
                    )
                })
        })
        .cloned()
        .collect();
    if !missing.is_empty() {
        bail!(
            "--admin requires passing required_status_checks; {} has missing or non-passing required checks: {}",
            pr.key,
            missing.join(", ")
        );
    }
    if requirements.strict && client.behind_by(&slug, base, &pr.head_sha)? != 0 {
        bail!(
            "{} is behind {base} and the branch requires it to be up to date",
            pr.key
        );
    }
    pr.admin_override_reason(cfg.allows_missing_checks(&slug), cfg.require_approval)
        .ok_or_else(|| {
            anyhow!(
                "no longer ready for admin review override: {}",
                cfg.blockers(pr).join(", ")
            )
        })
}

pub(super) fn prepare(
    client: &mut Client,
    cfg: &Config,
    discovered: &mut Discovered,
    method: &str,
) -> Result<BTreeMap<String, AdminOverride>> {
    let eligible = |pr: &Pr| {
        pr.admin_override_reason(
            cfg.allows_missing_checks(&pr.key.slug()),
            cfg.require_approval,
        )
    };
    if !discovered
        .skipped
        .iter()
        .any(|skipped| eligible(&skipped.pr).is_some())
    {
        return Ok(BTreeMap::new());
    }
    let login = client.viewer_login()?;
    let mut overrides = BTreeMap::new();
    let mut kept = Vec::new();
    for mut skipped in std::mem::take(&mut discovered.skipped) {
        if eligible(&skipped.pr).is_none() {
            kept.push(skipped);
            continue;
        }
        match verify(client, cfg, &skipped.pr, method, &login) {
            Err(refusal) => {
                skipped.reasons.push(format!("{refusal:#}"));
                kept.push(skipped);
            }
            Ok(reason) => {
                overrides.insert(
                    skipped.pr.key.to_string(),
                    AdminOverride {
                        reason: reason.to_string(),
                        requested_by: login.clone(),
                        at: None,
                    },
                );
                discovered.ready.push((skipped.pr, skipped.scope));
            }
        }
    }
    discovered.skipped = kept;
    Ok(overrides)
}
