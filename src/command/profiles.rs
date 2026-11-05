use std::path::PathBuf;

use anyhow::Result;

use crate::profile::{self, Inputs, ProfileSet, ProfileSource, orgs_text};

pub struct ProfilesOptions {
    pub config: Option<PathBuf>,
}

pub fn chosen(set: &ProfileSet, cwd: Option<&str>) -> Option<(String, ProfileSource)> {
    let inputs = Inputs::new(None, &[], cwd);
    profile::select(set, &inputs)
        .ok()
        .map(|found| (found.profile.name.clone(), found.source))
}

fn folders_text(folders: &[String]) -> String {
    if folders.is_empty() {
        "(none)".to_string()
    } else {
        folders.join(", ")
    }
}

fn pad(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

fn aliases_text(aliases: &[String]) -> String {
    if aliases.is_empty() {
        "(none)".to_string()
    } else {
        aliases.join(", ")
    }
}

fn line(mark: &str, cells: &[String], widths: &[usize]) -> String {
    let last = cells.len() - 1;
    let joined: Vec<String> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            if i == last {
                cell.clone()
            } else {
                pad(cell, widths[i])
            }
        })
        .collect();
    format!("{mark} {}", joined.join("  "))
}

pub fn table(set: &ProfileSet, picked: Option<&(String, ProfileSource)>) -> String {
    let with_aliases = set.profiles.iter().any(|p| !p.aliases.is_empty());
    let titles: Vec<String> = if with_aliases {
        ["NAME", "ALIASES", "ORGS", "FOLDERS"]
    } else {
        ["NAME", "", "ORGS", "FOLDERS"]
    }
    .iter()
    .filter(|title| !title.is_empty())
    .map(|title| (*title).to_string())
    .collect();
    let rows: Vec<Vec<String>> = set
        .profiles
        .iter()
        .map(|p| {
            let mut cells = vec![p.name.clone()];
            if with_aliases {
                cells.push(aliases_text(&p.aliases));
            }
            cells.push(orgs_text(&p.config.orgs));
            cells.push(folders_text(&p.folders));
            cells
        })
        .collect();
    let widths: Vec<usize> = titles
        .iter()
        .enumerate()
        .map(|(column, title)| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain([title.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut lines = vec![line(" ", &titles, &widths)];
    for row in &rows {
        let mark = match picked {
            Some((name, _)) if name == &row[0] => "*",
            _ => " ",
        };
        lines.push(line(mark, row, &widths));
    }
    if let Some(orgs) = set.org_aliases.listing() {
        lines.push(format!("org aliases: {orgs}"));
    }
    lines.join("\n")
}

pub fn footer(picked: Option<&(String, ProfileSource)>) -> String {
    match picked {
        Some((name, source)) => format!("from here: {name} ({})", source.label()),
        None => "from here: no profile resolves; choose one with --profile NAME or run from a profile folder".to_string(),
    }
}

pub fn run(opts: ProfilesOptions) -> Result<i32> {
    let (set, path) = ProfileSet::load(opts.config.as_deref())?;
    let cwd = profile::working_dir();
    let picked = chosen(&set, cwd.as_deref());
    match path {
        Some(path) => println!("config: {}", path.display()),
        None => println!("config: none found, using built-in defaults"),
    }
    println!("{}", table(&set, picked.as_ref()));
    println!("{}", footer(picked.as_ref()));
    Ok(0)
}
