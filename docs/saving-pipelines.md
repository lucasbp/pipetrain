# Saving deploys

Two things in the plan exist to cut the number of deploys: the "already in the base or in the train" check and the bundle suggestion. Both are read-only. This page explains why they exist, the arithmetic, and how to apply the suggestion by hand.

## Why a PR can be left out: already in the base

A PR whose commits are already in its base branch has nothing to merge. Merging it again, or planning it, wastes a step and can trigger a deploy that changes nothing. Before planning, pipetrain compares the head commit of each ready PR with its base branch and, when the PR is not part of the train itself, with the train branch (`train_prefix` plus the date, for example `deploy/29-10-2026`). If the PR adds no commit to that branch, it goes to `SKIPPED` with the reason `already in <branch>`.

That comparison is by commits, so a PR whose change reached the base through a squash merge or a rebase still looks ahead. A second check covers it: when every file the PR changes already has identical content in the base or the train branch, the PR goes to `SKIPPED` with the reason `already in <branch> (same content)`. It does not follow renamed files and skips PRs with more than 100 files; close those on GitHub if they are redundant.

## Why the plan suggests a deploy branch

Each PR that targets a base such as `main` is a deploy of that repo. Two PRs of `acme/billing-api` against `main` are two deploys, and each one runs its pipeline. If both are merged into a train branch first, and that branch is merged into `main` once, the repo deploys once.

The plan suggests this when 2 or more ready PRs of the same repo target the same base, and that base is not already a train branch.

## The arithmetic

For one repo and one base with N ready PRs:

- Without bundling: N deploys.
- With a bundle: 1 deploy, the single PR from the train branch to the base.
- Saved: N - 1.

Across the whole plan, in `stats` (and the `deploys` line of the text output):

| Value | Meaning |
|---|---|
| `deploys_now` | Ready PRs in the plan whose base is not a train branch. Every one is a deploy if nothing is bundled. This counts all of them, including PRs that are in no bundle. |
| `deploys_saved` | The sum of `saved` over all bundles. |
| `deploys_after` | `deploys_now - deploys_saved`. |

Example: 7 ready PRs, 2 of them in one bundle. `deploys_now` is 7, `deploys_saved` is 1, `deploys_after` is 6. The text output prints `deploys   7 now, 6 after bundling`.

When a PR of the group already has the train branch as its head (`existing_train_pr` in the JSON), the plan does not suggest creating the branch again. The other PRs are retargeted to that branch, and that PR is the single deploy PR.

## How to apply it by hand

pipetrain only plans. A command that executes a plan does not exist yet, so the team does these steps itself, for each entry under `BUNDLES`:

1. Create the branch named in the suggestion (`deploy/<date>`) from the base.
2. Change the base of each PR listed under `retarget` to that branch.
3. Merge those PRs into the branch, in the order the plan gives.
4. Open one PR from the branch to the original base, and merge it. That is the one deploy.

When `existing_train_pr` is set, skip steps 1 and 4: retarget the PRs to that PR's branch, merge them there, and merge that PR into the base.

Run `pipetrain plan` again afterwards. Retargeted PRs now have a train branch as their base, so they are no longer counted in `deploys_now` and no longer bundled.

## What `plan` does not do

- It never creates a branch.
- It never changes the base of a PR.
- It never merges, comments or writes anything to GitHub.
- It does not check that the bundle is safe. It only counts deploys. Whether the PRs can ship together is a decision for whoever runs the train.

The JSON fields are in [plan-json.md](plan-json.md) (`bundles`, `deploy_branch`, `stats`). The rules that order the PRs are in [planner.md](planner.md).
