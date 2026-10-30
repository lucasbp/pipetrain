use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use super::{Profile, ProfileSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileSource {
    Flag,
    Args,
    Folder,
    Default,
    Only,
}

impl ProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Args => "args",
            Self::Folder => "folder",
            Self::Default => "default",
            Self::Only => "only",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Flag => "from --profile",
            Self::Args => "from the PR arguments",
            Self::Folder => "from the working folder",
            Self::Default => "the default profile",
            Self::Only => "the only profile",
        }
    }
}

pub struct Inputs<'a> {
    pub flag: Option<&'a str>,
    pub arg_orgs: &'a [String],
    pub cwd: Option<&'a str>,
    pub case_insensitive: bool,
}

impl<'a> Inputs<'a> {
    pub fn new(flag: Option<&'a str>, arg_orgs: &'a [String], cwd: Option<&'a str>) -> Self {
        Self {
            flag,
            arg_orgs,
            cwd,
            case_insensitive: cfg!(windows),
        }
    }
}

#[derive(Debug)]
pub struct Selection<'a> {
    pub profile: &'a Profile,
    pub source: ProfileSource,
}

pub fn components(path: &str) -> Vec<String> {
    let text = path.replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    text.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn folder_depth(folder: &str, cwd: &str, case_insensitive: bool) -> Option<usize> {
    let wanted = components(folder);
    let actual = components(cwd);
    if wanted.is_empty() || wanted.len() > actual.len() {
        return None;
    }
    let same = wanted.iter().zip(&actual).all(|(a, b)| {
        if case_insensitive {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    });
    same.then_some(wanted.len())
}

pub fn select<'a>(set: &'a ProfileSet, inputs: &Inputs) -> Result<Selection<'a>> {
    if let Some(name) = inputs.flag {
        return by_flag(set, name);
    }
    let steps = [
        (by_args(set, inputs.arg_orgs), ProfileSource::Args),
        (by_folder(set, inputs), ProfileSource::Folder),
        (by_default(set), ProfileSource::Default),
        (only(set), ProfileSource::Only),
    ];
    for (found, source) in steps {
        if let Some(profile) = found {
            return Ok(Selection { profile, source });
        }
    }
    bail!(
        "no profile could be chosen; profiles:\n{}\nchoose one with --profile NAME or run from a profile folder",
        set.listing()
    )
}

fn by_flag<'a>(set: &'a ProfileSet, name: &str) -> Result<Selection<'a>> {
    let profile = set
        .find(name)
        .ok_or_else(|| anyhow!("unknown profile `{name}`; profiles:\n{}", set.listing()))?;
    Ok(Selection {
        profile,
        source: ProfileSource::Flag,
    })
}

fn owns_all(profile: &Profile, orgs: &[String]) -> bool {
    orgs.iter().all(|org| {
        profile
            .config
            .orgs
            .iter()
            .any(|own| own.eq_ignore_ascii_case(org))
    })
}

fn by_args<'a>(set: &'a ProfileSet, orgs: &[String]) -> Option<&'a Profile> {
    if orgs.is_empty() {
        return None;
    }
    let mut owners = set.profiles.iter().filter(|p| owns_all(p, orgs));
    match (owners.next(), owners.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

fn by_folder<'a>(set: &'a ProfileSet, inputs: &Inputs) -> Option<&'a Profile> {
    let cwd = inputs.cwd?;
    let mut best: Option<(usize, &Profile)> = None;
    for profile in &set.profiles {
        for folder in &profile.folders {
            let Some(depth) = folder_depth(folder, cwd, inputs.case_insensitive) else {
                continue;
            };
            if best.is_none_or(|(deepest, _)| depth > deepest) {
                best = Some((depth, profile));
            }
        }
    }
    best.map(|(_, profile)| profile)
}

fn by_default(set: &ProfileSet) -> Option<&Profile> {
    set.default_profile
        .as_deref()
        .and_then(|name| set.find(name))
}

fn only(set: &ProfileSet) -> Option<&Profile> {
    match set.profiles.as_slice() {
        [single] => Some(single),
        _ => None,
    }
}
