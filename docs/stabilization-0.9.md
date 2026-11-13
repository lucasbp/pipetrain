# Stabilization plan for 0.9

Goal: make pipetrain dependable for the daily merge queue before it gains any new write power. 0.9 explains before, tracks during and allows recovery after. Nothing here widens what pipetrain writes to GitHub.

Baseline: `origin/main` at `fae64e7`, version 0.8.1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --locked` were green on that commit before any slice.

Source review: an external code review of tag v0.8.0 (13 Nov 2026). Every finding below was re-checked against 0.8.1; the ones 0.8.1 already fixed are listed in "Already fixed in 0.8.1".

## Daily scenarios and current state (0.8.1)

| # | Scenario | Covered by tests? | Behavior in 0.8.1 |
|---|---|---|---|
| S1 | `plan` across several repos of one org | Yes (`tests/order.rs`, `tests/rules.rs`, `tests/train.rs`) | Works. |
| S2 | `plan --all --emit queue` for a runner | Yes (`tests/emit.rs`, `tests/emit_wait.rs`) | Works. Conflict and bundle warnings are comments and are dropped when `merge` turns the queue into steps (`src/emit.rs`, `src/merge.rs`). |
| S3 | `merge` dry run, then `--apply`, waiting each deploy | Yes (`tests/merge_run.rs`, 76 tests) | Works for the happy path. Deploy is "passed" when any `push` run for the merge SHA passes, twice in a row (`src/command/merge.rs` `wait_for_deploy`). |
| S4 | PR into `main` whose deploy workflow filters by `on.push.paths` and the PR touches none of them (incident 1) | No. `src/workflows.rs` has a test that treats `paths` as "runs" | Workflows are only read when the base matches `train_prefix` (`deploy_phase`). On `main` it polls, sees no run, waits `no_run_grace_seconds` (120 s) and stops with advice to add the whole repo to `no_deploy_repos`. |
| S5 | Auxiliary workflow on push (lint, labeler) passes before the real deploy starts | No | The auxiliary run confirms the deploy. No notion of which workflow was expected. |
| S6 | Deploy chained through `workflow_run` | No | Runs are queried with `event=push` (`src/github/endpoints.rs`), so chained runs are never seen, although `docs/merge.md` presents the double poll as protection for chains. |
| S7 | More than 100 runs for one SHA | No | `per_page=100`, no truncation check. |
| S8 | Repo without Actions deploy | Yes (`no_deploy_repos`) | Works, per repo. |
| S9 | Deploy fails for a pre-existing cause, another PR fixes it and deploys (incident 3) | Partly: resume of the same SHA after a re-run is tested | The pending record is tied to the old SHA forever. A later green deploy does not resolve it; the fix PR on the same repo/base can be blocked by the failure (`src/command/merge/failure.rs`). No command to inspect or discard a record. |
| S10 | New queue in the same profile while an old pending record exists (incident 6) | Partly (profile isolation is tested since 0.8.1) | 0.8.1 keeps one state file per profile and prints its path. Inside a profile every pending record is resumed at the start of any run, with no age, no queue identity, no lock between two processes and no schema validation on read. |
| S11 | Plan says ready, executor refuses because `mergeStateStatus` is not `CLEAN` (incident 4) | No | `src/github/parse.rs` keeps only `BEHIND`. The executor refuses `blocked`, `dirty`, `unstable`, `draft`. Plan and executor disagree. |
| S12 | NuGet version gate (`check-nuget-version`) red | Partly: failed check names show | No link to the check, no cause or next step. |
| S13 | PR base is a feature branch, not the default branch | No | Default branch is never read; base and default branch are not compared. |
| S14 | Stacked PRs: child into parent head, then parent (incident 5) | Partly (`tests/train.rs`, planner ordering) | Planner orders child before parent. After the child merges, the parent head SHA changes and the executor refuses it as "head moved". A feature base whose parent PR is not selected is not flagged. |
| S15 | Parallel merge progress (incident 2) | Partly (0.8.1 multiline) | Three live lines, but only the PR that spoke last is shown. Counts mix workers and steps; "merged" and "deploy confirmed" are not separated; parallelism is not shown before confirmation. |
| S16 | Consumer waits for a package the publisher just released | Partly (`tests/feed_wait.rs` for `feed --wait`) | `merge` decides from a snapshot taken before the publisher merged; when the package appears it asks a human or needs `--gates-confirmed`, which also confirms every infra gate. |
| S17 | Resume with `--state` | Yes | Works for interruption, lost merge response and re-run of the same run. |
| S18 | `feed` and `bump` | Yes (`tests/feed_*.rs`, `tests/bump_*.rs`) | Works. |
| S19 | Agent use: `--json` and exit code | Partly | A dry run, or an `--apply` where every PR is blocked, exits 0. No strict mode. |
| S20 | Long deploy wait | Partly | Timeout adds up sleeps only, ignoring HTTP, pacing and retries. One transport error ends the wait. Default API budget (300 requests) is below what four 30 minute deploys polled every 15 s need. |
| S21 | Install and update | Partly (`tests/update_cli.rs`) | Updater verifies SHA-256 and rolls back. `install.ps1` and `install.sh` have no download deadline and the PowerShell installer has no rollback. README still promises sequential merges without Git. |

## Already fixed in 0.8.1

- Single global state file: named profiles now get `pipetrain/queues/<profile>/merge-state.json`, `merge.state_file` is supported, the effective path is printed, and unfinished legacy state blocks an automatic switch (`src/merge/storage.rs`). Queue identity, age, lock and schema validation are still missing.
- Unknown mergeability right after an earlier merge: both readiness checks now retry for up to a minute and stop resumably.
- Merges into an intermediate train branch now warn that the change did not reach the release target.
- Single-line progress: replaced by up to three lines (counts, target, operation). Only one PR is visible at a time.

## Gaps by impact

1. Deploy confirmation is not verifiable: path filters ignored, workflows only read on train bases, any push run confirms, chained runs invisible, 100 run cap unchecked (S4 to S7).
2. State lifecycle: no queue identity, age, lock or schema check; no way to inspect or discard; a later green deploy cannot resolve an old failure (S9, S10).
3. Plan and executor disagree on readiness; blocked selections exit 0; base vs default branch unknown (S11 to S13, S19).
4. Parallel progress hides active PRs and conflates merge with deploy (S15).
5. Stacks break the executor after the child lands; package wait is a snapshot (S14, S16).
6. Timeouts are not wall clock, API budget is undersized, incident regressions and install hardening missing (S20, S21).

## Slices

Each slice is one Codex run in this worktree, validated with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test --locked`, then committed on its own. Order follows dependencies: 1b needs the expected workflow set of 1a, 2b needs both, 3 and 4 build on the state of 2, 6 closes the release.

Rules for every slice: no comments in code; every new behavior gets a test against the fake GitHub server and on-disk state, as `tests/` already does; never run the binary against real GitHub or with `--apply`; no commits by the executor; existing `--json` fields keep their name, type and meaning (new fields are additive and documented in `docs/` in the same slice; anything else bumps `schema`); no new GitHub write; no new crate unless the slice says so; public repo, so no private names.

### 1a. Workflow filters on any base

- SCOPE: `src/workflows.rs`, `deploy_phase` in `src/command/merge.rs`, the PR files endpoint, `docs/merge.md`. Read workflows for every base, not only train bases. Evaluate `on.push.branches`, `branches-ignore`, `paths`, `paths-ignore` against the merged PR's changed files (including the previous name of a renamed file). Three outcomes per repo: expected (with the workflow files), dispensed (with the evidence: which filters excluded which files) and indeterminate (with why: unreadable YAML, unsupported glob, incomplete file list, more than 300 changed files). Dispensed ends the step as done without polling. Indeterminate keeps today's polling and grace. The dry run shows the outcome per PR. The "list it in `no_deploy_repos`" advice only appears when no workflow runs on push at all.
- FORBIDDEN: changing `no_deploy_repos` semantics, new GitHub writes.
- DONE WHEN: tests cover incident 1 (PR into `main` touching no filtered path ends dispensed without waiting), `paths-ignore`, negated patterns, rename, truncated file list, and the old test that treated `paths` as "runs" is replaced; gates green.

### 1b. Expected workflows confirm the deploy

- SCOPE: `wait_for_deploy`, `src/merge.rs` verdicts, runs endpoint, `Pending` record, `docs/merge.md`. Persist the expected workflow files in the pending record. Only runs of expected workflows can confirm; an auxiliary workflow cannot. Track runs by head SHA without the `event=push` filter, and include workflows triggered by `workflow_run` of an expected workflow (matched by `name:`); if the chain cannot be resolved, say so and do not confirm. Page past 100 runs or treat a truncated page as not yet confirmed. Report run URLs in the step detail and JSON (additive).
- FORBIDDEN: changing the meaning of existing JSON fields.
- DONE WHEN: tests: auxiliary workflow passes first and the deploy is still awaited; chained deploy is followed; more than 100 runs; legacy pending records without expected workflows still resume as before; gates green.

### 2a. State identity and safety

- SCOPE: `src/merge.rs`, `src/merge/storage.rs`, merge preview. Bump state schema to 2 with an in-place upgrade from 0 and 1; refuse a newer schema with a clear error. Record `created_at`, `updated_at`, the profile and a queue id; each pending record gets `recorded_at`. Exclusive lock (`std::fs::File::try_lock`) held for the whole `merge --apply`; a second process fails fast naming the lock file. Unique temp file per write. Dry run and confirmation show the state path, age and every inherited pending record with its age and SHA, marking records older than a configurable threshold as stale (warning only).
- FORBIDDEN: deleting or migrating state automatically, changing which pending records are resumed.
- DONE WHEN: tests: newer schema refused, legacy file upgraded, two stores on the same file (second fails), inherited pending listed with age in dry run text and JSON; gates green.

### 2b. Inspect, discard and resolve by a later deploy

- SCOPE: new `pipetrain state show [--json]` and `pipetrain state discard <id> --reason <text>` (local file only, `--apply` style confirmation), resolution path in resume. A discarded record moves to an audit list with reason and time and is never reported as a confirmed deploy. When a pending deploy failed and the base branch now has a later commit that contains the old SHA (compare API) and whose expected workflows passed, the record resolves as "resolved by later deploy" with the run URL and SHA as evidence. Resume messages name the SHA and run being waited on and say when repeating will not help.
- FORBIDDEN: any GitHub write, re-running workflows, removing a record without an audit entry.
- DONE WHEN: tests for incident 3 (failed deploy, later green deploy on a descendant resolves it, unrelated later commit does not), discard audit, `state show` JSON documented; gates green.

### 3. One preflight for plan and merge

- SCOPE: `src/github/parse.rs`, `src/model.rs`, the GraphQL query, `config.blockers`, plan and merge reports, `docs/plan-json.md`, `docs/merge.md`. Keep the full `mergeStateStatus` and use the same readiness function in plan and executor; read the repo default branch and show base vs default. Each PR is eligible, blocked or waiting on an earlier step, with cause, next step and check links (name and URL). New `--strict` for `plan` and `merge`: non-zero exit when the selection is fully blocked or when a dry run would merge nothing.
- FORBIDDEN: changing default exit codes, inferring private repo policy from a check name.
- DONE WHEN: tests for incident 4 (blocked, dirty, unstable in plan), feature base vs default branch, check links, strict exit codes for plan and merge; gates green.

### 4. Merge progress per PR

- SCOPE: `src/progress*`, `src/live.rs`, `src/command/merge/parallel.rs`, final summary, `docs/merge.md`. One live row per active PR with repo#N, destination, phase and elapsed time; counters split into waiting, running, merged and deploy confirmed; parallelism shown before confirmation. Non-interactive output unchanged except the new summary counts.
- FORBIDDEN: changing order, polling or resumability.
- DONE WHEN: tests for incident 2 (two active PRs both visible with phases), summary separating merged and deploy confirmed, parallelism line in the preview; gates green.

### 5a. Stacks coherent with the current strategy

- SCOPE: executor revalidation, plan report. After a child merges into the parent head, re-read the parent: accept the new head only when it is the child's merge result on top of the planned SHA, then re-check review and checks; otherwise stop with the cause. Flag a PR whose base is a non-default, non-train branch with no selected parent ("base not landed"). Show the whole chain in plan and preview.
- FORBIDDEN: retargeting PRs, updating branches, weakening the head SHA guard.
- DONE WHEN: tests for incident 5 (child then parent merges; parent with lost approval stops; unrelated push to parent stops; orphan feature base flagged); gates green.

### 5b. Package wait checks the feed

- SCOPE: package wait step in `merge`. When the wait step is reached, check the feed with the `feed --wait` mechanism under a bounded wait before asking anyone. Published: proceed with evidence. Not published: today's prompt or stop.
- FORBIDDEN: changing what `--gates-confirmed` confirms (owner decision, see below).
- DONE WHEN: tests with a fake feed: published after a delay proceeds without a prompt; never published stops as today; gates green.

### 6. Deadlines, budget, regressions, install

- SCOPE: wall-clock deadlines with `Instant` (through an injectable clock) for deploy waits and mergeability retries, bounded retry of transient read errors during a wait, API budget estimate in the preview with a warning when the queue cannot fit, `tests/incidents.rs` asserting the six incidents end to end, download deadline in both installers and rollback in `install.ps1`, README aligned with parallel merges and `--bump`, CHANGELOG `Unreleased` section.
- FORBIDDEN: version bump, tag, release.
- DONE WHEN: tests: a slow HTTP fake makes the deadline fire on wall clock; transient error during a wait is retried; budget warning; six incident tests green; gates green.

## Decisions for the owner

- Should pending records outside the current selection keep resuming automatically (today) or need an explicit opt-in? 2a only shows them, with age.
- Should `--gates-confirmed` stop confirming package waits once 5b verifies the feed? 5b leaves the flag unchanged.
- Stale threshold default for 2a (proposed 24 h, warning only).
- Parent-first stacks with retarget (review item 7) stays out of 0.9.
