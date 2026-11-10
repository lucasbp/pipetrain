use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

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
    let (set, config_path) = ProfileSet::load(config)?;
    let aliases = set.org_aliases.clone();
    let prs = aliases.resolve_refs(raw_prs);
    let from_args = arg_orgs(&prs);
    let cwd = profile::working_dir();
    let inputs = Inputs::new(flag, &from_args, cwd.as_deref());
    let selection = profile::select(&set, &inputs)?;
    let mut cfg = selection.profile.config.clone();
    if let Some(path) = cfg.merge.state_file.as_ref() {
        if path.as_os_str().is_empty() {
            bail!("merge.state_file is empty: choose a durable state file");
        }
        if path.is_relative() {
            let config_path = config_path
                .as_deref()
                .ok_or_else(|| anyhow!("cannot resolve merge.state_file without a config file"))?;
            let cwd = std::env::current_dir().context("cannot locate the config directory")?;
            cfg.merge.state_file = Some(configured_state_file(path, config_path, &cwd));
        }
    }
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

fn configured_state_file(path: &Path, config_file: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let config_file = if config_file.is_absolute() {
        config_file.to_path_buf()
    } else {
        cwd.join(config_file)
    };
    config_file.parent().unwrap_or(cwd).join(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_state_paths_are_relative_to_the_loaded_config_not_the_working_repo() {
        let root = std::env::temp_dir();
        let config_file = root.join("settings/config.toml");
        let relative = Path::new("queues/acme.json");
        let first = configured_state_file(relative, &config_file, &root.join("repo-a"));
        let second = configured_state_file(relative, &config_file, &root.join("repo-b"));
        assert_eq!(first, second);
        assert_eq!(first, root.join("settings/queues/acme.json"));
        assert_eq!(
            configured_state_file(relative, Path::new("config.toml"), &root),
            root.join(relative)
        );
        let absolute = root.join("fixed-state.json");
        assert_eq!(
            configured_state_file(&absolute, &config_file, &root),
            absolute
        );
    }
}
