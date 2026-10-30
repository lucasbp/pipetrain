# pipetrain

pipetrain is a command-line tool that plans the order in which pull requests should merge across several repositories. It reads GitHub, builds a dependency graph from what it finds, and prints the order: what can merge at the same time, what has to wait for what, and why.

It is read-only. It never merges, pushes, comments or changes anything on GitHub, and it stores nothing. A `go` command that would execute a plan does not exist yet.

## Install

Requires a recent stable Rust toolchain (the crate uses edition 2024).

```
cargo build --release
```

The binary is `target\release\pipetrain.exe` (`target/release/pipetrain` outside Windows). To put it on your `PATH`:

```
cargo install --path .
```

## Authentication

pipetrain asks the GitHub CLI for a token (`gh auth token`) and uses it if the command succeeds. Otherwise it falls back to the `GH_TOKEN` environment variable. With neither, it stops with an error telling you to run `gh auth login` or set `GH_TOKEN`.

The token is used in memory for the run. It is never written to disk, to the config file or to the output. It needs to be able to read the pull requests, checks and file contents of the repositories you plan.

## Usage

```
pipetrain plan [OPTIONS] [PRS]...
```

| Command | What it does |
|---|---|
| `pipetrain plan` | Discovers today's train in the configured orgs and plans it. |
| `pipetrain plan acme/orders-api#12 acme/billing-api#3` | Plans exactly these PRs, no discovery. |
| `pipetrain plan --all` | Plans every approved open PR in the configured orgs, whatever its branch. For any day, not only the train day. Cannot be combined with explicit PRs. |
| `pipetrain plan --json` | Prints the plan as JSON instead of text (contract in `docs/plan-json.md`). |

| Option | Meaning |
|---|---|
| `--config <path>` | Use this config file. An unreadable file is an error. |
| `--org <org>` | Org to look at. Repeatable. Overrides `orgs` in the config. |
| `--date <date>` | Train date, instead of today. Accepts the date format of `train_date_format` (default `dd-mm-yyyy`, for example `--date 29-10-2026`) or ISO `yyyy-mm-dd` (`--date 2026-10-29`). |
| `--verbose` | Log every API request to stderr. |
| `--color auto\|always\|never` | Colors in the text output. `auto` (the default) colors only on a terminal and honors `NO_COLOR`. `always` colors even when piped, `never` never does. `--json` is never colored. |

PR references are written as `org/repo#123`. A PR that cannot be found stops the run.

## How the queue is discovered

The train branch is `train_prefix` plus the date, for example `deploy/29-10-2026`. The date is written with `train_date_format` (default `dd-mm-yyyy`). In each org, pipetrain searches open, non-archived PRs and keeps the ones that are:

- on the train branch (`train`);
- on a branch name that is open in 2 or more repos, which is how one change spans repos (`shared-branch`);
- targeting the train branch (`into-train`).

Of those, only PRs that are ready enter the plan: not draft, approved, checks green, no merge conflict. A repo with no checks at all blocks its PRs, unless `allow_missing_checks` is on or the repo matches a glob in `checks_optional_repos`. The rest are listed as `SKIPPED` with the reasons, and a few of them raise a question when they affect a ready PR. How the order is then decided, rule by rule, is in [docs/planner.md](docs/planner.md).

## Any day: released PRs

The default discovery looks for the train, which exists on one day of the week. On any other day, with approved PRs waiting to go out, run:

```
pipetrain plan --all
```

`--all` needs orgs, from the config or from `--org`. It takes every open, non-archived PR in those orgs and treats the approved ones as released, whatever their branch name:

- Approved and ready (not draft, checks green, no merge conflict) enters the plan with scope `released`.
- Approved but not ready goes to `SKIPPED` with the reasons, for example `checks failing` or `draft`.
- Not approved is ignored without a trace: no review, review requested or changes requested.

A PR whose head is today's train branch keeps scope `train`, and one targeting it keeps `into-train`. In this mode PRs are not grouped by shared branch, so the `sibling-not-ready` and `stacked-not-ready` questions are not raised. Everything after discovery is the same: the already-merged check, files, dependencies, packages, stacked PRs, targets, gate, layers and bundles.

The `train` line of the output reads `all released PRs`. When a plain `pipetrain plan` finds nothing, it suggests `--all`.

## Two checks before the order

**Already merged.** A ready PR whose commits are already in its base branch, or already in the train branch, has nothing left to merge. pipetrain compares each ready PR with those branches on GitHub (GraphQL, up to 20 comparisons per request). When the PR adds nothing, it goes to `SKIPPED` with the reason `already in <branch>` and stays out of the plan. A branch that does not exist counts as "not contained", and a PR with no head commit is not checked. A second check catches squash merges: when every file the PR changes already has identical content in the base or the train branch, the PR is skipped with `already in <branch> (same content)`. It does not follow renamed files and does not look at PRs with more than 100 files; those stay in the plan.

**Bundling deploys.** When 2 or more ready PRs of the same repo target the same base (for example `main`), each one is a deploy of that repo. The plan then adds a `BUNDLES` section suggesting one train branch instead: create it from the base, retarget those PRs to it, merge them there, and open a single PR from it to the base. N deploys become 1, and the `deploys` line shows both numbers. Bases that are themselves train branches are not bundled.

Both are read-only. The bundle is a suggestion: pipetrain does not create the branch and does not retarget anything. Why, the arithmetic and how to apply it by hand: [docs/saving-pipelines.md](docs/saving-pipelines.md).

## Example output

An invented run for the org `acme`, shown without colors (what `--color never` prints). When a plan has a single org, the `acme/` prefix is dropped from PR names:

```
pipetrain plan  (read-only, nothing was merged)
  train     deploy/29-10-2026
  orgs      acme
  found     8 ready, 2 skipped, 1 questions
  parallel  peak 3 at once (cap 4)
  deploys   8 now, 7 after bundling
  api       23 requests, 4811 left

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
        create deploy/29-10-2026 from main, retarget billing-api#230, billing-api#233, merge them there, then open one PR deploy/29-10-2026 -> main

SKIPPED  not ready, left out of the plan
  billing-api#228  Fix rounding in invoice PDF  [train: deploy/29-10-2026]  (already in main)
  billing-api#231  WIP retry failed charges  [shared: feat/retry-charges]  (draft)

?  QUESTIONS  the planner could not decide these
  docs-site#5  [layer-unknown]  acme/docs-site is not in any layer, so its place on branch `feat/invoice-totals` is undecided
        fix: add the repo to a [[layer]] in the config
```

How to read it:

- **WHERE IT CAME FROM**: how each PR got into the queue, with its scope and the criterion. `train`: the PR head is the train branch. `into-train`: the PR targets the train branch. `shared`: the head branch is open in 2 or more repos. `released`: approved PR on any branch, from `--all`. `explicit`: named on the command line. Each scope shows how many PRs are ready and how many were skipped, and `shared` also lists which PRs share each branch.
- **ORDER**: the whole plan in one line per step. A step starts only when every PR of the step above is merged and deployed. `pause` is the gate (DDL or infra): you confirm before anything else starts. The lanes below say why each PR is where it is.
- **GATE**: a PR that needs a human to stop and confirm first (a database change, or infrastructure). It goes before everything else and is not part of any lane.
- **LANE**: a chain of PRs. Lanes are independent of each other, so they can run side by side. Inside a lane, merge top to bottom.
- **s1, s2, s3**: the stage. PRs with the same stage have nothing between them and can merge at the same time.
- **after**: the PR that must merge first, the rule that says so, and the reason.
- **?**: the PR has an open question, listed at the end with a suggested fix. A question never changes the exit code.
- **[api]**, **[lib]**: the layer of the repo, from your config.
- **deploys**: how many deploys the ready PRs cause today (every PR that does not target a train branch is one), and how many remain if every bundle is applied.
- **BUNDLES**: a suggestion to merge several PRs of one repo through one train branch. Shown only when there is something to bundle. Nothing was created or retargeted.
- **SKIPPED**: PRs that are not ready, or that are `already in` a branch. `already in main` means the PR head is already part of `main`.

A `NOTES` section appears at the end when there is something informational, for example a `Depends-On` that points at a PR already merged.

## Configuration

pipetrain looks for a config file in this order and uses the first one it finds:

1. `--config <path>`, when given.
2. `./pipetrain.toml` in the current directory.
3. `pipetrain/config.toml` in the user config directory (`%APPDATA%` on Windows, `~/.config` on Linux, `~/Library/Application Support` on macOS).

With no file at all it runs on built-in defaults, which means you must pass `--org` or explicit PRs, because `orgs` is empty by default.

[pipetrain.example.toml](pipetrain.example.toml) lists every key with its default. Unknown keys are rejected, so a typo is an error and not a silent no-op. The layer map is the part you must write yourself: pipetrain cannot know which of your repos is a library and which is a front end. Why is explained in [docs/planner.md](docs/planner.md).

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

`--verbose` prints each request, and each wait, to stderr. The `api` line of the output shows how many requests the run used and how many GitHub says remain.

## Exit code

| Code | Meaning |
|---|---|
| 0 | A plan was generated. Skipped PRs and `?` questions do not change this. |
| 1 | An error: no token, bad config, bad `--date`, a PR not found, budget reached, GitHub failure. The message goes to stderr as `error: ...`. |

A command-line usage mistake (an unknown option, for example) exits with clap's own code, 2.

## Out of scope

pipetrain does not merge, push, approve, comment, label, re-run checks or write anything to GitHub, and nothing in it is a deploy tool. It decides order, not safety: it does not know whether a migration is backward compatible, only that it has to go first.

## Documentation

- [docs/planner.md](docs/planner.md): the rules, in the order they apply, and every question the planner can raise.
- [docs/plan-json.md](docs/plan-json.md): the `--json` contract.
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
