use super::*;
use crate::command::feed::{self, FeedOptions};
use crate::feed::{WaitEnd, WaitPlan};
use crate::planner::Rule;

fn options(env: &Env, target: &str) -> Option<FeedOptions> {
    let (named, rest) = target.strip_prefix("package ")?.split_once(" from ")?;
    let (_, consumer) = rest.split_once(" must be published before ")?;
    let consumer = consumer.split_whitespace().next()?;
    let item = env.plan.items.iter().find(|item| item.id == consumer)?;
    let dep = item.after.iter().find(|dep| {
        dep.rule == Rule::Package
            && dep.package.as_deref().is_some_and(|package| {
                dep.version
                    .as_deref()
                    .is_some_and(|version| named == format!("{package} {version}"))
            })
    })?;
    let package = dep.package.clone()?;
    let producers = item
        .after
        .iter()
        .filter(|other| {
            other.rule == Rule::Package
                && other.package == dep.package
                && other.version == dep.version
        })
        .filter_map(|other| env.plan.items.iter().find(|item| item.id == other.id))
        .map(|item| item.org.clone())
        .collect::<Vec<_>>();
    Some(FeedOptions {
        org: feed::publishing_org(&package, &producers),
        package,
        version: dep.version.clone(),
        ecosystem: None,
        registry: env.settings.feed_registry.clone(),
        json: false,
    })
}

pub(super) fn check(env: &Env, hooks: &mut Hooks, report: &mut StepReport) -> bool {
    let Some(opts) = options(env, &report.target) else {
        return false;
    };
    let package = &opts.package;
    let version = opts.version.as_deref().unwrap_or_default();
    (hooks.progress)(&format!("checking feed for {package} {version}"));
    let started = Instant::now();
    let waited = feed::wait_for(
        &opts,
        WaitPlan {
            interval: Duration::from_secs(env.cfg.merge.package_poll_seconds.max(1)),
            timeout: Duration::from_secs(env.cfg.merge.package_wait_seconds),
        },
        |attempts, delay, error| {
            let reason = error.unwrap_or("not published yet");
            (hooks.progress)(&format!(
                "{package} {version}: {reason} (attempt {attempts}); checking again in {}",
                crate::elapsed::format(delay)
            ));
            (hooks.sleep)(delay);
        },
        || started.elapsed(),
    );
    match waited.map(|waited| waited.end) {
        Ok(WaitEnd::Published(published)) => {
            report.status = Status::Skipped;
            report.detail = format!("{package} {version}: published in {}", published.feed);
        }
        Ok(WaitEnd::Refused { had_token }) => stop(
            report,
            format!(
                "{package} {version}: {}",
                crate::feed::refused_message(had_token)
            ),
        ),
        Ok(WaitEnd::TimedOut {
            error: Some(error), ..
        }) => {
            stop(
                report,
                format!("{package} {version}: feed check failed: {error}"),
            );
        }
        Err(error) => stop(report, format!("{package} {version}: {error}")),
        Ok(WaitEnd::TimedOut { error: None, .. }) => return false,
    }
    true
}
