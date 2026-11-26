# Opt-in GitHub sandbox tests

`tests/e2e_sandbox.rs` runs the real compiled CLI against four repositories.
API, web and static are private. Guarded is public and requires a review on `main`.
The harness checks exit codes, merge JSON, local state JSON and text, and
Actions evidence. It is ignored in the normal suite, never runs in CI, and has
no scheduled job. A human must run it with an authenticated `gh` installation.

| Repository | Coverage |
|---|---|
| `lucasbp/pipetrain-sandbox-api` | A change outside `deploy/**` dispenses the wait without the 120 second grace; a change to `deploy/version.txt` confirms the expected workflow. |
| `lucasbp/pipetrain-sandbox-web` | A failed deploy is resolved by a later successful descendant deploy, reproducing the later-deploy recovery incident. Also participates in the parallel case. |
| `lucasbp/pipetrain-sandbox-static` | Deployment outside Actions is dispensed through `merge.no_deploy_repos`, alone and alongside web. |
| `lucasbp/pipetrain-sandbox-guarded` | Required review refuses a normal merge; `--admin` merges with identity, timestamp and local audit evidence; a failing PR check still refuses `--admin`. |

From the repository root in PowerShell 7:

```powershell
$env:PIPETRAIN_E2E='1'; cargo test --test e2e_sandbox -- --ignored --test-threads=1
```

Remove the activation variable afterwards with `Remove-Item Env:PIPETRAIN_E2E`.
Without its exact value `1`, even explicitly requested ignored tests return
immediately with a skip message. Add `--nocapture` to see successful tests' skip
messages and wall times. Run only one harness process at a time: these tests
reset shared sandbox branches and must not run concurrently.

Every write helper and scenario checks the single repository guard, which
requires `lucasbp/pipetrain-sandbox-`. Startup parses the versioned
`tests/e2e/sandbox.toml` or, for guarded scenarios, `tests/e2e/guarded.toml`,
checks every repository-bearing configuration field, and rejects profiles, org
aliases, a configured state file and authors. Guarded uses the owner's real
`require_approval = true` setting and one merge at a time. The layer glob
only assigns ordering; it is not a discovery allowlist. Every merge therefore
requires explicit, guarded `lucasbp/pipetrain-sandbox-<name>#<number>` references.
There is no broad discovery selection or empty selection.

The harness uses the `gh` login; pipetrain obtains its token itself through
`gh auth token`. The harness never reads or stores tokens and removes inherited
token variables from child processes. It pins `gh api` to `github.com` and
removes any inherited `PIPETRAIN_API_URL` override. Each scenario uses an absolute
config path, its own temporary state and cache, disabled logging and update
checks, and null stdin. State commands accept neither `--no-cache` nor
`--no-progress`, so those flags are passed only to merge.

Each scenario resets `main` to the runtime-resolved annotated `baseline` tag
before preparation. It creates unique `e2e/<scenario>-<phase>-<unix-millis>`
branches and waits up to 60 seconds for clean PR mergeability, or settled
`mergeable_state == "blocked"` with `mergeable == true` in guarded. Cleanup on normal
return or panic closes its remaining open PRs, deletes its branches, resets
`main`, and removes temporary files. Failures are printed without panicking.
Abrupt process termination cannot run cleanup; the next run still resets `main`
but leftover PRs or branches from a killed process may need manual removal.
API subprocesses have a 20 second limit, merge subprocesses a four minute limit,
and the manual Actions waiter a three minute limit with three second polling.
The guarded check-run waiter has the same three minute limit and polling interval;
it waits for checks to appear and for all of them to complete.

The expected API deploy case kills its own merge subprocess after the state
file contains the accepted merge SHA and the expected workflow prediction.
The harness stays alive for cleanup. It checks the pending record with
`state show`, verifies the SHA on GitHub, then starts another CLI process using
the same state file and PR. The resumed request trace must show deploy reads
at that SHA and zero merge PUTs. The successful run settles the pending record.

Web recovery selects the already merged fix PR B explicitly. Discovery reports it
under `not_merging` as already merged, while pending A is resumed from the same
state file. No new merge is scheduled. The later SHA and run evidence must match
both the resume resolution and its single audit entry.

The parallel case performs one JSON apply. It also checks real `state show`
text, then passes the returned steps through pipetrain's public merge text
renderer to assert both PR references and the exact deploy summary. This covers
the renderer without a second apply or extra Actions run; it does not exercise
the text-mode merge subprocess wiring. `state show` itself has no deploy summary.

The guarded refusal cases assert no merge steps, the selected PR's `not_merging`
reason, its live open and unmerged state, and an unchanged baseline tip. Nothing
merged returns exit code `0` without `--strict`; `applied` remains `false`. The
override case checks the login from `gh api user --jq .login`, the RFC 3339 audit
timestamp, GitHub's `merged_by`, the merge SHA in `state show --json`, text audit
output, and no remaining pending records. The two cases without workflows assert
no check runs on their head SHA. Only the failing-check case adds a workflow,
before opening its PR, and expects exactly one completed check with `failure`.

Budget about eight minutes for a healthy full execution, subject to GitHub
scheduling and API latency; this is an estimate, not a measured duration. It
needs five Actions runs: four short deploy jobs (one API, two recovery web jobs,
and one parallel web job), plus one deliberately failing guarded PR check.
The API and web jobs use private-repository Actions minutes. The guarded check
runs on a standard runner in a public repository. Forced baseline resets are skipped by the job condition;
the baseline commit also carries `[skip ci]`. No workflows are rerun by the test.

## Recreating the sandbox

These are manual setup instructions, not part of the harness. Create each repo
with `gh repo create lucasbp/pipetrain-sandbox-<x> --private --add-readme`, replacing
`<x>` with `api`, `web`, and `static`. Clone them into separate directories and
ensure their default branch is `main`. Leave branch protection and PR checks
disabled. Keep `README.md` in all three. Add `deploy/version.txt` containing `0`
to API and `index.html` containing a minimal page to static. Web must have no
`FAIL_DEPLOY` file at baseline. Static has no workflow.

In API, save this as `.github/workflows/deploy.yml`:

```yaml
name: deploy
on:
  push:
    branches: [main]
    paths: ['deploy/**']
jobs:
  deploy:
    if: ${{ !github.event.forced }}
    runs-on: ubuntu-latest
    steps:
      - run: sleep 5
```

In web, save this as `.github/workflows/deploy.yml`:

```yaml
name: deploy
on:
  push:
    branches: [main]
jobs:
  deploy:
    if: ${{ !github.event.forced }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          sparse-checkout: FAIL_DEPLOY
          sparse-checkout-cone-mode: false
      - run: |
          sleep 5
          if [ -f FAIL_DEPLOY ]; then exit 1; fi
```

In each clone, commit the baseline content, tag it with an annotated tag, and
push it. If recreating an existing repository, remove or rename that repository
first yourself; these instructions assume a new repo and no existing tag.

```powershell
git add .
git commit -m 'Sandbox baseline [skip ci]'
git tag -a baseline -m 'Sandbox baseline'
git push origin main baseline
```

The tag must point at the setup commit containing the files above. The harness
resolves it with `gh api repos/lucasbp/<repo>/commits/baseline --jq .sha`; no
commit SHA is hardcoded. GitHub authentication must allow Contents, Pull
requests and Actions reads, plus Contents and Pull requests writes, including
force-resetting these unprotected sandbox `main` branches.

## Guarded repository setup

Create `lucasbp/pipetrain-sandbox-guarded` as a public repository with
default branch `main`, containing only `README.md` at baseline and no workflows.
Create and push the annotated `baseline` tag on that setup commit as above.
Keep rulesets disabled and required status checks unset. As the repository
administrator, apply this classic branch protection after pushing the baseline:

```powershell
'{"required_status_checks":null,"enforce_admins":false,"required_pull_request_reviews":{"required_approving_review_count":1,"dismiss_stale_reviews":false,"require_code_owner_reviews":false},"restrictions":null,"allow_force_pushes":true,"allow_deletions":false}' | gh api -X PUT repos/lucasbp/pipetrain-sandbox-guarded/branches/main/protection --input -
```

One account suffices: the authenticated repository administrator also authors
the PRs, and GitHub refuses self-approval. Each PR therefore stays review-blocked
without needing a second user. `enforce_admins = false` permits the administrator
to merge past that missing review. `allow_force_pushes = true` lets the harness
reset `main` to baseline during preparation and cleanup.

`guarded.toml` dispenses deployment through `merge.no_deploy_repos`. The failing
check scenario adds `.github/workflows/fail.yml` only on its branch, with
`on: pull_request`, one `ubuntu-latest` job and `run: exit 1`. It costs one extra
Actions run, is never merged, and is removed with its branch. The other guarded
scenarios add only `e2e.txt` and start no Actions runs. Authentication must also
permit creating the workflow file for that single failing-check scenario.
