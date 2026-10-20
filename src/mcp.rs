//! MCP server. Tool names carry the `drawer_` prefix.
//!
//! Agents address nodes by path. Trash, restore and purge take a node `id`, since a trashed
//! node no longer has a live path. `author` names the actor for permission checks. When a call
//! leaves it out, the actor is the server's configured author (`drawer --author NAME stdio`),
//! else the client's own name from the MCP handshake (`claude-code`, `codex-mcp-client`, ...),
//! so history shows which harness made each change.
use std::{collections::BTreeMap, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

use rmcp::{
    RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig,
        ToolAnnotations,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::service::{Drawer, Edit, Node, NodeUpdate, Ref, WriteOpts};

#[derive(Clone)]
pub struct DrawerMcp {
    svc: Drawer,
    /// Actor for calls that don't pass `author`; the client's name is used when this is unset.
    author: Option<String>,
    verified_actor: Option<String>,
    tool_router: ToolRouter<Self>,
}

impl DrawerMcp {
    pub fn new(svc: Drawer) -> Self {
        let mut tool_router = Self::tool_router();
        for route in tool_router.map.values_mut() {
            let tool = &mut route.attr;
            let schema = portable_schema(Value::Object((*tool.input_schema).clone()));
            if let Value::Object(schema) = schema {
                tool.input_schema = Arc::new(schema);
            }
            let (title, ann) = annotations_for(&tool.name);
            tool.title = Some(title.into());
            tool.annotations = Some(ann);
        }
        Self {
            svc,
            author: None,
            verified_actor: None,
            tool_router,
        }
    }

    /// Record `author` on changes when a call doesn't name one. Empty means unset.
    pub fn with_author(mut self, author: Option<String>) -> Self {
        self.author = author.filter(|a| !a.trim().is_empty());
        self
    }

    /// HTTP password mode has one verified identity, regardless of tool arguments.
    pub fn with_verified_admin(mut self, enabled: bool) -> Self {
        self.verified_actor = enabled.then(|| "admin".into());
        self
    }

    pub fn with_verified_actor(mut self, actor: String) -> Self {
        self.verified_actor = Some(actor);
        self
    }

    /// Who a call acts as: its `author`, else the configured author, else the client's name.
    fn actor(&self, author: Option<String>, ctx: &RequestContext<RoleServer>) -> String {
        if let Some(actor) = &self.verified_actor {
            return actor.clone();
        }
        author
            .filter(|a| !a.trim().is_empty())
            .or_else(|| self.author.clone())
            .or_else(|| {
                let info = ctx.peer.peer_info()?;
                let name = info.client_info.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
            .unwrap_or_else(|| "agent".into())
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PathArg {
    /// Absolute path such as /notes/todo.md
    path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ListArg {
    /// Folder to list; defaults to the root, /.
    path: Option<String>,
    /// How many levels to descend. 1 (default) lists direct children; up to 10 lists everything below, with full paths.
    depth: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct MkdirArg {
    /// Absolute path such as /projects/new; missing parents are created too.
    path: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadArg {
    /// Absolute path such as /notes/todo.md
    path: String,
    /// Specific revision to read; defaults to the head revision.
    revision: Option<i64>,
    /// "text" (default) or "base64". Binary files such as PDFs, Word or Excel documents and images must be read as base64.
    encoding: Option<String>,
    /// First line to return, 1-based. Use with limit to page through large text files.
    offset: Option<usize>,
    /// Number of lines to return from offset. Large files are cut at 2000 lines unless a range is given.
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct EditArg {
    /// Absolute path of an existing text file.
    path: String,
    /// Exact text to replace, copied from the file including whitespace and indentation. Must appear once unless replace_all is true.
    old_string: String,
    /// Replacement text. Use an empty string to delete old_string.
    new_string: String,
    /// Replace every occurrence instead of requiring exactly one.
    replace_all: Option<bool>,
    /// Why this change was made (stored with the revision).
    comment: Option<String>,
    author: Option<String>,
    /// Optional precondition: only edit while the head revision is N.
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DiffArg {
    /// Absolute path of a text file.
    path: String,
    /// Older revision; defaults to the one before to_revision. 0 means an empty file.
    from_revision: Option<i64>,
    /// Newer revision; defaults to the head revision.
    to_revision: Option<i64>,
    /// Lines of unchanged context around each change (default 3).
    context: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RecentArg {
    /// How many files to return (default 20, at most 200).
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArg {
    /// Absolute path; parent folders are created automatically.
    path: String,
    /// Full new content of the file: plain text, or base64 when encoding is "base64".
    content: String,
    /// "text" (default) or "base64". Use base64 to store binary files such as PDF, docx, xlsx or images.
    encoding: Option<String>,
    /// Why this change was made (stored with the revision).
    comment: Option<String>,
    /// Who is writing, e.g. the agent's name. Defaults to "agent". Also the name checked against sharing grants.
    author: Option<String>,
    /// Custom key/value properties stored with this revision; values are strings, e.g. {"source": "web"}.
    properties: Option<BTreeMap<String, String>>,
    /// Optional precondition: 0 = only if no live file is here; N = only if the head revision is N. On failure nothing changes, so re-read and merge.
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DeleteArg {
    /// Absolute path of the file or folder to move to the trash.
    path: String,
    /// Optional precondition on the head revision, as for drawer_write.
    if_head_revision: Option<i64>,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RollbackArg {
    /// Absolute path of the file.
    path: String,
    /// Revision whose content becomes the new head revision.
    revision: i64,
    author: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CopyArg {
    /// Absolute path to copy from.
    from: String,
    /// Absolute path to copy to. May equal `from`, which adds a revision with the same content.
    to: String,
    /// Revision of `from` to copy; defaults to the head revision.
    source_revision: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct MoveArg {
    /// Current absolute path.
    from: String,
    /// New absolute path. Renames if only the last segment differs; moves otherwise.
    to: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StarArg {
    /// Absolute path of the file or folder.
    path: String,
    /// true to star, false to unstar.
    starred: bool,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct IdArg {
    /// Node id, as shown by drawer_trash or drawer_stat.
    id: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ShareArg {
    /// Absolute path of the file or folder.
    path: String,
    /// Who gets access: an agent name, or "anyone".
    principal: String,
    /// reader, writer or owner.
    role: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct UnshareArg {
    /// Absolute path of the file or folder.
    path: String,
    principal: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RememberArg {
    /// Note name, e.g. "Project Drawer" or "people/Alice". An existing note with that name is updated.
    name: String,
    /// Markdown body. Link other notes with [[Note Name]] and tag with #tag.
    content: String,
    /// Tags to attach (without #).
    tags: Option<Vec<String>>,
    /// true adds the content to the end of an existing note instead of replacing it.
    append: Option<bool>,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RecallArg {
    /// Case-insensitive text matched against note names and bodies. Omit to list recent notes.
    query: Option<String>,
    /// Only notes with this tag (#project also matches #project/sub).
    tag: Option<String>,
    /// Most results to return (default 20).
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NoteArg {
    /// Note name or folder/name, as written in a [[link]].
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArg {
    /// Case-insensitive text matched against file paths and file contents.
    query: String,
    /// Most results to return (default 20).
    limit: Option<usize>,
}

/// Tool input schemas in the subset every major harness and model API accepts.
///
/// schemars emits `"type": ["string", "null"]` for optional fields, `format` hints and a
/// `$schema` key. OpenAI accepts them, but Gemini (and harnesses that forward schemas to it)
/// rejects type arrays and unknown keywords. Optional fields are already optional because they
/// are absent from `required`, so dropping `null` loses nothing.
fn portable_schema(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                match k.as_str() {
                    "$schema" | "format" | "additionalProperties" | "default" => continue,
                    "type" => {
                        let ty = match v {
                            Value::Array(types) => {
                                let mut types: Vec<Value> =
                                    types.into_iter().filter(|t| t != "null").collect();
                                if types.len() == 1 {
                                    types.remove(0)
                                } else {
                                    Value::Array(types)
                                }
                            }
                            other => other,
                        };
                        out.insert(k, ty);
                    }
                    _ => {
                        out.insert(k, portable_schema(v));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(portable_schema).collect()),
        other => other,
    }
}

/// A display title plus hints that let harnesses auto-approve safe calls and confirm risky ones.
fn annotations_for(name: &str) -> (&'static str, ToolAnnotations) {
    let read = |title| {
        (
            title,
            ToolAnnotations::new().open_world(false).read_only(true),
        )
    };
    let change = |title, destructive, idempotent| {
        (
            title,
            ToolAnnotations::new()
                .open_world(false)
                .read_only(false)
                .destructive(destructive)
                .idempotent(idempotent),
        )
    };
    match name {
        "drawer_list" => read("List folder"),
        "drawer_read" => read("Read file"),
        "drawer_stat" => read("File info"),
        "drawer_revisions" => read("Revision history"),
        "drawer_diff" => read("Compare revisions"),
        "drawer_trash" => read("List trash"),
        "drawer_starred" => read("List starred"),
        "drawer_recent" => read("Recently changed"),
        "drawer_search" => read("Search files"),
        "memory_recall" => read("Search memories"),
        "memory_read" => read("Read memory"),
        "memory_tags" => read("List memory tags"),
        "memory_graph" => read("Memory link graph"),
        "memory_remember" => change("Save memory", false, false),
        "memory_forget" => change("Delete memory", false, true),
        // gone for good: history included
        "drawer_purge" => change("Delete forever", true, false),
        "drawer_unshare" => change("Remove access", true, true),
        // reversible: trash can be restored, writes and moves keep history
        "drawer_mkdir" => change("Create folder", false, true),
        "drawer_delete" => change("Move to trash", false, true),
        "drawer_restore" => change("Restore from trash", false, true),
        "drawer_star" => change("Star", false, true),
        "drawer_share" => change("Share", false, true),
        "drawer_move" => change("Move or rename", false, true),
        "drawer_write" => change("Write file", false, false),
        "drawer_edit" => change("Edit file", false, false),
        "drawer_rollback" => change("Roll back", false, false),
        "drawer_copy" => change("Copy file", false, false),
        _ => change("Drawer", false, false),
    }
}

/// A node as tool results list it: what an agent needs to act, without hashes and timestamps
/// it rarely uses. `drawer_stat` returns the full node.
#[derive(Serialize)]
struct Entry<'a> {
    path: &'a str,
    kind: &'a str,
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<&'a str>,
    modified_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified_by: Option<&'a str>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    starred: bool,
}

fn entries(nodes: &[Node]) -> Vec<Entry<'_>> {
    nodes
        .iter()
        .map(|n| Entry {
            path: &n.path,
            kind: n.kind,
            id: &n.id,
            revision: n.head_revision,
            size: n.size,
            mime: n.mime.as_deref(),
            modified_at: &n.modified_at,
            modified_by: n.modified_by.as_deref(),
            starred: n.starred,
        })
        .collect()
}

/// Text reads longer than this many lines are cut unless the caller asks for a range.
const MAX_READ_LINES: usize = 2000;
/// Or longer than this many bytes, so one read can't flood a model's context.
const MAX_READ_BYTES: usize = 256 * 1024;

/// The requested window of `text`, and a note saying which lines it holds when that is not
/// the whole file.
fn window(text: &str, offset: Option<usize>, limit: Option<usize>) -> (String, Option<String>) {
    let ranged = offset.is_some() || limit.is_some();
    if !ranged && text.len() <= MAX_READ_BYTES && text.lines().count() <= MAX_READ_LINES {
        return (text.to_string(), None);
    }
    let total = text.lines().count();
    if total == 0 {
        return (String::new(), Some("the file is empty".into()));
    }
    let start = offset.unwrap_or(1).max(1);
    let want = limit.unwrap_or(MAX_READ_LINES).max(1);
    let mut out = String::new();
    let mut last = start - 1;
    for line in text.split_inclusive('\n').skip(start - 1).take(want) {
        if !ranged && out.len() + line.len() > MAX_READ_BYTES && last >= start {
            break;
        }
        out.push_str(line);
        last += 1;
    }
    if last < start {
        return (
            String::new(),
            Some(format!(
                "offset {start} is past the end: the file has {total} lines"
            )),
        );
    }
    let more = if last < total {
        format!("; pass offset={} to continue", last + 1)
    } else {
        String::new()
    };
    (
        out,
        Some(format!("showing lines {start}-{last} of {total}{more}")),
    )
}

/// Images small enough to hand to a vision model directly.
const MAX_INLINE_IMAGE: usize = 5 * 1024 * 1024;

/// Compact JSON: tool results go straight into a model's context, where whitespace costs tokens.
fn json<T: serde::Serialize>(v: T) -> Result<String, String> {
    serde_json::to_string(&v).map_err(|e| e.to_string())
}

fn base64_wanted(encoding: &Option<String>) -> Result<bool, String> {
    match encoding.as_deref() {
        None | Some("text") | Some("utf8") | Some("utf-8") => Ok(false),
        Some("base64") => Ok(true),
        Some(other) => Err(format!(
            "unknown encoding {other:?}; use \"text\" or \"base64\""
        )),
    }
}

#[tool_router]
impl DrawerMcp {
    #[tool(
        description = "List the live files and folders inside a folder (default /). Pass depth > 1 to list everything below it in one call. Each entry has its path, kind, id, revision, size and who changed it last."
    )]
    async fn drawer_list(&self, Parameters(a): Parameters<ListArg>) -> Result<String, String> {
        let path = a
            .path
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or("/");
        let depth = a.depth.unwrap_or(1).clamp(1, 10);
        if depth == 1 {
            return json(entries(
                &self.svc.list(path).await.map_err(|e| e.to_string())?,
            ));
        }
        let (nodes, truncated) = self
            .svc
            .walk(path, depth)
            .await
            .map_err(|e| e.to_string())?;
        if truncated {
            return json(serde_json::json!({ "entries": entries(&nodes), "truncated": true }));
        }
        json(entries(&nodes))
    }

    #[tool(
        description = "Create a folder, including any missing parents. Returns the folder's node."
    )]
    async fn drawer_mkdir(
        &self,
        Parameters(a): Parameters<MkdirArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        json(
            self.svc
                .mkdir(&a.path, &self.actor(a.author, &ctx))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "Read a file (the head revision unless one is given). Text comes back as text followed by a [drawer] line with the revision to pass as if_head_revision when writing. Large text files are cut at 2000 lines; page with offset and limit. PNG, JPEG, GIF and WebP images come back as image content. Pass encoding \"base64\" for other binary files such as PDF, docx or xlsx."
    )]
    async fn drawer_read(
        &self,
        Parameters(a): Parameters<ReadArg>,
    ) -> Result<CallToolResult, String> {
        let (rev, data) = self
            .svc
            .read(Ref::Path(&a.path), a.revision)
            .await
            .map_err(|e| e.to_string())?;
        if base64_wanted(&a.encoding)? {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                BASE64.encode(data),
            )]));
        }
        // Raster images come back as MCP image content, so vision models can look at them.
        let viewable = matches!(
            rev.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        );
        if a.encoding.is_none() && viewable && data.len() <= MAX_INLINE_IMAGE {
            return Ok(CallToolResult::success(vec![
                ContentBlock::text(format!(
                    "{} (revision {}, {}, {} bytes)",
                    a.path, rev.revision, rev.mime, rev.size
                )),
                ContentBlock::image(BASE64.encode(data), rev.mime),
            ]));
        }
        let text = String::from_utf8(data).map_err(|_| {
            format!(
                "{} is binary ({}); read it with encoding \"base64\"",
                a.path, rev.mime
            )
        })?;
        let (body, range) = window(&text, a.offset, a.limit);
        let mut meta = format!(
            "[drawer] {} revision {}, {}, {} bytes",
            a.path, rev.revision, rev.mime, rev.size
        );
        if let Some(range) = range {
            meta.push_str(&format!(", {range}"));
        }
        Ok(CallToolResult::success(vec![
            ContentBlock::text(body),
            ContentBlock::text(meta),
        ]))
    }

    #[tool(
        description = "Create or overwrite a whole file. Every change becomes a new revision. For a small change to an existing text file, drawer_edit is cheaper. Pass encoding \"base64\" to store binary files such as PDF, docx, xlsx or images. Returns new_revision=false when nothing changed."
    )]
    async fn drawer_write(
        &self,
        Parameters(a): Parameters<WriteArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let opts = WriteOpts {
            author: self.actor(a.author, &ctx),
            comment: a.comment.unwrap_or_default(),
            properties: a.properties.unwrap_or_default(),
            if_head_revision: a.if_head_revision,
            required_parent: None,
        };
        let data = if base64_wanted(&a.encoding)? {
            BASE64
                .decode(a.content.trim())
                .map_err(|e| format!("content is not valid base64: {e}"))?
        } else {
            a.content.into_bytes()
        };
        let out = self
            .svc
            .write(&a.path, &data, opts)
            .await
            .map_err(|e| e.to_string())?;
        json(written(&out))
    }

    #[tool(
        description = "Edit a text file by replacing old_string with new_string, saved as one new revision. old_string must match the file exactly and appear once (or set replace_all). Safe with other writers: if the file changes during the edit, nothing is saved and you get an error to retry."
    )]
    async fn drawer_edit(
        &self,
        Parameters(a): Parameters<EditArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let mut opts = WriteOpts::new(self.actor(a.author, &ctx), a.comment.unwrap_or_default());
        opts.if_head_revision = a.if_head_revision;
        let edit = Edit {
            old: a.old_string,
            new: a.new_string,
            replace_all: a.replace_all.unwrap_or(false),
        };
        let out = self
            .svc
            .edit(&a.path, &[edit], opts)
            .await
            .map_err(|e| e.to_string())?;
        json(written(&out))
    }

    #[tool(
        description = "Move a file or folder to the trash. Restore it with drawer_restore, or delete it forever with drawer_purge."
    )]
    async fn drawer_delete(
        &self,
        Parameters(a): Parameters<DeleteArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let node = self
            .svc
            .trash(
                Ref::Path(&a.path),
                a.if_head_revision,
                &self.actor(a.author, &ctx),
            )
            .await
            .map_err(|e| e.to_string())?;
        json(
            serde_json::json!({ "trashed": node.path, "id": node.id, "restore_with": "drawer_restore" }),
        )
    }

    #[tool(
        description = "Show a node's full metadata: id, kind, head revision, hash, size, mime, owner, who changed it last, starred, trashed, properties, timestamps."
    )]
    async fn drawer_stat(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(
            self.svc
                .stat(Ref::Path(&a.path))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "List all revisions of a file, newest first, with author, comment, size and time."
    )]
    async fn drawer_revisions(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(
            self.svc
                .revisions(Ref::Path(&a.path))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "Show what changed in a text file between two revisions, as a unified diff. Defaults to the latest change (head vs the revision before it)."
    )]
    async fn drawer_diff(&self, Parameters(a): Parameters<DiffArg>) -> Result<String, String> {
        let d = self
            .svc
            .diff(
                Ref::Path(&a.path),
                a.from_revision,
                a.to_revision,
                a.context.unwrap_or(3).min(50),
            )
            .await
            .map_err(|e| e.to_string())?;
        let summary = format!(
            "{} revision {} -> {}: +{} -{} lines",
            d.path, d.from, d.to, d.added, d.removed
        );
        Ok(if d.diff.is_empty() {
            format!("{summary} (no changes)")
        } else {
            format!("{summary}\n{}", d.diff)
        })
    }

    #[tool(
        description = "Make an earlier revision the head revision. Adds a new revision with the old content; history is kept."
    )]
    async fn drawer_rollback(
        &self,
        Parameters(a): Parameters<RollbackArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let mut opts = WriteOpts::new(self.actor(a.author, &ctx), "");
        opts.if_head_revision = a.if_head_revision;
        let out = self
            .svc
            .rollback(Ref::Path(&a.path), a.revision, opts)
            .await
            .map_err(|e| e.to_string())?;
        json(written(&out))
    }

    #[tool(
        description = "Copy a file to another path, or to its own path as a new revision. Content is shared, not duplicated."
    )]
    async fn drawer_copy(
        &self,
        Parameters(a): Parameters<CopyArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let mut opts = WriteOpts::new(self.actor(a.author, &ctx), a.comment.unwrap_or_default());
        opts.if_head_revision = a.if_head_revision;
        let out = self
            .svc
            .copy(Ref::Path(&a.from), &a.to, a.source_revision, opts)
            .await
            .map_err(|e| e.to_string())?;
        json(written(&out))
    }

    #[tool(
        description = "Rename or move a file or folder. Keeps its id, history and sharing. Creates missing destination folders."
    )]
    async fn drawer_move(
        &self,
        Parameters(a): Parameters<MoveArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let node = self
            .svc
            .move_to(Ref::Path(&a.from), &a.to, &self.actor(a.author, &ctx))
            .await
            .map_err(|e| e.to_string())?;
        json(&entries(std::slice::from_ref(&node))[0])
    }

    #[tool(description = "Star or unstar a file or folder.")]
    async fn drawer_star(
        &self,
        Parameters(a): Parameters<StarArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let node = self
            .svc
            .stat(Ref::Path(&a.path))
            .await
            .map_err(|e| e.to_string())?;
        let u = NodeUpdate {
            starred: Some(a.starred),
            ..NodeUpdate::default()
        };
        let node = self
            .svc
            .update(&node.id, u, &self.actor(a.author, &ctx))
            .await
            .map_err(|e| e.to_string())?;
        json(serde_json::json!({ "path": node.path, "starred": node.starred }))
    }

    #[tool(
        description = "List the top-most items in the trash, with the ids drawer_restore and drawer_purge take."
    )]
    async fn drawer_trash(&self) -> Result<String, String> {
        json(entries(
            &self.svc.trash_list().await.map_err(|e| e.to_string())?,
        ))
    }

    #[tool(
        description = "Restore a trashed file or folder by id. Its parent folder must not be in the trash."
    )]
    async fn drawer_restore(
        &self,
        Parameters(a): Parameters<IdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let u = NodeUpdate {
            trashed: Some(false),
            ..NodeUpdate::default()
        };
        let node = self
            .svc
            .update(&a.id, u, &self.actor(a.author, &ctx))
            .await
            .map_err(|e| e.to_string())?;
        json(&entries(std::slice::from_ref(&node))[0])
    }

    #[tool(
        description = "Permanently delete a trashed file or folder by id, with its history. This cannot be undone."
    )]
    async fn drawer_purge(
        &self,
        Parameters(a): Parameters<IdArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        self.svc
            .purge(&a.id, &self.actor(a.author, &ctx))
            .await
            .map_err(|e| e.to_string())?;
        Ok("deleted forever".into())
    }

    #[tool(
        description = "Grant a principal a role (reader, writer or owner) on a file or folder. Applies to everything below it."
    )]
    async fn drawer_share(
        &self,
        Parameters(a): Parameters<ShareArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let node = self
            .svc
            .stat(Ref::Path(&a.path))
            .await
            .map_err(|e| e.to_string())?;
        json(
            self.svc
                .share(&node.id, &a.principal, &a.role, &self.actor(a.author, &ctx))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "Remove a principal's grant on a file or folder.")]
    async fn drawer_unshare(
        &self,
        Parameters(a): Parameters<UnshareArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let node = self
            .svc
            .stat(Ref::Path(&a.path))
            .await
            .map_err(|e| e.to_string())?;
        json(
            self.svc
                .unshare(&node.id, &a.principal, &self.actor(a.author, &ctx))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "List starred files and folders.")]
    async fn drawer_starred(&self) -> Result<String, String> {
        json(entries(
            &self.svc.starred().await.map_err(|e| e.to_string())?,
        ))
    }

    #[tool(
        description = "List the most recently changed files across the whole drawer, newest first, with who changed them. Use it to catch up on what other agents or people did."
    )]
    async fn drawer_recent(&self, Parameters(a): Parameters<RecentArg>) -> Result<String, String> {
        let limit = a.limit.unwrap_or(20).clamp(1, 200);
        json(entries(
            &self.svc.recent(limit).await.map_err(|e| e.to_string())?,
        ))
    }

    #[tool(
        description = "Search live file names and text contents. Name matches come first; content matches include the line number and a snippet."
    )]
    async fn drawer_search(&self, Parameters(a): Parameters<SearchArg>) -> Result<String, String> {
        json(
            self.svc
                .search(&a.query, a.limit.unwrap_or(20))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "Save a memory as a Markdown note in the /memory vault (Obsidian-style). Use [[Note Name]] to link related notes and #tags or the tags argument to categorise. An existing note with the same name is replaced, or extended with append=true. Prefer one note per topic and link them."
    )]
    async fn memory_remember(
        &self,
        Parameters(a): Parameters<RememberArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let actor = self.actor(a.author, &ctx);
        let out = crate::vault::save(
            &self.svc,
            &a.name,
            &a.content,
            &a.tags.unwrap_or_default(),
            a.append.unwrap_or(false),
            &actor,
        )
        .await
        .map_err(|e| e.to_string())?;
        json(out)
    }

    #[tool(
        description = "Delete a memory note by name. It moves to the trash, so it can be restored with drawer_restore. Links to it stay in other notes as unresolved links."
    )]
    async fn memory_forget(
        &self,
        Parameters(a): Parameters<NoteArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<String, String> {
        let actor = self.actor(None, &ctx);
        json(
            crate::vault::forget(&self.svc, &a.name, &actor)
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "Find memories by text and/or tag, newest first. Returns names, tags and a matching line. Call this before answering questions that earlier notes might cover; read a hit with memory_read."
    )]
    async fn memory_recall(&self, Parameters(a): Parameters<RecallArg>) -> Result<String, String> {
        json(
            crate::vault::list(
                &self.svc,
                a.tag.as_deref(),
                a.query.as_deref(),
                a.limit.unwrap_or(20),
            )
            .await
            .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "Read a memory note by name, with the notes it links to and the notes that link back to it (backlinks)."
    )]
    async fn memory_read(&self, Parameters(a): Parameters<NoteArg>) -> Result<String, String> {
        json(
            crate::vault::get(&self.svc, &a.name)
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(
        description = "The memory link graph: every note as a node (with its tags and link counts) and every [[link]] as an edge. Notes linked to but not yet written appear as nodes without a path. Use it to see how memories relate and which topics are missing notes."
    )]
    async fn memory_graph(&self) -> Result<String, String> {
        json(
            crate::vault::graph(&self.svc)
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "List every tag used in the memory vault with how many notes use it.")]
    async fn memory_tags(&self) -> Result<String, String> {
        json(
            crate::vault::tags(&self.svc)
                .await
                .map_err(|e| e.to_string())?,
        )
    }
}

/// What a write tool reports: enough to chain the next guarded write.
fn written(out: &crate::service::WriteOutcome) -> Value {
    serde_json::json!({
        "path": out.node.path,
        "id": out.node.id,
        "revision": out.revision,
        "new_revision": out.new_revision,
        "size": out.node.size,
        "mime": out.node.mime,
    })
}

/// Sent to the client on connect; most harnesses add it to the model's context.
const INSTRUCTIONS: &str = "Drawer is versioned file storage shared by agents and people. \
Paths are absolute (/notes/todo.md); parent folders are created on write. Every write adds a \
revision, so nothing is lost: drawer_revisions, drawer_diff and drawer_rollback show and undo \
changes. To change part of a text file, use drawer_edit (exact old_string -> new_string) instead \
of rewriting it. When other agents may edit the same file, read it, then write with \
if_head_revision set to the revision the read reported; on a precondition failure, re-read and \
merge. drawer_recent shows what changed lately, and drawer_list with depth > 1 shows a whole \
tree. Binary files (PDF, Office, images) travel as base64 with encoding \"base64\". Deleting \
moves to the trash; drawer_purge is the only permanent delete. Long-term memory lives in the \
/memory folder as an Obsidian-style vault of Markdown notes: memory_recall searches it by text or \
tag, memory_read opens a note with its links and backlinks, and memory_remember saves one, memory_forget deletes one (to the trash). Link \
related notes with [[Note Name]] and tag with #tag.";

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DrawerMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("drawer", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
