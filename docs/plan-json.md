# `pipetrain plan --json` contract (schema 1)

`pipetrain plan --json` prints one pretty-printed JSON object to stdout and nothing else on stdout. Progress logs from `--verbose` and errors go to stderr. Consumers (for example Pipedeck) should read stdout only.

The structs live in `src/plan.rs` (`Plan`, `SkippedOut`, `Stats`), `src/bundle.rs` (`Bundle`), `src/planner.rs` (`Item`, `Dependency`, `Rule`) and `src/model.rs` (`Question`). This document follows them field by field.

## Compatibility guarantee

- New fields are only added. A consumer must ignore fields it does not know.
- `schema` goes up if a field is removed, renamed, changes type, or changes meaning.
- The current value is `1`.
- Arrays are always present, possibly empty. A field typed `T | null` is always present, with `null` when it has no value.

## Plan (top level)

| Field | Type | Meaning |
|---|---|---|
| `schema` | integer | Contract version. `1`. |
| `generated_at` | string | RFC 3339 timestamp with seconds and the local UTC offset, for example `2026-10-29T14:03:11-03:00`. |
| `train` | string | The train branch (`train_prefix` + the date in the configured `train_date_format`, default `dd-mm-yyyy`), the literal `explicit PRs` when PRs were passed on the command line, or the literal `all released PRs` with `--all`. |
| `deploy_branch` | string | The train branch of the run date (`train_prefix` + the date in the configured `train_date_format`, default `dd-mm-yyyy`). Unlike `train`, it is always a real branch name, even when PRs were passed on the command line. It is the branch a bundle suggestion names. Never `null`. |
| `profile` | Profile | The config profile in use: `name` (string, `default` for a flat config) and `source` (string), one of `flag`, `args`, `folder`, `default` or `only`, meaning which selection rule chose it. The field is omitted when no profile was resolved. Added without a schema bump: readers that ignore unknown fields are unaffected. |
| `orgs` | string[] | Organizations searched, after `--org` or the config. |
| `preflight` | Readiness[] | Additive per-PR readiness snapshot, sorted by ID, including items and skipped PRs. See Readiness below. |
| `items` | Item[] | Every ready PR in the plan, sorted by `org`, `repo`, `number`. Gate PRs are included. |
| `gate` | string[] | Ids of the PRs that are gates, sorted by `org`, `repo`, `number`. Empty when there is none. |
| `lanes` | string[][] | Chains of PR ids. Within a lane, PRs merge in the order listed. Gate PRs never appear in a lane. |
| `bundles` | Bundle[] | Suggestions to replace several deploys of one repo with a single one. Empty when there is nothing to bundle. Read-only: nothing is created or retargeted. See `saving-pipelines.md`. |
| `conflicts` | Conflict[] | PRs of the same repo that change the same files. Always present, `[]` when there is none. Read-only: it never changes `items`, `gate` or `lanes`. Added after schema 1 shipped, and additive only. |
| `skipped` | SkippedOut[] | PRs found but not ready, or already contained in their base or in the train branch, sorted by `id`. They are not in the plan. |
| `questions` | Question[] | What the planner could not decide. Discovery and scan questions come first, then planner questions. |
| `notes` | string[] | Informational lines. Today only `X depends on Y, already merged`. |
| `stats` | Stats | Request budget and parallelism figures. |
| `shown_authors` | string[] (optional) | Lowercase logins the author filter keeps visible. Present only when a filter is on. |

The author filter (`--mine`, `--author`, `authors` in the config) only hides. `shown_authors`, `hidden` and `stats.hidden_by_author` are absent without a filter, so an unfiltered plan is byte for byte the same as before.

## Item

One ready PR.

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `org/repo#number`. This is the key used by every other field that refers to a PR. |
| `org` | string | Organization. |
| `repo` | string | Repository name. |
| `number` | integer | PR number. |
| `title` | string | PR title. |
| `url` | string | PR web URL. |
| `author` | string (optional) | The GitHub login of the PR author. Omitted when unknown. |
| `hidden` | boolean (optional) | `true` when the author filter hides this PR. Omitted otherwise. A hidden item keeps its place in `items`, `lanes` and `gate`, because the plan is computed with every author. |
| `head` | string | Head branch. |
| `base` | string | Base branch. |
| `sha` | string | Head commit SHA read at plan time. |
| `layer` | string or null | Name of the first `[[layer]]` whose `repos` match `org/repo`. `null` when the repo is in no layer. |
| `scope` | string | Why the PR was discovered: `train`, `shared-branch`, `into-train`, `explicit` or `released`. |
| `gate` | string or null | `"ddl"`, `"infra"` or `null`. |
| `gate_reason` | string or null | For a gate PR, the first changed path that matched (`ddl_globs` or `infra_globs`), or `repo is listed in deploy.infra_repos`. `null` when `gate` is `null`. |
| `manual_pause` | boolean | `true` when the PR is a gate: whoever runs the train must stop and confirm before continuing. |
| `gate_late` | boolean | Present, and `true`, only for a late gate: a gate PR that runs after the PRs it waits for through a `package`, `dependency` or `target` edge, with the pause right before it. Absent for every other PR, so plans without a late gate are unchanged. |
| `stage` | integer | Position in time, starting at 1. PRs in the same stage can merge at the same time. Gate edges are ignored when computing it, so a gate PR is usually stage 1 and the stage of everything else does not count the wait for the gate. |
| `lane` | integer or null | 1-based index into `lanes`. `null` for a gate PR. |
| `after` | Dependency[] | PRs that must merge before this one. Every gate PR that goes first appears here with `rule` `gate`. |

## Dependency

One edge "the PR in `id` goes before this item".

| Field | Type | Meaning |
|---|---|---|
| `id` | string | The PR that goes first. |
| `rule` | string | `depends-on`, `package`, `stacked`, `order`, `dependency`, `target`, `layer` or `gate`. |
| `reason` | string | Human sentence naming the cause. Free text: do not parse it. |
| `implied` | boolean | `true` when another predecessor of this item already reaches `id`, so the edge adds no constraint. A viewer can hide it. |
| `package` | string | Only on `package` and `dependency` edges: the package name that `id` publishes and this item consumes, as the manifests spell it. Omitted on every other edge. Added after schema 1 shipped, and additive only. |
| `version` | string | Optional. Only on a `package` edge whose version was readable: the version this item bumps the package to, read from the added line of the item's own manifest diff, with a leading `^`, `~`, `>=`, `>`, `=` or `v` and any brackets removed. Omitted when the version is a variable, a wildcard, a range, a Go pseudo-version, a `path`, `workspace` or `git` dependency, or differs between files, and on every other edge, so plans without a readable version are byte-identical to before. Additive only: schema stays 1. |
| `published` | boolean | Optional. Only with `plan --check-feeds`, on a `package` edge that has a `version`, when the feed answered: `true` when the version is published, `false` when it is not yet. Omitted without the flag and when the check failed. Informational: it never changes `items`, `gate` or `lanes`. Additive only: schema stays 1. |
| `feed_error` | string | Optional. Only with `plan --check-feeds`, on the same edges, when the feed could not answer (refused with 401 or 403, package not found, unreachable). One line of free text: do not parse it. For a refusal it says `needs read:packages`. Omitted without the flag and when the feed answered. Additive only: schema stays 1. |

## Bundle

One suggestion: in `repo`, several PRs go to the same `base`, so they would deploy one by one. A single deploy branch would deploy them once. It applies when 2 or more ready PRs of one repo target the same base, and that base is not itself a train branch (does not start with `train_prefix`, compared case-insensitively). Sorted by repo, then base.

| Field | Type | Meaning |
|---|---|---|
| `repo` | string | `org/repo`, as written on the first PR of the group. |
| `base` | string | The shared base branch, for example `main`. |
| `branch` | string | The branch to merge the PRs into: the top-level `deploy_branch` followed by `-` and the base, always (`deploy/29-10-2026-main`, `deploy/29-10-2026-feature-x`; the base in lowercase, every character that is not a letter, digit, `.` or `_` turned into `-`), so that two bundles never propose the same branch and a branch is never reused across bases. Before 0.4.0 the suffix appeared only when one repo had bundles on more than one base. An open PR in the group whose head is exactly the unsuffixed `deploy_branch` (from a bundle made by an older version) is kept as `existing_train_pr` and `branch` carries its name. |
| `existing_train_pr` | string or null | Id of a PR in the group whose head branch already is `branch` (compared case-sensitively). That PR already carries the others once they merge into it. `null` when no PR in the group has that head. |
| `retarget` | string[] | Ids of the PRs to point at `branch`. Never contains `existing_train_pr`. |
| `deploys_now` | integer | Number of PRs in the group: the deploys without bundling. |
| `deploys_after` | integer | Deploys with the bundle. Always `1`. |
| `saved` | integer | `deploys_now - deploys_after`. |

## Conflict

A file-overlap report between two ready PRs of the same repo: a hint that they may conflict, not a merge simulation. PRs stacked on each other (a `stacked` edge, either direction) are left out. Sorted by `a`, then `b`.

| Field | Type | Meaning |
|---|---|---|
| `a` | string | Id of the first PR, `org/repo#number`. |
| `b` | string | Id of the second PR. Sorts after `a`. |
| `repo` | string | `org/repo`, lowercase. |
| `files` | string[] | The paths both PRs change, sorted. |
| `kind` | string | `lockfile` when every shared file is a lockfile (`package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `packages.lock.json`), `sql` when every one is a `.sql` file under a `db` or `migrations` folder, `generated` when every one is under an `obj` or `bin` folder, `other` otherwise, including mixed kinds. |

## SkippedOut

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `org/repo#number`. |
| `title` | string | PR title. |
| `url` | string | PR web URL. |
| `author` | string (optional) | The GitHub login of the PR author. Omitted when unknown. |
| `hidden` | boolean (optional) | `true` when the author filter hides this PR. Omitted otherwise. |
| `scope` | string | `train`, `shared-branch`, `into-train`, `explicit` or `released`. |
| `head` | string | The head branch of the skipped PR, so a `shared-branch` skip can be traced to the branch name that matched. |
| `reasons` | string[] | One or more of: `already merged`, `closed`, `draft`, `changes requested`, `review required`, `not approved`, `checks pending`, `checks failing`, `checks failing: <names>`, `no checks reported`, `merge conflict`, `branch behind its base, update it`, `already in <ref>`, `already in <ref> (same content)`. `branch behind its base, update it` is read from GitHub's `mergeStateStatus` and appears only when it is `BEHIND`, which GitHub reports when branch protection requires the branch to be up to date; a PR with a merge conflict carries `merge conflict` instead. A merged or closed PR carries only that one reason. `already in <ref>` also stands alone, where `<ref>` is the branch that already contains the PR head: its base branch, or `deploy_branch`. `already in <ref> (same content)` stands alone too: every file the PR changes has identical content in `<ref>`, though the commits differ (for example after a squash merge). `checks failing: <names>` lists the failing checks, for example `checks failing: build, test`: sorted, at most 5 names and then `, +N more`; the plain `checks failing` stays when GitHub gives no name. Full merge-state blockers can also add `branch protection blocks merge (BLOCKED)`, `merge conflict (DIRTY)`, `checks are not passing (UNSTABLE)` and `draft (DRAFT)`. UNKNOWN mergeability is a preflight warning rather than a skipped reason. The `already in ` and `checks failing` reasons have a variable part, so match them by the `already in ` and `checks failing` prefixes; the `(same content)` suffix tells the two `already in` forms apart. |

## Question

| Field | Type | Meaning |
|---|---|---|
| `pr` | string or null | The PR the question is about. `null` for a question about a repo (`tree-truncated`, `manifests-truncated`, `dependency-cycle`). |
| `code` | string | Stable identifier. Full list and meaning in `planner.md`. |
| `message` | string | What is undecided. Free text. |
| `hint` | string | How to resolve it. Free text. |
| `hidden` | boolean (optional) | `true` when the question is about a PR hidden by the author filter. Omitted otherwise. A question without a PR is never hidden. |

Consumers should switch on `code`, never on `message` or `hint`.

## Stats

| Field | Type | Meaning |
|---|---|---|
| `requests` | integer | GitHub requests made by this run. |
| `rate_remaining` | integer or null | Last `x-ratelimit-remaining` value seen. `null` when no response carried it. |
| `peak_parallel` | integer | Largest number of non-gate PRs in one stage. |
| `max_parallel` | integer | The configured `max_parallel`. It is a cap shown for reference: the planner does not split stages to respect it. |
| `deploys_now` | integer | Items in the plan whose base is not a train branch: the deploys if nothing is bundled. |
| `deploys_saved` | integer | Sum of `saved` over `bundles`. `0` when there are no bundles. |
| `deploys_after` | integer | `deploys_now - deploys_saved`: the deploys if every suggestion is applied. |
| `cache_hits` | integer | Lookups answered by the local cache. `0` with `--no-cache` or `--refresh`. |
| `cache_misses` | integer | Lookups the cache could not answer, so they went to GitHub. `0` with `--no-cache`. |
| `elapsed_ms` | integer | Milliseconds of wall clock from the start of the command until the data was gathered, before rendering. |
| `feed_checks` | integer (optional) | Distinct package and version pairs looked up in a feed by `plan --check-feeds`. Omitted when `0`, so a plan without the flag is byte-identical to before. |
| `hidden_by_author` | integer (optional) | How many PRs (items and skipped) are hidden by the author filter. Omitted when `0`. |

## Example

```json
{
  "schema": 1,
  "generated_at": "2026-10-29T14:03:11-03:00",
  "train": "deploy/29-10-2026",
  "deploy_branch": "deploy/29-10-2026",
  "profile": {"name": "default", "source": "only"},
  "orgs": ["acme"],
  "items": [
    {
      "id": "acme/orders-api#41",
      "org": "acme",
      "repo": "orders-api",
      "number": 41,
      "title": "Add order status column",
      "url": "https://github.com/acme/orders-api/pull/41",
      "head": "deploy/29-10-2026",
      "base": "main",
      "sha": "0123456789abcdef0123456789abcdef01234567",
      "layer": "api",
      "scope": "train",
      "gate": "ddl",
      "gate_reason": "db/migrations/0042_status.sql",
      "manual_pause": true,
      "stage": 1,
      "lane": null,
      "after": []
    }
  ],
  "gate": ["acme/orders-api#41"],
  "lanes": [],
  "bundles": [],
  "conflicts": [],
  "skipped": [],
  "questions": [],
  "notes": [],
  "stats": {
    "requests": 9,
    "rate_remaining": 4870,
    "peak_parallel": 0,
    "max_parallel": 4,
    "deploys_saved": 0,
    "deploys_now": 1,
    "deploys_after": 1,
    "cache_hits": 0,
    "cache_misses": 0,
    "elapsed_ms": 14600
  }
}
```

A non-empty `bundles` entry, for two PRs of `acme/billing-api` against `main`, and the matching `already in` skip:

```json
{
  "bundles": [
    {
      "repo": "acme/billing-api",
      "base": "main",
      "branch": "deploy/29-10-2026",
      "existing_train_pr": null,
      "retarget": ["acme/billing-api#230", "acme/billing-api#233"],
      "deploys_now": 2,
      "deploys_after": 1,
      "saved": 1
    }
  ],
  "conflicts": [
    {
      "a": "acme/billing-api#230",
      "b": "acme/billing-api#233",
      "repo": "acme/billing-api",
      "files": ["package-lock.json"],
      "kind": "lockfile"
    }
  ],
  "skipped": [
    {
      "id": "acme/billing-api#228",
      "title": "Fix rounding in invoice PDF",
      "url": "https://github.com/acme/billing-api/pull/228",
      "scope": "shared-branch",
      "reasons": ["already in main"]
    },
    {
      "id": "acme/billing-api#231",
      "title": "Add retry to the webhook",
      "url": "https://github.com/acme/billing-api/pull/231",
      "scope": "train",
      "reasons": ["checks failing: build, test"]
    }
  ]
}
```

Both examples are hand-written to show shapes, not captured from a run.

## Readiness

`preflight` is additive; schema stays `1` and existing fields keep their names, types and meanings. It covers the selected PRs, including those excluded by readiness or containment. `items`, `lanes`, `gate` and `after` still define the queue and its ordering. A waiting PR can remain in the queue because an earlier step or manual confirmation can release it. Match `id` against the existing item/skipped `hidden` flags when applying author visibility. Merge reports include only visible preflight entries.

| Field | Type | Meaning |
|---|---|---|
| `id` | string | PR ID, `org/repo#number`. |
| `status` | string | `eligible`, `blocked` or `waiting`. Eligible passes the current preflight; blocked has readiness/containment causes and is excluded from the queue; waiting passes readiness but has internal predecessors, unresolved questions or a manual gate. |
| `causes` | string[] | Human explanations. Eligible uses `preflight passed`; waiting names earlier PRs and their ordering reasons, unresolved questions or the manual gate. Do not parse this text. |
| `next_step` | string | How to proceed. Blocked checks ask for inspection/fix or rerun, without inferring policy from a check name. |
| `base` | string | PR base branch, including for skipped PRs. |
| `default_branch` | string | Repository default branch from `repository.defaultBranchRef.name`; empty when unavailable. |
| `merge_state_status` | string | Full GraphQL `mergeStateStatus`, preserved as returned; empty when unavailable. The internal `behind` flag is retained. |
| `warnings` | string[] | Informational, never new ordering edges. UNKNOWN or unknown mergeability warns `GitHub still computing`; the executor retries for up to a minute. A non-default, non-train base warns `base not landed` when no other selected PR in the same repo has that base as its head. Missing default-branch metadata does not invent a destination. |
| `checks` | CheckDetail[] | Reported check contexts, sorted and deduplicated by name, state and URL. Each is `{name: string, state: string, url: string or null}`. CheckRun uses conclusion, falling back to status, and `detailsUrl`; StatusContext uses state and `targetUrl`. Unknown URLs remain null. |

BLOCKED, DIRTY, UNSTABLE, DRAFT and BEHIND block through the same readiness rules used for executor revalidation. CLEAN and HAS_HOOKS pass this preflight. UNKNOWN only warns; any independent blocker still blocks. The executor's final REST check still requires exactly `clean`, including after a HAS_HOOKS preflight. Passing a plan never substitutes for that live check, the head SHA guard, approval or checks. Feature-base warnings do not retarget PRs or change stack order.

## Strict exit code

`plan --strict` exits `4` when no visible selected PR is `eligible`, including an empty selection or a selection consisting only of blocked/waiting PRs. It still prints the full requested text, JSON or queue report. This condition takes precedence over queue exit `3` for open questions. If an eligible PR exists, the previous exit behavior remains, including queue exit `3` for questions. Without `--strict`, normal text/JSON plans retain exit `0` and queue mode retains its existing question/error codes. Errors and usage failures retain their existing codes.

`merge --strict` uses the same code for a fully ineligible selection or a dry run with no merge step; see [merge.md](merge.md). Code `4` was previously unused by the binary.
