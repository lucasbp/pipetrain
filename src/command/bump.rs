use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde_json::{Value, json};

use super::apply::tint;
use super::feed::{self as feed_command, FeedOptions};
use super::order::{Names, configured, names_of, read_repo, requests_line};
use super::resolve;
use crate::cache::{Cache, CacheMode};
use crate::config::ApiConfig;
use crate::consumer_bump::{RestoreMode, RestoreSettings};
use crate::elapsed;
use crate::feed::{self, Ecosystem};
use crate::github::{self, Client};
use crate::manifest;
use crate::progress::{self, Progress};
use crate::style::{bold, dim, green, yellow};

mod pins;
pub use pins::{check_workspace_sources, pin_manifest};

pub struct BumpOptions {
    pub package: String,
    pub to: Option<String>,
    pub ecosystem: Option<Ecosystem>,
    pub org: Option<String>,
    pub registry: Option<String>,
    pub repos: Vec<String>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub verbose: bool,
    pub cache: CacheMode,
    pub progress: bool,
    pub json: bool,
    pub only: Vec<Level>,
    pub changes_only: bool,
    pub check: bool,
    pub apply: bool,
    pub yes: bool,
    pub prerelease: bool,
    pub restore_mode: Option<RestoreMode>,
    pub solution: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Level {
    Major,
    Minor,
    Patch,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Major => "major",
            Self::Minor => "minor",
            Self::Patch => "patch",
        }
    }
}

pub fn level_of(current: &str, target: &str) -> Level {
    let from = feed::Version::parse(current);
    let to = feed::Version::parse(target);
    let (from, to) = (from.numbers(), to.numbers());
    for index in 0..from.len().max(to.len()) {
        if from.get(index).unwrap_or(&0) != to.get(index).unwrap_or(&0) {
            return match index {
                0 => Level::Major,
                1 => Level::Minor,
                _ => Level::Patch,
            };
        }
    }
    Level::Patch
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Change,
    UpToDate,
    Ahead,
    Range,
    OtherLevel,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::UpToDate => "up_to_date",
            Self::Ahead => "ahead",
            Self::Range => "range",
            Self::OtherLevel => "other_level",
        }
    }
}

fn is_exact(spec: &str) -> bool {
    let core = spec.split(['-', '+']).next().unwrap_or_default();
    !core.is_empty()
        && core
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && spec
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

pub fn classify(current: &str, target: &str) -> Status {
    if !is_exact(current) {
        return Status::Range;
    }
    match feed::compare(current, target) {
        std::cmp::Ordering::Equal => Status::UpToDate,
        std::cmp::Ordering::Less => Status::Change,
        std::cmp::Ordering::Greater => Status::Ahead,
    }
}

pub fn target_version(
    to: Option<&str>,
    lookup: impl FnOnce() -> Result<Result<String, i32>>,
) -> Result<Result<String, i32>> {
    match to {
        Some(version) => Ok(Ok(version.trim().to_string())),
        None => lookup(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRefs {
    pub repo: String,
    pub refs: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub repos: Vec<RepoRefs>,
    pub warnings: Vec<String>,
}

pub(crate) fn in_ecosystem(path: &str, ecosystem: Ecosystem) -> bool {
    let lower = path.to_lowercase();
    match ecosystem {
        Ecosystem::Nuget => lower.ends_with("proj") || lower.ends_with(".props"),
        Ecosystem::Npm => lower == "package.json" || lower.ends_with("/package.json"),
        Ecosystem::Cargo => path.rsplit('/').next() == Some("Cargo.toml"),
        Ecosystem::Pypi => path.rsplit('/').next() == Some("pyproject.toml"),
        Ecosystem::Go => path.rsplit('/').next() == Some("go.mod"),
    }
}

pub fn refs_of(
    manifests: &[(String, String, String)],
    package: &str,
    ecosystem: Ecosystem,
) -> Vec<(String, String)> {
    let tree = manifests
        .iter()
        .map(|(_, path, _)| path.clone())
        .collect::<Vec<_>>();
    checked_refs(manifests, &tree, package, ecosystem).unwrap_or_default()
}

fn checked_refs(
    manifests: &[(String, String, String)],
    tree: &[String],
    package: &str,
    ecosystem: Ecosystem,
) -> Result<Vec<(String, String)>> {
    let mut refs = Vec::new();
    for (repo, path, text) in manifests
        .iter()
        .filter(|(_, path, _)| in_ecosystem(path, ecosystem))
    {
        let mut versions = if pins::handles(ecosystem) {
            pins::versions(ecosystem, text, package).map_err(|_| {
                anyhow::anyhow!("could not parse {repo}/{path} for dependency updates")
            })?
        } else {
            manifest::package_versions(path, text, package)
        };
        if pins::handles(ecosystem)
            && !versions.is_empty()
            && let Err(reason) =
                check_workspace_sources(ecosystem, package, path, text, tree, |ancestor| {
                    manifests
                        .iter()
                        .find(|(_, p, _)| p == ancestor)
                        .map(|(_, _, text)| text.clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!("could not read ancestor manifest {ancestor}")
                        })
                })
        {
            versions.fill(format!("manual: {reason}"));
        }
        refs.extend(versions.into_iter().map(|v| (path.clone(), v)));
    }
    Ok(refs)
}

pub fn scan(
    client: &mut Client,
    names: &Names,
    package: &str,
    ecosystem: Ecosystem,
) -> Result<Scan> {
    let mut questions = Vec::new();
    let mut found = Scan::default();
    let mut readable = 0;
    progress::phase("reading manifests");
    for (index, repo) in names.values().enumerate() {
        match read_repo(client, repo, &mut questions)? {
            Some(snapshot) => {
                readable += 1;
                let mut refs =
                    checked_refs(&snapshot.manifests, &snapshot.tree, package, ecosystem)?;
                if pins::handles(ecosystem) && (snapshot.incomplete || !refs.is_empty()) {
                    let guard = if snapshot.incomplete {
                        let reason = "the repository tree or manifest contents were incomplete";
                        for (_, current) in &mut refs {
                            *current = format!("manual: {reason}");
                        }
                        Err(reason.to_owned())
                    } else {
                        crate::consumer_bump::apply::check_manifest_only(
                            ecosystem,
                            &snapshot.tree,
                            &refs,
                        )
                    };
                    if let Err(reason) = guard {
                        found.warnings.push(format!(
                            "warning: {repo}: --apply needs manual review: {reason}"
                        ));
                    }
                }
                if !refs.is_empty() {
                    found.repos.push(RepoRefs {
                        repo: repo.clone(),
                        refs,
                    });
                }
            }
            None => found.warnings.push(format!(
                "warning: skipped {repo}: GitHub answered not found, check the name in the config"
            )),
        }
        progress::count(index + 1, names.len());
    }
    if readable == 0 {
        bail!("no repo could be read: every name was not found, check the names in the config");
    }
    found
        .warnings
        .extend(questions.iter().map(|q| format!("warning: {}", q.message)));
    Ok(found)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub current: String,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRows {
    pub repo: String,
    pub files: Vec<Row>,
    pub restore: Option<RestoreSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub package: String,
    pub ecosystem: Ecosystem,
    pub target: String,
    pub repos: Vec<RepoRows>,
    pub ignored_prerelease: Option<String>,
}

pub fn restore_note(settings: &RestoreSettings) -> String {
    match &settings.solution {
        Some(solution) => format!("restore: {}, solution {solution}", settings.mode.as_str()),
        None => format!("restore: {}", settings.mode.as_str()),
    }
}

impl Report {
    pub fn build(package: &str, ecosystem: Ecosystem, target: &str, repos: &[RepoRefs]) -> Self {
        let repos = repos
            .iter()
            .map(|found| RepoRows {
                repo: found.repo.clone(),
                files: found
                    .refs
                    .iter()
                    .map(|(path, current)| Row {
                        path: path.clone(),
                        current: current.clone(),
                        status: if pins::handles(ecosystem) {
                            pins::classify(ecosystem, package, current, target)
                        } else {
                            classify(current, target)
                        },
                    })
                    .collect(),
                restore: None,
            })
            .collect();
        Self {
            package: package.to_string(),
            ecosystem,
            target: target.to_string(),
            repos,
            ignored_prerelease: None,
        }
    }

    pub fn with_ignored_prerelease(mut self, ignored: Option<String>) -> Self {
        self.ignored_prerelease = ignored;
        self
    }

    pub fn with_restore(mut self, settings: impl Fn(&str) -> RestoreSettings) -> Self {
        for repo in &mut self.repos {
            repo.restore = Some(settings(&repo.repo));
        }
        self
    }

    pub fn restrict(mut self, only: &[Level]) -> Self {
        if only.is_empty() {
            return self;
        }
        let target = self.target.clone();
        for row in self.repos.iter_mut().flat_map(|repo| repo.files.iter_mut()) {
            if row.status == Status::Change
                && !only.contains(&pins::level(self.ecosystem, &row.current, &target))
            {
                row.status = Status::OtherLevel;
            }
        }
        self
    }

    pub fn count(&self, status: Status) -> usize {
        self.repos
            .iter()
            .flat_map(|repo| &repo.files)
            .filter(|row| row.status == status)
            .count()
    }
}

fn row_text(report: &Report, row: &Row, color: bool) -> String {
    let head = format!("{}  {} {}", row.path, report.package, row.current);
    match row.status {
        Status::Change => format!("{head} -> {}", tint(color, green, &report.target)),
        Status::UpToDate => format!("{head} {}", tint(color, dim, "up to date")),
        Status::Ahead => format!(
            "{head} {}",
            tint(color, dim, "ahead of target (left alone)")
        ),
        Status::Range => format!("{head} {}", tint(color, yellow, "range, left alone")),
        Status::OtherLevel => format!(
            "{head} {}",
            tint(color, dim, "different level (left alone)")
        ),
    }
}

fn summary_line(report: &Report, elapsed: Option<Duration>) -> String {
    let change = report.count(Status::Change);
    let up_to_date = report.count(Status::UpToDate);
    let left = report.count(Status::Ahead)
        + report.count(Status::Range)
        + report.count(Status::OtherLevel);
    let repos = report.repos.len();
    let noun = if repos == 1 { "repo" } else { "repos" };
    let suffix = elapsed
        .map(|took| format!(" in {}", elapsed::format(took)))
        .unwrap_or_default();
    format!(
        "{change} would change, {up_to_date} up to date, {left} left alone in {repos} {noun}{suffix}"
    )
}

pub fn render_with(report: &Report, color: bool, elapsed: Option<Duration>) -> String {
    render_rows(report, color, elapsed, false)
}

pub fn render_rows(
    report: &Report,
    color: bool,
    elapsed: Option<Duration>,
    changes_only: bool,
) -> String {
    let mut out = String::new();
    let title = format!("bump {} to {}", report.package, report.target);
    out.push_str(&format!("{}\n", tint(color, bold, &title)));
    if let Some(ignored) = &report.ignored_prerelease {
        out.push_str(&format!(
            "{}\n",
            tint(
                color,
                yellow,
                &format!("ignored newer prerelease {ignored}; pass --prerelease to use it")
            )
        ));
    }
    for repo in &report.repos {
        let rows: Vec<&Row> = repo
            .files
            .iter()
            .filter(|row| !changes_only || row.status == Status::Change)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let note = repo
            .restore
            .as_ref()
            .map(|settings| format!("  {}", tint(color, dim, &restore_note(settings))))
            .unwrap_or_default();
        out.push_str(&format!("{}{note}\n", tint(color, bold, &repo.repo)));
        for row in rows {
            out.push_str(&format!("  {}\n", row_text(report, row, color)));
        }
    }
    out.push_str(&format!("{}\n", summary_line(report, elapsed)));
    out.push_str(&format!(
        "{}\n",
        tint(color, yellow, "dry run: nothing was written")
    ));
    out
}

pub fn render(report: &Report) -> String {
    render_with(report, false, None)
}

pub fn to_json(report: &Report) -> Value {
    to_json_with(report, &[], false)
}

pub fn to_json_with(report: &Report, only: &[Level], changes_only: bool) -> Value {
    let repos: Vec<Value> = report
        .repos
        .iter()
        .filter(|repo| !changes_only || repo.files.iter().any(|row| row.status == Status::Change))
        .map(|repo| {
            let files: Vec<Value> = repo
                .files
                .iter()
                .filter(|row| !changes_only || row.status == Status::Change)
                .map(|row| {
                    json!({
                        "path": row.path,
                        "current": row.current,
                        "status": row.status.as_str(),
                    })
                })
                .collect();
            let mut entry = json!({ "repo": repo.repo, "files": files });
            if let Some(settings) = &repo.restore {
                entry["restore_mode"] = json!(settings.mode.as_str());
                if let Some(solution) = &settings.solution {
                    entry["solution"] = json!(solution);
                }
            }
            entry
        })
        .collect();
    let mut out = json!({
        "schema": 1,
        "package": report.package,
        "ecosystem": report.ecosystem.as_str(),
        "target": report.target,
        "repos": repos,
        "dry_run": true,
    });
    if let Some(ignored) = &report.ignored_prerelease {
        out["ignored_prerelease"] = json!(ignored);
    }
    if !only.is_empty() {
        out["only"] = only.iter().map(|level| level.as_str()).collect();
    }
    out
}

pub struct Outcome {
    pub report: Option<Report>,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub exit: i32,
    pub elapsed: Duration,
    pub(crate) feed: Option<FeedOptions>,
    pub(crate) api: Option<ApiConfig>,
}

impl Outcome {
    fn stop(before: Vec<String>, exit: i32, started: Instant) -> Self {
        Self {
            report: None,
            before,
            after: Vec::new(),
            exit,
            elapsed: started.elapsed(),
            feed: None,
            api: None,
        }
    }
}

pub fn execute(opts: &BumpOptions) -> Result<Outcome> {
    let started = Instant::now();
    let mut before = Vec::new();
    let resolved = resolve::load(opts.config.as_deref(), opts.profile.as_deref(), &[], &[])?;
    let cfg = resolved.cfg;
    let (exact, globs) = configured(&cfg);
    let from_config = opts.repos.is_empty();
    let names = if from_config {
        names_of(exact)
    } else {
        names_of(resolved.aliases.resolve_refs(&opts.repos))
    };
    if names.is_empty() {
        bail!("no repos given and the profile has no exact repo names in [[layer]] or [[order]]");
    }
    if from_config && !globs.is_empty() {
        before.push(format!(
            "warning: ignored names with a glob: {}",
            globs.join(", ")
        ));
    }
    let ecosystem = opts
        .ecosystem
        .unwrap_or_else(|| Ecosystem::default_for(&opts.package));
    if pins::handles(ecosystem) && (opts.solution.is_some() || opts.restore_mode.is_some()) {
        bail!(
            "--solution and --restore-mode are unavailable for {} manifest-only updates",
            ecosystem.as_str()
        );
    }
    let feed_options = FeedOptions {
        package: opts.package.clone(),
        version: None,
        ecosystem: Some(ecosystem),
        org: opts.org.as_deref().map(|org| resolved.aliases.resolve(org)),
        registry: opts.registry.clone(),
        json: false,
    };
    let mut ignored_prerelease = None;
    let target = match target_version(opts.to.as_deref(), || {
        Ok(
            match feed_command::latest_target(&feed_options, opts.prerelease, &mut before)? {
                Ok(target) => {
                    ignored_prerelease = target.ignored_prerelease;
                    Ok(target.version)
                }
                Err(code) => Err(code),
            },
        )
    })? {
        Ok(target) => target,
        Err(code) => return Ok(Outcome::stop(before, code, started)),
    };
    if target.is_empty() {
        bail!("--to needs a version");
    }
    if pins::handles(ecosystem) && !pins::valid_version(ecosystem, &opts.package, &target) {
        bail!(
            "{} is not a supported exact {} version for {}; choose an exact published version compatible with the package",
            target,
            ecosystem.as_str(),
            opts.package
        );
    }
    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose)
        .with_cache(Cache::at_default(opts.cache));
    let progress = Progress::start(opts.progress);
    let scanned = scan(&mut client, &names, &opts.package, ecosystem)?;
    drop(progress);
    before.extend(scanned.warnings.iter().cloned());
    if scanned.repos.is_empty() {
        before.push(format!(
            "{} is not referenced in any scanned manifest",
            opts.package
        ));
        return Ok(Outcome::stop(before, 1, started));
    }
    let mut report = Report::build(&opts.package, ecosystem, &target, &scanned.repos)
        .restrict(&opts.only)
        .with_ignored_prerelease(ignored_prerelease);
    if ecosystem == Ecosystem::Nuget {
        report = report.with_restore(|repo| {
            cfg.bump
                .settings_for(repo, opts.restore_mode, opts.solution.as_deref())
        });
    }
    let elapsed = started.elapsed();
    let mut after = vec![requests_line(
        client.requests(),
        client.cache_hits(),
        elapsed,
    )];
    let pending = report.count(Status::Change);
    let mut exit = 0;
    if opts.check && pending > 0 {
        after.push(format!("--check: {pending} would change"));
        exit = 1;
    }
    Ok(Outcome {
        report: Some(report),
        before,
        after,
        exit,
        elapsed,
        feed: Some(feed_options),
        api: Some(cfg.api.clone()),
    })
}

pub fn json_text(outcome: &Outcome, opts: &BumpOptions) -> Result<Option<String>> {
    let Some(report) = &outcome.report else {
        return Ok(None);
    };
    let json = to_json_with(report, &opts.only, opts.changes_only);
    Ok(Some(serde_json::to_string_pretty(&json)?))
}

pub fn run(opts: BumpOptions) -> Result<i32> {
    if opts.apply {
        super::bump_apply::check_gate(&opts)?;
    }
    let outcome = execute(&opts)?;
    if opts.apply {
        return super::bump_apply::run(&opts, outcome);
    }
    for note in &outcome.before {
        eprintln!("{note}");
    }
    if let Some(report) = &outcome.report {
        if opts.json {
            if let Some(text) = json_text(&outcome, &opts)? {
                println!("{text}");
            }
        } else {
            anstream::print!(
                "{}",
                render_rows(report, true, Some(outcome.elapsed), opts.changes_only)
            );
        }
    }
    for note in &outcome.after {
        eprintln!("{note}");
    }
    Ok(outcome.exit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(path: &str, text: &str) -> (String, String, String) {
        ("acme/api".to_string(), path.to_string(), text.to_string())
    }

    fn csproj(version: &str) -> String {
        format!(
            r#"<Project><PackageReference Include="Acme.Util" Version="{version}" /></Project>"#
        )
    }

    fn sample() -> Report {
        let repos = vec![
            RepoRefs {
                repo: "acme/api".to_string(),
                refs: vec![
                    ("src/A.csproj".to_string(), "1.2.0".to_string()),
                    ("src/B.csproj".to_string(), "1.5.0".to_string()),
                ],
            },
            RepoRefs {
                repo: "acme/web".to_string(),
                refs: vec![
                    ("C.csproj".to_string(), "2.0.0".to_string()),
                    ("D.csproj".to_string(), "[1.0,2.0)".to_string()),
                ],
            },
        ];
        Report::build("Acme.Util", Ecosystem::Nuget, "1.5.0", &repos)
    }

    #[test]
    fn extraction_reads_only_manifests_of_the_chosen_ecosystem() {
        let manifests = vec![
            manifest("src/A.csproj", &csproj("1.2.0")),
            manifest(
                "web/package.json",
                r#"{"dependencies":{"Acme.Util":"1.0.0"}}"#,
            ),
            manifest("Directory.Packages.props", &csproj("1.4.0")),
        ];
        assert_eq!(
            refs_of(&manifests, "Acme.Util", Ecosystem::Nuget),
            vec![
                ("src/A.csproj".to_string(), "1.2.0".to_string()),
                ("Directory.Packages.props".to_string(), "1.4.0".to_string())
            ]
        );
        assert_eq!(
            refs_of(&manifests, "Acme.Util", Ecosystem::Npm),
            vec![("web/package.json".to_string(), "1.0.0".to_string())]
        );
    }

    #[test]
    fn extraction_skips_other_packages() {
        let manifests = vec![manifest("A.csproj", &csproj("1.2.0"))];
        assert!(refs_of(&manifests, "Acme.Other", Ecosystem::Nuget).is_empty());
    }

    #[test]
    fn status_compares_exact_versions_with_the_feed_ordering() {
        assert_eq!(classify("1.2.0", "1.10.0"), Status::Change);
        assert_eq!(classify("1.10.0", "1.10.0"), Status::UpToDate);
        assert_eq!(classify("1.10", "1.10.0"), Status::UpToDate);
        assert_eq!(classify("2.0.0", "1.10.0"), Status::Ahead);
        assert_eq!(classify("1.0.0-beta.1", "1.0.0"), Status::Change);
    }

    #[test]
    fn floating_specs_are_ranges() {
        for spec in [
            "^1.2.0",
            "~1.2.0",
            "*",
            "1.*",
            "[1.0,2.0)",
            ">=1.0.0",
            "latest",
            "1.x",
        ] {
            assert_eq!(classify(spec, "9.9.9"), Status::Range, "{spec}");
        }
    }

    #[test]
    fn the_target_comes_from_to_without_asking_the_feed() {
        let target = target_version(Some(" 1.5.0 "), || panic!("the feed must not be asked"));
        assert_eq!(target.unwrap().unwrap(), "1.5.0");
    }

    #[test]
    fn the_target_comes_from_the_feed_without_to() {
        let target = target_version(None, || Ok(Ok("3.0.0".to_string())));
        assert_eq!(target.unwrap().unwrap(), "3.0.0");
        let refused = target_version(None, || Ok(Err(2)));
        assert_eq!(refused.unwrap().unwrap_err(), 2);
    }

    #[test]
    fn the_plain_render_lists_every_row_and_the_summary() {
        let text = render(&sample());
        let expected = [
            "bump Acme.Util to 1.5.0",
            "acme/api",
            "  src/A.csproj  Acme.Util 1.2.0 -> 1.5.0",
            "  src/B.csproj  Acme.Util 1.5.0 up to date",
            "acme/web",
            "  C.csproj  Acme.Util 2.0.0 ahead of target (left alone)",
            "  D.csproj  Acme.Util [1.0,2.0) range, left alone",
            "1 would change, 1 up to date, 2 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn the_summary_shows_the_elapsed_time() {
        let text = render_with(&sample(), false, Some(Duration::from_millis(3_200)));
        assert!(text.contains("in 2 repos in 3.2 s\n"), "{text}");
    }

    #[test]
    fn the_colored_render_wraps_the_pieces_in_escape_codes() {
        let text = render_with(&sample(), true, None);
        assert!(text.contains('\u{1b}'));
    }

    #[test]
    fn json_has_the_documented_shape() {
        assert_eq!(
            to_json(&sample()),
            json!({
                "schema": 1,
                "package": "Acme.Util",
                "ecosystem": "nuget",
                "target": "1.5.0",
                "repos": [
                    {"repo": "acme/api", "files": [
                        {"path": "src/A.csproj", "current": "1.2.0", "status": "change"},
                        {"path": "src/B.csproj", "current": "1.5.0", "status": "up_to_date"}
                    ]},
                    {"repo": "acme/web", "files": [
                        {"path": "C.csproj", "current": "2.0.0", "status": "ahead"},
                        {"path": "D.csproj", "current": "[1.0,2.0)", "status": "range"}
                    ]}
                ],
                "dry_run": true
            })
        );
    }

    fn leveled() -> Report {
        let repos = vec![
            RepoRefs {
                repo: "acme/api".to_string(),
                refs: vec![
                    ("A.csproj".to_string(), "1.0.0".to_string()),
                    ("B.csproj".to_string(), "2.0.0".to_string()),
                    ("C.csproj".to_string(), "2.1.0".to_string()),
                    ("D.csproj".to_string(), "2.1.5".to_string()),
                    ("E.csproj".to_string(), "3.0.0".to_string()),
                    ("F.csproj".to_string(), "^1.0".to_string()),
                ],
            },
            RepoRefs {
                repo: "acme/web".to_string(),
                refs: vec![
                    ("G.csproj".to_string(), "2.1.5".to_string()),
                    ("H.csproj".to_string(), "3.0.0".to_string()),
                ],
            },
        ];
        Report::build("Acme.Util", Ecosystem::Nuget, "2.1.5", &repos)
    }

    fn statuses(report: &Report) -> Vec<Status> {
        report
            .repos
            .iter()
            .flat_map(|repo| repo.files.iter().map(|row| row.status))
            .collect()
    }

    #[test]
    fn the_level_is_the_first_number_that_differs() {
        assert_eq!(level_of("1.2.0", "2.0.0"), Level::Major);
        assert_eq!(level_of("1.2.0", "1.3.0"), Level::Minor);
        assert_eq!(level_of("1.2.0", "1.2.5"), Level::Patch);
        assert_eq!(level_of("1.2.0", "1.2.0.1"), Level::Patch);
        assert_eq!(level_of("1.0.0-beta", "1.0.0"), Level::Patch);
        assert_eq!(level_of("1.10", "1.10.1"), Level::Patch);
    }

    #[test]
    fn restrict_leaves_only_changes_of_the_chosen_level() {
        let report = leveled().restrict(&[Level::Minor]);
        assert_eq!(
            statuses(&report),
            vec![
                Status::OtherLevel,
                Status::Change,
                Status::OtherLevel,
                Status::UpToDate,
                Status::Ahead,
                Status::Range,
                Status::UpToDate,
                Status::Ahead,
            ]
        );
    }

    #[test]
    fn restrict_accepts_several_levels() {
        let report = leveled().restrict(&[Level::Major, Level::Patch]);
        assert_eq!(report.count(Status::Change), 2);
        assert_eq!(report.count(Status::OtherLevel), 1);
    }

    #[test]
    fn an_empty_restriction_changes_nothing() {
        assert_eq!(leveled().restrict(&[]), leveled());
    }

    #[test]
    fn the_summary_counts_other_levels_as_left_alone() {
        let text = render(&leveled().restrict(&[Level::Minor]));
        assert!(
            text.contains("1 would change, 2 up to date, 5 left alone in 2 repos"),
            "{text}"
        );
        assert!(text.contains("A.csproj  Acme.Util 1.0.0 different level (left alone)"));
    }

    #[test]
    fn changes_only_lists_just_the_changes_and_keeps_the_summary() {
        let text = render_rows(&leveled(), false, None, true);
        let expected = [
            "bump Acme.Util to 2.1.5",
            "acme/api",
            "  A.csproj  Acme.Util 1.0.0 -> 2.1.5",
            "  B.csproj  Acme.Util 2.0.0 -> 2.1.5",
            "  C.csproj  Acme.Util 2.1.0 -> 2.1.5",
            "3 would change, 2 up to date, 3 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn changes_only_follows_the_restriction() {
        let text = render_rows(&leveled().restrict(&[Level::Minor]), false, None, true);
        let expected = [
            "bump Acme.Util to 2.1.5",
            "acme/api",
            "  B.csproj  Acme.Util 2.0.0 -> 2.1.5",
            "1 would change, 2 up to date, 5 left alone in 2 repos",
            "dry run: nothing was written",
            "",
        ]
        .join("\n");
        assert_eq!(text, expected);
    }

    #[test]
    fn without_the_new_options_the_render_is_unchanged() {
        let report = leveled();
        assert_eq!(
            render_rows(&report, false, None, false),
            render_with(&report, false, None)
        );
    }

    #[test]
    fn json_without_options_equals_the_plain_json() {
        let report = leveled();
        let json = to_json_with(&report, &[], false);
        assert_eq!(json, to_json(&report));
        assert!(json.get("only").is_none());
    }

    #[test]
    fn json_lists_the_levels_it_was_restricted_to() {
        let report = leveled().restrict(&[Level::Minor]);
        let json = to_json_with(&report, &[Level::Minor], false);
        assert_eq!(json["only"], json!(["minor"]));
        assert_eq!(json["repos"][0]["files"][0]["status"], json!("other_level"));
        assert_eq!(json["repos"][0]["files"][1]["status"], json!("change"));
        assert_eq!(json["repos"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn json_changes_only_drops_other_files_and_repos() {
        let report = leveled().restrict(&[Level::Minor]);
        let json = to_json_with(&report, &[Level::Minor], true);
        assert_eq!(
            json["repos"],
            json!([
                {"repo": "acme/api", "files": [
                    {"path": "B.csproj", "current": "2.0.0", "status": "change"}
                ]}
            ])
        );
        assert_eq!(json["only"], json!(["minor"]));
    }
}
