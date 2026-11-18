# Known bugs

Open defects found in real runs. Each entry says what happened, what should have happened and where it stands. Fixed entries move to the CHANGELOG and leave this file.

## 1. A passed workflow is not a finished CodeDeploy deployment

- **Found:** 2026-11-18, nuria-tech train, pipetrain 0.9.2.
- **What happened:** `nuria-agents-gateway#38` merged and its Deploy workflow ended as success at 21:06Z. `nuria-agents-gateway#51`, on the same target, merged next and its deploy failed at 21:21Z with "The Deployment Group ... already has an active Deployment". The blue/green deployment started by the first workflow was still active after the workflow ended.
- **Expected:** for a target that deploys through CodeDeploy blue/green, wait until the deployment group has no active deployment before merging the next PR on that target, or at least retry the deploy once the group is free instead of reporting a failed deploy.
- **Status:** open. Workaround: rerun the failed jobs of the deploy run once the deployment finishes, then resume with the same `--state`.

## 2. The queue does not warn that its own package releases will fail a version check

- **Found:** 2026-11-18, nuria-tech train, pipetrain 0.9.2.
- **What happened:** the queue published four `CMUtil` versions (`nuria-utils#35`, `#48`, `#52`, `#54`). `connect-client#65`, later in the same queue, references `CMUtil` 1.2611.1117.31 and its `check-nuget-version` check allows at most 1 version behind. The check failed with 3 behind and the PR left the queue as "no longer ready".
- **Expected:** the plan knows which packages the queue publishes and which PRs reference them. It should flag, before the run, every PR that will fall behind a package the queue releases, so the bump goes in before the run, not after a failure.
- **Status:** open. Workaround: bump the reference in the consumer PR after the publisher deploys.
