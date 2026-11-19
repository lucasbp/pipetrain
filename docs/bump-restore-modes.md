# Bump restore modes, solution choice and prereleases

Why `bump --apply` behaves as it does for NuGet repos and for the default target. Usage is in the [README](../README.md#bumping-a-package).

## Why three restore modes

A locked restore proves the pin only when the repo commits `packages.lock.json`. Many repos do not, and skipping them silently left the bump undone. Each mode is an explicit trade:

- `locked` (default, the previous behavior): `dotnet restore --use-lock-file`, then `--locked-mode`. The lock files are the proof, so the commit includes them.
- `unlocked`: a plain restore plus a build. No lock file is expected or written. The build is the proof that the pinned version resolves and compiles; it runs with `--no-restore` after the restore.
- `skip`: only the manifest edit. Nothing proves the pin, and the PR body says so. Use it for repos where a restore or build cannot run in an isolated checkout.

The mode in effect is printed per repo in the dry run, in the apply output and as `restore_mode` in `--json`. `merge --bump` is unchanged and stays `locked`, because publication-backed preparation relies on the lock files.

Precedence: `--restore-mode`, then `[bump.repos."org/repo"]`, then `[bump]`, then `locked`.

## Why a solution choice

A repo with both a `.sln` and a `.slnx`, or several solutions, has no safe automatic pick: restoring the wrong one proves nothing about the others. With no choice the repo is skipped and the message lists the files found. `--solution <path>` or `solution` in the repo entry resolves it, and a path that is not in the repo skips the repo with a message.

## Why the stable default

The newest version in a feed can be a prerelease such as `1.2.0-develop`. Moving consumers to it by default is rarely wanted and cannot be undone by the dry run alone. The default target is the newest version without a `-` suffix (`+build` metadata is not a prerelease). `--prerelease` opts in, and the dry run says when a newer prerelease was ignored. `feed` itself is unchanged and still reports the latest version.

## Contracts

`--json` stays at schema 1 and only gains optional fields: `ignored_prerelease`, per-repo `restore_mode` and `solution`, and `restore_mode` in apply results.
