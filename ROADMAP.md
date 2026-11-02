# Roadmap

pipetrain plans the merge order of pull requests across repos. This file says where it is going. It is a direction, not a promise: nothing here has a date, and the order can change when real use says so.

## Principles

- **Read-only by default.** The default commands never write to GitHub. Anything that writes is a separate, explicit, opt-in mode.
- **Stable contracts.** `plan --json` (schema 1) and `plan --emit queue` only ever gain fields or lines. Tools that consume them keep working.
- **When in doubt, serial.** An edge the planner cannot prove goes in series and shows its reason. Wrong-but-slow beats wrong-and-broken.
- **Config first.** Rules that differ between teams (layers, gates, branch naming) live in the config, not in the code.

## Done

- `plan`: dependency order from `Depends-On`, package bumps, same deploy target, DDL/infra pauses, layers, parallel lanes, with the reason of every edge and a `?` for what it cannot decide.
- Discovery of the day's train branch, branches shared across repos, `--all` for any released PR, or explicit `org/repo#n`.
- Already-merged checks by commit and by file content (catches squash merges).
- Deploy-branch bundle suggestions that save pipeline runs.
- `--json` (schema 1), `--emit queue`, colored output with origin and order maps.
- Typed errors that say what happened and what to do next.
- `doctor`: checks the token source, API reach, rate limit, config and org visibility, with the exact command to fix each problem.
- Profiles (`--profile`, `profiles`, `orgs`, `init`), selected by flag, working folder, `default_profile` or the only profile, and an explicit repo order in the config (`[[order]]`).
- Automatic dependency order from the manifests (NuGet and npm): when a repo references a package that another repo in the plan publishes, the publisher goes first and the reason names the package. A project with no `PackageId` publishes its assembly or project name. An explicit `[[order]]` wins, and repos that reference each other raise a `dependency-cycle` question. Dependencies between ecosystems (NuGet to npm) cannot be detected and stay explicit.

- Queue output carries a `# bundle` comment per suggested deploy branch.
- Queue output carries a `# wait` comment before a PR that bumps a package published earlier in the queue. Checking the feed stays with the runner.
- `pipetrain order suggest` prints the discovered chain as an `[[order]]` block, read-only.

- A local cache for data that never changes for a key (the paths of a PR keyed by base tip and head sha, trees and manifests by commit sha, blob ids by head sha), with `--no-cache` and `--refresh` on `plan` and `order suggest`. Mutable data such as reviews, checks and branch tips is never cached, and neither is a token or a diff. The second run of a real queue went from 38 to 11 requests with the same output.
- A progress spinner on stderr for `plan` and `order suggest`, drawn only on a terminal after 300 ms, off with `--no-progress`, `--json`, `CI=true` or a redirected `--emit queue`. stdout is never touched.
- A DDL or infra PR that must follow another PR by a package, dependency or target edge is a late gate: it runs after its predecessors with the pause right before it, instead of a question. `--json` marks it with `gate_late`.
- The summaries show how long the run took: a `time` line in `plan`, `... in 3.2 s` on the `order suggest` request line, a `time` line in `doctor` and `stats.elapsed_ms` in `--json`.
- The `# wait` comment names the version the consumer bumps to, read from the consumer PR's manifest diff, and `--json` carries it as `after[].version`. Checking the package feed is `pipetrain feed`.
- A read-only conflict report: PRs of the same repo that change the same files, with the kind of file (lockfile, sql, generated, other), in the text output, `--json` (`conflicts`) and as `# conflict:` lines in the queue. The reason of a PR skipped for failing checks names the failing checks.
- `pipetrain bundle`: dry run by default, `--apply` creates the deploy branch and retargets the bundled PRs, one write at a time, stops at the first failure. It never merges or opens the deploy PR.
- PR author in the output and a filter for your own PRs (--mine, --author, authors in the config): the plan is computed with everyone and the PRs of other authors are only hidden, so the order stays right.
- The spinner is colored and shows the elapsed time while it runs.
- `pipetrain bundle --apply` shows a live colored checklist on stderr while it writes, one row per action with its duration, and stops with the failed row in red. Off with `--json`, `--no-progress`, `CI=true` or a redirected stderr.

- `pipetrain feed <package> [--version <v>]`: a read-only check of whether a version is published in its NuGet or npm feed (GitHub Packages or a public registry), with `--json` and exit codes a runner can use.
- `pipetrain bump <package> [--to <v>]`: a dry run of the consumer bump, built on `feed`. It reads the manifests of the chosen repos (or the exact repos of the profile) and reports each as `change`, `up_to_date`, `ahead` or `range`, with `--json`. It writes nothing and has no `--apply`.
- `plan --check-feeds`: an opt-in flag that appends `(published)`, `(not yet)` or `(feed check failed: ...)` to each `# wait` comment that names a version, using the `feed` lookup. Informational only, with `published` and `feed_error` on `after[]` in `--json`.
- A train label in addition to the branch name: `train_label` in the config (and in a profile) marks the PRs of the train by label, next to `train_branch`. A PR that matches either one is in the train.
- `bump --only major|minor|patch`, `--changes-only` and `--check`: additive options on the dry run. `--only` (repeatable) leaves the other levels as `other_level`, `--changes-only` lists just the manifests that would change, and `--check` exits 1 when something would. Without them the output and the JSON are unchanged.
- `pipetrain feed <package> --version <v> --wait [--interval <s>] [--timeout <s>]`: blocks until the version is published, with exit codes a runner can use (0 published, 1 timeout, 2 refused, 3 persistent error). Progress goes to stderr, the result to stdout once, and `--json` gains `waited`, `attempts`, `elapsed_seconds` and `timed_out` with schema 1 intact.

## Next

Small, well understood, in this order:

1. `bump --apply`: an opt-in mode that opens one PR per repo with the new version, on top of the dry run.
2. Package detection for `Cargo.toml` and `pyproject.toml`, next to npm and NuGet.
3. A deeper conflict report that simulates the merge, beyond files changed by more than one PR.

## Later

Bigger or riskier, no order yet:

- Resolve repo names without the org (`repo#12`) by indexing the repos of the known profiles.
- Discover orgs and repos from local clones, and use the token Git already stores as a last-resort credential.
- Conflict resolution in steps: mechanical rules first (lockfiles, version bumps, append-only files), then proposed diffs for the rest that a human approves, then rebase on a throwaway branch that is always reversible.
- An execution mode that performs the merges. Opt-in, resumable, with a report. Never the default.
- A GitHub Action and an MCP server on top of `--json`, so automation and AI agents can ask what is safe to merge and in what order.
- Bump consumers automatically after a package release: wait for the new version to reach the feed, update the reference in the dependent repos, then release them. This writes to repos, so it ships as an opt-in mode.
- Package detection for Go, Maven and Gradle.
- Other forges.
- Optional encryption for local storage.

## Not planned

- Replacing Git. pipetrain works with Git and GitHub, not instead of them.
- Writing to a repo without an explicit flag.

## How to influence this

Open an issue describing the workflow you need. "My setup is not supported" issues are the most useful ones: say how your repos relate and what the planner got wrong.
