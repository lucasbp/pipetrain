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

## Next

Small, well understood, in this order:

1. `pipetrain order suggest`: print the discovered dependency chain as an `[[order]]` block, so it can be pinned in the config.
2. A safe cache for data that never changes for a given sha (PR files, trees, manifests), with `--no-cache` and `--refresh`. Mutable data such as reviews and checks is never cached.
3. More ways to find the train: a label in addition to the branch name.
4. Package detection for `Cargo.toml` and `pyproject.toml`, next to npm and NuGet.
5. A read-only conflict report: which PRs of a train will conflict and what kind of conflict it is.
6. A `# wait` line in `--emit queue` when a PR publishes a package that a later PR consumes, naming the package and the version, so a runner can wait for the release before continuing. Checking the feed itself starts with GitHub Packages (NuGet) and the npm registry.

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
