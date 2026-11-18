# Known bugs

Open defects found in real runs. Each entry says what happened, what should have happened and where it stands. Fixed entries move to the CHANGELOG and leave this file.

## 1. A failed deploy inherited from an earlier run blocks an unrelated queue

- **Found:** 2026-11-18, nuria-tech train, pipetrain 0.9.1.
- **What happened:** `merge --apply` with 18 PRs merged nothing. The state file carried `nuria-tech/nuria-auth#34 into master at e350a23`, merged on 2026-11-11, whose Deploy Lambda run had failed and whose base had no later commit. The run stopped on it ("repeating unchanged will not help") and skipped all 19 steps, including PRs in repos with no relation to `nuria-auth@master` (nuria-utils, nuria-hub, nuria-agents-gateway, connect-client and others). No PR in the queue targeted `nuria-auth@master`.
- **Expected:** a failed inherited deploy blocks only the PRs whose deploy target is the same repo and branch, plus the PRs that depend on them. Everything else runs, and the output says up front that the pending record is kept in the state and skipped.
- **Also wrong:** the dry run of the same queue listed the inherited pending record but finished as if the queue could run, so the dry run and `--apply` disagreed. The way out today is a manual `pipetrain state discard`, which should not be needed for an unrelated repo.
- **Status:** fix in progress on branch `fix/inherited-failed-deploy-no-block`.

## 2. A gate on the parent of a stack turns into an open question and refuses the whole queue

- **Found:** 2026-11-18, nuria-tech train, pipetrain 0.9.1.
- **What happened:** `nuria-auth#40` (base `master`, infra gate) is the base branch of `nuria-auth#41` and `nuria-auth#42` (base `feat/totp-hardening`). The plan reported "`nuria-auth#40` is a gate but must come after `nuria-auth#41` by rule `stacked`, so it cannot run first" as an open question, and `merge --apply` refused to start without `--allow-questions`.
- **Expected:** merge the stacked children into the parent branch first, then pause at the gate when the parent's turn comes. A gate means "pause and confirm", not "must be first in the whole queue".
- **Status:** open. Workaround: run the stack in separate `merge` calls (children first, then the parent and what follows it on the same target).
