# Bump needed by the version gate

Part of issue 54. This page covers the prediction in `merge` and its execution under `--bump-behind`.

## What it does

For each PR in the queue, `merge` looks at the packages its repo consumes from another repo and that are pinned to an exact version. If the number of published versions newer than the pin, plus the releases the queue itself will publish, reaches the repo's limit (the gate fails at `behind >= limit`; pre-releases are not counted), the PR would fail the version gate. The limit comes from `max-behind` in the repo workflows, including the reusable workflows they call, or `merge.package_max_behind`; an unreadable limit never holds a PR.

The PR is then left out as `waiting: needs a bump of <package> to <version>` and `merge` shows a `bump` step before it. `plan` and `bundle` do not query feeds for this and are unchanged.

## Why these rules

- Same major only. A major bump is a breaking change that a person must choose.
- Stable only. A prerelease never satisfies a feed check that people read as released.
- The target is the newest stable in the major, since a smaller step would fall behind again at once.
- Only exact pins. A range or floating version moves by itself and has nothing to rewrite.
- A pin newer than every version in the feed was already bumped to what the queue publishes, so the queue's releases are not counted against it.
- When only the queue's releases push the pin behind, there is no feed version to name. The target is shown as `the version the queue publishes`.

## Waiting, not blocked

The cause is a `waiting` cause: it resolves with a bump PR, not a failing check. Dependents of the held PR wait with `depends on <repo#n>, which is waiting`, using the existing propagation.

## Executing it: `--bump-behind`

Without the flag `merge --apply` behaves as before and the step stays a prediction. With `merge --bump-behind --apply`, before the merges, each held PR with a named target version gets:

1. A lookup of the PR on GitHub to learn its base branch (`master` or `main`, the default branch of the consumer).
2. A branch `pipetrain/bump/<package>-<version>` cut from the tip of that base, with the pin rewritten in every manifest that pins the package below the target. The held PR is not involved in this branch, so every held PR of the same repo, package and version shares one bump PR.
3. The same proof `bump --apply` runs, chosen by `--restore-mode locked|unlocked|skip` and `--solution`, or by `[bump]` and `[bump.repos]` in the config (flag, then repo, then global, then `locked`). The branch is pushed only after the proof passes.
4. A draft PR from that branch into the base branch. Once a person merges it, the base carries the new pin.

The held PR still carries the old pin in its own branch, so the step does not end at the merge of the bump PR. Once it is merged, `--bump-behind` updates the branch of the held PR with its base (GitHub `update-branch`, which merges the base into the branch and never rebases), and the next plan reads the new pin and releases the PR to the queue, with the gate and preflight evaluated again. This is deliberate: the bump never lands as a commit on the feature branch, so the review the reviewer approved is not invalidated by the pin and Pullfrog does not ask for another review of the feature because of it. The only new commit on that branch is the merge of the base.

If the update conflicts, the held PR becomes `blocked: merge conflict` (a `blocked` cause, not `waiting`, since a person has to resolve it) and the queue goes on. Dependents of that PR follow the existing propagation and wait for it.

The bump PR is saved in the state file under `bump_behind` (`id`, `package`, `from`, `to`, `number`, `pull_request_url`, `branch`, `recorded_at`), one entry per held PR; entries of the same repo, package and version point at the same bump PR. The field is only added to the state, the schema number does not change, and a state file with it keeps the run resumable. The next run looks at the saved PR before anything else:

| Saved PR | Step | Held PR |
|---|---|---|
| open | `waiting`, names the URL, nothing is opened | stays `waiting` |
| merged | `done`, branch updated with the base, entry removed | released by the next plan, which reads the new pin |
| merged, update conflicts | `waiting` is not used: the PR is `blocked: merge conflict`, entry removed | a person resolves the conflict |
| closed without merge | `failed`, entry removed | the next run opens a new one |

If the base already pins the version (the bump PR was merged and the entry was lost, or someone bumped by hand), no bump PR is opened and the update happens at once. Running the command twice therefore opens one bump PR, and repeating it after the merge repeats only the idempotent update. If the state file is lost, an existing bump branch is detected and nothing is pushed over it.

The release itself happens in the next run: the plan of the current run was already read, so the updated PR is evaluated again, with gate and preflight, by the next `merge`.

Pipetrain never merges the bump PR: it is opened as a draft, so a person marks it ready and the repo's own review rules apply. This is also why the held PR stays `waiting` and does not change the exit code. Merging a bump PR that needs no review once its CI is green is not done by this change.

A held PR that needs a version the queue itself publishes (no `to`) is not bumped by `--bump-behind`: `--bump` prepares that one after the publisher deploys.

A failed bump (restore, build, push or PR creation) is a `failed` step and exits 1, because a real operation failed. It follows `--dependency-failure`: with `skip` only the PRs that depend on the held one wait and independent PRs merge; with `stop` the run stops when some PR depends on it. `waiting` never changes the exit code.

## JSON

`merge --json` lists each bump as a step with `kind: bump`. The `bumps` field of the plan model is additive and omitted when empty, see [plan-json.md](plan-json.md).
