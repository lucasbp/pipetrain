# Agent guide

Read this first. `CLAUDE.md` imports it.

## What this is

pipetrain is a read-only Rust CLI that plans the merge order of pull requests across several repos. It reads GitHub, builds a dependency graph and prints the order. The only write it does to GitHub is `pipetrain bundle --apply`: it creates a deploy branch and changes the base of the PRs bundled into it. See "Out of scope" in the README.

## Layout

- `src/main.rs`, `src/lib.rs`: CLI entry point and library root.
- `src/planner.rs`, `src/planner/`: the graph and the ordering rules. The rules are documented in `docs/planner.md`.
- `src/github/`: GitHub API client. `src/config/`, `src/profile/`: config and profiles.
- `src/doctor/`: the `pipetrain doctor` checks. `src/cache/`: local cache.
- `src/detect.rs`, `src/cargo.rs`, `src/gomod.rs`, `src/pyproject.rs`, `src/manifest.rs`: package detection from manifests.
- `docs/`: the output contracts (`plan-json.md`, `queue-output.md`) and design notes.
- `tests/`: integration tests, one file per feature.

## Commands

```
cargo build --release
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

Edition 2024: needs a recent stable Rust. CI runs clippy and tests on Linux, macOS and Windows for every push to `main` and every pull request.

## Rules

- **No secrets in code or tests.** Tests use fake tokens such as `ghp_FAKE0123456789`. The real token comes from `GH_TOKEN` or `gh auth`. Never print it.
- **`pipetrain.toml` is local and gitignored.** Only `pipetrain.example.toml` is committed.
- **Public repo.** Do not put private org names, private repo names or internal URLs in code, tests or docs.
- **No comments in code.** `src/` has none today. Write the reason in `docs/` instead.
- **The planner only adds ordering constraints, never removes one.** When it cannot justify a direction, it asks a question instead of guessing.
- **Output contracts.** Changing `--json` or `--emit queue` output means updating the matching file in `docs/` in the same change.
- **Git.** One worktree per change, rebased on the freshly fetched `origin/main`, opened as a PR. Do not push straight to `main`.
