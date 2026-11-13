use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::apply::{Gate, gate};
use crate::merge::{AuditAction, AuditEntry, Store, resolve_state_path};

#[derive(clap::Args)]
pub struct StateArgs {
    #[arg(long, global = true, help = "Override the local merge state file")]
    state: Option<PathBuf>,
    #[arg(long, global = true, help = "Profile selected as for merge")]
    profile: Option<String>,
    #[arg(long, global = true, help = "Config file selected as for merge")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    action: StateAction,
}

#[derive(clap::Subcommand)]
enum StateAction {
    #[command(about = "Read-only: inspect local merge state and audit history")]
    Show {
        #[arg(long, help = "Print the state inspection contract as JSON")]
        json: bool,
    },
    #[command(about = "Discard one pending record locally with an audit reason")]
    Discard {
        id: String,
        #[arg(long, help = "Required nonempty audit reason")]
        reason: String,
        #[arg(long, help = "Confirm without a terminal prompt")]
        yes: bool,
    },
}

pub fn show(store: &Store, profile: &str, stale_pending_hours: u64) -> Result<Value> {
    let preview = store.preview(Some(profile), stale_pending_hours);
    let mut value = serde_json::to_value(&store.state)?;
    let metadata = serde_json::to_value(&preview)?;
    for (key, field) in metadata.as_object().expect("preview is an object") {
        if key != "pending" {
            value[key] = field.clone();
        }
    }
    value["active_jobs"] = json!(store.state.active_jobs);
    for (pending, age) in value["pending"]
        .as_array_mut()
        .expect("pending is an array")
        .iter_mut()
        .zip(&preview.pending)
    {
        pending["age_seconds"] = json!(age.age_seconds);
        pending["age"] = json!(age.age);
        pending["stale"] = json!(age.stale);
    }
    Ok(value)
}

fn render(store: &Store, profile: &str, stale_pending_hours: u64) -> Result<String> {
    let mut out = store.preview(Some(profile), stale_pending_hours).render();
    writeln!(out, "schema: {}", store.state.schema)?;
    writeln!(
        out,
        "created_at: {}",
        store.state.created_at.as_deref().unwrap_or("unknown")
    )?;
    writeln!(
        out,
        "updated_at: {}",
        store.state.updated_at.as_deref().unwrap_or("unknown")
    )?;
    for pending in &store.state.pending {
        writeln!(
            out,
            "pending {}: {}@{} SHA {}",
            pending.id,
            pending.repo,
            pending.base,
            pending.sha.as_deref().unwrap_or("unknown")
        )?;
    }
    writeln!(
        out,
        "deploy PRs: {}",
        serde_json::to_string(&store.state.deploy_prs)?
    )?;
    writeln!(
        out,
        "active jobs: {}",
        serde_json::to_string(&store.state.active_jobs)?
    )?;
    writeln!(
        out,
        "audit: {}",
        serde_json::to_string_pretty(&store.state.audit)?
    )?;
    Ok(out)
}

pub fn run(args: StateArgs) -> Result<i32> {
    let interactive = std::io::stdin().is_terminal();
    if let StateAction::Discard { reason, yes, .. } = &args.action {
        if reason.trim().is_empty() {
            bail!("discard requires a nonempty --reason");
        }
        if gate(true, *yes, interactive, None) == Gate::Refuse {
            bail!("refusing to discard without a terminal: pass --yes");
        }
    }
    let resolved = super::resolve::load(args.config.as_deref(), args.profile.as_deref(), &[], &[])?;
    let path = resolve_state_path(
        args.state.as_deref(),
        resolved.cfg.merge.state_file.as_deref(),
        Some(&resolved.info.name),
    )?;
    let mut store = Store::open(Some(path.clone()))?;
    match args.action {
        StateAction::Show { json } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&show(
                        &store,
                        &resolved.info.name,
                        resolved.cfg.merge.stale_pending_hours
                    )?)?
                );
            } else {
                print!(
                    "{}",
                    render(
                        &store,
                        &resolved.info.name,
                        resolved.cfg.merge.stale_pending_hours
                    )?
                );
            }
        }
        StateAction::Discard { id, reason, yes } => {
            store.lock()?;
            let pending = store
                .state
                .pending
                .iter()
                .find(|pending| pending.id == id)
                .ok_or_else(|| anyhow!("no pending record {id} in {}", path.display()))?;
            let entry = AuditEntry::new(pending, AuditAction::Discarded, reason.trim().to_string());
            if !yes {
                eprintln!(
                    "Discard {id} from {} (repo {}, base {}, SHA {})?\nReason: {}",
                    path.display(),
                    entry.repo,
                    entry.base,
                    entry.sha.as_deref().unwrap_or("unknown"),
                    entry.reason
                );
                eprint!("Type yes: ");
                std::io::stderr().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if gate(true, false, interactive, Some(&answer)) != Gate::Proceed {
                    eprintln!("aborted, state unchanged");
                    return Ok(1);
                }
            }
            store.archive(entry)?;
            println!(
                "discarded {id}; audit saved in {}; no deploy confirmed",
                path.display()
            );
        }
    }
    Ok(0)
}
