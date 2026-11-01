use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::resolve;
use crate::cache::{Cache, CacheMode};
use crate::config::Config;
use crate::elapsed;
use crate::emit::{EXIT_QUESTIONS, REPO_LEVEL};
use crate::github::{self, ApiError, Client};
use crate::model::Question;
use crate::progress::{self, Progress};
use crate::publishers;
use graph::{Analysis, Chain};

pub mod graph;

pub type Names = BTreeMap<String, String>;

pub struct OrderOptions {
    pub repos: Vec<String>,
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub verbose: bool,
    pub cache: CacheMode,
    pub progress: bool,
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

type Manifests = Vec<(String, String, String)>;

fn read_repo(
    client: &mut Client,
    repo: &str,
    questions: &mut Vec<Question>,
) -> Result<Option<Manifests>> {
    let read = client
        .default_branch_sha(repo)
        .and_then(|sha| super::scan_repo(client, repo, &sha, questions));
    match read {
        Ok(manifests) => Ok(Some(manifests)),
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ApiError>(),
        Some(ApiError::NotFound { .. })
    )
}

pub fn suggest(client: &mut Client, names: &Names) -> Result<Suggestion> {
    let mut questions = Vec::new();
    let mut manifests = Vec::new();
    let mut warnings = Vec::new();
    let mut readable = Names::new();
    progress::phase("reading manifests");
    for (index, (slug, repo)) in names.iter().enumerate() {
        match read_repo(client, repo, &mut questions)? {
            Some(found) => {
                manifests.extend(found);
                readable.insert(slug.clone(), repo.clone());
            }
            None => warnings.push(format!(
                "warning: skipped {repo}: GitHub answered not found, check the name in the config"
            )),
        }
        progress::count(index + 1, names.len());
    }
    if readable.is_empty() {
        bail!("no repo could be read: every name was not found, check the names in the config");
    }
    let published = publishers::collect(&manifests);
    let references = publishers::collect_references(&manifests);
    let pairs =
        publishers::dependency_pairs(|repo| readable.contains_key(repo), &references, &published);
    let mut suggestion = report(&graph::analyze(&pairs), &readable, &questions);
    suggestion.stderr.splice(0..0, warnings);
    Ok(suggestion)
}

pub fn run(opts: OrderOptions) -> Result<i32> {
    let started = Instant::now();
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
    let mut client = Client::new(github::token()?, &cfg.api, opts.verbose)
        .with_cache(Cache::at_default(opts.cache));
    let progress = Progress::start(opts.progress);
    let suggestion = suggest(&mut client, &names)?;
    drop(progress);
    print!("{}", suggestion.stdout);
    for line in &suggestion.stderr {
        eprintln!("{line}");
    }
    eprintln!(
        "{}",
        requests_line(client.requests(), client.cache_hits(), started.elapsed())
    );
    Ok(suggestion.exit)
}

fn requests_line(requests: u32, cache_hits: u32, elapsed: Duration) -> String {
    let took = elapsed::format(elapsed);
    if cache_hits > 0 {
        format!("{requests} requests in {took}, {cache_hits} from cache")
    } else {
        format!("{requests} requests in {took}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_line_shows_the_time_and_the_cache_hits() {
        let took = Duration::from_millis(3_200);
        assert_eq!(requests_line(9, 0, took), "9 requests in 3.2 s");
        assert_eq!(
            requests_line(9, 4, took),
            "9 requests in 3.2 s, 4 from cache"
        );
    }
}
