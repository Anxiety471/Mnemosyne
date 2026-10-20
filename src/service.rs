//! The service layer shared by the REST API and the MCP server.
//!
//! The model follows Google Drive. Every file and folder is a node with a stable id and a
//! parent. Names and paths are views over the tree, so rename, move and trash never rewrite
//! other rows. A file's content is a list of immutable revisions. Role grants sit on nodes
//! and apply to everything below them.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions,
    sqlite::SqliteRow,
};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::blob::BlobStore;
use crate::error::{Error, Result};

const MAX_CONTENT_SCAN: i64 = 1024 * 1024;
/// Bytes of a file search holds in memory at once. Matches spanning a boundary are found
/// via the overlap carry in `scan_blob`.
const SEARCH_CHUNK: usize = 64 * 1024;

const SNIPPET_CHARS: usize = 160;
/// Most nodes `walk` returns, so a recursive listing of a huge tree stays bounded.
pub const MAX_WALK: usize = 5000;
const MAX_PROPERTY_ENTRIES: usize = 32;
const MAX_PROPERTY_KEY: usize = 64;
const MAX_PROPERTY_VALUE: usize = 1024;
pub const ROOT_ID: &str = "root";

const READER: u8 = 1;
const WRITER: u8 = 2;
pub(crate) const OWNER: u8 = 3;

pub type Properties = std::collections::BTreeMap<String, String>;

#[derive(Clone)]
pub struct Drawer {
    pub(crate) pool: SqlitePool,
    pub(crate) data_dir: std::path::PathBuf,
    _store_lease: Arc<std::fs::File>,
    pub(crate) quota: Arc<std::sync::atomic::AtomicI64>,
    quota_lock: Arc<Mutex<()>>,
    pub(crate) public_prefix: String,
    blobs: BlobStore,
    pub(crate) transfer_dir: std::path::PathBuf,
    pub(crate) transfer_lock: Arc<Mutex<()>>,
    pub(crate) deposit_lock: Arc<Mutex<()>>,
    // Memory-vault parse cache: note node id -> (head revision, parsed tags/links). A hit
    // means the note's blob was already read and parsed, so per-request vault work is a
    // metadata query plus a re-parse of only the notes whose head revision changed.
    pub(crate) vault_cache:
        Arc<Mutex<std::collections::HashMap<String, (i64, crate::vault::Parsed)>>>,
    // Makes each check-then-write sequence one atomic step. Fine for the single-process server
    // this crate is; several processes sharing a store would need `BEGIN IMMEDIATE`.
    pub(crate) write_lock: Arc<Mutex<()>>,
}

/// How a caller names a node: by path (`/notes/todo.md`) or by its stable id.
#[derive(Debug, Clone, Copy)]
pub enum Ref<'a> {
    Path(&'a str),
    Id(&'a str),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    /// Derived from the parent chain at read time, so it changes when the node moves.
    pub path: String,
    pub kind: &'static str,
    pub owner: String,
    pub head_revision: Option<i64>,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub mime: Option<String>,
    /// Author of the head revision: who last changed the content. `None` for folders.
    pub modified_by: Option<String>,
    pub properties: Properties,
    pub trashed: bool,
    pub starred: bool,
    pub created_at: String,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Revision {
    pub node_id: String,
    pub revision: i64,
    pub hash: String,
    pub size: i64,
    pub mime: String,
    pub author: String,
    pub comment: String,
    pub properties: Properties,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub node: Node,
    pub revision: i64,
    /// False when nothing changed (same content and properties), so no revision was added.
    pub new_revision: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Permission {
    pub principal: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub path: String,
    pub matched: &'static str,
    pub snippet: String,
    /// 1-based line of the snippet, for content matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// A text edit: replace `old` with `new`, once (it must be unique) or everywhere.
#[derive(Debug, Clone)]
pub struct Edit {
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

/// What `Drawer::diff` compared, and the unified diff between the two revisions.
#[derive(Debug, Clone, Serialize)]
pub struct Diff {
    pub path: String,
    pub from: i64,
    pub to: i64,
    pub added: usize,
    pub removed: usize,
    /// Unified diff text; empty when the contents are the same.
    pub diff: String,
}

/// Who is changing a file, why, with what properties, and under which precondition.
#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    pub author: String,
    pub comment: String,
    pub properties: Properties,
    /// `Some(0)`: only create when there is no live file here.
    /// `Some(n)`: only change the file while its head revision is `n`.
    pub if_head_revision: Option<i64>,
    /// Require the destination parent to retain this identity at commit time.
    pub required_parent: Option<String>,
}

impl WriteOpts {
    pub fn new(author: impl Into<String>, comment: impl Into<String>) -> Self {
        Self {
            author: author.into(),
            comment: comment.into(),
            ..Self::default()
        }
    }
}

/// Counts for `drawer doctor` and the like.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stats {
    pub files: i64,
    pub folders: i64,
    /// Top-most trashed nodes, the same things the trash lists.
    pub trashed: i64,
    pub revisions: i64,
}

/// Fields that `Drawer::update` changes. `None` leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct NodeUpdate {
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub starred: Option<bool>,
    pub trashed: Option<bool>,
}

/// Normalize a user path to `/a/b/c` form. Rejects `..` and control characters.
pub fn normalize(path: &str) -> Result<String> {
    let mut parts = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(Error::Invalid("'..' is not allowed in paths".into())),
            s if s.chars().any(|c| c.is_control()) => {
                return Err(Error::Invalid("control characters in path".into()));
            }
            s => parts.push(s),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn segments(norm: &str) -> Vec<&str> {
    norm.split('/').filter(|s| !s.is_empty()).collect()
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.chars().any(|c| c.is_control())
    {
        return Err(Error::Invalid(format!("bad name {name:?}")));
    }
    Ok(())
}

/// Keys are header-safe tokens; values are bounded text.
pub fn validate_properties(m: &Properties) -> Result<()> {
    if m.len() > MAX_PROPERTY_ENTRIES {
        return Err(Error::Invalid(format!(
            "at most {MAX_PROPERTY_ENTRIES} properties"
        )));
    }
    for (k, v) in m {
        let key_ok = !k.is_empty()
            && k.len() <= MAX_PROPERTY_KEY
            && k.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if !key_ok {
            return Err(Error::Invalid(format!(
                "bad property key {k:?}: use 1-64 of [A-Za-z0-9-_.]"
            )));
        }
        if v.len() > MAX_PROPERTY_VALUE || v.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid(format!(
                "property {k:?} must be at most {MAX_PROPERTY_VALUE} bytes, no control characters"
            )));
        }
    }
    Ok(())
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

/// `/a/b/c` → `["/a", "/a/b", "/a/b/c"]`. Every path the destination parent would need.
fn ancestor_paths(path: &str) -> Vec<String> {
    segments(path)
        .iter()
        .scan(String::new(), |acc, seg| {
            *acc = child_path(if acc.is_empty() { "/" } else { acc }, seg);
            Some(acc.clone())
        })
        .collect()
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

/// Text-like media types whose content search can scan.
fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "application/yaml" | "application/toml"
        )
}

/// Byte offset of the first case-insensitive match of `q` in `text`, or `None`. Compares
/// character-by-character so multi-byte characters and expansions like `İ` are handled
/// without lowering (and re-mapping) the whole buffer.
fn find_match(text: &str, q: &str) -> Option<usize> {
    if q.is_empty() {
        return None;
    }
    let qchars: Vec<char> = q.chars().collect();
    // Walk candidate start positions in char space; `text_chars` is indexed in parallel.
    let text_chars: Vec<char> = text.chars().collect();
    let last_start = text_chars.len().saturating_sub(1);
    for start in 0..=last_start {
        // Try to match the whole query beginning at `start`, character by character, where
        // each text character's full lowercase fold must equal the next query character(s).
        let mut qi = 0;
        let mut ti = start;
        while ti < text_chars.len() && qi < qchars.len() {
            let folded: Vec<char> = text_chars[ti].to_lowercase().collect();
            if folded.is_empty() {
                break;
            }
            // The fold must consume one or more query chars exactly.
            if qi + folded.len() > qchars.len() {
                break;
            }
            if qchars[qi..qi + folded.len()] != folded[..] {
                break;
            }
            qi += folded.len();
            ti += 1;
        }
        if qi == qchars.len() {
            // Matched: return the byte offset of `start`.
            return Some(
                text.char_indices()
                    .nth(start)
                    .map(|(b, _)| b)
                    .unwrap_or(text.len()),
            );
        }
    }
    None
}

/// A line trimmed to at most `SNIPPET_CHARS` characters, keeping the match in view.
fn snippet(line: &str, lowered_query: &str) -> String {
    let line = line.trim();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return line.to_string();
    }
    // Lowercasing can change lengths for a few scripts; the estimate only places the window.
    let at = line
        .to_lowercase()
        .find(lowered_query)
        .map_or(0, |b| line.to_lowercase()[..b].chars().count());
    let start = at
        .saturating_sub(SNIPPET_CHARS / 3)
        .min(chars.len() - SNIPPET_CHARS);
    let end = start + SNIPPET_CHARS;
    let mut out: String = chars[start..end].iter().collect();
    if start > 0 {
        out.insert(0, '…');
    }
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// Media type from the file extension, falling back to a UTF-8 check for unknown extensions.
fn guess_mime(path: &str, data: &[u8]) -> String {
    let name = name_of(path).to_ascii_lowercase();
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => name.as_str(), // Dockerfile, Makefile, ...
    };
    let mime = match ext {
        // documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        // text and data
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        // images, audio, video
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        // archives
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        _ if std::str::from_utf8(data).is_ok() => "text/plain",
        _ => "application/octet-stream",
    };
    // Source files (.rs, .py, .ts, ...) fall through to text/plain; the UI picks a highlighter from the extension.
    mime.into()
}

/// One edit applied to `text`, or why it can't be. Agents usually send `\n` line endings, so
/// a file with `\r\n` endings is matched with those too.
fn apply_edit(text: &str, e: &Edit) -> std::result::Result<String, String> {
    if e.old.is_empty() {
        return Err("old text is empty".into());
    }
    let crlf = |s: &str| s.replace("\r\n", "\n").replace('\n', "\r\n");
    let (old, new) = if !text.contains(&e.old) && text.contains("\r\n") && e.old.contains('\n') {
        (crlf(&e.old), crlf(&e.new))
    } else {
        (e.old.clone(), e.new.clone())
    };
    match text.matches(&old).count() {
        0 => Err(
            "old text not found; re-read the file and copy it exactly, including whitespace".into(),
        ),
        1 => Ok(text.replacen(&old, &new, 1)),
        _ if e.replace_all => Ok(text.replace(&old, &new)),
        n => Err(format!(
            "old text appears {n} times; include more surrounding text to make it unique, or set replace_all"
        )),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn rank(role: &str) -> u8 {
    match role {
        "reader" => READER,
        "writer" => WRITER,
        "owner" => OWNER,
        _ => 0,
    }
}

const NODE_SELECT: &str = "SELECT n.id, n.parent_id, n.name, n.kind, n.owner, n.head_revision, n.trashed, \
    n.starred, n.created_at, n.modified_at, r.hash, r.size, r.mime, r.author, r.properties \
    FROM nodes n LEFT JOIN revisions r ON r.node_id = n.id AND r.revision = n.head_revision";

const REVISION_SELECT: &str = "SELECT node_id, revision, hash, size, mime, author, comment, properties, created_at FROM revisions";

fn node_from_row(r: &SqliteRow, path: String) -> Node {
    let props: Option<String> = r.get("properties");
    Node {
        id: r.get("id"),
        parent_id: r.get("parent_id"),
        name: r.get("name"),
        path,
        kind: if r.get::<String, _>("kind") == "folder" {
            "folder"
        } else {
            "file"
        },
        owner: r.get("owner"),
        head_revision: r.get("head_revision"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        modified_by: r.get("author"),
        properties: parse_props(props.as_deref(), &r.get::<String, _>("id")),
        trashed: r.get::<i64, _>("trashed") != 0,
        starred: r.get::<i64, _>("starred") != 0,
        created_at: r.get("created_at"),
        modified_at: r.get("modified_at"),
    }
}

fn revision_from_row(r: &SqliteRow) -> Revision {
    let props: String = r.get("properties");
    Revision {
        node_id: r.get("node_id"),
        revision: r.get("revision"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        author: r.get("author"),
        comment: r.get("comment"),
        properties: parse_props(Some(&props), &r.get::<String, _>("node_id")),
        created_at: r.get("created_at"),
    }
}

/// Stored properties are user-controlled JSON. A corrupt value (hand-edited row, partial
/// write) must not take down every read: log it and fall back to empty.
fn parse_props(raw: Option<&str>, id: &str) -> Properties {
    match raw {
        Some(p) => match serde_json::from_str(p) {
            Ok(props) => props,
            Err(e) => {
                tracing::warn!(node = id, error = %e, properties = %p, "ignoring unreadable properties");
                Properties::new()
            }
        },
        None => Properties::new(),
    }
}

/// `if_head_revision` against the live head (`None` when there is no live file).
fn check_precondition(path: &str, live: Option<i64>, expected: Option<i64>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let ok = match live {
        None => expected == 0,
        Some(cur) => expected == cur,
    };
    if ok {
        return Ok(());
    }
    Err(Error::PreconditionFailed(match live {
        Some(cur) => format!("{path} is at revision {cur}, not {expected}"),
        None => format!("{path} does not exist, expected revision {expected}"),
    }))
}

/// Where a write lands: an existing live node, a trashed file it revives, or a new path.
enum Slot {
    Live(String),
    Revive(String),
    Create {
        anchor: String,
        missing: Vec<String>,
    },
}

async fn columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(pool)
        .await?)
}

async fn insert_node(
    tx: &mut Transaction<'_, Sqlite>,
    parent: &str,
    name: &str,
    kind: &str,
    owner: &str,
    ts: &str,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO nodes (id, parent_id, name, kind, owner, trashed, starred, created_at, modified_at) \
         VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?)",
    )
    .bind(&id)
    .bind(parent)
    .bind(name)
    .bind(kind)
    .bind(owner)
    .bind(ts)
    .bind(ts)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Bring a store on the pre-revision schema (`files`, `folders`, `versions` keyed by path) into
/// the node model. Runs once: the legacy tables are dropped in the same transaction.
async fn migrate_legacy(pool: &SqlitePool) -> Result<()> {
    if columns(pool, "files").await?.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let mut ids: HashMap<String, String> = HashMap::from([("/".to_string(), ROOT_ID.to_string())]);

    let folders = sqlx::query("SELECT path, created_at FROM folders ORDER BY length(path)")
        .fetch_all(&mut *tx)
        .await?;
    for r in folders {
        let path: String = r.get("path");
        let created: String = r.get("created_at");
        let parent = ids
            .get(&parent_of(&path))
            .cloned()
            .ok_or_else(|| Error::Internal(format!("legacy folder {path} has no parent")))?;
        let id = insert_node(
            &mut tx,
            &parent,
            name_of(&path),
            "folder",
            "migrated",
            &created,
        )
        .await?;
        ids.insert(path, id);
    }

    let mut file_nodes: HashMap<i64, String> = HashMap::new();
    let files = sqlx::query(
        "SELECT id, path, current_version, deleted, created_at, updated_at FROM files ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for r in files {
        let path: String = r.get("path");
        let parent = ids
            .get(&parent_of(&path))
            .cloned()
            .ok_or_else(|| Error::Internal(format!("legacy file {path} has no parent folder")))?;
        let created: String = r.get("created_at");
        let id = insert_node(
            &mut tx,
            &parent,
            name_of(&path),
            "file",
            "migrated",
            &created,
        )
        .await?;
        sqlx::query(
            "UPDATE nodes SET head_revision = ?, trashed = ?, modified_at = ? WHERE id = ?",
        )
        .bind(r.get::<i64, _>("current_version"))
        .bind(r.get::<i64, _>("deleted"))
        .bind(r.get::<String, _>("updated_at"))
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        file_nodes.insert(r.get("id"), id);
    }

    let versions = sqlx::query("SELECT file_id, version, hash, size, mime, author, comment, created_at FROM versions ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    for r in versions {
        let node_id = file_nodes
            .get(&r.get::<i64, _>("file_id"))
            .ok_or_else(|| Error::Internal("legacy version without a file".into()))?;
        sqlx::query(
            "INSERT INTO revisions (node_id, revision, hash, size, mime, author, comment, properties, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, '{}', ?)",
        )
        .bind(node_id)
        .bind(r.get::<i64, _>("version"))
        .bind(r.get::<String, _>("hash"))
        .bind(r.get::<i64, _>("size"))
        .bind(r.get::<String, _>("mime"))
        .bind(r.get::<String, _>("author"))
        .bind(r.get::<String, _>("comment"))
        .bind(r.get::<String, _>("created_at"))
        .execute(&mut *tx)
        .await?;
    }

    for table in ["versions", "files", "folders"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE {table}")))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

impl Drawer {
    /// Open (or create) a store rooted at `data_dir`: `drawer.db` plus `blobs/`.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        Self::open_leased(data_dir, false).await
    }

    /// Offline maintenance excludes all current Drawer processes using this store.
    pub async fn open_maintenance(data_dir: &Path) -> Result<Self> {
        Self::open_leased(data_dir, true).await
    }

    async fn open_leased(data_dir: &Path, exclusive: bool) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir).await?;
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data_dir.join(".maintenance.lock"))?;
        let locked = if exclusive {
            fs2::FileExt::try_lock_exclusive(&lease)
        } else {
            fs2::FileExt::try_lock_shared(&lease)
        };
        locked.map_err(|_| {
            Error::Conflict("store is in use; stop Drawer before offline maintenance".into())
        })?;
        let opts = SqliteConnectOptions::new()
            .filename(data_dir.join("drawer.db"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // WAL makes NORMAL durable against crashes of this process; only an OS crash can
            // lose the last transactions. Waiting on a lock beats failing with SQLITE_BUSY when
            // the CLI or a second server touches the same store.
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(10))
            .pragma("temp_store", "memory");
        // Schema and migrations run on a single connection that is closed before the shared
        // pool opens, so no pooled connection ever holds a plan from the old schema.
        let setup = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&setup)
            .await?;
        migrate_legacy(&setup).await?;
        setup.close().await;
        let pool = SqlitePool::connect_with(opts).await?;
        Ok(Self {
            pool,
            data_dir: data_dir.to_owned(),
            _store_lease: Arc::new(lease),
            quota: Arc::new(std::sync::atomic::AtomicI64::new(i64::MAX)),
            quota_lock: Arc::new(Mutex::new(())),
            public_prefix: String::new(),
            blobs: BlobStore::new(data_dir.join("blobs")),
            transfer_dir: data_dir.join("transfers"),
            transfer_lock: Arc::new(Mutex::new(())),
            deposit_lock: Arc::new(Mutex::new(())),
            vault_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) async fn check_quota(&self, additional: i64) -> Result<()> {
        let quota = self.quota.load(std::sync::atomic::Ordering::Acquire);
        if quota == i64::MAX {
            return Ok(());
        }
        let used: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size),0) FROM revisions")
            .fetch_one(&self.pool)
            .await?;
        if used.saturating_add(additional) > quota {
            return Err(Error::Forbidden(
                "storage quota exceeded; history and trash count toward quota".into(),
            ));
        }
        Ok(())
    }

    // ----- lookups -------------------------------------------------------------------------

    /// Id and kind of a child by name. `trashed` picks which side of the trash to look at.
    async fn child(
        &self,
        parent: &str,
        name: &str,
        trashed: bool,
    ) -> Result<Option<(String, String)>> {
        Ok(sqlx::query(
            "SELECT id, kind FROM nodes WHERE parent_id = ? AND name = ? AND trashed = ?",
        )
        .bind(parent)
        .bind(name)
        .bind(trashed as i64)
        .fetch_optional(&self.pool)
        .await?
        .map(|r| (r.get("id"), r.get("kind"))))
    }

    async fn path_of(&self, id: &str) -> Result<String> {
        self.paths_of(&[id.to_string()])
            .await?
            .remove(id)
            .ok_or_else(|| Error::NotFound(format!("node {id}")))
    }

    /// Paths of many nodes in one query, by walking each up to the root.
    async fn paths_of(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "WITH RECURSIVE up(start, cur, path) AS (\
               SELECT n.id, n.parent_id, n.name FROM nodes n WHERE n.id IN (SELECT value FROM json_each(?)) \
               UNION ALL \
               SELECT up.start, n.parent_id, n.name || '/' || up.path FROM up JOIN nodes n ON n.id = up.cur \
               WHERE n.parent_id IS NOT NULL) \
             SELECT start, path FROM up WHERE cur IS NULL OR cur = ?",
        )
        .bind(serde_json::to_string(ids).expect("strings serialize"))
        .bind(ROOT_ID)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let start: String = r.get("start");
                // The root's own row has an empty name and no parent.
                let path = if start == ROOT_ID {
                    "/".to_string()
                } else {
                    format!("/{}", r.get::<String, _>("path"))
                };
                (start, path)
            })
            .collect())
    }

    /// Nodes from rows of `NODE_SELECT`, with their paths filled in by one query.
    async fn nodes_with_paths(&self, rows: &[SqliteRow]) -> Result<Vec<Node>> {
        let ids: Vec<String> = rows.iter().map(|r| r.get("id")).collect();
        let mut paths = self.paths_of(&ids).await?;
        Ok(rows
            .iter()
            .zip(ids)
            .map(|(r, id)| node_from_row(r, paths.remove(&id).unwrap_or_default()))
            .collect())
    }

    /// A node by id, trashed or not.
    async fn node(&self, id: &str, path: Option<String>) -> Result<Node> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!("{NODE_SELECT} WHERE n.id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound(format!("node {id}")))?;
        let path = match path {
            Some(p) => p,
            None => self.path_of(id).await?,
        };
        Ok(node_from_row(&r, path))
    }

    /// A live node by path, or `None`.
    async fn lookup(&self, p: &str) -> Result<Option<Node>> {
        let norm = normalize(p)?;
        let mut id = ROOT_ID.to_string();
        for seg in segments(&norm) {
            match self.child(&id, seg, false).await? {
                Some((cid, _)) => id = cid,
                None => return Ok(None),
            }
        }
        Ok(Some(self.node(&id, Some(norm)).await?))
    }

    async fn resolve(&self, r: Ref<'_>) -> Result<Node> {
        match r {
            Ref::Id(id) => self.node(id, None).await,
            Ref::Path(p) => self
                .lookup(p)
                .await?
                .ok_or_else(|| Error::NotFound(format!("no live file or folder at {p}"))),
        }
    }

    /// Decide where a write to `norm` lands, walking live folders from the root.
    async fn slot(&self, norm: &str) -> Result<Slot> {
        let segs = segments(norm);
        let mut anchor = ROOT_ID.to_string();
        for (i, seg) in segs.iter().enumerate() {
            let last = i + 1 == segs.len();
            match self.child(&anchor, seg, false).await? {
                Some((cid, kind)) => {
                    if last {
                        return Ok(Slot::Live(cid));
                    }
                    if kind == "file" {
                        return Err(Error::Conflict(format!(
                            "/{} is a file",
                            segs[..=i].join("/")
                        )));
                    }
                    anchor = cid;
                }
                None => {
                    let trashed = if last {
                        self.child(&anchor, seg, true).await?
                    } else {
                        None
                    };
                    if let Some((tid, kind)) = trashed
                        && kind == "file"
                    {
                        return Ok(Slot::Revive(tid));
                    }
                    return Ok(Slot::Create {
                        anchor,
                        missing: segs[i..].iter().map(|s| s.to_string()).collect(),
                    });
                }
            }
        }
        Ok(Slot::Live(ROOT_ID.to_string()))
    }

    /// True when `ancestor` is `id` itself or one of its ancestors.
    async fn is_ancestor_or_self(&self, ancestor: &str, id: &str) -> Result<bool> {
        Ok(sqlx::query(
            "WITH RECURSIVE anc(id) AS (SELECT ? UNION ALL SELECT n.parent_id FROM nodes n JOIN anc ON n.id = anc.id \
             WHERE n.parent_id IS NOT NULL) SELECT 1 FROM anc WHERE id = ?",
        )
        .bind(id)
        .bind(ancestor)
        .fetch_optional(&self.pool)
        .await?
        .is_some())
    }

    /// Role check: grants on the node or any ancestor apply. A node with no grants anywhere is
    /// open, and its owner always passes. This is cooperative: callers name themselves.
    pub(crate) async fn authorize(&self, id: &str, actor: &str, need: u8) -> Result<()> {
        let rows = sqlx::query(
            "WITH RECURSIVE anc(id) AS (SELECT ? UNION ALL SELECT n.parent_id FROM nodes n JOIN anc ON n.id = anc.id \
             WHERE n.parent_id IS NOT NULL) \
             SELECT p.principal, p.role FROM permissions p JOIN anc ON p.node_id = anc.id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let owner: String = sqlx::query_scalar("SELECT owner FROM nodes WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        if owner == actor {
            return Ok(());
        }
        let best = rows
            .iter()
            .filter(|r| {
                let p: String = r.get("principal");
                p == actor || p == "anyone"
            })
            .map(|r| rank(&r.get::<String, _>("role")))
            .max()
            .unwrap_or(0);
        if best >= need {
            return Ok(());
        }
        let needed = if need == OWNER { "owner" } else { "writer" };
        Err(Error::Forbidden(format!(
            "{actor} needs {needed} access to this node"
        )))
    }

    /// Result of SQLite's `PRAGMA quick_check`: `ok` for a healthy database.
    pub async fn integrity_check(&self) -> Result<String> {
        Ok(sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn stats(&self) -> Result<Stats> {
        let row = sqlx::query(
            "SELECT \
                (SELECT COUNT(*) FROM nodes WHERE kind = 'file' AND trashed = 0) AS files, \
                (SELECT COUNT(*) FROM nodes WHERE kind = 'folder' AND trashed = 0 AND id <> 'root') AS folders, \
                (SELECT COUNT(*) FROM nodes WHERE trashed = 1 AND parent_id IN (SELECT id FROM nodes WHERE trashed = 0)) AS trashed, \
                (SELECT COUNT(*) FROM revisions) AS revisions",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(Stats {
            files: row.get("files"),
            folders: row.get("folders"),
            trashed: row.get("trashed"),
            revisions: row.get("revisions"),
        })
    }

    /// Blob hashes that a revision points at but that are missing from disk.
    pub async fn missing_blobs(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT DISTINCT hash FROM revisions ORDER BY hash")
            .fetch_all(&self.pool)
            .await?;
        let mut missing = Vec::new();
        for r in rows {
            let hash: String = r.get("hash");
            if !self.blobs.contains(&hash).await? {
                missing.push(hash);
            }
        }
        Ok(missing)
    }

    // ----- reads ---------------------------------------------------------------------------

    /// Live children of a folder, by name.
    pub async fn list(&self, path: &str) -> Result<Vec<Node>> {
        let folder = self
            .lookup(path)
            .await?
            .filter(|n| n.kind == "folder")
            .ok_or_else(|| Error::NotFound(format!("folder {path}")))?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.parent_id = ? AND n.trashed = 0 ORDER BY n.name"
        )))
        .bind(&folder.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let name: String = r.get("name");
                node_from_row(r, child_path(&folder.path, &name))
            })
            .collect())
    }

    pub async fn stat(&self, r: Ref<'_>) -> Result<Node> {
        self.resolve(r).await
    }

    /// One revision of a file (the head by default) and its content.
    pub async fn read(&self, r: Ref<'_>, revision: Option<i64>) -> Result<(Revision, Vec<u8>)> {
        let node = self.resolve(r).await?;
        let rev = self.revision_row(&node.id, revision).await?;
        let data = self.blobs.get(&rev.hash).await?;
        Ok((rev, data))
    }

    /// Resolve a file revision to its content-addressed on-disk path for streamed HTTP downloads.
    pub async fn read_file_path(
        &self,
        r: Ref<'_>,
        revision: Option<i64>,
    ) -> Result<(Revision, std::path::PathBuf)> {
        let node = self.resolve(r).await?;
        let rev = self.revision_row(&node.id, revision).await?;
        let path = self.blobs.path_for(&rev.hash)?;
        if !tokio::fs::try_exists(&path).await? {
            return Err(Error::Internal(format!("blob {} missing", rev.hash)));
        }
        Ok((rev, path))
    }

    async fn revision_row(&self, node_id: &str, revision: Option<i64>) -> Result<Revision> {
        let node = self.node(node_id, None).await?;
        let want = match revision {
            Some(n) => n,
            None => node
                .head_revision
                .ok_or_else(|| Error::Invalid(format!("{} is a folder", node.path)))?,
        };
        let r = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{REVISION_SELECT} WHERE node_id = ? AND revision = ?"
        )))
        .bind(node_id)
        .bind(want)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("{} revision {want}", node.path)))?;
        Ok(revision_from_row(&r))
    }

    /// All revisions of a file, newest first.
    pub async fn revisions(&self, r: Ref<'_>) -> Result<Vec<Revision>> {
        let node = self.resolve(r).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{REVISION_SELECT} WHERE node_id = ? ORDER BY revision DESC"
        )))
        .bind(&node.id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(revision_from_row).collect())
    }

    /// Top-most trashed nodes: what the trash shows. Their children come with them.
    pub async fn trash_list(&self) -> Result<Vec<Node>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.trashed = 1 AND n.parent_id IN (SELECT id FROM nodes WHERE trashed = 0) ORDER BY n.name"
        )))
        .fetch_all(&self.pool)
        .await?;
        self.nodes_with_paths(&rows).await
    }

    pub async fn starred(&self) -> Result<Vec<Node>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.starred = 1 AND n.trashed = 0 ORDER BY n.name"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut nodes = self.nodes_with_paths(&rows).await?;
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(nodes)
    }

    /// Live files, most recently changed first: what agents and people have been working on.
    pub async fn recent(&self, limit: usize) -> Result<Vec<Node>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.kind = 'file' AND n.trashed = 0 ORDER BY n.modified_at DESC, n.name LIMIT ?"
        )))
        .bind(limit.clamp(1, 1000) as i64)
        .fetch_all(&self.pool)
        .await?;
        self.nodes_with_paths(&rows).await
    }

    /// Everything under a folder, depth first in path order, down to `max_depth` levels
    /// (1 is the folder's own children). Returns at most `MAX_WALK` nodes, and whether the
    /// listing was cut short.
    pub async fn walk(&self, path: &str, max_depth: usize) -> Result<(Vec<Node>, bool)> {
        let folder = self
            .lookup(path)
            .await?
            .filter(|n| n.kind == "folder")
            .ok_or_else(|| Error::NotFound(format!("folder {path}")))?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            // `sort` joins names with char(1), which sorts below every allowed name character,
            // so a folder's children come right after it.
            "WITH RECURSIVE sub(id, depth, rel, sort) AS (\
               SELECT id, 1, name, name FROM nodes WHERE parent_id = ? AND trashed = 0 \
               UNION ALL \
               SELECT n.id, sub.depth + 1, sub.rel || '/' || n.name, sub.sort || char(1) || n.name \
               FROM nodes n JOIN sub ON n.parent_id = sub.id WHERE n.trashed = 0 AND sub.depth < ?) \
             {} JOIN sub ON sub.id = n.id ORDER BY sub.sort LIMIT ?",
            NODE_SELECT.replacen(" FROM nodes n", ", sub.rel FROM nodes n", 1)
        )))
        .bind(&folder.id)
        .bind(max_depth.max(1) as i64)
        .bind(MAX_WALK as i64 + 1)
        .fetch_all(&self.pool)
        .await?;
        let truncated = rows.len() > MAX_WALK;
        let base = if folder.path == "/" {
            String::new()
        } else {
            folder.path.clone()
        };
        let nodes = rows
            .iter()
            .take(MAX_WALK)
            .map(|r| node_from_row(r, format!("{base}/{}", r.get::<String, _>("rel"))))
            .collect();
        Ok((nodes, truncated))
    }

    pub async fn permissions(&self, id: &str) -> Result<Vec<Permission>> {
        let rows = sqlx::query(
            "SELECT principal, role FROM permissions WHERE node_id = ? ORDER BY principal",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| Permission {
                principal: r.get("principal"),
                role: r.get("role"),
            })
            .collect())
    }

    /// Case-insensitive search over live file paths and the text of their head revisions.
    /// Name matches come first, then content matches, each in path order.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::Invalid("empty query".into()));
        }
        let limit = limit.clamp(1, 500);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.kind = 'file' AND n.trashed = 0 AND n.head_revision IS NOT NULL"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut files = self.nodes_with_paths(&rows).await?;
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let (named, rest): (Vec<Node>, Vec<Node>) = files
            .into_iter()
            .partition(|f| f.path.to_lowercase().contains(&q));
        let mut hits: Vec<SearchHit> = named
            .into_iter()
            .take(limit)
            .map(|f| SearchHit {
                id: f.id,
                path: f.path,
                matched: "name",
                snippet: String::new(),
                line: None,
            })
            .collect();
        for f in rest {
            if hits.len() >= limit {
                break;
            }
            let (Some(size), Some(mime), Some(hash)) =
                (f.size, f.mime.as_deref(), f.hash.as_deref())
            else {
                continue;
            };
            if size > MAX_CONTENT_SCAN || !is_text(mime) {
                continue;
            }
            if let Some((line, text)) = self.scan_blob(hash, &q).await? {
                hits.push(SearchHit {
                    id: f.id,
                    path: f.path,
                    matched: "content",
                    snippet: snippet(&text, &q),
                    line: Some(line),
                });
            }
        }
        Ok(hits)
    }

    /// Find the first case-insensitive match of `q` in a blob, streaming it in fixed-size
    /// chunks so a large file is never held in memory. Returns the 1-based line number and the
    /// original line text containing the match.
    async fn scan_blob(&self, hash: &str, q: &str) -> Result<Option<(usize, String)>> {
        use tokio::io::AsyncReadExt;
        let mut reader = self.blobs.stream_chunks(hash, SEARCH_CHUNK).await?;
        // `buf` holds the not-yet-ruled-out tail of the file. `lines_before` counts complete
        // lines fully before `buf`, so a match's line number is exact.
        let mut buf: Vec<u8> = Vec::new();
        let mut lines_before: usize = 0;
        let mut chunk = vec![0u8; SEARCH_CHUNK];
        loop {
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            // Never split a multi-byte character: keep any incomplete tail for next round.
            let good = match std::str::from_utf8(&chunk[..n]) {
                Ok(_) => n,
                Err(e) => e.valid_up_to(),
            };
            // `good` may be less than `n`; the remainder is carried by re-reading is not
            // possible, so it is kept here and prepended to the next chunk's work by virtue of
            // staying in `buf` only if it can still match — which it cannot be dropped below.
            buf.extend_from_slice(&chunk[..good]);
            let text = match std::str::from_utf8(&buf) {
                Ok(t) => t.to_string(),
                Err(_) => return Ok(None), // invalid UTF-8: not searchable text
            };
            if let Some(m) = find_match(&text, q) {
                let line_start = text[..m].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_no = lines_before + text[..m].matches('\n').count() + 1;
                // The line may run past what we have buffered; read to its end.
                let line_end = text[m..].find('\n').map(|i| m + i).unwrap_or(text.len());
                let mut line_bytes = text.as_bytes()[line_start..line_end].to_vec();
                if line_end == text.len() {
                    let mut tail = Vec::new();
                    let mut rest = [0u8; 8192];
                    loop {
                        let k = reader.read(&mut rest).await?;
                        if k == 0 {
                            break;
                        }
                        if let Some(pos) = rest[..k].iter().position(|&b| b == b'\n') {
                            tail.extend_from_slice(&rest[..pos]);
                            break;
                        }
                        tail.extend_from_slice(&rest[..k]);
                    }
                    line_bytes.extend_from_slice(&tail);
                }
                let line_text = String::from_utf8_lossy(&line_bytes).into_owned();
                return Ok(Some((line_no, line_text)));
            }
            // No match: drop everything that cannot still begin a match. Keep the last
            // `q.len() - 1` bytes (the overlap across the chunk boundary) and never cut a
            // line, so line numbers and the snippet stay exact.
            let carry = q.len().saturating_sub(1);
            if buf.len() > carry {
                let mut drop = buf.len() - carry;
                while drop > 0 && buf[drop - 1] != b'\n' {
                    drop -= 1;
                }
                if drop > 0 {
                    let dropped: String = String::from_utf8_lossy(&buf[..drop]).into_owned();
                    lines_before += dropped.matches('\n').count();
                    buf.drain(..drop);
                }
            }
        }
        Ok(None)
    }

    // ----- writes --------------------------------------------------------------------------

    /// Create a folder and any missing parents. Existing folders are returned as they are.
    pub async fn mkdir(&self, path: &str, actor: &str) -> Result<Node> {
        let _guard = self.write_lock.lock().await;
        let norm = normalize(path)?;
        let mut id = ROOT_ID.to_string();
        let mut prefix = String::new();
        let mut checked = false;
        for seg in segments(&norm) {
            prefix = child_path(if prefix.is_empty() { "/" } else { &prefix }, seg);
            match self.child(&id, seg, false).await? {
                Some((cid, kind)) => {
                    if kind == "file" {
                        return Err(Error::Conflict(format!("{prefix} is a file")));
                    }
                    id = cid;
                }
                None => {
                    if !checked {
                        self.authorize(&id, actor, WRITER).await?;
                        checked = true;
                    }
                    let mut tx = self.pool.begin().await?;
                    id = insert_node(&mut tx, &id, seg, "folder", actor, &now()).await?;
                    tx.commit().await?;
                }
            }
        }
        self.node(&id, Some(norm)).await
    }

    /// Store `data` as the next revision of `path`, unless it is refused or nothing changed.
    pub async fn write(&self, path: &str, data: &[u8], opts: WriteOpts) -> Result<WriteOutcome> {
        let norm = normalize(path)?;
        if norm == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        validate_properties(&opts.properties)?;
        let _quota = self.quota_lock.lock().await;
        if let Err(error) = self.check_quota(data.len() as i64).await
            && !self.blobs.contains(&BlobStore::hash(data)).await?
        {
            return Err(error);
        }
        let hash = self.blobs.put(data).await?;
        let mime = guess_mime(&norm, data);
        self.commit_blob(
            &norm,
            &hash,
            data.len() as i64,
            &mime,
            &opts.properties,
            &opts,
        )
        .await
    }

    /// Commit a staged transfer without loading the full file into memory.
    pub async fn write_staged(
        &self,
        path: &str,
        staged: &Path,
        opts: WriteOpts,
        expected_hash: Option<&str>,
    ) -> Result<WriteOutcome> {
        let norm = normalize(path)?;
        if norm == "/" {
            return Err(Error::Invalid("cannot write to root".into()));
        }
        validate_properties(&opts.properties)?;
        let _quota = self.quota_lock.lock().await;
        // Mirror `write()`: bytes already in the store don't count against the quota, so
        // re-uploading identical content never fails. Hash first, then charge what's new.
        let (hash, size, created) = self.blobs.put_file_checked(staged, expected_hash).await?;
        if expected_hash.is_some_and(|expected| expected != hash) {
            return Err(Error::Invalid(
                "uploaded file SHA-256 checksum mismatch".into(),
            ));
        }
        // The blob now exists, so `contains` is always true; use `created` to decide.
        if created {
            self.check_quota(size).await?;
        }
        let mut file = tokio::fs::File::open(staged).await?;
        use tokio::io::AsyncReadExt;
        let mut sample = vec![0u8; 8192];
        let n = file.read(&mut sample).await?;
        sample.truncate(n);
        let mime = guess_mime(&norm, &sample);
        self.commit_blob(&norm, &hash, size, &mime, &opts.properties, &opts)
            .await
    }

    // Roll back newly unreferenced bytes on failed named-user writes. The quota guard
    // serializes blob writers; metadata is rechecked against copies under write_lock.
    async fn commit_blob(
        &self,
        norm: &str,
        hash: &str,
        size: i64,
        mime: &str,
        properties: &Properties,
        opts: &WriteOpts,
    ) -> Result<WriteOutcome> {
        let result = self.commit(norm, hash, size, mime, properties, opts).await;
        if result.is_err() && self.quota.load(std::sync::atomic::Ordering::Acquire) != i64::MAX {
            let _guard = self.write_lock.lock().await;
            let referenced: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM revisions WHERE hash=?)")
                    .bind(hash)
                    .fetch_one(&self.pool)
                    .await?;
            if !referenced {
                let _ = tokio::fs::remove_file(self.blobs.path_for(hash)?).await;
            }
        }
        result
    }

    /// Core write path, shared by `write`, `copy` and `rollback`. The blob must already exist.
    async fn commit(
        &self,
        norm: &str,
        hash: &str,
        size: i64,
        mime: &str,
        properties: &Properties,
        opts: &WriteOpts,
    ) -> Result<WriteOutcome> {
        let _guard = self.write_lock.lock().await;
        if let Some(parent) = &opts.required_parent {
            let path = norm
                .rsplit_once('/')
                .map(|(p, _)| if p.is_empty() { "/" } else { p })
                .unwrap_or("/");
            if self.stat(Ref::Path(path)).await?.id != *parent {
                return Err(Error::PreconditionFailed(
                    "destination folder changed".into(),
                ));
            }
        }
        let slot = self.slot(norm).await?;

        // Who we check, and what the precondition sees as the live head.
        let (auth_id, live_head) = match &slot {
            Slot::Live(id) => {
                let n = self.node(id, None).await?;
                if n.kind == "folder" {
                    return Err(Error::Conflict(format!("{norm} is a folder")));
                }
                (id.clone(), n.head_revision)
            }
            Slot::Revive(id) => (id.clone(), None),
            Slot::Create { anchor, .. } => (anchor.clone(), None),
        };
        self.authorize(&auth_id, &opts.author, WRITER).await?;
        check_precondition(norm, live_head, opts.if_head_revision)?;

        let props_json = serde_json::to_string(properties).expect("string map serializes");
        if let Slot::Live(id) = &slot {
            let head = live_head.ok_or_else(|| {
                Error::Internal(format!("node {id} is live but has no head revision"))
            })?;
            let same = sqlx::query(
                "SELECT hash, properties FROM revisions WHERE node_id = ? AND revision = ?",
            )
            .bind(id)
            .bind(head)
            .fetch_one(&self.pool)
            .await?;
            if same.get::<String, _>("hash") == hash
                && same.get::<String, _>("properties") == props_json
            {
                let node = self.node(id, Some(norm.to_string())).await?;
                return Ok(WriteOutcome {
                    node,
                    revision: head,
                    new_revision: false,
                });
            }
        }

        self.check_quota(size).await?;
        let ts = now();
        let mut tx = self.pool.begin().await?;
        let node_id = match &slot {
            Slot::Live(id) => id.clone(),
            Slot::Revive(id) => {
                sqlx::query("UPDATE nodes SET trashed = 0, modified_at = ? WHERE id = ?")
                    .bind(&ts)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                id.clone()
            }
            Slot::Create { anchor, missing } => {
                let mut parent = anchor.clone();
                for dir in &missing[..missing.len() - 1] {
                    parent =
                        insert_node(&mut tx, &parent, dir, "folder", &opts.author, &ts).await?;
                }
                insert_node(
                    &mut tx,
                    &parent,
                    &missing[missing.len() - 1],
                    "file",
                    &opts.author,
                    &ts,
                )
                .await?
            }
        };
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM revisions WHERE node_id = ?",
        )
        .bind(&node_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO revisions (node_id, revision, hash, size, mime, author, comment, properties, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&node_id)
        .bind(next)
        .bind(hash)
        .bind(size)
        .bind(mime)
        .bind(&opts.author)
        .bind(&opts.comment)
        .bind(&props_json)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE nodes SET head_revision = ?, modified_at = ? WHERE id = ?")
            .bind(next)
            .bind(&ts)
            .bind(&node_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let node = self.node(&node_id, Some(norm.to_string())).await?;
        Ok(WriteOutcome {
            node,
            revision: next,
            new_revision: true,
        })
    }

    /// Change part of a text file: each edit replaces `old` with `new`, in order. The result is
    /// written as one new revision, guarded on the head revision that was edited, so a write
    /// that lands in between makes this fail instead of being overwritten. Properties carry over
    /// from the head unless `opts` sets some.
    pub async fn edit(
        &self,
        path: &str,
        edits: &[Edit],
        mut opts: WriteOpts,
    ) -> Result<WriteOutcome> {
        if edits.is_empty() {
            return Err(Error::Invalid("no edits given".into()));
        }
        let node = self.resolve(Ref::Path(path)).await?;
        let head = node
            .head_revision
            .ok_or_else(|| Error::Invalid(format!("{} is a folder", node.path)))?;
        check_precondition(&node.path, Some(head), opts.if_head_revision)?;
        let (rev, data) = self.read(Ref::Id(&node.id), Some(head)).await?;
        let mut text = String::from_utf8(data).map_err(|_| {
            Error::Invalid(format!(
                "{} is binary ({}); only text files can be edited",
                node.path, rev.mime
            ))
        })?;
        for (i, e) in edits.iter().enumerate() {
            text = apply_edit(&text, e).map_err(|msg| {
                let which = if edits.len() > 1 {
                    format!("edit {}: ", i + 1)
                } else {
                    String::new()
                };
                Error::Invalid(format!("{which}{msg} in {} (revision {head})", node.path))
            })?;
        }
        if opts.properties.is_empty() {
            opts.properties = rev.properties;
        }
        opts.if_head_revision = Some(head);
        self.write(&node.path, text.as_bytes(), opts).await
    }

    /// Unified diff between two revisions of a text file. `to` defaults to the head and
    /// `from` to the revision before `to`; revision 0 stands for an empty file.
    pub async fn diff(
        &self,
        r: Ref<'_>,
        from: Option<i64>,
        to: Option<i64>,
        context: usize,
    ) -> Result<Diff> {
        let node = self.resolve(r).await?;
        let head = node
            .head_revision
            .ok_or_else(|| Error::Invalid(format!("{} is a folder", node.path)))?;
        let to = to.unwrap_or(head);
        let from = from.unwrap_or(to - 1).max(0);
        let text_of = |rev: i64| {
            let node = &node;
            async move {
                if rev == 0 {
                    return Ok(String::new());
                }
                let (meta, data) = self.read(Ref::Id(&node.id), Some(rev)).await?;
                String::from_utf8(data).map_err(|_| {
                    Error::Invalid(format!(
                        "{} revision {rev} is binary ({}); only text can be diffed",
                        node.path, meta.mime
                    ))
                })
            }
        };
        let (old, new) = (text_of(from).await?, text_of(to).await?);
        let td = similar::TextDiff::from_lines(&old, &new);
        let (mut added, mut removed) = (0, 0);
        for change in td.iter_all_changes() {
            match change.tag() {
                similar::ChangeTag::Insert => added += 1,
                similar::ChangeTag::Delete => removed += 1,
                similar::ChangeTag::Equal => {}
            }
        }
        let diff = if added + removed == 0 {
            String::new()
        } else {
            td.unified_diff()
                .context_radius(context)
                .header(
                    &format!("{}@{from}", node.path),
                    &format!("{}@{to}", node.path),
                )
                .to_string()
        };
        Ok(Diff {
            path: node.path,
            from,
            to,
            added,
            removed,
            diff,
        })
    }

    /// Copy one revision of a file to `to`, which may be the file's own path (a new revision
    /// with the same content). Content is shared by hash. The source properties carry over.
    pub async fn copy(
        &self,
        from: Ref<'_>,
        to: &str,
        source_revision: Option<i64>,
        opts: WriteOpts,
    ) -> Result<WriteOutcome> {
        let src = self.resolve(from).await?;
        if src.kind == "folder" {
            return Err(Error::Invalid(
                "copying folders is not supported yet".into(),
            ));
        }
        let to = normalize(to)?;
        if to == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        let rev = self.revision_row(&src.id, source_revision).await?;
        self.commit(&to, &rev.hash, rev.size, &rev.mime, &rev.properties, &opts)
            .await
    }

    /// Roll back by making an old revision the newest one. History is never rewritten.
    pub async fn rollback(
        &self,
        r: Ref<'_>,
        revision: i64,
        mut opts: WriteOpts,
    ) -> Result<WriteOutcome> {
        let node = self.resolve(r).await?;
        if node.trashed {
            return Err(Error::NotFound(format!("{} is in the trash", node.path)));
        }
        if opts.comment.is_empty() {
            opts.comment = format!("rollback to revision {revision}");
        }
        self.copy(Ref::Id(&node.id), &node.path, Some(revision), opts)
            .await
    }

    /// Rename, move, star, trash or restore a node. Trashing a folder trashes everything under
    /// it, and restoring one restores everything under it.
    pub async fn update(&self, id: &str, u: NodeUpdate, actor: &str) -> Result<Node> {
        if id == ROOT_ID {
            return Err(Error::Invalid("the root cannot be changed".into()));
        }
        let _guard = self.write_lock.lock().await;
        let cur = self.node(id, None).await?;
        self.authorize(id, actor, WRITER).await?;
        if let Some(name) = &u.name {
            validate_name(name)?;
        }
        let new_parent = u
            .parent_id
            .clone()
            .or_else(|| cur.parent_id.clone())
            .expect("non-root nodes have a parent");
        if u.parent_id.is_some() {
            self.authorize(&new_parent, actor, WRITER).await?;
            let dest = self.node(&new_parent, None).await?;
            if dest.kind != "folder" || dest.trashed {
                return Err(Error::Invalid("destination must be a live folder".into()));
            }
            if self.is_ancestor_or_self(id, &new_parent).await? {
                return Err(Error::Invalid("cannot move a folder into itself".into()));
            }
        }
        let new_name = u.name.clone().unwrap_or_else(|| cur.name.clone());
        let new_trashed = u.trashed.unwrap_or(cur.trashed);
        if u.trashed == Some(false) && cur.trashed && self.node(&new_parent, None).await?.trashed {
            return Err(Error::Conflict("restore the parent folder first".into()));
        }
        if !new_trashed
            && let Some((clash, _)) = self.child(&new_parent, &new_name, false).await?
            && clash != id
        {
            return Err(Error::Conflict(format!("{new_name} already exists there")));
        }

        let ts = now();
        let mut tx = self.pool.begin().await?;
        if new_trashed != cur.trashed {
            sqlx::query(
                "UPDATE nodes SET trashed = ?, modified_at = ? WHERE id IN (\
                 WITH RECURSIVE sub(id) AS (SELECT ? UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id) \
                 SELECT id FROM sub)",
            )
            .bind(new_trashed as i64)
            .bind(&ts)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE nodes SET parent_id = ?, name = ?, starred = ?, modified_at = ? WHERE id = ?",
        )
        .bind(&new_parent)
        .bind(&new_name)
        .bind(u.starred.unwrap_or(cur.starred) as i64)
        .bind(&ts)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.node(id, None).await
    }

    /// Trash a file or folder, with an optional precondition on its head revision.
    pub async fn trash(
        &self,
        r: Ref<'_>,
        if_head_revision: Option<i64>,
        actor: &str,
    ) -> Result<Node> {
        let node = self.resolve(r).await?;
        check_precondition(&node.path, node.head_revision, if_head_revision)?;
        self.update(
            &node.id,
            NodeUpdate {
                trashed: Some(true),
                ..NodeUpdate::default()
            },
            actor,
        )
        .await
    }

    /// Rename or move a live node to `to`, creating the destination's parent folders.
    pub async fn move_to(&self, from: Ref<'_>, to: &str, actor: &str) -> Result<Node> {
        let node = self.resolve(from).await?;
        let norm = normalize(to)?;
        if norm == "/" {
            return Err(Error::Invalid("cannot move to the root".into()));
        }
        // A rename in place changes nothing.
        if norm == node.path && name_of(&norm) == node.name {
            return self.node(&node.id, Some(norm)).await;
        }
        // Validate before any folder is created: a refused move must leave no trace.
        let name = name_of(&norm);
        validate_name(name)?;
        let parent_path = parent_of(&norm);
        // A file sitting in the way of a new destination parent, e.g. /x/f when moving
        // /y to /x/f/g.txt.
        for ancestor in ancestor_paths(&parent_path) {
            if let Some(n) = self.lookup(&ancestor).await?
                && n.kind == "file"
            {
                return Err(Error::Conflict(format!("{ancestor} is a file")));
            }
        }
        // Check the clash before creating folders when the parent already exists.
        if let Some(p) = self.lookup(&parent_path).await?
            && p.kind == "folder"
            && self
                .child(&p.id, name, false)
                .await?
                .is_some_and(|(cid, _)| cid != node.id)
        {
            return Err(Error::Conflict(format!("{name} already exists there")));
        }
        // The parent must exist (creating it happens below, after these checks) and the
        // mover must be able to write the destination parent: update() re-checks it too, but
        // only after mkdir() may have created folders.
        let parent = match self.lookup(&parent_path).await? {
            None => self.mkdir(&parent_path, actor).await?,
            Some(p) if p.kind == "file" => {
                return Err(Error::Conflict(format!("{parent_path} is a file")));
            }
            Some(p) => p,
        };
        if node.id == parent.id || self.is_ancestor_or_self(&node.id, &parent.id).await? {
            return Err(Error::Invalid("cannot move a folder into itself".into()));
        }
        if node.trashed {
            return Err(Error::NotFound(format!("{} is in the trash", node.path)));
        }
        // update() checks this too, but before folders are created.
        self.authorize(&node.id, actor, WRITER).await?;
        self.authorize(&parent.id, actor, WRITER).await?;
        if self
            .child(&parent.id, name, false)
            .await?
            .is_some_and(|(cid, _)| cid != node.id)
        {
            return Err(Error::Conflict(format!("{name} already exists there")));
        }
        self.update(
            &node.id,
            NodeUpdate {
                parent_id: Some(parent.id),
                name: Some(name.to_string()),
                ..NodeUpdate::default()
            },
            actor,
        )
        .await
    }

    /// Permanently delete a trashed node and everything under it, with its revisions and grants.
    /// Blobs stay on disk until a garbage collector removes unreferenced ones.
    pub async fn purge(&self, id: &str, actor: &str) -> Result<()> {
        if id == ROOT_ID {
            return Err(Error::Invalid("the root cannot be deleted".into()));
        }
        let _guard = self.write_lock.lock().await;
        let node = self.node(id, None).await?;
        if !node.trashed {
            return Err(Error::Conflict(format!(
                "move {} to the trash before deleting it forever",
                node.path
            )));
        }
        self.authorize(id, actor, OWNER).await?;
        let subtree = "(WITH RECURSIVE sub(id) AS (SELECT ? UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id) SELECT id FROM sub)";
        let mut tx = self.pool.begin().await?;
        for table in ["revisions", "permissions"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE node_id IN {subtree}"
            )))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM nodes WHERE id IN {subtree}"
        )))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replace the grant for `principal` on a node. Roles: reader, writer, owner.
    pub async fn share(
        &self,
        id: &str,
        principal: &str,
        role: &str,
        actor: &str,
    ) -> Result<Vec<Permission>> {
        validate_name(principal)?;
        if rank(role) == 0 {
            return Err(Error::Invalid(format!(
                "role must be reader, writer or owner, not {role:?}"
            )));
        }
        let _guard = self.write_lock.lock().await;
        self.authorize(id, actor, OWNER).await?;
        sqlx::query(
            "INSERT INTO permissions (node_id, principal, role) VALUES (?, ?, ?) \
             ON CONFLICT (node_id, principal) DO UPDATE SET role = excluded.role",
        )
        .bind(id)
        .bind(principal)
        .bind(role)
        .execute(&self.pool)
        .await?;
        self.permissions(id).await
    }

    pub async fn unshare(&self, id: &str, principal: &str, actor: &str) -> Result<Vec<Permission>> {
        let _guard = self.write_lock.lock().await;
        self.authorize(id, actor, OWNER).await?;
        let n = sqlx::query("DELETE FROM permissions WHERE node_id = ? AND principal = ?")
            .bind(id)
            .bind(principal)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!(
                "no grant for {principal} on this node"
            )));
        }
        self.permissions(id).await
    }
}
