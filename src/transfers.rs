//! Resumable, offset-checked uploads staged on disk. Small fixed chunks keep RAM bounded.
use axum::{
    Json,
    body::Bytes,
    extract::{Path as UrlPath, State},
    http::HeaderMap,
};
use chrono::{Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    service::{Drawer, WriteOpts, normalize},
};

const MAX_TRANSFER_BYTES: i64 = 4 * 1024 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_BATCH_FILES: usize = 100;

#[derive(Deserialize)]
pub struct NewTransfer {
    path: String,
    size: i64,
    sha256: Option<String>,
    if_head_revision: Option<i64>,
}
#[derive(Serialize)]
struct TransferInfo {
    id: String,
    path: String,
    size: i64,
    received: i64,
    expires_at: String,
    sha256: Option<String>,
}
fn actor(h: &HeaderMap) -> String {
    h.get("x-drawer-author")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("api")
        .to_string()
}
fn valid_id(id: &str) -> Result<()> {
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| Error::NotFound("transfer".into()))
}
fn partial_path(s: &Drawer, id: &str) -> std::path::PathBuf {
    s.transfer_dir.join(format!("{id}.part"))
}
async fn owned(s: &Drawer, id: &str, who: &str) -> Result<sqlx::sqlite::SqliteRow> {
    valid_id(id)?;
    let row = sqlx::query("SELECT id, path, size, received, actor, expires_at, sha256, if_head_revision FROM transfers WHERE id = ?")
        .bind(id).fetch_optional(&s.pool).await?
        .ok_or_else(|| Error::NotFound("transfer".into()))?;
    if row.get::<String, _>("actor") != who {
        return Err(Error::Forbidden("transfer belongs to another user".into()));
    }
    if row.get::<String, _>("expires_at") <= Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    {
        return Err(Error::NotFound("transfer expired".into()));
    }
    Ok(row)
}
fn info(r: &sqlx::sqlite::SqliteRow) -> TransferInfo {
    TransferInfo {
        id: r.get("id"),
        path: r.get("path"),
        size: r.get("size"),
        received: r.get("received"),
        expires_at: r.get("expires_at"),
        sha256: r.get("sha256"),
    }
}
fn validate(b: &NewTransfer) -> Result<String> {
    let norm = normalize(&b.path)?;
    if norm == "/" || !(0..=MAX_TRANSFER_BYTES).contains(&b.size) {
        return Err(Error::Invalid(
            "transfer must be a non-root file up to 4 GiB".into(),
        ));
    }
    if let Some(expected) = &b.sha256
        && (expected.len() != 64 || !expected.bytes().all(|v| v.is_ascii_hexdigit()))
    {
        return Err(Error::Invalid(
            "sha256 must contain 64 hexadecimal characters".into(),
        ));
    }
    Ok(norm)
}
/// Caller holds `transfer_lock` and has already checked quota for the whole request.
async fn insert(s: &Drawer, who: &str, norm: &str, b: &NewTransfer) -> Result<Value> {
    let id = Uuid::new_v4().to_string();
    let expires = (Utc::now() + Duration::days(2)).to_rfc3339_opts(SecondsFormat::Millis, true);
    tokio::fs::create_dir_all(&s.transfer_dir).await?;
    tokio::fs::File::create(partial_path(s, &id)).await?;
    let result = sqlx::query(
        "INSERT INTO transfers (id, path, size, received, actor, sha256, if_head_revision, expires_at) VALUES (?, ?, ?, 0, ?, ?, ?, ?)",
    ).bind(&id).bind(norm).bind(b.size).bind(who)
        .bind(b.sha256.as_ref().map(|x| x.to_lowercase())).bind(b.if_head_revision).bind(&expires)
        .execute(&s.pool).await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(partial_path(s, &id)).await;
        return Err(e.into());
    }
    Ok(json!({"id":id, "path":norm, "size":b.size, "received":0, "expires_at":expires}))
}
async fn reserved(s: &Drawer) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COALESCE(SUM(size),0) FROM transfers WHERE expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").fetch_one(&s.pool).await?)
}
pub async fn start(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<NewTransfer>,
) -> Result<Json<Value>> {
    let norm = validate(&b)?;
    let _lock = s.transfer_lock.lock().await;
    s.check_quota(reserved(&s).await?.saturating_add(b.size))
        .await?;
    Ok(Json(insert(&s, &actor(&headers), &norm, &b).await?))
}
#[derive(Deserialize)]
pub struct BatchStart {
    files: Vec<NewTransfer>,
}
/// Starts one transfer per file, all or nothing: any invalid entry, duplicate path or quota
/// shortfall rejects the whole batch before anything is staged.
pub async fn start_batch(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<BatchStart>,
) -> Result<Json<Value>> {
    if b.files.is_empty() || b.files.len() > MAX_BATCH_FILES {
        return Err(Error::Invalid(format!(
            "batch must contain 1 to {MAX_BATCH_FILES} files"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut norms = Vec::with_capacity(b.files.len());
    let mut total = 0i64;
    for file in &b.files {
        let norm = validate(file).map_err(|e| Error::Invalid(format!("{}: {e}", file.path)))?;
        if !seen.insert(norm.clone()) {
            return Err(Error::Invalid(format!("duplicate path in batch: {norm}")));
        }
        total = total.saturating_add(file.size);
        norms.push(norm);
    }
    let who = actor(&headers);
    let _lock = s.transfer_lock.lock().await;
    s.check_quota(reserved(&s).await?.saturating_add(total))
        .await?;
    let mut started = Vec::with_capacity(b.files.len());
    for (file, norm) in b.files.iter().zip(&norms) {
        match insert(&s, &who, norm, file).await {
            Ok(v) => started.push(v),
            Err(error) => {
                for v in &started {
                    let id = v["id"].as_str().unwrap_or_default();
                    let _ = sqlx::query("DELETE FROM transfers WHERE id = ?")
                        .bind(id)
                        .execute(&s.pool)
                        .await;
                    let _ = tokio::fs::remove_file(partial_path(&s, id)).await;
                }
                return Err(error);
            }
        }
    }
    Ok(Json(json!({ "transfers": started })))
}
#[derive(Deserialize)]
pub struct BatchComplete {
    ids: Vec<String>,
}
/// Completes each transfer independently and reports a status per id, so one failed file
/// does not hide the files that were saved.
pub async fn complete_batch(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<BatchComplete>,
) -> Result<Json<Value>> {
    if b.ids.is_empty() || b.ids.len() > MAX_BATCH_FILES {
        return Err(Error::Invalid(format!(
            "batch must contain 1 to {MAX_BATCH_FILES} transfers"
        )));
    }
    let mut results = Vec::with_capacity(b.ids.len());
    for id in b.ids {
        match complete(State(s.clone()), UrlPath(id.clone()), headers.clone()).await {
            Ok(Json(saved)) => results.push(json!({"id":id, "ok":true, "file":saved})),
            Err(error) => results.push(json!({"id":id, "ok":false, "error":error.to_string()})),
        }
    }
    Ok(Json(json!({ "results": results })))
}
pub async fn status(
    State(s): State<Drawer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let row = owned(&s, &id, &actor(&headers)).await?;
    Ok(Json(
        serde_json::to_value(info(&row)).expect("serializable"),
    ))
}
pub async fn append(
    State(s): State<Drawer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    data: Bytes,
) -> Result<Json<Value>> {
    if data.is_empty() || data.len() > MAX_CHUNK_BYTES {
        return Err(Error::Invalid("upload chunks must be 1 to 8 MiB".into()));
    }
    let requested: i64 = headers
        .get("x-upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Error::Invalid("missing or invalid x-upload-offset".into()))?;
    let _guard = s.transfer_lock.lock().await;
    let row = owned(&s, &id, &actor(&headers)).await?;
    let received: i64 = row.get("received");
    let size: i64 = row.get("size");
    if requested != received {
        return Err(Error::PreconditionFailed(format!(
            "expected offset {received}, got {requested}"
        )));
    }
    if received + data.len() as i64 > size {
        return Err(Error::Invalid("chunk exceeds transfer size".into()));
    }
    let paused: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM transfer_history WHERE id = ? AND status = 'paused')",
    )
    .bind(&id)
    .fetch_one(&s.pool)
    .await?;
    if paused {
        return Err(Error::Conflict(
            "transfer is paused; resume it before appending".into(),
        ));
    }
    let path = partial_path(&s, &id);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .await?;
    let disk_size = file.metadata().await?.len();
    if disk_size < received as u64 {
        return Err(Error::Internal(
            "transfer staging file is incomplete".into(),
        ));
    }
    // A crash between data write and database update is recoverable.
    file.set_len(received as u64).await?;
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(received as u64)).await?;
    file.write_all(&data).await?;
    file.sync_data().await?;
    let next = received + data.len() as i64;
    sqlx::query("UPDATE transfers SET received = ? WHERE id = ?")
        .bind(next)
        .bind(&id)
        .execute(&s.pool)
        .await?;
    Ok(Json(json!({ "id": id, "received": next, "size": size })))
}
pub async fn complete(
    State(s): State<Drawer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let _guard = s.transfer_lock.lock().await;
    let who = actor(&headers);
    let row = owned(&s, &id, &who).await?;
    let size: i64 = row.get("size");
    let received: i64 = row.get("received");
    if size != received {
        return Err(Error::Conflict(format!(
            "upload incomplete: {received} of {size} bytes"
        )));
    }
    let path = partial_path(&s, &id);
    if tokio::fs::metadata(&path).await?.len() != size as u64 {
        return Err(Error::Conflict("staging file length mismatch".into()));
    }
    let filename: String = row.get("path");
    let sha256: Option<String> = row.get("sha256");
    let mut opts = WriteOpts::new(who, "resumable upload");
    opts.if_head_revision = row.get("if_head_revision");
    let saved = match s
        .write_staged(&filename, &path, opts, sha256.as_deref())
        .await
    {
        Ok(saved) => saved,
        Err(error) => {
            history_state(&s, &id, "failed", Some(error.to_string())).await?;
            return Err(error);
        }
    };
    history_state(&s, &id, "completed", None).await?;
    sqlx::query("DELETE FROM transfers WHERE id = ?")
        .bind(&id)
        .execute(&s.pool)
        .await?;
    tokio::fs::remove_file(path).await?;
    Ok(Json(serde_json::to_value(saved).expect("serializable")))
}
pub async fn cancel(
    State(s): State<Drawer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    let _guard = s.transfer_lock.lock().await;
    owned(&s, &id, &actor(&headers)).await?;
    history_state(&s, &id, "cancelled", None).await?;
    sqlx::query("DELETE FROM transfers WHERE id = ?")
        .bind(&id)
        .execute(&s.pool)
        .await?;
    match tokio::fs::remove_file(partial_path(&s, &id)).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(Json(json!({"ok":true})))
}

impl Drawer {
    /// Remove abandoned staging files and metadata once their two-day lease expires.
    pub async fn prune_expired_transfers(&self) -> Result<usize> {
        let _guard = self.transfer_lock.lock().await;
        let rows = sqlx::query("SELECT id FROM transfers WHERE expires_at <= ?")
            .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
            .fetch_all(&self.pool)
            .await?;
        let mut cleaned = 0usize;
        for row in rows {
            let id: String = row.get("id");
            if valid_id(&id).is_err() {
                continue;
            }
            let part = partial_path(self, &id);
            match tokio::fs::remove_file(part).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            history_state(self, &id, "expired", None).await?;
            sqlx::query("DELETE FROM transfers WHERE id = ?")
                .bind(&id)
                .execute(&self.pool)
                .await?;
            cleaned += 1;
        }
        Ok(cleaned)
    }
}

async fn history_state(s: &Drawer, id: &str, status: &str, error: Option<String>) -> Result<()> {
    sqlx::query("UPDATE transfer_history SET status = ?, error = ?, received = COALESCE((SELECT received FROM transfers WHERE id = ?), received), updated_at = ? WHERE id = ?")
        .bind(status).bind(error).bind(id).bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)).bind(id).execute(&s.pool).await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct ChangeTransfer {
    status: String,
}
pub async fn change(
    State(s): State<Drawer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    Json(body): Json<ChangeTransfer>,
) -> Result<Json<Value>> {
    if !["paused", "active"].contains(&body.status.as_str()) {
        return Err(Error::Invalid("status must be paused or active".into()));
    }
    let _guard = s.transfer_lock.lock().await;
    owned(&s, &id, &actor(&headers)).await?;
    history_state(&s, &id, &body.status, None).await?;
    Ok(Json(json!({"ok":true})))
}
pub async fn list(State(s): State<Drawer>, headers: HeaderMap) -> Result<Json<Value>> {
    let rows = sqlx::query("SELECT h.id, h.path, h.size, COALESCE(t.received,h.received) AS received, h.status, h.direction, h.error, h.updated_at FROM transfer_history h LEFT JOIN transfers t ON t.id = h.id WHERE h.actor = ? ORDER BY h.updated_at DESC LIMIT 200")
        .bind(actor(&headers)).fetch_all(&s.pool).await?;
    Ok(Json(
        json!({"transfers":rows.iter().map(|r| json!({"id":r.get::<String,_>("id"),"path":r.get::<String,_>("path"),"size":r.get::<i64,_>("size"),"received":r.get::<i64,_>("received"),"status":r.get::<String,_>("status"),"direction":r.get::<String,_>("direction"),"error":r.get::<Option<String>,_>("error"),"updated_at":r.get::<String,_>("updated_at")})).collect::<Vec<_>>()}),
    ))
}
impl Drawer {
    pub async fn record_download(&self, actor: &str, path: &str, size: i64) -> Result<()> {
        sqlx::query("INSERT INTO transfer_history (id, actor, path, size, status, direction, updated_at) VALUES (?, ?, ?, ?, 'issued', 'download', ?)")
            .bind(Uuid::new_v4().to_string()).bind(actor).bind(path).bind(size).bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)).execute(&self.pool).await?;
        Ok(())
    }
}
