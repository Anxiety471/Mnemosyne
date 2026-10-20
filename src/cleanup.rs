//! Explicit offline maintenance. The exclusive store lease prevents blob/commit races.
use crate::service::Drawer;
use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Args)]
pub struct CleanupArgs {
    /// Number of newest revisions to retain per file (heads and active share pins always kept).
    #[arg(long, default_value_t = 10)]
    pub keep_revisions: i64,
    /// Permanently delete trash older than this many days, including folder descendants.
    #[arg(long, default_value_t = 30)]
    pub trash_days: i64,
    /// Minimum age for unreferenced blobs. Never deletes recent blob files.
    #[arg(long, default_value_t = 1)]
    pub orphan_days: u64,
    /// Apply the preview. Without this flag nothing is deleted.
    #[arg(long)]
    pub apply: bool,
}
#[derive(Serialize)]
pub struct CleanupReport {
    pub applied: bool,
    pub trash_nodes: usize,
    pub old_revisions: usize,
    pub orphan_blobs: usize,
    pub orphan_bytes: u64,
    #[serde(skip)]
    pub(crate) removed: Vec<PathBuf>,
}
pub async fn cleanup(data: &Path, args: &CleanupArgs) -> Result<CleanupReport> {
    if args.keep_revisions < 1
        || args.trash_days < 1
        || args.orphan_days < 1
        || args.trash_days > 36500
        || args.orphan_days > 36500
    {
        bail!("retention must be at least one revision/day and at most 36500 days");
    }
    if !data.join("drawer.db").is_file() {
        bail!("store does not exist");
    }
    let s = Drawer::open_maintenance(data).await?;
    let trash_before = (chrono::Utc::now() - chrono::Duration::days(args.trash_days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let trashed: Vec<String>=sqlx::query_scalar("WITH RECURSIVE doomed(id) AS (SELECT id FROM nodes WHERE trashed=1 AND modified_at < ? UNION SELECT n.id FROM nodes n JOIN doomed d ON n.parent_id=d.id) SELECT id FROM doomed").bind(trash_before).fetch_all(&s.pool).await?;
    let trash: HashSet<_> = trashed.iter().collect();
    let old: Vec<(String,i64)>=sqlx::query_as("SELECT r.node_id,r.revision FROM (SELECT node_id,revision,ROW_NUMBER() OVER (PARTITION BY node_id ORDER BY revision DESC) AS position FROM revisions) r JOIN nodes n ON n.id=r.node_id WHERE r.position > ? AND r.revision != n.head_revision AND NOT EXISTS (SELECT 1 FROM download_links l JOIN download_link_revisions pin ON pin.link_id=l.id WHERE l.node_id=r.node_id AND pin.revision=r.revision AND l.expires_at > strftime('%Y-%m-%dT%H:%M:%fZ','now'))").bind(args.keep_revisions).fetch_all(&s.pool).await?;
    let old: BTreeSet<_> = old
        .into_iter()
        .filter(|(id, _)| !trash.contains(id))
        .collect();
    let rows: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT node_id,revision,hash FROM revisions")
            .fetch_all(&s.pool)
            .await?;
    let surviving: HashSet<_> = rows
        .into_iter()
        .filter(|(id, rev, _)| !trash.contains(id) && !old.contains(&(id.clone(), *rev)))
        .map(|(_, _, hash)| hash)
        .collect();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(args.orphan_days * 86400))
        .context("retention date is out of range")?;
    let mut removed = Vec::new();
    let mut orphan_bytes = 0;
    for (path, size) in crate::storage::files(&data.join("blobs")).await? {
        let relative = path.strip_prefix(data.join("blobs"))?;
        let parts: Vec<_> = relative.components().collect();
        if parts.len() != 2 {
            continue;
        }
        let first = parts[0].as_os_str().to_string_lossy();
        let second = parts[1].as_os_str().to_string_lossy();
        if first.len() != 2 || second.len() != 62 {
            continue;
        }
        let hash = format!("{first}{second}");
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) || surviving.contains(&hash) {
            continue;
        }
        if tokio::fs::metadata(&path).await?.modified()? > cutoff {
            continue;
        }
        orphan_bytes += size;
        removed.push(path);
    }
    let report = CleanupReport {
        applied: args.apply,
        trash_nodes: trashed.len(),
        old_revisions: old.len(),
        orphan_blobs: removed.len(),
        orphan_bytes,
        removed,
    };
    if args.apply {
        let mut tx = s.pool.begin().await?;
        for id in &trashed {
            sqlx::query("DELETE FROM revisions WHERE node_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM permissions WHERE node_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        // One statement permits SQLite to enforce parent references at statement end.
        sqlx::query("CREATE TEMP TABLE cleanup_nodes(id TEXT PRIMARY KEY)")
            .execute(&mut *tx)
            .await?;
        for id in &trashed {
            sqlx::query("INSERT INTO cleanup_nodes(id) VALUES(?)")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM nodes WHERE id IN (SELECT id FROM cleanup_nodes)")
            .execute(&mut *tx)
            .await?;
        sqlx::query("DROP TABLE cleanup_nodes")
            .execute(&mut *tx)
            .await?;
        for (id, rev) in &old {
            sqlx::query("DELETE FROM revisions WHERE node_id=? AND revision=?")
                .bind(id)
                .bind(rev)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        // Metadata first: a failed unlink leaves an orphan for the next run, never a missing live blob.
        for path in &report.removed {
            tokio::fs::remove_file(path).await?;
        }
    }
    s.pool.close().await;
    Ok(report)
}
