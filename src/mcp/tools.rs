use std::path::PathBuf;

use clap::ValueEnum;
use serde_json::{Value, json};

use crate::cache::CacheMode;
use crate::command::authors::AuthorFilter;
use crate::command::bump::{self, BumpOptions, Level};
use crate::command::feed::{self, FeedOptions};
use crate::command::order::{self, OrderOptions};
use crate::command::{PlanOptions, plan_json};
use crate::doctor::{self, DoctorOptions};
use crate::feed::Ecosystem;

pub const NAMES: [&str; 5] = ["plan", "feed", "bump", "doctor", "order_suggest"];

pub struct Output {
    pub text: Option<String>,
    pub notes: Vec<String>,
}

pub enum Failure {
    Invalid(String),
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

fn string_prop(text: &str) -> Value {
    json!({ "type": "string", "description": text })
}

fn list_prop(text: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": text })
}

fn flag_prop(text: &str) -> Value {
    json!({ "type": "boolean", "description": text })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": true }
    })
}

pub fn definitions() -> Vec<Value> {
    let profile =
        string_prop("Profile name; default: from the folder, default_profile or the only one");
    let org = list_prop("GitHub orgs, overrides the profile's orgs");
    let ecosystem = string_prop("nuget or npm; default: npm for @scoped names, else nuget");
    let feed_org = string_prop("GitHub org that owns the feed; needed for nuget without registry");
    let registry = string_prop("Feed URL to ask instead of the default");
    vec![
        tool(
            "plan",
            "Merge order of open PRs across repos with the reason for each step (plan --json, schema 1). Read-only.",
            json!({
                "prs": list_prop("PRs as org/repo#123 or repo#123; empty discovers today's train"),
                "all": flag_prop("Every approved open PR, whatever its branch; not with prs"),
                "org": org.clone(),
                "profile": profile.clone(),
                "date": string_prop("Train date, dd-mm-yyyy or yyyy-mm-dd"),
                "check_feeds": flag_prop("Ask the package feeds whether waited versions are published"),
                "mine": flag_prop("Only PRs by the token's login")
            }),
            &[],
        ),
        tool(
            "feed",
            "Whether a package version is published in its NuGet or npm feed (feed --json, schema 1). Read-only.",
            json!({
                "package": string_prop("Package id, e.g. Acme.Util or @acme/ui"),
                "version": string_prop("Version to look for; empty lists the latest"),
                "ecosystem": ecosystem.clone(),
                "org": feed_org.clone(),
                "registry": registry.clone()
            }),
            &["package"],
        ),
        tool(
            "bump",
            "Dry run: which manifests would move to which version of a package (bump --json, schema 1). Writes nothing.",
            json!({
                "package": string_prop("Package id"),
                "to": string_prop("Target version; default: latest published"),
                "ecosystem": ecosystem,
                "org": feed_org,
                "registry": registry,
                "repos": list_prop("Repos as org/repo to scan; default: the profile's exact names"),
                "only": list_prop("Only these levels: major, minor, patch"),
                "changes_only": flag_prop("List only manifests that would change"),
                "profile": profile.clone()
            }),
            &["package"],
        ),
        tool(
            "doctor",
            "Checks the token, API, rate limit, config and orgs (doctor --json, schema 1). Read-only.",
            json!({ "profile": profile.clone(), "org": org }),
            &[],
        ),
        tool(
            "order_suggest",
            "Publication order between repos from their manifests, as an [[order]] TOML block. Text, not JSON. Read-only.",
            json!({
                "repos": list_prop("Repos as org/repo; default: the profile's exact names"),
                "profile": profile
            }),
            &[],
        ),
    ]
}

fn text_arg(args: &Value, key: &str) -> Result<Option<String>, Failure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(Failure::Invalid(format!("`{key}` must be a string"))),
    }
}

fn need(args: &Value, key: &str) -> Result<String, Failure> {
    text_arg(args, key)?.ok_or_else(|| Failure::Invalid(format!("`{key}` is required")))
}

fn flag(args: &Value, key: &str) -> Result<bool, Failure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(Failure::Invalid(format!("`{key}` must be true or false"))),
    }
}

fn list(args: &Value, key: &str) -> Result<Vec<String>, Failure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
                _ => Err(Failure::Invalid(format!(
                    "`{key}` must hold non-empty strings"
                ))),
            })
            .collect(),
        Some(_) => Err(Failure::Invalid(format!(
            "`{key}` must be a list of strings"
        ))),
    }
}

fn ecosystem(args: &Value) -> Result<Option<Ecosystem>, Failure> {
    text_arg(args, "ecosystem")?
        .map(|text| {
            Ecosystem::from_str(&text, true)
                .map_err(|_| Failure::Invalid("`ecosystem` must be nuget or npm".to_string()))
        })
        .transpose()
}

fn levels(args: &Value) -> Result<Vec<Level>, Failure> {
    list(args, "only")?
        .iter()
        .map(|text| {
            Level::from_str(text, true)
                .map_err(|_| Failure::Invalid("`only` takes major, minor or patch".to_string()))
        })
        .collect()
}

fn plan(args: &Value, config: Option<PathBuf>) -> Result<Output, Failure> {
    let prs = list(args, "prs")?;
    let all = flag(args, "all")?;
    if all && !prs.is_empty() {
        return Err(Failure::Invalid(
            "`all` cannot be used with `prs`".to_string(),
        ));
    }
    let options = PlanOptions {
        prs,
        all,
        json: true,
        emit: None,
        config,
        profile: text_arg(args, "profile")?,
        orgs: list(args, "org")?,
        authors: AuthorFilter::from_flags(flag(args, "mine")?, Vec::new(), false),
        date: text_arg(args, "date")?,
        verbose: false,
        cache: CacheMode::Use,
        progress: false,
        check_feeds: flag(args, "check_feeds")?,
    };
    Ok(Output {
        text: Some(plan_json(&options)?),
        notes: Vec::new(),
    })
}

fn feed_tool(args: &Value) -> Result<Output, Failure> {
    let options = FeedOptions {
        package: need(args, "package")?,
        version: text_arg(args, "version")?,
        ecosystem: ecosystem(args)?,
        org: text_arg(args, "org")?,
        registry: text_arg(args, "registry")?,
        json: true,
    };
    let mut notes = Vec::new();
    let rendered = feed::render(&options, &mut notes)?;
    Ok(Output {
        text: rendered.stdout,
        notes,
    })
}

fn bump_tool(args: &Value, config: Option<PathBuf>) -> Result<Output, Failure> {
    let options = BumpOptions {
        package: need(args, "package")?,
        to: text_arg(args, "to")?,
        ecosystem: ecosystem(args)?,
        org: text_arg(args, "org")?,
        registry: text_arg(args, "registry")?,
        repos: list(args, "repos")?,
        config,
        profile: text_arg(args, "profile")?,
        verbose: false,
        cache: CacheMode::Use,
        progress: false,
        json: true,
        only: levels(args)?,
        changes_only: flag(args, "changes_only")?,
        check: false,
    };
    let outcome = bump::execute(&options)?;
    let text = bump::json_text(&outcome, &options)?;
    let mut notes = outcome.before;
    notes.extend(outcome.after);
    Ok(Output { text, notes })
}

fn doctor_tool(args: &Value, config: Option<PathBuf>) -> Result<Output, Failure> {
    let options = DoctorOptions {
        config,
        profile: text_arg(args, "profile")?,
        orgs: list(args, "org")?,
        json: true,
    };
    let (text, _) = doctor::json_report(&options);
    Ok(Output {
        text: Some(text),
        notes: Vec::new(),
    })
}

fn order_tool(args: &Value, config: Option<PathBuf>) -> Result<Output, Failure> {
    let options = OrderOptions {
        repos: list(args, "repos")?,
        config,
        profile: text_arg(args, "profile")?,
        verbose: false,
        cache: CacheMode::Use,
        progress: false,
    };
    let done = order::execute(&options)?;
    let mut notes = done.before;
    notes.extend(done.suggestion.stderr);
    notes.push(done.after);
    Ok(Output {
        text: Some(done.suggestion.stdout),
        notes,
    })
}

pub fn known(name: &str) -> bool {
    NAMES.contains(&name)
}

pub fn call(name: &str, args: &Value, config: Option<PathBuf>) -> Result<Output, Failure> {
    match name {
        "plan" => plan(args, config),
        "feed" => feed_tool(args),
        "bump" => bump_tool(args, config),
        "doctor" => doctor_tool(args, config),
        "order_suggest" => order_tool(args, config),
        other => Err(Failure::Invalid(format!("unknown tool `{other}`"))),
    }
}
