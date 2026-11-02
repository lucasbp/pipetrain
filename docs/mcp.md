# MCP server

`pipetrain mcp` serves five read-only tools over the Model Context Protocol, so an AI agent can ask for the merge order without parsing terminal output. It speaks JSON-RPC 2.0 on standard input and output, one message per line. There is no port, no daemon and no network listener: the client starts the process and closes stdin to stop it.

## Tools

At most five, each with a short description, because every tool weighs on the agent's context.

| Tool | Returns | Arguments |
|---|---|---|
| `plan` | The `plan --json` document, schema 1 (see [plan-json.md](plan-json.md)). | `prs[]`, `all`, `org[]`, `profile`, `date`, `check_feeds`, `mine` |
| `feed` | The `feed --json` document, schema 1. | `package` (required), `version`, `ecosystem`, `org`, `registry` |
| `bump` | The `bump --json` document, schema 1. Always a dry run. | `package` (required), `to`, `ecosystem`, `org`, `registry`, `repos[]`, `only[]`, `changes_only`, `profile` |
| `doctor` | The `doctor --json` document, schema 1. | `profile`, `org[]` |
| `order_suggest` | The `[[order]]` TOML block that `pipetrain order suggest` prints. | `repos[]`, `profile` |

`order_suggest` is the one tool that is not JSON, because `order suggest` has no `--json`. Its text is the same text the command prints on stdout.

Nothing that writes is exposed: no `bundle --apply`, no `bump --apply`, no `update`, no `init --write`. The tool list marks every tool `readOnlyHint: true`, and a test asserts that the list has exactly these five names.

Each tool runs the same code as its command (`plan_json`, `feed::render`, `bump::execute`, `doctor::json_report`, `order::execute`), so the JSON is the command's JSON and a change to a contract in one place changes both. Progress is always off.

## Results

A successful call returns `content[0]` with the document as text, and, when the command had something to say on stderr (a skipped repo, a request count), `content[1]` with those lines. The document stays pure: parse `content[0]` and ignore the rest.

A command that fails (not found, rate limit, no token, a refused feed) returns `isError: true` and the message, already free of the token. The server keeps running. A bad argument is also `isError: true` with `invalid arguments: ...`. An unknown tool name or a missing name is the JSON-RPC error `-32602`.

A tool call that gets a valid answer is not an error even when the command would exit non-zero: `doctor` with a failed check, `plan` with open questions and `feed` with a version that is not published all return their document and `isError: false`. Read the document for the verdict.

## Protocol

Hand-written, `initialize`, `notifications/initialized`, `ping`, `tools/list` and `tools/call`. The server answers with the client's `protocolVersion` when it is `2025-06-18`, `2025-03-26` or `2024-11-05`, and `2025-06-18` otherwise. Errors: `-32700` for text that is not JSON, `-32600` for a line that is not a request object, `-32601` for an unknown method, `-32602` for bad parameters. Blank lines are ignored, notifications get no answer, and batches are not supported (the 2025-06-18 revision removed them).

stdout carries protocol and nothing else. Warnings and logs go to stderr or into the tool result, never to stdout.

Why not the `rmcp` crate: the server needs five methods and one request loop. `rmcp` brings an async runtime and a large dependency tree into a CLI that is synchronous and ships a small static binary, and it would have to track a protocol that still moves. The hand-written loop is about 200 lines, is covered by tests that spawn the binary, and can be swapped for a crate later without touching the tools.

## Configuration, token and cache

The token comes from `GH_TOKEN` or `gh auth`, exactly as for the CLI, and is never part of any answer: the client redacts it from API errors, and the server also replaces the value of `GH_TOKEN` with `[redacted]` in every result as a second guard. The config is the usual one (`./pipetrain.toml`, then the user config dir); `pipetrain mcp --config PATH` pins one file for every call. The local cache is on, as in the CLI, and the request budget of the config applies to each call.

## Usage log

When the [local log](log.md) is on, each tool call is one line with the command `mcp:<tool>` (`mcp:plan`, `mcp:doctor`), with the same fields and the same exclusions as any other run. The server process itself is not logged.

## Test hook: `PIPETRAIN_API_URL`

When set to an `http://` URL whose host is `127.0.0.1`, `localhost` or `[::1]`, the GitHub client talks to that address instead of `https://api.github.com`. Any other value is ignored, so the variable can never redirect a token to a remote host. The tests use it to run the real binary against a fake API. It is not a supported way to use GitHub Enterprise.
