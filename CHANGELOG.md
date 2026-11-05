# Changelog

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
