# Common flows

This is the narrative index for planning and running a queue. Commands use public placeholders and PowerShell syntax. Apply examples describe owner workflows; agents must follow the sandbox-only execution rule in [AGENTS.md](../AGENTS.md). The [sandbox e2e guide](e2e-sandbox.md) explains real-write validation through the guarded harness.

## Building the Wednesday queue

### When to use

Use `plan` for the train identified by the run date and configured discovery rules. Wednesday is the owner's queue-building workflow, not a weekday restriction in the CLI. Use `--all` to consider ready open PRs regardless of branch, including on other days.

### Commands

```powershell
pipetrain plan
pipetrain plan --date 2026-11-11
pipetrain plan --all --emit queue
```

### What happens in each phase

Discovery selects the train, shared branches and PRs targeting the train; readiness checks exclude blocked PRs. The planner filters changes already present, builds ordering constraints and reports stages, gates, questions and bundle suggestions. `--all --emit queue` uses the broader selection and emits runner input rather than the human report. Neither command merges anything. See [planner rules](planner.md) and the [queue contract](queue-output.md) for selection, ordering and streams.

### What can go wrong and the right way out

Read `SKIPPED` reasons and resolve questions before executing the queue. Queue exit 3 means questions remain even though stdout contains queue lines. Use an explicit date for a different day's train; `--all` does not bypass readiness. For machine handling and empty selections, see [agent use](#use-by-agents).

### Tests

- `tests/train.rs`: `discovers_the_train_and_its_stacked_prs`, `leaves_out_noise_and_explains_the_one_skipped`.
- `tests/released.rs`: `approved_green_pr_on_any_branch_is_released`, `all_and_explicit_discovery_agree_on_every_review_state`.
- `tests/emit.rs`: `every_ready_pr_appears_once_and_none_skipped`, `order_is_the_flattened_stages_and_respects_every_edge`, `open_questions_give_exit_code_3_and_question_lines`.

## Merging with deploy waits

### When to use

Use `merge` to inspect execution, then `merge --apply` to execute the queue with live PR revalidation and deployment waits. It uses each PR's existing base; an intermediate train branch still requires a separate reviewed promotion PR for release.

### Commands

```powershell
pipetrain merge 'acme/api#12' 'acme/web#3'
pipetrain merge 'acme/api#12' 'acme/web#3' --apply
pipetrain merge 'acme/api#12' 'acme/web#3' --apply --yes
```

### What happens in each phase

The dry run builds the plan, previews local state and predicts workflows without polling deployments or writing merge state. Apply locks and reloads state and asks for `yes`; `--yes` skips this initial confirmation and is required without an interactive terminal. Execution resumes pending work, revalidates each PR, saves intent before the merge request, records the merge SHA and waits for the deploy verdict. Independent repositories can run concurrently up to `max_parallel`, with one active PR per repository, and deploys that need the same self-hosted runner or group run one at a time (see [deploy resources](merge.md#deploy-resources)); a deploy `queued` for a runner does not use its slot or its timeout. DDL and infra gates are all asked at the start, before any merge; a confirmed gate PR runs in parallel with the PRs that do not depend on it, and a refused one holds only its own branch. See [merge phases, confirmation and deploy tracking](merge.md).

### What can go wrong and the right way out

A changed head or base, lost approval, failing checks or non-clean REST mergeability prevents merging. Resolve the cause and rebuild the plan. Unknown mergeability can stop resumably after its bounded retry. Open questions block apply unless explicitly accepted with `--allow-questions`; this does not waive readiness. A timeout or missing run keeps pending state. Use [recovery](#failed-deploy-and-resumption) rather than repeating the merge manually. `--yes` does not confirm DDL, infra or package gates.

### Tests

- `tests/merge_run.rs`: `a_dry_run_lists_the_steps_in_queue_order_and_writes_nothing`, `declining_the_confirmation_merges_nothing`, `two_ready_prs_merge_one_at_a_time_and_each_deploy_ends_before_the_next_merge`, `parallel_real_workers_merge_independent_repositories_with_coordinated_state`, `a_pr_that_lost_its_approval_is_not_merged_and_the_rest_are_skipped`, `unknown_mergeability_is_retried_for_a_minute_and_then_stops_without_merging`.
- `tests/merge_run.rs`: `gates_are_asked_together_before_any_merge_and_a_refusal_holds_only_its_branch`, `a_batch_confirmation_of_all_gates_asks_once_and_lets_every_branch_go`, `without_a_terminal_every_gate_holds_its_own_branch_and_the_rest_goes_on`, `gates_confirmed_answers_every_gate_without_asking`, `a_dry_run_lists_the_gates_it_would_ask_and_asks_nothing`.
- `tests/merge_cli.rs`: `yes_without_apply_is_a_usage_error`.
- `tests/bundle_apply.rs`: `the_gate_follows_its_truth_table` covers the shared initial-confirmation gate.
- `tests/e2e_sandbox.rs`: `e2e_api_expected_deploy`, `e2e_parallel_web_and_static` (opt-in).

## Repositories or changes that do not deploy

### When to use

Use `merge.no_deploy_repos` when the entire repository should dispense the Actions deploy wait. For PR-specific exclusions, workflow `push` branch and path filters can provide the evidence without exempting the repository.

### Commands

With the repository configuration already selected:

```powershell
pipetrain merge 'acme/static#4' --config .\pipetrain.toml
pipetrain merge 'acme/static#4' --config .\pipetrain.toml --apply
pipetrain merge 'acme/api#12'
pipetrain merge 'acme/api#12' --apply
```

### What happens in each phase

The configured repository exemption takes precedence without reading workflows or changed paths. Otherwise the dry run predicts from the PR head; execution evaluates at the merge SHA using a fresh, complete changed-file list. A proved exclusion ends the merged step without polling. `expected`, `dispensed` and `indeterminate` describe different evidence. See [workflow filters and deploy verdicts](merge.md#what-it-waits-for) for the supported syntax and limits.

### What can go wrong and the right way out

Unreadable YAML, unsupported patterns or incomplete file evidence keep the wait. Missing runs alone do not justify adding a repository exemption. Inspect the prediction and workflow configuration; use an exemption only when the repository-level policy is intended. A dispensed wait does not prove production deployment.

### Tests

- `tests/merge_run.rs`: `a_repo_listed_in_no_deploy_repos_is_not_waited_for`, `a_train_branch_is_not_waited_for_when_no_workflow_runs_on_a_push_to_it`, `main_paths_miss_dispenses_without_polling`, `main_paths_match_waits_for_deploy_and_passes`, `incomplete_pr_files_remain_indeterminate_and_wait`.
- `tests/incidents.rs`: `incident_1_main_outside_paths_is_dispensed_without_grace_wait`.
- `tests/e2e_sandbox.rs`: `e2e_api_outside_deploy_paths`, `e2e_static_no_deploy` (opt-in).

## Failed deploy and resumption

### When to use

Use saved state after a failed deployment, timeout or interruption. Inspect it before deciding whether to resume or deliberately stop tracking a pending wait.

### Commands

Inspect and resume using the same state selection:

```powershell
pipetrain state show --state .\queue-state.json
pipetrain state show --state .\queue-state.json --json
pipetrain merge --all --state .\queue-state.json --apply
```

Only when intentionally abandoning tracking of that record:

```powershell
pipetrain state discard 'acme/api#12' --state .\queue-state.json --reason 'Tracking intentionally closed after inspection'
pipetrain state discard 'acme/api#12' --state .\queue-state.json --reason 'Tracking intentionally closed after inspection' --yes
```

The second discard command is the non-interactive alternative, not a second discard of the same record.

### What happens in each phase

`state show` reads local metadata and pending work without credentials, a lock or GitHub access. Apply rebuilds the plan and resumes inherited records before new merges, including records outside the current selection. A missing saved merge SHA is reconciled with GitHub. A failed saved deploy can pass after its rerun, or be resolved by a later successful descendant deployment on the same base. The latter retains original failure evidence and moves the pending record to audit with the later SHA and run URLs. Discard instead records an operator reason and removes tracking without confirming deployment. See [state inspection, discard and later-deploy resolution](state.md) and [merge resumption](merge.md#resuming).

### What can go wrong and the right way out

A later unrelated commit, incomplete evidence, unresolved workflow chain or all-skipped later deploy cannot resolve the failure. Inspect the reported cause; resume once a qualifying deploy succeeds. A stale age is a warning and does not remove work. Lock contention means another execution holds the state; the remaining `.lock` file alone does not mean a lock is held. Unsupported or invalid state is refused. Keep the file for inspection rather than deleting pending records by hand. Failure continuation and publisher policies are specified in [merge.md](merge.md#deployment-failure-policy).

### Tests

- `tests/merge_run.rs`: `a_deploy_that_never_ends_stops_and_the_next_run_resumes_without_merging_again`, `a_failed_deploy_can_resume_after_a_successful_rerun_without_another_merge`, `later_descendant_deploy_resolves_with_audit_and_unblocks_same_target`, `unrelated_later_commit_does_not_resolve_or_poll_its_green_deploy`, `later_deploy_needs_complete_expected_success_and_run_evidence`.
- `tests/state_cli.rs`: `state_show_json_exposes_documented_fields_without_mutating_or_locking`, `state_discard_preserves_metadata_and_appends_audit_exactly_once`, `state_discard_obeys_merge_lock_but_show_can_read_it`.
- `tests/incidents.rs`: `incident_3_failed_deploy_is_resolved_by_later_descendant_deploy`, `incident_6_old_pending_shows_age_and_stale_and_lock_excludes_second_process`.
- `tests/e2e_sandbox.rs`: `e2e_web_later_deploy_recovery` (opt-in).

## Stacked PRs

### When to use

Use this flow when a selected child targets the head branch of a selected parent. Include the chain so the planner can put children before parents and execution can revalidate the parent after the child lands.

### Commands

```powershell
pipetrain plan 'acme/api#12' 'acme/api#11' 'acme/api#10'
pipetrain merge 'acme/api#12' 'acme/api#11' 'acme/api#10'
pipetrain merge 'acme/api#12' 'acme/api#11' 'acme/api#10' --apply
```

### What happens in each phase

Plan notes and merge previews show the selected chain. Children merge into their existing bases first. The executor records child merge evidence in this invocation and can accept the resulting parent head only after ancestry, exact head shape, fresh readiness and REST checks. Sequential and parallel execution share this narrow exception to changed-head refusal. See [the stacked-PR checks](merge.md#stacked-prs) for the exact proof.

### What can go wrong and the right way out

An unrelated push still fails the head guard. A parent that loses approval or awaits checks stops before its merge; complete the required review or checks and rerun. Already merged children stay merged. A parent that is an open PR of the same repository is added to the selection by itself (origin `stack-parent`), and so are its own parents, up to the PR that lands on the default or a train branch; a cycle of bases stops at the first repeated PR. The order is unchanged: children first, parent revalidated after. A parent that is not ready (draft, no approval, checks, behind) goes to `SKIPPED` with its own cause and its children go to `SKIPPED` with `stack parent <org/repo#n> not ready`. A base with no open PR, or with several, keeps the feature-base preflight warning; select the parent or verify that destination deliberately. Evidence from this invocation does not authorize arbitrary head changes after restarting.

### Tests

- `tests/merge_run.rs`: `three_pr_stack_shows_full_chain_in_plan_and_preview_and_merges_in_order`, `stacked_parent_lost_approval_stops_with_reapprove_next_step`, `stacked_parent_pending_checks_stop_with_cause_and_next_step`, `stacked_parent_unrelated_push_is_refused`, `stacked_orphan_feature_base_keeps_preflight_warning_in_plan_and_preview`.
- `tests/incidents.rs`: `incident_5_stacked_child_then_parent_merge_with_verified_new_head`.

## NuGet gate and package waits

### When to use

Use the package wait when a consumer bumps a package published by another PR in the queue. A dependency ordering edge alone does not add `# wait: package`. Use `--check-feeds` to annotate the plan, `feed --wait` for an independent runner wait, and `--gates-confirmed` only when intentionally confirming execution gates. Publication-backed consumer preparation is a separate `merge --bump` opt-in.

### Commands

```powershell
pipetrain plan --all --emit queue --check-feeds
pipetrain feed Acme.Util --org acme --version 2.0.0 --wait --timeout 900 --no-progress
pipetrain merge --all
pipetrain merge --all --apply --yes --gates-confirmed
pipetrain merge --all --bump
pipetrain merge --all --bump --apply
```

### What happens in each phase

The plan identifies publishers and the consumer's requested version, then emits a package wait before that consumer. Feed annotations are informational. Execution rechecks a known version after the publisher finishes using the shared bounded feed loop; published evidence releases the consumer without a question. A missing version keeps manual confirmation. `--yes` answers only initial apply confirmation; `--gates-confirmed` answers gates, including the manual fallback after an absent-version timeout. DDL and infra pauses also retain explicit gate handling. See [queue waits](queue-output.md#wait-comments) and [package waits during merge](feed-wait.md#package-waits-during-merge).

With `--bump`, publication proof must establish an exact newly pushed NuGet version from the recorded execution and its presence in the feed. A planned package edge prepares a draft targeting the consumer head. If the release crosses the consumer's `max-behind` limit, affected PRs sharing a base wait for a shared draft into that base. Keep the command running while a reviewer integrates the draft. It updates branches where needed, rechecks the pins and continues after approval and successful CI on the updated heads. After interruption or timeout, repeat the command with the same state file. See [publication-backed preparation](publication-bump.md).

### What can go wrong and the right way out

A feed 401/403 stops even with `--gates-confirmed`; fix authentication and package access. An absent version at timeout can require manual confirmation, while a failed final lookup stops with its cause. Standalone wait exit codes live in [feed-wait.md](feed-wait.md#exit-codes). Missing logs, ambiguous publication or unsupported pins require inspection rather than inventing a version. Partial bump operations are saved for inspection and are not blindly repeated; inspect any reserved branch or draft PR before retrying.

### Tests

- Queue and annotations: `tests/emit_wait.rs`, `a_package_edge_puts_a_wait_right_before_the_consumer_merge`, `a_dependency_edge_does_not_put_a_wait`, `the_wait_names_the_version_the_consumer_bumps_to`; `tests/feed_check.rs`, `a_nuget_version_is_checked_through_the_service_index`, `without_the_check_nothing_is_added_to_the_queue_or_the_json`.
- Execution gates and waits: `tests/merge_run.rs`, `a_ddl_pr_adds_a_gate_that_stops_the_queue_until_it_is_confirmed`, `gates_confirmed_passes_every_gate_without_asking`, `package_wait_uses_nuget_service_index_and_records_feed_evidence`, `package_wait_timeout_keeps_manual_confirmation_and_gates_confirmed`, `package_wait_refused_feed_stops_with_cause_even_with_gates_confirmed`, `package_wait_without_version_keeps_manual_gate_without_reading_feed`.
- Standalone feed wait: `tests/feed_wait.rs`, `waits_until_the_version_appears`, `times_out_with_exit_one_and_the_latest_version`, `unauthorized_aborts_without_retrying`, `a_persistent_server_error_exits_three_with_empty_stdout`.
- Consumer preparation: `tests/merge_bump.rs`, `new_publication_saves_inputs_and_pre_effect_marker_then_reuses_the_review_pr`, `partial_remote_operation_is_durable_attention_and_never_retried_blindly`; `tests/merge_run.rs`, `bump_partial_preparation_blocks_its_consumer_but_continues_independent_work`.

## Bundle, feed and bump

### `bundle`

#### When to use

Use a bundle suggestion when multiple ready PRs for one repository and base can be collected into one deploy branch. Whether they can safely ship together remains an owner decision.

#### Commands

```powershell
pipetrain bundle --all
pipetrain bundle --all --apply
pipetrain merge --all --apply --open-pr
```

#### What happens in each phase

The dry run previews branch creation and retargeting. Apply confirms, creates or validates the deploy branch and retargets the selected PRs. The final command merges them and attempts to open the promotion PR after the bundle's merges and waits. `bundle --apply --open-pr` can instead open it immediately after retargeting. Neither opening mode merges the promotion PR. See [saving deploys](saving-pipelines.md), the [bundle reference](../README.md#applying-the-bundle-suggestion) and [opening the deploy PR](merge.md#opening-the-deploy-pr).

#### What can go wrong and the right way out

A pre-existing branch must contain the current base tip. Inspect a refused branch before choosing another date or removing it yourself. A failed write stops subsequent actions; inspect the result and rerun to reuse completed branch creation or retargets. PRs still targeting a train branch can defer opening its promotion PR. Resolve those PRs and rerun with `--open-pr`.

#### Tests

- `tests/bundle_apply.rs`: `a_dry_run_only_reads_the_base_ref`, `apply_creates_the_branch_then_retargets_each_pr_in_order`, `a_branch_from_another_base_is_refused_on_a_dry_run_and_on_apply`, `a_422_on_a_retarget_stops_the_rest`, `open_pr_is_the_last_write_of_the_bundle_and_never_merges`.
- `tests/merge_run.rs`: `open_pr_adds_one_step_after_the_last_merge_of_the_bundle`, `a_run_cut_off_before_open_pr_opens_it_once_on_the_next_run`, `a_recorded_bundle_with_a_pr_still_on_the_train_branch_does_not_open`.

### `feed`

#### When to use

Use `feed` for a read-only NuGet, npm, Cargo, PyPI or Go publication lookup outside queue execution. Use `--wait` when the next runner step needs a particular version.

#### Commands

```powershell
pipetrain feed Acme.Util --org acme --version 2.0.0
pipetrain feed Acme.Util --org acme --version 2.0.0 --json
pipetrain feed Acme.Util --org acme --version 2.0.0 --wait
```

#### What happens in each phase

The command resolves the ecosystem and feed, reads versions and reports whether the requested version exists. Without a version it reports available versions; `--wait` requires a version and repeats the same lookup. Feed selection and lookup output are in the [feed reference](../README.md#checking-a-package-feed); timing, streams and wait results are in [feed-wait.md](feed-wait.md).

#### What can go wrong and the right way out

A NuGet lookup needs an org unless an explicit registry is supplied. Correct a refused token's package permissions. A timeout with an absent version differs from a final failed attempt; branch on the documented wait exit code and inspect stderr before retrying.

#### Tests

- `tests/feed_cli.rs`: `nuget_version_is_found_through_the_service_index`, `nuget_requires_an_org_without_a_registry`, `json_output_has_the_documented_shape`.
- `tests/feed_wait.rs`: `wait_requires_a_version`, `waits_until_the_version_appears`, `transient_errors_are_retried_until_published`.

### `bump`

#### When to use

Use standalone `bump` to inspect which manifests on repository default branches would change for a package version. It is always a dry run; writable consumer preparation belongs to `merge --bump` above.

#### Commands

```powershell
pipetrain bump Acme.Util --org acme --to 2.0.0 --repo acme/api
pipetrain bump Acme.Util --org acme --to 2.0.0 --repo acme/api --check --json
```

#### What happens in each phase

It takes the target directly from `--to`, or looks up the latest stable version in the feed when `--to` is absent (`--prerelease` opts into a newer prerelease; the dry run states when one was ignored). For NuGet it also shows, per repo, the restore mode `--apply` would use (`locked`, `unlocked` or `skip`, from `--restore-mode`, `[bump.repos]` or `[bump]`) and the chosen solution. The reasons for the modes are in [bump restore modes](bump-restore-modes.md). It scans the selected repositories' manifests and classifies current pins against that target. It prints proposed changes without editing, committing, pushing or opening PRs. `--check` supplies a nonzero result when manifests would change. `--apply` opts into one draft PR per repo after the version is proven in the feed. Selection, filtering and error codes live in the [standalone bump reference](../README.md#bumping-a-package).

#### What can go wrong and the right way out

There is no standalone `--apply`. Supply repositories explicitly or configure exact repository names for the scan. A failed latest-version lookup, no readable repository or no reference to the package is an error. An explicit `--to` is not feed validation. Inspect stderr because `--check` also uses exit 1 for proposed changes.

#### Tests

- `tests/bump_scan.rs`: `a_dry_run_reports_each_manifest_and_only_reads`, `an_unknown_repo_is_skipped_with_a_warning_and_the_rest_is_read`, `when_no_repo_can_be_read_the_scan_fails`.
- `tests/bump_cli.rs`: `apply_does_not_exist`, `a_package_missing_from_the_feed_exits_1`, `without_repos_or_profile_names_it_stops_before_the_feed`, `without_a_stable_version_the_default_target_stops_and_names_the_way_out`, `the_new_flags_are_accepted_by_the_parser`.
- `tests/bump_apply.rs`: `unlocked_restores_plainly_builds_and_opens_a_repo_without_lock_files`, `skip_pins_and_opens_without_restoring_or_building`, `a_chosen_solution_resolves_a_repo_with_two_of_them`, `two_solutions_without_a_choice_name_the_files_in_the_skip_reason`.
- `tests/bump_config.rs`: `a_repo_entry_beats_the_global_mode_and_the_global_mode_beats_the_default`, `the_command_line_beats_the_config_and_the_repo_key_ignores_case`.

## Use by agents

### When to use

Use versioned JSON for structured decisions, `--strict` when a selection without an eligible PR should fail, and MCP when the agent needs read-only tools over stdin/stdout.

### Commands

```powershell
pipetrain plan --all --json --strict
$planExit = $LASTEXITCODE
pipetrain plan --all --emit queue --strict
$queueExit = $LASTEXITCODE
pipetrain merge --all --json --strict
$mergeExit = $LASTEXITCODE
pipetrain mcp --config .\pipetrain.toml
```

The MCP process expects a client to send protocol messages on stdin; it is not a one-shot plan command.

### What happens in each phase

Parse the [plan JSON contract](plan-json.md), including preflight eligibility, causes and next steps. A normal text/JSON plan exits 0 even with questions; queue mode exits 3 for open questions. Strict plan exits 4 if no visible selected PR is eligible, including empty or waiting-only selections, and takes precedence over queue exit 3. Reports are still printed. Queue streams and line semantics are specified in [queue-output.md](queue-output.md).

For merge, use its separate [JSON and exit contract](merge.md#exit-codes): 0 for a dry run or completed execution, 1 for failure or declined confirmation, 3 for an apply blocked by questions, and 5 for a resumable stop. Strict merge adds exit 4 for a fully ineligible selection or a dry run without a merge step; existing errors, open-question refusal and resumable outcomes retain their codes. General operational errors exit 1 and command-line syntax errors normally exit 2; feed has its own codes.

MCP exposes five read-only tools using the command implementations. A valid returned document can have `isError: false` even when the corresponding CLI would exit nonzero, so inspect its contents. Tool list, protocol, result envelopes and token handling stay in [mcp.md](mcp.md).

### What can go wrong and the right way out

Do not combine `--json` with `--emit queue`. Check exit status as well as parsing the report; a printed queue with open questions is not automatic authorization to run it. An MCP command failure is a tool error and the server remains available. Execution commands are not MCP tools. For real-write validation use the [sandbox harness](e2e-sandbox.md).

### Tests

- `tests/train.rs`: `json_output_keeps_the_documented_shape`.
- `tests/preflight.rs`: `strict_returns_four_for_fully_blocked_plan_and_merge_dry_run_only_when_requested`, `strict_distinguishes_empty_waiting_and_mixed_selections`, `strict_apply_with_no_eligible_pr_preserves_default_exit_and_does_not_merge`, `strict_merge_dry_run_with_only_inherited_resume_reports_four_without_writes`.
- `tests/emit.rs`: `open_questions_give_exit_code_3_and_question_lines`.
- `tests/mcp_cli.rs`: `initialize_and_tools_list_expose_five_read_only_tools`, `plan_returns_the_schema_1_json_for_an_explicit_pr`, `a_command_failure_is_a_tool_error_and_the_server_keeps_going`, `the_token_never_appears_in_any_answer`.

## Merging past a missing review (`merge --admin`)

To be filled by the pull request that adds `--admin`.

## Flows without tests

- Merging past a missing review (`merge --admin`): No test.

All implemented flows and sub-flows above cite existing tests. Sandbox tests are opt-in; their presence does not imply they ran during a documentation change.
