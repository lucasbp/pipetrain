use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use pipetrain::cache::CacheMode;
use pipetrain::command::apply::{self, ApplyOptions};
use pipetrain::command::init::{self, InitOptions};
use pipetrain::command::order::{self, OrderOptions};
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
        about = "Writes to GitHub only with --apply: creates the deploy branch of each bundle and retargets its PRs"
    )]
    Bundle(BundleArgs),

    #[command(
        about = "Read-only: checks the token, the API, the rate limit, the config and the orgs"
    )]
    Doctor(DoctorArgs),

    #[command(about = "Read-only: lists the profiles and which one applies from here")]
    Profiles(ProfilesArgs),

    #[command(about = "Read-only: lists the GitHub orgs you can see and which profile has them")]
    Orgs(OrgsArgs),

    #[command(
        about = "Read-only: discovers the publication order between repos from their manifests"
    )]
    Order(OrderArgs),

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
struct OrderArgs {
    #[command(subcommand)]
    action: OrderAction,
}

#[derive(Subcommand)]
enum OrderAction {
    #[command(about = "Read-only: prints an [[order]] block ready to paste into the config")]
    Suggest(SuggestArgs),
}

#[derive(clap::Args)]
struct SuggestArgs {
    #[arg(
        value_name = "REPOS",
        help = "Repos as org/repo (default: the exact names in the profile's layer and order)"
    )]
    repos: Vec<String>,

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

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,

    #[arg(
        long,
        conflicts_with = "refresh",
        help = "Do not read or write the local cache of data that never changes for a sha"
    )]
    no_cache: bool,

    #[arg(
        long,
        help = "Ignore cached data and fetch it again, then rewrite the cache"
    )]
    refresh: bool,

    #[arg(long, help = "Do not draw the progress indicator on stderr")]
    no_progress: bool,
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
        conflicts_with = "refresh",
        help = "Do not read or write the local cache of data that never changes for a sha"
    )]
    no_cache: bool,

    #[arg(
        long,
        help = "Ignore cached data and fetch it again, then rewrite the cache"
    )]
    refresh: bool,

    #[arg(long, help = "Do not draw the progress indicator on stderr")]
    no_progress: bool,

    #[arg(
        long,
        value_enum,
        default_value = "auto",
        help = "Colors: auto (only on a terminal, honors NO_COLOR), always, never"
    )]
    color: clap::ColorChoice,
}

#[derive(clap::Args)]
struct BundleArgs {
    #[arg(help = "PRs to bundle as org/repo#123; without any, today's train is discovered")]
    prs: Vec<String>,

    #[arg(
        long,
        conflicts_with = "prs",
        help = "Look at every approved open PR in the orgs, whatever its branch"
    )]
    all: bool,

    #[arg(
        long,
        help = "Create the deploy branches and retarget the PRs; without it nothing is written"
    )]
    apply: bool,

    #[arg(
        long,
        requires = "apply",
        help = "Do not ask for confirmation before writing"
    )]
    yes: bool,

    #[arg(long, help = "Print the report as JSON")]
    json: bool,

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
        conflicts_with = "refresh",
        help = "Do not read or write the local cache of data that never changes for a sha"
    )]
    no_cache: bool,

    #[arg(
        long,
        help = "Ignore cached data and fetch it again, then rewrite the cache"
    )]
    refresh: bool,

    #[arg(long, help = "Do not draw the progress indicator on stderr")]
    no_progress: bool,
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

fn run_doctor(args: DoctorArgs) -> Result<i32> {
    apply_color(args.color);
    doctor::run(DoctorOptions {
        config: args.config,
        profile: args.profile,
        orgs: args.orgs,
        json: args.json,
    })
}

fn run_plan(args: PlanArgs) -> Result<i32> {
    apply_color(args.color);
    let progress = pipetrain::progress::detect(args.no_progress, args.json, args.emit.is_some());
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
        cache: CacheMode::from_flags(args.no_cache, args.refresh),
        progress,
    })
}

fn run_bundle(args: BundleArgs) -> Result<i32> {
    let progress = pipetrain::progress::detect(args.no_progress, args.json, false);
    apply::run(ApplyOptions {
        plan: PlanOptions {
            prs: args.prs,
            all: args.all,
            json: args.json,
            emit: None,
            config: args.config,
            profile: args.profile,
            orgs: args.orgs,
            date: args.date,
            verbose: args.verbose,
            cache: CacheMode::from_flags(args.no_cache, args.refresh),
            progress,
        },
        apply: args.apply,
        yes: args.yes,
    })
}

fn run_command(command: Command) -> Result<i32> {
    match command {
        Command::Doctor(args) => run_doctor(args),
        Command::Plan(args) => run_plan(args),
        Command::Bundle(args) => run_bundle(args),
        Command::Profiles(args) => profiles::run(ProfilesOptions {
            config: args.config,
        }),
        Command::Orgs(args) => orgs::run(OrgsOptions {
            config: args.config,
            verbose: args.verbose,
        }),
        Command::Order(args) => run_order(args),
        Command::Init(args) => init::run(InitOptions {
            write: args.write,
            force: args.force,
            verbose: args.verbose,
        }),
    }
}

fn run_order(args: OrderArgs) -> Result<i32> {
    match args.action {
        OrderAction::Suggest(args) => {
            let progress = pipetrain::progress::detect(args.no_progress, false, false);
            order::run(OrderOptions {
                repos: args.repos,
                config: args.config,
                profile: args.profile,
                verbose: args.verbose,
                cache: CacheMode::from_flags(args.no_cache, args.refresh),
                progress,
            })
        }
    }
}

fn dispatch(command: Command) -> ExitCode {
    match run_command(command) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    dispatch(Cli::parse().command)
}
