# End-to-end testing plan

Goal: no surprises in real use. Every behavior that matters in a real merge queue is proven by running the real `pipetrain` binary against something that behaves like GitHub, on the three operating systems it ships for. The closest analogy is Playwright for a web app: pipetrain has no browser, so the equivalent is a harness that drives the compiled binary end to end, first against a fake GitHub and later against a disposable real sandbox.

Status: proposal. Nothing here is implemented yet. Waves are approved one at a time by the owner; items marked **Owner decision** cannot start without one.

Baseline: `origin/main` at `bbec647` (0.9.0), about 1240 tests: 274 unit tests in `src/`, 970 integration tests in `tests/`.

## 1. What exists today

| Layer | Where | What it proves | What it cannot catch |
|---|---|---|---|
| Unit | `#[cfg(test)]` modules in `src/` (planner, live rows, workflows, publishers, update) | Pure logic: ordering rules, workflow filters, text truncation, version parsing | Anything that crosses a process, a socket, the file system or the terminal |
| Library integration with a fake GitHub | `tests/common/mod.rs` (`serve`, `serve_requests`, canned replies), `tests/merge_run.rs` (scripted `Rig`), `tests/retry.rs`, `tests/api_server.rs`, `tests/search_truncated.rs` and most other files | Request shapes, retries on 5xx and secondary rate limit, error mapping and token redaction, merge scheduling, deploy polling, state on disk | Whether the fake answers like the real API. Every fixture is hand written from what the code expects, so a drift in GitHub is invisible |
| CLI through the compiled binary | Files using `CARGO_BIN_EXE_pipetrain`: `incidents.rs`, `state_cli.rs`, `merge_cli.rs`, `mcp_cli.rs`, `update_cli.rs`, `progress_cli.rs`, `profile*.rs`, `feed_cli.rs`, `log_cli.rs` and others | Argument parsing, exit codes, `--json` output, profile and state file resolution, MCP over stdio, the updater against a fake release server | A real terminal (every run has stdout and stderr as pipes, so live progress is only tested as "draws nothing"); release binaries built with `lto`, `strip` and `panic = "abort"`; installers |
| Incident regressions | `tests/incidents.rs` (six incidents of 13 Nov 2026) | Path filters dispense a deploy on `main`; a later descendant deploy resolves an old failure; plan never says ready when the merge would refuse; stacked child then parent; both parallel phases visible; stale pending shows age and the lock excludes a second process | Same limit as the fake: the scenario is only as real as the hand-written replies |
| Recorded snapshot | `tests/fixtures/train-2026-10-28/` (`files.json`, `manifests.json`, `search.json`) | Planner on a real-shaped train | It is a one-time snapshot with no refresh path and no check that the API still answers that way |
| Installers | `tests/installers.rs` | `install.sh` passes deadlines to `curl` and `wget` | `install.ps1` is not run; no installer is run end to end; the installed binary is never executed |
| CI | `.github/workflows/ci.yml` | `clippy` and `cargo test --locked` on ubuntu, macOS and Windows for every PR | `cargo fmt --check` is not run; release targets (musl, `aarch64`, `x86_64-apple-darwin`) are only built at tag time, after `cargo test` ran only on ubuntu; nothing executes the release artifacts |

Incidents the suite must keep covering, and where they live today:

| Incident | Covered by |
|---|---|
| Deploy never starts because the PR touches none of the workflow `paths:` | `incidents.rs` incident 1, `workflows.rs` unit tests |
| State stuck after a failed deploy that a later deploy resolved | `incidents.rs` incident 3 |
| Plan says ready but the merge refuses | `incidents.rs` incident 4 |
| PR stacks | `incidents.rs` incident 5, `train.rs` |
| New queue inheriting a pending record from old state | `incidents.rs` incident 6, `state_cli.rs`, `merge_state.rs` |
| Confusing merge progress | `incidents.rs` incident 2, `src/progress/multiline.rs` unit tests (render only, not a real terminal) |
| State lock between runs | `merge_state.rs`, `merge_state_reacquire.rs`, `incidents.rs` incident 6 |

## 2. Gaps for "no surprises"

Risk is what happens in real use if the gap stays open. Cost is P (hours), M (one to three days), G (a week or more, or recurring cost).

### a. Contract with the real GitHub API

- Risk: high. Every fake reply is written from the code's own assumptions. A renamed GraphQL field, a new `mergeStateStatus` value, a changed `total_count` behavior or a new rate limit header passes every test and fails the first real run.
- Cost: M for recorded fixtures, M for the scheduled contract run (needs the sandbox of item b).
- How: (1) a `record` mode in the fake server helper that proxies to a real API once and writes the exchange as JSON under `tests/fixtures/contract/`, with tokens and private names scrubbed by the existing redaction; (2) replay tests that run the CLI against those recordings; (3) a scheduled workflow (weekly, plus manual dispatch) that runs a small read-only contract suite against the sandbox organization and fails when a response no longer parses or a field the code reads is missing. The contract suite asserts shapes, not values.

### b. Real sandbox, end to end

- Risk: high. `merge --apply` has never run in a test against real GitHub. Branch protection, merge queues, required checks, the real lag between a merge and its `push` run, and real `paths:` evaluation by Actions are only modeled.
- Cost: G. Setup plus the recurring cost of Actions minutes in the sandbox.
- How: a disposable organization (or a dedicated user account) with three or four throwaway repos: one deploy workflow with `paths:`, one without filters, one `workflow_run` chain, a stack of two PRs, and a fake NuGet version gate as a required check that a script flips red or green. A setup script creates the PRs from a known commit; the run executes `pipetrain plan`, `merge` dry run and `merge --apply` from the release binary; a teardown script closes PRs and deletes branches. Triggered manually and before a release, never on every PR. Uses its own fine-grained token limited to the sandbox repos, stored as a secret of this repo only. Never pointed at a production organization.
- **Owner decision**: create the sandbox organization or account and its repos, create and store the token, and accept the Actions cost.

### c. Terminal behavior

- Risk: medium. Live progress, color and multiline redraw are only unit tested as strings, and every CLI test runs through pipes, so the code path for a real terminal (`IsTerminal` true) is never executed end to end. Windows Terminal, PowerShell 7 and macOS Terminal differ in how they handle cursor movement.
- Cost: P for snapshots and the output contract, M for the pseudo-terminal harness.
- How: (1) snapshot tests of `--help`, dry-run output and `--json` with `insta` or `trycmd` (new dev-dependency, needs approval), with time and paths normalized; (2) a pseudo-terminal harness (`portable-pty` works on Windows ConPTY, macOS and Linux) that runs `plan` and `merge` against the fake server and asserts the final screen through a terminal emulator crate such as `vt100`; (3) declare exit codes and `--json` `schema` as a versioned contract in `docs/` with one test per documented code.

### d. Injected failures

- Risk: medium to high. Retries on 502 and secondary rate limit are covered. Not covered end to end: a slow server that exceeds the client timeout mid-poll; a connection reset after the merge request is sent but before the reply (resume after interruption is covered in `tests/merge_run.rs` through the library `Rig`, not by killing the binary); a run list with more than 100 runs for one SHA (documented to stay pending and time out, see `docs/merge.md`); a process killed between merge and state save, then resumed.
- Cost: M.
- How: extend the fake server with a fault script (delay, reset after N bytes, status sequence, truncated pages) and add CLI tests that kill the child process at a named point (the fake server blocks on a request until the test kills the process), then run again with the same state file and assert the resume. Every documented failure in `docs/merge.md` gets one such test.

### e. Concurrency

- Risk: medium. Two processes on one state file are covered for the lock itself. Not covered: two real `merge --apply` processes started at the same time on the same profile, and a `state discard` racing an active merge, through the binary.
- Cost: P to M.
- How: CLI tests that start two binaries against one fake server and one state directory, with the server holding the first one at a known request, and assert that exactly one proceeds and the other exits with the lock message and no write.

### f. Property tests and fuzzing

- Risk: medium. The planner promises to only add ordering constraints and never remove one, and to ask instead of guessing; today that is proven by examples only. The workflow parser accepts user YAML.
- Cost: M.
- How: `proptest` (new dev-dependency) generating random sets of PRs, bases, stacks and file overlaps, asserting invariants: the output order respects every edge, no cycle is silently broken, stacked children come before parents, adding an unrelated PR never reorders the others. For workflows: random `branches`, `branches-ignore`, `paths`, `paths-ignore` and file lists, compared against a small reference implementation of the Actions filter rules. Optional later: `cargo fuzz` on the YAML and JSON parsers (Linux only, nightly).

### g. Platforms, installers and the release binary

- Risk: high for a release. `cargo test` runs on three systems, but the shipped artifacts (musl static builds, `aarch64`, Intel macOS, release profile with `panic = "abort"`) are never executed. `install.ps1` is never run. The updater is tested against a fake release, not with a real downloaded archive.
- Cost: M.
- How: (1) add `cargo fmt --check` to CI; (2) in `release.yml`, run `cargo test --locked` on the three systems before building, not only on ubuntu; (3) after each build, a smoke job per target that unpacks the archive and runs `pipetrain --version`, `pipetrain --help` and one `plan` against a small fake server started by the job; (4) a scheduled installer smoke that runs `install.sh` (ubuntu, macOS) and `install.ps1` (Windows, PowerShell 7) against the latest real release and then `pipetrain update --check`.
- **Owner decision**: whether release smoke blocks publishing (recommended) or only reports.

### h. Coverage as a gap finder

- Risk: low by itself; it points at the other gaps.
- Cost: P.
- How: `cargo llvm-cov` in a manual or weekly workflow that publishes the HTML report as an artifact. Use it to list uncovered branches in `src/command/merge*`, `src/github/` and `src/merge/storage.rs` and turn the meaningful ones into tests. No percentage target and no gate.

### i. Use by agents

- Risk: medium. Agents depend on `--json`, `--strict` and exit codes, and on the MCP server in `src/mcp/`. `tests/mcp_cli.rs` already checks the five tool names, read-only hints, protocol errors and token redaction. Not pinned: each tool's `inputSchema` and the shape of each tool result as a versioned contract, and a check that `docs/mcp.md`, `docs/plan-json.md` and `docs/queue-output.md` match what the binary emits.
- Cost: P to M.
- How: snapshot the `tools/list` answer and one result per tool (normalized); generate a JSON Schema per `--json` command and validate both the documented examples and real output against it; a test that fails when a documented field is missing from the output or an emitted field is undocumented.

## 3. Waves

| Wave | Content | Effort | Codex alone? | Needs the owner |
|---|---|---|---|---|
| 0 | `cargo fmt --check` in CI; keep the fixture helper and lock isolation from the hardening PR as the rule for new tests | P | Yes | No |
| 1 | Output contract: snapshots of `--help`, dry run and `--json`; exit codes documented and tested; MCP `tools/list` and tool results pinned (c.1, c.3, i) | M | Yes, once the snapshot crate is approved | **Owner decision**: approve `insta` or `trycmd` as a dev-dependency |
| 2 | Injected failures and concurrency through the binary: fault script in the fake server, kill and resume, two processes on one state, more than 100 runs (d, e) | M | Yes | No |
| 3 | Properties: planner invariants and workflow filters with `proptest` (f) | M | Yes, with a review of the invariants | **Owner decision**: approve `proptest` as a dev-dependency; confirm the invariant list |
| 4 | Release confidence: tests on three systems before building, smoke of every release artifact, installer smoke, coverage report (g, h) | M | Yes for the workflow files | **Owner decision**: whether smoke blocks the release |
| 5 | Pseudo-terminal harness for live progress on ConPTY, macOS and Linux (c.2) | M | Partly; output needs human review once | No |
| 6 | Real sandbox: recorded fixtures, weekly contract run, `merge --apply` end to end before each release (a, b) | G | Scripts yes; setup no | **Owner decision**: sandbox organization or account, repos, scoped token stored as a repo secret, Actions cost |

Order rationale: waves 0 to 2 remove the most likely surprises with no new credential and no recurring cost. Wave 6 is the only one that proves behavior against GitHub itself, so it is the largest gain, but it needs decisions and a credential first; recorded fixtures (a.1, a.2) can start as soon as the sandbox exists, before the scheduled run.

## Rules for every wave

- Tests never use a real token from a production organization and never run `--apply` outside the sandbox of wave 6.
- New temporary directories use the shared helper in `tests/common/mod.rs`; a test that spawns child processes while other tests in the same binary take the state lock lives in its own test binary.
- No comments in code; the reason for a test design goes in this document or next to the contract it protects in `docs/`.
- Public repo: no private organization or repo names in fixtures, recordings or docs.
