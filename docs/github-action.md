# GitHub Action

`action.yml` at the root of the repo is a composite action that runs `pipetrain plan` in a workflow. It is read-only: no step writes to the repo or to GitHub. `pipetrain bundle` is not part of it.

```yaml
- uses: lucasbp/pipetrain@v0.2.0
  with:
    refs: acme/api#12 acme/web#3
```

The action only works from a tag that contains `action.yml`. `v0.2.0` is the first. To follow `main` instead, pin a commit SHA and set `version` too, because a SHA names no release.

A full workflow is in [examples/pipetrain-plan.yml](examples/pipetrain-plan.yml).

## What it does

1. Picks the release: `version` if set, otherwise the release tag the action was referenced by, otherwise (a branch or a SHA) the latest full release.
2. Downloads the archive for the runner from the GitHub Release (`pipetrain-v<version>-<target>.tar.gz`, `.zip` on Windows) and `SHA256SUMS`, and checks the SHA-256. A mismatch or a missing line stops the step before anything is installed.
3. Puts the executable in a temp folder and adds it to `PATH` for the rest of the job.
4. Runs `pipetrain plan` with `--no-progress --color never`.
5. Writes the result to the step outputs and to the step summary, then exits.

Runners: Linux x86_64 and arm64, macOS arm64 and x86_64, Windows x86_64. Windows on ARM has no release asset.

## Inputs

| Input | Default | Meaning |
|---|---|---|
| `version` | the action's own tag | Release to install, `0.2.0` or `v0.2.0`. |
| `refs` | empty | PRs to plan, separated by spaces, commas or new lines: `org/repo#n` or `repo#n` (see [repo-names.md](repo-names.md)). Empty plans the day's train from the config. |
| `all` | `false` | Plan every approved open PR in the configured orgs. Not combinable with `refs`. |
| `profile` | empty | Profile, as `--profile`. |
| `config` | empty | Path to `pipetrain.toml` in the workspace. Run `actions/checkout` first. |
| `emit` | `queue` | `json`, `queue` or `text`. |
| `fail-on-questions` | `false` | Fail the step on exit 3 (queue with open questions). |
| `github-token` | `github.token` | Token pipetrain reads GitHub with. It reaches the environment of the plan step only. |

Every input is validated before it reaches the command line, and none is interpolated into a shell script.

## Outputs

| Output | Meaning |
|---|---|
| `queue` | The queue ([queue-output.md](queue-output.md)), when `emit` is `queue`. |
| `json` | The plan ([plan-json.md](plan-json.md)), when `emit` is `json`. |
| `text` | The text plan, when `emit` is `text`. |
| `exit-code` | Exit code of `pipetrain plan`: 0 a plan, 3 a queue with open questions, anything else an error. |

Only the output named by `emit` is filled. The plan runs once, to keep the GitHub API budget.

## Exit codes

- 0: success.
- 3: the plan is complete but has open questions (`emit: queue` only). The step succeeds unless `fail-on-questions` is `true`. The questions are in the log and the summary, and `exit-code` is `3` either way.
- Anything else fails the step, after the outputs and the summary are written.

## Token

The default `github.token` only reads the repo the workflow runs in, so it can plan PRs of that repo. For other repos use a read-only fine-grained token from a secret (Pull requests: read, Contents: read, Metadata: read) and pass it as `github-token`. Never write a token in the workflow file. See [Authentication](../README.md#authentication).

## Using the outputs safely

PR titles, branch names and the queue come from other people. Pass an output to a later step through `env`, as in the example, never with `${{ steps.plan.outputs.queue }}` inside a `run:` script. The action itself disables workflow commands while it prints the plan, so a PR title such as `::add-mask::` does nothing in its log.
