# Releasing

A release is a tag. Pushing a tag `v*` runs `.github/workflows/release.yml`; nothing else publishes anything.

## Cut a release

1. Set `version` in `Cargo.toml`, update `Cargo.lock` (`cargo check`) and merge to `main`.
2. Tag the merge commit and push the tag. The tag must be `v` plus the `Cargo.toml` version; the workflow refuses a mismatch.

```
git tag v0.2.0
git push origin v0.2.0
```

A tag with a hyphen (`v0.2.0-rc.1`) is published as a pre-release. `pipetrain update --check` and the install scripts only follow the latest full release.

## What the workflow does

| Job | What |
|---|---|
| `check` | The tag matches `Cargo.toml`, then `cargo test --locked`. |
| `build` | One build per target, each archived as `pipetrain-vX.Y.Z-<target>.zip` (Windows) or `.tar.gz`, holding the executable, `LICENSE` and `README.md`. On the two Linux targets it also builds a `.deb` with `cargo-deb` and an `.rpm` with `cargo-generate-rpm`, from the same static musl binary. |
| `release` | Writes `SHA256SUMS`, attests build provenance for every file with `actions/attest-build-provenance`, and creates the GitHub Release with `gh release create` and the default `GITHUB_TOKEN`. No other secret is used. |

Targets: `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`. The aarch64 Linux build runs on GitHub's native arm runner, so it needs no cross toolchain.

Left out on purpose: Windows on ARM (no asset mapping yet), 32-bit targets, FreeBSD, and `.deb`/`.rpm` for macOS and Windows (not applicable). The `.deb` and `.rpm` use the musl binary, so they have no libc dependency.

Every action is pinned to a commit SHA. The comment next to each SHA names the major version it was taken from.

## The asset contract

The install scripts and the in-place installer depend on these names. Changing them breaks installed copies:

- `pipetrain-v<version>-<target>.zip` for Windows, `.tar.gz` for the rest.
- `SHA256SUMS` in the `sha256sum` format, one line per asset.
- The executable at the root of the archive or in one folder.

For now installs and upgrades go through `install.sh` and `install.ps1`. The in-place installer (`pipetrain update --experimental-install`) is not ready and may change.

## crates.io (manual, not part of the workflow)

Publishing the crate is a separate decision and no workflow does it. When you decide to, from a clean checkout of the tagged commit and with the token only in the environment:

```
cargo publish --dry-run --locked
cargo publish --locked
```

`CARGO_REGISTRY_TOKEN` must be set in your shell for the second command; do not write it to a file. A published version cannot be replaced, only yanked, so run the dry run first and publish only after the GitHub Release is out and the installers work.

## Checking a release by hand

```
gh release download v0.2.0 --repo lucasbp/pipetrain --pattern 'SHA256SUMS' --pattern 'pipetrain-*-x86_64-unknown-linux-musl.tar.gz'
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify pipetrain-v0.2.0-x86_64-unknown-linux-musl.tar.gz --repo lucasbp/pipetrain
```
