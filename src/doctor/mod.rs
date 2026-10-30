pub mod check;
pub mod orgs;
pub mod report;

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde_json::{Value, json};

use crate::config::ApiConfig;
use crate::github::{ApiError, Client, probe_gh, resolve};
use crate::hint::Os;
use crate::profile::{self, Inputs, ProfileSet};
use check::{Check, ConfigState, ProfileState};

const VIEWER_QUERY: &str = "query { viewer { login } }";
const HEAD_CHECKS: usize = 2;

pub struct DoctorOptions {
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub orgs: Vec<String>,
    pub json: bool,
}

struct Loaded {
    state: ConfigState,
    profile: ProfileState,
    orgs: Vec<String>,
}

fn flat_listing(set: &ProfileSet) -> String {
    set.listing()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ")
}

fn pick(set: &ProfileSet, flag: Option<&str>) -> (ProfileState, Vec<String>) {
    let cwd = profile::working_dir();
    let inputs = Inputs::new(flag, &[], cwd.as_deref());
    match profile::select(set, &inputs) {
        Ok(found) => {
            let state = ProfileState::Chosen {
                name: found.profile.name.clone(),
                label: found.source.label().to_string(),
            };
            (state, found.profile.config.orgs.clone())
        }
        Err(_) => {
            let listing = flat_listing(set);
            let state = match flag {
                Some(name) => {
                    ProfileState::Rejected(format!("unknown profile `{name}`; profiles: {listing}"))
                }
                None => ProfileState::Unresolved(format!(
                    "no profile resolves from here; profiles: {listing}"
                )),
            };
            (state, Vec::new())
        }
    }
}

fn load_config(path: Option<&Path>, flag: Option<&str>) -> Loaded {
    match ProfileSet::load(path) {
        Ok((set, Some(found))) => {
            let (profile, orgs) = pick(&set, flag);
            Loaded {
                state: ConfigState::Found(found),
                profile,
                orgs,
            }
        }
        Ok((_, None)) => Loaded {
            state: ConfigState::NotFound,
            profile: ProfileState::Unknown,
            orgs: Vec::new(),
        },
        Err(err) => Loaded {
            state: ConfigState::Invalid(format!("{err:#}")),
            profile: ProfileState::Unknown,
            orgs: Vec::new(),
        },
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
    gather_with(config, None, extra_orgs)
}

pub fn gather_with(
    config: Option<&Path>,
    profile: Option<&str>,
    extra_orgs: &[String],
) -> Vec<Check> {
    let loaded = load_config(config, profile);
    let orgs = if extra_orgs.is_empty() {
        loaded.orgs.clone()
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
    checks.push(check::config(&loaded.state));
    checks.push(check::profile(&loaded.profile));
    checks.extend(org_checks);
    checks
}

pub fn run(opts: DoctorOptions) -> Result<i32> {
    let checks = gather_with(opts.config.as_deref(), opts.profile.as_deref(), &opts.orgs);
    if opts.json {
        println!("{}", report::json(&checks));
    } else {
        anstream::print!("{}", report::text(&checks));
    }
    Ok(report::exit_code(&checks))
}
