# How the admin override works on GitHub

`pipetrain merge --admin` lets a repository administrator merge a PR whose only blocker is a missing required review. This page records how GitHub implements that override, so the flag does not rest on assumptions. Flow and usage: [flows.md](flows.md); command contract: [merge.md](merge.md).

Everything marked *observed* was reproduced on 13 Nov 2026 against the public sandbox `lucasbp/pipetrain-sandbox-guarded`, a personal repository on GitHub Free whose `main` requires one approving review. The PR author was the repository owner, who cannot approve their own PR.

## There is no admin parameter

- `gh pr merge --admin` sends the same GraphQL `mergePullRequest` mutation as a normal merge. The flag only changes gh's client-side decision: it skips gh's own `BLOCKED`/`BEHIND` check and the merge queue path. Source: `pkg/cmd/pr/merge/merge.go` and `http.go` in `cli/cli`.
- The REST endpoint pipetrain uses, `PUT /repos/{owner}/{repo}/pulls/{number}/merge`, has no bypass field either. Its documentation does not mention administrators. Only the asynchronous merge endpoint documents a `bypass_rules` field; pipetrain does not use it.
- GitHub decides server-side from the identity of the token. *Observed*: as an admin, `PUT .../pulls/1/merge` with `sha` returned `200 {"merged": true}` while the PR was `BLOCKED` with `REVIEW_REQUIRED`.

Consequence: pipetrain's own preflight is the only thing that keeps a normal `merge --apply` from merging a review-blocked PR with an admin token. That is why the existing `mergeable_state == clean` check stays as it is without `--admin`, and why `--admin` is opt-in per run.

## What the PR looks like while blocked by review

*Observed* on the sandbox PR with no checks:

| Source | Field | Value |
|---|---|---|
| GraphQL | `mergeStateStatus` | `BLOCKED` |
| GraphQL | `reviewDecision` | `REVIEW_REQUIRED` |
| GraphQL | `mergeable` | `MERGEABLE` |
| GraphQL | `commits.statusCheckRollup` | `null` |
| GraphQL | `viewerCanMergeAsAdmin` | `true` (classic protection, admins not included) |
| REST | `mergeable_state` | `blocked` |
| REST | `mergeable` | `true` |

`BLOCKED` alone does not say why. It is also used for required checks not yet reported, required conversation resolution, required signatures and other rules. pipetrain therefore classifies the cause itself from the fields above and the base branch's rules, and refuses when anything other than the review is required and not proven satisfied.

## Classic branch protection

- By default the rules of a branch protection rule do not apply to people with admin permission on the repository. The setting "Do not allow bypassing the above settings" (`enforce_admins` in the REST API) removes that exemption. Source: [About protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches).
- *Observed* with `enforce_admins` false: admin merge succeeds, `viewerCanMergeAsAdmin` is `true`.
- *Observed* with `enforce_admins` true: the same request returns `405` with `At least 1 approving review is required by reviewers with write access.`, and `viewerCanMergeAsAdmin` is `false`.
- `bypass_pull_request_allowances` lets named users, teams or apps skip the pull request requirements. The REST documentation does not state whether it is available for repositories owned by a personal account (not confirmed). It is not needed for the owner's case and pipetrain does not read it; `viewerCanMergeAsAdmin` already reflects the outcome.
- Reading `GET /repos/{owner}/{repo}/branches/{branch}/protection` requires admin permission, which `--admin` verifies first. A `404` means the branch has no classic protection.

## Rulesets

- Rulesets have no default exemption for administrators: only actors on the ruleset's bypass list can bypass, and a repository role such as admin can be put on it. Rulesets and classic protection stack, and the most restrictive rule wins. Source: [About rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/about-rulesets).
- *Observed* with classic protection (admins not included) plus a ruleset requiring one approval and an empty bypass list: `405` with `Repository rule violations found ... At least 1 approving review is required`, and `viewerCanMergeAsAdmin` was `false`. The ruleset blocked even though classic protection alone would have allowed the admin.
- *Observed* after adding the repository admin role (`actor_type: RepositoryRole`, `actor_id: 5`) with `bypass_mode: always`: admin merge succeeded and `viewerCanMergeAsAdmin` was `true`.
- `GET /repos/{owner}/{repo}/rules/branches/{branch}` lists the rules in force for a branch (an empty array when none). It does not show the bypass list; `viewerCanMergeAsAdmin` is the signal for that.
- Bypass mode `pull_request` (bypass only through a pull request) was not tested (not confirmed).

## What pipetrain checks with `--admin`

1. The PR is blocked only by review: open, not draft, `reviewDecision == REVIEW_REQUIRED`, checks green (or none reported where missing checks are allowed), known mergeability, no conflict, not behind, `mergeStateStatus == BLOCKED`, and no other plan blocker.
2. `GET /repos/{owner}/{repo}` reports `permissions.admin == true` for the token's user, and GraphQL `viewerCanMergeAsAdmin` is `true` for the PR.
3. Classic protection and all branch rules are read. Every required check name from classic `required_status_checks.contexts[]`, classic `required_status_checks.checks[].context` and ruleset `parameters.required_status_checks[].context` must appear in the PR's `check_details` with `SUCCESS`, `NEUTRAL` or `SKIPPED` (case-insensitive). Missing or non-passing checks refuse the override and are named in the refusal, even if another check or the aggregate rollup is green. Duplicate reports of a required name must all pass. Missing checks are allowed only when the existing configuration allows them and no required names are missing.
4. When classic `required_status_checks.strict` or ruleset `strict_required_status_checks_policy` is true, `GET /repos/{owner}/{repo}/compare/{base}...{head_sha}` must report `behind_by == 0`. An unreadable comparison refuses the override.
5. Ruleset `pull_request.parameters.required_review_thread_resolution` refuses the override when true. If `allowed_merge_methods` is present, it must include the configured merge method in every applicable rule. Classic or ruleset `required_linear_history` refuses the `merge` method; `squash` and `rebase` remain eligible. Required signatures, classic required conversation resolution, a locked branch and unknown rules also refuse.
6. Planning and execution use the same verification function for permissions, fresh branch requirements, required check names, strict comparison, method restrictions and the pure review-only eligibility check. Execution does not reuse planning's permissions or protection. A marked step waits for unknown GraphQL mergeability using the existing 60 second deadline and pause, even while review is required. REST unknown or empty states also retain the 60 second settle loop. After REST settles, execution rereads the PR through GraphQL, requires its head SHA and base to match REST, and repeats the verification before merging. REST `mergeable_state` must be exactly `blocked`. A step without the marker cannot acquire an override during execution. If the PR became approved and `clean` in the meantime it is merged normally and no override is recorded.

Each successful override is recorded in the merge report, the pending record and the separate durable `admin_overrides` history with the reason, the login of the token's user and the time. History and pending metadata are saved atomically with the merge SHA and retained on resume. Failed, stopped or skipped execution steps without a merge SHA do not retain `admin_override` in the report, including after HTTP 405. A pending record can retain an untimestamped marker when the merge outcome is uncertain, so resume can confirm the outcome. No `AuditAction` variant is added.

## Known limits

- GitHub applies the bypass to the whole classic protection or applicable ruleset. pipetrain proves the supported non-review requirements before sending the merge request and refuses unknown rules or unreadable required-check lists. Required checks are verified by name and reported state; pipetrain does not prove the originating app's identity. A required name absent from the returned GraphQL check details is refused, including if it falls outside that snapshot's context limit.
- Required conversation resolution, ruleset `required_review_thread_resolution` and required signatures are refused even when they would be satisfied, because pipetrain does not read review threads or commit signatures. Such a PR needs separate verification and a manual merge outside pipetrain.
- GitHub may add new reasons for `BLOCKED`. The verification is a preflight, not an atomic server-side constraint: permissions, protection, checks or reviews can change after the last read and before GitHub processes the merge. The request pins the head SHA but cannot atomically pin those other inputs. Organization merge queues were not tested. The REST merge endpoint does not support merging with a merge queue.
