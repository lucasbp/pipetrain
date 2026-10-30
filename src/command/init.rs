use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::orgs;
use crate::config::ApiConfig;
use crate::github::{self, Client};

pub struct InitOptions {
    pub write: Option<Option<PathBuf>>,
    pub force: bool,
    pub verbose: bool,
}

fn bare_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn table_name(org: &str) -> String {
    if bare_key(org) {
        org.to_string()
    } else {
        format!("\"{}\"", org.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

pub fn proposal(orgs: &[String]) -> String {
    let mut text = String::from(
        "# Proposed pipetrain config: one profile per GitHub organization you can see.\n\
         # Nothing here is active until you save it as your config file.\n\
         # Fill in `folders` so pipetrain picks the profile from the folder you run it in.\n\
         # default_profile = \"name\"\n",
    );
    for org in orgs {
        text.push_str(&format!(
            "\n[profile.{}]\norgs = [\"{}\"]\n# folders = [\"/path/to/{}\"]\n",
            table_name(org),
            org.replace('\\', "\\\\").replace('"', "\\\""),
            org
        ));
    }
    text
}

pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("pipetrain").join("config.toml"))
}

pub fn write_config(path: &Path, text: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            path.display()
        );
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

fn target(write: Option<PathBuf>) -> Result<PathBuf> {
    match write.or_else(default_path) {
        Some(path) => Ok(path),
        None => bail!("no config directory found; pass --write PATH"),
    }
}

pub fn run(opts: InitOptions) -> Result<i32> {
    let mut client = Client::new(github::token()?, &ApiConfig::default(), opts.verbose);
    let visible = orgs::fetch(&mut client)?;
    let text = proposal(&visible.orgs);
    match opts.write {
        None => {
            print!("{text}");
            eprintln!("nothing was written; pass --write [PATH] to save this proposal");
        }
        Some(path) => {
            let path = target(path)?;
            write_config(&path, &text, opts.force)?;
            eprintln!("wrote {}", path.display());
        }
    }
    Ok(0)
}
