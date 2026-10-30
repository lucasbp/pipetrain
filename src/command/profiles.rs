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

pub fn table(set: &ProfileSet, picked: Option<&(String, ProfileSource)>) -> String {
    let rows: Vec<[String; 3]> = set
        .profiles
        .iter()
        .map(|p| {
            [
                p.name.clone(),
                orgs_text(&p.config.orgs),
                folders_text(&p.folders),
            ]
        })
        .collect();
    let width = |column: usize, title: &str| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .chain([title.chars().count()])
            .max()
            .unwrap_or(0)
    };
    let (w0, w1) = (width(0, "NAME"), width(1, "ORGS"));
    let mut lines = vec![format!(
        "  {}  {}  FOLDERS",
        pad("NAME", w0),
        pad("ORGS", w1)
    )];
    for row in &rows {
        let mark = match picked {
            Some((name, _)) if name == &row[0] => "*",
            _ => " ",
        };
        lines.push(format!(
            "{mark} {}  {}  {}",
            pad(&row[0], w0),
            pad(&row[1], w1),
            row[2]
        ));
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
