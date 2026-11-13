# pipetrain merge

`pipetrain merge` follows the dependencies computed by `pipetrain plan`, runs independent repositories up to the configured `max_parallel`, and waits for each PR deployment before releasing its consumers. Setting `max_parallel = 1` preserves sequential execution. It is the second command that writes to GitHub, after `bundle --apply`.

Without `--apply` nothing is written. The dry run prints the steps it would take, the PRs the plan left out and a workflow prediction for each PR. It reads GitHub to build the plan and evaluate the workflow filters; it does not poll runs or write merge state.

```
pipetrain merge [OPTIONS] [PRS]...
pipetrain merge --apply [--yes] [--gates-confirmed] [--allow-questions] [OPTIONS] [PRS]...
```

The PR selection flags (`PRS`, `--all`, `--mine`, `--author`, `--org`, `--date`, `--config`, `--profile`) are the ones of `plan` and `bundle`. The plan is built with `--check-feeds`, so a `# wait` line already says whether the package is published.

## What it merges

The steps come from the same lines that `plan --emit queue` prints (see [queue-output.md](queue-output.md)). The report keeps that order; independent steps can finish in another order:

| Queue line | Step |
|---|---|
| `merge:<id>` | Merge that PR, then wait for its deploy. |
| `# pause: ...` (DDL or infra gate) | Stop and ask you to confirm that the DDL or infra change is done. |
| `# wait: package ...` | Ask you to confirm that the package is published, unless the line ends with `(published)`. |

Every other `#` line is a comment and is ignored. A PR the plan did not mark as ready is not in the queue, so `merge` cannot merge it. The report lists those PRs under `not_merging` with the reasons the plan gave.

## What it checks before each merge

The destination is the PR's existing base branch, not a branch derived from today's date. A target matching `train_prefix` is an intermediate train branch: the dry run, confirmation and final summary warn that promotion to the release target still requires a separate reviewed PR. A completed merge or a workflow that does not run on this branch does not prove a production deployment. `--open-pr` only attempts to prepare the promotion PR and never merges it; an unresolved destination or other PRs still targeting the train can require manual follow-up.

Right before each merge it reads the PR again, because the earlier merges can change it. It does not merge, and the step is `failed`, if any of these is true:

- The PR is no longer open, or the plan's rules for a ready PR no longer hold (not approved, checks not green, draft, conflict). With `require_approval = false` a PR with no review decision is not refused for that, `changes requested` still is, and the check is the same function the plan uses.
- The head commit is not the one the plan saw, unless the verified stacked-child exception below applies. The merge request carries the verified SHA, so GitHub itself refuses a PR that moved in between.
- The base branch is not the one the plan saw.
- GitHub's `mergeable_state` is anything but `clean` (`behind`, `dirty`, `blocked`, `unstable`, `draft`). pipetrain never updates a branch for you.

When GitHub is still recalculating mergeability, both the GraphQL readiness query and the REST pre-merge check retry up to 12 times with five-second intervals, showing progress. Other blockers such as a lost approval are refused immediately. If either check remains unknown after its one-minute retry window, the step is `stopped` (exit 5), no merge is attempted and the same command can be run again. This handles the transient recalculation after an earlier PR updates the same base without weakening review, checks, head SHA or base protections. This retry applies to execution-time revalidation; a PR already excluded from the initial plan remains excluded.

### Stacked PRs

Children land into the parent branch first; the parent is revalidated after. Plan notes, dry runs and confirmations show the entire selected chain, such as `acme/api#12 → feature/y (#11) → feature/x (#10) → main`. The chain and strategy use existing plan `notes` and merge step `detail` strings, without adding JSON fields or changing their meaning. A feature base without a selected parent keeps the shared preflight's `base not landed` warning.

After successfully recording a child's merge SHA, this invocation retains its actual merged head and destination as evidence, shared by sequential and parallel execution. Before accepting a changed parent head, the executor requires all of the following:

- The child merged in this invocation into the parent's head branch in the same repository, and the compare API proves that the new parent head contains the parent's planned SHA.
- The new head is exactly the latest recorded child merge SHA for that parent branch, or it is one commit ahead of that SHA and its commit object has exactly two parents: the parent's planned SHA and the child's verified head SHA. A rollback to an earlier child result, a later descendant, different parents or missing ancestry evidence is refused.
- The fresh GraphQL readiness snapshot and the REST pre-merge response name the same head SHA. Reviews and checks pass the same readiness function used by ordinary execution, the base is unchanged and REST mergeability is clean.

The merge request uses this verified new head SHA. Workflow file prediction also checks the PR's changed files against that head. Any other head change remains refused. There is no retarget, separate branch update, new GitHub write or durable authorization to accept arbitrary changes after restarting. A new invocation uses its new plan's SHA; inherited deployment records retain their existing resume behavior.

A parent that lost approval or has pending checks is `stopped` before any parent merge request, naming the cause and readiness next step, including `re-approve acme/api#10 after its stacked child landed`. Children already merged remain merged; rerun after the required review or checks are complete. Ordinary readiness failures keep their existing status.

## What it waits for

Before waiting, pipetrain evaluates the workflow files (`.github/workflows/*.yml` and `*.yaml`) for every PR base, including `main`, feature branches and train branches. Execution reads them at the merge commit SHA. A dry run reads them at the planned PR head SHA, so its prediction is provisional until the merge. Within an executor, readings are cached by repository, exact base branch, workflow SHA and the sorted set of changed filenames.

The changed files come from `GET /repos/{repo}/pulls/{number}/files`, rather than the plan's file snapshot. Both `filename` and `previous_filename` count for a rename. pipetrain checks the PR head and its REST `changed_files` total, follows file pages through the final short page and requires the count to equal that total. A missing total, missing filename or rename source, duplicate file, mismatched count, unreadable response or more than 300 changed files makes the prediction indeterminate. The 300 limit counts changed files, before adding previous names.

Each push has one prediction:

| Prediction | Evidence and behavior |
|---|---|
| `expected` | Lists every workflow file whose push filters match, plus separately resolved `workflow_run` chains. Only these expected workflow paths can confirm deployment. |
| `dispensed` | Every readable push workflow excludes this branch or these changed files, or the repo has no push workflow. End the step as `done` without polling runs. The detail names each excluding workflow, filter, patterns and changed filenames. |
| `indeterminate` | The workflow YAML, glob or push inputs cannot be read reliably, or the workflow tree is truncated or malformed. Continue polling with the existing grace and timeout. Uncertainty never dispenses the wait, even if another workflow can be evaluated. |

`merge.no_deploy_repos` retains precedence: matching repositories are dispensed by configuration without fetching workflows or changed files. This setting still applies to the whole repo, rather than to one PR or path filter.

Branch and path conditions must both match for a workflow to be expected:

| `on` | Runs on this branch push? |
|---|---|
| `push` with no filter, `[push, ...]` or `push:` | yes |
| `push.branches` | when the branch matches the ordered patterns |
| `push.branches-ignore` | unless the branch matches the ordered ignore patterns |
| `push.paths` | when at least one current or previous changed filename matches the ordered patterns |
| `push.paths-ignore` | when at least one current or previous changed filename is not ignored |
| `push.tags` or `push.tags-ignore` without a branch filter | no |
| no `push` event | no |

Filters accept a string or a list of strings. `*` matches within a path segment, `**` crosses `/`, and `**/` also matches zero directories. A leading `!` removes a previous match; a later positive pattern can include it again. Matching is case sensitive and covers the entire branch or filename. Unsupported patterns (including `?`, `+`, character classes, braces, backslashes, embedded `!` or three consecutive stars), malformed lists and conflicting include/ignore filters are indeterminate. Filter patterns are validated before deciding a workflow is excluded.

For a saved `expected` prediction, the merge commit SHA is polled with `GET /repos/{repo}/actions/runs?head_sha={sha}&per_page=100`, without an event filter. Runs are matched by their full repository-relative `path`, never by the display name. Every expected workflow must have a run, all its runs must be `completed` with `success` or `skipped`, and two consecutive complete polls must see the same number of expected runs. All-skipped expected runs finish as `skipped`; a mixture of success and skipped finishes as `done`. An expected run ending in any other conclusion, including `neutral`, fails the step even while other expected runs are going. Auxiliary runs cannot confirm, fail or delay this verdict; their observed failures remain in the deploy detail with name and URL.

The prediction preserves the original push paths in `workflows` and adds `chained_workflows` for workflows whose `on.workflow_run.workflows` cites the explicit top-level `name:` of an expected workflow. Resolution includes transitive chains and uses literal, case-sensitive names. A chain of a known workflow excluded by the push filters is not expected. Missing, unreadable or ambiguous chain names are recorded in `untracked_chains`: the detail says `chained workflow X cannot be tracked`, and green runs cannot confirm that step. It remains pending until timeout, including on resume. Unresolved references are treated conservatively because their relationship to the expected push cannot be proved. Chained branch/type filters do not dispense the wait.

For `indeterminate` predictions and legacy pending records without a saved prediction, the previous verdict is retained: query `head_sha={sha}&event=push&per_page=100`, require at least one run, accept `success`, `skipped` or `neutral`, fail on any other completed conclusion, and require the same run count across two successful polls. Legacy train records retain their branch-only dispensing check; this temporary prediction does not opt them into expected-path tracking.

In both modes, a response with `total_count` different from the number of runs read cannot confirm deployment in that poll and resets the consecutive-poll confirmation. Without `total_count`, a full 100-run page is treated as incomplete. An unreadable run list is also incomplete. This slice deliberately keeps one page per poll rather than following pages: a SHA with more than 100 runs remains pending and times out. A visible expected failure still fails immediately in an incomplete response.

A run that ended `cancelled` is ignored when a newer run (a higher run id) of the same workflow exists for the same commit. Expected-path tracking additionally requires that replacement to have the same path. A cancelled run without a replacement, or a replacement that was cancelled too, fails the step.

The two polls stabilize the observed run count. Chain protection comes from requiring every resolved chain path to finish, including chains whose runs have not appeared yet; two settled readings of auxiliary runs alone provide no evidence of deployment.

If no run appears within `merge.no_run_grace_seconds`, an expected or indeterminate step is `stopped`. When auxiliary or parent runs are present but an expected path is missing, the wait continues until the deploy timeout. An untrackable chain always uses the deploy timeout, even without runs. The message preserves the prediction and its reason. A path filter that proves no workflow should run is reported as dispensed immediately. Absence of runs alone never recommends adding the entire repo to `merge.no_deploy_repos`; only evidence that the repo has no push workflows could justify that configuration.

## Opening the deploy PR

```
pipetrain merge --apply --open-pr
```

When the PRs of a bundle were retargeted to `deploy/<date>-<base>` (see `bundle --apply`), the plan merges them into that branch one by one. With `--open-pr`, after the last PR of each such bundle is merged and its wait is over, one more step opens the PR `deploy/<date>-<base>` into the base, with the branch name as title and the merged PRs in the body. It never merges that PR. The step is listed as planned in a dry run.

- It only runs when every step before it ended `done` or `skipped`. A blocking failure or stopped step halts the run. After a leaf deploy failure, only bundles with proven independent members may open; affected bundles remain blocked.
- It does not duplicate: when an open PR from that branch into the base exists, or GitHub answers that one already exists, the step is `skipped`. When the deploy PR is itself in the queue, it is not opened either.
- The base is not in the PR the bundle left behind, so it is found by name: the one branch of the repo whose name, written the way `bundle` writes a suffix, equals the suffix of the deploy branch. None or several matches, or an old unsuffixed `deploy/<date>` with no PR, fail the step with `open the PR by hand`.
- It survives an interrupted run. With `--open-pr`, every PR merged into a train branch is recorded in the state file under its repo and branch. The record is removed once the step is settled: the PR was opened, one already existed, the deploy PR is in the queue, or the branch no longer exists. A run that ended after the last merge and before the step leaves the record, and the next `merge --apply --open-pr` plans the step again at the end of the queue, even when nothing is left to merge. A record that is left because the step failed stays too, so the next run tries again.
- It is conservative about when to open. It never opens when the branch does not exist, when an open PR from the branch into the base already exists, or when an open PR still targets the train branch (a PR of the bundle that was not ready or was not merged). In that last case the step is `skipped` with the number of that PR, the record stays, and a later run opens the PR once nothing targets the branch. The base is found with the date of the run, so a branch of another day needs `--date` or the PR opened by hand.
- Without `--open-pr` nothing is recorded and a record from an earlier run is ignored.
- It needs Pull requests: write.

`bundle --apply --open-pr` opens the same PR right after the retargets, before anything is merged, so every merge into the branch then runs the PR's checks. `merge --open-pr` is the one to use for the whole flow.

## Confirmation and gates

Dry runs and apply confirmations show the configured limit before the steps, for example `up to 4 PRs at a time, one per repository (including different bases)`. This is an upper bound: dependencies, gates and the existing repository exclusion can reduce concurrency. A repository cannot run two workers even when the PR bases differ.

On an interactive terminal, execution shows one header with queue counters and total elapsed time, followed by one row for every active PR. Each row includes organization/repository/PR, destination branch, current phase and that PR's elapsed time. Sequential execution uses the same format with one active row. For example on a sufficiently wide terminal:

```text
⠋ waiting: 0, running: 2, merged: 1, deploy confirmed: 0, failed/stopped: 0 · 1m 12s
acme/api#123 → main | waiting for deploy: .github/workflows/deploy.yml; deploy acme/api: 1 of 1 expected workflow runs still going · 1m 10s
acme/ui#42 → main | readiness: checking merge readiness of acme/ui#42 · 2.0 s
```

`waiting` excludes active and finished operations. `running` counts active operations, including their deploy waits. Merge, resume, gate, package wait and deploy PR steps all contribute to waiting and running counts. Gate and other non-PR operations use their own target label. `merged` counts PR merge requests accepted during this invocation, even when deploy is still running or fails. Earlier merges resumed from state do not increase it. `deploy confirmed` counts completed original deploy confirmations, excluding dispensed waits and resolutions by later deploys. `failed/stopped` counts terminal failures and stops. Blocked or skipped steps leave the waiting count without becoming confirmed deploys.

Phases include readiness, merging, waiting for deploy (with expected workflow paths, or a legacy polling label), resolving a later deploy and gate confirmation. Every active row is retained, including in narrow terminals; each line is truncated to the current terminal width. Total and per-PR clocks advance between polls, and a phase update retains the PR's start time. The total clock continues across gate prompts. The spinner pauses before gate prompts and clears before final results. `--no-progress`, JSON output, CI and redirected stderr disable the animation. These status updates do not change merge order, checks, polling intervals or resumability. Cancellation does not undo merges already accepted by GitHub; durable pending records remain available for recovery.

- `--apply` shows the steps and asks you to type `yes`. `--yes` skips only this question.
- Every gate asks again, even with `--yes`. A package wait with a known version first checks the feed again when execution reaches that step, after its publishers have finished. It uses the same lookup and bounded polling loop as `feed --wait`, independently of any earlier `plan --check-feeds` snapshot. Publication completes the wait as `skipped`, with the package, version and feed URL in `detail`, and releases the consumer without a question.
- If the version is still missing at the package timeout, the package wait asks as before. Without a terminal it does not ask: the step is `stopped` and the run ends. A package dependency without a known version keeps this manual confirmation path without a feed request.
- A feed authentication refusal (401 or 403) stops immediately with the cause and `read:packages` hint, without asking, including with `--gates-confirmed`. Transient lookup failures are retried; if the final attempt at the timeout fails, the wait is `stopped` with that cause rather than treating an unreadable feed as an absent version.
- `--gates-confirmed` answers all of them for you. Use it on the run that follows the gate you did by hand, or when you already did the DDL and infra changes.

`--gates-confirmed` retains its meaning for manual DDL/infra gates and package confirmation after an absence timeout. It does not suppress the new feed check or recover an authentication failure. Dry runs do not poll package feeds. Sequential and parallel execution use the same package wait check. Package checks only read feeds and never publish a package.

## Open questions in the plan

When the plan has open questions (`? ...` lines, the ones that make `plan --emit queue` exit with 3) `--apply` merges nothing and exits with 3. `--allow-questions` merges anyway. The order of the plan is a guess for what the questions are about, so read them first.

## Timeouts

| Setting | Default | What |
|---|---|---|
| `merge.timeout_minutes` | `30` | How long to wait for the deploy of one merge. |
| `merge.repo_timeout_minutes` | none | Table of `"org/repo" = minutes`, overrides the default for that repo (case-insensitive). |
| `merge.poll_seconds` | `15` | Time between two polls. |
| `merge.package_poll_seconds` | `10` | Seconds between package feed checks; values below 1 use 1 second. |
| `merge.package_wait_seconds` | `600` | Package wait timeout in seconds, independently of deploy timeouts. `0` checks once without sleeping. |
| `merge.no_run_grace_seconds` | `120` | How long to wait for the first workflow run to appear. |
| `merge.no_deploy_repos` | `[]` | Globs of repos that have no deploy workflow on push. |
| `merge.method` | `"merge"` | `merge`, `squash` or `rebase`. |

`--timeout-minutes N` overrides the deploy timeout of every repo for one run and `--method` overrides `merge.method`. A deploy timeout stops the run; it never merges the next PR. Package wait timeouts use the manual confirmation fallback described above. Feed HTTP requests retain the request timeout used by `feed --wait`; an in-flight request can finish after the package wait deadline.

```toml
[merge]
method = "merge"
timeout_minutes = 30
no_deploy_repos = ["acme/docs"]

[merge.repo_timeout_minutes]
"acme/api" = 60
```

## Resuming

Text output streams each completed step once. Its final summary counts PRs merged during this invocation separately from earlier merges checked, then lists deploys confirmed, dispensed, indeterminate, resolved by later deploy, skipped and failed. A dispensed wait is not a confirmed deploy. An indeterminate prediction stays indeterminate in these new counters even if legacy polling returns a successful result; existing step statuses and exit codes retain their meaning. `done` counts completed steps, including their deploy waits, rather than successful merge requests alone. A PR whose merge succeeded but whose deployment failed stays merged and remains pending in the state file. The JSON output continues to contain every step, its status, merge SHA and deploy result.

Before it merges, pipetrain records the PR in a state file. After the deploy passes, or all detected workflow runs are skipped, it removes the record. A run that is stopped, fails, is interrupted or loses its connection leaves the record, and the next `merge --apply` starts by finishing that wait, before it touches the queue:

- A record without a merge commit (the process ended between the request and the answer) is resolved by asking GitHub whether the PR was merged.
- A record whose deploy failed is checked again at its saved SHA. If it still fails, resume reads the base tip and requires ancestry proof plus a later successful deployment before resolving it as `resolved by a later deploy of <short SHA>`, with run URLs. New records save `deploy_prediction`, including expected workflow paths and chains; resume reuses that decision. Without a saved expected list, all push runs on the later SHA must pass under legacy rules. Ordinary legacy resume keeps its existing behavior, including branch-only workflow checks on train bases.
- The plan itself is not saved. It is built again, so merged PRs are gone from it and the queue continues from the first one still open.

State is local and durable by default. Named profiles use `pipetrain/queues/<profile-key>/merge-state.json` in the user state directory (the local data directory on Windows). The selected canonical profile name identifies one stable queue across repositories and invocations; aliases and case changes do not create another queue. Simple names use their lowercase form. Other UTF-8 bytes are percent-encoded, including dots, path separators and percent signs; Windows reserved names escape their first byte. Different names cannot collide through replacement of punctuation. The implicit `default` profile retains `pipetrain/merge-state.json`.

Inspect it with `pipetrain state show [--json]`, or intentionally stop tracking one pending record with `pipetrain state discard <id> --reason <text>`. Both use the same path and profile resolution as merge. Discard requires terminal confirmation or `--yes`, shares the merge lock, and atomically moves the record into the additive `audit` list. It never confirms a deploy. See [state.md](state.md) for the inspection JSON and audit contracts.

Later-deploy resolution also moves the record into audit, retaining the old SHA and saving the later SHA and observed run URLs. It requires two passed readings with complete responses and URL evidence. An all-skipped later deploy, unrelated commit or unresolved workflow chain cannot resolve a failed deploy. A later deployment not yet passed stays pending for another invocation. This check does not create another full deploy timeout. Resume messages name the SHA and observed runs, and explain when the base has no later commit and repeating without changing anything will not help. Successful resolution clears the failed prerequisite before following same-repo/base merges are evaluated.

Path precedence is `--state FILE`, then `merge.state_file` from the selected configuration, then the automatic profile path. Relative configured paths resolve from the loaded config file's directory; relative `--state` paths retain their command-line working-directory meaning. The effective path is also shown once on stderr except with `--json` or `--no-progress`. Dry runs and apply confirmations include the State preview described below, including with `--no-progress`. A missing user directory is an error unless a path override is provided; production commands never silently use memory-only storage.

To persist a custom path without repeating a command flag:

```toml
[profile.acme.merge]
state_file = 'queues/acme.json'
```

Without a path override, unfinished work in the legacy global state blocks a named profile before any GitHub writes, even if a profile state already exists. Choose that legacy file explicitly with `--state` or `merge.state_file`, reconcile it, and then deliberately choose how to continue. Pending deploys, queued deploy PRs, active jobs and dependency bump journals are checked. State is never automatically migrated, deleted or combined. An empty legacy state is left in place. An unreadable or invalid state file is an error, never a reset.

### State identity, locking and age

The local state file uses schema `2`, independently of the merge report's schema `1`. Reading accepts an absent schema (schema `0`), `0`, `1` and `2`, normalizing to `2` in memory. Reading alone does not rewrite a file. The next normal save writes schema `2` in the same location and preserves pending work. A schema above `2` fails with `written by a newer pipetrain` before writing the state or acquiring its lock. A future schema detected on the locked re-read also fails without rewriting the state.

State adds nullable `created_at`, `updated_at`, `profile` and `queue_id`. The first save establishes a UTC RFC 3339 creation timestamp and a locally generated opaque queue ID, and every save updates the UTC RFC 3339 update timestamp. The queue ID identifies the state file's persistent queue across invocations, including after all work completes; it is not a hash of the current selection or an execution ID. Existing identity and creation time are retained. A confirmed apply records the selected canonical profile if the state has no recorded profile. An explicit state override retains an existing profile so its origin remains visible. For legacy files, absent identity and timestamps remain unknown until the next normal save adopts them; the adoption timestamp does not reconstruct the original creation time.

Each newly recorded pending merge adds `recorded_at` in UTC RFC 3339 before its merge request. Later SHA updates and resume preserve that timestamp. Missing legacy timestamps remain unknown, including after saving, rather than inventing an age.

An apply acquires an exclusive OS lock with `std::fs::File::try_lock` on `<state>.lock` before confirmation or any state writes, re-reads the state under that lock, and holds the handle until the store is dropped at the end of the invocation. Another apply fails immediately, names the lock file and reports that another execution is active. The file can remain after release; ownership is the OS lock on its handle, not file existence, so interruption does not require deleting it. Dry runs never acquire a lock or create state directories or lock files. Read-only previews can inspect a locked state.

Every save exclusively creates a unique `<state>.<pid>.<n>.tmp` sibling and atomically renames it over the state file. Existing temporary files are not overwritten. Failed writes leave the prior state in place and clean up only the temporary file created by that write. No legacy temporary file is reused.

The dry run and apply confirmation show a `State` block with the path, file age since its last filesystem modification, recorded profile, selected profile, queue ID and every inherited pending record. Each record shows its ID, base, seven-character SHA (or `SHA unknown`) and age since `recorded_at`. Missing or invalid timestamps display `age unknown`; unknown ages are never marked stale. Future timestamps display age zero. Records strictly older than `merge.stale_pending_hours` are marked `stale (warning only)`. The threshold is an unsigned number of hours, defaults to `24`, and accepts `0` to warn on any known positive age:

```toml
[merge]
stale_pending_hours = 24
```

Neither stale markers nor a different current selection or profile filter, discard or change the order of inherited pending work. Apply still resumes all pending records first. The preview is a snapshot taken before confirmation and execution, including in the final JSON report.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Dry run, or everything done. |
| `1` | A step failed, or you did not confirm. |
| `3` | The plan has open questions and `--allow-questions` was not given. |
| `4` | Only with `--strict`: no visible selected PR is eligible, or a dry run contains no merge step. Existing errors, open-question apply refusal and resumable failures retain their codes. |
| `5` | Stopped and resumable: a deploy timed out or has no run, a gate was not confirmed, or a state could not be read from GitHub. Run the same command again. |

## JSON

`--json` prints one object. `schema` is `1`; new fields may be added, existing ones keep their meaning.

| Field | Type | Meaning |
|---|---|---|
| `schema` | integer | `1`. |
| `applied` | boolean | `false` for a dry run, or when nothing was confirmed. |
| `stopped` | string, optional | Why it did not start (open questions). |
| `steps` | Step[] | In execution order. Steps resumed from an earlier run come first. |
| `not_merging` | `{id, reasons[]}[]` | PRs the plan left out and why. |
| `questions` | string[] | The `? ...` lines of the plan. |
| `preflight` | Readiness[] | Additive readiness snapshot for every visible selected PR, including blocked PRs, using the same shape as [plan-json.md](plan-json.md#readiness). |
| `state` | StatePreview | Additive snapshot of local state before execution, also present in dry runs. |
| `max_parallel` | integer | Additive configured concurrency upper bound, also present in dry runs. The existing exclusion is one worker per repository, including different bases. |
| `summary` | MergeSummary | Additive invocation result counts, all zero before execution, including dry runs and declined confirmations. |

`MergeSummary` contains integer fields `merged`, `earlier_merges_checked`, `deploy_confirmed`, `deploy_dispensed`, `deploy_indeterminate`, `deploy_resolved`, `deploy_skipped`, `deploy_failed`, `failed` and `stopped`. `merged` counts merge steps with a merge SHA; `earlier_merges_checked` counts resume steps with a merge SHA. The six deploy counts partition those merged or checked PRs: original deploy confirmed, wait dispensed by configuration or filters, outcome not reliably determined (including a timeout, unreadable discovery or runs, or interrupted post-merge bookkeeping), audited resolution by a later deploy, all observed workflows skipped, and original deploy failed. A later resolution increments only `deploy_resolved`, retaining the original failure evidence. `failed` and `stopped` count steps of every kind with those statuses, including readiness and gate failures without a merge. These counts do not include historical discarded records and do not change existing status semantics.

`deploy_outcome` is an additive optional step field with values `confirmed`, `dispensed`, `indeterminate`, `skipped` or `failed`. It records the original deploy evaluation separately from the step status; `resolution` takes precedence in summary counts. Legacy pending records still follow their existing polling rules: a passed legacy poll without a saved indeterminate prediction counts as confirmed. A successful poll with an explicitly indeterminate prediction is counted as indeterminate. A merged step without a deploy outcome is also counted as indeterminate. Gates and steps that never merged omit it.

`StatePreview` contains `path: string | null`, `file_age_seconds: integer | null`, `file_age: string`, `created_at: string | null`, `updated_at: string | null`, `profile: string | null` (recorded origin), `selected_profile: string | null` (current selection), `queue_id: string | null`, `stale_pending_hours: integer` and `pending: PendingPreview[]`. Unknown metadata is `null`; age strings use `age Hh Mm Ss` or `age unknown`. File age measures the last filesystem modification, independently of the stored timestamps. `path` is null only for library callers using memory-only storage.

Each `PendingPreview` is `{id: string, base: string, sha: string | null, recorded_at: string | null, age_seconds: integer | null, age: string, stale: boolean}`. `sha` is the first seven characters of the saved merge SHA. `pending` includes every inherited record, even outside the current plan. These are additive report fields; the existing report schema, fields and resume behavior retain their meanings.

A step is `{kind, target, status, detail}` plus `merge_sha`, `deploy`, `blocked_by`, `deploy_prediction` and `expected_runs` when they apply. `blocked_by` identifies the failed PR responsible for a blocked step. `kind` is `resume`, `merge`, `gate`, `wait` or `open-pr`. `status` is `planned`, `done`, `skipped`, `failed` or `stopped`; after a global stop the steps that did not run are `skipped` with the detail `stopped after an earlier failure`. Dependency and same-target skips include a specific reason.

`deploy_prediction` is an additive optional object on merge and resume steps. It is present for each evaluated PR in a dry run and for merged PRs after evaluation. Gates and unevaluated steps omit it. The same object is stored on new pending records with `serde(default)` compatibility for older state files. Existing fields and schema `1` keep their meaning.

| Prediction shape | Fields |
|---|---|
| `{status: "expected", workflows: string[], chained_workflows?: string[], untracked_chains?: string[]}` | `workflows` retains the full push workflow paths, for example `.github/workflows/deploy.yml`. `chained_workflows` adds resolved workflow paths triggered through `workflow_run`. `untracked_chains` lists workflow paths whose upstream names cannot be resolved reliably; these prevent confirmation. Both new lists are optional, omitted when empty, and default to empty when loading older predictions. |
| `{status: "dispensed", reason: string, workflows: Exclusion[]}` | Each exclusion is `{workflow: string, filter: string, patterns: string[], files: string[]}`. `filter` identifies the excluding push filter, and `files` contains the complete current/previous filename set. The list is empty when there are no push workflows or `no_deploy_repos` overrides evaluation. |
| `{status: "indeterminate", reason: string}` | Why a reliable prediction was unavailable. |

`expected_runs` is an additive optional array of `{path: string, name: string, html_url: string, status: string, conclusion: string | null}`. It contains the observed expected runs from the latest successful poll, including chained runs and cancelled runs replaced by a newer run. Missing workflows remain in the prediction rather than receiving invented run URLs. Empty arrays are omitted, including for legacy polling. The existing `deploy` string also includes expected run names and URLs and observed auxiliary failures.

`resolution` is an additive optional step field present only when a failed saved deploy is resolved by a later one: `{status: "resolved_by_later_deploy", sha: string, runs: Run[]}`. The full `sha` identifies the later base tip; `runs` uses the same run shape as `expected_runs` and includes the later deploy evidence (all push runs for legacy pending records). The step's ordinary `status` is `done`; `detail` explicitly says `resolved by a later deploy of <short SHA>` and includes run URLs. Existing `merge_sha` retains the original merge SHA, and `expected_runs` retains the original expected run observations. Those original failures are not relabeled successful. The report schema stays 1 and the state schema stays 2 because the new fields are additive. Discard produces no successful merge or deploy step.

Dry-run text shows `expected`, `dispensed` or `indeterminate` per PR together with workflow paths or the reason. The existing step `status` remains `planned` until execution. A dispensed merge is `done` with its merge SHA; it does not claim that a deploy ran.

## Not part of it

- It is not in the MCP server: [docs/mcp.md](mcp.md) lists only read-only tools.
- It never approves, never updates a branch, never re-runs a workflow, never reverts, never deletes a branch and opens a PR only with `--open-pr`.
- It does not know whether two PRs are safe together. The order and the gates are the plan's; the rest is your call.

## Design questions left open

These were decided on the conservative side. Each can be loosened later with a new flag or setting without changing what exists.

1. **No workflow run after a merge.** An expected or indeterminate prediction remains `stopped` after the grace period. Only complete filter evidence or the explicit repo configuration dispenses the wait; dispensing does not claim a deploy happened.
2. **Gates under `--yes`.** `--yes` does not answer gates. Is a separate `--gates-confirmed` the right split, or should a CI run be able to pre-approve a named gate (for example by PR id)?
3. **Open questions.** They block `--apply`. Some questions (a sibling that is not ready, for example) may be acceptable to merge past; is one switch for all of them too coarse?
4. **Branches that are behind.** A `behind` PR fails the step and asks you to update it. Should `merge` offer `--update-branch`, knowing that it re-triggers the PR's checks and invalidates the plan?
6. **A deploy that failed after the merge.** pipetrain keeps the record and never reverts. Continuation follows the dependency failure policy below. Resume can resolve a later successful descendant deployment with audit evidence, and deliberate local discard requires confirmation and a reason.

## Deployment failure policy

A failed deploy remains `failed`, keeps its merge SHA and pending state, and makes the invocation exit with 1. A leaf failure continues only PRs whose independence is proved by the plan. PRs on the same repo and base, direct consumers and transitive dependents are blocked with their failed prerequisite named. Graph edges and ordering constraints remain in place.

`--dependency-failure stop` is the default: a deploy failure with downstream dependency consumers stops the whole remaining queue. `--dependency-failure skip` blocks affected consumers and continues proven independent PRs. Neither policy changes the global stop for merge/readiness failures, unconfirmed gates, timeouts or uncertain deployment outcomes. Associated package waits and late gates are skipped when their consumer is blocked; unrelated gates still require confirmation.

New pending records contain optional `continuation` metadata: `dependents` captures the transitive dependency closure, `has_consumers` records dependency contracts, and `independent` maps independent PR IDs to their planned head SHAs. Resuming uses this snapshot together with the rebuilt graph, because merged PRs can disappear from the plan. A changed or new head without saved proof is blocked. Older state files still load, but an unresolved failed deploy without this metadata stops safely. Use the audited state discard command for intentional removal rather than deleting or editing pending state.

Text results use green for completed steps, red for failures, yellow for blocked skips and stopped gates, and gray for normal skips such as already merged PRs or workflows skipped by filters. An all-skipped deploy is a normal `skipped` result with its merge SHA retained and pending record removed after the usual two settled polls. With a saved expected list, auxiliary conclusions do not affect the result: expected successes remain green and expected failures remain red. Legacy polling continues to judge every observed push workflow. Colors respect `--color`, `NO_COLOR` and redirected output. JSON remains plain and retains the full report.

## Parallel execution

`max_parallel` bounds active merge and deployment workers across repositories. Each worker has a separate GitHub client while all clients share the request budget, pacing window and observed rate limits. The budget lock is released before sleeping or issuing HTTP requests. Only one worker runs in a repository at a time, including PRs with different base branches. PRs targeting the same repository and base also keep their queue dependency, so a failed deployment blocks later merges into that target.

The executor uses `Plan.items[].after` across lanes. A consumer starts only after its internal prerequisites finish successfully or are legitimately skipped. Existing package publication waits and DDL/infra confirmations remain barriers: active work drains before a prompt, and later work cannot pass it. Deploy PR creation also runs as a barrier. Dependency waits associated with failed consumers are blocked without prompting; independent work remains eligible under the selected failure policy.

The coordinator owns confirmations, progress and the state file. Workers send typed pending/SHA/deploy-PR mutations over a bounded channel and wait for the durable acknowledgement before proceeding. No worker writes the state file directly. `active_jobs` records scheduler steps before dispatch; existing `pending` entries and merge SHAs remain the source for resolving interrupted merges and deployment waits. Recovered pending deployments are reconciled before dispatching new merges. A pending entry without a confirmed merge SHA is checked against GitHub before any replay.

A publisher failure stops new dispatches by default. The skip policy blocks its descendants and continues independent work. A leaf deployment failure continues proven independent work. Readiness failures, uncertain outcomes and coordinator persistence failures stop new dispatches. Workers already running are drained and their results recorded; the executor does not cancel deployment workflows.

The scheduler exposes a coordinator preparation callback after prerequisite completion and before dispatch. A blocked preparation result affects only that consumer and its descendants. Preparation errors stop new dispatches before mutation. This callback does not change package versions or create publication PRs by itself.

## Optional reviewed consumer bumps

`--bump` enables publication-backed consumer preparation; `--no-bump` explicitly
keeps the default disabled behavior. They cannot be combined. Without `--apply`,
no bump branch, workspace or PR is created. The dry run and apply confirmation
state these potential writes and identify candidate consumers without inventing
versions. `--bump-push-step` changes the exact publisher step name, whose default
is `Push to GitHub Packages`.

A proved new NuGet publication can prepare one isolated bump branch and draft PR
per consumer head. The draft targets that consumer's head branch and needs CI
and review. The consumer and its descendants stay blocked in yellow; independent
items can continue. A skipped or duplicate publication emits a gray bump skip
and leaves ordinary consumer checks and gates intact. The coordinator persists
publication inputs and preparation markers for safe resume and duplicate PR
protection. See [Publication-backed consumer preparation](publication-bump.md)
for proof requirements, workspace limits and partial-operation recovery.

The JSON report adds `bump_enabled` when enabled, optional `bump_candidates`, and
`kind: bump` events with a structured `bump` outcome. These additive fields retain
schema 1. Events are appended after fixed queue steps and streamed at preparation
time. A draft requiring review or a partial operation contributes a stopped
attention event and exit code 5 without changing the dependency graph.

## Shared preflight

The preview and JSON include each selected PR as `eligible`, `blocked` or `waiting`, with causes, next step, base, repository default branch, full GraphQL merge state, warnings and check names/states/URLs. The additive `preflight` array uses the [plan Readiness contract](plan-json.md#readiness); it is a snapshot before execution and never changes existing step status semantics or report schema `1`. Text previews and confirmations show the same information, including PRs excluded from merge steps. Author-hidden PRs are omitted from the merge preflight.

Plan discovery and executor revalidation share the merge-state readiness rules near `Pr::blockers_with`. BLOCKED, DIRTY, UNSTABLE, DRAFT and BEHIND block with a cause and corrective step. UNKNOWN warns `GitHub still computing`, without blocking a plan; executor GraphQL and REST revalidation still retry for up to a minute and stop resumably if GitHub does not settle. CLEAN and HAS_HOOKS pass preflight, but the final REST mergeability guard still requires exactly `clean`. Head SHA, base, review and check guards remain in force.

CheckRun `detailsUrl` and StatusContext `targetUrl` are reported with each check's name and state, including failures. A missing URL is null. Check names supply evidence only; no private check policy is inferred. `base not landed` is a warning when the known default branch differs from the PR base, the base is not a train branch, and no other selected PR in that repository has the base as its head. It neither retargets the PR nor changes stack ordering.

`--strict` preserves the normal report and returns previously unused code `4` after an otherwise successful invocation when no visible selected PR is eligible, including an empty or fully blocked selection. A dry run also returns `4` if its arranged steps contain no `merge` step, even if inherited resume/open-PR steps exist. Waiting means an earlier step, unresolved question or manual gate must be addressed before the PR is currently eligible. Existing nonzero errors and apply refusals keep precedence. Without `--strict`, a dry run or an empty/fully blocked selection retains its previous exit code. Strict mode does not authorize writes or weaken any executor guard.
