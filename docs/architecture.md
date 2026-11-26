# Architecture

For user workflows, start with [flows.md](flows.md). This page maps the source and records design choices. Output shapes belong to their contract documents rather than this map.

## Source map

Paths below are relative to the repository root. Each line identifies a source module or entry point.

### Entry points and command orchestration

- `src/main.rs`: clap arguments, command dispatch, exit handling, log and update-notice integration.
- `src/lib.rs`: library module exports.
- `src/command.rs`: plan collection, GitHub reads, author filtering, optional feed annotations and report delivery.
- `src/command/resolve.rs`: shared config, profile, org and PR argument resolution.
- `src/command/names.rs`: explicit PR reference parsing and bare repository-name resolution.
- `src/command/authors.rs`: configured and command-line author filters, including the authenticated login.
- `src/command/apply.rs`: bundle execution, initial confirmation gate, GitHub writes and action reports.
- `src/command/checklist.rs`: live bundle action checklist.
- `src/command/feed.rs`: feed command options, plan annotations, wait orchestration and CLI output.
- `src/command/bump.rs`: read-only default-branch manifest scan and proposed version-change reports.
- `src/command/order.rs`: manifest-backed repository order suggestions and TOML output.
- `src/command/order/graph.rs`: repository dependency analysis for order suggestions.
- `src/command/init.rs`: proposed configuration generation and optional local file writing.
- `src/command/profiles.rs`: profile listing and selection reporting.
- `src/command/orgs.rs`: visible GitHub organization listing and profile associations.
- `src/command/state.rs`: local state inspection and confirmed, audited discard.
- `src/command/log.rs`: local usage-log summary command.
- `src/command/update.rs`: release checks, download verification and executable update orchestration.

### Selection, configuration and planning

- `src/config.rs`: configuration defaults, validation, readiness policy and compiled matching rules.
- `src/config/order.rs`: compilation and ranking of explicit repository order patterns.
- `src/profile.rs`: profile sets, config loading and profile metadata.
- `src/profile/parse.rs`: profile configuration parsing and inheritance.
- `src/profile/select.rs`: profile choice from flags, arguments, folders, default or sole profile.
- `src/profile/alias.rs`: organization aliases and canonical name resolution.
- `src/model.rs`: PRs, references, questions, checks and shared readiness/blocker representation.
- `src/discovery.rs`: dated train, shared-branch, into-train, all-ready and explicit PR selection.
- `src/contained.rs`: commit-containment checks against base and train branches.
- `src/content.rs`: blob-identity checks for changes already present by content.
- `src/content/slots.rs`: cached and batched blob-identity lookups for content checks.
- `src/plan.rs`: assembled report, preflight statuses, stages, lanes, bundles, conflicts and author visibility.
- `src/planner.rs`: dependency graph rules, stacked chains, questions, stages and lanes.
- `src/planner/dependency.rs`: publisher/consumer ordering from package references.
- `src/planner/order.rs`: ordering constraints from configured repository lists.
- `src/bundle.rs`: deploy-branch naming, train-branch recognition and bundle suggestions.
- `src/conflicts.rs`: same-repository file-overlap hints, excluding stacked pairs.
- `src/emit.rs`: stable queue lines, gate/package comments and queue exit status.

### Package discovery and publication

- `src/detect.rs`: PR-body dependencies, manifest diff detection and bumped package versions.
- `src/manifest.rs`: package IDs, references and versions, dispatching ecosystem-specific parsers.
- `src/cargo.rs`: Cargo manifest publication, references and diff versions.
- `src/pyproject.rs`: Python manifest publication, references and diff versions.
- `src/gomod.rs`: Go module publication, references and diff versions.
- `src/publishers.rs`: manifest candidate selection and publisher/reference maps.
- `src/feed.rs`: NuGet/npm feed data, version comparison, wait loop and report rendering.
- `src/feed/registry.rs` owns registry HTTP requests, authentication and exact-version lookups shared by the feed command and publication verification.
- `src/publication.rs`: exact new-publication verification and opaque publication evidence.
- `src/publication/backend.rs`: GitHub workflow/job/log collection and exact NuGet feed adapter.
- `src/consumer_bump.rs`: evidence-backed pin preparation through the bump gateway.
- `src/consumer_bump/pin.rs` classifies an existing dependency pin for both the bump preview and consumer preparation.
- `src/consumer_bump/journal.rs`: durable publisher inputs, consumer contracts and preparation markers.
- `src/consumer_bump/backend.rs`: isolated Git workspaces, pin edits, restore checks, branch reservation and draft PR operations.
- `src/consumer_bump/backend/process.rs`: owned subprocess trees and platform-specific termination.

### Merge execution and recovery

- `src/merge.rs` parses queue steps, owns state types and store operations, and re-exports the merge API.
- `src/merge/verdict.rs` judges observed runs against legacy or expected workflow rules.
- `src/merge/report.rs` defines the serialized report, step details, summaries and exit status.
- `src/merge/estimate.rs` defines the API estimate, budget warnings and polling schedule used by both the estimate and the executor.
- `src/merge/storage.rs`: state paths, schema loading, previews, exclusive locking and atomic saves.
- `src/workflows.rs`: push-filter evaluation, expected workflow paths, chained workflows and conservative predictions.
- `src/merge_scheduler.rs`: dependency-aware worker scheduling, repository exclusion, barriers and failure policies.
- `src/command/merge.rs`: merge CLI orchestration, live revalidation and merge requests.
- `src/command/merge/deploy.rs` tracks a deployment at the merged SHA. It applies the workflow verdict to the step report and handles polling, retries and runner waits.
- `src/command/merge/compact.rs` and `src/command/merge/verbose.rs` render compact and detailed merge reports.
- `src/command/merge/estimate.rs` calculates projected API requests from the plan, pending deploys and deploy predictions.
- `src/command/merge/parallel.rs`: queue-to-job translation, worker clients and coordinator-owned state/progress.
- `src/command/merge/stacked.rs`: in-invocation child evidence and verification of a changed parent head.
- `src/command/merge/failure.rs`: failed-deploy dependency closure and independence checks for continuation.
- `src/command/merge/recovery.rs`: ancestry and run evidence for later-deploy resolution.
- `src/command/merge/package.rs`: execution-time package feed waits before consumers.
- `src/command/merge/bump.rs`: publication verification, consumer preparation and durable bump recovery.

### GitHub transport and local cache

- `src/github/mod.rs`: GitHub client module and shared API type exports.
- `src/github/client.rs`: synchronous HTTP, read retries, write requests, authenticated downloads and client forks.
- `src/github/endpoints.rs`: PR, ref, comparison, workflow, content and write endpoint operations.
- `src/github/parse.rs`: API response parsing into planner and executor data.
- `src/github/cached.rs`: cache serialization and reconstruction for immutable API results.
- `src/github/guard.rs`: shared request budgets and pacing across client forks.
- `src/github/reply.rs`: HTTP reply and rate-limit parsing.
- `src/github/retry.rs`: bounded retry classification, backoff and jitter.
- `src/github/error.rs`: API error types, diagnostics and secret redaction.
- `src/github/token.rs`: token lookup from `gh auth token` and `GH_TOKEN`, with redacted debug output.
- `src/cache/mod.rs`: cache modes, location and hit/miss accounting.
- `src/cache/key.rs`: validated repository/endpoint/key paths.
- `src/cache/store.rs`: versioned, size-limited entries and temporary-file replacement.
- `src/cache/prune.rs`: age and total-size pruning.

### Diagnostics, MCP and terminal output

- `src/doctor/mod.rs`: setup-check collection and doctor command orchestration.
- `src/doctor/check.rs`: token, API, rate, configuration and profile check classification.
- `src/doctor/orgs.rs`: organization visibility probing and SSO-related diagnostics.
- `src/doctor/report.rs`: doctor text/JSON output and exit status.
- `src/mcp/mod.rs`: line-oriented JSON-RPC request loop, protocol negotiation and result redaction.
- `src/mcp/tools.rs`: five read-only tool definitions and adapters to command implementations.
- `src/render.rs`: human-readable plan rendering.
- `src/progress.rs`: stderr progress painting, active rows, timing and cursor restoration.
- `src/progress/merge.rs`: merge counters and active-operation phase aggregation.
- `src/progress/term.rs`: platform-specific terminal setup and width.
- `src/progress/multiline.rs`: terminal-screen test support and multiline progress tests.
- `src/live.rs`: live action-checklist rendering and row states.
- `src/style.rs`: text color helpers.
- `src/elapsed.rs`: elapsed-time formatting and millisecond conversion.
- `src/hint.rs`: platform-specific authentication and connectivity guidance.

### Usage logging and executable updates

- `src/log/mod.rs`: opt-in logging policy and log directory selection.
- `src/log/record.rs`: bounded usage record fields and error classification.
- `src/log/stats.rs`: request and cache counters for usage records.
- `src/log/store.rs`: local log append, rotation and reading.
- `src/log/summary.rs`: aggregate counts, durations and summary output.
- `src/update/mod.rs`: update submodule exports.
- `src/update/version.rs`: release version parsing and comparison.
- `src/update/target.rs`: supported platform and archive selection.
- `src/update/release.rs`: release metadata and trusted download URL checks.
- `src/update/checksum.rs`: SHA-256 computation and checksum-file lookup.
- `src/update/archive.rs`: executable extraction from release archives.
- `src/update/replace.rs`: writable-path checks, executable replacement and Windows leftover handling.
- `src/update/notice.rs`: cached, bounded and suppressible update notices.
- `src/update/error.rs`: update error types.

## Boundaries to watch in review

| Area | Evidence and review guidance |
| --- | --- |
| Merge execution | `merge_one` and `merge_plan_inner` still coordinate GitHub writes, durable checkpoints and recovery in `src/command/merge.rs`. A deeper split needs an explicit owner for the intent saved before a merge and the SHA recorded afterwards. Exercise both resume and parallel-worker tests when changing that sequence. |
| Bundle and merge output | In `src/command/apply.rs`, `run_step` writes to GitHub while `live_rows` and `render_actions` present the result. In `src/command/merge/compact.rs`, `enrich` reads failed-job details from GitHub. Keep those reads out of rendering when extending the reports. |
| Large cohesive files | `src/main.rs` declares and binds clap arguments. `src/render.rs` renders a plan from memory. `tests/merge_run.rs` exercises the executor against a local test server, including lost approvals and stacked heads. Their sizes reflect three different jobs. Review changes against each file's job. |

## Design decisions

### Durable state has its own identity, schema and lock

State schema 2 lives in `src/merge.rs`. Report schema 1 lives in `src/merge/report.rs`. `src/merge/storage.rs` accepts older state in memory and refuses newer schemas before writing. State identity persists across invocations and completed queues; timestamps and pending ages preserve unknown legacy values. Named profiles select separate durable queue paths. The rationale for the particular profile-directory layout is not stated (reason not documented). Exact path precedence, legacy protection and field contracts are in [merge state](merge.md#resuming) and [state inspection](state.md).

The intent is durable recovery after the process ends between a GitHub write and its reply or between merge and deployment confirmation. Intent is recorded before merging, so resume can reconcile an unknown SHA without assuming the request failed. The exclusive OS lock spans apply; reloading under it replaces the pre-lock snapshot. Unique temporary siblings and atomic replacement preserve the previous state on failed saves. The queue ID names the persistent queue, not a particular selection. The choice of its particular ID construction algorithm has no stated rationale (reason not documented).

`src/command/state.rs` keeps inspection read-only and discard explicit, locked and audited. Discard ends tracking without claiming deployment. `src/command/merge/recovery.rs` instead requires descendant ancestry and complete successful run evidence for later-deploy resolution, retaining the original failed SHA. The reason is to distinguish an intentional removal from proved recovery; see [state.md](state.md#later-deploy-resolution).

### Deploy waits follow expected workflows and push filters

`src/workflows.rs` produces `expected`, `dispensed` or `indeterminate`. Push branch and path filters must both match, and rename sources count as changed paths. Execution reads workflows at the merge SHA, while dry-run prediction uses the planned head. The explicit repository exemption precedes prediction. Complete exclusions can dispense waiting; unreadable or unsupported inputs retain polling.

Expected-path tracking in `src/merge/verdict.rs` requires the expected push and resolved `workflow_run` chain paths at the merge SHA. Auxiliary green jobs cannot stand in for deployment; unresolved chains cannot silently confirm it. Two complete polls with a stable run count settle successful observations. These choices prevent unrelated or early-finishing runs from providing false deploy evidence and preserve waiting under uncertainty. Saved predictions keep resume tied to the same expectation. Legacy records retain their older verdict rules. Supported filters, completeness limits and all verdict details live in [merge.md](merge.md#what-it-waits-for).

### Plan and merge share one readiness policy

`Config::blockers` in `src/config.rs` delegates to `Pr::blockers_with` in `src/model.rs`. Discovery uses that policy before including PRs; `src/plan.rs` assembles preflight statuses for selected PRs, and execution in `src/command/merge.rs` calls the same blocker policy on a fresh snapshot. This avoids a plan presenting a blocked PR as ready while the executor refuses it, the mismatch recorded as incident 4 in [the stabilization notes](stabilization-0.9.md). Feature-base warnings do not retarget anything. Final REST mergeability and head/base checks remain separate execution guards. Readiness fields and strict selection semantics live in [plan-json.md](plan-json.md#readiness).

### Stacked merges accept only proved child effects

The planner adds child-before-parent target edges and reports the chain. `src/command/merge/stacked.rs` records child evidence shared by sequential and parallel execution. A parent head may change only through the narrow ancestry and commit-shape proof plus fresh readiness and matching REST head checks. The reason is to permit the selected child landing without treating unrelated pushes as authorized head changes. This evidence belongs to the current invocation; it is not durable permission to accept later changes. Exact conditions and review recovery live in [merge.md](merge.md#stacked-prs).

### Parallel execution preserves graph constraints and repository exclusion

The planner only adds ordering constraints: waiting is cheaper than a wrong parallel merge, and unjustified directions become questions. Stages and lanes derive from that graph; their full rules are in [planner.md](planner.md). `src/emit.rs` serializes its execution order under the [queue contract](queue-output.md).

`src/command/merge/parallel.rs` translates steps into jobs for `src/merge_scheduler.rs`. `max_parallel` is an upper bound; 1 preserves sequential execution. The ordinary non-bump path then uses the sequential executor; `--bump` uses the scheduler even at that limit. One repository has at most one active worker, even across different bases. Dependencies and barriers can lower actual concurrency. The coordinator handles confirmations, durable state checkpoints and progress while worker clients share the API budget. Failure policies block affected work and allow only proved independent continuation. These constraints preserve ordering and conservative failure handling while independent repositories overlap. A separate rationale for choosing repository-wide exclusion instead of base-wide exclusion is not stated (reason not documented). See [merge.md](merge.md) for execution and failure semantics.

### Read retries, immutable cache keys and secret redaction

`src/github/client.rs` routes reads through the bounded policy in `src/github/retry.rs`; writes take the single-attempt path. Selected server errors and secondary rate limits use backoff and jitter, while exhausted primary quotas are reported. `src/command/merge/deploy.rs` additionally bounds deploy polling and retry work by execution deadlines. The rationale for applying automatic client retries only to reads is not explicitly stated (reason not documented). Transport failures still return from the client's read call; deploy polling has its own recovery. Policy limits and request accounting are described in the README's [API budget](../README.md#api-budget).

`src/github/endpoints.rs` and `src/github/cached.rs` cache SHA-bound data; mutable PR lists, reviews, checks and branch tips are fetched again. This reduces repeated requests without freezing readiness. Cache files omit tokens, headers and patches; patch-bearing manifest changes are fetched again. Entry corruption or an unsupported cache version becomes a miss. Storage keys, bounds and pruning stay local to `src/cache/`. User controls and cache contents live in the README's [local cache](../README.md#local-cache).

`src/github/token.rs` redacts debug values, `src/github/error.rs` removes the active token from diagnostics, and `src/mcp/mod.rs` adds a `GH_TOKEN` replacement guard on tool results. Download authorization stays constrained by origin. The reason is to keep credentials out of output and prevent authenticated downloads from forwarding them to unrelated hosts; see [mcp.md](mcp.md#configuration-token-and-cache) and [publication-bump.md](publication-bump.md).

### Writable consumer preparation requires publication proof

`src/publication.rs` exposes opaque evidence only after checking execution identity, a matching new NuGet push and the exact feed version. `src/consumer_bump.rs` consumes that evidence; its backend creates isolated workspaces, verifies pins and restores, reserves a fresh remote branch and opens a draft against the consumer head. The journal marks intent before side effects so uncertain partial operations stop for inspection. The reason is to avoid inventing versions, treating duplicate pushes as new publication, changing existing checkouts or bypassing review. Standalone `bump` remains a read-only report. Full constraints and recovery belong to [publication-bump.md](publication-bump.md).

### MCP reuses commands and exposes no execution writes

`src/mcp/tools.rs` calls the command implementations so tools and CLI share report meaning. The small synchronous stdin/stdout JSON-RPC loop avoids an async runtime and large dependency tree for five tools; this rationale is recorded in [mcp.md](mcp.md). Merge execution and discard are not tools. JSON and queue compatibility rules stay in [plan-json.md](plan-json.md), [queue-output.md](queue-output.md) and the relevant command contracts.
