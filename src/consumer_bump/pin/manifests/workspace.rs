use super::super::{directory, sibling};
use ::toml::de::DeTable;
use anyhow::{Context, Result, bail};
use globset::GlobBuilder;
use std::collections::BTreeSet;

use crate::feed::Ecosystem;

fn glob_matches(workspace: &DeTable<'_>, setting: &str, relative: &str) -> Result<bool> {
    let Some(patterns) = workspace.get(setting) else {
        return Ok(false);
    };
    let list = patterns
        .get_ref()
        .as_array()
        .context("workspace members and exclusions must be lists of paths")?;
    for entry in list {
        let raw = entry
            .get_ref()
            .as_str()
            .context("workspace member must be a path")?;
        let pattern = raw.trim_start_matches("./").trim_end_matches('/');
        if GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()?
            .compile_matcher()
            .is_match(relative)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn explicit_root(directory: &str, workspace: &str) -> Result<String> {
    let mut parts: Vec<_> = directory.split('/').filter(|s| !s.is_empty()).collect();
    if workspace.starts_with('/') || workspace.contains(['\\', ':']) {
        bail!("review the external Cargo workspace path");
    }
    for part in workspace.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts
                    .pop()
                    .context("Cargo workspace root is outside the repository")?;
            }
            _ => parts.push(part),
        }
    }
    parts.push("Cargo.toml");
    Ok(parts.join("/"))
}

fn workspace<'a, 'i>(root: &'a DeTable<'i>, ecosystem: Ecosystem) -> Option<&'a DeTable<'i>> {
    let scope = match ecosystem {
        Ecosystem::Cargo => root,
        Ecosystem::Pypi => root
            .get("tool")?
            .get_ref()
            .get("uv")?
            .get_ref()
            .as_table()?,
        _ => return None,
    };
    scope.get("workspace")?.get_ref().as_table()
}

pub fn check_workspace_sources(
    ecosystem: Ecosystem,
    package: &str,
    path: &str,
    original: &str,
    tree: &[String],
    mut read: impl FnMut(&str) -> Result<String>,
) -> Result<()> {
    let filename = match ecosystem {
        Ecosystem::Cargo => "Cargo.toml",
        Ecosystem::Pypi => "pyproject.toml",
        _ => return Ok(()),
    };
    let local = DeTable::parse(original).map_err(|_| anyhow::anyhow!("cannot parse {path}"))?;
    if workspace(local.get_ref(), ecosystem).is_some() {
        return Ok(());
    }
    let directory = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let explicit = (ecosystem == Ecosystem::Cargo)
        .then(|| {
            local
                .get_ref()
                .get("package")?
                .get_ref()
                .get("workspace")?
                .get_ref()
                .as_str()
        })
        .flatten()
        .map(|dir| explicit_root(directory, dir))
        .transpose()?;
    let mut candidates = Vec::new();
    if let Some(candidate) = &explicit {
        if !tree.contains(candidate) {
            bail!("review the unavailable Cargo workspace {candidate}");
        }
        check_manifest_only(ecosystem, tree, &[(candidate.clone(), String::new())])
            .map_err(anyhow::Error::msg)?;
        candidates.push(candidate.clone());
    } else {
        for (end, _) in path.rmatch_indices('/') {
            let parent = path[..end].rsplit_once('/').map_or("", |(p, _)| p);
            let candidate = if parent.is_empty() {
                filename.to_owned()
            } else {
                format!("{parent}/{filename}")
            };
            if tree.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    for candidate in candidates {
        let text = read(&candidate)?;
        let root = DeTable::parse(&text)
            .map_err(|_| anyhow::anyhow!("cannot parse possible workspace root {candidate}"))?;
        let Some(ws) = workspace(root.get_ref(), ecosystem) else {
            if explicit.is_some() {
                bail!("review the explicit Cargo workspace in {candidate}");
            }
            continue;
        };
        let parent = candidate.strip_suffix(filename).unwrap_or_default();
        let relative = directory.strip_prefix(parent).unwrap_or(directory);
        if explicit.is_none()
            && (glob_matches(ws, "exclude", relative)?
                || (ecosystem == Ecosystem::Pypi && !glob_matches(ws, "members", relative)?))
        {
            continue;
        }
        let redirected = match ecosystem {
            Ecosystem::Cargo => super::toml::patched(root.get_ref(), package),
            Ecosystem::Pypi => super::toml::uv_source(root.get_ref(), package),
            _ => false,
        };
        if redirected {
            bail!(
                "{candidate} overrides the source or version of {package}; review the workspace before bumping {path}"
            );
        }
        break;
    }
    Ok(())
}

pub(crate) fn check_manifest_only(
    ecosystem: Ecosystem,
    tree: &[String],
    rows: &[(String, String)],
) -> Result<(), String> {
    let (manifest, companions): (&str, &[&str]) = match ecosystem {
        Ecosystem::Cargo => (
            "Cargo.toml",
            &["Cargo.lock", ".cargo/config.toml", ".cargo/config"],
        ),
        Ecosystem::Pypi => (
            "pyproject.toml",
            &[
                "uv.toml",
                "uv.lock",
                "poetry.lock",
                "pdm.lock",
                "Pipfile.lock",
                "pylock.toml",
            ],
        ),
        Ecosystem::Go => (
            "go.mod",
            &["go.sum", "go.work", "go.work.sum", "vendor/modules.txt"],
        ),
        _ => return Err("unsupported manifest-only ecosystem".into()),
    };
    if crate::publishers::candidate_paths(tree).1 {
        return Err("the repository exceeds the manifest scan limit; review all consumer pins before updating".into());
    }
    let paths: BTreeSet<&str> = tree.iter().map(String::as_str).collect();
    for (path, _) in rows {
        if path.rsplit('/').next() != Some(manifest) || !paths.contains(path.as_str()) {
            return Err(format!("expected {manifest} at the selected path {path}"));
        }
        let mut parent = directory(path);
        loop {
            if let Some(file) = companions
                .iter()
                .map(|name| sibling(parent, name))
                .find(|file| paths.contains(file.as_str()))
            {
                return Err(format!(
                    "review {file} before bumping {path}: the manifest-only update cannot reconcile it"
                ));
            }
            if ecosystem == Ecosystem::Pypi
                && let Some(file) = paths.iter().find(|file| {
                    let name = file.rsplit('/').next().unwrap_or_default();
                    directory(file) == parent
                        && name.starts_with("pylock.")
                        && name.ends_with(".toml")
                })
            {
                return Err(format!(
                    "review {file} before bumping {path}: the manifest-only update cannot reconcile it"
                ));
            }
            if parent.is_empty() {
                break;
            }
            parent = directory(parent);
        }
    }
    Ok(())
}
