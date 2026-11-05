use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use toml::{Table, Value};

use super::select::components;
use super::{IMPLICIT, OrgAliases, Profile, ProfileSet};
use crate::config::{Config, validate_date_format, validate_train_label};

pub fn parse(text: &str) -> Result<ProfileSet> {
    let mut shared: Table = text.parse().context("invalid config")?;
    let default_profile = take_default(&mut shared)?;
    let org_aliases = OrgAliases::parse(&mut shared)?;
    let tables = take_profiles(&mut shared)?;
    if tables.is_empty() {
        return flat(shared, default_profile, org_aliases);
    }
    for key in ["folders", "aliases"] {
        if shared.contains_key(key) {
            bail!("`{key}` is only allowed inside a [profile.NAME] table");
        }
    }
    let mut profiles = Vec::new();
    for (name, own) in tables {
        profiles.push(build(&name, &shared, own)?);
    }
    reject_shared_folders(&profiles)?;
    reject_alias_collisions(&profiles)?;
    org_aliases.check_profiles(&profiles)?;
    let set = ProfileSet {
        profiles,
        default_profile,
        org_aliases,
    };
    check_default(&set)?;
    Ok(set)
}

fn flat(
    shared: Table,
    default_profile: Option<String>,
    org_aliases: OrgAliases,
) -> Result<ProfileSet> {
    if shared.contains_key("aliases") {
        bail!("`aliases` is only allowed inside a [profile.NAME] table");
    }
    let config: Config = shared.try_into().context("invalid config")?;
    validate_date_format(&config.train_date_format)?;
    validate_train_label(config.train_label.as_deref())?;
    let mut set = ProfileSet::single(config);
    set.default_profile = default_profile;
    set.org_aliases = org_aliases;
    set.org_aliases.check_profiles(&set.profiles)?;
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
    let aliases = take_aliases(name, &mut own)?;
    let mut merged = shared.clone();
    merged.extend(own);
    let config: Config = merged
        .try_into()
        .with_context(|| format!("invalid config in profile `{name}`"))?;
    validate_date_format(&config.train_date_format)
        .with_context(|| format!("invalid config in profile `{name}`"))?;
    validate_train_label(config.train_label.as_deref())
        .with_context(|| format!("invalid config in profile `{name}`"))?;
    Ok(Profile {
        name: name.to_string(),
        aliases,
        folders,
        config,
    })
}

fn take_aliases(name: &str, own: &mut Table) -> Result<Vec<String>> {
    let Some(value) = own.remove("aliases") else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        bail!("`profile.{name}.aliases` must be a list of names");
    };
    let mut aliases = Vec::new();
    for item in items {
        match item {
            Value::String(alias) if !alias.trim().is_empty() => aliases.push(alias),
            _ => bail!("`profile.{name}.aliases` must hold non-empty names"),
        }
    }
    Ok(aliases)
}

fn reject_alias_collisions(profiles: &[Profile]) -> Result<()> {
    let mut owners: Vec<(String, &str, bool)> = profiles
        .iter()
        .map(|p| (p.name.to_lowercase(), p.name.as_str(), false))
        .collect();
    for profile in profiles {
        for alias in &profile.aliases {
            let key = alias.to_lowercase();
            if let Some((_, owner, is_alias)) = owners.iter().find(|(k, _, _)| *k == key) {
                if !is_alias && *owner == profile.name {
                    bail!("alias `{alias}` in profile `{owner}` repeats the profile's own name");
                }
                if *is_alias && *owner == profile.name {
                    bail!("alias `{alias}` is listed twice in profile `{owner}`");
                }
                let what = if *is_alias {
                    "an alias of"
                } else {
                    "the name of"
                };
                bail!(
                    "alias `{alias}` in profile `{}` is already {what} profile `{owner}`",
                    profile.name
                );
            }
            owners.push((key, &profile.name, true));
        }
    }
    Ok(())
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
