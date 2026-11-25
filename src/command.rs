use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, bail};
use chrono::{Local, NaiveDate, SecondsFormat};

use crate::cache::{Cache, CacheMode};
use crate::command::authors::{self as author_filter, AuthorFilter};
use crate::config::{Config, Rules, render_date};
use crate::contained;
use crate::content;
use crate::detect;
use crate::discovery::{self, Discovered, Scope, Skipped};
use crate::elapsed;
use crate::emit::{self, Emit};
use crate::github::{self, ApiError, Client};
use crate::model::{Pr, PrKey, Question};
use crate::plan::{self, Gathered, Plan};
use crate::planner::release;
use crate::planner::{self, DepState, behind};
use crate::progress::{self, Progress};
use crate::publishers::{self, Publishers, References};
use crate::render;
use crate::workflows;

pub mod apply;
pub mod authors;
pub mod bump;
pub mod bump_apply;
mod checklist;
pub mod feed;
pub mod init;
pub mod log;
pub mod merge;
pub mod names;
pub mod order;
pub mod orgs;
pub mod profiles;
mod resolve;
pub mod state;
pub mod update;

const MAX_EXTERNAL_REFS: usize = 20;

pub struct PlanOptions {
    pub prs: Vec<String>,
    pub all: bool,
    pub json: bool,
    pub emit: Option<Emit>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub orgs: Vec<String>,
    pub authors: AuthorFilter,
    pub date: Option<String>,
    pub verbose: bool,
    pub cache: CacheMode,
    pub progress: bool,
    pub check_feeds: bool,
    pub check_bumps: bool,
    pub strict: bool,
}

struct Collected {
    ready: Vec<(Pr, Scope)>,
    skipped: Vec<Skipped>,
    questions: Vec<Question>,
    external: BTreeMap<String, DepState>,
    publishers: Publishers,
    references: References,
}

const ISO_DATE_FORMAT: &str = "%Y-%m-%d";

pub fn parse_date(text: &str, format: &str) -> Result<NaiveDate> {
    if let Ok(date) = NaiveDate::parse_from_str(text, format) {
        return Ok(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, ISO_DATE_FORMAT) {
        return Ok(date);
    }
    let sample = NaiveDate::from_ymd_opt(2026, 10, 29).expect("constant date is valid");
    let configured = render_date(format, sample);
    let iso = sample.format(ISO_DATE_FORMAT).to_string();
    match configured {
        Some(shown) if shown != iso => bail!(
            "--date must look like {shown} (the configured train_date_format `{format}`) or {iso} (ISO), got `{text}`"
        ),
        _ => bail!("--date must look like {iso}, got `{text}`"),
    }
}

fn discover(
    client: &mut Client,
    cfg: &Config,
    explicit: &[PrKey],
    train: &str,
    released: bool,
    authors: &[String],
) -> Result<Discovered> {
    let mut pool = Vec::new();
    if explicit.is_empty() || !authors.is_empty() {
        for org in &cfg.orgs {
            pool.extend(client.open_prs(org)?);
        }
    }
    let mut out = if explicit.is_empty() {
        if released {
            discovery::released(pool.clone(), cfg, train)
        } else {
            discovery::discover_authored(pool.clone(), cfg, train, authors)
        }
    } else {
        let mut prs = Vec::new();
        for key in explicit {
            match client.pr(key)? {
                Some(pr) => prs.push(pr),
                None => {
                    return Err(ApiError::NotFound {
                        what: key.to_string(),
                    }
                    .into());
                }
            }
        }
        let mut found = discovery::explicit(prs, cfg);
        discovery::add_authored(&mut found, &pool, cfg, authors);
        found
    };
    let have_pool = !pool.is_empty();
    discovery::add_stack_parents(&mut out, cfg, |child| {
        if have_pool {
            Ok(pool
                .iter()
                .filter(|p| p.head_ref == child.base_ref)
                .cloned()
                .collect())
        } else {
            client.open_prs_with_head(&child.key, &child.base_ref)
        }
    })?;
    Ok(out)
}

fn load_files(client: &mut Client, ready: &mut [(Pr, Scope)]) -> Result<()> {
    let total = ready.len();
    for (index, (pr, _)) in ready.iter_mut().enumerate() {
        let (files, complete) = client.files(&pr.key, &pr.head_sha, &pr.base_sha)?;
        pr.files = files;
        pr.files_complete = complete;
        progress::count(index + 1, total);
    }
    Ok(())
}

fn external_states(
    client: &mut Client,
    ready: &[(Pr, Scope)],
) -> Result<BTreeMap<String, DepState>> {
    let prs: Vec<Pr> = ready.iter().map(|(p, _)| p.clone()).collect();
    let mut external = BTreeMap::new();
    for key in planner::external_refs(&prs)
        .into_iter()
        .take(MAX_EXTERNAL_REFS)
    {
        external.insert(key.lookup_id(), client.dep_state(&key)?);
    }
    Ok(external)
}

fn wanted_packages(rules: &Rules, ready: &[(Pr, Scope)]) -> BTreeSet<String> {
    ready
        .iter()
        .flat_map(|(pr, _)| detect::bumped_packages(&pr.files))
        .filter(|name| !rules.packages.contains_key(name))
        .collect()
}

fn repos_to_scan(ready: &[(Pr, Scope)]) -> BTreeMap<String, (String, String)> {
    let mut repos: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (pr, _) in ready {
        repos
            .entry(pr.key.slug().to_lowercase())
            .or_insert_with(|| (pr.key.slug(), pr.head_sha.clone()));
    }
    repos
}

pub(crate) fn scan_repo(
    client: &mut Client,
    slug: &str,
    sha: &str,
    questions: &mut Vec<Question>,
) -> Result<Vec<(String, String, String)>> {
    let (tree, truncated) = client.tree(slug, sha)?;
    if truncated {
        questions.push(Question::new(
            None,
            "tree-truncated",
            format!("the file tree of {slug} is too large to scan, so it may publish a package that was not seen"),
            "pin the package under [packages] in the config",
        ));
    }
    let (paths, cut) = publishers::candidate_paths(&tree);
    if cut {
        questions.push(Question::new(
            None,
            "manifests-truncated",
            format!(
                "{slug} has more than {} package manifests, only the shallowest were read, so a published package may be missing",
                publishers::MAX_MANIFESTS_PER_REPO
            ),
            "pin the package under [packages] in the config",
        ));
    }
    let blobs = client.blobs(slug, sha, &paths)?;
    Ok(blobs
        .into_iter()
        .map(|(path, text)| (slug.to_string(), path, text))
        .collect())
}

fn scan_publishers(
    client: &mut Client,
    rules: &Rules,
    ready: &[(Pr, Scope)],
    questions: &mut Vec<Question>,
) -> Result<(Publishers, References, Vec<release::Manifest>)> {
    let repos = repos_to_scan(ready);
    if repos.len() < 2 && wanted_packages(rules, ready).is_empty() {
        return Ok((Publishers::new(), References::new(), Vec::new()));
    }
    progress::phase("reading manifests");
    let mut manifests = Vec::new();
    for (index, (slug, sha)) in repos.values().enumerate() {
        manifests.extend(scan_repo(client, slug, sha, questions)?);
        progress::count(index + 1, repos.len());
    }
    Ok((
        publishers::collect(&manifests),
        publishers::collect_references(&manifests),
        manifests,
    ))
}

fn release_context(
    client: &mut Client,
    cfg: &Config,
    rules: &Rules,
    ready: &[(Pr, Scope)],
    scanned: &(Publishers, References, Vec<release::Manifest>),
    with_bumps: bool,
) -> Result<release::Context> {
    let (publishers, references, manifests) = scanned;
    let prs: Vec<&Pr> = ready.iter().map(|(pr, _)| pr).collect();
    let mut context = release::Context::new(
        rules,
        publishers,
        references,
        manifests,
        &prs,
        cfg.merge.package_max_behind,
    );
    let mut candidates = context.limit_candidates();
    if with_bumps {
        candidates.extend(behind::consumers(&context));
    }
    if candidates.is_empty() {
        return Ok(context);
    }
    progress::phase("reading workflow limits");
    let repos = repos_to_scan(ready);
    for (index, repo) in candidates.iter().enumerate() {
        if let Some((slug, sha)) = repos.get(repo) {
            let files = workflows::read(client, slug, sha)?;
            context
                .limits
                .insert(repo.clone(), workflows::max_behind(&files));
        }
        progress::count(index + 1, candidates.len());
    }
    Ok(context)
}

fn pair_packages(
    context: &release::Context,
    ready: &[(Pr, Scope)],
) -> BTreeMap<String, Vec<String>> {
    let pairs = publishers::dependency_pairs(
        |repo| context.repos.contains(repo),
        &context.references,
        &context.publishers,
    );
    let mut found = BTreeMap::new();
    for ((_, consumer), package) in &pairs {
        let pinned = context
            .versions
            .get(&(consumer.clone(), package.clone()))
            .is_some_and(|specs| specs.iter().any(|spec| behind::is_pin(spec)));
        let pending = ready.iter().any(|(pr, _)| {
            pr.key.slug().to_lowercase() == *consumer
                && !detect::bumped_packages(&pr.files).contains(package)
        });
        if !pinned || !pending {
            continue;
        }
        let orgs = context
            .publishers
            .get(package)
            .map(|owners| {
                owners
                    .iter()
                    .filter_map(|slug| slug.split_once('/').map(|(org, _)| org.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        found.insert(package.clone(), orgs);
    }
    found
}

fn read_feeds(wanted: &BTreeMap<String, Vec<String>>) -> behind::Feeds {
    if wanted.is_empty() {
        return behind::Feeds::new();
    }
    progress::phase("reading package feeds");
    let mut feeds = behind::Feeds::new();
    for (index, (package, orgs)) in wanted.iter().enumerate() {
        let opts = feed::FeedOptions {
            package: package.clone(),
            version: None,
            ecosystem: None,
            org: feed::publishing_org(package, orgs),
            registry: None,
            json: false,
        };
        if let Ok(Ok(versions)) = feed::versions(&opts, &mut Vec::new()) {
            feeds.insert(package.clone(), versions);
        }
        progress::count(index + 1, wanted.len());
    }
    feeds
}

fn deploy_branch_for(cfg: &Config, date: Option<&str>) -> Result<String> {
    let date = match date {
        None => Local::now().date_naive(),
        Some(text) => parse_date(text, &cfg.train_date_format)?,
    };
    Ok(discovery::train_branch(cfg, date))
}

fn explicit_targets(raw_prs: &[String], cfg: &Config) -> Result<Vec<names::Target>> {
    let explicit = names::parse_targets(raw_prs)?;
    if explicit.is_empty() && cfg.orgs.is_empty() {
        bail!(
            "no orgs to look at: pass --org, name PRs as org/repo#123, or set `orgs` in pipetrain.toml"
        );
    }
    Ok(explicit)
}

fn train_label(explicit: &[names::Target], all: bool, deploy_branch: &str) -> String {
    if !explicit.is_empty() {
        "explicit PRs".to_string()
    } else if all {
        "all released PRs".to_string()
    } else {
        deploy_branch.to_string()
    }
}

fn gathered_orgs(cfg: &Config, explicit: &[PrKey]) -> Vec<String> {
    if explicit.is_empty() {
        return cfg.orgs.clone();
    }
    let mut orgs: Vec<String> = explicit.iter().map(|k| k.org.clone()).collect();
    orgs.sort();
    orgs.dedup();
    orgs
}

fn into_gathered(
    cfg: &Config,
    explicit: &[PrKey],
    train: String,
    deploy_branch: String,
    client: &Client,
    collected: Collected,
) -> Gathered {
    Gathered {
        generated_at: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        train,
        deploy_branch,
        train_prefix: cfg.train_prefix.clone(),
        orgs: gathered_orgs(cfg, explicit),
        ready: collected
            .ready
            .into_iter()
            .map(|(pr, scope)| (pr, scope.as_str().to_string()))
            .collect(),
        skipped: collected.skipped,
        questions: collected.questions,
        external: collected.external,
        publishers: collected.publishers,
        references: collected.references,
        max_parallel: cfg.max_parallel,
        requests: client.requests(),
        rate_remaining: client.remaining(),
    }
}

fn plan_text(built: &Plan) -> Result<String> {
    Ok(serde_json::to_string_pretty(built)?)
}

pub fn plan_json(opts: &PlanOptions) -> Result<String> {
    let (built, _) = build_plan(opts)?;
    plan_text(&built)
}

fn emit(built: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", plan_text(built)?);
    } else {
        anstream::print!("{}", render::text(built));
    }
    Ok(())
}

fn print_queue(built: &Plan) {
    for line in emit::question_lines(built) {
        eprintln!("{line}");
    }
    let lines = emit::queue_lines(built);
    if lines.is_empty() {
        eprintln!("no PRs ready");
    }
    for line in lines {
        println!("{line}");
    }
}

fn deliver(built: &Plan, json: bool, mode: Option<Emit>) -> Result<i32> {
    match mode {
        Some(Emit::Queue) => {
            print_queue(built);
            Ok(emit::exit_code(built))
        }
        None => {
            emit(built, json)?;
            Ok(0)
        }
    }
}

pub fn plan(opts: PlanOptions) -> Result<i32> {
    let (built, _) = build_plan(&opts)?;
    let code = deliver(&built, opts.json, opts.emit)?;
    Ok(if opts.strict && !built.has_eligible() {
        emit::EXIT_NO_ELIGIBLE
    } else {
        code
    })
}

pub(crate) fn build_plan(opts: &PlanOptions) -> Result<(Plan, Client)> {
    let (built, client, _) = build_plan_with_config(opts)?;
    Ok((built, client))
}

pub(crate) fn build_plan_with_config(opts: &PlanOptions) -> Result<(Plan, Client, Config)> {
    build_plan_with_prepare(opts, |_, _, _, _| Ok(()))
}

fn detect_deploy_resources(
    client: &mut Client,
    plan: &Plan,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let targets: std::collections::BTreeSet<(String, String)> = plan
        .items
        .iter()
        .map(|item| (format!("{}/{}", item.org, item.repo), item.base.clone()))
        .collect();
    if targets.is_empty() {
        return std::collections::BTreeMap::new();
    }
    progress::phase("detecting deploy resources");
    crate::deploy_resources::detect_all(client, &targets)
}

fn build_plan_with_prepare(
    opts: &PlanOptions,
    prepare: impl FnOnce(&mut Client, &Config, &mut Discovered, &str) -> Result<()>,
) -> Result<(Plan, Client, Config)> {
    let started = Instant::now();
    let resolve::Resolved {
        cfg,
        info: profile,
        prs,
        ..
    } = resolve::load(
        opts.config.as_deref(),
        opts.profile.as_deref(),
        &opts.prs,
        &opts.orgs,
    )?;
    let mut rules = Rules::new(&cfg)?;
    let deploy_branch = deploy_branch_for(&cfg, opts.date.as_deref())?;
    let explicit = explicit_targets(&prs, &cfg)?;
    let train = train_label(&explicit, opts.all, &deploy_branch);

    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose)
        .with_cache(Cache::at_default(opts.cache))
        .with_http_cache();
    crate::quota::refuse(
        &mut client,
        &cfg.api,
        &crate::quota::plan_need(cfg.orgs.len(), explicit.len()),
    )?;
    let progress = Progress::start(opts.progress);
    if names::has_bare(&explicit) {
        progress::phase("resolving repo names");
    }
    let explicit = names::resolve(&mut client, explicit, &cfg.orgs)?;
    let selecting = author_filter::resolve(&opts.authors.selecting(), &mut client)?;
    progress::phase("discovering pull requests");
    let mut discovered = discover(
        &mut client,
        &cfg,
        &explicit,
        &deploy_branch,
        opts.all,
        &selecting,
    )?;
    prepare(&mut client, &cfg, &mut discovered, &deploy_branch)?;
    let Discovered {
        ready,
        mut skipped,
        mut questions,
    } = discovered;

    progress::phase(&format!("reading {} pull requests", ready.len()));
    let (mut ready, merged) = contained::filter(&mut client, ready, &deploy_branch)?;
    skipped.extend(merged);

    load_files(&mut client, &mut ready)?;

    progress::phase("checking contents");
    let (ready, same) = content::filter(&mut client, ready, &deploy_branch)?;
    skipped.extend(same);

    let external = external_states(&mut client, &ready)?;
    let scanned = scan_publishers(&mut client, &rules, &ready, &mut questions)?;
    let release_context = release_context(
        &mut client,
        &cfg,
        &rules,
        &ready,
        &scanned,
        opts.check_bumps,
    )?;
    let (publishers, references, _) = scanned;
    let mut ready = ready;
    let mut wanted = pair_packages(&release_context, &ready);
    if opts.check_bumps {
        wanted.extend(behind::candidates(&release_context));
    }
    let feeds = read_feeds(&wanted);
    rules.evidence = publishers::Evidence {
        pins: release_context.versions.clone(),
        feeds: feeds.clone(),
    };
    let bumps = if opts.check_bumps {
        behind::hold(&release_context, &feeds, &mut ready, &mut skipped)
    } else {
        Vec::new()
    };

    let collected = Collected {
        ready,
        skipped,
        questions,
        external,
        publishers,
        references,
    };
    let gathered = into_gathered(&cfg, &explicit, train, deploy_branch, &client, collected);
    let mut built = plan::build(&rules, gathered);
    release::annotate(&mut built, &release_context);
    built.bumps = bumps;
    built.deploy_resources = detect_deploy_resources(&mut client, &built);
    built.profile = Some(profile);
    let logins = match &opts.authors {
        AuthorFilter::Only(_) => selecting,
        other => author_filter::resolve(&other.effective(&cfg), &mut client)?,
    };
    if !logins.is_empty() {
        plan::hide_other_authors(&mut built, &logins);
    }
    if opts.check_feeds {
        built.stats.feed_checks = feed::annotate(&mut built, &mut feed::Checker::new());
    }
    built.refresh_stages(&cfg.merge.deploy_slots);
    let usage = client.usage();
    built.stats.requests = client.requests();
    built.stats.core_requests = usage.core;
    built.stats.graphql_requests = usage.graphql;
    built.stats.search_requests = usage.search;
    built.stats.not_modified = usage.not_modified;
    built.stats.cache_hits = client.cache_hits();
    built.stats.cache_misses = client.cache_misses();
    client.set_live(true);
    built.stats.elapsed_ms = elapsed::millis(started.elapsed());
    drop(progress);
    Ok((built, client, cfg))
}
