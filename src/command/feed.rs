use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::feed::registry::{self, Fetched, Session, short_reason};
pub use crate::feed::registry::{Check, Checker};
use crate::feed::{self, Attempt, Ecosystem, Report, WaitEnd, WaitPlan, Waited};
use crate::plan::Plan;
use crate::planner::{Dependency, Rule};
use crate::progress;

mod sources;
pub(super) use sources::Restrictions;

pub struct FeedOptions {
    pub package: String,
    pub version: Option<String>,
    pub ecosystem: Option<Ecosystem>,
    pub org: Option<String>,
    pub registry: Option<String>,
    pub json: bool,
}

fn fetch(session: &Session, ecosystem: Ecosystem, opts: &FeedOptions) -> Result<Fetched> {
    registry::fetch(
        session,
        ecosystem,
        &opts.package,
        opts.version.as_deref(),
        opts.org.as_deref(),
        opts.registry.as_deref(),
    )
}

pub(crate) fn publishing_org(package: &str, producers: &[String]) -> Option<String> {
    registry_org(Ecosystem::default_for(package), package, producers)
}

pub(crate) fn registry_org(
    ecosystem: Ecosystem,
    package: &str,
    producers: &[String],
) -> Option<String> {
    match ecosystem {
        Ecosystem::Npm => {
            let scope = package
                .trim_start_matches('@')
                .split('/')
                .next()
                .unwrap_or_default();
            producers
                .iter()
                .find(|org| org.eq_ignore_ascii_case(scope))
                .cloned()
        }
        Ecosystem::Nuget => producers.first().cloned(),
        _ => None,
    }
}

fn is_checkable(dep: &Dependency) -> bool {
    dep.rule == Rule::Package && dep.package.is_some() && dep.version.is_some()
}

pub fn annotate(plan: &mut Plan, checker: &mut Checker) -> usize {
    let orgs: BTreeMap<String, String> = plan
        .items
        .iter()
        .map(|item| (item.id.clone(), item.org.clone()))
        .collect();
    let query = |dep: &Dependency| {
        let coordinate = dep.feed_coordinate.clone()?;
        let org = registry_org(
            coordinate.ecosystem,
            &coordinate.package,
            orgs.get(&dep.id)
                .map(std::slice::from_ref)
                .unwrap_or_default(),
        );
        Some((coordinate, org))
    };
    let wanted: BTreeSet<_> = plan
        .items
        .iter()
        .flat_map(|item| &item.after)
        .filter(|dep| is_checkable(dep))
        .filter_map(query)
        .collect();
    let total = wanted.len();
    let mut results = BTreeMap::new();
    if total > 0 {
        progress::phase("checking feeds");
    }
    for (done, (coordinate, org)) in wanted.into_iter().enumerate() {
        let outcome = checker.check_coordinate(&coordinate, org.as_deref());
        results.insert((coordinate, org), outcome);
        progress::count(done + 1, total);
    }
    for dep in plan
        .items
        .iter_mut()
        .flat_map(|item| item.after.iter_mut())
        .filter(|dep| is_checkable(dep))
    {
        match query(dep).and_then(|key| results.get(&key)) {
            Some(Check::Published) => dep.published = Some(true),
            Some(Check::Absent) => dep.published = Some(false),
            Some(Check::Failed(reason)) => dep.feed_error = Some(reason.clone()),
            None => {
                dep.feed_error = Some(dep.feed_restriction.clone().unwrap_or_else(|| {
                    "cannot identify one registry source for this dependency".into()
                }))
            }
        }
    }
    total
}

fn lookup(
    opts: &FeedOptions,
    notes: &mut Vec<String>,
    selecting_target: bool,
) -> Result<Result<Report, i32>> {
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    let session = Session::new();
    let fetched = fetch(&session, ecosystem, opts)?;
    let fetched = if selecting_target && ecosystem == Ecosystem::Go {
        registry::exclude_go_retractions(&session, &opts.package, fetched)?
    } else {
        fetched
    };
    match fetched {
        Fetched::Refused { had_token } => {
            notes.push(refusal(ecosystem, had_token));
            Ok(Err(feed::EXIT_REFUSED))
        }
        Fetched::NotFound { feed } => {
            notes.push(feed::not_found_message(&feed));
            Ok(Err(feed::EXIT_NOT_PUBLISHED))
        }
        Fetched::Found { feed, versions } => Ok(Ok(Report::build(
            &opts.package,
            ecosystem,
            &feed,
            opts.version.as_deref(),
            &versions,
        ))),
    }
}

pub fn versions(opts: &FeedOptions, notes: &mut Vec<String>) -> Result<Result<Vec<String>, i32>> {
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    Ok(match fetch(&Session::new(), ecosystem, opts)? {
        Fetched::Refused { had_token } => {
            notes.push(refusal(ecosystem, had_token));
            Err(feed::EXIT_REFUSED)
        }
        Fetched::NotFound { feed } => {
            notes.push(feed::not_found_message(&feed));
            Err(feed::EXIT_NOT_PUBLISHED)
        }
        Fetched::Found { versions, .. } => Ok(versions),
    })
}

pub fn latest(opts: &FeedOptions, notes: &mut Vec<String>) -> Result<Result<String, i32>> {
    Ok(match lookup(opts, notes, false)? {
        Ok(report) => match report.latest {
            Some(latest) => Ok(latest),
            None => {
                notes.push(feed::not_found_message(&report.feed));
                Err(feed::EXIT_NOT_PUBLISHED)
            }
        },
        Err(code) => Err(code),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub version: String,
    pub ignored_prerelease: Option<String>,
}

pub fn choose_target(report: &feed::Report, prerelease: bool) -> Result<Target, String> {
    let latest = report
        .latest
        .clone()
        .ok_or_else(|| feed::not_found_message(&report.feed))?;
    if prerelease {
        return Ok(Target {
            version: latest,
            ignored_prerelease: None,
        });
    }
    match &report.latest_stable {
        Some(stable) => Ok(Target {
            ignored_prerelease: (*stable != latest).then_some(latest),
            version: stable.clone(),
        }),
        None => Err(format!(
            "{} has no stable version in the feed (latest is {latest}); pass --prerelease or --to",
            report.package
        )),
    }
}

pub fn latest_target(
    opts: &FeedOptions,
    prerelease: bool,
    notes: &mut Vec<String>,
) -> Result<Result<Target, i32>> {
    Ok(match lookup(opts, notes, true)? {
        Ok(report) => match choose_target(&report, prerelease) {
            Ok(target) => Ok(target),
            Err(message) => {
                notes.push(message);
                Err(feed::EXIT_NOT_PUBLISHED)
            }
        },
        Err(code) => Err(code),
    })
}

pub fn proof(opts: &FeedOptions, notes: &mut Vec<String>) -> Result<Result<bool, i32>> {
    Ok(lookup(opts, notes, false)?.map(|report| report.published == Some(true)))
}

pub struct WaitOptions {
    pub interval: Duration,
    pub timeout: Duration,
    pub progress: bool,
}

fn attempt(session: &Session, ecosystem: Ecosystem, opts: &FeedOptions) -> Attempt {
    let pending = |feed: &str, versions: &[String]| {
        Report::build(
            &opts.package,
            ecosystem,
            feed,
            opts.version.as_deref(),
            versions,
        )
    };
    match fetch(session, ecosystem, opts) {
        Err(error) => Attempt::Failed(short_reason(&error)),
        Ok(Fetched::Refused { had_token }) => Attempt::Refused { had_token },
        Ok(Fetched::NotFound { feed }) => Attempt::Pending(pending(&feed, &[])),
        Ok(Fetched::Found { feed, versions }) => {
            let report = pending(&feed, &versions);
            if report.published == Some(true) {
                Attempt::Published(report)
            } else {
                Attempt::Pending(report)
            }
        }
    }
}

fn wait_message(
    package: &str,
    version: &str,
    attempts: u32,
    delay: Duration,
    error: Option<&str>,
) -> String {
    let next = crate::elapsed::format(delay);
    match error {
        Some(reason) => format!(
            "{package} {version}: attempt {attempts} failed ({reason}); trying again in {next}"
        ),
        None => format!(
            "{package} {version}: not published yet (attempt {attempts}); checking again in {next}"
        ),
    }
}

pub fn wait_for(
    opts: &FeedOptions,
    plan: WaitPlan,
    pause: impl FnMut(u32, Duration, Option<&str>),
    now: impl FnMut() -> Duration,
) -> Result<Waited> {
    if opts.version.is_none() {
        bail!("--wait requires --version");
    }
    if plan.interval.is_zero() {
        bail!("--interval must be at least 1 second");
    }
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    if ecosystem == Ecosystem::Nuget
        && feed::nuget_index_url(opts.org.as_deref(), opts.registry.as_deref()).is_none()
    {
        bail!("--org is required for nuget unless --registry is given");
    }
    registry::validate_version(ecosystem, opts.version.as_deref())?;
    let session = Session::new();
    Ok(feed::wait_loop(
        plan,
        || attempt(&session, ecosystem, opts),
        pause,
        now,
    ))
}

pub fn run_wait(opts: FeedOptions, wait: WaitOptions) -> Result<i32> {
    let started = Instant::now();
    let package = opts.package.clone();
    let version = opts.version.clone().unwrap_or_default();
    let progress = progress::Progress::start(wait.progress);
    progress::phase(&format!("waiting for {package} {version}"));
    let waited = wait_for(
        &opts,
        WaitPlan {
            interval: wait.interval,
            timeout: wait.timeout,
        },
        |attempts, delay, error| {
            progress::line(&wait_message(&package, &version, attempts, delay, error));
            std::thread::sleep(delay);
        },
        || started.elapsed(),
    )?;
    drop(progress);
    let code = waited.exit_code();
    match &waited.end {
        WaitEnd::Published(report) => print_wait(report, &waited, false, opts.json)?,
        WaitEnd::Refused { had_token } => eprintln!(
            "{}",
            refusal(
                opts.ecosystem
                    .unwrap_or_else(|| Ecosystem::default_for(&opts.package)),
                *had_token
            )
        ),
        WaitEnd::TimedOut {
            error: Some(reason),
            ..
        } => eprintln!(
            "{package} {version}: gave up after {}s: {reason}",
            waited.elapsed_seconds()
        ),
        WaitEnd::TimedOut {
            last: Some(report),
            error: None,
        } => print_wait(report, &waited, true, opts.json)?,
        WaitEnd::TimedOut {
            last: None,
            error: None,
        } => {}
    }
    Ok(code)
}

fn print_wait(report: &Report, waited: &Waited, timed_out: bool, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&feed::render_wait_json(report, waited, timed_out))?
        );
    } else if timed_out {
        println!(
            "{}",
            feed::render_timeout_text(report, waited.elapsed_seconds())
        );
    } else {
        println!("{}", feed::render_text(report));
    }
    Ok(())
}

pub(crate) fn refusal(ecosystem: Ecosystem, had_token: bool) -> String {
    match ecosystem {
        Ecosystem::Nuget | Ecosystem::Npm => feed::refused_message(had_token),
        _ => "the registry refused the request (401/403)".into(),
    }
}

pub struct Rendered {
    pub stdout: Option<String>,
    pub exit: i32,
}

pub fn render(opts: &FeedOptions, notes: &mut Vec<String>) -> Result<Rendered> {
    Ok(match lookup(opts, notes, false)? {
        Err(code) => Rendered {
            stdout: None,
            exit: code,
        },
        Ok(report) => Rendered {
            stdout: Some(if opts.json {
                serde_json::to_string_pretty(&feed::render_json(&report))?
            } else {
                feed::render_text(&report)
            }),
            exit: report.exit_code(),
        },
    })
}

pub fn run(opts: FeedOptions) -> Result<i32> {
    let mut notes = Vec::new();
    let rendered = render(&opts, &mut notes);
    for note in &notes {
        eprintln!("{note}");
    }
    let rendered = rendered?;
    if let Some(text) = &rendered.stdout {
        println!("{text}");
    }
    Ok(rendered.exit)
}

#[cfg(test)]
mod choose_tests {
    use super::*;

    fn report(versions: &[&str]) -> Report {
        let found: Vec<String> = versions.iter().map(|v| v.to_string()).collect();
        Report::build("Acme.Util", Ecosystem::Nuget, "feed", None, &found)
    }

    #[test]
    fn the_default_is_the_latest_stable_and_reports_the_ignored_prerelease() {
        let target = choose_target(&report(&["1.0.0", "1.1.0", "1.2.0-develop"]), false).unwrap();
        assert_eq!(target.version, "1.1.0");
        assert_eq!(target.ignored_prerelease.as_deref(), Some("1.2.0-develop"));
    }

    #[test]
    fn a_stable_latest_ignores_nothing() {
        let target = choose_target(&report(&["1.0.0", "1.1.0"]), false).unwrap();
        assert_eq!(target.version, "1.1.0");
        assert_eq!(target.ignored_prerelease, None);
    }

    #[test]
    fn a_prerelease_older_than_the_stable_is_not_reported() {
        let target = choose_target(&report(&["1.0.0-rc.1", "1.0.0"]), false).unwrap();
        assert_eq!(target.version, "1.0.0");
        assert_eq!(target.ignored_prerelease, None);
    }

    #[test]
    fn the_opt_in_takes_the_latest_even_when_it_is_a_prerelease() {
        let target = choose_target(&report(&["1.1.0", "1.2.0-develop"]), true).unwrap();
        assert_eq!(target.version, "1.2.0-develop");
        assert_eq!(target.ignored_prerelease, None);
    }

    #[test]
    fn only_prereleases_without_the_opt_in_is_an_error_that_names_the_way_out() {
        let message = choose_target(&report(&["1.0.0-beta"]), false).unwrap_err();
        assert!(message.contains("no stable version"), "{message}");
        assert!(message.contains("--prerelease"), "{message}");
    }
}
