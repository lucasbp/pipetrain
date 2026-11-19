# Bump needed by the version gate

Part of issue 54. This page covers the prediction in `merge`; executing the bump under `--bump-behind` is a separate change.

## What it does

For each PR in the queue, `merge` looks at the packages its repo consumes from another repo and that are pinned to an exact version. If the number of published versions newer than the pin, plus the releases the queue itself will publish, is above the repo's limit, the PR would fail the version gate. The limit comes from `max-behind` in the workflows or `merge.package_max_behind`; an unreadable limit never holds a PR.

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

## JSON

`merge --json` lists each bump as a step with `kind: bump`. The `bumps` field of the plan model is additive and omitted when empty, see [plan-json.md](plan-json.md).
