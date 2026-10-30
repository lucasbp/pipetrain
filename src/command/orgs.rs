use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::config::ApiConfig;
use crate::github::{self, Client};
use crate::profile::ProfileSet;

const ORGS_QUERY: &str = "query($after: String) { viewer { login organizations(first: 100, after: $after) { nodes { login } pageInfo { hasNextPage endCursor } } } }";
const MAX_PAGES: usize = 10;

pub struct OrgsOptions {
    pub config: Option<PathBuf>,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub viewer: Option<String>,
    pub logins: Vec<String>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visible {
    pub viewer: Option<String>,
    pub orgs: Vec<String>,
}

pub fn parse(body: &Value) -> Page {
    let viewer = body
        .pointer("/data/viewer/login")
        .and_then(Value::as_str)
        .map(str::to_string);
    let logins = body
        .pointer("/data/viewer/organizations/nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("login").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let more = body
        .pointer("/data/viewer/organizations/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next = body
        .pointer("/data/viewer/organizations/pageInfo/endCursor")
        .and_then(Value::as_str)
        .filter(|_| more)
        .map(str::to_string);
    Page {
        viewer,
        logins,
        next,
    }
}

pub fn fetch(client: &mut Client) -> Result<Visible> {
    let mut viewer = None;
    let mut orgs: Vec<String> = Vec::new();
    let mut after = Value::Null;
    for _ in 0..MAX_PAGES {
        let body = client
            .query("organizations", ORGS_QUERY, json!({ "after": after }))?
            .into_graphql("organizations")?;
        let page = parse(&body);
        viewer = viewer.or(page.viewer);
        orgs.extend(page.logins);
        match page.next {
            Some(cursor) => after = Value::String(cursor),
            None => {
                orgs.sort_by_key(|login| login.to_lowercase());
                orgs.dedup();
                return Ok(Visible { viewer, orgs });
            }
        }
    }
    bail!("more than {MAX_PAGES} pages of organizations; stopping")
}

fn profiles_with(set: &ProfileSet, login: &str) -> Vec<String> {
    set.profiles
        .iter()
        .filter(|p| p.config.orgs.iter().any(|o| o.eq_ignore_ascii_case(login)))
        .map(|p| p.name.clone())
        .collect()
}

pub fn render(visible: &Visible, set: &ProfileSet) -> String {
    let mut lines = Vec::new();
    if let Some(login) = &visible.viewer {
        lines.push(format!("signed in as {login}"));
    }
    lines.push(format!("{} organizations visible", visible.orgs.len()));
    for login in &visible.orgs {
        let owners = profiles_with(set, login);
        if owners.is_empty() {
            lines.push(format!("  {login}  (not in any profile)"));
        } else {
            lines.push(format!("  {login}  profile: {}", owners.join(", ")));
        }
    }
    lines.join("\n")
}

pub fn run(opts: OrgsOptions) -> Result<i32> {
    let (set, _) = ProfileSet::load(opts.config.as_deref())?;
    let mut client = Client::new(github::token()?, &ApiConfig::default(), opts.verbose);
    let visible = fetch(&mut client)?;
    println!("{}", render(&visible, &set));
    Ok(0)
}
