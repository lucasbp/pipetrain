use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{Local, NaiveDate, SecondsFormat};

use crate::config::{Config, Rules, render_date};
use crate::contained;
use crate::content;
use crate::detect;
use crate::discovery::{self, Discovered, Scope, Skipped};
use crate::emit::{self, Emit};
use crate::github::{self, ApiError, Client};
use crate::model::{Pr, PrKey, Question};
use crate::plan::{self, Gathered, Plan};
use crate::planner::{self, DepState};
use crate::publishers::{self, Publishers};
use crate::render;

pub mod init;
pub mod orgs;
pub mod profiles;
mod resolve;

const MAX_EXTERNAL_REFS: usize = 20;

pub struct PlanOptions {
    pub prs: Vec<String>,
    pub all: bool,
    pub json: bool,
    pub emit: Option<Emit>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub orgs: Vec<String>,
    pub date: Option<String>,
    pub verbose: bool,
}

struct Collected {
    ready: Vec<(Pr, Scope)>,
    skipped: Vec<Skipped>,
    questions: Vec<Question>,
    external: BTreeMap<String, DepState>,
    publishers: Publishers,
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
) -> Result<Discovered> {
    if explicit.is_empty() {
        let mut all = Vec::new();
        for org in &cfg.orgs {
            all.extend(client.open_prs(org)?);
        }
        if released {
            return Ok(discovery::released(all, cfg, train));
        }
        return Ok(discovery::discover(all, cfg, train));
    }
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
    Ok(discovery::explicit(prs, cfg))
}

fn load_files(client: &mut Client, ready: &mut [(Pr, Scope)]) -> Result<()> {
    for (pr, _) in ready.iter_mut() {
        let (files, complete) = client.files(&pr.key)?;
        pr.files = files;
        pr.files_complete = complete;
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

fn scan_repo(
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
) -> Result<Publishers> {
    if wanted_packages(rules, ready).is_empty() {
        return Ok(Publishers::new());
    }
    let mut manifests = Vec::new();
    for (slug, sha) in repos_to_scan(ready).values() {
        manifests.extend(scan_repo(client, slug, sha, questions)?);
    }
    Ok(publishers::collect(&manifests))
}

fn deploy_branch_for(cfg: &Config, date: Option<&str>) -> Result<String> {
    let date = match date {
        None => Local::now().date_naive(),
        Some(text) => parse_date(text, &cfg.train_date_format)?,
    };
    Ok(discovery::train_branch(cfg, date))
}

fn explicit_targets(raw_prs: &[String], cfg: &Config) -> Result<Vec<PrKey>> {
    let mut explicit = Vec::new();
    for raw in raw_prs {
        match PrKey::parse(raw) {
            Some(key) => explicit.push(key),
            None => bail!("`{raw}` is not a PR reference; use org/repo#123"),
        }
    }
    if explicit.is_empty() && cfg.orgs.is_empty() {
        bail!(
            "no orgs to look at: pass --org, name PRs as org/repo#123, or set `orgs` in pipetrain.toml"
        );
    }
    Ok(explicit)
}

fn train_label(explicit: &[PrKey], all: bool, deploy_branch: &str) -> String {
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
        max_parallel: cfg.max_parallel,
        requests: client.requests(),
        rate_remaining: client.remaining(),
    }
}

fn emit(built: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(built)?);
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
    let (cfg, profile) = resolve::load(
        opts.config.as_deref(),
        opts.profile.as_deref(),
        &opts.prs,
        &opts.orgs,
    )?;
    let rules = Rules::new(&cfg)?;
    let deploy_branch = deploy_branch_for(&cfg, opts.date.as_deref())?;
    let explicit = explicit_targets(&opts.prs, &cfg)?;
    let train = train_label(&explicit, opts.all, &deploy_branch);

    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose);
    let Discovered {
        ready,
        mut skipped,
        mut questions,
    } = discover(&mut client, &cfg, &explicit, &deploy_branch, opts.all)?;

    let (mut ready, merged) = contained::filter(&mut client, ready, &deploy_branch)?;
    skipped.extend(merged);

    load_files(&mut client, &mut ready)?;

    let (ready, same) = content::filter(&mut client, ready, &deploy_branch)?;
    skipped.extend(same);

    let external = external_states(&mut client, &ready)?;
    let publishers = scan_publishers(&mut client, &rules, &ready, &mut questions)?;

    let collected = Collected {
        ready,
        skipped,
        questions,
        external,
        publishers,
    };
    let gathered = into_gathered(&cfg, &explicit, train, deploy_branch, &client, collected);
    let mut built = plan::build(&rules, gathered);
    built.profile = Some(profile);
    deliver(&built, opts.json, opts.emit)
}
