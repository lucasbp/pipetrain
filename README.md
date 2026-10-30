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
| `pipetrain plan --json` | Prints the plan as JSON instead of text (contract in `docs/plan-json.md`). |

| Option | Meaning |
|---|---|
| `--config <path>` | Use this config file. An unreadable file is an error. |
| `--org <org>` | Org to look at. Repeatable. Overrides `orgs` in the config. |
| `--date dd-mm-yyyy` | Train date, instead of today. For example `--date 29-10-2026`. |
| `--verbose` | Log every API request to stderr. |

PR references are written as `org/repo#123`. A PR that cannot be found stops the run.

## How the queue is discovered

The train branch is `train_prefix` plus the date, for example `deploy/29-10-2026`. In each org, pipetrain searches open, non-archived PRs and keeps the ones that are:

- on the train branch (`train`);
- on a branch name that is open in 2 or more repos, which is how one change spans repos (`shared-branch`);
- targeting the train branch (`into-train`).

Of those, only PRs that are ready enter the plan: not draft, approved, checks green, no merge conflict. The rest are listed as `SKIPPED` with the reasons, and a few of them raise a question when they affect a ready PR. How the order is then decided, rule by rule, is in [docs/planner.md](docs/planner.md).

## Example output

An invented run for the org `acme`:

```
pipetrain plan  (read-only, nothing was merged)
  train     deploy/29-10-2026
  orgs      acme
  found     7 ready, 1 skipped, 1 questions
  parallel  peak 3 at once (cap 4)
  api       23 requests, 4811 left

GATE  runs first, pauses for your confirmation
    acme/orders-api#41     [api] Add order status column
        DDL (db/migrations/0042_status.sql): every lane below waits for this

LANE 1
  s1    acme/billing-api#230   [api] Round invoice totals to cents
  s2    acme/billing-api#233   [api] Add invoice export endpoint
          after acme/billing-api#230  (target: same deploy target acme/billing-api@main)

LANE 2
  s1  ? acme/docs-site#5       Document the order status field

LANE 3
  s1    acme/shared-lib#7      [lib] Add order status enum
  s2    acme/orders-client#12  [client] Expose order status in the client
          after acme/shared-lib#7  (layer: layer lib goes before layer client on branch `feat/order-status`)
  s3    acme/orders-web#88     [front] Show order status badge
          after acme/orders-client#12  (layer: layer client goes before layer front on branch `feat/order-status`)

SKIPPED  not ready, left out of the plan
  acme/billing-api#231  WIP retry failed charges  (draft)

?  QUESTIONS  the planner could not decide these
  acme/docs-site#5  [layer-unknown]  acme/docs-site is not in any layer, so its place on branch `feat/order-status` is undecided
        fix: add the repo to a [[layer]] in the config
```

How to read it:

- **GATE**: a PR that needs a human to stop and confirm first (a database change, or infrastructure). It goes before everything else and is not part of any lane.
- **LANE**: a chain of PRs. Lanes are independent of each other, so they can run side by side. Inside a lane, merge top to bottom.
- **s1, s2, s3**: the stage. PRs with the same stage have nothing between them and can merge at the same time.
- **after**: the PR that must merge first, the rule that says so, and the reason.
- **?**: the PR has an open question, listed at the end with a suggested fix. A question never changes the exit code.
- **[api]**, **[lib]**: the layer of the repo, from your config.

A `NOTES` section appears at the end when there is something informational, for example a `Depends-On` that points at a PR already merged.

## Configuration

pipetrain looks for a config file in this order and uses the first one it finds:

1. `--config <path>`, when given.
2. `./pipetrain.toml` in the current directory.
3. `pipetrain/config.toml` in the user config directory (`%APPDATA%` on Windows, `~/.config` on Linux, `~/Library/Application Support` on macOS).

With no file at all it runs on built-in defaults, which means you must pass `--org` or explicit PRs, because `orgs` is empty by default.

[pipetrain.example.toml](pipetrain.example.toml) lists every key with its default. Unknown keys are rejected, so a typo is an error and not a silent no-op. The layer map is the part you must write yourself: pipetrain cannot know which of your repos is a library and which is a front end. Why is explained in [docs/planner.md](docs/planner.md).

## API budget

A run spends GitHub API requests: a search per org, a few per PR, one tree per repo it needs to scan. Three settings under `[api]` keep that bounded:

| Key | Default | Behavior |
|---|---|---|
| `max_requests` | 150 | The run stops with an error when it reaches this many requests. |
| `max_per_minute` | 60 | The run slows down to stay under this rate. It waits, it does not fail. |
| `min_remaining` | 200 | The run stops with an error when GitHub reports fewer requests left than this. |

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
- [pipetrain.example.toml](pipetrain.example.toml): the config, annotated.

## License

MIT.
