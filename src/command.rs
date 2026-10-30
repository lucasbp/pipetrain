use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{Local, NaiveDate, SecondsFormat};

use crate::config::{Config, Rules};
use crate::detect;
use crate::discovery::{self, Discovered};
use crate::github::{self, Client};
use crate::model::{Pr, PrKey, Question};
use crate::plan::{self, Gathered};
use crate::planner::{self, DepState};
use crate::publishers::{self, Publishers};
use crate::render;

const MAX_EXTERNAL_REFS: usize = 20;

pub struct PlanOptions {
    pub prs: Vec<String>,
    pub json: bool,
    pub config: Option<PathBuf>,
    pub orgs: Vec<String>,
    pub date: Option<String>,
    pub verbose: bool,
}

fn parse_date(value: Option<&str>) -> Result<NaiveDate> {
    match value {
        None => Ok(Local::now().date_naive()),
        Some(text) => NaiveDate::parse_from_str(text, "%d-%m-%Y")
            .with_context(|| format!("--date must look like 29-10-2026, got `{text}`")),
    }
}

fn discover(
    client: &mut Client,
    cfg: &Config,
    explicit: &[PrKey],
    train: &str,
) -> Result<Discovered> {
    if explicit.is_empty() {
        let mut all = Vec::new();
        for org in &cfg.orgs {
            all.extend(client.open_prs(org)?);
        }
        return Ok(discovery::discover(all, cfg, train));
    }
    let mut prs = Vec::new();
    for key in explicit {
        match client.pr(key)? {
            Some(pr) => prs.push(pr),
            None => {
                bail!("{key} was not found; check the reference and that the token can read it")
            }
        }
    }
    Ok(discovery::explicit(prs, cfg))
}

fn load_files(client: &mut Client, ready: &mut [(Pr, discovery::Scope)]) -> Result<()> {
    for (pr, _) in ready.iter_mut() {
        let (files, complete) = client.files(&pr.key)?;
        pr.files = files;
        pr.files_complete = complete;
    }
    Ok(())
}

fn scan_publishers(
    client: &mut Client,
    rules: &Rules,
    ready: &[(Pr, discovery::Scope)],
    questions: &mut Vec<Question>,
) -> Result<Publishers> {
    let wanted: BTreeSet<String> = ready
        .iter()
        .flat_map(|(pr, _)| detect::bumped_packages(&pr.files))
        .filter(|name| !rules.packages.contains_key(name))
        .collect();
    if wanted.is_empty() {
        return Ok(Publishers::new());
    }
    let mut repos: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (pr, _) in ready {
        repos
            .entry(pr.key.slug().to_lowercase())
            .or_insert_with(|| (pr.key.slug(), pr.head_sha.clone()));
    }
    let mut manifests = Vec::new();
    for (slug, sha) in repos.values() {
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
        for (path, text) in client.blobs(slug, sha, &paths)? {
            manifests.push((slug.clone(), path, text));
        }
    }
    Ok(publishers::collect(&manifests))
}

pub fn plan(opts: PlanOptions) -> Result<()> {
    let (mut cfg, _) = Config::load(opts.config.as_deref())?;
    if !opts.orgs.is_empty() {
        cfg.orgs = opts.orgs.clone();
    }
    let rules = Rules::new(&cfg)?;
    let date = parse_date(opts.date.as_deref())?;

    let mut explicit = Vec::new();
    for raw in &opts.prs {
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

    let train = if explicit.is_empty() {
        discovery::train_branch(&cfg, date)
    } else {
        "explicit PRs".to_string()
    };

    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose);
    let Discovered {
        mut ready,
        skipped,
        mut questions,
    } = discover(&mut client, &cfg, &explicit, &train)?;

    load_files(&mut client, &mut ready)?;

    let prs: Vec<Pr> = ready.iter().map(|(p, _)| p.clone()).collect();
    let mut external = BTreeMap::new();
    for key in planner::external_refs(&prs)
        .into_iter()
        .take(MAX_EXTERNAL_REFS)
    {
        let state: DepState = client.dep_state(&key)?;
        external.insert(key.lookup_id(), state);
    }

    let publishers = scan_publishers(&mut client, &rules, &ready, &mut questions)?;

    let gathered = Gathered {
        generated_at: Local::now().to_rfc3339_opts(SecondsFormat::Secs, false),
        train,
        orgs: if explicit.is_empty() {
            cfg.orgs.clone()
        } else {
            let mut orgs: Vec<String> = explicit.iter().map(|k| k.org.clone()).collect();
            orgs.sort();
            orgs.dedup();
            orgs
        },
        ready: ready
            .into_iter()
            .map(|(pr, scope)| (pr, scope.as_str().to_string()))
            .collect(),
        skipped,
        questions,
        external,
        publishers,
        max_parallel: cfg.max_parallel,
        requests: client.requests(),
        rate_remaining: client.remaining(),
    };
    let built = plan::build(&rules, gathered);

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&built)?);
    } else {
        print!("{}", render::text(&built));
    }
    Ok(())
}
