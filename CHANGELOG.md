# Changelog

## 0.9.0

- Merge deploy waits and both mergeability checks use injectable monotonic wall-clock deadlines, include HTTP, pacing and retry time, and cap every wait sleep by the remaining time, including no-run grace.
- Merge retries transient transport and HTTP 5xx deploy reads up to three consecutive times, resetting on recovery and stopping resumably with pending state after exhaustion.
- Merge previews and confirmations estimate discovery, readiness/merge checks and deploy polls using per-repository deadlines; additive JSON api_estimate and advisory warnings identify api.max_requests without changing its default or blocking confirmation.
- Add six end-to-end incident regressions for path-filter dispensing on main, simultaneous PR phases, later-deploy recovery, blocked planning, stacked merges, and stale pending state with cross-process locking.
- Installers bound downloads to 120 seconds, shell connections to 15 seconds, and wget total runtime with a watchdog; PowerShell restores the previous binary if replacement or its final step fails, preserving download URLs and checksum verification.
- README describes parallel merges, local state inspection/discard and optional merge --bump Git/.NET preparation while retaining read-only defaults.

- Merge package waits recheck and poll the feed after the publisher finishes, automatically releasing consumers with version and feed evidence; configure the interval and timeout with merge.package_poll_seconds (10) and merge.package_wait_seconds (600).
- Merge retains manual package confirmation after an absence timeout, while feed authentication refusals stop immediately with their cause and failed final lookups stop after retries; --gates-confirmed retains its manual gate meaning.

- Merge accepts a stacked parent's changed head only with recorded child merge evidence, planned-SHA ancestry and fresh readiness for that exact head; lost approval or pending checks stop with the next step, and the merge request uses the verified SHA.
- Plan notes, merge dry runs and confirmations show complete selected stack chains and explain that children land into the parent branch before the parent is revalidated.

- Interactive merge progress retains one row per active PR with its base, phase and elapsed time; waiting excludes active work, and the header separates merge acceptance, deploy confirmation and failures.
- Merge dry runs and confirmations show the configured parallelism upper bound and the existing one-worker-per-repository restriction, including different bases.
- Merge text summaries and additive JSON summary, max_parallel and deploy_outcome fields distinguish merged PRs from deploys confirmed, dispensed, indeterminate, resolved by a later deploy, skipped and failed.

- Plan and merge share preflight blockers for full GitHub merge states; UNKNOWN warns while the executor retains its one-minute retry and final clean-only guard.
- Plan and merge previews and additive JSON preflight entries show eligibility, causes, next steps, bases, repository default branches and check names, states and URLs.
- Plan and merge warn about feature bases without a selected parent; queue output adds only preflight comments and preserves existing step ordering.
- Add opt-in `plan --strict` and `merge --strict` exit code 4 for selections without an eligible PR, including merge dry runs with no merge steps; default codes remain unchanged.

- Add local `state show` inspection in text and JSON, using merge state path and profile selection without contacting GitHub.
- Add confirmed `state discard` with a required reason, the merge state lock and atomic audit history; discarded records never count as confirmed deploys.
- Resume failed deploys through a later successful deploy only with ancestry proof, complete expected workflow evidence and two settled readings; save the later SHA and run URLs in audit and additive report resolution fields.
- Resume progress and failure messages identify the saved SHA and runs, explaining when repeating an unchanged failed deploy cannot help.
- Merge state schema 2 records queue identity, profile and UTC timestamps; legacy schemas 0 and 1 upgrade on save, while newer schemas are refused without changing the file.
- Merge apply holds an exclusive process lock beside the state file, reloads under the lock and uses unique temporary files for atomic state replacement.
- Merge dry runs, confirmations and JSON show state origin, file age and inherited pending records, with warning-only stale markers controlled by `merge.stale_pending_hours` (default 24).

- Merge confirms deploys only from saved expected workflow paths; auxiliary runs cannot decide the verdict and their failures remain visible.
- Merge follows resolved and transitive `workflow_run` chains on the merge SHA, persists them for resume, and keeps untrackable chains pending with a diagnostic.
- Merge refuses deploy confirmation from incomplete workflow-run responses, including more than 100 runs for a SHA.
- Merge step details include expected run names and URLs, also exposed by the additive JSON `expected_runs` field.

- Merge evaluates push branch and path filters on every base using complete PR file pagination and rename sources, dispensing the wait with evidence when no workflow should run and keeping uncertain inputs on the existing polling path.
- Merge dry runs show each PR's workflow prediction in text and the additive JSON `deploy_prediction` field.
- Pending merges save their workflow prediction, including expected workflow paths, for resume and subsequent verification; older records retain legacy polling behavior.

## 0.8.1

### Added

- Interactive merge progress separates queue counts, the current organization/repository/PR and operation details into up to three live lines. The target is bold when terminal styling is enabled, and each line fits the current terminal width.

- Named profiles automatically keep one durable merge queue across repositories and invocations. `--state` remains an override, and `merge.state_file` can persist a custom location in application configuration. Relative configured locations resolve from that configuration file, and unfinished legacy state blocks an automatic switch instead of being ignored.

### Fixed

- Execution-time GraphQL readiness checks retry temporary unknown mergeability after an earlier merge, rather than failing before the REST retry path. Both checks show progress for up to one minute each and stop resumably if GitHub cannot settle; other blockers remain enforced.
- Merge previews, confirmations and final summaries warn when a PR targets an intermediate train branch, including a train from an earlier date. Completing those merges or skipping their workflows does not mean the changes reached the release target.

## 0.8.0

### Added

- Merge execution now runs independent repositories concurrently up to `max_parallel`, preserving dependency order, one active operation per repository, gate barriers and durable checkpoints before effects. Running work is reconciled on resume without repeating merges.
- `merge --bump` optionally prepares NuGet consumer updates after a new publisher push is proved by its workflow and exact package feed version. Pins and lockfiles are prepared together in an isolated worktree, and a draft PR targets the consumer branch for CI and review before the consumer can merge. `--no-bump` explicitly keeps automatic preparation disabled.
- Publication inputs, consumer contracts and bump preparation markers persist in the merge state. Skipped or duplicate publications never select an old latest version or create a consumer bump. Partial preparations stop for inspection rather than creating duplicate PRs.

### Changed

- Parallel progress shows completed steps, active operations and remaining work. GitHub request and pacing limits are shared across workers.

### Fixed

- Animated status fits the current terminal width, preserving the step/PR context and elapsed time without wrapping each frame. Narrow terminals use a compact prefix, and truncation preserves UTF-8 and color controls.

## 0.7.4

### Added

- Interactive `merge --apply` now shows a spinner and elapsed time while checking PR readiness, merging and waiting for deployment workflows. It pauses for gate prompts and respects `--no-progress`, JSON, CI and redirected stderr.

- Leaf deployment failures now continue proven independent PRs while preserving the failed status and pending state. Dependency publisher failures stop by default; `--dependency-failure skip` blocks consumers and continues independent PRs. New resume metadata preserves dependency safety; failed legacy records stop safely.
- Streamed results honor terminal colors: successful steps green, failures red, blocked or stopped steps yellow, and normal skips gray. All-skipped workflow runs settle as normal skips without hiding failures in other workflows.

## 0.7.3

### Fixed

- `merge --apply` no longer repeats every streamed step in its final text output. The final summary separately counts merged PRs and earlier merges checked, so a failed deployment does not hide a successful merge behind `0 done`.
- A failed deployment explicitly says that it failed and explains how to resume with the same state file after fixing or re-running the workflow. Pending deploys still stop the queue; JSON retains the complete step report.

## 0.7.2

### Fixed

- The search for open PRs now checks the total GitHub reports (`issueCount`). When GitHub cuts the list short without an error, pipetrain searches again (up to 2 more times) and, if the list is still short, fails with `GitHub returned N of M open PRs in <org> (the search was cut short). Try again in a minute, or run with --verbose.` instead of returning an empty or partial plan.
- The search asks for pages of 20 PRs instead of 50.

## 0.7.1

### Fixed

- `plan --all` and `bundle --all` no longer drop an unapproved PR in silence when `require_approval` is `true`: it is listed under `SKIPPED` with the same reasons `merge` gives (`not approved`, `review required`, `changes requested`), so `plan --all`, `plan <pr>`, `merge` and `merge --apply` agree on what a ready PR is.
- `merge --apply` with nothing to merge no longer prints the dry run banner ("dry run, nothing was merged; pass --apply to do it").

## 0.7.0

### Added

- `require_approval` config option, also settable per profile, `true` by default. With `false`, a PR with no review decision or `REVIEW_REQUIRED` is no longer `SKIPPED` as `not approved` or `review required`, `plan --all` considers every open PR instead of only approved ones, and `merge` does not refuse such a PR when it revalidates. `changes requested` still blocks. Config only, there is no flag.

## 0.6.0

### Changed

- `pipetrain update` without a flag now installs: it downloads the latest release, checks its SHA-256 against `SHA256SUMS` and replaces the running executable, wherever it lives. It no longer exits 1 with a "not ready" message. `--check` is unchanged. `--experimental-install` is still accepted, hidden, and does nothing extra.
- On Windows the old executable is renamed to `pipetrain.exe.old` and deleted on the next start; on Linux and macOS the new file is renamed over the old one.

### Added

- `pipetrain update --dry-run` shows the current and new version, the asset and the executable it would replace, without downloading or writing. `--json` gains `dry_run`.
- `pipetrain update` checks that the folder of the executable is writable before downloading and names the path when it is not.

## 0.5.0

### Added

- Profile `aliases`: a `[profile.NAME]` table may list short names (`aliases = ["a", "ac"]`). An alias is accepted by `--profile` and by `default_profile`, case-insensitively, and the profile that comes out is always the real one. `pipetrain profiles` adds an `ALIASES` column only when some profile has aliases, and `doctor` and the unknown profile and no profile chosen errors list them as `aliases: a, ac`. An alias that repeats the profile's own name, is listed twice, or equals another profile's name or alias is refused when the config is read, and `aliases` outside a `[profile.NAME]` table is an error.
- `[org_alias]` table: short names for real orgs (`lab = "acme-labs"`). The alias is replaced before anything else reads it, in `--org` (`plan`, `bundle`, `merge`, `bump`, `feed`, `doctor`), in `org/repo#N` and PR URL arguments, in the `org/repo` arguments of `bump` and `order suggest`, and in the profile rule that reads the orgs of the arguments. `pipetrain profiles` prints an `org aliases:` line. An alias that differs from another only by case, points to another alias or to itself, or equals an org of any profile (or of a flat config) is refused when the config is read.

Plans, caches and every output keep the real org and profile names, so the `--json` and `--emit queue` contracts are unchanged. See [docs/aliases.md](docs/aliases.md).

## 0.4.0

### Changed

- `bundles[].branch` in `plan --json` (and the branch `bundle` creates) now always carries the base as a suffix: `deploy/<date>-<base>`, for example `deploy/05-11-2026-master`. Until 0.3.0 the suffix appeared only when one repo had bundles on more than one base, so anything that read `bundles[].branch` and expected `deploy/<date>` for a single bundle must read the field instead. An open PR whose head is exactly the unsuffixed `deploy/<date>` of an older bundle is still taken as the train PR and keeps its name.
- `bundle --apply` never reuses a deploy branch that came from another base. When the branch already exists it must contain the tip of the bundle's base, otherwise `bundle` stops with `refusing to reuse the branch` before writing anything (a dry run reports it too). The check is repeated if GitHub answers that the branch already exists at the moment of the create.
- A PR whose branch GitHub reports as `BEHIND` its base (`mergeStateStatus`, which branch protection that requires up to date branches produces) is now listed under `SKIPPED` with the reason `branch behind its base, update it`. Nothing else that is ready changes.

### Added

- `plan --emit queue` prints `# step <n>` and `# lane <n>` before each PR (the `stage` and `lane` of the plan; a gate PR has no lane line), so a runner can run lanes side by side. Only additions to the contract: every `merge:` and `# pause` line is unchanged and a runner skips the new lines like any `#` line. See [docs/queue-output.md](docs/queue-output.md).
- `bundle --apply --open-pr` opens the PR `deploy/<date>-<base>` into the base after the last retarget of each bundle, without merging it. It does not open a second one when the deploy PR already exists, and a failed retarget skips it. Needs Pull requests: write. Opening the PR before the bundle is merged starts a round of CI per merge; for the whole flow prefer `merge --open-pr`.
- `merge --apply --open-pr` opens the deploy PR `deploy/<date>-<base>` into its base only after the last PR of the bundle was merged and its deploy wait ended. Any failed or stopped step before it means nothing is opened, and a deploy PR that already exists is not duplicated. It never opens when the branch is gone, when the deploy PR already exists or when an open PR still targets the train branch. The merged PRs are recorded in the state file, so a run cut off between the last merge and the step opens the PR on the next `merge --apply --open-pr`, even with nothing left to merge. The report gains the step kind `open-pr`; a dry run lists it as planned.
- `merge` ignores a workflow run that ended `cancelled` when a newer run of the same workflow exists for the same commit (CI that cancels in-progress runs on a PR leaves such runs). A cancelled run that nothing replaced still fails the step.

### Changed in `merge`

- A merge into a train branch (`deploy/...`) used to skip the deploy wait without looking. It now reads the workflow files of the repo at the merge commit (`.github/workflows/*.yml`, via the tree and blob API, once per repo and branch in a run) and skips the wait only when none of them runs on a push to that branch, reading the `branches`, `branches-ignore` and `tags` filters of `on: push`. A `paths` filter counts as a possible run. When a workflow runs on the push, or the files cannot be read or parsed (no access, a filter it does not understand, invalid YAML), it waits like for any base and the step says why. This adds the crate `yaml-rust2` (pure Rust YAML parser).
- The second read that confirms the same number of finished runs is there to catch a workflow chained with `workflow_run`. It is not shortened, and it should only be after measuring the real delay of the chain. See [docs/merge.md](docs/merge.md).

The `--json` output of `plan` keeps schema 1; `bundles[].branch` changes as described above and the reports of `bundle --json` and `merge --json` gain the kind `open-pr`.

## 0.3.0

### Added

- `pipetrain merge`: merges the PRs of the plan one at a time, in the order of `plan --emit queue`. Dry run by default; `--apply` asks for confirmation unless `--yes` is given. Before each merge it reads the PR again and refuses one that is not approved, green and `clean`, or whose head moved since the plan. After each merge it waits for the workflow runs of the push to finish before starting the next PR. DDL and infra gates and unpublished package waits ask for confirmation (`--gates-confirmed` answers them). Per repo timeouts live under `[merge]` in the config, and a run that stops can be resumed by running the same command again. Contract and open design questions: [docs/merge.md](docs/merge.md).
- `pipetrain doctor` lists every config file it finds, says which one won and why, and warns when there is more than one.
- Requests to GitHub that only read (REST `GET` and GraphQL queries) are retried up to 4 times with exponential backoff and jitter on `500`, `502`, `503`, `504` and on secondary rate limits (honoring `Retry-After` up to 60 seconds). Each retry is announced on stderr. An exhausted primary rate limit is still reported, not waited out. Writes are never retried.

### Changed

- `plan` and `bundle`: when one repo has bundles on more than one base branch, each deploy branch is named after its base (`deploy/05-11-2026-main`, `deploy/05-11-2026-feature-x`) instead of every bundle proposing the same name. A repo with one bundle keeps `deploy/<date>`. `bundle` refuses, with a message, when two bases would still produce the same branch name.
- A PR whose head or base is a train branch with a base suffix (`deploy/05-11-2026-feature-x`) is discovered as part of that day's train, so the plan keeps seeing it after `bundle --apply`.

The `--json` output of `plan` and the `--emit queue` output keep their contracts; `bundles[].branch` can now carry the base suffix described above.

## 0.2.0

First entry of this file. See the [releases page](https://github.com/lucasbp/pipetrain/releases) for the changes before it.
