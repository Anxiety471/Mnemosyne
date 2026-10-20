//! Physical use and logical file totals deliberately measure different things.
use crate::{error::Result, service::Drawer};
use axum::{Json, extract::State};
use serde::Serialize;
use std::{collections::HashSet, path::Path};

#[derive(Serialize)]
pub struct StorageStats {
    pub quota_bytes: Option<i64>,
    pub live_files: i64,
    pub live_bytes: i64,
    pub revisions: i64,
    pub revision_bytes: i64,
    pub blob_bytes: u64,
    pub unreferenced_blob_bytes: u64,
    pub staging_bytes: u64,
    pub database_bytes: u64,
    pub filesystem_total_bytes: u64,
    pub filesystem_available_bytes: u64,
}

pub(crate) async fn files(root: &Path) -> Result<Vec<(std::path::PathBuf, u64)>> {
    let mut output = Vec::new();
    let mut todo = vec![root.to_owned()];
    while let Some(path) = todo.pop() {
        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                todo.push(entry.path());
            } else if kind.is_file() {
                output.push((entry.path(), entry.metadata().await?.len()));
            }
        }
    }
    Ok(output)
}

pub async fn stats(State(s): State<Drawer>) -> Result<Json<StorageStats>> {
    let _guard = s.write_lock.lock().await;
    let (live_files, live_bytes): (i64, i64) = sqlx::query_as("WITH RECURSIVE live(id) AS (SELECT 'root' UNION ALL SELECT n.id FROM nodes n JOIN live ON n.parent_id=live.id WHERE n.trashed=0) SELECT COUNT(*), COALESCE(SUM(r.size),0) FROM nodes n JOIN live ON n.id=live.id JOIN revisions r ON r.node_id=n.id AND r.revision=n.head_revision").fetch_one(&s.pool).await?;
    let (revisions, revision_bytes): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(size),0) FROM revisions")
            .fetch_one(&s.pool)
            .await?;
    let hashes: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT DISTINCT hash FROM revisions")
            .fetch_all(&s.pool)
            .await?
            .into_iter()
            .collect();
    let blobs = files(&s.data_dir.join("blobs")).await?;
    let mut unreferenced_blob_bytes = 0;
    for (path, size) in &blobs {
        let hash = format!(
            "{}{}",
            path.parent()
                .and_then(|p| p.file_name())
                .unwrap_or_default()
                .to_string_lossy(),
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        if hash.len() == 64
            && hash.bytes().all(|b| b.is_ascii_hexdigit())
            && !hashes.contains(&hash)
        {
            unreferenced_blob_bytes += size;
        }
    }
    let staging_bytes = files(&s.transfer_dir)
        .await?
        .iter()
        .map(|(_, size)| size)
        .sum();
    let mut database_bytes = 0;
    for name in ["drawer.db", "drawer.db-wal", "drawer.db-shm"] {
        match tokio::fs::metadata(s.data_dir.join(name)).await {
            Ok(m) => database_bytes += m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let dir = s.data_dir.clone();
    let (total, available) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        Ok((fs2::total_space(&dir)?, fs2::available_space(&dir)?))
    })
    .await
    .map_err(|e| crate::error::Error::Internal(e.to_string()))??;
    Ok(Json(StorageStats {
        quota_bytes: {
            let limit = s.quota.load(std::sync::atomic::Ordering::Acquire);
            (limit != i64::MAX).then_some(limit)
        },
        live_files,
        live_bytes,
        revisions,
        revision_bytes,
        blob_bytes: blobs.iter().map(|(_, n)| n).sum(),
        unreferenced_blob_bytes,
        staging_bytes,
        database_bytes,
        filesystem_total_bytes: total,
        filesystem_available_bytes: available,
    }))
}
