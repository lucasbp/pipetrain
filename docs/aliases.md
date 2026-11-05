# Profile and org aliases

Two kinds of short name, both only for typing. Neither changes what pipetrain reads from GitHub or what it prints.

## Profile aliases

A `[profile.NAME]` table may list `aliases`:

```toml
default_profile = "a"

[profile.acme]
aliases = ["a", "ac"]
orgs = ["acme"]

[profile.globex]
aliases = ["gx"]
orgs = ["globex"]
```

An alias is accepted wherever a profile name is: `--profile gx`, and `default_profile`. Lookup is ascii case-insensitive, the real name first, then the aliases. The profile that comes out is always the real one, so `doctor` says `profile globex (from --profile)` and the `profile` field of `plan --json` carries `globex`, never the alias.

Rules, checked when the config is read:

| Case | Result |
|---|---|
| `aliases` outside a `[profile.NAME]` table (top level, flat config) | Error: `aliases` is only allowed inside a `[profile.NAME]` table. |
| Not a list of non-empty strings | Error naming `profile.NAME.aliases`. |
| An alias equal to the profile's own name, or listed twice in the profile | Error naming the profile. |
| An alias equal to the name of another profile | Error naming both profiles. |
| An alias equal to an alias of another profile | Error naming both profiles. |
| `default_profile` that is neither a name nor an alias | Error: `default_profile` names `zz`, which is not a profile, followed by the list of profiles. |

An alias that collides is refused, never resolved by order, because the planner never guesses.

`pipetrain profiles` adds an `ALIASES` column, only when some profile has aliases (a profile without them shows `(none)`). `doctor`, the unknown profile error and the no profile chosen error list the aliases next to each profile as `aliases: a, ac`.

## Org aliases

A top-level `[org_alias]` table maps a short name to a real org:

```toml
[org_alias]
lab = "acme-labs"
```

The alias is replaced by the real org before anything else looks at it, in:

- `--org` of `plan`, `bundle`, `merge`, `bump`, `feed` and `doctor`;
- the PR arguments in the forms `org/repo#12` and `https://github.com/org/repo/pull/12`;
- the repo arguments of `bump` and `order suggest` that carry an org (`org/repo`);
- the profile rule that reads the orgs of the arguments: `lab/api#3` selects the profile that has `acme-labs`.

Lookup is ascii case-insensitive. A name that is not an alias goes through unchanged, so a real org is never touched.

Because the replacement happens first, the plan, the cache keys and every output (`--json`, `--emit queue`, the report of `merge` and `bundle`) hold the real org. `lab/api#3` shows up as `acme-labs/api#3`, and no alias is ever written to the cache or sent to GitHub. The `--json` and `--emit queue` contracts are the same.

Rules, checked when the config is read:

| Case | Result |
|---|---|
| Not a table of strings, or an empty name or value | Error naming `org_alias`. |
| Two aliases that differ only by case | Error: the alias is defined twice. |
| An alias that points to another alias, or to itself | Error: name the real org. |
| An alias equal to an org of any profile, or of a flat config | Error: an alias must not shadow a real org. |

`pipetrain profiles` prints an `org aliases: lab = acme-labs` line after the table when the table exists.

## Why

Profiles and orgs get long names, and the same short forms were retyped by hand in every command. Aliases remove the typing without a second source of truth: the real name stays the only name in plans, caches and logs, so a plan taken with an alias is identical to one taken with the full name. The collision rules exist so that an alias can never change which org or profile a name meant before the alias was added.

Code: `src/profile/alias.rs` (`OrgAliases`), `src/profile/parse.rs` (the parsing and the profile alias errors), `src/profile.rs` (`ProfileSet::find`), `src/command/resolve.rs` (where the aliases are applied). Tests: `tests/profile_alias.rs`, `tests/org_alias.rs`.
