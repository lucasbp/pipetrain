use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;

mod parse;
mod select;

pub use select::{Inputs, ProfileSource, Selection, folder_depth, select};

pub const IMPLICIT: &str = "default";

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub folders: Vec<String>,
    pub config: Config,
}

#[derive(Debug, Clone)]
pub struct ProfileSet {
    pub profiles: Vec<Profile>,
    pub default_profile: Option<String>,
}

impl ProfileSet {
    pub fn parse(text: &str) -> Result<Self> {
        parse::parse(text)
    }

    pub fn single(config: Config) -> Self {
        Self {
            profiles: vec![Profile {
                name: IMPLICIT.to_string(),
                folders: Vec::new(),
                config,
            }],
            default_profile: None,
        }
    }

    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        if let Some(path) = explicit {
            return Ok((Self::read(path)?, Some(path.to_path_buf())));
        }
        for path in Config::search_paths() {
            if path.is_file() {
                return Ok((Self::read(&path)?, Some(path)));
            }
        }
        Ok((Self::single(Config::default()), None))
    }

    fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn find(&self, name: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn listing(&self) -> String {
        self.profiles
            .iter()
            .map(|p| format!("  {}  orgs: {}", p.name, orgs_text(&p.config.orgs)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn into_single_config(self) -> Result<Config> {
        if self.profiles.len() != 1 {
            bail!(
                "the config defines {} profiles; choose one with a profile name",
                self.profiles.len()
            );
        }
        Ok(self
            .profiles
            .into_iter()
            .next()
            .map(|p| p.config)
            .unwrap_or_default())
    }
}

pub fn working_dir() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub fn orgs_text(orgs: &[String]) -> String {
    if orgs.is_empty() {
        "(none)".to_string()
    } else {
        orgs.join(", ")
    }
}
