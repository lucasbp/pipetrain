# pipetrain merge

`pipetrain merge` runs the merge order that `pipetrain plan` computed: one PR at a time, only while the PR is still ready, and it waits for the deploy of the target branch to finish before it starts the next one. It is the second command that writes to GitHub, after `bundle --apply`.

Without `--apply` nothing is written. The dry run prints the steps it would take and the PRs the plan left out, and calls GitHub only to build the plan.

```
pipetrain merge [OPTIONS] [PRS]...
pipetrain merge --apply [--yes] [--gates-confirmed] [--allow-questions] [OPTIONS] [PRS]...
```

The PR selection flags (`PRS`, `--all`, `--mine`, `--author`, `--org`, `--date`, `--config`, `--profile`) are the ones of `plan` and `bundle`. The plan is built with `--check-feeds`, so a `# wait` line already says whether the package is published.

## What it merges

The steps come from the same lines that `plan --emit queue` prints (see [queue-output.md](queue-output.md)), in the same order:

| Queue line | Step |
|---|---|
| `merge:<id>` | Merge that PR, then wait for its deploy. |
| `# pause: ...` (DDL or infra gate) | Stop and ask you to confirm that the DDL or infra change is done. |
| `# wait: package ...` | Ask you to confirm that the package is published, unless the line ends with `(published)`. |

Every other `#` line is a comment and is ignored. A PR the plan did not mark as ready is not in the queue, so `merge` cannot merge it. The report lists those PRs under `not_merging` with the reasons the plan gave.

## What it checks before each merge

Right before each merge it reads the PR again, because the earlier merges can change it. It does not merge, and the step is `failed`, if any of these is true:

- The PR is no longer open, or the plan's rules for a ready PR no longer hold (not approved, checks not green, draft, conflict).
- The head commit is not the one the plan saw. The merge request also carries that sha, so GitHub itself refuses a PR that moved in between.
- The base branch is not the one the plan saw.
- GitHub's `mergeable_state` is anything but `clean` (`behind`, `dirty`, `blocked`, `unstable`, `draft`, or still unknown after three tries). pipetrain never updates a branch for you.

## What it waits for

After a merge it takes the merge commit sha and polls `GET /repos/{repo}/actions/runs?head_sha={sha}&event=push`. The deploy is finished when there is at least one workflow run, all of them are `completed`, and two polls in a row saw the same number of runs. A run that ended as anything but `success`, `skipped` or `neutral` (`failure`, `cancelled`, `timed_out`, `action_required`) fails the step, even while other runs are still going.

A run that ended `cancelled` is ignored, not failed, when a newer run (a higher run id) of the same workflow exists for the same commit: CI that cancels in-progress runs leaves such runs behind, and the newer run is the one that counts, so it is still waited for and judged. A cancelled run that nothing replaced, or a newer run that was cancelled too, fails the step.

The two polls in a row exist to catch a workflow chained with `workflow_run`: when the first workflow finishes, the chained one may take a moment to appear, and one poll alone would call the deploy finished before it. The second poll is not to be shortened or dropped without measuring how long a chain really takes to show up in the API.

The wait is skipped, and the step is `done` with a note, when:

- the repo matches `merge.no_deploy_repos`;
- the base of the PR is a train branch (it starts with `train_prefix`) and the workflows of the repo were read and none of them runs on a push to that branch.

For a train branch pipetrain reads the workflow files at the merge commit (`.github/workflows/*.yml` and `*.yaml`, once per repo and branch in a run) and looks at `on: push`:

| `on` | Runs on a push to the branch? |
|---|---|
| `push` with no filter, or `[push, ...]` | yes |
| `push.branches` | yes when a pattern matches (`*` does not cross `/`, `**` does, a leading `!` takes a branch back out) |
| `push.branches-ignore` | yes unless a pattern matches |
| `push.tags` or `push.tags-ignore` alone | no |
| `push.paths` or `paths-ignore` | counted as yes, because it depends on the files |
| no `push` | no |

When a workflow runs on the push, or the files cannot be read or understood (no access, a tree too large to list, invalid YAML, a pattern with `?`, `+`, `[` or `\`), the step waits for the deploy like for any other base and says why: `workflow ci.yml runs on push to deploy/05-11-2026-main` or `could not read the workflows (...)`. Assuming that a push to a train branch deploys nothing is wrong for repos that deploy on `deploy/**`, so it is never assumed.

If no run appears within `merge.no_run_grace_seconds`, the step is `stopped`, not `done`: pipetrain cannot tell "no deploy exists" from "the deploy has not started". List the repo in `merge.no_deploy_repos` if it really has none.

## Opening the deploy PR

```
pipetrain merge --apply --open-pr
```

When the PRs of a bundle were retargeted to `deploy/<date>-<base>` (see `bundle --apply`), the plan merges them into that branch one by one. With `--open-pr`, after the last PR of each such bundle is merged and its wait is over, one more step opens the PR `deploy/<date>-<base>` into the base, with the branch name as title and the merged PRs in the body. It never merges that PR. The step is listed as planned in a dry run.

- It only runs when every step before it ended `done` or `skipped`. A failed or stopped step halts the run, and the open step is `skipped` with `stopped after an earlier failure`.
- It does not duplicate: when an open PR from that branch into the base exists, or GitHub answers that one already exists, the step is `skipped`. When the deploy PR is itself in the queue, it is not opened either.
- The base is not in the PR the bundle left behind, so it is found by name: the one branch of the repo whose name, written the way `bundle` writes a suffix, equals the suffix of the deploy branch. None or several matches, or an old unsuffixed `deploy/<date>` with no PR, fail the step with `open the PR by hand`.
- It survives an interrupted run. With `--open-pr`, every PR merged into a train branch is recorded in the state file under its repo and branch. The record is removed once the step is settled: the PR was opened, one already existed, the deploy PR is in the queue, or the branch no longer exists. A run that ended after the last merge and before the step leaves the record, and the next `merge --apply --open-pr` plans the step again at the end of the queue, even when nothing is left to merge. A record that is left because the step failed stays too, so the next run tries again.
- It is conservative about when to open. It never opens when the branch does not exist, when an open PR from the branch into the base already exists, or when an open PR still targets the train branch (a PR of the bundle that was not ready or was not merged). In that last case the step is `skipped` with the number of that PR, the record stays, and a later run opens the PR once nothing targets the branch. The base is found with the date of the run, so a branch of another day needs `--date` or the PR opened by hand.
- Without `--open-pr` nothing is recorded and a record from an earlier run is ignored.
- It needs Pull requests: write.

`bundle --apply --open-pr` opens the same PR right after the retargets, before anything is merged, so every merge into the branch then runs the PR's checks. `merge --open-pr` is the one to use for the whole flow.

## Confirmation and gates

- `--apply` shows the steps and asks you to type `yes`. `--yes` skips only this question.
- Every gate and unpublished package wait asks again, even with `--yes`. Without a terminal it does not ask: the step is `stopped` and the run ends.
- `--gates-confirmed` answers all of them for you. Use it on the run that follows the gate you did by hand, or when you already did the DDL and infra changes.

## Open questions in the plan

When the plan has open questions (`? ...` lines, the ones that make `plan --emit queue` exit with 3) `--apply` merges nothing and exits with 3. `--allow-questions` merges anyway. The order of the plan is a guess for what the questions are about, so read them first.

## Timeouts

| Setting | Default | What |
|---|---|---|
| `merge.timeout_minutes` | `30` | How long to wait for the deploy of one merge. |
| `merge.repo_timeout_minutes` | none | Table of `"org/repo" = minutes`, overrides the default for that repo (case-insensitive). |
| `merge.poll_seconds` | `15` | Time between two polls. |
| `merge.no_run_grace_seconds` | `120` | How long to wait for the first workflow run to appear. |
| `merge.no_deploy_repos` | `[]` | Globs of repos that have no deploy workflow on push. |
| `merge.method` | `"merge"` | `merge`, `squash` or `rebase`. |

`--timeout-minutes N` overrides the timeout of every repo for one run and `--method` overrides `merge.method`. A timeout stops the run; it never merges the next PR.

```toml
[merge]
method = "merge"
timeout_minutes = 30
no_deploy_repos = ["acme/docs"]

[merge.repo_timeout_minutes]
"acme/api" = 60
```

## Resuming

Before it merges, pipetrain records the PR in a state file. After the deploy passes it removes the record. A run that is stopped, fails, is interrupted or loses its connection leaves the record, and the next `merge --apply` starts by finishing that wait, before it touches the queue:

- A record without a merge commit (the process ended between the request and the answer) is resolved by asking GitHub whether the PR was merged.
- A record whose deploy failed is checked again, so re-running the workflow and then running `merge` again continues.
- The plan itself is not saved. It is built again, so merged PRs are gone from it and the queue continues from the first one still open.

The state file is `pipetrain/merge-state.json` in the user state directory (the local data directory on Windows). `--state FILE` chooses another one. A state file that cannot be read is an error, never a reset.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Dry run, or everything done. |
| `1` | A step failed, or you did not confirm. |
| `3` | The plan has open questions and `--allow-questions` was not given. |
| `5` | Stopped and resumable: a deploy timed out or has no run, a gate was not confirmed, or a state could not be read from GitHub. Run the same command again. |

## JSON

`--json` prints one object. `schema` is `1`; new fields may be added, existing ones keep their meaning.

| Field | Type | Meaning |
|---|---|---|
| `schema` | integer | `1`. |
| `applied` | boolean | `false` for a dry run, or when nothing was confirmed. |
| `stopped` | string, optional | Why it did not start (open questions). |
| `steps` | Step[] | In execution order. Steps resumed from an earlier run come first. |
| `not_merging` | `{id, reasons[]}[]` | PRs the plan left out and why. |
| `questions` | string[] | The `? ...` lines of the plan. |

A step is `{kind, target, status, detail}` plus `merge_sha` and `deploy` when they apply. `kind` is `resume`, `merge`, `gate`, `wait` or `open-pr`. `status` is `planned`, `done`, `skipped`, `failed` or `stopped`; after a stop or a failure the steps that did not run are `skipped` with the detail `stopped after an earlier failure`.

## Not part of it

- It is not in the MCP server: [docs/mcp.md](mcp.md) lists only read-only tools.
- It never approves, never updates a branch, never re-runs a workflow, never reverts, never deletes a branch and opens a PR only with `--open-pr`.
- It does not know whether two PRs are safe together. The order and the gates are the plan's; the rest is your call.

## Design questions left open

These were decided on the conservative side. Each can be loosened later with a new flag or setting without changing what exists.

1. **No workflow run after a merge.** Treated as `stopped`, not as success, because "there is no deploy" and "it has not started" look the same from the API. Should a repo with no run ever count as deployed without being listed in `merge.no_deploy_repos`?
2. **Gates under `--yes`.** `--yes` does not answer gates. Is a separate `--gates-confirmed` the right split, or should a CI run be able to pre-approve a named gate (for example by PR id)?
3. **Open questions.** They block `--apply`. Some questions (a sibling that is not ready, for example) may be acceptable to merge past; is one switch for all of them too coarse?
4. **Branches that are behind.** A `behind` PR fails the step and asks you to update it. Should `merge` offer `--update-branch`, knowing that it re-triggers the PR's checks and invalidates the plan?
5. **One deploy at a time across repos.** Merges are strictly sequential, even for PRs of different repos that the plan puts in the same step. Parallel lanes (`max_parallel` in the plan) are not used.
6. **A deploy that failed after the merge.** pipetrain stops and keeps the record; it does not revert. Whether to roll forward or back is not something a tool should guess.
