# MCP server: your screen history as a first-class agent surface

Status: **design + first implementation**, 2026-10-10, against v0.0.6. Held to the same bar as AnchorDesk's
MCP server (`Spillers-Technology/AnchorDesk`, `docs/mcp-auth.md`, `backend/src/routes/mcp.ts`) and to the company's
agent-surface standard, STD-005 (`corporate-strategy/standards/STD-005-agent-surface.md`). Decision record:
`corporate-strategy/board/proposals/rsrewind-mcp-server.md`.

## What it is for

"What was the error I saw before lunch?", "find the price on that product page yesterday", "what was I working on when
the build broke?": an AI assistant that can search and read your screen history answers those directly. `rsrewind mcp`
gives Claude Code, Claude Desktop, Cursor, Codex or any MCP client that ability, through the same read-only query layer
the window uses.

## The bar (from AnchorDesk, adapted)

| AnchorDesk invariant | rsRewind |
|---|---|
| **Parity is a release invariant**: every web/REST workflow ships with an MCP tool in the same change | Every read workflow of the CLI and the window has a tool (table below), and a test fails if a CLI read command has no tool |
| **Protocol-level contract test** with a real client over a real transport | Tests speak JSON-RPC 2.0 to the actual `rsrewind mcp` server over stdio: `initialize`, `tools/list` (the exact advertised contract), `tools/call` on a synthetic history |
| Tools run **as the user**, same validation, channel-tagged audit `alice (mcp)` | The server is the user's own process. Each call is logged with tool, request id, result count and duration: **never** recognized text, titles or pixels (CLAUDE.md logging rule) |
| Role-gated tools | A **ceiling** in `config.toml` `[mcp]` (below), applied to `tools/list` **and** re-checked when a tool is called (STD-005: discovery filtering alone is not a boundary) |
| Tool annotations (`readOnlyHint`, `destructiveHint`, ...) | The same, on every tool |
| Server version = package version | `serverInfo.version` = the workspace version |
| Docs: coverage table + client setup | `docs/mcp.md` (user guide) + this design |

## Transport and identity

- **stdio only.** The client starts `rsrewind mcp` as a child process and speaks newline-delimited JSON-RPC on its
  stdin/stdout. rsRewind opens no socket and runs no server ("No listening socket, no local HTTP server" stays true),
  there are no credentials to issue or leak, and only programs the user configured can reach it.
- **Principal** = the OS user who launched it, the same user who owns the history. **Channel** = `mcp`.
- **Remote access** (an HTTP/SSE endpoint with tokens and OAuth, like AnchorDesk's) would need a listening socket and so
  an amendment of the "No" list; it is a separate motion, not part of this work.

## Privacy: the one rsRewind-specific risk

An MCP client sends tool results to its model. For most assistants that model runs in the provider's cloud, so **using
these tools sends parts of your screen history off this computer**, which is the one thing rsRewind otherwise never
does. Therefore:

1. **Off by default.** `rsrewind mcp` answers `initialize` with a single `status` tool that explains how to enable it,
   until `[mcp] enabled = true`. Enabling it is a deliberate act (`rsrewind mcp --enable`, which prints exactly what
   will be shared and where).
2. **Screenshots are separately gated** (`allow_screenshots`, default `false`): text first, pixels only if asked for.
3. **A time window** (`max_age_days`, default 30) and **a machine allowlist** (`sources`, default: all) cap what any tool
   can return.
4. Nothing the privacy rules excluded exists to be returned: exclusions apply at capture time, before storage.
5. Every response that carries screen content says where it came from (machine, time), so an agent can cite it.

## Consequence classes (STD-005)

| Tool | What it does | Class | Default |
|---|---|---|---|
| `search` | Full-text search of recognized text, with app/title/time filters, across machines | C0 read | on |
| `recent` | The latest moments, newest first, paged | C0 | on |
| `moment_at` | What was on screen at a given time | C0 | on |
| `get_moment` | One moment: time, machine, app, window, recognized text and its line boxes | C0 | on |
| `get_screenshot` | The picture of one moment (WebP, optionally downscaled) | C0, sensitive | **off** until `allow_screenshots` |
| `list_gaps` | When nothing was recorded, and why | C0 | on |
| `list_sources` | The machines in this history and their spans | C0 | on |
| `get_status` | Recorder state, OCR backlog, privacy-enforcement note | C0 | on |
| `pause_recording` | Pause for N minutes or until resumed (via `rsrewind pause`) | C1, protective | on |
| resume / forget / start / stop / export / import | — | C2/C3 | **not exposed**: turning recording back on, deleting history and moving it are the human's decisions |

## Parity map

| CLI / window | MCP |
|---|---|
| `search`, the search box | `search` |
| `recent`, the room | `recent` |
| scrubbing to a time | `moment_at` |
| the detail pane | `get_moment` |
| the stage / viewer | `get_screenshot` |
| `gaps`, gap bands | `list_gaps` |
| `sources`, the sidebar | `list_sources` |
| `status`, the chip | `get_status` |
| tray "Pause" | `pause_recording` |

## Implementation

- New crate `rsrewind-mcp`: depends on `rsrewind-core` and `rsrewind-query` only (boundary-tested like the UI). It reads
  through the `History` facade on a read-only connection; its one action (`pause`) runs `rsrewind pause` as a child
  process, like the tray.
- **No async runtime.** The official Rust SDK (`rmcp`) requires tokio; rsRewind's design keeps to plain threads
  (ARCHITECTURE "Why no async runtime"). The server is a small synchronous JSON-RPC 2.0 loop over stdin/stdout
  (`initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`), with protocol-version negotiation.
- Results carry both a short text summary and `structuredContent` (with `outputSchema`) so agents get typed data.
- Bounded everywhere: page sizes capped, screenshots capped in bytes, text truncated with a marker.

## Tests (required before merge)

- Protocol: initialize (version negotiation, server info = workspace version), `tools/list` contract (names, annotations,
  schemas), every tool called through JSON-RPC on a synthetic history.
- Ceiling: disabled server exposes only `get_status`; `allow_screenshots = false` hides **and** refuses `get_screenshot`;
  `max_age_days` and `sources` are enforced on every tool, including direct calls of hidden tools.
- Parity: a list of CLI read subcommands must each map to a tool.
- Logging: a call's log line contains no recognized text.
