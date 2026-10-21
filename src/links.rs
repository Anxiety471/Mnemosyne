//! Expiring, revocable, read-only file links. Link secrets are never stored in plaintext.
use chrono::{Duration, SecondsFormat, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::service::{Drawer, OWNER, Ref};

#[derive(Debug, Serialize)]
pub struct DownloadLink {
    pub id: String,
    pub created_at: String,
    pub expires_at: String,
    pub revision: Option<i64>,
    pub max_downloads: Option<i64>,
    pub downloads: i64,
}

#[derive(Debug, Serialize)]
pub struct NewDownloadLink {
    pub id: String,
    /// Returned only once; the database stores only its SHA-256 digest.
    pub token: String,
    pub created_at: String,
    pub expires_at: String,
    pub revision: Option<i64>,
    pub max_downloads: Option<i64>,
    pub downloads: i64,
}

impl Drawer {
    /// Only files can be linked. A link grants read-only access to the latest revision,
    /// and cannot be used to browse other files or invoke the authenticated API.
    pub async fn create_download_link(
        &self,
        id: &str,
        actor: &str,
        days: i64,
    ) -> Result<NewDownloadLink> {
        self.create_download_link_with_revision(id, actor, days, None)
            .await
    }

    pub async fn create_download_link_with_revision(
        &self,
        id: &str,
        actor: &str,
        days: i64,
        revision: Option<i64>,
    ) -> Result<NewDownloadLink> {
        self.create_download_link_with_options(id, actor, days, revision, None)
            .await
    }

    pub async fn create_download_link_with_options(
        &self,
        id: &str,
        actor: &str,
        days: i64,
        revision: Option<i64>,
        max_downloads: Option<i64>,
    ) -> Result<NewDownloadLink> {
        if max_downloads.is_some_and(|n| !(1..=1_000_000).contains(&n)) {
            return Err(Error::Invalid("download limit must be 1 to 1000000".into()));
        }
        if !(1..=30).contains(&days) {
            return Err(Error::Invalid(
                "link expiry must be between 1 and 30 days".into(),
            ));
        }
        let _guard = self.write_lock.lock().await;
        let node = self.stat(Ref::Id(id)).await?;
        if node.kind != "file" || node.trashed || self.stat(Ref::Path(&node.path)).await?.id != id {
            return Err(Error::NotFound("live file".into()));
        }
        self.authorize(id, actor, OWNER).await?;
        if let Some(revision) = revision {
            self.read_file_path(Ref::Id(id), Some(revision)).await?;
        }

        // Two independent UUIDv4 values provide roughly 244 bits of unpredictable entropy.
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        let link_id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let expires_at =
            (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO download_links (id, node_id, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&link_id)
            .bind(id)
            .bind(token_hash)
            .bind(&created_at)
            .bind(&expires_at)
            .execute(&mut *tx)
            .await?;
        if let Some(revision) = revision {
            sqlx::query("INSERT INTO download_link_revisions (link_id, revision) VALUES (?, ?)")
                .bind(&link_id)
                .bind(revision)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(max_downloads) = max_downloads {
            sqlx::query("INSERT INTO download_link_limits (link_id, max_downloads) VALUES (?, ?)")
                .bind(&link_id)
                .bind(max_downloads)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(NewDownloadLink {
            id: link_id,
            token,
            created_at,
            expires_at,
            revision,
            max_downloads,
            downloads: 0,
        })
    }

    pub async fn list_download_links(&self, id: &str, actor: &str) -> Result<Vec<DownloadLink>> {
        self.stat(Ref::Id(id)).await?;
        self.authorize(id, actor, OWNER).await?;
        let rows = sqlx::query(
            "SELECT d.id, d.created_at, d.expires_at, p.revision, l.max_downloads, COALESCE(l.downloads, 0) AS downloads FROM download_links d LEFT JOIN download_link_revisions p ON p.link_id = d.id LEFT JOIN download_link_limits l ON l.link_id = d.id WHERE d.node_id = ? ORDER BY d.created_at DESC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DownloadLink {
                id: r.get("id"),
                created_at: r.get("created_at"),
                expires_at: r.get("expires_at"),
                revision: r.get("revision"),
                max_downloads: r.get("max_downloads"),
                downloads: r.get("downloads"),
            })
            .collect())
    }

    pub async fn revoke_download_link(&self, id: &str, link_id: &str, actor: &str) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        self.stat(Ref::Id(id)).await?;
        self.authorize(id, actor, OWNER).await?;
        let changed = sqlx::query("DELETE FROM download_links WHERE id = ? AND node_id = ?")
            .bind(link_id)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound("download link".into()));
        }
        Ok(())
    }

    /// No actor is trusted here. The only credential is the 64-character link secret.
    /// Trashing a file or any ancestor makes its links unusable until restored.
    pub async fn download_link_file(
        &self,
        token: &str,
    ) -> Result<(String, i64, std::path::PathBuf)> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::NotFound("download link".into()));
        }
        let _guard = self.write_lock.lock().await;
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let row = sqlx::query("SELECT d.id, d.node_id, p.revision, l.max_downloads, COALESCE(l.downloads, 0) AS downloads FROM download_links d LEFT JOIN download_link_revisions p ON p.link_id = d.id LEFT JOIN download_link_limits l ON l.link_id = d.id WHERE d.token_hash = ? AND d.expires_at > ?")
            .bind(hash)
            .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound("download link".into()))?;
        let id: String = row.get("node_id");
        let node = self.stat(Ref::Id(&id)).await?;
        if node.kind != "file" || node.trashed {
            return Err(Error::NotFound("download link".into()));
        }
        // Path resolution checks the full ancestor chain, not just the file's trashed flag.
        let live = self.stat(Ref::Path(&node.path)).await?;
        if live.id != id {
            return Err(Error::NotFound("download link".into()));
        }
        let revision: Option<i64> = row.get("revision");
        let (rev, file_path) = self.read_file_path(Ref::Id(&id), revision).await?;
        if row.get::<Option<i64>, _>("max_downloads").is_some() {
            let used = sqlx::query("UPDATE download_link_limits SET downloads = downloads + 1 WHERE link_id = ? AND downloads < max_downloads")
                .bind(row.get::<String, _>("id")).execute(&self.pool).await?.rows_affected();
            if used == 0 {
                return Err(Error::NotFound("download link".into()));
            }
        }
        Ok((node.name, rev.size, file_path))
    }
}
