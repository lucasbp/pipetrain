# `pipetrain plan --json` contract (schema 1)

`pipetrain plan --json` prints one pretty-printed JSON object to stdout and nothing else on stdout. Progress logs from `--verbose` and errors go to stderr. Consumers (for example Pipedeck) should read stdout only.

The structs live in `src/plan.rs` (`Plan`, `SkippedOut`, `Stats`), `src/planner.rs` (`Item`, `Dependency`, `Rule`) and `src/model.rs` (`Question`). This document follows them field by field.

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
| `train` | string | The train branch (`train_prefix` + `dd-mm-yyyy`), or the literal `explicit PRs` when PRs were passed on the command line. |
| `orgs` | string[] | Organizations searched, after `--org` or the config. |
| `items` | Item[] | Every ready PR in the plan, sorted by `org`, `repo`, `number`. Gate PRs are included. |
| `gate` | string[] | Ids of the PRs that are gates, sorted by `org`, `repo`, `number`. Empty when there is none. |
| `lanes` | string[][] | Chains of PR ids. Within a lane, PRs merge in the order listed. Gate PRs never appear in a lane. |
| `skipped` | SkippedOut[] | PRs found but not ready, sorted by `id`. They are not in the plan. |
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
| `scope` | string | Why the PR was discovered: `train`, `shared-branch`, `into-train` or `explicit`. |
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
| `rule` | string | `depends-on`, `package`, `stacked`, `target`, `layer` or `gate`. |
| `reason` | string | Human sentence naming the cause. Free text: do not parse it. |
| `implied` | boolean | `true` when another predecessor of this item already reaches `id`, so the edge adds no constraint. A viewer can hide it. |

## SkippedOut

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `org/repo#number`. |
| `title` | string | PR title. |
| `url` | string | PR web URL. |
| `scope` | string | `train`, `shared-branch`, `into-train` or `explicit`. |
| `reasons` | string[] | One or more of: `already merged`, `closed`, `draft`, `changes requested`, `review required`, `not approved`, `checks pending`, `checks failing`, `no checks reported`, `merge conflict`, `mergeability unknown, retry in a minute`. A merged or closed PR carries only that one reason. |

## Question

| Field | Type | Meaning |
|---|---|---|
| `pr` | string or null | The PR the question is about. `null` for a question about a repo (`tree-truncated`, `manifests-truncated`). |
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

## Example

```json
{
  "schema": 1,
  "generated_at": "2026-10-29T14:03:11-03:00",
  "train": "deploy/29-10-2026",
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
  "skipped": [],
  "questions": [],
  "notes": [],
  "stats": {
    "requests": 9,
    "rate_remaining": 4870,
    "peak_parallel": 0,
    "max_parallel": 4
  }
}
```

The example is hand-written to show shapes, not captured from a run.
