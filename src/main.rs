use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use pipetrain::command::init::{self, InitOptions};
use pipetrain::command::orgs::{self, OrgsOptions};
use pipetrain::command::profiles::{self, ProfilesOptions};
use pipetrain::command::{PlanOptions, plan};
use pipetrain::doctor::{self, DoctorOptions};
use pipetrain::emit::Emit;

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

    #[command(
        about = "Read-only: checks the token, the API, the rate limit, the config and the orgs"
    )]
    Doctor(DoctorArgs),

    #[command(about = "Read-only: lists the profiles and which one applies from here")]
    Profiles(ProfilesArgs),

    #[command(about = "Read-only: lists the GitHub orgs you can see and which profile has them")]
    Orgs(OrgsArgs),

    #[command(
        about = "Prints a proposed config with one profile per visible org; writes only with --write"
    )]
    Init(InitArgs),
}

#[derive(clap::Args)]
struct ProfilesArgs {
    #[arg(
        long,
        help = "Config file (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,
}

#[derive(clap::Args)]
struct OrgsArgs {
    #[arg(
        long,
        help = "Config file (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,
}

#[derive(clap::Args)]
struct InitArgs {
    #[arg(
        long,
        num_args = 0..=1,
        value_name = "PATH",
        help = "Save the proposal (default path: the user config dir); without this nothing is written"
    )]
    write: Option<Option<PathBuf>>,

    #[arg(
        long,
        requires = "write",
        help = "Overwrite the file if it already exists"
    )]
    force: bool,

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,
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
        value_enum,
        conflicts_with = "json",
        help = "Print only a queue of `merge:org/repo#N` lines for a runner; exit 3 if questions are open"
    )]
    emit: Option<Emit>,

    #[arg(
        long,
        help = "Config file (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,

    #[arg(
        long,
        help = "Profile to use; without it the folder, the default or the only profile decides"
    )]
    profile: Option<String>,

    #[arg(
        long = "org",
        help = "Org to look at, overrides the profile's orgs; repeatable"
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

#[derive(clap::Args)]
struct DoctorArgs {
    #[arg(
        long,
        help = "Config file (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,

    #[arg(
        long,
        help = "Profile to check; without it the folder, the default or the only profile decides"
    )]
    profile: Option<String>,

    #[arg(
        long = "org",
        help = "Org to check, overrides the profile's orgs; repeatable"
    )]
    orgs: Vec<String>,

    #[arg(long, help = "Print the checks as JSON")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "auto",
        conflicts_with = "json",
        help = "Colors: auto (only on a terminal, honors NO_COLOR), always, never"
    )]
    color: clap::ColorChoice,
}

fn apply_color(choice: clap::ColorChoice) {
    anstream::ColorChoice::write_global(match choice {
        clap::ColorChoice::Always => anstream::ColorChoice::Always,
        clap::ColorChoice::Never => anstream::ColorChoice::Never,
        clap::ColorChoice::Auto => anstream::ColorChoice::Auto,
    });
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Doctor(args) => {
            apply_color(args.color);
            doctor::run(DoctorOptions {
                config: args.config,
                profile: args.profile,
                orgs: args.orgs,
                json: args.json,
            })
        }
        Command::Plan(args) => {
            apply_color(args.color);
            plan(PlanOptions {
                prs: args.prs,
                all: args.all,
                json: args.json,
                emit: args.emit,
                config: args.config,
                profile: args.profile,
                orgs: args.orgs,
                date: args.date,
                verbose: args.verbose,
            })
        }
        Command::Profiles(args) => profiles::run(ProfilesOptions {
            config: args.config,
        }),
        Command::Orgs(args) => orgs::run(OrgsOptions {
            config: args.config,
            verbose: args.verbose,
        }),
        Command::Init(args) => init::run(InitOptions {
            write: args.write,
            force: args.force,
            verbose: args.verbose,
        }),
    };
    match result {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
