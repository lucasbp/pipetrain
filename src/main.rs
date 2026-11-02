use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use pipetrain::cache::CacheMode;
use pipetrain::command::apply::{self, ApplyOptions};
use pipetrain::command::authors::AuthorFilter;
use pipetrain::command::bump::{self, BumpOptions, Level as BumpLevel};
use pipetrain::command::feed::{self, FeedOptions, WaitOptions};
use pipetrain::command::init::{self, InitOptions};
use pipetrain::command::log::{self, LogOptions};
use pipetrain::command::order::{self, OrderOptions};
use pipetrain::command::orgs::{self, OrgsOptions};
use pipetrain::command::profiles::{self, ProfilesOptions};
use pipetrain::command::update::{self, UpdateOptions};
use pipetrain::command::{PlanOptions, plan};
use pipetrain::doctor::{self, DoctorOptions};
use pipetrain::emit::Emit;
use pipetrain::feed::{DEFAULT_INTERVAL, DEFAULT_TIMEOUT, Ecosystem};
use pipetrain::update::notice::{self, Suppress};

#[derive(Parser)]
#[command(
    name = "pipetrain",
    version,
    about = "Plans the merge order of pull requests across repos"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Do not look for a newer release or print the note about one (also PIPETRAIN_NO_UPDATE_CHECK=1)"
    )]
    no_update_check: bool,

    #[arg(
        long,
        global = true,
        help = "Do not write the local usage log for this run (also PIPETRAIN_LOG=0); the log is off unless `log = true` or PIPETRAIN_LOG=1"
    )]
    no_log: bool,

    #[command(subcommand)]
    command: Command,
}

fn suppression(cli: &Cli) -> Suppress {
    let (json, emit_queue, is_update) = match &cli.command {
        Command::Plan(args) => (args.json, args.emit.is_some(), false),
        Command::Bundle(args) => (args.json, false, false),
        Command::Doctor(args) => (args.json, false, false),
        Command::Feed(args) => (args.json, false, false),
        Command::Bump(args) => (args.json, false, false),
        Command::Update(_) => (false, false, true),
        _ => (false, false, false),
    };
    Suppress {
        flag: cli.no_update_check,
        json,
        emit_queue,
        is_update,
    }
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

    #[command(
        about = "Read-only: tells whether a package version is published in its NuGet or npm feed"
    )]
    Feed(FeedArgs),

    #[command(
        about = "Read-only: shows which manifests would move to which version of a package; writes nothing"
    )]
    Bump(BumpArgs),

    #[command(
        about = "Updates this executable from the latest GitHub release; only runs when you call it, --check only reports"
    )]
    Update(UpdateArgs),

    #[command(
        about = "Read-only: summarizes the local usage log, which is off unless you turn it on"
    )]
    Log(LogArgs),

    #[command(
        about = "Read-only: serves plan, feed, bump (dry run), doctor and order_suggest as MCP tools over stdin and stdout"
    )]
    Mcp(McpArgs),
}

#[derive(clap::Args)]
struct McpArgs {
    #[arg(
        long,
        help = "Config file for every tool call (default: ./pipetrain.toml, then the user config dir)"
    )]
    config: Option<PathBuf>,
}

#[derive(clap::Args)]
struct LogArgs {
    #[arg(long, help = "Count runs, error rates by type and median time")]
    summary: bool,

    #[arg(long, requires = "summary", help = "Print the summary as JSON")]
    json: bool,
}

#[derive(clap::Args)]
struct UpdateArgs {
    #[arg(
        long,
        help = "Only report whether a newer release exists; exits 10 when there is one, 0 when not, and downloads nothing"
    )]
    check: bool,

    #[arg(
        long,
        conflicts_with = "check",
        help = "Download, verify and swap this executable (not ready, may change); without it `update` changes nothing"
    )]
    experimental_install: bool,

    #[arg(long, help = "Print the result as JSON")]
    json: bool,

    #[arg(long, help = "Do not draw the progress indicator on stderr")]
    no_progress: bool,

    #[arg(long, hide = true)]
    api_url: Option<String>,

    #[arg(long, help = "Log every API request to stderr")]
    verbose: bool,
}

#[derive(clap::Args)]
struct BumpArgs {
    #[arg(help = "Package id, e.g. Acme.Util or @acme/ui")]
    package: String,

    #[arg(
        long,
        help = "Target version; without it the latest published version is used"
    )]
    to: Option<String>,

    #[arg(
        long,
        value_enum,
        help = "nuget or npm (default: npm when the package starts with @, else nuget)"
    )]
    ecosystem: Option<Ecosystem>,

    #[arg(
        long,
        help = "GitHub org that owns the feed; required for nuget unless --registry is given"
    )]
    org: Option<String>,

    #[arg(
        long,
        help = "Feed to ask instead of the default: the base URL for npm, the service index URL for nuget"
    )]
    registry: Option<String>,

    #[arg(
        long = "repo",
        help = "Repo to scan as org/repo; repeatable, without it the exact names in the profile's [[layer]] and [[order]]"
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

    #[arg(long, help = "Print the report as JSON")]
    json: bool,

    #[arg(
        long,
        value_enum,
        default_value = "auto",
        conflicts_with = "json",
        help = "Colors: auto (only on a terminal, honors NO_COLOR), always, never"
    )]
    color: clap::ColorChoice,

    #[arg(
        long,
        value_enum,
        help = "Only count a change of this level (major, minor or patch; repeatable), the others are left alone"
    )]
    only: Vec<BumpLevel>,

    #[arg(long, help = "List only the manifests that would change")]
    changes_only: bool,

    #[arg(
        long,
        help = "Exit 1 when something would change, 0 when nothing would"
    )]
    check: bool,
}

#[derive(clap::Args)]
struct FeedArgs {
    #[arg(help = "Package id, e.g. Acme.Util or @acme/ui")]
    package: String,

    #[arg(
        long,
        help = "Version to look for; without it the latest versions are listed"
    )]
    version: Option<String>,

    #[arg(
        long,
        value_enum,
        help = "nuget or npm (default: npm when the package starts with @, else nuget)"
    )]
    ecosystem: Option<Ecosystem>,

    #[arg(
        long,
        help = "GitHub org that owns the feed; required for nuget unless --registry is given"
    )]
    org: Option<String>,

    #[arg(
        long,
        help = "Feed to ask instead of the default: the base URL for npm, the service index URL for nuget"
    )]
    registry: Option<String>,

    #[arg(long, help = "Print the result as JSON")]
    json: bool,

    #[arg(
        long,
        help = "Keep checking until --version is published or --timeout runs out; exits 0 published, 1 timed out, 2 refused (401/403), 3 feed errors until the timeout"
    )]
    wait: bool,

    #[arg(
        long,
        value_name = "SECONDS",
        help = "With --wait: seconds between checks, at least 1 (default 10)"
    )]
    interval: Option<u64>,

    #[arg(
        long,
        value_name = "SECONDS",
        help = "With --wait: give up after this many seconds, 0 checks once (default 600)"
    )]
    timeout: Option<u64>,

    #[arg(
        long,
        help = "With --wait: do not draw the progress indicator on stderr"
    )]
    no_progress: bool,
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
    #[arg(
        help = "PRs to plan as org/repo#123 or repo#123 (the org is searched in the profile's orgs); without any, today's train is discovered"
    )]
    prs: Vec<String>,

    #[arg(
        long,
        conflicts_with = "prs",
        help = "Plan every approved open PR in the orgs, whatever its branch; for days other than the train day"
    )]
    all: bool,

    #[arg(long, help = "Show only PRs by the token's own login")]
    mine: bool,

    #[arg(
        long = "author",
        value_name = "LOGIN",
        help = "Show only PRs by this author; repeatable, @me is you"
    )]
    authors: Vec<String>,

    #[arg(
        long,
        conflicts_with_all = ["mine", "authors"],
        help = "Ignore the authors set in the config and show every author"
    )]
    all_authors: bool,

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

    #[arg(
        long,
        help = "Ask the package feeds whether the version each wait line names is published, and say so on the line"
    )]
    check_feeds: bool,
}

#[derive(clap::Args)]
struct BundleArgs {
    #[arg(
        help = "PRs to bundle as org/repo#123 or repo#123 (the org is searched in the profile's orgs); without any, today's train is discovered"
    )]
    prs: Vec<String>,

    #[arg(
        long,
        conflicts_with = "prs",
        help = "Look at every approved open PR in the orgs, whatever its branch"
    )]
    all: bool,

    #[arg(long, help = "Show only PRs by the token's own login")]
    mine: bool,

    #[arg(
        long = "author",
        value_name = "LOGIN",
        help = "Show only PRs by this author; repeatable, @me is you"
    )]
    authors: Vec<String>,

    #[arg(
        long,
        conflicts_with_all = ["mine", "authors"],
        help = "Ignore the authors set in the config and show every author"
    )]
    all_authors: bool,

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

    #[arg(
        long,
        value_enum,
        default_value = "auto",
        conflicts_with = "json",
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
        authors: AuthorFilter::from_flags(args.mine, args.authors, args.all_authors),
        date: args.date,
        verbose: args.verbose,
        cache: CacheMode::from_flags(args.no_cache, args.refresh),
        progress,
        check_feeds: args.check_feeds,
    })
}

fn run_bundle(args: BundleArgs) -> Result<i32> {
    apply_color(args.color);
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
            authors: AuthorFilter::from_flags(args.mine, args.authors, args.all_authors),
            date: args.date,
            verbose: args.verbose,
            cache: CacheMode::from_flags(args.no_cache, args.refresh),
            progress,
            check_feeds: false,
        },
        apply: args.apply,
        yes: args.yes,
    })
}

fn run_bump(args: BumpArgs) -> Result<i32> {
    apply_color(args.color);
    let progress = pipetrain::progress::detect(args.no_progress, args.json, false);
    bump::run(BumpOptions {
        package: args.package,
        to: args.to,
        ecosystem: args.ecosystem,
        org: args.org,
        registry: args.registry,
        repos: args.repos,
        config: args.config,
        profile: args.profile,
        verbose: args.verbose,
        cache: CacheMode::from_flags(args.no_cache, args.refresh),
        progress,
        json: args.json,
        only: args.only,
        changes_only: args.changes_only,
        check: args.check,
    })
}

fn run_command(command: Command) -> Result<i32> {
    match command {
        Command::Bump(args) => run_bump(args),
        Command::Update(args) => update::run(UpdateOptions {
            check: args.check,
            install: args.experimental_install,
            json: args.json,
            progress: pipetrain::progress::detect(args.no_progress, args.json, false),
            api_url: args.api_url,
            verbose: args.verbose,
        }),
        Command::Log(args) => log::run(LogOptions {
            summary: args.summary,
            json: args.json,
        }),
        Command::Mcp(args) => pipetrain::mcp::serve(args.config),
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
        Command::Feed(args) => run_feed(args),
        Command::Init(args) => init::run(InitOptions {
            write: args.write,
            force: args.force,
            verbose: args.verbose,
        }),
    }
}

fn run_feed(args: FeedArgs) -> Result<i32> {
    let wait = args.wait;
    if !wait && (args.interval.is_some() || args.timeout.is_some() || args.no_progress) {
        bail!("--interval, --timeout and --no-progress need --wait");
    }
    let progress = pipetrain::progress::detect(args.no_progress, args.json, false);
    let options = FeedOptions {
        package: args.package,
        version: args.version,
        ecosystem: args.ecosystem,
        org: args.org,
        registry: args.registry,
        json: args.json,
    };
    if !wait {
        return feed::run(options);
    }
    feed::run_wait(
        options,
        WaitOptions {
            interval: Duration::from_secs(args.interval.unwrap_or(DEFAULT_INTERVAL)),
            timeout: Duration::from_secs(args.timeout.unwrap_or(DEFAULT_TIMEOUT)),
            progress,
        },
    )
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

fn command_name(command: &Command) -> Option<&'static str> {
    Some(match command {
        Command::Plan(_) => "plan",
        Command::Bundle(_) => "bundle",
        Command::Doctor(_) => "doctor",
        Command::Profiles(_) => "profiles",
        Command::Orgs(_) => "orgs",
        Command::Order(_) => "order",
        Command::Init(_) => "init",
        Command::Feed(_) => "feed",
        Command::Bump(_) => "bump",
        Command::Update(_) => "update",
        Command::Log(_) | Command::Mcp(_) => return None,
    })
}

fn config_path(command: &Command) -> Option<PathBuf> {
    match command {
        Command::Plan(args) => args.config.clone(),
        Command::Bundle(args) => args.config.clone(),
        Command::Doctor(args) => args.config.clone(),
        Command::Profiles(args) => args.config.clone(),
        Command::Orgs(args) => args.config.clone(),
        Command::Bump(args) => args.config.clone(),
        Command::Order(args) => match &args.action {
            OrderAction::Suggest(suggest) => suggest.config.clone(),
        },
        _ => None,
    }
}

struct Logging {
    name: &'static str,
    started: Instant,
}

fn start_logging(cli: &Cli) -> Option<Logging> {
    let name = command_name(&cli.command)?;
    let path = config_path(&cli.command);
    if !pipetrain::log::enabled(cli.no_log, path.as_deref()) {
        return None;
    }
    pipetrain::log::stats::take();
    Some(Logging {
        name,
        started: Instant::now(),
    })
}

fn finish_logging(logging: Option<Logging>, result: &Result<i32>) {
    let Some(logging) = logging else {
        return;
    };
    let (exit, error) = match result {
        Ok(code) => (*code, None),
        Err(err) => (1, Some(pipetrain::log::kind_of(err))),
    };
    let millis = u64::try_from(logging.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let record = pipetrain::log::build(
        logging.name,
        exit,
        error,
        millis,
        pipetrain::log::stats::take(),
    );
    pipetrain::log::write(&record);
}

fn dispatch(result: &Result<i32>) -> ExitCode {
    match result {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(u8::try_from(*code).unwrap_or(1)),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let suppress = suppression(&cli);
    let logging = start_logging(&cli);
    let result = run_command(cli.command);
    let code = dispatch(&result);
    finish_logging(logging, &result);
    notice::announce(suppress);
    code
}
