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

- Package detection for `Cargo.toml`, `pyproject.toml` and `go.mod`, next to NuGet and npm: a repo that publishes a package and another that references it get the `dependency` edge, and a readable version bump in the manifest diff gives the `package` edge and the `# wait` line. `path`, `workspace` and `git` dependencies, compound ranges and Go pseudo-versions never become a version. `feed`, `bump` and `--check-feeds` still know only NuGet and npm.
- `plan` and `bundle` accept `repo#12` without the org: the name is looked up in the orgs of the active profile (or `--org`), one `GET /repos` per org and distinct name, never cached. A name in two orgs is an error that lists them, and a name in none is an error with the next step. `org/repo#12` and `--json` and `--emit queue` are unchanged.

- Prebuilt binaries: a tag `v*` triggers a workflow that builds Windows x86_64, Linux x86_64 and aarch64 (static musl) and macOS arm64 and x86_64, plus `.deb` and `.rpm` for Linux x86_64 and aarch64, and publishes them with `SHA256SUMS` and a build provenance attestation to a GitHub Release. `install.sh` and `install.ps1` install into a user folder and verify the SHA-256. Publishing the crate on crates.io is a separate manual step, documented in `docs/releasing.md`.
- `pipetrain update --check`: read-only, exits 10 when a newer release exists. A passive one-line notice on stderr, at most once every 24 hours, interactive terminals only and off with `--no-update-check` or `PIPETRAIN_NO_UPDATE_CHECK=1`. The in-place swap of the executable exists behind `--experimental-install` and is not ready.
- A GitHub Action (`action.yml`, composite, read-only): installs the release binary for the runner, verifies it against `SHA256SUMS`, runs `plan` with `refs`, `all`, `profile`, `config` and `emit` (`json`, `queue`, `text`), and publishes `queue`, `json`, `text` and `exit-code` as outputs and in the step summary. Exit 3 only fails the step with `fail-on-questions`. See `docs/github-action.md`.

- An opt-in local usage log (`log = true`, `PIPETRAIN_LOG=1`, off with `--no-log`): one JSON line per run with the command name, exit code, error type, duration, request count and cache hit, never tokens, names, arguments or paths, rotated at 1 MiB. `pipetrain log --summary` reads it. Nothing is sent anywhere.

- `pipetrain mcp`: a read-only MCP server over stdio with five tools (`plan`, `feed`, `bump` as a dry run, `doctor`, `order_suggest`) that return the same `--json` schema 1 as the commands. No port, no daemon, no write tool. See `docs/mcp.md`.

## Next

Small, well understood, in this order:

1. Finish the in-place update (`update` without a flag): download, verify the SHA-256 and swap the executable, on top of the existing experimental path.
2. `bump --apply`: an opt-in mode that opens one PR per repo with the new version, on top of the dry run.
3. A deeper conflict report that simulates the merge, beyond files changed by more than one PR.
4. `feed`, `bump` and `plan --check-feeds` for Cargo (crates.io), `pyproject.toml` (PyPI) and Go modules: today they only know NuGet and npm.
5. `cargo audit` (and `cargo deny` if it fits) in the CI workflow, so advisories fail a PR instead of being checked by hand.

## Later

Bigger or riskier, no order yet:

- Discover orgs and repos from local clones, and use the token Git already stores as a last-resort credential.
- Conflict resolution in steps: mechanical rules first (lockfiles, version bumps, append-only files), then proposed diffs for the rest that a human approves, then rebase on a throwaway branch that is always reversible.
- An execution mode that performs the merges. Opt-in, resumable, with a report. Never the default.
- Bump consumers automatically after a package release: wait for the new version to reach the feed, update the reference in the dependent repos, then release them. This writes to repos, so it ships as an opt-in mode.
- Using `pipetrain mcp` from cloud sessions and Codex: a documented path that downloads the release with a SHA-256 check and registers the server, no hosted service and no token stored on a server.
- Package detection for Maven and Gradle.
- Other forges.
- Optional encryption for local storage.

## Not planned

- Replacing Git. pipetrain works with Git and GitHub, not instead of them.
- Writing to a repo without an explicit flag.

## How to influence this

Open an issue describing the workflow you need. "My setup is not supported" issues are the most useful ones: say how your repos relate and what the planner got wrong.
