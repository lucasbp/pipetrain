# State inspection and recovery

`pipetrain state show [--json] [--state FILE] [--profile P] [--config FILE]`
inspects local merge state without credentials, GitHub requests, locking or
writing the file. Text includes the path, schema, profiles, dates, pending ages,
deploy PRs, active job IDs and audit history. A missing file is shown as empty
state and is not created. Saved job IDs do not prove jobs are still running.

Path selection uses the merge resolver: explicit `--state`, configured
`merge.state_file`, then the automatic profile file. Relative configured paths
resolve from the config file; explicit relative paths resolve from the working
directory. Profile selection and legacy unfinished-state protection match merge.

`pipetrain state discard <id> --reason <text> [--yes]` accepts the same state,
profile and config options. The reason must contain non-whitespace text. It
requires a terminal and the exact answer `yes` after trimming, or explicit
`--yes`. Without either it refuses before opening a lock or changing the file.
The prompt identifies the file, record, repo, base, SHA and reason.

Discard holds the same exclusive lock as `merge --apply`, reloads under it, and
atomically saves both removal from `pending` and a new `audit` entry. Unknown or
already discarded IDs fail without rewriting state. Existing audit and unrelated
state, including deploy PRs, bump journals and scheduler job IDs, are preserved.
Discard stops tracking a wait; it never confirms a deploy, and subsequent merge
invocations do not resume that record.

State commands disable update notices and usage logging. They neither re-run
workflows nor write to GitHub. The MCP tool set is unchanged and does not expose
`discard`.

## `state show --json` contract

Stdout contains one object without a wrapper. `schema` is the loaded state schema,
currently `2`, not a separate report schema. Legacy schemas 0 and 1 normalize to
2 in memory without rewriting the inspected file. Newer schemas are refused.
Optional fields can be added without changing existing meanings.

| Field | Type | Meaning |
|---|---|---|
| `path` | string | Selected file path. Explicit relative paths may remain relative. |
| `schema` | integer | Supported loaded state schema, currently 2. |
| `profile` | string or null | Recorded profile. |
| `selected_profile` | string | Profile selected for this invocation. |
| `queue_id` | string or null | Durable queue identity. |
| `created_at`, `updated_at` | string or null | Stored UTC RFC3339 timestamps; unknown in legacy or absent state. |
| `file_age_seconds` | integer or null | Nonnegative seconds since filesystem modification; null for unavailable metadata. |
| `file_age` | string | `age Hh Mm Ss`, or `age unknown`. |
| `stale_pending_hours` | integer | Configured warning threshold, default 24. |
| `pending` | PendingInspection[] | All pending records, including inherited ones outside the current selection. |
| `deploy_prs` | DeployPr[] | Saved deploy PR preparation records. |
| `active_jobs` | string[] | Saved scheduler IDs, always present, including when empty. |
| `archived` | ArchivedState[] | Files this command archived beside the state file (see [archiving](merge.md#archiving-a-stale-state-of-another-queue)), oldest name first; always present, empty when none. Each has `path`, `archived_at` (UTC RFC3339 read from the file name) and `pending` (number of pending records in it). Additive, the state schema stays `2`. |
| `audit` | AuditEntry[] | Discard and later-deploy resolution history, always present, including when empty. |
| `admin_overrides` | AdminOverrideRecord[], optional | Durable successful admin override history; omitted when empty. |
| `bump` | object, optional | Publication/consumer journal. Optional `queued_bumps` records the selected consumer, base, head, SHA, exact pin, package owners and publisher PRs before producer merge. For a draft into a consumer branch, optional preparation fields store the base, intended pins, exact prepared commit and verified consumer head. |
| `bump_behind` | array, optional | Per-consumer base-branch bumps. For queue publications, optional `publication` records the base, consumer head branch, prepared commit, consumer SHA and the base tip for which an update was requested. A definitive update rejection is recorded for that head. The number is zero and the URL empty until a draft is identified. |

Each `PendingInspection` has `id`, `repo`, `base` (strings), `sha` (full saved SHA
or null), `recorded_at` (RFC3339 string or null), `age_seconds` (nonnegative integer
or null), `age` (same format as file age), and `stale` (true only when known age
exceeds the threshold). Invalid or missing recording timestamps have unknown
age and are not stale. Optional `continuation` and `deploy_prediction` keep the
state shapes documented in [merge.md](merge.md), without recomputation.

Pending records also accept optional `admin_override`:
`{reason: "REVIEW_REQUIRED", requested_by: string, at?: string}`. It is saved
before the merge request to retain the override intent if the reply is lost.
The UTC RFC3339 `at` is added in the same save that records the merge SHA, or
when resume first confirms that SHA. Resume copies this record into the step
report, even without `--admin`, preserving a known timestamp.

Each `AdminOverrideRecord` has string fields `id`, `repo`, `base`, `sha`,
`reason`, `requested_by` and `at`. `sha` is the full merge SHA, `reason` is
`REVIEW_REQUIRED`, `requested_by` is the token's viewer login, and `at` is the
UTC RFC3339 recording time. The record is appended atomically with the pending
SHA and timestamp, once per PR/repository/base/SHA. It survives successful waits,
discard, later-deploy resolution and new queues in the same file, just like
`audit`. Neither history has a retention limit.

These fields are additive: state schema remains `2` and report schema remains
`1`. Missing `admin_override` defaults to none and missing `admin_overrides`
defaults to an empty list; neither key is serialized when empty. An older
pipetrain can read the file but rewrites it without these unknown fields, losing
the override history and pending marker. No new `AuditAction` variant is used.

Text inspection lists every history record as
`admin override: <id> merged past REVIEW_REQUIRED by <login> at <time> (<sha short>)`,
using seven characters for the SHA.

Each `DeployPr` is `{slug: string, branch: string, ids: string[]}`. These records
describe preparation intent, not confirmed deployment.

Each `AuditEntry` has:

| Field | Type | Meaning |
|---|---|---|
| `id`, `repo`, `base` | string | Original record identity and target. |
| `sha` | string or null | Original saved SHA, never replaced by the later SHA. |
| `action` | string | `discarded` or `resolved_by_later_deploy`. |
| `reason` | string | Trimmed operator reason, or resolution explanation with run URLs. |
| `recorded_at` | string | UTC RFC3339 audit timestamp. |
| `resolution` | Resolution, optional | Present only for later-deploy resolution; absent for discard. |

`Resolution` is `{status: "resolved_by_later_deploy", sha: string, runs: Run[]}`.
Its SHA is the full later base tip. Each run is `{path: string, name: string,
html_url: string, status: string, conclusion: string | null}`, supplying evidence
for that later SHA. Original failed runs are not relabeled successful.

## Later deploy resolution

Resume first evaluates the saved SHA as usual. If that deploy failed, it reads
the current base tip and compares it against the saved SHA. Only a different tip
containing that SHA can resolve the wait. Diverged, behind or unreadable ancestry
retains the record. Saved expected lists require all expected push and chained
workflows; unresolved chains prevent confirmation. Without a saved list, all
push runs on the later SHA are judged by legacy rules.

Resolution requires complete responses, nonempty run URL evidence and two passed
readings with the same run count, separated by the configured poll interval.
All-skipped runs do not resolve a failed deploy. A later deployment still waiting,
missing, failed or incomplete is reported and left for a future resume; recovery
does not start another full timeout loop. Successful resolution atomically moves
the record to audit and unblocks following merges on the same repo/base.

Exit 0 means successful inspection or discard. Exit 1 means refusal, declined
confirmation, lock contention, unknown record, unsupported state or another
operational error. Invalid command-line syntax exits 2.
