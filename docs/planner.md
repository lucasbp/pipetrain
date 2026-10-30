# How the planner decides

The planner turns a set of ready PRs into a directed graph: an edge `A -> B` means "A merges before B". Stages and lanes are read off that graph. Everything is deterministic: same PRs and same config give the same plan.

The guiding principle is **when in doubt, serial**. Waiting is cheap and a wrong parallel merge is not, so every rule only ever adds an ordering constraint. The planner never removes one, and it never guesses a direction it cannot justify: it asks a question instead (see "Questions").

Code: `src/planner.rs` (rules, graph, stages, lanes), `src/discovery.rs` (which PRs are in), `src/contained.rs` (PRs already in the base or the train by commit), `src/content.rs` (same, by file content), `src/bundle.rs` (deploy bundling suggestion), `src/detect.rs` (Depends-On and package bumps), `src/config.rs` (gate, layer, target matching).

## What goes into the plan

Only PRs that are ready: not merged, not closed, not draft, approved (or with no review decision at all, see below), checks green and no merge conflict. A PR that fails any of these shows up under `skipped` with the reasons and takes no part in ordering.

Why only ready PRs: the plan answers "in what order do I merge what can be merged now". A PR that is not ready cannot be merged, so ordering it would be fiction. When a skipped PR matters to a ready one, the planner raises a question instead (`sibling-not-ready`, `stacked-not-ready`, `depends-on-open`).

### Already in the base or in the train

A ready PR can still have nothing left to merge: its commits may already be in its base branch (merged by another route, or its branch was merged into the base directly), or already in the train branch. Planning it would put a no-op merge in the order.

After discovery and before anything is read about the PR's files, each ready PR (explicit ones included) is compared on GitHub, through GraphQL `ref.compare(headRef: <sha>)`, in batches of up to 20 comparisons per request, against:

- its base branch, always;
- the train branch of the run date (`train_prefix` + the date in the configured `train_date_format`, default `dd-mm-yyyy`), unless the PR targets the train branch or comes from it.

When `aheadBy` is 0 against a branch, the PR head adds nothing there, and the PR goes to `skipped` with the single reason `already in <ref>`, where `<ref>` is the first branch that contains it. It takes no part in ordering and raises no question.

Limits of the check:

- A branch that does not exist in the repo counts as "not contained", so a repo without that train branch is never skipped because of it.
- A PR with no head SHA is not checked.
- Any GraphQL error other than a missing ref aborts the run, like any other GitHub failure.
- The commit check alone does not see a squash merge: the squash commit is a different commit, so the PR still looks ahead. The content check below covers that.

#### Same content in the base or in the train

A second check runs on the PRs that survive the commit check, once their changed files are loaded. For each branch from the list above, it compares the Git blob id at `<branch>:<path>` with the one at `<head sha>:<path>`, for every file the PR changes, through GraphQL `object(expression: ...) { oid }`. If every file has the same blob id on that branch, the base already holds exactly what the PR would bring, typically because another PR was squash-merged first. The PR goes to `skipped` with the single reason `already in <ref> (same content)`.

What it detects:

- A PR whose every changed file already has identical content in the base or in the train branch, however it got there (squash merge of another PR, a cherry-pick, a manual commit).
- A deletion that is already applied: a path missing on both sides counts as equal, as long as the branch itself exists.

What it does not detect, and keeps in the plan:

- Renamed files. Only the new path is compared, so a rename is kept unless the new path already matches the branch.
- PRs with more than 100 changed files, and PRs with no changed files.
- PRs whose file list is incomplete (the files API stops at 300 files).
- Partial overlap: if any one file differs, or any blob id could not be read, the PR is kept. When in doubt, keep.

Cost: the commit check and this one are separate. This one needs 2 lookups per file plus 1 per branch to confirm the branch exists, sent in batches of at most 50 lookups per request, all through the same request guard as every other call. A 100-file PR against one branch is 201 lookups, 5 requests. Files are loaded only for PRs that the commit check did not already remove.

### Bundling deploys

When 2 or more ready PRs of one repo target the same base, and that base is not a train branch (it does not start with `train_prefix`, compared case-insensitively), the plan adds a `bundles` entry. The suggestion is to create the train branch from that base, retarget those PRs to it, merge them there, and open one PR from the train branch to the base: 1 deploy instead of N. If one of the PRs already has the train branch as its head, it is kept as the carrier and the others retarget to its branch.

This is a suggestion only. It is not an ordering rule, adds no edge and changes no stage or lane; `plan` never creates a branch and never retargets a PR. Why it exists and how to apply it by hand: [saving-pipelines.md](saving-pipelines.md). Fields: [plan-json.md](plan-json.md), section "Bundle".

Review and checks, as read from GitHub:

- Review: `APPROVED`, `CHANGES_REQUESTED` or `REVIEW_REQUIRED` from the review decision. When the repo has no decision, the latest opinionated reviews are used. Reasons: `changes requested`, `review required`, `not approved`.
- Checks: success is ready, failure or error is `checks failing`, anything else is `checks pending`. Mergeability that GitHub has not computed yet is `mergeability unknown, retry in a minute`, because it often follows a merge into the base and a conflict may hide behind it. No checks at all is `no checks reported`, unless `allow_missing_checks = true` or the repo matches `checks_optional_repos`.
- `merge conflict` when GitHub reports the PR as conflicting.

How PRs are discovered (scope, in this order of precedence):

1. `train`: the head branch is the train branch (`train_prefix` + the date in the configured `train_date_format`, default `dd-mm-yyyy`; case-insensitive).
2. `shared-branch`: the same head branch is open in 2 or more repos. Ignored for this purpose: the train branch, anything starting with `train_prefix` or with an `ignore_branch_prefixes` entry, and `main`, `master`, `develop`, `development`.
3. `into-train`: the base branch is the train branch.
4. `explicit`: PRs passed on the command line. They skip discovery and go through the same readiness check. A PR that cannot be found aborts the run.
5. `released`: only with `--all`. Every open PR whose review is approved, whatever its branch. Inside `--all` the precedence is: head is the train branch gives `train`; base is the train branch gives `into-train`; anything else is `released`. An approved PR that fails the readiness check goes to `SKIPPED` with the reasons, and a PR that is not approved is ignored without a trace. Shared branches are not looked for, so `sibling-not-ready` and `stacked-not-ready` are not raised. `--all` and explicit PRs are mutually exclusive.

Anything else is discarded without a trace.

## Rules, in the order the code applies them

Order matters: when two rules disagree, the earlier one wins (see "Conflicts").

### 1. depends-on

A PR body line `Depends-On: org/repo#123` means "123 goes before me". Accepted forms: `org/repo#123`, `#123` (same repo) and a `https://github.com/org/repo/pull/123` URL. Several values can share a line, separated by comma or space. The line may be quoted (`>`) or bulleted, the keyword is case-insensitive, and HTML comments are ignored.

Why it comes first: it is the only rule where a human said the order out loud. Every inferred rule yields to it.

If the target is in the plan, an edge is added. If it is not:

- already merged: a line under `notes`, no constraint;
- open: question `depends-on-open`;
- closed without merging: question `depends-on-closed`;
- not found, or not readable by the token: question `depends-on-missing`.
- beyond the lookup cap of 20 external references per run (kept to protect the API budget): question `depends-on-not-checked`.

### 2. package

When a PR bumps a package that another repo in the plan publishes, the publisher goes first. Why: the consumer's build needs the new version to exist.

Bumps are read from changed files: `.csproj`, `.fsproj`, `.props` and `.targets` (`PackageReference` or `PackageVersion` with `Include` or `Update` and a `Version` or `VersionOverride`), and `package.json` (semver values, ignoring `name` and `version`). Lock files are ignored.

Publishers come from a scan of the repos that have a ready PR (manifests found by file name: `.csproj`, `.fsproj`, `package.json`, skipping `node_modules`, `bin`, `obj`, `.git`; shallowest first, at most 40 per repo), then `[packages]` in the config overrides the scan for the package it names. The scan only runs when some PR bumps a package that is not pinned in `[packages]`.

A PR is never made to wait for its own repo.

### 3. stacked

Within one repo, if PR J has as base the head branch of PR I, then J goes before I. Why: J merges into I's branch, so J's changes ride along when I merges. Merging I first would orphan J.

### 4. target

PRs that deploy to the same target are serialized, lowest PR number first (adjusted so it never contradicts an edge already present). The target is `<group>@<base branch>`, where `<group>` is the `[targets]` value for the repo, or the repo itself (`org/repo`) when it is not listed. So two PRs against `main` in the same repo are serial, and two repos mapped to the same group are serial with each other.

Why: two merges to one deploy pipeline race each other. This is the rule that makes "same repo, same branch" serial without anybody asking.

### 5. gate

A gate PR is one that needs a human to stop and confirm before anything else proceeds:

- `ddl`: a changed file matches `deploy.ddl_globs`. Checked first.
- `infra`: the repo is in `deploy.infra_repos`, or a changed file matches `deploy.infra_globs`.

The reason shown is the first matching path, or `repo is listed in deploy.infra_repos`.

Every gate PR is ordered before every PR that is not a gate. Gate PRs have no edge among themselves: the planner does not choose which migration goes first. They are reported in `gate`, are marked `manual_pause`, and are kept out of the lanes.

Why gates are applied before layers: a migration must land before the code that reads it, whatever layer that code lives in. Letting the layer rule decide first could put the migration after its consumer.

### 6. layer

For each head branch open in 2 or more repos, PRs are ranked by the `[[layer]]` order in the config (first match on `org/repo`), and each rank goes before the next rank present. Why: a change that spans repos has to land bottom-up (library, client, API, backend-for-frontend, front) or the upper layer deploys against something that does not exist yet.

PRs of the same layer have no edge between them from this rule. A PR whose repo is in no layer raises `layer-unknown` and is left out of this rule.

Why the layer map is config and not code: layers are the shape of one company's architecture. Nothing in a PR says "this repo is a client library". Guessing from names would produce confident wrong orders.

## Conflicts

Rules add edges one by one. An edge that would close a cycle is rejected, and the earlier edge stands. What happens next depends on the rule that lost:

- `depends-on`, `package`, `stacked`: the loss is reported as `edge-conflict` on the PR that was supposed to wait.
- `target` and `gate`: silent. These are defaults for safety, not statements about the change, so losing one is expected.
- `layer`: reported as `edge-conflict`, except when either PR is a gate, in which case it stays silent (the gate rule already won, on purpose).

An edge that already exists under another rule is not added a second time, so `after` shows the first rule that claimed it.

To change an outcome the planner got wrong, add a `Depends-On` line to the PR that should go first. It is rule 1, so it wins.

## Stages and lanes

**Stage**: the longest chain of predecessors ending at the PR, counting from 1. PRs in one stage have nothing between them and can merge at the same time. Gate edges are not counted: a gate PR is run first by hand, and counting it would push every stage down by one and hide the real parallelism. `max_parallel` is only a number shown beside `peak_parallel`; it does not split stages.

**Lane**: a way to draw the graph as chains, so a person can follow one chain at a time. PRs are walked by stage and, within a stage, in id order. Each PR joins the first lane whose last PR is one of its predecessors; if none is, it starts a new lane. This is greedy, not optimal: it gives a readable decomposition, not the fewest lanes. Gate PRs are never in a lane.

A PR can have predecessors in other lanes. The `after` lines in the text output list the ones that matter. An edge is `implied` when another predecessor of the same PR already reaches it, and is hidden in the text output.

## Questions

A question is something the planner could not decide or that you should know before merging. It never changes the exit code. Each has a stable `code`.

| Code | Raised in | Meaning | How to resolve |
|---|---|---|---|
| `layer-unknown` | planner | The PR shares a head branch with another repo, but its repo is in no `[[layer]]`, so its place is undecided. | Add the repo to a `[[layer]]` in the config. |
| `package-ambiguous` | planner | The PR bumps a package that more than one other repo publishes. Edges are still added to every publisher in the plan. | Pin the publisher under `[packages]`. |
| `tree-truncated` | scan | GitHub truncated a repo's file tree, so a publisher may have gone unseen. `pr` is `null`. | Pin the package under `[packages]`. |
| `files-truncated` | planner | The PR has more than 300 changed files, only 300 were read, so package and DDL detection may be incomplete. | Check the diff by hand, or add the dependency with `Depends-On`. |
| `depends-on-invalid` | planner | A `Depends-On` value is unreadable, or points at the PR itself. | Write it as `org/repo#123`, or remove the line. |
| `depends-on-open` | planner | Waits for a PR that is open but not in the plan (not ready, or outside the train). | Get it ready and into the plan, or remove the `Depends-On`. |
| `depends-on-closed` | planner | Waits for a PR closed without merging. | Fix or remove the `Depends-On`. |
| `depends-on-missing` | planner | The referenced PR could not be found, or the token cannot read its repo. | Check the reference and the token's access. |
| `depends-on-not-checked` | planner | The plan lists more than 20 outside references, so this one was not looked up. | Check that PR by hand, or plan fewer PRs at once. |
| `gate-has-predecessor` | planner | A DDL or infra PR must come after a non-gate PR by an earlier rule (for example a stacked PR into the train branch), so it cannot run first. | Confirm the DDL can run after that PR, or split it out. |
| `manifests-truncated` | scan | A repo has more than 40 package manifests and only the shallowest were read. `pr` is `null`. | Pin the package under `[packages]`. |
| `edge-conflict` | planner | A rule wanted X before Y, but an earlier rule already puts X after Y. The earlier rule won. | If the order is wrong, add a `Depends-On` to the PR that should go first. |
| `sibling-not-ready` | discovery | The branch is also open in another repo whose PR is not ready (reasons given). Not raised for explicit PRs. | Hold this PR until the sibling is ready, or drop the sibling from the change. |
| `stacked-not-ready` | discovery | A PR that merges into the train branch is not ready, so its changes would miss this one. Not raised for explicit PRs. | Get that PR ready before the train, or drop it from the deploy. |

Only `layer-unknown`, `package-ambiguous`, `tree-truncated` and `manifests-truncated` are fixed by editing the TOML. The rest are fixed on the PRs themselves, because they describe the state of the change, not the shape of the architecture.

## Known limits

- A PR with more than 300 changed files is read partially (`files-truncated`).
- Package publishers are found by manifest scan, at most 40 manifests per repo, and a truncated tree can hide one.
- No rule reads code: ordering is inferred from branches, files and PR text, never from what the code does.
- The planner decides order, not safety. It does not know whether a migration is backward compatible.
