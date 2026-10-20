<div align="center">

# Drawer

**Versioned file storage for agents, modelled on Google Drive.**

One Rust binary serves a REST API, an MCP server and a web UI over the same store.

[![CI](https://github.com/Anxiety471/drawer/actions/workflows/ci.yml/badge.svg)](https://github.com/Anxiety471/drawer/actions/workflows/ci.yml)

<img src="docs/screenshots/browse-light.png" alt="Drawer file browser in light mode" width="860">

</div>

Agents write files all day and sooner or later one of them overwrites something it shouldn't. Drawer keeps every version, puts deleted things in a trash, and lets two agents share a folder without silently clobbering each other's writes.

For a private Ubuntu file-transfer server, see [deploying Drawer with Docker Compose](docs/deploy.md).
The personal-server build supports an administrator password, resumable browser uploads, upload folders, and bulk file actions (see the web UI features below).

## Quickstart

Linux and macOS, one command:

```sh
curl -fsSL https://raw.githubusercontent.com/Anxiety471/drawer/main/install.sh | sh
drawer serve                                # http://127.0.0.1:8787
```

Install and update take seconds because they download a prebuilt release. Releases are published automatically: bumping `version` in `Cargo.toml` on `main` builds the archives and creates `vX.Y.Z` (pushing a `v*` tag also works). Until a release exists the installer builds from source, which takes minutes, and `drawer update` has nothing to fetch.

The installer downloads the release for your machine, checks its SHA-256, and puts `drawer` in `~/.local/bin` with the UI in `~/.local/share/drawer/ui`. If no release exists for your platform it builds from source with cargo (installing Rust via rustup if needed).

To update an install, run `drawer update`. It fetches the latest release for your machine, checks its SHA-256 against `SHA256SUMS`, and replaces the binary and the UI in place. `drawer update --check` only reports whether a newer release exists. Restart `drawer serve` afterwards to run the new version. The store is never touched. The web UI shows an **Update to vX** button in the top bar when a newer release exists; it asks first, then runs the same update.

To remove Drawer, run `drawer uninstall`. It stops and removes the background service if you installed one, then deletes the web UI and the binary. Your data in `~/.drawer` is left alone.

Drawer runs on its own; Hermes or another agent harness is optional. By default,
all store commands use `~/.drawer`, regardless of your current working directory:

| Path | Contents |
| --- | --- |
| `~/.drawer/drawer.db` | File metadata, revisions and download links |
| `~/.drawer/blobs/` | Content-addressed file contents |
| `~/.drawer/transfers/` | Temporary resumable uploads |

Override the location with `drawer --data-dir /path/to/store serve` or
`DRAWER_DATA_DIR=/path/to/store drawer serve`. The flag takes precedence over the
variable. If no absolute home directory is available through `HOME` or
`USERPROFILE`, set one of these explicit overrides. Docker Compose already sets
`DRAWER_DATA_DIR=/data` and continues using its named volume.

**Existing stores:** older versions defaulted to `./data`. This change does not
move or delete that directory. Keep using it with an absolute path, for example
`drawer --data-dir /home/you/old-project/data serve`. Back up the complete store
(database, WAL files if present, blobs and transfers) before relocating it while
all Drawer writers are stopped. Never copy just the uploaded files or database.

Optionally, point an agent at it. `drawer mcp-config --format <harness>` prints the exact config for Hermes, Claude Code, Codex, Gemini CLI, VS Code, opencode, or any client that takes an `mcpServers` JSON block. See [MCP and agent harnesses](#mcp-and-agent-harnesses).

## Features

- **Stable ids.** Every file and folder has an id that survives rename, move and trash. Moving a folder moves everything under it.
- **Revisions.** Every save is a new immutable revision, with its author. Diff any two revisions; rolling back adds the old content as the new head, so nothing is lost.
- **Targeted edits.** Replace an exact piece of text instead of rewriting the whole file, saved as one guarded revision. Cheaper for agents and safe alongside other writers.
- **Safe concurrent writes.** Writes accept `if_head_revision`. A stale writer gets `412 Precondition Failed` and nothing changes.
- **Trash.** Deleting moves things to the trash, with their history. Restore by id, or purge for good.
- **Star, search, copy, move.** Star files and folders, search names and contents (name matches first, content matches with line numbers), and copy without re-uploading: copies share content by hash.
- **Recent activity.** See which files changed last and which agent or person changed them.
- **Download links.** Create a 7-day link from a file's menu and copy it to anyone who can reach your server. Links download only the latest file revision, expire automatically, and can be revoked in the UI. The database stores only hashes of the link secrets.
- **Cooperative agent sharing.** Grant `reader`, `writer` or `owner` to an agent principal on a folder and it covers everything below. These grants are not user authentication; callers can self-declare their principal.
- **Custom properties.** Key/value pairs stored with each revision.
- **Any file type.** Stored byte-for-byte, up to 512 MB per upload.
- **Previews in the UI.** PDF, Word (`.docx`), Excel (`.xlsx`, `.xls`, `.ods`), CSV/TSV, Markdown, HTML, images, audio and video. Text and source code are editable with syntax highlighting. Other types (`.doc`, `.pptx`, archives) download.
- **Optional bearer token** for the HTTP API, MCP endpoint and UI.

Content is stored as sha256-addressed blobs on disk, and metadata lives in SQLite.

## Web UI

A Drive-style browser with My Drawer, Recent, Starred and Trash, a search box over names and contents (press `/`), and a version history panel on every file.

- **Live.** Open folders and files pick up what agents save within a few seconds. An unchanged file reloads itself; one with unsaved edits shows who saved what instead of losing your work.
- **See what changed.** Each revision in the history has a diff against the one before it.
- **Organise.** Sort by name, modified or size; rename, move, copy and download from a row's menu (or right-click); trashing offers Undo.
- **Links work.** The folder and open file are in the URL, so reload, back/forward and bookmarks do what you expect.
- **Token prompt.** When the server runs with a token, the UI asks for it once and remembers it in the browser.
- **Share a download.** Open a file's More actions menu, choose **Share download link**, then **Create 7-day link** and **Copy**. Reopen the menu to view expiry times or revoke an existing link. A secret is visible only when newly created, not later.
- **Permissions.** A row's More actions menu opens **Permissions**, where you see who holds a grant on that node and can add or remove one. See [Sharing and grants](#sharing-and-grants).

<p align="center">
  <img src="docs/screenshots/history.png" alt="Version history for a Markdown file" width="860"><br>
  <img src="docs/screenshots/editor.png" alt="Editing a Markdown file with unsaved changes" width="860">
</p>

There is a dark theme too: <img src="docs/screenshots/browse-dark.png" alt="Drawer file browser in dark mode" width="860">

## Command line

`drawer` with no subcommand runs `serve`. The file commands work on the store directly, so no server has to be running. Paths look like `/notes/todo.md`. Run `drawer --help` or `drawer <command> --help` for every flag.

```sh
drawer put ./report.pdf /work/            # upload into a folder (trailing / or existing folder)
drawer put - /notes/todo.md < todo.md     # from stdin; needs an explicit file path
drawer ls -l /work                        # kind, revision, size, modified, name
drawer cat /notes/todo.md > copy.md       # stdout; --revision N for an old one
drawer get /work/report.pdf               # save to ./report.pdf; --force to overwrite
drawer tree /work -L 2                    # folders and files as a tree
drawer edit /notes/todo.md --old "- [ ] ship" --new "- [x] ship"   # exact-text replace, one revision
drawer history /notes/todo.md             # revisions, newest first
drawer diff /notes/todo.md                # what the latest revision changed; or: diff PATH FROM TO
drawer rollback /notes/todo.md 2          # adds revision 2's bytes as the new head
drawer recent                             # most recently changed files, and by whom
drawer search "quarterly roadmap"         # names and contents
drawer rm /notes/old.md                   # to the trash; the id is kept
drawer trash                              # list it, with ids
drawer restore <id>                       # bring it back
drawer purge <id> --yes                   # gone for good
```

| Group | Commands |
| --- | --- |
| Run | `serve` (default), `stdio`, `update [--check]`, `doctor`, `mcp-config`, `completions <shell>` |
| Files | `ls`, `tree`, `mkdir`, `put`, `edit`, `cat`, `get`, `stat`, `mv`, `cp` |
| Versions | `history`, `diff`, `rollback`, `recent` (`cp` takes `--revision`, `cat` and `get` too) |
| Trash | `rm`, `trash`, `restore <id>`, `purge <id> --yes` |
| Search and stars | `search`, `star`, `unstar`, `starred` |
| Sharing | `perms`, `share <path> <principal> <role>`, `unshare <path> <principal>` |

Global flags: `--data-dir` (`DRAWER_DATA_DIR`, default `~/.drawer`), `--author` (`DRAWER_AUTHOR`, default `$USER`) which is recorded on every change, and `--json` for machine-readable output from commands that return data.

`drawer doctor` checks database integrity, counts, missing blob files and whether the UI is built. `drawer completions bash|zsh|fish|powershell|elvish` prints a shell completion script.

Writes are serialized inside one process. A CLI write while a server is writing to the same store can race it, so route concurrent writes through one or the other.

## MCP and agent harnesses

Drawer speaks MCP two ways:

- **Streamable HTTP** at `http://127.0.0.1:8787/mcp`, served by `drawer serve`. Best when several agents, or agents and people, share files.
- **Stdio** with `drawer --data-dir ./data stdio`. The harness starts Drawer itself.

For Hermes Agent (`~/.hermes/config.yaml`):

```yaml
mcp_servers:
  drawer:
    command: "/usr/local/bin/drawer"
    args: ["--data-dir", "/srv/drawer-data", "stdio"]
```

`drawer mcp-config` prints this for your paths. Pick the harness with `--format`, and add `--url http://127.0.0.1:8787/mcp` to connect to a running server instead of starting one:

| `--format` | Prints |
| --- | --- |
| `hermes` (default) | YAML for `~/.hermes/config.yaml` |
| `claude-code` | a `claude mcp add` command |
| `codex` | TOML for `~/.codex/config.toml` |
| `gemini` | JSON for `~/.gemini/settings.json` (`httpUrl` for HTTP) |
| `vscode` | JSON for `.vscode/mcp.json`, with a password prompt for the token |
| `opencode` | JSON for `opencode.json` |
| `json` | an `mcpServers` block for Claude Desktop, Cursor, Windsurf and most others |

The server exposes 27 tools:

| Tools | |
| --- | --- |
| `drawer_list`, `drawer_stat`, `drawer_search`, `drawer_starred`, `drawer_recent` | browse and find |
| `drawer_mkdir`, `drawer_read`, `drawer_write`, `drawer_edit` | create and change content |
| `drawer_revisions`, `drawer_diff`, `drawer_rollback`, `drawer_copy`, `drawer_move` | history and layout |
| `drawer_delete`, `drawer_trash`, `drawer_restore`, `drawer_purge` | trash lifecycle |
| `drawer_star`, `drawer_share`, `drawer_unshare` | stars and grants |
| `memory_remember`, `memory_recall`, `memory_read`, `memory_forget`, `memory_tags`, `memory_graph` | [AI memory vault](#ai-memory-vault) |

Built for how agents work:

- **Small edits stay small.** `drawer_edit` replaces an exact `old_string` with `new_string`, like the edit tools in Claude Code and Codex, so an agent doesn't resend a whole file to change a line.
- **Whole trees in one call.** `drawer_list` with `depth` up to 10 lists everything below a folder with full paths.
- **Reads say what to guard on.** `drawer_read` follows text with a `[drawer]` line giving the revision to pass as `if_head_revision`. Long files are cut at 2000 lines (or 256 KB) with a note saying how to page with `offset` and `limit`, so one read can't flood the context.
- **Lean results.** Tool results are compact JSON, and listings carry only what an agent acts on: path, kind, id, revision, size, type, when and by whom.
- **History knows who.** Calls that don't pass `author` are recorded under the client's own MCP name (`claude-code`, `codex-mcp-client`, `gemini-cli-mcp-client`, ...), or under `--author` / `DRAWER_AUTHOR` for `drawer stdio`. `drawer_recent` and `drawer_diff` then show what each agent did.
- **Portable schemas and hints.** Input schemas avoid the JSON Schema features Gemini and other strict APIs reject. Every tool has a display title and read-only/destructive hints, so harnesses can auto-approve reads and confirm `drawer_purge`, the only destructive tool.

`drawer_read` and `drawer_write` take `encoding: "base64"` for binary files such as PDF, docx, xlsx or images. `drawer_read` returns PNG, JPEG, GIF and WebP images as image content so vision models can look at them.

For concurrent agents, read the file, then write with `if_head_revision` set to the revision you read (or use `drawer_edit`, which guards itself). On a conflict, re-read and merge.

Setup for Hermes, Claude Code, Claude Desktop, Codex, Gemini CLI, Cursor, VS Code, the OpenAI Agents SDK and LangChain is in [docs/harnesses.md](docs/harnesses.md).

**Mirroring Hermes output.** MCP only saves what the agent chooses to save. [`integrations/hermes/drawer-mirror`](integrations/hermes/drawer-mirror/README.md) is a Hermes plugin that copies every successful `write_file` and `patch` into Drawer after the fact, and records the links Hermes shares. Hermes keeps its own artifacts, and both can run side by side.

### AI memory vault

Drawer can hold long-term memory for AI agents, designed like an [Obsidian](https://obsidian.md) vault. Memories are plain Markdown notes in the `/memory` folder, so they are versioned, searchable, and editable in the web UI like any other file, and you can open the folder in Obsidian itself via a synced copy (`drawer sync`).

- **Notes** are `.md` files. The note name is the file name; folders (`people/Alice`) work as in Obsidian.
- **Links**: `[[Note Name]]`, `[[folder/Note|alias]]` and `[[Note#Heading]]`. A bare name matches the note in any folder, shortest path first. Reading a note returns its outgoing links (marking ones that point to notes that don't exist yet) and its **backlinks**.
- **Tags**: `#tag` in the text and `tags: [a, b]` (or a YAML list) in frontmatter. `#project` also matches `#project/drawer`. Code blocks and inline code are ignored.
- **Derived index.** Links, backlinks and tags are computed from the files on each request, so edits made through any route (UI, REST, `drawer_write`, sync) are reflected at once. This suits vaults of up to a few thousand notes.

Agents use the `memory_*` MCP tools: `memory_recall` (text and/or tag search), `memory_read` (a note with links and backlinks), `memory_remember` (create, replace or `append`) and `memory_tags`. The server's MCP instructions tell clients to use them. The same operations are in the REST API under `/memory`, and on the command line:

```sh
drawer memory remember "Project Drawer" "Rust file server. Owner: [[people/Alice]]" -t project
echo "Prefers tabs" | drawer memory remember Preferences -t style --append
drawer memory recall "file server"      # or: drawer memory recall --tag project
drawer memory read "Project Drawer"     # content, links and backlinks
drawer memory forget "Old note"      # to the trash; `drawer restore <id>` undoes it
drawer memory graph                     # who links to whom; missing notes marked with ?
drawer memory tags
```

**Managing memories.** The Memories panel is full create, read, update and delete: **+ New memory** (or click a dashed graph node to write a note that is linked but missing), **Edit** and **Delete** on an open note. Delete moves the note to the trash, so it can be restored.

**Browsing memories.** The web UI has a **Memories** entry in the sidebar. Its **Graph** view is Obsidian's graph view: every note is a node, every `[[link]]` an edge, colour follows the first tag, size follows how connected a note is, and dashed nodes are notes that are linked but not written yet. Drag, zoom, filter by name or `#tag`, and click a node to open the note. **List** view has search and tag chips; opened notes render with clickable `[[links]]` and a "Linked from" list. The graph is also `GET /drawer/v1/memory/graph` and the `memory_graph` MCP tool. Or open the same files in the normal file browser under `/memory`.

**How an AI saves memories.** Connect it over MCP (see above); the server's instructions tell it to call `memory_recall` before answering and `memory_remember` to save durable facts. Agents without MCP can `PUT /drawer/v1/memory/note` or run `drawer memory remember`.

## REST API

Base path `/drawer/v1`. Nodes are named by `path=` or `id=`. Mutations name their actor with the `x-drawer-author` header (default `api`). `GET /health` and `GET /drawer/v1/health` both return `{"ok": true}`. With `--token` set, both need the bearer token like everything else.

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | live children of a folder; `&depth=N` for everything N levels down |
| POST | `/folders` | `{"path"}`, creates parents too |
| DELETE | `/folders?path=` or `?id=` | trash a folder (`&if_head_revision=N` optional) |
| GET | `/files?path=` or `?id=` | raw bytes; `&revision=N`; `&download=true` for an attachment. Response carries `x-drawer-revision`, `x-drawer-prop-*` and an `ETag`; `If-None-Match` gets a 304 |
| PUT | `/files?path=` | raw body; `x-drawer-comment`, `x-drawer-prop-<key>`, `&if_head_revision=N` |
| DELETE | `/files?path=` or `?id=` | trash a file (`&if_head_revision=N` optional) |
| POST | `/transfers` | start one resumable upload: `{path, size, sha256?, if_head_revision?}` |
| POST | `/transfers/batch` | start up to 100 uploads at once: `{files:[{path, size, sha256?, if_head_revision?}]}`. All or nothing: any invalid or duplicate path, or a quota shortfall for the combined size, rejects the batch. Returns `{transfers:[...]}` in order |
| PUT | `/transfers/{id}` | append a chunk (1 to 8 MiB) at `x-upload-offset` |
| POST | `/transfers/{id}/complete` | save the staged file as a new revision |
| POST | `/transfers/batch/complete` | complete many: `{ids:[...]}`. Each id succeeds or fails independently; returns `{results:[{id, ok, file?|error?}]}` |
| GET | `/files/meta?path=` or `?id=` | id, head revision, size, mime, starred, trashed, properties |
| POST | `/edit` | `{"path","old_string","new_string","replace_all","author","comment","if_head_revision"}`: exact-text replace as one revision |
| GET | `/revisions?path=` or `?id=` | revision history, newest first |
| GET | `/diff?path=` or `?id=` | unified diff; `&from=N&to=M` (default: head vs the one before), `&context=N` |
| POST | `/rollback` | `{"path"\|"id","revision","author","comment","if_head_revision"}` |
| POST | `/copy` | `{"from"\|"from_id","to","source_revision","author","comment","if_head_revision"}` |
| POST | `/move` | `{"from"\|"from_id","to"}`: rename or move, keeps the id |
| PATCH | `/nodes/{id}` | `{"name","parent_id","starred","trashed"}`: rename, move, star, trash, restore |
| DELETE | `/nodes/{id}` | purge; the node must already be in the trash |
| GET | `/trash` | top-most trashed nodes |
| GET | `/starred` | starred live nodes |
| GET / PUT | `/nodes/{id}/permissions` | list grants; `PUT {"principal","role"}` (owner only) |
| DELETE | `/nodes/{id}/permissions/{principal}` | remove a grant (owner only) |
| POST | `/nodes/{id}/links` | `{"expires_in_days":7}` (1–30). Returns a one-time `token` for a read-only download link |
| GET | `/nodes/{id}/links` | list link ids, creation/expiry times, but never the secret token |
| DELETE | `/nodes/{id}/links/{link_id}` | revoke a download link |
| GET | `/s/{token}` (outside `/drawer/v1`) | public attachment download, no instance bearer token needed; 404 when invalid, expired, revoked or trashed |
| GET | `/search?q=` | names first, then contents with `line` and `snippet` |
| GET | `/memory/notes?tag=&q=&limit=` | memory notes, newest first, filtered by tag and/or text |
| GET / PUT / DELETE | `/memory/note` | `GET ?name=` returns content, tags, links and backlinks; `PUT {"name","content","tags","append"}` creates, replaces or appends; `DELETE ?name=` moves the note to the trash |
| GET | `/memory/tags` | every tag with its note count |
| GET | `/memory/graph` | link graph: `nodes` (note, tags, in/out counts; no `path` for unwritten notes) and `edges` |
| GET | `/recent?limit=` | live files, most recently changed first, with `modified_by` |
| GET | `/update` | is a newer release published? `{current, latest, update_available}` |
| POST | `/update` | install the latest release if newer; the server runs the new version after a restart |

Files are served with `nosniff`, and everything except PDFs gets a `sandbox` CSP, so stored HTML or SVG never runs scripts. Write responses include `new_revision`: `false` means content and properties matched the head, so nothing was added.

## Architecture

```
src/service.rs   nodes, revisions, grants, trash, search: the one service layer
src/api.rs       REST API at /drawer/v1
src/mcp.rs       MCP tools (rmcp), stdio and streamable HTTP
src/cli.rs       the drawer command
src/blob.rs      sha256 content-addressed blobs on disk
src/schema.sql   SQLite metadata (sqlx)
ui/              React, TypeScript, Vite: the web UI
integrations/    harness plugins, e.g. the Hermes drawer-mirror
```

The REST API, MCP server, CLI and UI all call the same service layer, so they agree on permissions, revisions and the trash.

**Security for download links.** Configure a strong `DRAWER_TOKEN` before using Drawer beyond a trusted local machine. The authenticated owner can create a public, read-only capability link which anyone possessing it can download until it expires or is revoked. A valid link does **not** give access to any REST API or MCP operation. Put Drawer behind HTTPS for sharing outside your private Tailscale network, and treat the URL as a password: avoid posting it in public logs or forwarding it accidentally. A link points at the latest revision and becomes unavailable while the file or any containing folder is trashed. If the server is reachable only over Tailscale, the recipient also needs access to that tailnet or a controlled public HTTPS endpoint.

**Sharing and grants.** A node with no grants is open to every actor that can reach the store; grants only narrow access, they never widen it. `drawer_share <path> <principal> <role>` and the UI's **Permissions** dialog grant `reader`, `writer` or `owner` on a node and everything below it, and `drawer_unshare` / the same dialog remove one. The `principal` is a self-declared name, so grants are cooperative bookkeeping, not an authentication boundary. Whoever can reach the server with `DRAWER_TOKEN` (or an admin session) already has full access; put Drawer on a private network and treat the token as the access control.

**Cross-origin access.** A server with no admin password and no token answers no CORS headers at all: browser pages on another origin cannot call the API. Set `DRAWER_CORS_ORIGINS` to a comma-separated list of origins to allow exactly those, for example `https://drawer.example.com`, or to `*` for the old permissive default. A server that requires a bearer token keeps the permissive `*` default, since its callers authenticate. With `DRAWER_ADMIN_PASSWORD` set, cookie auth is already refused for cross-origin requests and this variable has no effect.

**Login rate limiting.** With an admin password configured, `/auth/login` allows five failed attempts per client address and username, then refuses further attempts with `429 Too Many Requests` and a `Retry-After` header. The delay doubles on each further failure, up to 15 minutes, and a successful login clears the counter. The admin password is stored as an Argon2id hash with a random salt, the same scheme named-user accounts use.

**Limits.** Real user accounts don't exist yet. Actors are self-declared, so grants are cooperative, not a security boundary: a node with no grants is open to everyone who can reach the store, and a grant only narrows that. Copying folders isn't supported yet. There are no signed URLs, no blob garbage collection and no Postgres backend. Treat the optional bearer token as the access control.

**Upgrading.** Stores created on the earlier path-keyed schema are migrated in place on open. Their trashed files land in the trash. Write responses changed: `created` is now `new_revision`, and `version` is now `revision`. MCP calls without an `author` used to be recorded as `agent`; they now carry the client's name, so a sharing grant written for `agent` needs the client's name instead (or start `drawer stdio` with `--author agent`). MCP tool results are compact JSON, and the listing tools return lean entries; `drawer_stat` still returns the full node.

**Performance.** Listings, search, starred and trash resolve every path in one recursive query rather than one query per ancestor. SQLite runs in WAL mode with `synchronous=NORMAL` and a busy timeout, so a CLI command and a server can share a store without `database is locked` errors.

## Configuration

| Variable | Used by | Default |
| --- | --- | --- |
| `DRAWER_DATA_DIR` | all store commands | `~/.drawer` |
| `DRAWER_ADDR` | `serve` | `127.0.0.1:8787` |
| `DRAWER_COOKIE_SECURE` | `serve` | `false`; `1` or `true` requires HTTPS |
| `DRAWER_CORS_ORIGINS` | `serve` | unset (no CORS headers in open mode) |
| `DRAWER_PUBLISH` | compose | `127.0.0.1:8787` |
| `DRAWER_MEM_LIMIT` | compose | `512M` |
| `DRAWER_CPU_LIMIT` | compose | `1.0` |
| `DRAWER_TOKEN` | `serve` | unset (an empty value counts as unset) |
| `DRAWER_UI_DIR` | `serve` | `./ui/dist`, then `../share/drawer/ui` beside the binary |
| `DRAWER_AUTHOR` | CLI writes | `$USER` |
| `DRAWER_PREFIX` | installer | `~/.local` |
| `DRAWER_VERSION` | installer | `latest` |
| `DRAWER_SOURCE=1` | installer | build from source instead of downloading a release |
| `DRAWER_RUSTUP_ROOT` | installer | `https://static.rust-lang.org/rustup` |

`serve` warns when it listens beyond loopback without a token.

## Development

```sh
cargo test && cargo clippy --all-targets -- -D warnings
cd ui && npm ci && npm run dev     # UI dev server, proxies /drawer to 127.0.0.1:8787
cd ui && npm run build             # type-check and build ui/dist
cargo run --release -- serve       # API, MCP and UI on http://127.0.0.1:8787
```

CI runs `cargo fmt --check`, clippy, the Rust tests, `cargo audit`, the UI tests and build, `npm audit`, shellcheck, hadolint, ruff, the maintenance-script round trip, and a check that the MCP tool count in this README matches `src/mcp.rs`.

Releases: pushing a `v*` tag builds Linux (x86_64, aarch64) and macOS (x86_64, aarch64) binaries and attaches them with `SHA256SUMS` (`.github/workflows/release.yml`).

## Linux background service

`drawer service install` installs and starts a per-user systemd service using your current binary and configured store. Use `--no-start` to write the unit for review, then `systemctl --user daemon-reload && systemctl --user enable --now drawer`. Existing units are never overwritten. `drawer service start|stop|status|uninstall` manages it; uninstall leaves your data in place.

Put `DRAWER_ADMIN_PASSWORD` and `DRAWER_TOKEN` in `~/.config/drawer/env` (create the parent directory and use `chmod 600` on the file) before starting. Do not put secrets in the service unit. Logs: `journalctl --user -u drawer`. To run after logout and at boot, enable user lingering with `loginctl enable-linger "$USER"` if your administrator permits it. Docker deployments should continue using Compose restart policies.

Browser uploads compute SHA-256 incrementally in a worker before sending chunks, pass the expected hash to the server and compare it with the committed revision. Resume IDs include the content hash, so selecting a different same-sized file cannot splice two files together. Hashing is bounded to 4 MiB chunks and works on private HTTP as well as HTTPS.

Public links can be fixed to a specific revision: select **Keep this link on revision N** in the share dialog, or provide `revision` when creating a link over REST. Later edits do not change that link's contents. Default links continue following the latest head. Trash, expiry and revocation apply to both modes.

Share links optionally accept a download limit (1–1,000,000) from the dialog or `max_downloads` in REST. A request consumes a use when an authorized download is issued, even if the recipient cancels the stream. Exhausted links return 404. Concurrent requests reserve uses atomically; blank means unlimited.

## Transfer manager

Open **Transfers** in the sidebar to see active, paused, failed, expired, cancelled and completed uploads. Pause, select the same file to resume/retry, or cancel its staging data. Metadata and progress survive restarts; an expired lease cannot be resumed. The last 200 entries for your actor are shown. Authenticated download requests appear as **issued**; your browser's native download manager owns their completion and retry state.

Select files/folders and choose **Download ZIP**, or use a folder's **Download folder as ZIP** menu. The archive preserves full Drawer-relative paths and empty folders, deduplicates overlapping selections, streams in bounded memory with ZIP64 support, and snapshots revisions at request time. A request supports up to 200 selections/5000 entries; oversized selections fail instead of silently truncating. Password sessions use native streaming downloads; legacy bearer-token browsers still buffer the resulting ZIP in the browser.

### Read-only host folder mounts

Register a server directory from the local operator CLI, using the server's store:

```sh
drawer mount add Documents /srv/documents
drawer mount list
drawer mount remove Documents
```

Open **Host folders** in the sidebar to browse registered directories, select a file,
and import it into a new Drawer path. Imports stream through a temporary file, are
limited to 512 MiB each, and never overwrite an existing Drawer file. The original
host file is not modified. Removing a registration preserves imported files.
Registration requires local CLI access; HTTP clients can only use registered roots.
Capability-based filesystem access prevents parent traversal and symlink escapes.
Symlinks are omitted from directory listings. Only register directories you intend
to expose to every authenticated operator of this instance. Container deployments
must bind-mount the directory into the container first. This exposes host folders
inside Drawer; it does not provide an operating-system FUSE mount.

### Activity history

The sidebar's **Activity history** records REST changes, file reads, public share
requests, logins (including failures), and MCP transport requests. Events persist
in SQLite and can be paginated with `GET /drawer/v1/activity?before=ID&limit=100`.
Routine listing/polling is excluded. Only method, route template, actor, HTTP status
and time are stored: no request bodies, query strings, credentials or link tokens.
MCP entries describe transport requests, not individual tool calls. HTTP success
means a response was issued, not that a browser completed its download. CLI writes
are not included. Events are retained until the operator prunes the database;
this local history is best-effort and is not a tamper-proof compliance log.

### Verified snapshots and backup schedules

Stop Drawer before offline snapshots. Current Drawer processes hold a shared
store lease; maintenance takes an exclusive lease and refuses an active store.
Stop older binaries too, since they do not know about this lease.

```sh
drawer backup create --directory /other-disk/drawer-backups
drawer backup list --directory /other-disk/drawer-backups
drawer backup verify /other-disk/drawer-backups/drawer-UUID
drawer backup restore /other-disk/drawer-backups/drawer-UUID --to /new/store
# Linux systemd user service; briefly stops/restarts drawer.service on each run:
drawer backup schedule --directory /other-disk/drawer-backups --calendar daily
drawer backup unschedule
```

Snapshots contain a compact SQLite database, every referenced revision blob, and
a SHA-256 manifest. Creation verifies every file, SQLite integrity/foreign keys,
file head references and revision blob hashes before publishing the directory.
Restore only accepts a destination that does not exist. Interrupted restores leave
that new directory for inspection. Original stores and snapshots are preserved.
Host mount registrations are backed up; their external contents are not. Staged
uploads are excluded and must be re-uploaded after a restore. Keep snapshots on a
separate device and protect them like the original store. No retention deletion
is automatic. `--no-start` writes schedule units for inspection without systemd.

### Offline storage cleanup

Stop Drawer and make a verified backup first. Preview retention candidates with:

```sh
drawer cleanup --keep-revisions 10 --trash-days 30 --orphan-days 1
# Apply the same policy explicitly:
drawer cleanup --keep-revisions 10 --trash-days 30 --orphan-days 1 --apply
```

Cleanup requires an exclusive store lease. Preview is the default. It permanently
purges expired trash subtrees and old revisions, retaining each head, the newest
N revisions and revisions pinned by unexpired download links. Blob collection
uses references remaining after that policy, so shared/deduplicated content is
preserved. Only valid SHA-256 blob filenames older than the grace period qualify.
Metadata commits before unlinks; interrupted removal can leave extra orphans for
the next run. Recent orphans, temporary files and staged uploads are not deleted.

### Upload-only folder links

Use a folder's menu → **Create upload-only link**. Set expiry (up to 30 days),
file count and total byte budget, then copy the one-time-displayed URL. Recipients
get a standalone upload page; they cannot list, read or overwrite Drawer files.
Names receive random prefixes. Revoke links from the same dialog. Individual files
are limited to 512 MiB and requests to 15 minutes. Uploads require Content-Length,
reserve budget atomically before streaming, and stage to disk with bounded memory.
Failed uploads refund reservations; a crash or disconnected handler can leave a
reservation consumed, so create a replacement link if necessary. Revocation or
folder changes are rechecked at commit. Only token hashes are stored. Treat these
capability links as secrets; file contents are not scanned for malware.

The upload page itself is `src/deposit.html`, served at `/drop/{token}`: chunked, resumable uploads (start, append at an offset, complete) with client-side SHA-256 over 256 KiB chunks, and per-file rows showing progress with Cancel and Retry. The link's budget, file size and expiry are enforced per chunk on the server.

### Named users and private quotas

With `DRAWER_ADMIN_PASSWORD` enabled, sign in as `admin` and open **Users and
quotas** to create accounts, set quotas, or disable access. New accounts require
12-character passwords and store salted Argon2id hashes. Sign in with the account
name and password; the existing machine token remains an administrator credential.
Each account gets an independent store under `users/UUID`, including isolated MCP
sessions, search, revisions, transfers and activity. Named users cannot manage
accounts, expose host mounts or install server updates. Disabling an account
blocks existing sessions and its public links on their next request.

Quota is logical revision content, including history and trash; identical revisions
on different files each count. It is not a host filesystem hard quota. New staged
transfers reserve against existing transfer sizes. Lower quotas block writes until
usage fits. Share through expiring download/upload-only URLs, which include the
workspace identity; node permission grants do not bridge private workspaces.
Root snapshots include existing user stores, with an exclusive lease on each.
Cleanup targets only the selected store; operators may select a user's store with
`--data-dir` when offline maintenance is needed. Back up the whole root before
changing account data or cleaning private history.

### Desktop folder sync

```sh
# Credentials stay in environment variables, never in sync state or command arguments.
export DRAWER_TOKEN='your-machine-token'
drawer sync ~/Documents/Drawer --url https://drawer.example.com --remote /Desktop
# Keep synchronizing and retry offline failures every 30 seconds:
drawer sync ~/Documents/Drawer --url https://drawer.example.com --remote /Desktop --watch
# Named account: unset DRAWER_TOKEN; set DRAWER_SYNC_USERNAME and DRAWER_SYNC_PASSWORD.
# For a server with authentication disabled, pass --anonymous explicitly.
```

Sync supports files up to 4 GiB, empty folders and up to 5000 entries per selected
folder. It hashes local files incrementally, uses guarded resumable uploads, pins
downloads to immutable revisions, and verifies downloaded SHA-256 hashes before
installing them. Pending upload IDs and offsets survive restarts. Offline errors
preserve local files and state; `--watch` retries until interrupted.

When both sides change, the remote revision becomes the main local file and the
local version is preserved as `.conflict-UUID`, then uploaded on the next pass.
Previous local inodes from downloads are kept in `.drawer-sync/previous` for
recovery, including late writes from editors with open handles. Remove old recovery
copies manually when no longer needed. `.drawer-sync` contains durable state,
uses an exclusive local lock, and is never uploaded. Symlinks are skipped; portable
relative paths and capability-based access keep downloads inside the local root.

Deletion and rename propagation are deliberately absent: a surviving copy restores
its missing peer, and a rename can produce a second copy. This is a conservative
content sync, not a filesystem mirror or native desktop GUI. Choose a different
local directory for each server/account/remote folder; the durable binding rejects
accidental reuse. Download retries restart that file; uploads resume from the
server's committed offset. Servers must remain at the configured URL (redirects
are rejected so credentials are not forwarded to another origin).
