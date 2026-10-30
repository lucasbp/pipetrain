pub mod check;
pub mod orgs;
pub mod report;

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde_json::{Value, json};

use crate::config::{ApiConfig, Config};
use crate::github::{ApiError, Client, probe_gh, resolve};
use crate::hint::Os;
use check::{Check, ConfigState};

const VIEWER_QUERY: &str = "query { viewer { login } }";
const HEAD_CHECKS: usize = 2;

pub struct DoctorOptions {
    pub config: Option<PathBuf>,
    pub orgs: Vec<String>,
    pub json: bool,
}

fn load_config(path: Option<&Path>) -> (ConfigState, Vec<String>) {
    match Config::load(path) {
        Ok((cfg, Some(found))) => (ConfigState::Found(found), cfg.orgs),
        Ok((_, None)) => (ConfigState::NotFound, Vec::new()),
        Err(err) => (ConfigState::Invalid(format!("{err:#}")), Vec::new()),
    }
}

fn probe_api(client: &mut Client) -> Result<Value, ApiError> {
    client
        .query("viewer", VIEWER_QUERY, json!({}))?
        .into_graphql("viewer")
}

fn probe_rate(client: &mut Client) -> Result<Value, ApiError> {
    client.get("/rate_limit")?.into_body("rate limit")
}

fn permissive(orgs: usize) -> ApiConfig {
    ApiConfig {
        max_requests: u32::try_from(10 + 2 * orgs).unwrap_or(u32::MAX),
        max_per_minute: 1000,
        min_remaining: 0,
    }
}

fn skipped(reason: &str, orgs: &[String], with_api: bool) -> Vec<Check> {
    let mut out = Vec::new();
    if with_api {
        out.push(Check::skipped("api", reason));
    }
    out.push(Check::skipped("rate", reason));
    out.extend(orgs::skipped(orgs));
    out
}

pub fn remote(mut client: Client, orgs: &[String]) -> Vec<Check> {
    let api = probe_api(&mut client);
    let mut checks = vec![check::api(&api)];
    if api.is_err() {
        checks.extend(skipped("the API check failed", orgs, false));
        return checks;
    }
    let rate = probe_rate(&mut client);
    checks.push(check::rate(&rate, Utc::now().timestamp()));
    checks.extend(orgs::checks(&mut client, orgs));
    checks
}

pub fn gather(config: Option<&Path>, extra_orgs: &[String]) -> Vec<Check> {
    let (state, configured) = load_config(config);
    let orgs = if extra_orgs.is_empty() {
        configured
    } else {
        extra_orgs.to_vec()
    };
    let gh = probe_gh();
    let gh_state = gh.state();
    let found = resolve(gh, std::env::var("GH_TOKEN").ok());
    let mut checks = vec![check::token(&found, gh_state, Os::current())];
    let mut rest = match found {
        Ok(lookup) => {
            let client = Client::new(lookup.into_token(), &permissive(orgs.len()), false);
            remote(client, &orgs)
        }
        Err(_) => skipped("no token", &orgs, true),
    };
    let org_checks = rest.split_off(HEAD_CHECKS);
    checks.extend(rest);
    checks.push(check::config(&state));
    checks.extend(org_checks);
    checks
}

pub fn run(opts: DoctorOptions) -> Result<i32> {
    let checks = gather(opts.config.as_deref(), &opts.orgs);
    if opts.json {
        println!("{}", report::json(&checks));
    } else {
        anstream::print!("{}", report::text(&checks));
    }
    Ok(report::exit_code(&checks))
}
