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

## Architecture review

- Start with the closest workflow in [the module map](docs/architecture.md). Keep a rule with the data and tests that explain it. Split a module when two parts change for different reasons and the caller can pass a small, explicit input or result across the boundary. File length alone does not justify a split.
- Keep flag parsing, terminal output and exit handling at the CLI boundary. MCP handlers adapt the same application operation to their read-only protocol. Commands coordinate policy and I/O. Keep planner rules, verdicts and value comparisons callable with in-memory inputs. Put HTTP, Git and filesystem operations in the adapter for that resource.
- Trace the current callers before sharing or moving code. If two paths enforce the same rule, put it in their common domain module and import it from there. Keep genuinely different rules explicit, such as expected workflow verdicts and legacy verdicts. Prefer private modules and retain published paths through re-exports when moving library types.
- In review, trace a changed behavior from its CLI or MCP entry point to the rule and any external effect. Use the tests for that boundary. For a move, check existing imports, serialized fields, stdout/stderr, exit codes and dry-run behavior where affected. Add focused coverage when an affected contract has no regression test. Run the validation below.

### Automated dependency check

`cargo test --locked --test architecture` checks the production source of `planner`, plan assembly (`plan`), workflow verdicts (`merge::verdict`) and the plan renderer (`render`), including their inline and conventional file submodules. Pass data such as `github::Run` to these modules. Move command orchestration and network reads to a caller or adapter.

The check parses Rust paths and grouped, renamed or block-local imports. It flags direct dependencies on `crate::command`, `github::Client`, registry sessions and checkers, the `ureq` transport API and standard network sockets. It accepts passive types such as `ureq::http::StatusCode` and `std::net::IpAddr`. Use explicit imports from namespaces whose glob would expose a forbidden dependency. Diagnostics identify the source line and resolved path.

This is a source check. It skips `#[cfg(test)]` items and test functions. It inspects expression-list macro arguments but does not expand macros, infer receiver types or trace calls through other modules. Review indirect effects, additional transports and module cohesion in the PR.

## Validation

Before saying a change works, run exactly these checks:

```powershell
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The crate uses edition 2024. CI runs the format check on Linux and runs Clippy with fatal warnings and the locked test suite on Linux, macOS and Windows for pushes to `main` and pull requests. The suite includes the dependency check above and the CLI/MCP contract tests. A separate Linux job checks Cargo.lock against the RustSec advisory database as described in [Dependency audit](docs/dependency-audit.md). CI uses the newest stable Rust through `dtolnay/rust-toolchain@stable`, so run `rustup update stable` before trusting a local Clippy run.

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
