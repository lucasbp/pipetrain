use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use toml::{Table, Value};

use super::select::components;
use super::{IMPLICIT, Profile, ProfileSet};
use crate::config::{Config, validate_date_format};

pub fn parse(text: &str) -> Result<ProfileSet> {
    let mut shared: Table = text.parse().context("invalid config")?;
    let default_profile = take_default(&mut shared)?;
    let tables = take_profiles(&mut shared)?;
    if tables.is_empty() {
        return flat(shared, default_profile);
    }
    if shared.contains_key("folders") {
        bail!("`folders` is only allowed inside a [profile.NAME] table");
    }
    let mut profiles = Vec::new();
    for (name, own) in tables {
        profiles.push(build(&name, &shared, own)?);
    }
    reject_shared_folders(&profiles)?;
    let set = ProfileSet {
        profiles,
        default_profile,
    };
    check_default(&set)?;
    Ok(set)
}

fn flat(shared: Table, default_profile: Option<String>) -> Result<ProfileSet> {
    let config: Config = shared.try_into().context("invalid config")?;
    validate_date_format(&config.train_date_format)?;
    let mut set = ProfileSet::single(config);
    set.default_profile = default_profile;
    check_default(&set)?;
    Ok(set)
}

fn take_default(shared: &mut Table) -> Result<Option<String>> {
    match shared.remove("default_profile") {
        None => Ok(None),
        Some(Value::String(name)) => Ok(Some(name)),
        Some(_) => bail!("`default_profile` must be a string naming a profile"),
    }
}

fn take_profiles(shared: &mut Table) -> Result<Vec<(String, Table)>> {
    let Some(value) = shared.remove("profile") else {
        return Ok(Vec::new());
    };
    let Value::Table(all) = value else {
        bail!("`profile` must be a set of [profile.NAME] tables");
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (name, value) in all {
        if name.trim().is_empty() {
            bail!("a [profile.NAME] table has an empty name");
        }
        if !seen.insert(name.to_lowercase()) {
            bail!("profile `{name}` is defined twice (names are case-insensitive)");
        }
        let Value::Table(table) = value else {
            bail!("`profile.{name}` must be a table");
        };
        out.push((name, table));
    }
    Ok(out)
}

fn build(name: &str, shared: &Table, mut own: Table) -> Result<Profile> {
    let folders = take_folders(name, &mut own)?;
    let mut merged = shared.clone();
    merged.extend(own);
    let config: Config = merged
        .try_into()
        .with_context(|| format!("invalid config in profile `{name}`"))?;
    validate_date_format(&config.train_date_format)
        .with_context(|| format!("invalid config in profile `{name}`"))?;
    Ok(Profile {
        name: name.to_string(),
        folders,
        config,
    })
}

fn take_folders(name: &str, own: &mut Table) -> Result<Vec<String>> {
    let Some(value) = own.remove("folders") else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        bail!("`profile.{name}.folders` must be a list of folder paths");
    };
    let mut folders = Vec::new();
    for item in items {
        match item {
            Value::String(path) if !components(&path).is_empty() => folders.push(path),
            _ => bail!("`profile.{name}.folders` must hold non-empty folder paths"),
        }
    }
    Ok(folders)
}

fn reject_shared_folders(profiles: &[Profile]) -> Result<()> {
    let mut owners: Vec<(String, &str)> = Vec::new();
    for profile in profiles {
        for folder in &profile.folders {
            let key = components(folder).join("/").to_lowercase();
            if let Some((_, other)) = owners.iter().find(|(k, _)| *k == key) {
                bail!(
                    "folder `{folder}` is listed in profiles `{other}` and `{}`",
                    profile.name
                );
            }
            owners.push((key, &profile.name));
        }
    }
    Ok(())
}

fn check_default(set: &ProfileSet) -> Result<()> {
    let Some(name) = &set.default_profile else {
        return Ok(());
    };
    if set.find(name).is_some() {
        return Ok(());
    }
    let known: Vec<&str> = set.profiles.iter().map(|p| p.name.as_str()).collect();
    if known == [IMPLICIT] {
        bail!(
            "`default_profile` names `{name}`, but a flat config only has the profile `{IMPLICIT}`"
        );
    }
    bail!(
        "`default_profile` names `{name}`, which is not a profile (profiles: {})",
        known.join(", ")
    )
}
