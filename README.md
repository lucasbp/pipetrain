# pipetrain

pipetrain is a command-line tool that plans the order in which pull requests should merge across several repositories. It reads GitHub, builds a dependency graph from what it finds, and prints the order: what can merge at the same time, what has to wait for what, and why.

It is read-only by default. It never pushes or comments, and it stores nothing but a cache, the resume state of `merge` and, only if you turn it on, a [local usage log](#local-usage-log). Only two commands write to GitHub, both only with `--apply`: `pipetrain bundle --apply` creates a deploy branch, changes the base of the PRs bundled into it and, with `--open-pr`, opens the deploy PR, and `pipetrain merge --apply` merges the PRs of the plan one at a time, waiting for each deploy.

## Requirements

- **No Git needed.** pipetrain talks to the GitHub API over HTTPS. It never clones, fetches or runs `git`.
- **A GitHub token**, from one of two places: the GitHub CLI (`gh`, optional) once you ran `gh auth login`, or the `GH_TOKEN` environment variable. You need only one of them.
- **Rust only to build from source.** A prebuilt binary needs nothing else. Building needs a recent stable Rust toolchain (the crate uses edition 2024).

Not sure your setup is right? Run `pipetrain doctor` (see [Check your setup](#check-your-setup-doctor)). It checks the token, the API, your rate limit, the config and each org, and says how to fix what is wrong.

## Install

Pick one. Every prebuilt option comes from a [GitHub Release](https://github.com/lucasbp/pipetrain/releases) and is checked against the `SHA256SUMS` file in it.

### Install script

Both scripts install into a folder you own, without admin rights or `sudo`, and stop without installing anything if the SHA-256 does not match. They never edit your `PATH`: they print the line to add.

Linux and macOS (`~/.local/bin`):

```
curl -fsSL https://raw.githubusercontent.com/lucasbp/pipetrain/main/install.sh | sh
```

Windows (`%LOCALAPPDATA%\Programs\pipetrain`):

```
irm https://raw.githubusercontent.com/lucasbp/pipetrain/main/install.ps1 | iex
```

To read the script first, download it and run it. Set `PIPETRAIN_VERSION` (for example `0.2.0`) to pin a version and `PIPETRAIN_INSTALL_DIR` to choose the folder (`-Version` and `-InstallDir` in the PowerShell script).

### Download by hand

Take the archive for your system from the release page, check it against `SHA256SUMS`, and put `pipetrain` (`pipetrain.exe`) somewhere on your `PATH`.

| System | Asset |
|---|---|
| Windows x86_64 | `pipetrain-vX.Y.Z-x86_64-pc-windows-msvc.zip` |
| Linux x86_64 | `pipetrain-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 | `pipetrain-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` |
| macOS Apple silicon | `pipetrain-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `pipetrain-vX.Y.Z-x86_64-apple-darwin.tar.gz` |

The Linux binaries are static (musl), so they run on any distro. Each file also has a build provenance attestation: `gh attestation verify <file> --repo lucasbp/pipetrain`.

### Debian, Ubuntu, Fedora, Red Hat

The release carries a `.deb` and an `.rpm` for x86_64 and aarch64:

```
sudo apt install ./pipetrain_X.Y.Z-1_amd64.deb
sudo dnf install ./pipetrain-X.Y.Z-1.x86_64.rpm
```

These install to `/usr/bin`: update them by installing the newer package.

### From source

See [Requirements](#requirements).

```
cargo build --release
```

The binary is `target\release\pipetrain.exe` (`target/release/pipetrain` outside Windows). To put it on your `PATH`:

```
cargo install --path .
```

### Update

`pipetrain update --check` asks GitHub for the latest release and tells you whether it is newer than the one you run. It only reads: it downloads nothing and changes nothing. Exit code 0 means you are up to date, 10 means a newer release exists, 1 means an error. `--json` prints `schema`, `current`, `latest`, `update_available` and `updated`.

`pipetrain update` without a flag does nothing yet and exits 1: reinstall with `install.sh` or `install.ps1` instead. The in-place installer exists behind `--experimental-install` (not ready, may change): it downloads the asset of your platform, checks its SHA-256 against the release's `SHA256SUMS` and only then swaps the executable. Any failed check leaves the old executable in place.

When you run a command in an interactive terminal, pipetrain may print one line on stderr: `pipetrain X.Y.Z available, run pipetrain update --check`. It asks GitHub at most once every 24 hours (3 second timeout, any failure is silent) and keeps only the version and the time in the cache folder, never a token. The line never appears with `--json`, `--emit queue`, `CI=true`, when stdout or stderr is redirected, or in `update` itself. Turn it off with `--no-update-check` or `PIPETRAIN_NO_UPDATE_CHECK=1`.

An executable that came from a package manager (`.deb`, `.rpm`, `cargo install`) is better updated by that tool.

## Authentication

pipetrain asks the GitHub CLI for a token (`gh auth token`) and uses it if the command succeeds. Otherwise it falls back to the `GH_TOKEN` environment variable. With neither, it stops with an error that says how to get one for your system.

You do not need `gh`. Without it, create a fine-grained token with read-only access (Pull requests: read, Contents: read, Metadata: read; [`pipetrain bundle --apply`](#applying-the-bundle-suggestion) needs write on Contents and Pull requests) at <https://github.com/settings/personal-access-tokens/new> and set it for the session:

```powershell
$env:GH_TOKEN = "<your-token>"
```

```sh
export GH_TOKEN="<your-token>"
```

A classic token is not needed. Keep the token out of files and shell history where you can.

The token is used in memory for the run. It is never written to disk, to the config file or to the output, and `pipetrain doctor` never prints it either. It needs to be able to read the pull requests, checks and file contents of the repositories you plan. When an org enforces SAML SSO, the token must also be authorized for that org, or GitHub hides its private repos.

## Check your setup (doctor)

```
pipetrain doctor [--config <path>] [--profile <name>] [--org <org>]... [--json] [--color auto|always|never]
```

`doctor` runs seven read-only checks, one line each. It sends only `GET` requests and GraphQL queries, installs nothing, changes nothing and never prints the token.

| Check | What it looks at |
|---|---|
| `token` | Whether a token was found, and where (`gh auth token` or `GH_TOKEN`). |
| `api` | Whether GitHub accepts the token: a GraphQL `viewer` query. Shows the login. |
| `rate` | Your REST requests left (`GET /rate_limit`). Warns under 500. |
| `config` | Whether a config file was found, and whether it parses. |
| `configs` | Every config file found in the places searched, which one won and why. Warns when there is more than one, and says how to choose on purpose (`--config`). |
| `profile` | Which [profile](#profiles) applies from here and why, or what is wrong: an unknown `--profile`, or no profile that can be chosen. |
| `org:<name>` | One line per org, from the profile's `orgs` or `--org`: visible or not. Warns when no private repos come back, which is how SAML SSO usually shows up. That is a heuristic: an org with no private repos gets the same warning. |

Each line starts with a status: `[ ok ]`, `[warn]` or `[fail]`. A `warn` or `fail` is followed by what to do about it. When the token is missing or rejected, the checks that need it are shown as `skipped`. Output from a machine that is set up (names changed):

```
[ ok ] token found a token from `gh auth token`
[ ok ] api GraphQL answered, signed in as octocat
[ ok ] rate core: 4914 of 5000 requests left; it resets in about 39 minutes
[ ok ] config loaded ./pipetrain.toml
[ ok ] configs /home/you/project/pipetrain.toml (the only one found)
[ ok ] profile default (the only profile)
[ ok ] org:acme organization is visible, 134 private repos seen
7 ok, 0 warn, 0 fail
```

Output on a machine with no token, no `gh` and no config:

```
[fail] token no GitHub token: install the GitHub CLI (winget install --id GitHub.cli) and run `gh auth login`, or set GH_TOKEN to a fine-grained read-only token; run `pipetrain doctor` for details
       Install the GitHub CLI: winget install --id GitHub.cli
       Then sign in: gh auth login
       Without gh: create a fine-grained token with read-only access to the repos or org you plan (Pull requests: read, Contents: read, Metadata: read) at https://github.com/settings/personal-access-tokens/new
       then set it: $env:GH_TOKEN = "<your-token>"
       Keep the token out of files and shell history where possible. A classic token is not needed.
[warn] api skipped: no token
[warn] rate skipped: no token
[warn] config no config file found; `plan` needs `orgs` from a config or --org
       Create ./pipetrain.toml with `orgs = ["your-org"]`, or pass --org.
[ ok ] configs no config file in the places searched: /home/you/project/pipetrain.toml, then /home/you/.config/pipetrain/config.toml
[warn] org:acme skipped: needs a working token
1 ok, 4 warn, 1 fail
```

| Exit code | Meaning |
|---|---|
| 0 | No check failed. Warnings do not change this. |
| 1 | At least one check failed. |

Colors follow `plan`: `--color auto` (the default) colors only on a terminal and honors `NO_COLOR`.

`--json` prints the same checks for scripts and never colors. `--json` and `--color` cannot be combined (exit 2). Keys are sorted alphabetically, and `fix` is `null` when there is nothing to do:

```json
{
  "checks": [
    {
      "fix": null,
      "id": "token",
      "message": "found a token from `gh auth token`",
      "status": "ok"
    },
    {
      "fix": "Create ./pipetrain.toml with `orgs = [\"your-org\"]`, or pass --org.",
      "id": "config",
      "message": "no config file found; `plan` needs `orgs` from a config or --org",
      "status": "warn"
    }
  ],
  "schema": 1
}
```

`status` is `ok`, `warn` or `fail`. The shape is versioned by `schema` (currently 1). Fields are only ever added, so read the ones you know and ignore the rest.

## Usage

```
pipetrain plan [OPTIONS] [PRS]...
```

| Command | What it does |
|---|---|
| `pipetrain plan` | Discovers today's train in the configured orgs and plans it. |
| `pipetrain plan acme/orders-api#12 acme/billing-api#3` | Plans exactly these PRs, no discovery. |
| `pipetrain plan orders-api#12 billing-api#3` | Same, with the org left out: each repo name is searched in the orgs of the active profile. See [Repo names without the org](#repo-names-without-the-org). |
| `pipetrain plan --all` | Plans every approved open PR in the configured orgs, whatever its branch. For any day, not only the train day. Cannot be combined with explicit PRs. |
| `pipetrain plan --json` | Prints the plan as JSON instead of text (contract in `docs/plan-json.md`). |
| `pipetrain plan --emit queue` | Prints only a queue of `merge:org/repo#N` lines for a runner (contract in `docs/queue-output.md`). Cannot be combined with `--json`. |
| `pipetrain bundle` | Dry run of the bundle suggestion: shows the branch it would create and the PRs it would retarget. Writes nothing. Takes the same PR arguments and options as `plan`. |
| `pipetrain bundle --apply` | Creates the deploy branch (`deploy/<date>-<base>`), retargets the bundled PRs and, with `--open-pr`, opens the deploy PR. Asks for confirmation unless `--yes` is given. See [Applying the bundle suggestion](#applying-the-bundle-suggestion). |
| `pipetrain merge` | Dry run of the merge: the steps it would take in the order of the plan. Writes nothing. Takes the same PR arguments and options as `plan`. |
| `pipetrain merge --apply` | Merges the PRs of the plan one at a time and waits for each deploy before the next. Asks for confirmation unless `--yes` is given. See [Merging the queue](#merging-the-queue). |
| `pipetrain feed <PACKAGE> [--version <V>]` | Read-only: tells whether a package version is published in its NuGet or npm feed; `--wait` blocks until it is. See [Checking a package feed](#checking-a-package-feed). |
| `pipetrain bump <PACKAGE> [--to <V>]` | Dry run only: shows which manifests of the scanned repos would move to a version of a package. Writes nothing and has no `--apply`. See [Bumping a package](#bumping-a-package-dry-run). |
| `pipetrain profiles` | Read-only: lists the profiles, their orgs and folders, and which one applies from the current folder. |
| `pipetrain orgs` | Read-only: lists the GitHub orgs your token can see and which profile lists each one. |
| `pipetrain log --summary` | Read-only: summarizes the opt-in [local usage log](#local-usage-log): runs per command, error rate by type, median duration. |
| `pipetrain init` | Prints a proposed config with one profile per visible org. Writes nothing unless `--write [PATH]` is given; an existing file is kept unless `--force` is added. |

| Option | Meaning |
|---|---|
| `--config <path>` | Use this config file. An unreadable file is an error. |
| `--profile <name>` | Profile to use. Without it the choice follows [the rules below](#profiles). |
| `--org <org>` | Org to look at. Repeatable. Overrides `orgs` of the selected profile. |
| `--date <date>` | Train date, instead of today. Accepts the date format of `train_date_format` (default `dd-mm-yyyy`, for example `--date 29-10-2026`) or ISO `yyyy-mm-dd` (`--date 2026-10-29`). |
| `--mine` | On `plan` and `bundle`. Show only PRs by the token's own login. See [Only your PRs](#only-your-prs). Cannot be combined with `--all-authors`. |
| `--author <login>` | On `plan` and `bundle`. Show only PRs by this author; repeatable, `@me` is you. Cannot be combined with `--all-authors`. |
| `--all-authors` | On `plan` and `bundle`. Ignore the authors set in the config and show every author. |
| `--verbose` | Log every API request to stderr. |
| `--no-cache` | Neither read nor write the [local cache](#local-cache). Cannot be combined with `--refresh`. |
| `--refresh` | Ignore what the cache has, fetch it again and rewrite it. Cannot be combined with `--no-cache`. |
| `--check-feeds` | Only on `plan`. Asks the package feeds whether the version each `# wait` line names is already published, and says so on the line: `(published)`, `(not yet)` or `(feed check failed: ...)`. Off by default, so without it there are no extra requests and the output is unchanged. Informational only: order, gates, pauses and the exit code never change. Needs a token that can read packages (`read:packages`) for GitHub Packages feeds. See [Checking a package feed](#checking-a-package-feed). |
| `--no-progress` | Do not draw the progress indicator. It is drawn on stderr only when stderr is a terminal and never with `--json`, `--emit queue` redirected, or `CI=true`. |
| `--emit queue` | Replaces the plan with a queue for a runner: one `merge:org/repo#N` line per ready PR on stdout, questions on stderr, exit 3 when questions are open. See [Feeding a queue runner](#feeding-a-queue-runner). Not combinable with `--json`. |
| `--apply` | Only on `bundle`. Writes to GitHub: creates the deploy branches and retargets the PRs. Without it `bundle` is a dry run. |
| `--yes` | Only on `bundle`, and only together with `--apply`. Skips the confirmation prompt. Required when stdin is not a terminal. |
| `--open-pr` | Only on `bundle`, and only together with `--apply`. After the last retarget of each bundle, opens the PR `deploy/<date>-<base>` into the base, without merging it. A deploy PR that already exists is not duplicated. Needs Pull requests: write. |
| `--color auto\|always\|never` | On `plan` and `bundle`. Colors in the text output. `auto` (the default) colors only on a terminal and honors `NO_COLOR`. `always` colors even when piped, `never` never does. `--json` is never colored. |

PR references are written as `org/repo#123`, as a PR URL, or as `repo#123` without the org. A PR that cannot be found stops the run.

### Repo names without the org

`pipetrain plan api#12 web#3` looks for each repo name in the orgs of the active profile, or in the orgs given with `--org`, which override the profile. Details in [docs/repo-names.md](docs/repo-names.md).

- The repo is found in exactly one org: that org is used, and the repo is spelled the way GitHub spells it.
- The repo is in more than one org: the run stops with an error that lists the candidates (`acme/api, globex/api`). pipetrain never picks one. Write `org/repo#123` for the one you mean.
- The repo is in none of the orgs: the run stops with an error that names the orgs searched and the next step (write `org/repo#123`, or pass `--org`). A token that cannot see private repos gets the same answer.
- `org/repo#123` and PR URLs work as before and cost no extra request.
- Cost: one `GET /repos/{org}/{repo}` per org for each distinct name, counted by the [API budget](#api-budget). The same name on several PRs is looked up once. The answer is not cached, because whether a repo exists is not immutable.
- Read-only. `--json` (schema 1) and `--emit queue` are unchanged and always carry the full `org/repo#N`.

## How the queue is discovered

The train branch is `train_prefix` plus the date, for example `deploy/29-10-2026`. The date is written with `train_date_format` (default `dd-mm-yyyy`). In each org, pipetrain searches open, non-archived PRs and keeps the ones that are:

- on the train branch (`train`), or carrying the label set in `train_label` (also `train`, see below);
- on a branch name that is open in 2 or more repos, which is how one change spans repos (`shared-branch`);
- targeting the train branch (`into-train`).

Of those, only PRs that are ready enter the plan: not draft, approved, checks green, no merge conflict. A repo with no checks at all blocks its PRs, unless `allow_missing_checks` is on or the repo matches a glob in `checks_optional_repos`. The rest are listed as `SKIPPED` with the reasons, and a few of them raise a question when they affect a ready PR. How the order is then decided, rule by rule, is in [docs/planner.md](docs/planner.md).

**Finding the train by label.** Set `train_label` in the config (for example `train_label = "train"`) and any open PR with that label joins the train with scope `train`, whatever its branch name. The match ignores case, and a PR that carries the label is never counted as a shared branch. Without `train_label` nothing changes: only the branch name finds the train. An empty value is a config error. It can be set per profile, and a profile inherits the shared value. With `--all` a labelled approved PR also gets scope `train`.

## Any day: released PRs

The default discovery looks for the train, which exists on one day of the week. On any other day, with approved PRs waiting to go out, run:

```
pipetrain plan --all
```

`--all` needs orgs, from the config or from `--org`. It takes every open, non-archived PR in those orgs and treats the approved ones as released, whatever their branch name:

- Approved and ready (not draft, checks green, no merge conflict) enters the plan with scope `released`.
- Approved but not ready goes to `SKIPPED` with the reasons, for example `checks failing: build, test` or `draft`.
- Not approved is ignored without a trace: no review, review requested or changes requested.

A PR whose head is today's train branch keeps scope `train`, and one targeting it keeps `into-train`. In this mode PRs are not grouped by shared branch, so the `sibling-not-ready` and `stacked-not-ready` questions are not raised. Everything after discovery is the same: the already-merged check, files, dependencies, packages, stacked PRs, targets, gate, layers and bundles.

The `train` line of the output reads `all released PRs`. When a plain `pipetrain plan` finds nothing, it suggests `--all`.

## Two checks before the order

**Already merged.** A ready PR whose commits are already in its base branch, or already in the train branch, has nothing left to merge. pipetrain compares each ready PR with those branches on GitHub (GraphQL, up to 20 comparisons per request). When the PR adds nothing, it goes to `SKIPPED` with the reason `already in <branch>` and stays out of the plan. A branch that does not exist counts as "not contained", and a PR with no head commit is not checked. A second check catches squash merges: when every file the PR changes already has identical content in the base or the train branch, the PR is skipped with `already in <branch> (same content)`. It does not follow renamed files and does not look at PRs with more than 100 files; those stay in the plan.

**Bundling deploys.** When 2 or more ready PRs of the same repo target the same base (for example `main`), each one is a deploy of that repo. The plan then adds a `BUNDLES` section suggesting one train branch instead: create it from the base, retarget those PRs to it, merge them there, and open a single PR from it to the base. N deploys become 1, and the `deploys` line shows both numbers. Bases that are themselves train branches are not bundled.

Both checks are read-only, and so is `plan`. The bundle is a suggestion: `plan` does not create the branch and does not retarget anything. `pipetrain bundle --apply` does the first two steps for you ([below](#applying-the-bundle-suggestion)). Why, the arithmetic and how to apply it by hand: [docs/saving-pipelines.md](docs/saving-pipelines.md).

## Applying the bundle suggestion

```
pipetrain bundle [OPTIONS] [PRS]...
pipetrain bundle --apply [--yes] [--open-pr] [OPTIONS] [PRS]...
pipetrain bundle [--color auto|always|never] [OPTIONS] [PRS]...
```

`bundle` takes the `BUNDLES` section of the plan and, when you ask, carries out its first two steps. By default it is a dry run: it reads the tip of each base branch, prints what it would do and writes nothing. `--apply` writes. `--yes` skips the confirmation prompt, and it is required when stdin is not a terminal (without it `--apply` refuses to run before it reads a token or calls GitHub).

What it does, per bundle:

1. Creates the deploy branch from the current tip of the base. The name is always `deploy/<date>-<base>` (`deploy/<date>` is built from `train_prefix` and `train_date_format`; the base is lowercase, with every character that is not a letter, digit, `.` or `_` turned into `-`), so two bases of one repo never share a branch.
2. Changes the base of each bundled PR to that branch.
3. With `--open-pr`, opens the PR `deploy/<date>-<base>` into the base, after the last retarget of that bundle. Without the flag it does not open it.

What it does not do: it never merges, never deletes a branch and never force-pushes. Step 4 of [docs/saving-pipelines.md](docs/saving-pipelines.md) (merging) stays with you, or with `pipetrain merge`.

It never reuses a deploy branch that came from another base. When the branch already exists, it must contain the tip of the base of the bundle; otherwise `bundle` stops with an error before writing anything (on a dry run too). Delete the branch or bundle on another day with `--date`.

It stops at the first failed write and never retries. The actions after the failure, including the deploy PR, are reported as `skipped`. Running it again is safe: a branch that already exists from the right base is skipped, and so is a PR already based on it, so the second run picks up where the first stopped. When the deploy branch is already the head of an open PR, no branch is created, no second deploy PR is opened and only the retargets run.

The text report is colored like the plan: `--color auto` (the default) colors only on a terminal and honors `NO_COLOR`, `always` colors when piped, `never` never does. After `--apply` it ends with a summary line such as `2 done, 1 skipped, 0 failed in 1.4 s`, counted by status and timed from the moment the writes start (after the confirmation). A dry run has no summary. `--json` is never colored and cannot be combined with `--color` (exit 2).

On a terminal, `--apply` shows a live checklist on stderr: one block per repo (`<repo>  base <base>  branch <branch>`) and one row per write, redrawn in place. A row is `·` while it waits, a spinner while it runs, `✓` with its duration when done, a dash with the reason when skipped and `✗` when it failed. When a write fails, the row turns red with GitHub's message and the rows after it show `stopped after an earlier failure`; the exit code is still 1. When the list was shown and stdout is also a terminal, stdout gets only the summary line.

The checklist is drawn only when stderr is a terminal. With `--json`, `--no-progress`, `CI=true` or a redirected stderr there is no live list, and stdout carries the full text report (or the JSON) exactly as before. Colors follow `NO_COLOR` and `--color`.

Token permissions: Contents: write to create the branch and Pull requests: write to change a base and, with `--open-pr`, to open the deploy PR. A read-only token fails on the first write with a message that says so, and nothing after it runs.

| Code | Meaning |
|---|---|
| 0 | Dry run done, or every action applied or skipped. |
| 1 | Aborted at the prompt, or an error, or a failed write. |

With `--json` the report goes to stdout. `applied` is `false` on a dry run, and `status` is `planned` for what a dry run would do and `done`, `skipped` or `failed` after `--apply`:

```json
{
  "schema": 1,
  "applied": true,
  "bundles": [
    {
      "repo": "acme/api-core",
      "base": "master",
      "branch": "deploy/30-10-2026-master",
      "actions": [
        { "kind": "create-branch", "target": "deploy/30-10-2026-master", "status": "done", "detail": "" },
        { "kind": "retarget", "target": "acme/api-core#1", "status": "done", "detail": "" },
        { "kind": "retarget", "target": "acme/api-core#2", "status": "skipped", "detail": "already based on deploy/30-10-2026-master" },
        { "kind": "open-pr", "target": "deploy/30-10-2026-master", "status": "done", "detail": "" }
      ]
    }
  ]
}
```

After applying, run `pipetrain plan` again: the retargeted PRs now sit on the deploy branch.

## Example output

An invented run for the org `acme`, shown without colors (what `--color never` prints). When a plan has a single org, the `acme/` prefix is dropped from PR names. PRs show their author as a dim `@login` right after the name:

```
pipetrain plan  (read-only, nothing was merged)
  train     deploy/29-10-2026
  orgs      acme
  found     8 ready, 3 skipped, 1 questions
  parallel  peak 3 at once (cap 4)
  deploys   8 now, 7 after bundling
  api       23 requests, 4811 left
  time      14.6 s

WHERE IT CAME FROM  how each PR got into the queue
  ● train   1 ready, 1 skipped  head is deploy/29-10-2026
  ● shared  7 ready, 1 skipped  same head branch in 2 or more repos
        feat/invoice-export  billing-api#233 · orders-web#91
        feat/invoice-totals  billing-api#230 · docs-site#5
        feat/order-status  shared-lib#7 · orders-client#12 · orders-web#88

ORDER  a step starts when every PR of the step above is merged and deployed
  pause orders-api#41 DDL  (you confirm first)
  step 1  billing-api#230 · docs-site#5 · shared-lib#7
  step 2  billing-api#233 · orders-client#12
  step 3  orders-web#91 · orders-web#88

GATE  runs first, pauses for your confirmation
    orders-api#41     [api]    train   Add order status column
        DDL (db/migrations/0042_status.sql)  every lane below waits for this

LANE 1
  s1   billing-api#230   [api]    shared  Round invoice totals to cents
  s2   billing-api#233   [api]    shared  Add invoice export endpoint
            after billing-api#230  (target: same deploy target acme/billing-api@main)
  s3   orders-web#91     [front]  shared  Show the invoice export button
            after billing-api#233  (layer: layer api goes before layer front on branch `feat/invoice-export`)

LANE 2
  s1 ? docs-site#5                shared  Document the invoice totals

LANE 3
  s1   shared-lib#7      [lib]    shared  Add order status enum
  s2   orders-client#12  [client] shared  Expose order status in the client
            after shared-lib#7  (layer: layer lib goes before layer client on branch `feat/order-status`)
  s3   orders-web#88     [front]  shared  Show order status badge
            after orders-client#12  (layer: layer client goes before layer front on branch `feat/order-status`)

BUNDLES  one deploy branch instead of N deploys (suggestion, nothing was changed)
  billing-api  2 PRs into main -> 1 deploy, saves 1
        create deploy/29-10-2026-main from main, retarget billing-api#230, billing-api#233, merge them there, then open one PR deploy/29-10-2026-main -> main

CONFLICTS  same repo, same files; the order does not change
  billing-api#230 and billing-api#233  1 file (lockfile)
        package-lock.json

SKIPPED  not ready, left out of the plan
  billing-api#228  Fix rounding in invoice PDF  [train: deploy/29-10-2026]  (already in main)
  billing-api#231  WIP retry failed charges  [shared: feat/retry-charges]  (draft)
  billing-api#234  Add retry to the webhook  [train: deploy/29-10-2026]  (checks failing: build, test)

?  QUESTIONS  the planner could not decide these
  docs-site#5  [layer-unknown]  acme/docs-site is not in any layer, so its place on branch `feat/invoice-totals` is undecided
        fix: add the repo to a [[layer]] in the config
```

How to read it:

- **WHERE IT CAME FROM**: how each PR got into the queue, with its scope and the criterion. `train`: the PR head is the train branch. `into-train`: the PR targets the train branch. `shared`: the head branch is open in 2 or more repos. `released`: approved PR on any branch, from `--all`. `explicit`: named on the command line. Each scope shows how many PRs are ready and how many were skipped, and `shared` also lists which PRs share each branch.
- **ORDER**: the whole plan in one line per step. A step starts only when every PR of the step above is merged and deployed. `pause` is the gate (DDL or infra): you confirm before anything else starts. The lanes below say why each PR is where it is.
- **GATE**: a PR that needs a human to stop and confirm first (a database change, or infrastructure). It goes before everything else and is not part of any lane, unless it must follow another PR by a package, dependency or target edge: then it is a late gate, it runs after its predecessors with the pause right before it, and `--json` marks it with `gate_late`.
- **LANE**: a chain of PRs. Lanes are independent of each other, so they can run side by side. Inside a lane, merge top to bottom.
- **s1, s2, s3**: the stage. PRs with the same stage have nothing between them and can merge at the same time.
- **after**: the PR that must merge first, the rule that says so, and the reason.
- **?**: the PR has an open question, listed at the end with a suggested fix. A question never changes the exit code, except with `--emit queue` (see [Feeding a queue runner](#feeding-a-queue-runner)).
- **[api]**, **[lib]**: the layer of the repo, from your config.
- **deploys**: how many deploys the ready PRs cause today (every PR that does not target a train branch is one), and how many remain if every bundle is applied.
- **BUNDLES**: a suggestion to merge several PRs of one repo through one train branch. Shown only when there is something to bundle. Nothing was created or retargeted.
- **CONFLICTS**: PRs of the same repo that change the same files, with the kind of file (`lockfile`, `sql`, `generated`, `other`). It is a file-overlap hint, not a merge simulation, and PRs stacked on each other are left out. Shown only when there is something to report. It never changes the order. The queue carries the same finding as `# conflict:` lines.
- **SKIPPED**: PRs that are not ready, or that are `already in` a branch. `already in main` means the PR head is already part of `main`. A PR skipped for failing checks lists their names, for example `checks failing: build, test` (at most 5, then `+N more`).

A `NOTES` section appears at the end when there is something informational, for example a `Depends-On` that points at a PR already merged.

## Feeding a queue runner

```
pipetrain plan --all --emit queue
```

stdout carries only the queue, one item per line, ready for any tool that takes `merge:` items:

```
# step 1
merge:acme/orders-api#12
# pause: confirm DDL/infra before continuing
# step 1
# lane 1
merge:acme/billing-api#3
# step 2
# lane 1
merge:acme/web#7
```

- **Items.** `merge:<org>/<repo>#<n>`, one per ready PR. Skipped PRs never appear. Lines that start with `#` are comments.
- **Order.** The plan's execution order: gate PRs first, then the `# pause: ...` line, then every other PR by stage. A late gate (a gate that must follow a PR by a package, dependency or target edge) sits among the other PRs instead, printed as `# gate: ...`, the pause line, an optional `# wait` (for example `# wait: package @acme/kit 1.1.0 from acme/kit#6 must be published before acme/web#5`), then its `merge:` line. Inside one stage the order follows your `[[layer]]` entries (the first entry goes first, for example lib, client, api, bff, front), then PR number, then repo slug; a PR with no layer comes last in its stage. The gate PRs follow the same rule among themselves. The tiebreak never moves a PR across stages or against an `after` edge. There is no blank line.
- **Bundle comments.** When the plan suggests a deploy branch for a repo, the queue opens with one `# bundle <org>/<repo>: <N> PRs into <base> could be 1 deploy (saves <N-1>): <PR ids>` line per suggestion. It is informative: the saving only happens if you create the day's deploy branch first, and the `merge:` lines do not change.
- **Wait comments.** Only a PR that bumps the version of a package that other PRs of the queue publish waits: the line `# wait: package <pkg> <version> from <publisher PRs> must be published before <consumer PR>` comes right before the consumer's `merge:` line, with all the publishers on one line. `<version>` is the version the consumer bumps to, read from the added line of the consumer PR's own manifest diff; when it cannot be read (a variable, a wildcard, a range or conflicting versions) the line is `# wait: package <pkg> from <publisher PRs> must be published before <consumer PR>`. A PR that only references the package is ordered after the publisher but does not wait, since it can use the version already in the feed. The runner should stop at the line until that version of the package is in the feed. pipetrain does not query the feed, and a runner that does not understand the line skips it like any `#` line.
- **Pause.** The runner should stop at the `# pause` line until a human confirms the database or infrastructure change. If no PR follows the gate, the line is omitted.
- **Step and lane comments.** Each PR is preceded by `# step <n>` (its stage) and, unless it is a gate PR, `# lane <n>` (its lane in the plan). They are informative and a runner skips them like any `#` line; a runner that wants to run lanes side by side can read them.
- **Lanes are flattened.** The plan runs independent lanes side by side, the queue lists them one after the other. A serial runner is correct with it, only slower than the lanes would be.
- **Questions.** Each open question goes to stderr as `? <pr>: <message>` (`? (repo): ...` when it is not about one PR), before anything else. The queue is still printed.
- **No PRs ready.** stdout stays empty and stderr says `no PRs ready`.

| Exit code | Meaning |
|---|---|
| 0 | Queue printed, no open question. |
| 1 | An error, as in any run. |
| 3 | Queue printed, and at least one question is open. Read stderr before running the queue. |

`--all`, explicit PRs, `--config`, `--org`, `--date` and `--verbose` work as usual. `--color` and `NO_COLOR` have no effect. PowerShell example:

```powershell
$items = pipetrain plan --all --emit queue | Where-Object { $_ -notmatch '^#' }; & ship-queue.ps1 @items
```

The example drops the comment lines, so it does not stop at the pause. Split the queue at the `# pause` line when the runner has to wait there. The format is documented in [docs/queue-output.md](docs/queue-output.md).

## GitHub Action

`action.yml` is a read-only composite action that installs a release binary, checks its SHA-256 against `SHA256SUMS` and runs `pipetrain plan` in your workflow:

```yaml
- uses: lucasbp/pipetrain@v0.2.0
  with:
    refs: acme/api#12 acme/web#3
    emit: queue
```

Inputs: `version`, `refs`, `all`, `profile`, `config`, `emit` (`json`, `queue` or `text`), `fail-on-questions` and `github-token`. Outputs: `queue`, `json`, `text` and `exit-code`, plus the step summary. Exit 3 (open questions) does not fail the step unless `fail-on-questions` is `true`. No step writes to the repo or to GitHub, and the default `github.token` only sees the repo the workflow runs in. The tag in the example must be a release that contains `action.yml`. Details in [docs/github-action.md](docs/github-action.md), a full workflow in [docs/examples/pipetrain-plan.yml](docs/examples/pipetrain-plan.yml).

## Configuration

pipetrain looks for a config file in this order and uses the first one it finds:

1. `--config <path>`, when given.
2. `./pipetrain.toml` in the current directory.
3. `pipetrain/config.toml` in the user config directory (`%APPDATA%` on Windows, `~/.config` on Linux, `~/Library/Application Support` on macOS).

With no file at all it runs on built-in defaults, which means you must pass `--org` or explicit PRs, because `orgs` is empty by default.

[pipetrain.example.toml](pipetrain.example.toml) lists every key with its default. Unknown keys are rejected, so a typo is an error and not a silent no-op. The layer map is the part you must write yourself: pipetrain cannot know which of your repos is a library and which is a front end. Why is explained in [docs/planner.md](docs/planner.md).

### Profiles

One file can hold several setups. Top-level keys are shared by every profile, and each `[profile.<name>]` table replaces whole top-level keys for that profile, so a profile that sets `orgs` or `layer` ignores the shared value. A file with no `[profile.*]` table is a single profile called `default`, so every existing config keeps working unchanged.

```toml
default_profile = "acme"

[profile.acme]
folders = ["/work/acme"]
orgs = ["acme"]

[profile.sandbox]
folders = ["/work/sandbox"]
orgs = ["acme-labs"]
```

The profile is chosen by the first rule that applies:

1. `--profile <name>`.
2. The orgs of the `org/repo#N` arguments, when exactly one profile lists all of them. Arguments written as `repo#N` carry no org and take no part in this choice; they are searched in the orgs of the profile that the other rules pick.
3. The current folder, when it is inside the `folders` of a profile; the deepest matching folder wins.
4. `default_profile`.
5. The only profile, when there is just one.
6. Otherwise it stops with an error that lists the profiles.

`folders` is only allowed inside a profile, paths are absolute and not expanded (no `~`), and the same folder cannot be listed in two profiles. Profiles are never merged. `--org` replaces the `orgs` of the selected profile. The text report starts with a line such as `profile acme (from the working folder)`, and `--json` carries `profile {name, source}` with `source` one of `flag`, `args`, `folder`, `default` or `only`.

`pipetrain profiles` shows what applies from the current folder, `pipetrain orgs` lists the orgs your token can see and which profile has each, and `pipetrain init` prints a proposed config with one profile per visible org. `init --write` creates the file only when it does not exist yet, unless `--force` is given.

### Only your PRs

The plan is always computed with every PR, because the order of your PRs can depend on a PR from someone else. The author filter runs at the end and only hides: the text, the queue and the exit code skip the PRs of other authors, while `--json` keeps them marked `hidden`.

```
pipetrain plan --mine
pipetrain plan --author alice --author @me
pipetrain plan --all-authors
```

`--mine` is `--author @me`, and `@me` is the login of the token. Logins are compared ignoring case. The same filter can live in the config as `authors = ["@me"]`, top level or per profile, and a profile value replaces the shared one. The precedence is the command line, then the config, then no filter; `--all-authors` forces no filter even when the config sets `authors`.

With a filter on, the text report starts with a line such as `hidden    3 PRs by others`, and a `# wait:` line of the queue still names a hidden PR when your PR has to wait for it. A hidden PR never makes `--emit queue` exit 3. `pipetrain bundle` retargets only the visible PRs, notes `N PRs by others left alone: <ids>`, and drops a bundle with none of your PRs and no existing train PR (`skipped bundle <repo>: none of its PRs is yours` on stderr). Reading the token's login costs one extra API request, and only when `@me` or `--mine` is used. Fields in the JSON are in [docs/plan-json.md](docs/plan-json.md).

### Config order

`[[order]]` pins the relative order of repos that the other rules leave open. In each `repos` list the first glob goes first. You can write several `[[order]]` tables.

```toml
[[order]]
repos = ["acme/shared-*", "acme/api-*"]
```

Globs match `org/repo` and ignore case. Two PRs matched by the same glob, or from the same repo, get no edge. The rule applies after `stacked` and before `target`, and it loses to `depends-on`, `package` and `stacked`; a lost `order` edge is reported as an `edge-conflict` question. The position of the rule is in [docs/planner.md](docs/planner.md).

You do not have to write an `[[order]]` block for every library and consumer pair. The `dependency` rule reads the manifests of the repos that have a PR in the plan (`.csproj`, `.props`, `package.json`, `Cargo.toml`, `pyproject.toml` and `go.mod`) and puts the PR of a repo that publishes a NuGet, npm, Cargo, Python or Go package before the PR of a repo that references it. A dependency from one ecosystem to another is not detectable and stays an explicit `[[order]]`. `feed`, `bump` and `plan --check-feeds` only understand NuGet and npm: a `# wait` for a Cargo, Python or Go package is looked up in the wrong feed and ends as a failed check. An explicit `[[order]]` block wins over it, and two repos that reference each other raise a `dependency-cycle` question. Details and limits are in [docs/planner.md](docs/planner.md).

To pin the discovered chain in the config, `pipetrain order suggest [REPOS...]` reads the manifests at the tip of each default branch and prints an `[[order]]` block per group of connected repos, publisher first, with one `#` line of evidence per edge. Without REPOS it uses the exact names (no glob) in the `layer` and `order` entries of the selected profile. It is read-only: it never writes the config, and it exits 3 with a `? (repo)` line when two repos reference each other. A repo name that GitHub does not find is skipped with a warning on stderr, and the run fails only when no name could be read. A dependency from one ecosystem to another (NuGet to npm, for example) is not detectable and stays explicit.

```text
$ pipetrain order suggest acme/shared acme/api
# acme/api references acme.shared, published by acme/shared
[[order]]
repos = ["acme/shared", "acme/api"]
```

`train_date_format` is how the date appears in the train branch name, as a [chrono strftime](https://docs.rs/chrono/latest/chrono/format/strftime/index.html) string. The default is `%d-%m-%Y` (`deploy/29-10-2026`); `%Y-%m-%d` gives `deploy/2026-10-29`. The config is rejected, with the key named in the error, when the format is empty, is not a valid strftime string, cannot format a plain date (for example it uses a time field), or gives the same text for two different days because it has no day field. `--date` accepts this format or ISO `yyyy-mm-dd`.

## API budget

A run spends GitHub API requests: a search per org, a few per PR, one batched comparison per 20 PRs for the "already merged" check, one request per 25 changed files for the same-content check, one tree per repo it needs to scan. Three settings under `[api]` keep that bounded:

| Key | Default | Behavior |
|---|---|---|
| `max_requests` | 300 | The run stops with an error when it reaches this many requests. |
| `max_per_minute` | 100 | The run slows down to stay under this rate. It waits, it does not fail. |
| `min_remaining` | 200 | The run stops with an error when GitHub reports fewer requests left than this. |

GitHub counts the REST API (`core`) and GraphQL separately, and the guard follows that. `max_requests` and `max_per_minute` count every request of the run, REST and GraphQL together. `min_remaining` is checked per resource, using the `x-ratelimit-resource` header of each response, so a low GraphQL budget stops the run even when `core` is full. The `api` line of the output shows the lowest remaining value seen across resources.

Redirects are not followed. GitHub answers a renamed or moved repo with a redirect, and pipetrain stops with an error saying the repo was renamed or moved, instead of silently planning against a different one. Use the new name.

`--verbose` prints each request, and each wait, to stderr. The `api` line of the output shows how many requests the run used and how many GitHub says remain. On a terminal, a small spinner on stderr shows the phase, a counter and the elapsed time while pipetrain talks to GitHub, in color unless `NO_COLOR` is set or `--color never` is used; it never touches stdout. The `time` line is the wall-clock time of the run until the data was gathered; with the cache it is shorter.

Requests that only read are retried up to 4 times, with growing waits and jitter, when GitHub answers `500`, `502`, `503`, `504` or a secondary rate limit (`Retry-After` is honored up to 60 seconds). Each retry prints one line on stderr saying what GitHub answered and for which request. An exhausted primary rate limit is reported with its reset time, not waited out, and writes (`bundle --apply`, `merge --apply`) are never retried.

## Local cache

`plan` and `order suggest` keep a small cache on disk of data that can never change for a given key, so a second run right after the first asks GitHub for much less. On a real queue of 12 PRs across 7 repos, the second run in a row went from 38 requests to 11, with the same output.

What is cached, and the key of each entry:

| Data | Key |
|---|---|
| The paths a PR changes | repo, PR number, the live tip of the base branch and the head sha. The base tip is in the key because the changed files of a PR move when the base branch moves. |
| The file tree of a repo | repo and commit sha. |
| The text of package manifests | repo and commit sha. |
| The blob id of a file at a PR head | repo and head sha. |

What is never cached: anything that can change without a new sha, such as the list of open PRs, reviews, checks, merge state, the tip of a branch, and the "already merged" comparisons. Those are fetched on every run. The token, request headers and diffs are never written either: a PR that changes a package manifest, whose diff the planner reads, is always fetched again.

Where it lives: the `pipetrain` folder inside your user cache directory (`%LOCALAPPDATA%` on Windows, `~/.cache` on Linux, `~/Library/Caches` on macOS). Set `PIPETRAIN_CACHE_DIR` to use another folder. Entries are small JSON files written atomically. Entries older than 14 days are deleted, and the oldest go first when the folder passes 32 MiB. Deleting the folder is always safe; the next run rebuilds it.

`--no-cache` skips the cache completely and `--refresh` ignores what is there and writes it again. The plan is the same with or without the cache. `stats.cache_hits` and `stats.cache_misses` in `--json` count the lookups answered from disk and those that went to GitHub; the `api` line of the text output adds `N from cache` when there were hits, and `order suggest` does the same on its request line. `stats.requests` counts only real requests.

## Checking a package feed

```
pipetrain feed <PACKAGE> [--version <V>] [--ecosystem nuget|npm] [--org <ORG>] [--registry <URL>] [--json] [--wait [--interval <S>] [--timeout <S>]]
```

`feed` answers one question: is this package, at this version, published in its feed? It is read-only: it only sends `GET` requests, writes nothing and stores nothing. It does not read a config file or call the GitHub API.

| Ecosystem | Chosen when | Feed |
|---|---|---|
| `npm` | The package starts with `@`, or `--ecosystem npm`. | `https://npm.pkg.github.com` when the package is scoped and `--org` is given, otherwise `https://registry.npmjs.org`. `--registry <BASE URL>` overrides it. The request is `GET {base}/{package}` with `/` written as `%2f`. |
| `nuget` | Everything else, or `--ecosystem nuget`. | `https://nuget.pkg.github.com/{org}/index.json`, so `--org` is required unless `--registry <SERVICE INDEX URL>` is given. pipetrain reads the `PackageBaseAddress` resource of the service index and then `{base}{lowercase id}/index.json`. |

Output:

```
Acme.Util 1.2610.2314.49: published
Acme.Util 2.0.0: not published (latest 1.2610.2314.49)
```

Without `--version` it prints the latest version and the last 5, newest first. Versions are compared by their numeric segments, so 4-part NuGet versions work, and a pre-release (`1.0.0-beta`) sorts below `1.0.0`.

`--json` prints `{"schema": 1, "package", "ecosystem", "feed", "version", "published", "latest", "versions"}`. `version` and `published` are `null` when no `--version` was given, and `versions` holds the last 5. A package the feed does not know prints nothing on stdout, so `--json` stays empty in that case; read the exit code.

Token: GitHub Packages needs one, even for public packages. pipetrain uses the same lookup as every other command (`GH_TOKEN`, then `gh auth token`) and sends it only to hosts that end in `pkg.github.com`: as a bearer token for npm and as the password of a basic-auth login for NuGet. It is never printed. The token needs the `read:packages` scope; if the feed answers 401 or 403, check that the token has it. `registry.npmjs.org`, `nuget.org` and any other host get no token.

| Code | Meaning |
|---|---|
| 0 | The version is published, or no `--version` was asked. |
| 1 | The version is not published, the package is not in the feed (`package not found in <feed>`), or an error such as a network failure (`error: ...` on stderr). |
| 2 | The feed answered 401 or 403: the token is missing or lacks `read:packages`. |

### Waiting for a version: `feed --wait`

```
pipetrain feed <PACKAGE> --version <V> --wait [--interval <S>] [--timeout <S>] [--no-progress] [--json]
```

`--wait` keeps checking the feed until the version is published or the time runs out. It is the same read-only lookup as `feed`, repeated: it sends `GET` requests only and writes nothing.

| Flag | Default | Meaning |
|---|---|---|
| `--wait` | off | Poll instead of checking once. Needs `--version`. |
| `--interval <S>` | 10 | Whole seconds between checks, at least 1. The last pause is shortened so the wait never goes past the timeout. |
| `--timeout <S>` | 600 | Whole seconds to wait in total. `0` checks once and does not wait. |
| `--no-progress` | off | No spinner. The spinner is drawn only on a terminal, and never with `--json` or `CI=true`. |

`--interval`, `--timeout` and `--no-progress` without `--wait` are an error (exit 1), and so is `--wait` without `--version`.

stdout carries the final result, once. Progress and retry messages go to stderr:

```
Acme.Util 2.0.0: published
Acme.Util 2.0.0: not published after 600s (latest 1.2610.2314.49)
```

The second line has no `(latest ...)` when the feed lists no versions. With `--json` the output is the `feed` object plus `"waited": true`, `"attempts"`, `"elapsed_seconds"` and `"timed_out"`. Schema stays 1, and those four fields appear only with `--wait`.

| Code | Meaning |
|---|---|
| 0 | The version is published. It returns as soon as it appears. |
| 1 | The timeout passed and the version is still not published, or `--wait` was used wrongly. A package that does not exist yet counts as not yet published and keeps being retried. |
| 2 | The feed answered 401 or 403. It stops at once with no retry: waiting does not fix a missing `read:packages` scope. |
| 3 | At the timeout the last attempt still failed with a network error, an HTTP error or an invalid answer. Nothing is printed on stdout. |

A network or server error in the middle of the wait is retried until the timeout, so a short outage does not fail the run. If a later attempt reaches the feed and the version is still missing, the result is 1, not 3.

In a runner:

```
pipetrain feed Acme.Util --org acme --version 2.0.0 --wait --timeout 900 --no-progress
```

The step passes when the version is published and fails on any other code, so the next job starts only once the package is available. See [docs/feed-wait.md](docs/feed-wait.md).

### In the plan: `plan --check-feeds`

```
pipetrain plan --emit queue --check-feeds
# wait: package @acme/kit 1.1.0 from acme/kit#6 must be published before acme/web#5 (not yet)
```

With `--check-feeds`, `plan` runs this same lookup once for each distinct package and version named by a `# wait` line and appends the answer to the line. The ecosystem and feed follow the table above: the org is the first publisher PR's org for NuGet, and for a scoped npm package GitHub Packages is used when a publisher PR's org matches the scope, otherwise npmjs. A failed lookup never aborts the plan or changes its exit code. See [Wait comments](docs/queue-output.md#wait-comments) and the [JSON contract](docs/plan-json.md).

## Bumping a package (dry run)

```
pipetrain bump <PACKAGE> [--to <VERSION>] [--ecosystem nuget|npm] [--org <ORG>] [--registry <URL>] [--repo <ORG/REPO>]... [--profile <NAME>] [--only major|minor|patch]... [--changes-only] [--check] [--json]
```

`bump` shows which manifests would move to a version of a package. It is a dry run and only a dry run: there is no `--apply`, and clap rejects it as an unknown option.

What it reads:

- The target version: `--to`, or the latest version in the feed, looked up exactly like `pipetrain feed` (same ecosystem rules, same `--org` and `--registry`, same token handling).
- The repos to scan: every `--repo`, or, when none is given, the exact repo names in the active profile (`[[layer]]` and `[[order]]`).
- In each repo, the manifests of the default branch for the ecosystem (`.csproj`, `.fsproj` and `.props` for NuGet, `package.json` for npm), through the same cache-aware GitHub client as `plan`. Only `GET` requests and read-only GraphQL queries.

What it never does: write a file, create a branch, commit, push, open a pull request or touch a repo. It prints what it found and stops.

Output:

```
bump Acme.Util to 2.0.0
acme/api
  src/Api/Api.csproj  Acme.Util 1.2.0 -> 2.0.0
  src/Worker/Worker.csproj  Acme.Util up to date
1 would change, 1 up to date, 0 left alone in 1 repo in 1.4 s
dry run: nothing was written
```

| Status | Meaning |
|---|---|
| `change` | The manifest references an older version than the target. It would move. |
| `up_to_date` | The manifest already has the target version. |
| `ahead` | The manifest references a newer version than the target (`ahead of target (left alone)`). |
| `range` | The manifest uses a floating range such as `1.*` or `^1.2.0`. The raw spec is shown and it is left alone. |
| `other_level` | Only with `--only`: the manifest would change, but not by one of the chosen levels (`different level (left alone)`). |

Three options narrow what is reported. None of them is needed, and without them the output and the JSON are exactly as before.

- `--only major|minor|patch` (repeatable): count as `change` only the manifests whose move to the target is of one of these levels. The level is the first number that differs from the target (`1.2.0` to `2.0.0` is major, to `1.3.0` minor, and anything after that, including a prerelease becoming final or a fourth number, is patch). The other changes become `other_level`, are left alone and count under `left alone` in the summary. The other statuses are not touched.
- `--changes-only`: list only the manifests that would change, and only the repos that have at least one. The summary line still counts everything.
- `--check`: exit 1 when at least one manifest would change, after printing the report, with `--check: N would change` on stderr. Exit 0 when nothing would change. Meant for a runner that gates on a pending bump. With `--only`, only the chosen levels count.

Repos that do not reference the package are not listed. A scanned repo name that GitHub does not know is skipped with a warning on stderr, and the scan fails only when no repo could be read. The colors follow `bundle` (`--color auto|always|never`, not allowed with `--json`), and a spinner on stderr covers the scan like it does for `plan` (`--no-progress`, `--no-cache` and `--refresh` work as there).

`--json` prints `{"schema": 1, "package", "ecosystem", "target", "repos": [{"repo", "files": [{"path", "current", "status"}]}], "dry_run": true}` with `status` one of `change`, `up_to_date`, `ahead`, `range` or `other_level`. With `--only`, the object also has `"only": [...]` with the levels chosen (the key is absent otherwise). With `--changes-only`, `files` and `repos` hold only the changes.

| Code | Meaning |
|---|---|
| 0 | The scan ran, whether or not anything would change (and, with `--check`, nothing would). |
| 1 | With `--check`: at least one manifest would change (`--check: N would change` on stderr). Also: the package is referenced nowhere (`PACKAGE is not referenced in any scanned manifest`), the feed does not know the package or the version, no repo was given or could be read, or another error (`error: ...` on stderr). Code 1 also means "not referenced", so with `--check` tell the cases apart by the message on stderr. |
| 2 | The feed answered 401 or 403: the token is missing or lacks `read:packages` (the message and code of `feed`). |

## Exit code

| Code | Meaning |
|---|---|
| 0 | A plan was generated. Skipped PRs and `?` questions do not change this. |
| 1 | An error: no token, bad config, bad `--date`, a PR not found, budget reached, GitHub failure. The message goes to stderr as `error: ...`. For `bundle` it also means the prompt was declined or a write failed. |
| 3 | Only with `--emit queue`: the queue was printed, but at least one question is open. Without `--emit queue` this code is never used. |
| 5 | Only `merge --apply`: stopped and resumable (deploy timeout or no run, gate not confirmed). Run the same command again. See [docs/merge.md](docs/merge.md). |

`pipetrain update --check` also uses 10: a newer release exists.

A command-line usage mistake (an unknown option, for example) exits with clap's own code, 2. `pipetrain doctor` has its own two codes, listed in [Check your setup](#check-your-setup-doctor).

## Local usage log

Off by default. Turn it on with `log = true` in the config or `PIPETRAIN_LOG=1`; `--no-log` and `PIPETRAIN_LOG=0` turn it off. It writes one JSON line per run to `log.jsonl` in your user data directory (override with `PIPETRAIN_LOG_DIR`), rotated at 1 MiB, 3 files at most. Nothing is sent anywhere.

A line holds the time, the version, the subcommand name, the exit code, the error type (such as `ApiError`), the duration, the number of requests and whether the cache answered. It never holds a token, an org or repo name, a PR number or title, an argument value, a path, a diff or an error message. A failure to write it never fails or slows the command.

`pipetrain log --summary [--json]` counts runs per command, the error rate by type and the median duration. Details and the JSON shape: [docs/log.md](docs/log.md).

## Merging the queue

`pipetrain merge --apply` runs the order of `plan --emit queue`: it merges one PR, waits until the workflow runs of that push finish, and only then merges the next. Before each merge it reads the PR again and refuses one that is not approved, green and `clean`, or whose head moved after the plan, so a PR the plan marked as not ready is never merged. DDL and infra gates and unpublished package waits ask for confirmation (`--gates-confirmed` answers them; without a terminal the run stops there). A stop, a timeout or a failed deploy leaves a state file, and running the same command again resumes from there. Without `--apply` it only prints the steps.

```toml
[merge]
timeout_minutes = 30
no_deploy_repos = ["acme/docs"]

[merge.repo_timeout_minutes]
"acme/api" = 60
```

Exit code `5` means stopped and resumable. Everything else, the checks, the JSON and the design questions that were left open on purpose: [docs/merge.md](docs/merge.md).

## MCP server for AI agents

`pipetrain mcp` is a read-only [Model Context Protocol](https://modelcontextprotocol.io) server on stdin and stdout: no port, no daemon, nothing that writes. It has five tools, `plan`, `feed`, `bump` (dry run), `doctor` and `order_suggest`, and each returns the same `--json` (schema 1) its command prints; `order_suggest` returns the `[[order]]` text because that command has no `--json`. The token comes from `GH_TOKEN` or `gh auth` as in the CLI and never appears in an answer.

Claude Code:

```
claude mcp add pipetrain -- pipetrain mcp
claude mcp add pipetrain -- pipetrain mcp --config /path/to/pipetrain.toml
```

Keep the token out of the command: export `GH_TOKEN` in the environment that starts Claude Code, or rely on `gh auth login`.

Codex, in `~/.codex/config.toml` (not verified: written from the documented `mcp_servers` shape, not tested against Codex):

```toml
[mcp_servers.pipetrain]
command = "pipetrain"
args = ["mcp"]
```

The tools, the result format, the errors and the protocol choice are in [docs/mcp.md](docs/mcp.md). With the [usage log](#local-usage-log) on, each tool call is logged as `mcp:<tool>`, with the same exclusions as any run.

## Out of scope

pipetrain does not push, approve, comment, label or re-run checks, and nothing in it is a deploy tool. It writes to GitHub only through two commands that need `--apply`: `pipetrain bundle --apply` creates a deploy branch and changes the base of the PRs bundled into it, and `pipetrain merge --apply` merges the PRs of the plan one at a time and waits for their deploy. It decides order, not safety: it does not know whether a migration is backward compatible, only that it has to go first.

## Documentation

- [docs/planner.md](docs/planner.md): the rules, in the order they apply, and every question the planner can raise.
- [docs/plan-json.md](docs/plan-json.md): the `--json` contract.
- [docs/queue-output.md](docs/queue-output.md): the `--emit queue` contract.
- [docs/github-action.md](docs/github-action.md): the GitHub Action, its inputs, outputs and exit codes.
- [docs/log.md](docs/log.md): the opt-in local usage log, what a line holds and what never enters it.
- [docs/mcp.md](docs/mcp.md): the MCP server, its five read-only tools, results, errors and the protocol choice.
- [docs/merge.md](docs/merge.md): the contract of `pipetrain merge`: what it checks, what it waits for, resuming, exit codes, JSON and the open design questions.
- [docs/releasing.md](docs/releasing.md): how a release is cut, what the workflow builds and the optional crates.io step.
- [docs/repo-names.md](docs/repo-names.md): how `repo#123` is resolved to an org, and what it costs.
- [docs/saving-pipelines.md](docs/saving-pipelines.md): why the plan checks "already in the base", why it suggests a deploy branch, and how to apply it by hand.
- [pipetrain.example.toml](pipetrain.example.toml): the config, annotated.

## Development

CI runs `cargo clippy --all-targets -- -D warnings` and `cargo test --locked` on Linux, macOS and Windows for every push to `main` and every pull request. To run the same checks locally:

```
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

## License

MIT, by Lucas Passos. See [LICENSE](LICENSE).
