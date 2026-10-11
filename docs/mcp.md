# Using rsRewind from an AI assistant (MCP)

`rsrewind mcp` lets an AI assistant search and read your screen history: "what was the error before lunch?",
"find the price I saw on that page yesterday". It speaks the [Model Context Protocol](https://modelcontextprotocol.io)
over stdio: your assistant starts it as a program. rsRewind opens no network port.

## Before you turn it on

**Assistants send what they read to their model, which usually runs on the provider's servers.** Turning this on is
a way selected rsRewind history can leave your computer through that assistant. It is off until you run:

```sh
rsrewind mcp --enable      # prints what is shared; rsrewind mcp --disable turns it off
```

Agents can then read recognized text and window details from the **last 30 days**. They **cannot** see screenshots
unless you allow it, and they **cannot** resume recording, delete or move history. They can pause recording.

## Connect your assistant

Use the full path to `rsrewind` if it is not on your PATH (Windows MSI: `%LOCALAPPDATA%\Programs\rsRewind\rsrewind.exe`;
Linux tarball: wherever you installed it, for example `~/.local/bin/rsrewind`).

| Client | Setup |
|---|---|
| Claude Code | `claude mcp add rsrewind -- rsrewind mcp` |
| Claude Desktop | `claude_desktop_config.json`: `{ "mcpServers": { "rsrewind": { "command": "rsrewind", "args": ["mcp"] } } }` |
| Cursor | `.cursor/mcp.json` (or global): the same `mcpServers` entry |
| Codex CLI | `~/.codex/config.toml`: `[mcp_servers.rsrewind]` with `command = "rsrewind"` and `args = ["mcp"]` |

Settings are re-read before every request. Disabling access, narrowing the time or machine scope, or revoking screenshots
takes effect on an existing connection's next call; invalid or unreadable configuration also turns access off. Restart
the assistant's connection after changing settings so its cached tool catalogue refreshes.

## What agents may access (`config.toml`, `[mcp]`)

```toml
[mcp]
enabled = true            # rsrewind mcp --enable / --disable
allow_screenshots = false # true lets get_screenshot return pictures
max_age_days = 30         # nothing older is returned; 0 = no limit
sources = []              # [] = every machine; or ["this", "<source id from rsrewind sources>"]
```

These limits apply both to what tools are offered and to every call: a hidden tool called directly is refused.

## Tools

| Tool | What it does |
|---|---|
| `search` | Find moments by on-screen text, with app, window title and time filters |
| `recent` | Latest moments, or those before a time |
| `moment_at` | What was on screen at a time |
| `get_moment` | One moment's time, machine, app, window and full recognized text (optionally with line positions) |
| `get_screenshot` | The picture of a moment (only with `allow_screenshots = true`) |
| `list_gaps` | When nothing was recorded, and why |
| `list_sources` | The machines in your history |
| `get_status` | Recorder state and what agents may access (always available, even when access is off) |
| `pause_recording` | Pause for some minutes or until you resume |

`pause_recording` only extends an existing pause. It cannot shorten an indefinite pause or resume recording. Repeating
a timed pause may extend its deadline, so the tool is not advertised as idempotent.

Times can be RFC 3339, `YYYY-MM-DD HH:MM`, `YYYY-MM-DD`, or a duration ago such as `30m`, `2h`, `7d`.
Local times use the server computer's timezone. Use RFC 3339 with an explicit offset to avoid ambiguous times.
Search returns at most 50 moments and recent at most 100. Gap requests span at most 366 days; gap and machine lists
return at most 500 rows with a `truncated` marker. Moment text is limited to 20,000 characters, screenshots to 4 MiB;
oversized text results are refused with a request to narrow the query. Screenshots return WebP image content; other
successful tools also return JSON text and `structuredContent` matching their advertised `outputSchema`.

## What is logged

Calls log the static tool name, channel `mcp`, local process-owner boundary, process ID, server-generated call number,
executed/refused decision, result count and duration. Client request IDs and arguments are not logged because they may
contain screen content. The process ID and call number correlate calls without copying client strings into logs.
Recognized text, window titles and pictures are never logged. These are operational logs, not a durable compliance
ledger or a separately authenticated application identity: the server runs as the OS user who launched it and inherits
that user's filesystem permissions. Do not configure an untrusted assistant to launch it.

The supported protocol versions are `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`. Clients must send
`initialize`, then `notifications/initialized`, before using tools. The server exits when standard input closes.
JSON-RPC batches are accepted only when `2025-03-26` was negotiated. No remote HTTP endpoint, OAuth flow, resources,
prompts, subscriptions or background tasks are advertised.
