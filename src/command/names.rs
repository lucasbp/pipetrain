use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Result, bail};

use crate::github::Client;
use crate::model::{self, PrKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Full(PrKey),
    Bare { repo: String, number: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    NoOrgs {
        reference: String,
    },
    NotFound {
        reference: String,
        orgs: Vec<String>,
    },
    Ambiguous {
        reference: String,
        candidates: Vec<String>,
    },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOrgs { reference } => write!(
                f,
                "`{reference}` has no org to search: pass --org, set `orgs` in pipetrain.toml, or write it as org/{reference}"
            ),
            Self::NotFound { reference, orgs } => write!(
                f,
                "`{reference}` names no repo in the orgs searched ({}): write it as org/{reference} if it lives elsewhere, or pass --org; the token may not see private repos",
                orgs.join(", ")
            ),
            Self::Ambiguous {
                reference,
                candidates,
            } => write!(
                f,
                "`{reference}` could be any of {}: write it as org/{reference} with the org you mean",
                candidates.join(", ")
            ),
        }
    }
}

impl std::error::Error for NameError {}

pub fn parse_targets(raw_prs: &[String]) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    for raw in raw_prs {
        if let Some(key) = PrKey::parse(raw) {
            targets.push(Target::Full(key));
        } else if let Some((repo, number)) = model::parse_bare(raw) {
            targets.push(Target::Bare { repo, number });
        } else {
            bail!("`{raw}` is not a PR reference; use org/repo#123 or repo#123");
        }
    }
    Ok(targets)
}

pub fn has_bare(targets: &[Target]) -> bool {
    targets.iter().any(|t| matches!(t, Target::Bare { .. }))
}

pub fn resolve(client: &mut Client, targets: Vec<Target>, orgs: &[String]) -> Result<Vec<PrKey>> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut keys = Vec::new();
    for target in targets {
        match target {
            Target::Full(key) => keys.push(key),
            Target::Bare { repo, number } => {
                let reference = format!("{repo}#{number}");
                let lower = repo.to_lowercase();
                if !found.contains_key(&lower) {
                    let slug = owner_of(client, &repo, &reference, orgs)?;
                    found.insert(lower.clone(), slug);
                }
                let slug = &found[&lower];
                let (org, name) = slug.split_once('/').unwrap_or((slug.as_str(), &repo));
                keys.push(PrKey::new(org, name, number));
            }
        }
    }
    Ok(keys)
}

fn owner_of(client: &mut Client, repo: &str, reference: &str, orgs: &[String]) -> Result<String> {
    if orgs.is_empty() {
        return Err(NameError::NoOrgs {
            reference: reference.to_string(),
        }
        .into());
    }
    let mut candidates: Vec<String> = Vec::new();
    for org in orgs {
        if let Some(slug) = client.repo_in_org(org, repo)?
            && !candidates.iter().any(|c| c.eq_ignore_ascii_case(&slug))
        {
            candidates.push(slug);
        }
    }
    match candidates.len() {
        0 => Err(NameError::NotFound {
            reference: reference.to_string(),
            orgs: orgs.to_vec(),
        }
        .into()),
        1 => Ok(candidates.remove(0)),
        _ => Err(NameError::Ambiguous {
            reference: reference.to_string(),
            candidates,
        }
        .into()),
    }
}
