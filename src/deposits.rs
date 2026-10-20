//! Expiring upload-only folder capabilities. No public read/list access is provided.
//! Uploads are chunked and resumable: the page starts a deposit-scoped staging
//! record, appends fixed-size chunks at an offset, and commits the assembled file
//! only after the declared SHA-256 matches, so a dropped connection never forces a restart.
use crate::{
    error::{Error, Result},
    service::{Drawer, OWNER, Ref, WriteOpts},
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{Html, IntoResponse, Response},
};
use futures_lite::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
#[derive(Deserialize)]
pub struct Policy {
    days: i64,
    max_files: i64,
    max_bytes: i64,
}
#[derive(Serialize, sqlx::FromRow)]
pub struct UploadLink {
    id: String,
    expires_at: String,
    max_files: i64,
    max_bytes: i64,
    files_uploaded: i64,
    bytes_uploaded: i64,
}
#[derive(Deserialize)]
pub struct Filename {
    name: String,
}
/// Largest single chunk accepted on a deposit append, matching the main UI's transfer chunks.
pub const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_STAGED_BYTES: i64 = crate::MAX_UPLOAD_BYTES as i64;
const DAY: &str = "%Y-%m-%dT%H:%M:%fZ";
fn actor(headers: &HeaderMap) -> &str {
    headers
        .get("x-drawer-author")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("api")
}
fn digest(token: &str) -> Result<String> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::NotFound("upload link".into()));
    }
    Ok(hex::encode(Sha256::digest(token.as_bytes())))
}
async fn folder(s: &Drawer, id: &str) -> Result<crate::service::Node> {
    let node = s.stat(Ref::Id(id)).await?;
    if node.kind != "folder" || node.trashed || s.stat(Ref::Path(&node.path)).await?.id != id {
        return Err(Error::NotFound("live upload folder".into()));
    }
    Ok(node)
}
fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 200
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        Err(Error::Invalid(
            "filename must be one component, at most 200 bytes".into(),
        ))
    } else {
        Ok(())
    }
}
fn valid_sha256(s: &str) -> Result<()> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Err(Error::Invalid("sha256 must be 64 hex characters".into()))
    } else {
        Ok(())
    }
}
async fn valid(s: &Drawer, token: &str) -> Result<(String, String)> {
    let (id, node): (String, String) = sqlx::query_as(
        "SELECT id,node_id FROM upload_links WHERE token_hash=? AND expires_at>strftime(?, 'now')",
    )
    .bind(digest(token)?)
    .bind(DAY)
    .fetch_optional(&s.pool)
    .await?
    .ok_or_else(|| Error::NotFound("upload link expired or revoked".into()))?;
    folder(s, &node).await?;
    Ok((id, node))
}
/// One `UPDATE … WHERE` that both checks and increments the budget, so concurrent
/// uploads cannot overshoot `max_files`/`max_bytes`.
async fn reserve_budget(s: &Drawer, id: &str, size: i64) -> Result<()> {
    let done=sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded+1,bytes_uploaded=bytes_uploaded+? WHERE id=? AND files_uploaded<max_files AND bytes_uploaded+?<=max_bytes AND expires_at>strftime(?, 'now')").bind(size).bind(id).bind(size).bind(DAY).execute(&s.pool).await?.rows_affected();
    if done == 0 {
        return Err(Error::Conflict("upload link budget exhausted".into()));
    }
    Ok(())
}
async fn release_budget(s: &Drawer, id: &str, size: i64) -> Result<()> {
    sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded-1,bytes_uploaded=MAX(bytes_uploaded-?, 0) WHERE id=?").bind(size).bind(id).execute(&s.pool).await?;
    Ok(())
}
async fn drop_staging(s: &Drawer, id: &str) -> Result<()> {
    match tokio::fs::remove_file(s.transfer_dir.join(format!("drop-{id}.part"))).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
async fn write_chunk(s: &Drawer, id: &str, requested: i64, data: &[u8]) -> Result<i64> {
    if data.is_empty() || data.len() > MAX_CHUNK_BYTES {
        return Err(Error::Invalid("upload chunks must be 1 to 8 MiB".into()));
    }
    if requested < 0 {
        return Err(Error::Invalid("offset must be non-negative".into()));
    }
    let row = sqlx::query("SELECT received,size FROM deposit_chunks WHERE id=?")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or_else(|| Error::NotFound("upload session".into()))?;
    let (received, size): (i64, i64) = (row.get("received"), row.get("size"));
    if requested != received {
        return Err(Error::PreconditionFailed(format!(
            "expected offset {received}, got {requested}"
        )));
    }
    if received + data.len() as i64 > size {
        return Err(Error::Invalid("chunk exceeds upload size".into()));
    }
    let path = s.transfer_dir.join(format!("drop-{id}.part"));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await?;
    let disk_size = file.metadata().await?.len();
    if disk_size < received as u64 {
        return Err(Error::Internal("upload staging file is incomplete".into()));
    }
    // A crash between data write and database update is recoverable.
    file.set_len(received as u64).await?;
    file.seek(std::io::SeekFrom::Start(received as u64)).await?;
    file.write_all(data).await?;
    file.sync_data().await?;
    let next = received + data.len() as i64;
    sqlx::query("UPDATE deposit_chunks SET received=? WHERE id=?")
        .bind(next)
        .bind(id)
        .execute(&s.pool)
        .await?;
    Ok(next)
}
async fn commit_staging(
    s: &Drawer,
    node: &str,
    chunk_id: &str,
    name: &str,
    sha256: &str,
) -> Result<Value> {
    let parent = folder(s, node).await?;
    let destination = format!(
        "{}/{}-{}",
        parent.path.trim_end_matches('/'),
        uuid::Uuid::new_v4().simple(),
        name
    );
    let mut opts = WriteOpts::new(parent.owner, "Upload-only deposit");
    opts.if_head_revision = Some(0);
    opts.required_parent = Some(node.to_string());
    let path = s.transfer_dir.join(format!("drop-{chunk_id}.part"));
    let saved = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        s.write_staged(&destination, &path, opts, Some(sha256)),
    )
    .await
    .unwrap_or_else(|_| Err(Error::Invalid("upload timed out".into())))?;
    sqlx::query("DELETE FROM deposit_chunks WHERE id=?")
        .bind(chunk_id)
        .execute(&s.pool)
        .await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(serde_json::to_value(saved).expect("serializable"))
}
pub async fn create(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(p): Json<Policy>,
) -> Result<Json<Value>> {
    if !(1..=30).contains(&p.days)
        || !(1..=10000).contains(&p.max_files)
        || !(1..=1_099_511_627_776).contains(&p.max_bytes)
    {
        return Err(Error::Invalid(
            "expiry 1–30 days, 1–10000 files, and 1 byte–1 TiB required".into(),
        ));
    }
    let _lock = s.write_lock.lock().await;
    folder(&s, &id).await?;
    s.authorize(&id, actor(&headers), OWNER).await?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let link = uuid::Uuid::new_v4().to_string();
    let expires = (chrono::Utc::now() + chrono::Duration::days(p.days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query("INSERT INTO upload_links(id,node_id,token_hash,expires_at,max_files,max_bytes) VALUES(?,?,?,?,?,?)").bind(&link).bind(id).bind(digest(&token)?).bind(&expires).bind(p.max_files).bind(p.max_bytes).execute(&s.pool).await?;
    Ok(Json(
        json!({"id":link,"token":token,"expires_at":expires,"url_path":format!("{}/drop/{token}",s.public_prefix)}),
    ))
}
pub async fn list(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<UploadLink>>> {
    s.authorize(&id, actor(&headers), OWNER).await?;
    Ok(Json(sqlx::query_as("SELECT id,expires_at,max_files,max_bytes,files_uploaded,bytes_uploaded FROM upload_links WHERE node_id=? ORDER BY expires_at DESC").bind(id).fetch_all(&s.pool).await?))
}
pub async fn revoke(
    State(s): State<Drawer>,
    Path((id, link)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let _lock = s.deposit_lock.lock().await;
    s.authorize(&id, actor(&headers), OWNER).await?;
    sqlx::query("DELETE FROM upload_links WHERE id=? AND node_id=?")
        .bind(link)
        .bind(id)
        .execute(&s.pool)
        .await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn landing(State(s): State<Drawer>, Path(token): Path<String>) -> Result<Response> {
    valid(&s, &token).await?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(include_str!("deposit.html")),
    )
        .into_response())
}
#[derive(Deserialize)]
pub struct ChunkStart {
    name: String,
    size: i64,
    sha256: String,
}
#[derive(Serialize)]
pub struct ChunkState {
    id: String,
    received: i64,
    size: i64,
}
/// Start a resumable upload: validates the declared size against the link budget and
/// any partial staging left from a previous attempt of the same file.
pub async fn start(
    State(s): State<Drawer>,
    Path(token): Path<String>,
    Json(b): Json<ChunkStart>,
) -> Result<axum::response::Response> {
    let response = start_core(&s, &token, b)
        .await
        .map(IntoResponse::into_response);
    public_event(&s, "POST", "/drop/{token}/start", response).await
}
pub(crate) async fn start_core(s: &Drawer, token: &str, b: ChunkStart) -> Result<Json<ChunkState>> {
    valid_name(&b.name)?;
    valid_sha256(&b.sha256)?;
    if b.size < 0 || b.size > MAX_STAGED_BYTES {
        return Err(Error::Invalid("file size must be 0 to 512 MiB".into()));
    }
    let (link_id, _node) = valid(s, token).await?;
    let _guard = s.deposit_lock.lock().await;
    let reuse=sqlx::query("SELECT id,received FROM deposit_chunks WHERE link_id=? AND name=? AND size=? AND sha256=? AND filename=? AND received<size AND expires_at>strftime(?, 'now')").bind(&link_id).bind(&b.name).bind(b.size).bind(&b.sha256).bind(&b.name).bind(DAY).fetch_optional(&s.pool).await?;
    if let Some(row) = reuse {
        let id: String = row.get("id");
        let received: i64 = row.get("received");
        return Ok(Json(ChunkState {
            id,
            received,
            size: b.size,
        }));
    }
    reserve_budget(s, &link_id, b.size).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let expires = (chrono::Utc::now() + chrono::Duration::hours(48))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    sqlx::query("INSERT INTO deposit_chunks(id,link_id,name,size,sha256,filename,expires_at) VALUES(?,?,?,?,?,?,?)").bind(&id).bind(&link_id).bind(&b.name).bind(b.size).bind(&b.sha256).bind(&b.name).bind(&expires).execute(&s.pool).await?;
    tokio::fs::create_dir_all(&s.transfer_dir).await?;
    tokio::fs::File::create(s.transfer_dir.join(format!("drop-{id}.part"))).await?;
    Ok(Json(ChunkState {
        id,
        received: 0,
        size: b.size,
    }))
}
/// Append a chunk at the exact offset the server already holds; `received` advances by the chunk.
pub async fn append(
    State(s): State<Drawer>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<axum::response::Response> {
    let response = append_core(&s, &token, &id, headers.get("x-upload-offset"), body)
        .await
        .map(IntoResponse::into_response);
    public_event(&s, "PUT", "/drop/{token}/append/{id}", response).await
}
pub(crate) async fn append_core(
    s: &Drawer,
    token: &str,
    id: &str,
    offset: Option<&axum::http::HeaderValue>,
    body: Body,
) -> Result<Json<Value>> {
    let (link_id, _node) = valid(s, token).await?;
    let _guard = s.deposit_lock.lock().await;
    sqlx::query_scalar::<_, String>("SELECT link_id FROM deposit_chunks WHERE id=?")
        .bind(id)
        .fetch_optional(&s.pool)
        .await?
        .filter(|owner| *owner == link_id)
        .ok_or_else(|| Error::NotFound("upload session".into()))?;
    let requested: i64 = offset
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Invalid("missing or invalid x-upload-offset".into()))?;
    let mut received = 0i64;
    let result = tokio::time::timeout(std::time::Duration::from_secs(900), async {
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Invalid("upload stream failed".into()))?;
            received = write_chunk(s, id, requested + received, &chunk).await?;
        }
        Ok::<_, Error>(())
    })
    .await
    .unwrap_or_else(|_| Err(Error::Invalid("upload timed out".into())));
    match result {
        Ok(()) => Ok(Json(json!({"id":id,"received":received}))),
        Err(error) => {
            // The server-side staging row is gone (expired or already completed); drop the
            // local copy rather than stranding it.
            if matches!(error, Error::NotFound(_)) {
                let _ = drop_staging(s, id).await;
            }
            Err(error)
        }
    }
}
/// Commit a fully received chunk after verifying the declared SHA-256.
pub async fn complete(
    State(s): State<Drawer>,
    Path((token, id)): Path<(String, String)>,
) -> Result<axum::response::Response> {
    let response = complete_core(&s, &token, &id)
        .await
        .map(IntoResponse::into_response);
    public_event(&s, "POST", "/drop/{token}/complete/{id}", response).await
}
pub(crate) async fn complete_core(s: &Drawer, token: &str, id: &str) -> Result<Json<Value>> {
    let (link_id, node) = valid(s, token).await?;
    let _guard = s.deposit_lock.lock().await;
    let row =
        sqlx::query("SELECT link_id,size,sha256,filename,received FROM deposit_chunks WHERE id=?")
            .bind(id)
            .fetch_optional(&s.pool)
            .await?
            .ok_or_else(|| Error::NotFound("upload session".into()))?;
    let (owner, size, sha256, filename, received): (String, i64, String, String, i64) = (
        row.get("link_id"),
        row.get("size"),
        row.get("sha256"),
        row.get("filename"),
        row.get("received"),
    );
    if owner != link_id {
        return Err(Error::NotFound("upload session".into()));
    }
    if received != size {
        return Err(Error::Conflict(format!(
            "upload incomplete: {received} of {size} bytes"
        )));
    }
    let path = s.transfer_dir.join(format!("drop-{id}.part"));
    match tokio::fs::metadata(&path).await {
        Ok(m) if m.len() == size as u64 => {}
        Ok(_) => return Err(Error::Conflict("staging file length mismatch".into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotFound("upload session".into()));
        }
        Err(e) => return Err(e.into()),
    }
    commit_staging(s, &node, id, &filename, &sha256)
        .await
        .map(Json)
}
/// Abandon a partial upload and give the reserved bytes back to the link budget.
pub async fn cancel(
    State(s): State<Drawer>,
    Path((token, id)): Path<(String, String)>,
) -> Result<axum::response::Response> {
    let response = cancel_core(&s, &token, &id)
        .await
        .map(IntoResponse::into_response);
    public_event(&s, "DELETE", "/drop/{token}/cancel/{id}", response).await
}
pub(crate) async fn cancel_core(s: &Drawer, token: &str, id: &str) -> Result<Json<Value>> {
    let (link_id, _node) = valid(s, token).await?;
    let _guard = s.deposit_lock.lock().await;
    let size: i64 = sqlx::query_scalar("SELECT size FROM deposit_chunks WHERE id=? AND link_id=?")
        .bind(id)
        .bind(&link_id)
        .fetch_optional(&s.pool)
        .await?
        .ok_or_else(|| Error::NotFound("upload session".into()))?;
    sqlx::query("DELETE FROM deposit_chunks WHERE id=?")
        .bind(id)
        .execute(&s.pool)
        .await?;
    drop_staging(s, id).await?;
    release_budget(s, &link_id, size).await?;
    Ok(Json(json!({"ok":true})))
}
/// The public drop page and its chunked session write into the tenant's own store, so a named
/// user's upload link is exercised the same way as the admin's.
async fn public_event(
    s: &Drawer,
    method: &str,
    route: &str,
    result: Result<axum::response::Response>,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    if let Err(error)=sqlx::query("INSERT INTO activity(actor,method,route,status,created_at) VALUES('anonymous',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))").bind(method).bind(route).bind(i64::from(response.status().as_u16())).execute(&s.pool).await { tracing::warn!(%error,"could not record public workspace activity"); }
    Ok(response)
}
struct Staged(std::path::PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub async fn upload(
    State(s): State<Drawer>,
    Path(token): Path<String>,
    Query(q): Query<Filename>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>> {
    if q.name.is_empty()
        || q.name.len() > 200
        || q.name == "."
        || q.name == ".."
        || q.name.contains(['/', '\\'])
        || q.name.chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "filename must be one component, at most 200 bytes".into(),
        ));
    }
    let size = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n >= 0 && *n <= crate::MAX_UPLOAD_BYTES as i64)
        .ok_or_else(|| Error::Invalid("Content-Length from 0 to 512 MiB is required".into()))?;
    let (id, node) = valid(&s, &token).await?;
    let reserved=sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded+1,bytes_uploaded=bytes_uploaded+? WHERE id=? AND files_uploaded<max_files AND bytes_uploaded+?<=max_bytes AND expires_at>strftime(?, 'now')").bind(size).bind(&id).bind(size).bind(DAY).execute(&s.pool).await?.rows_affected();
    if reserved == 0 {
        return Err(Error::Conflict("upload link budget exhausted".into()));
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(900), async {
        tokio::fs::create_dir_all(&s.transfer_dir).await?;
        let staged = Staged(
            s.transfer_dir
                .join(format!("drop-{}.part", uuid::Uuid::new_v4())),
        );
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged.0)
            .await?;
        let mut stream = body.into_data_stream();
        let mut received = 0i64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::Invalid("upload stream failed".into()))?;
            received += chunk.len() as i64;
            if received > size {
                return Err(Error::Invalid("upload exceeds declared length".into()));
            }
            file.write_all(&chunk).await?;
        }
        if received != size {
            return Err(Error::Invalid("upload length mismatch".into()));
        }
        file.sync_data().await?;
        drop(file);
        let _lock = s.deposit_lock.lock().await;
        valid(&s, &token).await?;
        let parent = folder(&s, &node).await?;
        let destination = format!(
            "{}/{}-{}",
            parent.path.trim_end_matches('/'),
            uuid::Uuid::new_v4().simple(),
            q.name
        );
        let mut opts = WriteOpts::new(parent.owner, "Upload-only deposit");
        opts.if_head_revision = Some(0);
        opts.required_parent = Some(node);
        s.write_staged(&destination, &staged.0, opts, None).await?;
        Ok::<_, Error>(Json(json!({"uploaded":true,"bytes":size})))
    })
    .await
    .unwrap_or_else(|_| Err(Error::Invalid("upload timed out".into())));
    if result.is_err() {
        sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded-1,bytes_uploaded=MAX(bytes_uploaded-?, 0) WHERE id=?").bind(size).bind(id).execute(&s.pool).await?;
    }
    result
}
