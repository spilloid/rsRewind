# Using rsRewind from an AI assistant (MCP)

`rsrewind mcp` lets an AI assistant search and read your screen history: "what was the error before lunch?",
"find the price I saw on that page yesterday". It speaks the [Model Context Protocol](https://modelcontextprotocol.io)
over stdio: your assistant starts it as a program. rsRewind opens no network port.

## Before you turn it on

**Assistants send what they read to their model, which usually runs on the provider's servers.** Turning this on is
the one way rsRewind history can leave your computer. It is off until you run:

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

After changing settings below, restart the assistant's connection so it reads the tool list again.

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

Times can be RFC 3339, `YYYY-MM-DD HH:MM`, `YYYY-MM-DD`, or a duration ago such as `30m`, `2h`, `7d`.

## What is logged

Each call writes one line to rsRewind's log: tool name, request id, number of results, duration. Never the text,
titles or pictures it returned.
