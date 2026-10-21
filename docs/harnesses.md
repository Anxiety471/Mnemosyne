# Using Drawer from agent harnesses

Drawer speaks the Model Context Protocol (MCP) in two ways. Every harness below supports at least one of them.

- **Streamable HTTP** at `http://127.0.0.1:8787/mcp`, served by `drawer serve`. This is the best choice when several agents, or agents and people, share the same files, because they all talk to one server. Pass `Authorization: Bearer <token>` when the server runs with `--token` / `DRAWER_TOKEN`.
- **Stdio** with `drawer --data-dir <dir> stdio`. The harness starts Drawer itself, so nothing else needs to run.

Stdio and HTTP can point at the same data directory at the same time (SQLite in WAL mode handles it), but you only get one shared view if everyone uses the same directory.

Tested clients: the official Python MCP SDK (which Hermes is built on) and the official TypeScript MCP SDK (used by Claude Desktop, Cursor, VS Code and Gemini CLI), both over stdio and over HTTP with a bearer token. The Rust test suite uses rmcp, the SDK that Codex is built on.

`drawer mcp-config --format <harness>` prints every config below for your binary and store, so you rarely need to type one by hand. Add `--url http://127.0.0.1:8787/mcp` for the shared HTTP form, and `--name` to call the server something other than `drawer`.

## What harnesses get

- 27 tools: 21 named `drawer_*` for files and 6 named `memory_*` for the Obsidian-style memory vault. Hermes shows them as `mcp_<server>_drawer_*`, Claude Code as `mcp__<server>__drawer_*`.
- Server instructions explaining revisions, `drawer_edit`, `if_head_revision` and base64. Most harnesses add these to the model's context.
- Tool titles and annotations. Reads are marked read-only, so harnesses that auto-approve read-only tools can do so. `drawer_purge` is marked destructive. Writes, edits, moves and trash are reversible, because history and the trash keep everything.
- Attribution without setup. A call with no `author` is recorded under the client's MCP name (`claude-code`, `codex-mcp-client`, `gemini-cli-mcp-client`, `Visual Studio Code`, ...). For stdio, `drawer --author NAME stdio` or `DRAWER_AUTHOR` sets it instead, which helps when several agents of the same harness share a store.
- Bounded results. Text reads are cut at 2000 lines or 256 KB with a note on how to page (`offset`, `limit`), and results are compact JSON, so they fit the tool-output limits of Claude Code, Codex and others.
- Input schemas restricted to the portable JSON Schema subset: no `$schema`, `format`, `additionalProperties` or `["type", "null"]` unions. Gemini and other strict model APIs accept them as-is.
- Binary files go in and out as base64 with `encoding: "base64"`. `drawer_read` returns PNG, JPEG, GIF and WebP files as MCP image content, so vision models can look at them.

In the examples below, replace `/usr/local/bin/drawer` and `/srv/drawer-data` with your paths. `DRAWER_TOKEN` is only needed if the server runs with a token.

## Hermes Agent

`~/.hermes/config.yaml`:

```yaml
mcp_servers:
  # local: Hermes starts Drawer
  drawer:
    command: "/usr/local/bin/drawer"
    args: ["--data-dir", "/srv/drawer-data", "stdio"]
    supports_parallel_tool_calls: true   # safe: concurrent writes are guarded by if_head_revision
```

```yaml
mcp_servers:
  # shared: talk to a running `drawer serve`
  drawer:
    url: "http://127.0.0.1:8787/mcp"
    headers:
      Authorization: "Bearer YOUR_TOKEN"
```

To keep an agent away from permanent deletes, add `tools: { exclude: [drawer_purge] }`.

### Mirror every file Hermes writes

MCP lets the agent save to Drawer when it chooses to. To make every `write_file` and `patch` land in Drawer whether or not the agent remembers, use the Hermes plugin in [`integrations/hermes/drawer-mirror`](../integrations/hermes/drawer-mirror/README.md). It is a `post_tool_call` hook that PUTs the file over REST after each successful write. It also appends the links in Hermes' replies to `/hermes/links.md`. Hermes keeps its own artifacts and memory as they are, and both can run side by side.

```yaml
plugins:
  enabled: [drawer-mirror]
```

with `DRAWER_MIRROR_ROOT=~/.hermes/artifacts` in `~/.hermes/.env`.

## Claude Code

`drawer mcp-config --format claude-code` prints the command.

```sh
claude mcp add drawer -- /usr/local/bin/drawer --data-dir /srv/drawer-data stdio
# or, shared:
claude mcp add --transport http drawer http://127.0.0.1:8787/mcp --header "Authorization: Bearer YOUR_TOKEN"
```

## Claude Desktop

`claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "drawer": { "command": "/usr/local/bin/drawer", "args": ["--data-dir", "/srv/drawer-data", "stdio"] }
  }
}
```

## OpenAI Codex CLI

`~/.codex/config.toml` (`drawer mcp-config --format codex`):

```toml
[mcp_servers.drawer]
command = "/usr/local/bin/drawer"
args = ["--data-dir", "/srv/drawer-data", "stdio"]

# or, shared:
# [mcp_servers.drawer]
# url = "http://127.0.0.1:8787/mcp"
# bearer_token_env_var = "DRAWER_TOKEN"
```

## Gemini CLI

`~/.gemini/settings.json` (`drawer mcp-config --format gemini`). Note that Gemini CLI uses `httpUrl` for streamable HTTP, while `url` means the older SSE transport.

```json
{
  "mcpServers": {
    "drawer": { "command": "/usr/local/bin/drawer", "args": ["--data-dir", "/srv/drawer-data", "stdio"] },
    "drawer-shared": { "httpUrl": "http://127.0.0.1:8787/mcp", "headers": { "Authorization": "Bearer YOUR_TOKEN" } }
  }
}
```

## Cursor

`.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global):

```json
{
  "mcpServers": {
    "drawer": { "url": "http://127.0.0.1:8787/mcp", "headers": { "Authorization": "Bearer YOUR_TOKEN" } }
  }
}
```

## VS Code (Copilot agent mode)

`.vscode/mcp.json` (`drawer mcp-config --format vscode`; with `--url` it adds a password prompt for the token):

```json
{
  "servers": {
    "drawer": { "type": "stdio", "command": "/usr/local/bin/drawer", "args": ["--data-dir", "/srv/drawer-data", "stdio"] }
  }
}
```

## opencode

`opencode.json` (`drawer mcp-config --format opencode`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "drawer": { "type": "local", "command": ["/usr/local/bin/drawer", "--data-dir", "/srv/drawer-data", "stdio"], "enabled": true }
  }
}
```

## Agent frameworks

OpenAI Agents SDK (Python):

```python
from agents.mcp import MCPServerStreamableHttp
drawer = MCPServerStreamableHttp(params={"url": "http://127.0.0.1:8787/mcp", "headers": {"Authorization": "Bearer YOUR_TOKEN"}})
```

LangChain / LangGraph via `langchain-mcp-adapters`:

```python
from langchain_mcp_adapters.client import MultiServerMCPClient
client = MultiServerMCPClient({"drawer": {"transport": "streamable_http", "url": "http://127.0.0.1:8787/mcp",
                                          "headers": {"Authorization": "Bearer YOUR_TOKEN"}}})
tools = await client.get_tools()
```

## Harnesses without MCP

Use the REST API at `/drawer/v1` (see the README). Files are plain HTTP: `PUT /drawer/v1/files?path=/a.md` with the raw bytes as the body, and `GET` to read them back. `POST /drawer/v1/edit` does an exact-text replace, and `GET /drawer/v1/diff` and `/recent` show what changed.

Shell-driven agents can also call the CLI directly: `drawer edit`, `drawer diff`, `drawer tree` and `drawer recent` all take `--json`.

## Local folder integration

Agents that need a real directory rather than an MCP call have two operator routes on the server.

**`drawer sync`** mirrors a local folder with a remote Drawer folder. It hashes local files incrementally, uses guarded resumable uploads, pins downloads to immutable revisions and verifies SHA-256 before installing. Pending uploads survive restarts, and `--watch` retries offline failures every 30 seconds. It is deliberately a content sync, not a filesystem mirror: deletions and renames do not propagate, and the remote wins when both sides change. See [Desktop folder sync](../README.md#desktop-folder-sync).

**`drawer mount`** registers a read-only host folder so any authenticated operator can browse it in the UI and import files into Drawer. Registration needs local CLI access; HTTP clients only see registered roots. Imports stream through a temporary file, are capped at 512 MiB, and never overwrite an existing Drawer file. Capability-based access prevents parent traversal and symlink escapes, and symlinks are omitted from listings. See [Read-only host folder mounts](../README.md#read-only-host-folder-mounts).

Neither route is an MCP tool; a harness reaches them through a shell if at all.

## Limits worth knowing

- Actors are self-declared through the `author` argument, so sharing grants are cooperative, not a security boundary. Put Drawer behind `--token` and a private network.
- MCP tool results carry whole files, and base64 makes binaries a third larger. Move very large files over REST instead of through the model's context.
- The legacy HTTP+SSE transport is not served; every harness above supports streamable HTTP or stdio.
