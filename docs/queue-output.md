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
| `# bundle <org>/<repo>: <N> PRs into <base> could be 1 deploy (saves <N-1>): <PR ids>` | Informative. One per suggested deploy branch. The runner skips it like any `#` line. |
| `# conflict: <PR id> and <PR id> both change <N file(s)> (<kind>)` | Informative. Two PRs of the same repo change the same files. `<N file(s)>` reads `1 file` or `N files`, and `<kind>` is `lockfile`, `sql`, `generated` or `other`. The runner skips it like any `#` line. |
| `# wait: package <pkg> from <PR ids> must be published before <org>/<repo>#<n>` | The PRs after `from` publish `<pkg>` and the PR in the `merge:` line right below bumps it to the version they publish. The runner stops there until the package is in the feed. pipetrain does not query the feed. A runner that does not understand the line ignores it like any `#` line. |
| `# ...` | Any line that starts with `#` is a comment. Skip it. |

## Wait comments

Only a PR that bumps the version of a package that other PRs of the same queue publish waits (edge rule `package`). One `# wait` line is printed immediately before the consumer's `merge:` line for each such package, naming every publisher in the queue, sorted by id. A PR that only references the package (edge rule `dependency`) has an ordering edge and no `# wait` line: it can use the version already in the feed. Gate PRs get the line too. Several lines for one consumer are sorted by package. The line names no version: the planner only knows the package name, and it makes no request to learn the published version. A queue with no such edge has no `# wait` line and is byte-identical to the same queue before this line existed. Every `merge:` and `# pause` line is the same with or without the `# wait` lines, in the same relative order.

## Bundle comments

When the plan suggests a deploy branch for a repo, the queue starts with one `# bundle` line per suggestion, before the first `merge:` line and before the gate. The PR ids are the PRs the plan would carry in that branch, the existing train PR first when there is one. The bundles keep the order of the plan's `bundles`. The saving only happens if a person creates the day's deploy branch first and retargets those PRs: the queue itself does not change. Every `merge:` and `# pause` line is the same with or without the bundle lines, in the same relative order.

## Conflict comments

When two ready PRs of the same repo change the same files, the queue starts with one `# conflict` line per pair, before the `# bundle` lines and before the first `merge:` line. The format is `# conflict: <a> and <b> both change <N file(s)> (<kind>)`, with the pairs in the order of the plan's `conflicts`. It is a report and never changes the order: every `merge:` and `# pause` line is the same with or without the conflict lines, in the same relative order. PRs stacked on each other are not reported.

## Order

1. Conflict comments, then bundle comments, when the plan has any. They are not items.
2. Gate PRs (database or infrastructure) that have no predecessor, in the order of the rule below.
3. The pause line, only when at least one PR follows those gates. A gate with nothing after it prints no pause.
4. Every other ready PR, in the order of the rule below, with the late gates among them.

The rule: by stage first, then, inside one stage, by layer, then PR number, then repo slug (`org/repo`). The layer order is the order of the `[[layer]]` entries in the config, the first entry goes first (for example lib, client, api, bff, front). A PR whose repo is in no layer comes after every layered PR of its stage. The tiebreak only reorders PRs of the same stage: stages and `after` edges are never changed.

## Late gates

The rule "gate PRs first" has one exception. A gate PR that must come after another PR by a `package`, `dependency` or `target` edge is a late gate: it is placed by the same rule as every other PR and printed as

```
# gate: <org>/<repo>#<n> waits for <PR ids> (<rule>: <detail>)
# pause: confirm DDL/infra before continuing
# wait: package <pkg> from <PR ids> must be published before <org>/<repo>#<n>
merge:<org>/<repo>#<n>
```

The `# wait` line is optional: it appears only when the gate bumps a package that another PR of the queue publishes. The ids after `waits for` are the predecessors by those three rules, sorted and without repeats. The detail lists each distinct `rule: text` pair joined by `; `, where `text` is the package name when the edge has one, else the reason of the edge. The pause line is never printed twice in a row: when the queue has early gates and the first PR after them is a late gate, the late gate's own pause serves. Every other `merge:` line keeps its place relative to the other non-gate PRs. Example:

```
merge:acme/kit#6
merge:acme/other#7
# gate: acme/web#5 waits for acme/kit#6 (package: @acme/kit)
# pause: confirm DDL/infra before continuing
# wait: package @acme/kit from acme/kit#6 must be published before acme/web#5
merge:acme/web#5
```

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
