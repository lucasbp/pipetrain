use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

use crate::model::{ChangedFile, Pr};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub orgs: Vec<String>,
    pub max_parallel: usize,
    pub train_prefix: String,
    pub ignore_branch_prefixes: Vec<String>,
    pub allow_missing_checks: bool,
    pub checks_optional_repos: Vec<String>,
    pub layer: Vec<LayerConfig>,
    pub deploy: DeployConfig,
    pub targets: BTreeMap<String, String>,
    pub packages: BTreeMap<String, String>,
    pub api: ApiConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerConfig {
    pub name: String,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeployConfig {
    pub ddl_globs: Vec<String>,
    pub infra_repos: Vec<String>,
    pub infra_globs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiConfig {
    pub max_requests: u32,
    pub max_per_minute: u32,
    pub min_remaining: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            orgs: Vec::new(),
            max_parallel: 4,
            train_prefix: "deploy/".to_string(),
            ignore_branch_prefixes: vec!["dependabot/".to_string(), "renovate/".to_string()],
            allow_missing_checks: false,
            checks_optional_repos: Vec::new(),
            layer: Vec::new(),
            deploy: DeployConfig::default(),
            targets: BTreeMap::new(),
            packages: BTreeMap::new(),
            api: ApiConfig::default(),
        }
    }
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            ddl_globs: vec![
                "**/*.sql".to_string(),
                "**/migrations/**".to_string(),
                "**/migration/**".to_string(),
            ],
            infra_repos: Vec::new(),
            infra_globs: vec!["**/*.tf".to_string()],
        }
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            max_requests: 300,
            max_per_minute: 100,
            min_remaining: 200,
        }
    }
}

impl Config {
    pub fn allows_missing_checks(&self, slug: &str) -> bool {
        self.allow_missing_checks
            || self.checks_optional_repos.iter().any(|pattern| {
                GlobBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .map(|g| g.compile_matcher().is_match(slug))
                    .unwrap_or(false)
            })
    }

    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("invalid config")
    }

    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        if let Some(path) = explicit {
            return Ok((Self::read(path)?, Some(path.to_path_buf())));
        }
        for path in Self::search_paths() {
            if path.is_file() {
                return Ok((Self::read(&path)?, Some(path)));
            }
        }
        Ok((Self::default(), None))
    }

    pub fn search_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("pipetrain.toml")];
        if let Some(dir) = dirs::config_dir() {
            paths.push(dir.join("pipetrain").join("config.toml"));
        }
        paths
    }

    fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GateKind {
    Ddl,
    Infra,
}

pub struct Rules {
    layers: Vec<(String, GlobSet)>,
    ddl: GlobSet,
    infra_globs: GlobSet,
    infra_repos: HashSet<String>,
    targets: HashMap<String, String>,
    pub packages: HashMap<String, String>,
}

fn build_set(patterns: &[String], what: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob: Glob = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("invalid glob `{pattern}` in {what}"))?;
        builder.add(glob);
    }
    builder
        .build()
        .with_context(|| format!("invalid globs in {what}"))
}

impl Rules {
    pub fn new(cfg: &Config) -> Result<Self> {
        let mut layers = Vec::new();
        for layer in &cfg.layer {
            if layer.name.trim().is_empty() {
                bail!("a [[layer]] has an empty name");
            }
            layers.push((
                layer.name.clone(),
                build_set(&layer.repos, &format!("layer `{}`", layer.name))?,
            ));
        }
        Ok(Self {
            layers,
            ddl: build_set(&cfg.deploy.ddl_globs, "deploy.ddl_globs")?,
            infra_globs: build_set(&cfg.deploy.infra_globs, "deploy.infra_globs")?,
            infra_repos: cfg
                .deploy
                .infra_repos
                .iter()
                .map(|r| r.to_lowercase())
                .collect(),
            targets: cfg
                .targets
                .iter()
                .map(|(k, v)| (k.to_lowercase(), v.clone()))
                .collect(),
            packages: cfg
                .packages
                .iter()
                .map(|(k, v)| (k.to_lowercase(), v.to_lowercase()))
                .collect(),
        })
    }

    pub fn layer_of(&self, slug: &str) -> Option<(usize, &str)> {
        self.layers
            .iter()
            .enumerate()
            .find(|(_, (_, set))| set.is_match(slug))
            .map(|(rank, (name, _))| (rank, name.as_str()))
    }

    pub fn gate_of(&self, pr: &Pr) -> Option<(GateKind, String)> {
        if let Some(path) = first_match(&self.ddl, &pr.files) {
            return Some((GateKind::Ddl, path));
        }
        if self.infra_repos.contains(&pr.key.slug().to_lowercase()) {
            return Some((
                GateKind::Infra,
                "repo is listed in deploy.infra_repos".to_string(),
            ));
        }
        first_match(&self.infra_globs, &pr.files).map(|path| (GateKind::Infra, path))
    }

    pub fn target_of(&self, pr: &Pr) -> String {
        let slug = pr.key.slug();
        let group = self
            .targets
            .get(&slug.to_lowercase())
            .cloned()
            .unwrap_or(slug);
        format!("{}@{}", group, pr.base_ref)
    }
}

fn first_match(set: &GlobSet, files: &[ChangedFile]) -> Option<String> {
    files
        .iter()
        .find(|f| set.is_match(&f.path))
        .map(|f| f.path.clone())
}
