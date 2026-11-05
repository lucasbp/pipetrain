# Changelog

## 0.4.0

### Changed

- `bundles[].branch` in `plan --json` (and the branch `bundle` creates) now always carries the base as a suffix: `deploy/<date>-<base>`, for example `deploy/05-11-2026-master`. Until 0.3.0 the suffix appeared only when one repo had bundles on more than one base, so anything that read `bundles[].branch` and expected `deploy/<date>` for a single bundle must read the field instead. An open PR whose head is exactly the unsuffixed `deploy/<date>` of an older bundle is still taken as the train PR and keeps its name.
- `bundle --apply` never reuses a deploy branch that came from another base. When the branch already exists it must contain the tip of the bundle's base, otherwise `bundle` stops with `refusing to reuse the branch` before writing anything (a dry run reports it too). The check is repeated if GitHub answers that the branch already exists at the moment of the create.
- A PR whose branch GitHub reports as `BEHIND` its base (`mergeStateStatus`, which branch protection that requires up to date branches produces) is now listed under `SKIPPED` with the reason `branch behind its base, update it`. Nothing else that is ready changes.

### Added

- `plan --emit queue` prints `# step <n>` and `# lane <n>` before each PR (the `stage` and `lane` of the plan; a gate PR has no lane line), so a runner can run lanes side by side. Only additions to the contract: every `merge:` and `# pause` line is unchanged and a runner skips the new lines like any `#` line. See [docs/queue-output.md](docs/queue-output.md).
- `bundle --apply --open-pr` opens the PR `deploy/<date>-<base>` into the base after the last retarget of each bundle, without merging it. It does not open a second one when the deploy PR already exists, and a failed retarget skips it. Needs Pull requests: write. Opening the PR before the bundle is merged starts a round of CI per merge; for the whole flow prefer `merge --open-pr`.
- `merge --apply --open-pr` opens the deploy PR `deploy/<date>-<base>` into its base only after the last PR of the bundle was merged and its deploy wait ended. Any failed or stopped step before it means nothing is opened, and a deploy PR that already exists is not duplicated. The report gains the step kind `open-pr`; a dry run lists it as planned.
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
