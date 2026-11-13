# feed --wait

`pipetrain feed <package> --version <v> --wait` blocks until a version is published in its feed. This page is the reference for its behavior and contract. The lookup itself (ecosystems, feeds, token handling) is described in the README under "Checking a package feed".

## Command

```
pipetrain feed <PACKAGE> --version <V> --wait [--interval <S>] [--timeout <S>] [--no-progress] [--json]
```

- `--version` is required. Without it the command stops with `--wait requires --version` (exit 1).
- `--interval` is the number of whole seconds between checks. Default 10, minimum 1.
- `--timeout` is the total number of whole seconds to wait. Default 600. `0` means a single check, with no waiting.
- `--interval`, `--timeout` and `--no-progress` are only valid with `--wait`; otherwise the command stops with exit 1.
- Ecosystem, `--org`, `--registry` and the token work exactly as in `feed`.

Usage errors of `--wait` exit with 1 and not with the clap default of 2, because 2 is reserved for a refused token.

## Loop

1. Check the feed with the same lookup as `feed`.
2. If the version is published, stop with exit 0.
3. If the feed answered 401 or 403, stop with exit 2. No retry.
4. If the elapsed time reached the timeout, stop with exit 1 (not published) or 3 (last attempt failed).
5. Otherwise sleep for `--interval`, shortened to the time left before the timeout, and go to 1.

Every attempt runs even when `--timeout` is smaller than `--interval`: the first attempt is immediate, and the last pause is cut so the total wait does not pass the timeout.

What counts as what:

| Answer from the feed | Treated as |
|---|---|
| The version is listed | Published. |
| The package exists and the version is not listed | Not yet published, retried. |
| The package does not exist (404) | Not yet published, retried. A first release has no package until it is pushed. |
| Network failure, timeout of the request, HTTP 5xx, invalid JSON | A failed attempt, retried. |
| 401 or 403 | Refused. Stops at once. |

The outcome at the timeout depends on the last attempt. If it reached the feed and the version is missing, the exit is 1. If it failed, the exit is 3.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Published. |
| 1 | Timeout with the version not published, or a usage error. |
| 2 | Refused by the feed (401 or 403). The token is missing or lacks `read:packages`. |
| 3 | The last attempt at the timeout failed with a network, HTTP or parsing error. |

Without `--wait` the exit codes of `feed` do not change.

## Output

stdout carries the final result once. Everything else goes to stderr.

Published:

```
acme-util 2.0.0: published
```

Timeout:

```
acme-util 2.0.0: not published after 600s (latest 1.4.0)
```

`(latest ...)` is left out when the feed lists no version. The number is the elapsed time in whole seconds, rounded to the nearest second.

Exit 2 and exit 3 print nothing on stdout. Their reason goes to stderr as `<package> <version>: gave up after <N>s: <reason>`, or the usual `read:packages` hint for a refusal.

Between attempts stderr gets one line per attempt, for example:

```
acme-util 2.0.0: not published yet (attempt 3); checking again in 10s
acme-util 2.0.0: attempt 4 failed (registry.example.com answered with status 502); trying again in 10s
```

On a terminal a spinner shows the elapsed time while it waits. It is off with `--no-progress`, `--json`, `CI=true` and a redirected stderr.

## JSON

`--json` prints the same object as `feed --json` plus four fields:

| Field | Type | Meaning |
|---|---|---|
| `waited` | boolean | Always `true` in this mode. |
| `attempts` | integer | How many times the feed was checked. |
| `elapsed_seconds` | integer | Whole seconds from the first attempt to the end. |
| `timed_out` | boolean | `true` when the wait ended by timeout. |

`schema` stays 1. The four fields appear only with `--wait`, so the output of `feed --json` without it is unchanged. Exit 2 and exit 3 print nothing on stdout in `--json` mode either.

## Use in a runner

```
pipetrain feed Acme.Util --org acme --version 2.0.0 --wait --timeout 900 --no-progress
```

The step succeeds when the version is published, fails with 1 on timeout, and fails with 2 or 3 when the check itself could not be done. A runner that wants to treat 3 as "try the job again later" can branch on the code.

## Not in scope

`plan --check-feeds` still checks each feed once and does not wait. `--wait` never writes to a repo or a feed.

## Package waits during merge

`merge --apply` reuses this lookup and wait loop when it reaches a package wait with a known version. It rechecks the feed after the publisher finishes, even when the plan's earlier snapshot says published, absent or failed. The ecosystem and publishing organization are inferred using the same rules as `plan --check-feeds`.

`[merge]` configures this wait separately from deploy polling: `package_poll_seconds = 10` and `package_wait_seconds = 600` by default. The interval has a minimum of 1 second; `0` is normalized to 1. A zero package timeout performs one lookup without sleeping. The CLI's `--timeout-minutes` overrides deploy waits only. As with `feed --wait`, an HTTP request in flight has its own request timeout and can finish after the polling deadline.

A published version releases the consumer without a prompt. The wait step retains status `skipped` and records the package, version and feed URL in the existing `detail` field, in text and merge JSON. No JSON fields or schemas change. If the version remains absent at the timeout, merge keeps its existing terminal question, non-interactive `stopped` result or confirmation through `--gates-confirmed`. A missing version keeps manual confirmation without a lookup.

401/403 stops immediately with the authentication cause, without asking or accepting `--gates-confirmed` as feed evidence. Other failed attempts are retried, and a failed final attempt stops with its cause. The standalone `feed --wait` exit codes above remain unchanged; a stopped merge package step uses merge's existing exit code 5. Dry-run merge does not wait on feeds. This reuse does not add writes to any feed or GitHub.
