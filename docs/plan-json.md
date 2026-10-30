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
| `items` | Item[] | Every ready PR in the plan, sorted by `org`, `repo`, `number`. Gate PRs are included. |
| `gate` | string[] | Ids of the PRs that are gates, sorted by `org`, `repo`, `number`. Empty when there is none. |
| `lanes` | string[][] | Chains of PR ids. Within a lane, PRs merge in the order listed. Gate PRs never appear in a lane. |
| `bundles` | Bundle[] | Suggestions to replace several deploys of one repo with a single one. Empty when there is nothing to bundle. Read-only: nothing is created or retargeted. See `saving-pipelines.md`. |
| `skipped` | SkippedOut[] | PRs found but not ready, or already contained in their base or in the train branch, sorted by `id`. They are not in the plan. |
| `questions` | Question[] | What the planner could not decide. Discovery and scan questions come first, then planner questions. |
| `notes` | string[] | Informational lines. Today only `X depends on Y, already merged`. |
| `stats` | Stats | Request budget and parallelism figures. |

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
| `head` | string | Head branch. |
| `base` | string | Base branch. |
| `sha` | string | Head commit SHA read at plan time. |
| `layer` | string or null | Name of the first `[[layer]]` whose `repos` match `org/repo`. `null` when the repo is in no layer. |
| `scope` | string | Why the PR was discovered: `train`, `shared-branch`, `into-train`, `explicit` or `released`. |
| `gate` | string or null | `"ddl"`, `"infra"` or `null`. |
| `gate_reason` | string or null | For a gate PR, the first changed path that matched (`ddl_globs` or `infra_globs`), or `repo is listed in deploy.infra_repos`. `null` when `gate` is `null`. |
| `manual_pause` | boolean | `true` when the PR is a gate: whoever runs the train must stop and confirm before continuing. |
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

## Bundle

One suggestion: in `repo`, several PRs go to the same `base`, so they would deploy one by one. A single deploy branch would deploy them once. It applies when 2 or more ready PRs of one repo target the same base, and that base is not itself a train branch (does not start with `train_prefix`, compared case-insensitively). Sorted by repo, then base.

| Field | Type | Meaning |
|---|---|---|
| `repo` | string | `org/repo`, as written on the first PR of the group. |
| `base` | string | The shared base branch, for example `main`. |
| `branch` | string | The branch to merge the PRs into: the same value as the top-level `deploy_branch`. |
| `existing_train_pr` | string or null | Id of a PR in the group whose head branch already is `branch` (compared case-sensitively). That PR already carries the others once they merge into it. `null` when no PR in the group has that head. |
| `retarget` | string[] | Ids of the PRs to point at `branch`. Never contains `existing_train_pr`. |
| `deploys_now` | integer | Number of PRs in the group: the deploys without bundling. |
| `deploys_after` | integer | Deploys with the bundle. Always `1`. |
| `saved` | integer | `deploys_now - deploys_after`. |

## SkippedOut

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `org/repo#number`. |
| `title` | string | PR title. |
| `url` | string | PR web URL. |
| `scope` | string | `train`, `shared-branch`, `into-train`, `explicit` or `released`. |
| `head` | string | The head branch of the skipped PR, so a `shared-branch` skip can be traced to the branch name that matched. |
| `reasons` | string[] | One or more of: `already merged`, `closed`, `draft`, `changes requested`, `review required`, `not approved`, `checks pending`, `checks failing`, `no checks reported`, `merge conflict`, `mergeability unknown, retry in a minute`, `already in <ref>`, `already in <ref> (same content)`. A merged or closed PR carries only that one reason. `already in <ref>` also stands alone, where `<ref>` is the branch that already contains the PR head: its base branch, or `deploy_branch`. `already in <ref> (same content)` stands alone too: every file the PR changes has identical content in `<ref>`, though the commits differ (for example after a squash merge). These are the only reasons with a variable part, so match them by the `already in ` prefix; the `(same content)` suffix tells the two apart. |

## Question

| Field | Type | Meaning |
|---|---|---|
| `pr` | string or null | The PR the question is about. `null` for a question about a repo (`tree-truncated`, `manifests-truncated`, `dependency-cycle`). |
| `code` | string | Stable identifier. Full list and meaning in `planner.md`. |
| `message` | string | What is undecided. Free text. |
| `hint` | string | How to resolve it. Free text. |

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
    "deploys_after": 1
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
  "skipped": [
    {
      "id": "acme/billing-api#228",
      "title": "Fix rounding in invoice PDF",
      "url": "https://github.com/acme/billing-api/pull/228",
      "scope": "shared-branch",
      "reasons": ["already in main"]
    }
  ]
}
```

Both examples are hand-written to show shapes, not captured from a run.
