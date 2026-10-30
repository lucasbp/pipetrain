# Queue output

`pipetrain plan --emit queue` prints the merge queue as plain lines, for a runner that takes `merge:` items. It is part of the stable contract, like `--json` schema 1: only additions, never a change to what exists.

## Streams

- **stdout**: the queue and nothing else. One item per line, no blank line.
- **stderr**: open questions, and `no PRs ready` when the queue is empty.

## Lines

| Line | Meaning |
|---|---|
| `merge:<org>/<repo>#<n>` | Merge this PR. |
| `# pause: confirm DDL/infra before continuing` | Stop here until a human confirms. |
| `# ...` | Any line that starts with `#` is a comment. Skip it. |

## Order

1. Gate PRs (database or infrastructure), in plan order.
2. The pause line, only when at least one PR follows the gate. A gate with nothing after it prints no pause.
3. Every other ready PR by stage, ties in the planner's order.

Lanes are flattened into one list. The plan lets lanes run side by side, a serial runner cannot, so the queue is correct but slower than the lanes. Every `after` edge of the plan is respected.

Only ready PRs are items. Skipped PRs never appear.

## Questions

Each open question is one stderr line, printed before any exit:

```
? acme/orders-api#7: <message>
? (repo): <message>
```

`(repo)` stands for a question that is not about one PR. The queue is still printed.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Queue printed, no open question. An empty queue also exits 0. |
| 1 | An error: no token, bad config, a PR not found, budget reached. `error: ...` on stderr. |
| 2 | Usage mistake, including `--emit` with `--json` (clap's own code). |
| 3 | Queue printed, at least one question open. |

`--color` and `NO_COLOR` have no effect. The `--json` schema and the default text output are unchanged by `--emit`.
