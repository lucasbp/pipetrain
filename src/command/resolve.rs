use std::path::Path;

use anyhow::Result;

use crate::config::Config;
use crate::model::PrKey;
use crate::plan::ProfileInfo;
use crate::profile::{self, Inputs, OrgAliases, ProfileSet};

pub fn arg_orgs(raw_prs: &[String]) -> Vec<String> {
    let mut orgs: Vec<String> = raw_prs
        .iter()
        .filter_map(|raw| PrKey::parse(raw))
        .map(|key| key.org)
        .collect();
    orgs.sort();
    orgs.dedup();
    orgs
}

pub struct Resolved {
    pub cfg: Config,
    pub info: ProfileInfo,
    pub prs: Vec<String>,
    pub aliases: OrgAliases,
}

pub fn load(
    config: Option<&Path>,
    flag: Option<&str>,
    raw_prs: &[String],
    orgs: &[String],
) -> Result<Resolved> {
    let (set, _) = ProfileSet::load(config)?;
    let aliases = set.org_aliases.clone();
    let prs = aliases.resolve_refs(raw_prs);
    let from_args = arg_orgs(&prs);
    let cwd = profile::working_dir();
    let inputs = Inputs::new(flag, &from_args, cwd.as_deref());
    let selection = profile::select(&set, &inputs)?;
    let mut cfg = selection.profile.config.clone();
    if !orgs.is_empty() {
        cfg.orgs = aliases.resolve_all(orgs);
    }
    let info = ProfileInfo {
        name: selection.profile.name.clone(),
        source: selection.source,
    };
    Ok(Resolved {
        cfg,
        info,
        prs,
        aliases,
    })
}
