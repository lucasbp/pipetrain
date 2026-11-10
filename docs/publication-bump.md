# Publication-backed consumer preparation

`merge --bump` opts into consumer preparation; the default and `--no-bump` disable it.
The two flags are mutually exclusive. Writes require `--apply` and its normal
confirmation. Dry runs and the confirmation explain the potential isolated bump
branches and draft PRs before any operation. The standalone bump command and MCP
bump tool remain read-only. Enabling preparation cannot bypass publication proof,
dependency ordering, CI, or human review.

Automatic preparation is limited to selected consumer PRs already eligible for
the merge plan. PRs excluded for missing approval, failing checks or other
readiness blockers are not edited by this option. Author-hidden PRs are not bump
targets. Those consumers require a separate reviewed preparation before joining
the queue. Bump audit events are additional to the queue-job progress count.

## Publication evidence

`publication::verify` accepts the publisher execution, its workflow run, the
package id, and an `ExactFeed`. It checks repository, run id and merge SHA before
constructing opaque `PublicationEvidence`. A successful push step must contain
a matching NuGet `Pushing package.version.nupkg` followed by `Your package was
pushed.`. Duplicate or skipped pushes do not qualify. A duplicate of another
package does not invalidate an already proved push of the requested package.
The exact version must also exist in the feed. No code path uses latest.

`publication::backend::collect_run` uses the existing GitHub client to read the
run, jobs and bounded job logs. Only messages inside the successful named push
step's time window count. Auxiliary skipped jobs do not invalidate that step.
Multiple matching publisher jobs and truncated job lists fail closed. The caller
supplies the logs API base, normally the GitHub API origin, so local fake servers
can exercise the same backend. Download credentials retain the client's existing
same-origin rules. `NugetFeed` reuses the existing exact-version feed checker.

The caller owns `PublisherExecution.published_this_execution`. Set it false for
a skipped publisher or a publisher merely discovered to have been merged before
this execution. Do not synthesize an eligible execution from an old successful
workflow. A resumed workflow may qualify only when its run was recorded as part
of the same execution. Persist execution and resulting evidence with the merge
state. Workflow success alone is insufficient because NuGet push can skip
duplicates.

Current publisher pipelines need no modification for the supported NuGet log
format. Localized or changed messages, missing logs, ambiguous jobs and unavailable
feed metadata stop preparation instead of guessing. A structured publisher artifact
could replace the log adapter later.

## Consumer preparation

`consumer_bump::prepare_many` takes publication results, an explicit consumer PR
snapshot, and a `BumpGateway`. With no published results it returns `Skipped`
without reading the consumer or creating a workspace. A published result targets that
specific package and exact version. Unsupported ranges, properties, conditional
package groups, version overrides and downgrades stop for manual review.
Published dependencies are grouped into one workspace, restore pair, commit,
branch and draft PR per consumer. Skipped results do not contribute a package
version. Conflicting evidence for one package is refused before any consumer
operation. Identical evidence is deduplicated. `prepare` remains the single-result
wrapper. `PreparedBump.publications` records the evidence for the pins changed.
Literal double-quoted PackageReference and central PackageVersion attributes are
supported. XML comments are ignored. The caller must select the relevant manifests
and solution from the current consumer head, not its default branch.

`LocalBackend` provides real git/dotnet operations through argument arrays without
a shell. It creates a new private bare repository and attached worktree in a
uniquely created directory under the supplied workspace root. It fetches only the
consumer branch and verifies its SHA before checkout. Existing checkouts are never
reused, reset or cleaned. Hooks and filesystem monitor commands are disabled for
these git operations. Existing git authentication and commit identity are required;
the backend does not install credentials or modify global configuration.

Preparation requires a clean workspace, patches the requested pins, performs
`dotnet restore --use-lock-file`, then repeats with `--locked-mode`. These commands
evaluate MSBuild from the consumer checkout, so the caller may invoke preparation
only within the user's authorized apply operation against a trusted repository.
The default timeout is supplied by the caller and must be positive and bounded.
Restore changes may include only selected manifests and packages.lock.json files.
The final manifest pins and changed lock graphs must resolve the exact published
version. Any failure before commit prevents push and PR creation.

Head checks run before workspace creation, commit, push and PR creation. The
GitHub remote reserves a previously nonexistent bump branch atomically at the
consumer snapshot SHA, then pushes its corrective commit without force. A branch
already in use is refused. The draft PR targets the consumer PR's head branch,
never the default branch. The remote also checks the prepared branch SHA before
creating the draft PR. `PreparedBump.needs_review` remains true; this module does
not merge either PR or waive existing dependencies.

The last head check cannot be atomic with GitHub PR creation. A later consumer
commit requires the coordinator to replan and revalidate before merging anything.
If a later remote operation fails, a reserved branch or draft PR may already
exist. They must be inspected before retry; the backend will not overwrite them.

## Failure recovery and validation

`LocalBackend.workspace()` exposes the newly created directory even after a
partial failure. It contains a pipetrain-owned marker, the private bare repository,
and any newly prepared worktree. Failures preserve them for inspection. There is
no automatic recursive cleanup and no deletion of an existing checkout. After
inspection, the owner can remove that isolated directory; unpublished work is
retained until that decision. Failed git/dotnet output is bounded and not printed
by the backend, preventing accidental credential disclosure through diagnostics.

Tests use local HTTP fixtures, fake remotes and a fake process runner. They cover
first publication, exact feed checks, publisher skip, target duplicate versus
another package duplicate, auxiliary skipped jobs, stale workflow identity,
changed consumer heads, literal and central pins, locked restore, unexpected
restore edits, and partial failures preserving existing files. No consumer
repository, package feed, real dotnet project or GitHub mutation is exercised by
those tests. The real ProcessRunner is compiled; actual publisher/consumer use
still needs an authorized runtime validation against a real publisher and consumer.

## Merge CLI and durable resume

The default publisher step name is exactly `Push to GitHub Packages`.
`--bump-push-step NAME` selects another exact workflow step name. The publisher's
organization selects the GitHub NuGet feed; its exact newly pushed version must
exist there. No path uses latest, modifies a publisher workflow, or invents a
package version. An all-skipped or duplicate publisher produces a gray bump skip,
without reading the consumer, creating a workspace or creating a PR. That bump
skip does not skip the consumer's ordinary merge checks and gates.

Consumers are read at their planned PR head SHA. Exactly one `.sln` or `.slnx`
must exist. Supported `.csproj` and `.props` manifests containing the target pins
are discovered at that same SHA; ambiguity or unsupported pins require attention.
The default workspace root is `bump-workspaces` inside the pipetrain cache. Each
preparation uses a new private bare repository and worktree and a new bump branch.
Git/dotnet operations have a bounded ten-minute timeout.

The coordinator alone owns the optional `bump` state journal. It records publisher
identity and consumer/package contracts before merging, the confirmed merge SHA,
and successful deployment completion in the same durable store operations as the
ordinary merge state. Later preparation stores run IDs and exact versions as
inputs, then revalidates the run, SHA, push log and exact feed on resume. Opaque
publication evidence is never deserialized from state. Older state without this
execution record cannot qualify an already merged publisher for automatic bump.
Historical consumer/package contracts allow verification when the merged publisher
is absent from the rebuilt plan; current contracts take priority for the same
package. No dependency edge is removed from the scheduler graph.

Before preparation has side effects, the journal saves the consumer head, planned
bump branch, nonce and workspace root. It stores the resulting PR URL and commit
after success. A partial marker stops that consumer for inspection, without
repeating a push or opening a duplicate PR. A completed draft is reused until it
is merged into the consumer head and its merge commit is contained in the newly
planned head. The newly planned head is rechecked through publication verification and pin
inspection even when the bump merge remains in its ancestry, so a later pin revert
can produce a corrective draft. Normal readiness, CI and review checks still run
before merging the consumer. Publication fingerprints invalidate completed/up-to-date markers when
new publisher inputs or contracts appear, so another publication is reevaluated
and can produce another reviewed bump.

Each preparation emits an auditable `kind: bump` step and structured `bump`
outcome. Normal skips are gray. A prepared draft or uncertain operation is yellow
and blocks that consumer and its descendants while independent jobs continue.
These attention steps make the invocation exit with 5. Events are streamed when
they occur and appended to the JSON report after the fixed scheduler steps, so
scheduler indices remain stable. No bump draft or consumer PR is auto-approved
or auto-merged by preparation.
