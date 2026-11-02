# Local usage log

An opt-in log of how pipetrain is used, so the tool can be tuned from real use. It is off by default. Nothing is ever sent anywhere: the file stays on your machine and you can delete it at any time.

## Turning it on and off

| Setting | Effect |
|---|---|
| `log = true` in the config (top level) | On. Shared by every profile. |
| `PIPETRAIN_LOG=1` | On, whatever the config says. |
| `PIPETRAIN_LOG=0` or `false` | Off, whatever the config says. |
| `--no-log` | Off for this run. Beats everything else. |

Order of precedence: `--no-log`, then `PIPETRAIN_LOG`, then the config. Without any of them nothing is written.

## What a line holds

One JSON object per run, one line each:

```json
{"time":"2026-11-02T03:40:06Z","version":"0.1.1","command":"update","exit":1,"error":"UpdateError","variant":"Network","duration_ms":2043,"requests":1,"cache_hit":false}
```

| Field | Meaning |
|---|---|
| `time` | UTC time of the end of the run, to the second. |
| `version` | pipetrain version. |
| `command` | Name of the subcommand only: `plan`, `bundle`, `doctor`, `profiles`, `orgs`, `order`, `init`, `feed`, `bump` or `update`. |
| `exit` | Exit code of the run. |
| `error` | Type of the error that ended the run: `ApiError`, `UpdateError` or `Other`. `null` when there was no error. |
| `variant` | Name of the variant of a typed error, for example `RateLimited`. `null` for `Other` and when there was no error. |
| `duration_ms` | Wall-clock time of the run. |
| `requests` | How many GitHub API requests the run made. |
| `cache_hit` | `true` when at least one lookup was answered by the local cache. |

## What is never written

Tokens, org names, repo names, PR numbers or titles, the values of arguments, file paths, diffs, and the text of error messages. The error is recorded only by its type and variant, which come from a fixed list in the code. The `log` command itself is not logged.

## Where it lives

The `pipetrain` folder inside your user data directory (`%APPDATA%` on Windows, `~/.local/share` on Linux, `~/Library/Application Support` on macOS), file `log.jsonl`. Set `PIPETRAIN_LOG_DIR` to use another folder.

When `log.jsonl` would pass 1 MiB it becomes `log.1.jsonl`, the old `log.1.jsonl` becomes `log.2.jsonl` and the old `log.2.jsonl` is deleted. At most 3 files and about 3 MiB are kept.

A failure to write the log (no permission, full disk, a path that cannot be created) is ignored: it never fails or slows down the command.

## Reading it

```
pipetrain log --summary [--json]
```

Read-only. Counts runs per command, failed runs per command, the median duration overall and per command, and the share of runs that ended in each error type. Without a log it says how to turn it on and prints zero runs.

`--json` prints schema 1:

```json
{
  "schema": 1,
  "runs": 4,
  "median_ms": 150,
  "commands": [{ "command": "plan", "runs": 3, "failed": 1, "median_ms": 200 }],
  "errors": [{ "error": "ApiError::RateLimited", "count": 1, "rate": 0.25 }]
}
```

`rate` is the count divided by all runs. Fields are only ever added.
