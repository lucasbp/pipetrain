# Repo names without the org

`plan` and `bundle` take PR references in three forms:

| Form | Example | Needs a lookup |
|---|---|---|
| Full reference | `acme/api#12` | No |
| PR URL | `https://github.com/acme/api/pull/12` | No |
| Bare name | `api#12` | Yes |

A reference that fits none of them, such as `api`, `#12`, `api#x` or `a/b/c#1`, is rejected before any request, with a message that names the reference and shows the expected `repo#123` form.

## How a bare name is resolved

1. The orgs to search are the `orgs` of the active profile. `--org` replaces them, as it does everywhere else. An org alias (see [aliases.md](aliases.md)) is replaced by its real org before the search, so the lookup and the plan use the real name.
2. For each distinct name, in the order of the orgs, pipetrain asks `GET /repos/{org}/{repo}`. A `404` means the org does not have that repo. Any other failure, including a bad token (`401`), a refusal (`403`) or a rate limit, stops the run as it does anywhere else; it is never read as "not found".
3. The result decides the outcome:

| Orgs that have the repo | Result |
|---|---|
| One | That org is used. The owner and the repo name come from GitHub's answer, so `api#1` finds `Api` and the plan says `acme/Api#1`. |
| More than one | `NameError::Ambiguous`: the run stops and lists the candidates, for example `acme/api, globex/api`. pipetrain never chooses. Write `org/repo#123`. |
| None | `NameError::NotFound`: the run stops, names the orgs it searched and says to write `org/repo#123` or pass `--org`. A token that cannot see private repos gives the same answer. |
| No orgs at all | `NameError::NoOrgs`: the run stops before any request and says to pass `--org`, set `orgs` in the config, or write the org. |

Names are compared without regard to case, so `api#1` and `API#2` are the same name and are looked up once.

## What it costs

One `GET /repos/{org}/{repo}` per org for each distinct name. The requests count against `max_requests` and `max_per_minute` like any other. With one org and three PRs in two repos, that is two requests.

The answer is not cached. The local cache only holds data that never changes for its key, and whether a repo exists in an org can change (created, renamed, transferred, made private), so a cached "not found" or "found" could be wrong.

## Profile choice

A bare name carries no org, so it takes no part in the profile rule that reads the orgs of the arguments. The profile is chosen by the other rules (flag, folder, default, only profile), and the name is searched in that profile's orgs. Mixing full and bare references is fine: the full ones can still pick the profile.

## What does not change

The feature only reads. The resolved references are full `org/repo#N` keys before discovery starts, so `--json` (schema 1), `--emit queue` and every other output are the same as if the org had been typed.

Code: `src/command/names.rs` (parsing, resolution and the errors), `src/model.rs` (`parse_bare`), `src/github/endpoints.rs` (`repo_in_org`). Tests: `tests/bare_names.rs`, against a local fake server.
