# GitHub API quota and cache

pipetrain spends GitHub API requests, and a run that exhausts them leaves a half-finished queue and blocks every other tool that shares the token. This page is the contract of the guard that prevents it and of the cache that makes repeated runs cheap.

## The reserve

`api.reserve` is the part of each rate-limit window that pipetrain never spends. It is a percentage of the `limit` GitHub reports, or an absolute number of requests, and it applies per resource (`core`, `graphql`, `search`).

```toml
[api]
reserve = "20%"
max_per_second = 10
```

| Key | Default | Behavior |
|---|---|---|
| `reserve` | `"20%"` | A string ending in `%`, or an integer. 20% of a 5000 window is 1000 requests. |
| `max_per_second` | 10 | Token bucket with a burst of the same size. It waits, it does not fail. Works together with `max_per_minute`. |

An absolute reserve is capped at the `limit` of its resource. During a run only `core`, `graphql` and any resource the run actually used (`search`) are compared with the reserve; other windows, such as `code_search`, are ignored, and `rate_remaining` is the lowest of the watched ones.

`min_remaining` is the older hard floor and stays: below it a request fails with an error. The reserve is the soft level above it, and it is the one that stops a merge cleanly.

## Three levels

1. **Start.** `plan` and `merge` read `GET /rate_limit` before anything else. That endpoint costs no quota. If the estimate of the command is larger than what is left above the reserve, the command refuses to start, changes nothing and exits with code **6**.
2. **During a merge.** After any response shows a resource below the reserve, no new merge starts. The check runs once the same-target settle wait is over and again just before the merge request is sent. The step ends `stopped`, the command exits **5** and the state file keeps the queue, so running the same command after the reset resumes it. A deploy that is already being followed is not abandoned: the poll interval grows (doubling, at most 15 minutes) and never sleeps past the reset time.
3. **Hard floor.** Below `min_remaining` any request is refused, as before.

The quota is never consumed to zero.

## The estimate

| Command | `core` | `graphql` |
|---|---|---|
| `plan` | 30 + 3 per PR named on the command line | 4 per org |
| `merge` | `api_estimate.total` of the merge report minus what the plan already spent | same as `plan` |

It is deliberately rough and on the high side; the real numbers are in the counters.

## The refusal

```
not enough GitHub API quota to start (api.reserve is 20%), nothing was changed:
  core: needs about 33, has 100 left with a reserve of 1000, missing 933
The quota resets at 2096-10-02 08:53 UTC (05:53 local), it resets in about ... Run the command again then, or lower api.reserve.
```

Exit code 6 is not used by anything else (0 ok, 1 error, 2 usage, 3 questions, 4 strict, 5 resumable stop, 10 update available). On `merge --apply` it is also the code when the merge was refused at start; the state file was not touched.

`PIPETRAIN_QUOTA_CHECK`: when the API base URL is a loopback address (tests and local fakes through `PIPETRAIN_API_URL`) the `/rate_limit` check is skipped, because fakes do not implement it. Setting this variable to any value turns the check on there too.

## Cheap polling

- Reads send `If-None-Match` and `If-Modified-Since` when the cache holds a validator. A `304 Not Modified` is refunded: it does not count against `max_requests` or the spent counters, and it is reported as `not_modified`. It still takes a slot in the `max_per_minute` and `max_per_second` pacing, because the request did reach GitHub and the pacing only bounds the rate of requests sent.
- While waiting for a deploy, once every expected workflow has a run, only runs that are not `completed` are read, one by id (`GET /repos/{slug}/actions/runs/{id}`) instead of listing the runs of the SHA again. Chained workflows and runs without an id keep the list read.
- `api.max_requests` counts every request of the run, polling included, so it bounds a long wait too. The poll interval is adaptive: the configured interval in the normal case, the backoff above when a resource is below the reserve.

## The persistent cache

Entries live in the user cache directory (`PIPETRAIN_CACHE_DIR` overrides it) under `http/`, next to the older file and tree cache, and are pruned with it (14 days, 32 MiB). It is shared by every command.

- **Key**: method, URL and body. For GraphQL the body is the query plus its variables, so the same query with other variables is another entry. A hash of the token is part of the key, so two accounts never share an entry.
- **Never stored**: the token and any `Authorization` header. Only status-200 bodies with no GraphQL `errors` and no cut-short search (fewer nodes than `issueCount` with no next page), the `ETag` and the `Last-Modified` are written.
- **Freshness**:

| Kind | Rule |
|---|---|
| PR, reviews, checks, PR lists and GraphQL reads while a plan is being built | Fresh for 60 seconds; after that they are revalidated with the validator. |
| Tree, blob and file by SHA (`/git/trees/<sha>`, `/git/blobs/<sha>`, `/contents/...?ref=<sha>`) | Immutable, never refetched. |
| Anything read after the plan is built (merge execution, deploy polling, bundle apply) | Never served by TTL: revalidated by `ETag`, or fetched. |

The last row is why a merge never acts on a minute-old PR state.

## Counters

Every `plan` and `merge` reports what it spent, split by resource: `core`, `graphql` and `search` requests, `cache_hits` (answered without the network) and `not_modified` (304). The text summary prints them on the `api` line and at the end of a merge (`API used: core 12, graphql 4, search 1; 9 from cache, 3 not modified`). In `--json` they are additive fields, schema stays `1`: [plan JSON](plan-json.md#stats) and [merge JSON](merge.md#json).
