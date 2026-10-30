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

    #[arg(
        long,
        conflicts_with = "prs",
        help = "Plan every approved open PR in the orgs, whatever its branch; for days other than the train day"
    )]
    all: bool,

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

    #[arg(
        long,
        help = "Train date instead of today: the config's train_date_format (default dd-mm-yyyy) or ISO yyyy-mm-dd"
    )]
    date: Option<String>,

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,

    #[arg(
        long,
        value_enum,
        default_value = "auto",
        help = "Colors: auto (only on a terminal, honors NO_COLOR), always, never"
    )]
    color: clap::ColorChoice,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Plan(args) => {
            anstream::ColorChoice::write_global(match args.color {
                clap::ColorChoice::Always => anstream::ColorChoice::Always,
                clap::ColorChoice::Never => anstream::ColorChoice::Never,
                clap::ColorChoice::Auto => anstream::ColorChoice::Auto,
            });
            plan(PlanOptions {
                prs: args.prs,
                all: args.all,
                json: args.json,
                config: args.config,
                orgs: args.orgs,
                date: args.date,
                verbose: args.verbose,
            })
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
