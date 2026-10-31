use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::resolve;
use crate::config::Config;
use crate::emit::{EXIT_QUESTIONS, REPO_LEVEL};
use crate::github::{self, Client};
use crate::model::Question;
use crate::publishers;
use graph::{Analysis, Chain};

pub mod graph;

pub type Names = BTreeMap<String, String>;

pub struct OrderOptions {
    pub repos: Vec<String>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub verbose: bool,
}

#[derive(Debug, Default)]
pub struct Suggestion {
    pub stdout: String,
    pub stderr: Vec<String>,
    pub exit: i32,
}

pub fn names_of(repos: impl IntoIterator<Item = String>) -> Names {
    let mut names = Names::new();
    for repo in repos {
        names.entry(repo.to_lowercase()).or_insert(repo);
    }
    names
}

fn configured(cfg: &Config) -> (Vec<String>, Vec<String>) {
    let layered = cfg.layer.iter().flat_map(|layer| &layer.repos);
    let ordered = cfg.order.iter().flat_map(|order| &order.repos);
    let (mut globs, exact): (Vec<String>, Vec<String>) = layered
        .chain(ordered)
        .cloned()
        .partition(|repo| repo.contains(['*', '?', '[']));
    globs.sort();
    globs.dedup();
    (exact, globs)
}

fn original<'a>(names: &'a Names, slug: &'a str) -> &'a str {
    names.get(slug).map_or(slug, String::as_str)
}

fn block(chain: &Chain, names: &Names) -> String {
    let mut lines: Vec<String> = chain
        .edges
        .iter()
        .map(|edge| {
            format!(
                "# {} references {}, published by {}",
                original(names, &edge.consumer),
                edge.package,
                original(names, &edge.publisher)
            )
        })
        .collect();
    let repos: Vec<String> = chain
        .repos
        .iter()
        .map(|repo| format!("\"{}\"", original(names, repo)))
        .collect();
    lines.push("[[order]]".to_string());
    lines.push(format!("repos = [{}]", repos.join(", ")));
    lines.join("\n") + "\n"
}

fn report(analysis: &Analysis, names: &Names, questions: &[Question]) -> Suggestion {
    let mut stderr: Vec<String> = questions
        .iter()
        .map(|q| format!("warning: {}", q.message))
        .collect();
    for (a, b) in &analysis.cycles {
        stderr.push(format!(
            "? {REPO_LEVEL}: dependency cycle between {} and {}",
            original(names, a),
            original(names, b)
        ));
    }
    if analysis.chains.is_empty() && analysis.cycles.is_empty() {
        stderr.push(format!(
            "no dependency order found among {} repos",
            names.len()
        ));
    }
    let blocks: Vec<String> = analysis
        .chains
        .iter()
        .map(|chain| block(chain, names))
        .collect();
    let exit = if analysis.cycles.is_empty() {
        0
    } else {
        EXIT_QUESTIONS
    };
    Suggestion {
        stdout: blocks.join("\n"),
        stderr,
        exit,
    }
}

pub fn suggest(client: &mut Client, names: &Names) -> Result<Suggestion> {
    let mut questions = Vec::new();
    let mut manifests = Vec::new();
    for repo in names.values() {
        let sha = client.default_branch_sha(repo)?;
        manifests.extend(super::scan_repo(client, repo, &sha, &mut questions)?);
    }
    let published = publishers::collect(&manifests);
    let references = publishers::collect_references(&manifests);
    let pairs =
        publishers::dependency_pairs(|repo| names.contains_key(repo), &references, &published);
    Ok(report(&graph::analyze(&pairs), names, &questions))
}

pub fn run(opts: OrderOptions) -> Result<i32> {
    let (cfg, _) = resolve::load(opts.config.as_deref(), opts.profile.as_deref(), &[], &[])?;
    let (exact, globs) = configured(&cfg);
    let from_config = opts.repos.is_empty();
    let names = if from_config {
        names_of(exact)
    } else {
        names_of(opts.repos)
    };
    if names.is_empty() {
        bail!("no repos given and the profile has no exact repo names in [[layer]] or [[order]]");
    }
    if from_config && !globs.is_empty() {
        eprintln!("warning: ignored names with a glob: {}", globs.join(", "));
    }
    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose);
    let suggestion = suggest(&mut client, &names)?;
    print!("{}", suggestion.stdout);
    for line in &suggestion.stderr {
        eprintln!("{line}");
    }
    eprintln!("{} requests", client.requests());
    Ok(suggestion.exit)
}
