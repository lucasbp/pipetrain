# Agent guide

Read this first. `CLAUDE.md` imports it.

## What this is

pipetrain is a Rust CLI that plans and executes merge queues across repositories. Planning is read-only on GitHub. `bundle --apply` creates deploy branches, retargets PRs and optionally opens deploy PRs. `merge --apply` merges PRs and waits for deployments; `merge --bump --apply` can also commit and push isolated dependency branches and open draft PRs. Standalone `bump` is a dry run unless given `--apply`, which opens one draft PR per repo after proving the version in the feed; the MCP `bump` tool stays a dry run. Start with [common flows](docs/flows.md); see the README's scope and [merge contract](docs/merge.md) for boundaries.

## Layout

- `src/main.rs` parses the CLI; `src/command.rs` and `src/command/` orchestrate commands.
- `src/planner.rs`, `src/planner/` and `src/plan.rs` build the dependency graph and reports. Ordering rules live in [docs/planner.md](docs/planner.md).
- `src/merge.rs`, `src/merge/`, `src/merge_scheduler.rs` and `src/command/merge/` handle execution, durable state, recovery and parallel workers.
- `src/github/`, `src/cache/`, `src/config.rs` and `src/profile/` handle API access, caching and configuration.
- `src/publication.rs` and `src/consumer_bump.rs`, with their submodules, handle publication proof and isolated consumer preparation.
- `src/mcp/` exposes read-only tools using the command implementations. Keep reusable output in a function returning it and print in the command's `run` path.
- `docs/` holds contracts and design notes; `tests/` holds integration tests. See [docs/architecture.md](docs/architecture.md) for the full module map and design decisions.

## Validation

Before saying a change works, run exactly these checks:

```powershell
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The crate uses edition 2024. CI uses the newest stable Rust through `dtolnay/rust-toolchain@stable` and runs clippy and tests on Linux, macOS and Windows for pushes to `main` and pull requests. CI does not run fmt. New stable toolchains have flagged new lints before, so run `rustup update stable` before trusting a local clippy run.

## Rules

- No comments in code. Put the reason in `docs/`.
- Never run `--apply` against a real repository except the `lucasbp/pipetrain-sandbox-*` repositories through the e2e harness described below.
- `--json` contracts are versioned. Update the matching contract document in the same change when output changes; preserve existing meanings. Queue output has its own stable contract in [docs/queue-output.md](docs/queue-output.md), and plan JSON is in [docs/plan-json.md](docs/plan-json.md).
- This is a public repo. No private organization names, private repository names or internal URLs in code, tests or docs. Use placeholders such as `acme/api`.
- No secrets in code or tests. Use fake tokens such as `ghp_FAKE0123456789`. Runtime authentication comes from `gh auth token` or `GH_TOKEN`; never print or persist a token.
- `pipetrain.toml` is local and gitignored. Only `pipetrain.example.toml` is committed.
- The planner only adds ordering constraints. If it cannot justify a direction, it asks a question instead of guessing.
- Git: one worktree per change, rebased on freshly fetched `origin/main`, opened as a PR. Do not push directly to `main`. Inspect `git status` before committing and stage only your own files.

## Sandbox e2e

The opt-in harness requires authenticated `gh` and writes only to its guarded public sandbox repositories. Run one harness process at a time from the repo root:

```powershell
$env:PIPETRAIN_E2E='1'; cargo test --test e2e_sandbox -- --ignored --test-threads=1
```

Remove the activation variable afterwards with `Remove-Item Env:PIPETRAIN_E2E`. The tests are ignored in the normal suite and do not run in CI. Read [docs/e2e-sandbox.md](docs/e2e-sandbox.md) for setup, guards, isolation, cleanup and interrupted-run limits.

## Tests that must not be deleted

- `tests/incidents.rs`: incident regressions for workflow filters, concurrent progress, later-deploy recovery, readiness, stacked PRs and state locking.
- `tests/e2e_sandbox.rs`: opt-in tests of the real CLI against guarded GitHub sandboxes.
- `tests/merge_state.rs`, `tests/merge_state_process.rs`, `tests/merge_state_reacquire.rs` and `tests/state_cli.rs`: state persistence, identity, schema compatibility, lock ownership, reloading and audited recovery.
- `tests/example_config.rs`: example configuration validation.
- `tests/installers.rs`: installer checks.
- `tests/merge_run.rs` and `tests/preflight.rs`: execution and shared-readiness coverage complementing the incident regressions.
