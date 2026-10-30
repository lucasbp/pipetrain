use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use pipetrain::command::{PlanOptions, plan};

#[derive(Parser)]
#[command(
    name = "pipetrain",
    version,
    about = "Plans the merge order of pull requests across repos"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Read-only: shows which PRs go in which order, and why")]
    Plan(PlanArgs),
}

#[derive(clap::Args)]
struct PlanArgs {
    #[arg(help = "PRs to plan as org/repo#123; without any, today's train is discovered")]
    prs: Vec<String>,

    #[arg(long, help = "Print the plan as JSON")]
    json: bool,

    #[arg(
        long,
        help = "Config file (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,

    #[arg(
        long = "org",
        help = "Org to look at, overrides the config; repeatable"
    )]
    orgs: Vec<String>,

    #[arg(long, help = "Train date as dd-mm-yyyy instead of today")]
    date: Option<String>,

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Plan(args) => plan(PlanOptions {
            prs: args.prs,
            json: args.json,
            config: args.config,
            orgs: args.orgs,
            date: args.date,
            verbose: args.verbose,
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
