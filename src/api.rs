//! REST API under `/drawer/v1`.
//!
//! Nodes can be named by `path` or by stable `id`. Mutations name their actor in the
//! `x-drawer-author` header (`api` by default).
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::error::{Error, Result};
use crate::service::{Drawer, Edit, NodeUpdate, Properties, Ref, WriteOpts, normalize};
use crate::update;

/// Custom properties travel as `x-drawer-prop-<key>` headers, like GCS's `x-goog-meta-*`.
const PROP_PREFIX: &str = "x-drawer-prop-";

#[derive(Deserialize)]
struct PathQ {
    path: Option<String>,
    id: Option<String>,
    revision: Option<i64>,
    if_head_revision: Option<i64>,
    /// `true` serves the file as an attachment instead of inline.
    download: Option<bool>,
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

#[derive(Deserialize)]
struct TreeQ {
    path: Option<String>,
    /// Levels to list; 1 (default) is direct children.
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct LimitQ {
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct DiffQ {
    path: Option<String>,
    id: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    context: Option<usize>,
}

#[derive(Deserialize)]
struct EditBody {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct RollbackBody {
    path: Option<String>,
    id: Option<String>,
    revision: i64,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct CopyBody {
    from: Option<String>,
    from_id: Option<String>,
    to: String,
    source_revision: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct MoveBody {
    from: Option<String>,
    from_id: Option<String>,
    to: String,
}

#[derive(Deserialize)]
struct UpdateBody {
    name: Option<String>,
    parent_id: Option<String>,
    starred: Option<bool>,
    trashed: Option<bool>,
}

#[derive(Deserialize)]
struct ShareBody {
    principal: String,
    role: String,
}

#[derive(Deserialize)]
struct LinkBody {
    max_downloads: Option<i64>,
    expires_in_days: Option<i64>,
    revision: Option<i64>,
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
    limit: Option<usize>,
}

/// Liveness probe, served at `/health` and `/drawer/v1/health`.
pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

/// Run blocking release work (curl, tar, file swaps) off the async runtime.
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
        .map_err(|e| Error::Upstream(format!("{e:#}")))
}

/// Is a newer release published than the one this server runs?
pub async fn update_check() -> Result<Json<update::Check>> {
    let status = blocking(|| update::check(&update::Config::from_env())).await?;
    Ok(Json(status))
}

/// Install the latest release if it is newer. The running server keeps its old code until restarted.
pub async fn update_install() -> Result<Json<Value>> {
    let (status, updated) = blocking(|| {
        let cfg = update::Config::from_env();
        let status = update::check(&cfg)?;
        if status.update_available {
            let exe = std::env::current_exe()?.canonicalize()?;
            update::install(&cfg, &status.latest, &exe)?;
        }
        let updated = status.update_available;
        Ok((status, updated))
    })
    .await?;
    Ok(Json(json!({
        "current": status.current,
        "latest": status.latest,
        "updated": updated,
        "restart_required": updated,
    })))
}

pub fn router() -> Router<Drawer> {
    Router::new()
        .route("/health", get(health))
        .route(
            "/users",
            get(crate::accounts::list).post(crate::accounts::create),
        )
        .route("/users/{id}", axum::routing::patch(crate::accounts::change))
        .route("/tree", get(tree))
        .route("/archive", get(crate::archive::download))
        .route("/storage", get(crate::storage::stats))
        .route("/activity", get(crate::activity::list))
        .route("/mounts", get(crate::mounts::list))
        .route("/mounts/{id}/entries", get(crate::mounts::browse))
        .route("/mounts/{id}/import", post(crate::mounts::import))
        .route("/recent", get(recent))
        .route("/update", get(update_check).post(update_install))
        .route("/diff", get(diff))
        .route("/edit", post(edit))
        .route("/folders", post(mkdir).delete(delete_file))
        .route("/files", get(read_file).put(write_file).delete(delete_file))
        .route(
            "/transfers",
            get(crate::transfers::list).post(crate::transfers::start),
        )
        .route("/transfers/batch", post(crate::transfers::start_batch))
        .route(
            "/transfers/batch/complete",
            post(crate::transfers::complete_batch),
        )
        .route(
            "/transfers/{id}",
            get(crate::transfers::status)
                .patch(crate::transfers::change)
                .put(crate::transfers::append)
                .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
                .delete(crate::transfers::cancel),
        )
        .route("/transfers/{id}/complete", post(crate::transfers::complete))
        .route("/files/meta", get(meta))
        .route("/revisions", get(revisions))
        .route("/rollback", post(rollback))
        .route("/copy", post(copy))
        .route("/move", post(move_to))
        .route(
            "/nodes/{id}",
            axum::routing::patch(update_node).delete(purge_node),
        )
        .route(
            "/nodes/{id}/upload-links",
            get(crate::deposits::list).post(crate::deposits::create),
        )
        .route(
            "/nodes/{id}/upload-links/{link}",
            axum::routing::delete(crate::deposits::revoke),
        )
        .route("/nodes/{id}/links", get(list_links).post(create_link))
        .route(
            "/nodes/{id}/links/{link_id}",
            axum::routing::delete(revoke_link),
        )
        .route("/nodes/{id}/permissions", get(list_permissions).put(share))
        .route(
            "/nodes/{id}/permissions/{principal}",
            axum::routing::delete(unshare),
        )
        .route("/trash", get(trash))
        .route("/starred", get(starred))
        .route("/search", get(search))
        .route("/memory/notes", get(crate::vault::list_notes))
        .route(
            "/memory/note",
            get(crate::vault::read_note)
                .put(crate::vault::save_note)
                .delete(crate::vault::delete_note),
        )
        .route("/memory/tags", get(crate::vault::list_tags))
        .route("/memory/graph", get(crate::vault::read_graph))
}

/// Exactly one of `id` or `path` must name the node.
fn pick<'a>(id: &'a Option<String>, path: &'a Option<String>) -> Result<Ref<'a>> {
    match (id, path) {
        (Some(id), _) => Ok(Ref::Id(id)),
        (None, Some(p)) => Ok(Ref::Path(p)),
        _ => Err(Error::Invalid("name the node with `id` or `path`".into())),
    }
}

fn target(q: &PathQ) -> Result<Ref<'_>> {
    pick(&q.id, &q.path)
}

/// RFC 5987 percent-encoding for `filename*=`.
fn encode_filename(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Headers are percent-encoded text: RFC 3986 triplets. `encoding: percent` says the sender
/// encoded a comment that is not valid ASCII (e.g. UTF-8 in `x-drawer-comment`).
fn header_text(h: &HeaderMap, name: &str, default: &str) -> Result<String> {
    let raw = h
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(default)
        .to_string();
    let encoded = h
        .get("x-drawer-comment-encoding")
        .and_then(|v| v.to_str().ok())
        == Some("percent");
    if !encoded {
        return Ok(raw);
    }
    percent_decode(&raw)
}

fn percent_decode(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        return Err(Error::Invalid(format!(
                            "{hex:?} is not a percent triplet in {input}"
                        )));
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| Error::Invalid(format!("{input} is not valid UTF-8")))
}

fn header_str(h: &HeaderMap, name: &str, default: &str) -> String {
    h.get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(default)
        .to_string()
}

fn actor(h: &HeaderMap) -> String {
    header_str(h, "x-drawer-author", "api")
}

fn out<T: Serialize>(v: T) -> Json<Value> {
    match serde_json::to_value(v) {
        Ok(value) => Json(value),
        Err(e) => Json(json!({ "error": format!("{e}") })),
    }
}

/// Author, comment, `x-drawer-prop-*` headers and the precondition become write options.
fn write_opts(h: &HeaderMap, if_head_revision: Option<i64>) -> Result<WriteOpts> {
    let mut properties = Properties::new();
    for (name, value) in h.iter() {
        if let Some(key) = name.as_str().strip_prefix(PROP_PREFIX) {
            let value = value
                .to_str()
                .map_err(|_| Error::Invalid(format!("{name} must be ASCII")))?;
            // Header names are case-insensitive, so keys are stored lowercase.
            properties.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
    Ok(WriteOpts {
        author: actor(h),
        comment: header_text(h, "x-drawer-comment", "")?,
        properties,
        if_head_revision,
        required_parent: None,
    })
}

async fn tree(State(s): State<Drawer>, Query(q): Query<TreeQ>) -> Result<Json<Value>> {
    let path = q.path.as_deref().unwrap_or("/");
    match q.depth.unwrap_or(1).clamp(1, 32) {
        1 => Ok(Json(
            json!({ "path": normalize(path)?, "entries": s.list(path).await? }),
        )),
        depth => {
            let (entries, truncated) = s.walk(path, depth).await?;
            Ok(Json(
                json!({ "path": normalize(path)?, "entries": entries, "truncated": truncated }),
            ))
        }
    }
}

async fn recent(State(s): State<Drawer>, Query(q): Query<LimitQ>) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "nodes": s.recent(q.limit.unwrap_or(50)).await? }),
    ))
}

async fn diff(State(s): State<Drawer>, Query(q): Query<DiffQ>) -> Result<Json<Value>> {
    Ok(out(s
        .diff(
            pick(&q.id, &q.path)?,
            q.from,
            q.to,
            q.context.unwrap_or(3).min(1000),
        )
        .await?))
}

/// Replace `old_string` with `new_string` in a text file, as one guarded revision.
async fn edit(
    State(s): State<Drawer>,
    verified: Option<axum::Extension<crate::auth::VerifiedActor>>,
    headers: HeaderMap,
    Json(b): Json<EditBody>,
) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(
        body_actor(verified.is_some(), b.author, &headers),
        b.comment.unwrap_or_default(),
    );
    opts.if_head_revision = b.if_head_revision;
    let e = Edit {
        old: b.old_string,
        new: b.new_string,
        replace_all: b.replace_all,
    };
    Ok(out(s.edit(&b.path, &[e], opts).await?))
}

async fn mkdir(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<PathBody>,
) -> Result<Json<Value>> {
    Ok(out(s.mkdir(&b.path, &actor(&headers)).await?))
}

/// Parse one RFC 7233 bytes range. Multi-ranges are intentionally unsupported.
fn one_range(input: &str, total: u64) -> Option<(u64, u64)> {
    let (a, b) = input.strip_prefix("bytes=")?.split_once('-')?;
    if b.contains(',') || total == 0 {
        return None;
    }
    if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((total.saturating_sub(suffix), total - 1));
    }
    let start: u64 = a.parse().ok()?;
    let end: u64 = if b.is_empty() {
        total - 1
    } else {
        b.parse::<u64>().ok()?.min(total - 1)
    };
    (start <= end && start < total).then_some((start, end))
}

async fn read_file(
    State(s): State<Drawer>,
    Query(q): Query<PathQ>,
    req: HeaderMap,
) -> Result<Response> {
    let name = s.stat(target(&q)?).await?.name;
    let (rev, file_path) = s.read_file_path(target(&q)?, q.revision).await?;
    // Content is addressed by hash, so the hash is a perfect validator: a client that already
    // has these bytes gets a 304 and no body.
    let etag = format!("\"{}\"", rev.hash);
    if req
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag))
    {
        let mut resp = axum::http::StatusCode::NOT_MODIFIED.into_response();
        resp.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag).expect("hex is ascii"),
        );
        resp.headers_mut()
            .insert("x-drawer-revision", HeaderValue::from(rev.revision));
        return Ok(resp);
    }
    let total = rev.size.max(0) as u64;
    let range = if let Some(value) = req.get(header::RANGE) {
        match value.to_str().ok().and_then(|s| one_range(s, total)) {
            Some(r) => Some(r),
            None => {
                let mut response = axum::http::StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{total}")).expect("numeric header"),
                );
                return Ok(response);
            }
        }
    } else {
        None
    };
    let mut file = tokio::fs::File::open(file_path).await?;
    let (start, end) = range.unwrap_or((0, total.saturating_sub(1)));
    if start != 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let count = if total == 0 { 0 } else { end - start + 1 };
    if q.download.unwrap_or(false) {
        let node = s.stat(target(&q)?).await?;
        s.record_download(&actor(&req), &node.path, rev.size)
            .await?;
    }
    let mut resp = Body::from_stream(ReaderStream::new(file.take(count))).into_response();
    if range.is_some() {
        *resp.status_mut() = axum::http::StatusCode::PARTIAL_CONTENT;
    }
    let h = resp.headers_mut();
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if range.is_some() {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")).expect("numeric header"),
        );
    }
    if let Ok(v) = HeaderValue::from_str(&rev.mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    let disposition = if q.download.unwrap_or(false) {
        "attachment"
    } else {
        "inline"
    };
    let value = format!("{disposition}; filename*=UTF-8''{}", encode_filename(&name));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&value).expect("percent-encoded is ascii"),
    );
    // Stored files are untrusted: never sniff them, and run HTML or SVG opened directly in a
    // sandbox with no scripts and an opaque origin. PDFs are exempt so the browser viewer works.
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if rev.mime != "application/pdf" {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "sandbox; default-src 'none'; img-src data:; style-src 'unsafe-inline'",
            ),
        );
    }
    h.insert("x-drawer-revision", HeaderValue::from(rev.revision));
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("hex is ascii"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(
        "x-drawer-hash",
        HeaderValue::from_str(&rev.hash).expect("hex is ascii"),
    );
    for (k, v) in &rev.properties {
        let name = HeaderName::from_bytes(format!("{PROP_PREFIX}{k}").as_bytes());
        let value = HeaderValue::from_str(v);
        if let (Ok(name), Ok(value)) = (name, value) {
            h.insert(name, value);
        }
    }
    Ok(resp)
}

async fn write_file(
    State(s): State<Drawer>,
    Query(q): Query<PathQ>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>> {
    let path = q
        .path
        .as_deref()
        .ok_or_else(|| Error::Invalid("PUT needs ?path=".into()))?;
    let opts = write_opts(&headers, q.if_head_revision)?;
    Ok(out(s.write(path, &body, opts).await?))
}

/// DELETE trashes a file or folder. Trash is reversible, so this is the everyday delete.
async fn delete_file(
    State(s): State<Drawer>,
    Query(q): Query<PathQ>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    Ok(out(s
        .trash(target(&q)?, q.if_head_revision, &actor(&headers))
        .await?))
}

async fn meta(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(out(s.stat(target(&q)?).await?))
}

async fn revisions(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "revisions": s.revisions(target(&q)?).await? }),
    ))
}

async fn rollback(
    State(s): State<Drawer>,
    verified: Option<axum::Extension<crate::auth::VerifiedActor>>,
    headers: HeaderMap,
    Json(b): Json<RollbackBody>,
) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(
        body_actor(verified.is_some(), b.author, &headers),
        b.comment.unwrap_or_default(),
    );
    opts.if_head_revision = b.if_head_revision;
    Ok(out(s
        .rollback(pick(&b.id, &b.path)?, b.revision, opts)
        .await?))
}

async fn copy(
    State(s): State<Drawer>,
    verified: Option<axum::Extension<crate::auth::VerifiedActor>>,
    headers: HeaderMap,
    Json(b): Json<CopyBody>,
) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(
        body_actor(verified.is_some(), b.author, &headers),
        b.comment.unwrap_or_default(),
    );
    opts.if_head_revision = b.if_head_revision;
    Ok(out(s
        .copy(pick(&b.from_id, &b.from)?, &b.to, b.source_revision, opts)
        .await?))
}

async fn move_to(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<MoveBody>,
) -> Result<Json<Value>> {
    Ok(out(s
        .move_to(pick(&b.from_id, &b.from)?, &b.to, &actor(&headers))
        .await?))
}

async fn update_node(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Value>> {
    let u = NodeUpdate {
        name: b.name,
        parent_id: b.parent_id,
        starred: b.starred,
        trashed: b.trashed,
    };
    Ok(out(s.update(&id, u, &actor(&headers)).await?))
}

/// DELETE on a node id permanently removes it. It must already be in the trash.
async fn purge_node(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    s.purge(&id, &actor(&headers)).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Create a 1-30 day download-only link for a file. The token is returned only once.
async fn create_link(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<LinkBody>,
) -> Result<Json<Value>> {
    let link = s
        .create_download_link_with_options(
            &id,
            &actor(&headers),
            body.expires_in_days.unwrap_or(7),
            body.revision,
            body.max_downloads,
        )
        .await?;
    let mut value = serde_json::to_value(&link).expect("serializable");
    value["url_path"] = json!(format!("{}/s/{}", s.public_prefix, link.token));
    Ok(Json(value))
}

async fn list_links(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "links": s.list_download_links(&id, &actor(&headers)).await? }),
    ))
}

async fn revoke_link(
    State(s): State<Drawer>,
    Path((id, link_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    s.revoke_download_link(&id, &link_id, &actor(&headers))
        .await?;
    Ok(Json(json!({ "ok": true })))
}

/// The one unauthenticated endpoint, gated by an unguessable, expiring capability.
/// Always download as an attachment: user-provided HTML/SVG must never execute.
pub async fn download_shared(
    State(s): State<Drawer>,
    Path(token): Path<String>,
) -> Result<Response> {
    let (name, size, file_path) = s.download_link_file(&token).await?;
    let file = tokio::fs::File::open(file_path).await?;
    let mut resp = Body::from_stream(ReaderStream::new(file)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(size.max(0) as u64),
    );
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename*=UTF-8''{}",
            encode_filename(&name)
        ))
        .expect("percent-encoded filename is ascii"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    Ok(resp)
}

async fn list_permissions(State(s): State<Drawer>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({ "permissions": s.permissions(&id).await? })))
}

async fn share(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<ShareBody>,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "permissions": s.share(&id, &b.principal, &b.role, &actor(&headers)).await? }),
    ))
}

async fn unshare(
    State(s): State<Drawer>,
    Path((id, principal)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "permissions": s.unshare(&id, &principal, &actor(&headers)).await? }),
    ))
}

async fn trash(State(s): State<Drawer>) -> Result<Json<Value>> {
    Ok(Json(json!({ "nodes": s.trash_list().await? })))
}

async fn starred(State(s): State<Drawer>) -> Result<Json<Value>> {
    Ok(Json(json!({ "nodes": s.starred().await? })))
}

async fn search(State(s): State<Drawer>, Query(q): Query<SearchQ>) -> Result<Json<Value>> {
    Ok(Json(
        json!({ "hits": s.search(&q.q, q.limit.unwrap_or(50)).await? }),
    ))
}

fn body_actor(verified: bool, supplied: Option<String>, headers: &HeaderMap) -> String {
    if verified {
        actor(headers)
    } else {
        supplied.unwrap_or_else(|| actor(headers))
    }
}
