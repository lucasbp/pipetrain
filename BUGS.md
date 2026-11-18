# Known bugs

Open defects found in real runs. Each entry says what happened, what should have happened and where it stands. Fixed entries move to the CHANGELOG and leave this file.

## 1. A gate on the parent of a stack turns into an open question and refuses the whole queue

- **Found:** 2026-11-18, nuria-tech train, pipetrain 0.9.1.
- **What happened:** `nuria-auth#40` (base `master`, infra gate) is the base branch of `nuria-auth#41` and `nuria-auth#42` (base `feat/totp-hardening`). The plan reported "`nuria-auth#40` is a gate but must come after `nuria-auth#41` by rule `stacked`, so it cannot run first" as an open question, and `merge --apply` refused to start without `--allow-questions`.
- **Expected:** merge the stacked children into the parent branch first, then pause at the gate when the parent's turn comes. A gate means "pause and confirm", not "must be first in the whole queue".
- **Status:** open. Workaround: run the stack in separate `merge` calls (children first, then the parent and what follows it on the same target).
