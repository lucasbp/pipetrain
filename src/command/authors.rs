use anyhow::Result;

use crate::config::Config;
use crate::github::Client;

pub const ME: &str = "@me";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AuthorFilter {
    #[default]
    Config,
    None,
    Only(Vec<String>),
}

impl AuthorFilter {
    pub fn from_flags(mine: bool, authors: Vec<String>, all_authors: bool) -> Self {
        if all_authors {
            return Self::None;
        }
        let mut list = authors;
        if mine {
            list.push(ME.to_string());
        }
        if list.is_empty() {
            Self::Config
        } else {
            Self::Only(list)
        }
    }

    pub fn effective(&self, cfg: &Config) -> Vec<String> {
        match self {
            Self::Config => cfg.authors.clone(),
            Self::None => Vec::new(),
            Self::Only(list) => list.clone(),
        }
    }
}

pub fn resolve(requested: &[String], client: &mut Client) -> Result<Vec<String>> {
    let mut viewer: Option<String> = None;
    let mut out: Vec<String> = Vec::new();
    for login in requested {
        let login = login.trim();
        let resolved = if login.eq_ignore_ascii_case(ME) {
            match &viewer {
                Some(own) => own.clone(),
                None => {
                    let own = client.viewer_login()?;
                    viewer = Some(own.clone());
                    own
                }
            }
        } else {
            login.trim_start_matches('@').to_lowercase()
        };
        if !resolved.is_empty() && !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    Ok(out)
}
