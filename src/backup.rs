//! Offline snapshots with content manifests and restore-to-new-directory verification.
use crate::service::Drawer;
use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
};
use tokio::io::AsyncReadExt;

#[derive(Subcommand)]
pub enum BackupAction {
    Create {
        #[arg(long)]
        directory: PathBuf,
    },
    List {
        #[arg(long)]
        directory: PathBuf,
    },
    Verify {
        snapshot: PathBuf,
    },
    /// Restore into a NEW store; the existing store is never replaced.
    Restore {
        snapshot: PathBuf,
        #[arg(long)]
        to: PathBuf,
    },
    /// Install a Linux user timer. Briefly stops drawer.service for each backup.
    Schedule {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, value_enum, default_value = "daily")]
        calendar: Calendar,
        #[arg(long)]
        no_start: bool,
    },
    Unschedule,
    /// Used by the timer; stop the user service, back up, and restart if it was active.
    Scheduled {
        #[arg(long)]
        directory: PathBuf,
    },
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Calendar {
    Hourly,
    Daily,
    Weekly,
}
impl Calendar {
    fn name(self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct Manifest {
    version: u32,
    created_at: String,
    files: Vec<BackupFile>,
}
#[derive(Serialize, Deserialize)]
struct BackupFile {
    path: String,
    sha256: String,
    bytes: u64,
}
async fn hash(path: &Path) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        bytes += n as u64;
    }
    Ok((hex::encode(digest.finalize()), bytes))
}
fn blob_name(hash: &str) -> Result<String> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid blob hash");
    }
    Ok(format!("blobs/{}/{}", &hash[..2], &hash[2..]))
}
async fn readonly(root: &Path) -> Result<sqlx::SqlitePool> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(root.join("drawer.db"))
        .read_only(true)
        .pragma("query_only", "ON");
    Ok(sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}
async fn snapshot_store(s: &Drawer, data: &Path, stage: &Path) -> Result<()> {
    sqlx::query("VACUUM INTO ?")
        .bind(
            stage
                .join("drawer.db")
                .to_str()
                .context("backup path must be UTF-8")?,
        )
        .execute(&s.pool)
        .await?;
    let hashes: Vec<String> = sqlx::query_scalar("SELECT DISTINCT hash FROM revisions")
        .fetch_all(&s.pool)
        .await?;
    for hash in hashes {
        let path = blob_name(&hash)?;
        let dest = stage.join(&path);
        tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
        tokio::fs::copy(data.join(path), dest).await?;
    }
    Ok(())
}
fn valid_backup_path(path: &str) -> bool {
    let inner = if let Some(rest) = path.strip_prefix("users/") {
        let Some((id, inner)) = rest.split_once('/') else {
            return false;
        };
        if uuid::Uuid::parse_str(id).is_err() {
            return false;
        }
        inner
    } else {
        path
    };
    inner == "drawer.db"
        || inner.strip_prefix("blobs/").is_some_and(|p| {
            p.len() == 65
                && p.as_bytes()[2] == b'/'
                && p.bytes()
                    .enumerate()
                    .all(|(i, b)| i == 2 || b.is_ascii_hexdigit())
        })
}
pub async fn create(data: &Path, directory: &Path) -> Result<PathBuf> {
    if !data.join("drawer.db").is_file() {
        bail!("store does not exist");
    }
    let s = Drawer::open_maintenance(data).await?;
    tokio::fs::create_dir_all(directory).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let stage = directory.join(format!(".stage-{id}"));
    tokio::fs::create_dir(&stage).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).await?;
    }
    let result = async {
        snapshot_store(&s, data, &stage).await?;
        let users: Vec<String> = sqlx::query_scalar("SELECT id FROM user_accounts")
            .fetch_all(&s.pool)
            .await?;
        for id in users {
            if uuid::Uuid::parse_str(&id).is_err() {
                bail!("invalid user store identifier");
            }
            let source = data.join("users").join(&id);
            if source.join("drawer.db").is_file() {
                let tenant = Drawer::open_maintenance(&source).await?;
                let target = stage.join("users").join(&id);
                tokio::fs::create_dir_all(&target).await?;
                let result = snapshot_store(&tenant, &source, &target).await;
                tenant.pool.close().await;
                result?;
            }
        }
        let mut files = Vec::new();
        for (path, _) in crate::storage::files(&stage).await? {
            let (sha256, bytes) = hash(&path).await?;
            files.push(BackupFile {
                path: path
                    .strip_prefix(&stage)?
                    .to_str()
                    .context("backup path encoding")?
                    .replace('\\', "/"),
                sha256,
                bytes,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = Manifest {
            version: 1,
            created_at: chrono::Utc::now().to_rfc3339(),
            files,
        };
        tokio::fs::write(
            stage.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )
        .await?;
        verify(&stage).await?;
        let final_path = directory.join(format!("drawer-{id}"));
        tokio::fs::rename(&stage, &final_path).await?;
        Ok::<_, anyhow::Error>(final_path)
    }
    .await;
    s.pool.close().await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&stage).await;
    }
    result
}
pub async fn verify(root: &Path) -> Result<Manifest> {
    let manifest_bytes = tokio::fs::read(root.join("manifest.json")).await?;
    if manifest_bytes.len() > 16 * 1024 * 1024 {
        bail!("manifest too large");
    }
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.version != 1 {
        bail!("unsupported backup format");
    }
    let mut seen = HashSet::new();
    let mut blobs = BTreeMap::new();
    for file in &manifest.files {
        let valid = valid_backup_path(&file.path);
        if !valid || !seen.insert(&file.path) {
            bail!("invalid or duplicate manifest path");
        }
        let path = root.join(&file.path);
        // Never traverse a symlink in a restored tree.
        let mut current = root.to_owned();
        for part in Path::new(&file.path).components() {
            current.push(part);
            if tokio::fs::symlink_metadata(&current)
                .await?
                .file_type()
                .is_symlink()
            {
                bail!("backup contains symlink");
            }
        }
        let (actual, size) = hash(&path).await?;
        if actual != file.sha256 || size != file.bytes {
            bail!("backup checksum mismatch: {}", file.path);
        }
        if file.path != "drawer.db" {
            blobs.insert(file.path.clone(), (actual, size));
        }
    }
    if !seen.contains(&"drawer.db".to_owned()) {
        bail!("backup is missing drawer.db");
    }
    for database in manifest
        .files
        .iter()
        .filter(|f| f.path.ends_with("drawer.db"))
    {
        let prefix = Path::new(&database.path).parent().unwrap();
        let db = readonly(&root.join(prefix)).await?;
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&db)
            .await?;
        if integrity != "ok" {
            bail!("SQLite integrity check failed: {integrity}");
        }
        let foreign_keys = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&db)
            .await?;
        if !foreign_keys.is_empty() {
            bail!("SQLite foreign key check failed");
        }
        let revisions: Vec<(String, i64)> =
            sqlx::query_as("SELECT DISTINCT hash,size FROM revisions")
                .fetch_all(&db)
                .await?;
        for (expected, size) in revisions {
            let entry = blobs
                .get(
                    &prefix
                        .join(blob_name(&expected)?)
                        .to_string_lossy()
                        .replace('\\', "/"),
                )
                .context("referenced blob missing from backup")?;
            if entry.0 != expected || entry.1 != size as u64 {
                bail!("referenced blob is corrupt");
            }
        }
        let missing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes n LEFT JOIN revisions r ON r.node_id=n.id AND r.revision=n.head_revision WHERE n.kind='file' AND r.node_id IS NULL").fetch_one(&db).await?;
        db.close().await;
        if missing > 0 {
            bail!("file head references missing revisions");
        }
    }
    Ok(manifest)
}
pub async fn restore(snapshot: &Path, target: &Path) -> Result<()> {
    let manifest = verify(snapshot).await?;
    tokio::fs::create_dir(target)
        .await
        .context("restore destination must not exist")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(0o700)).await?;
    }
    // Failure leaves the new directory for inspection, never modifies the original store.
    for file in &manifest.files {
        let dest = target.join(&file.path);
        tokio::fs::create_dir_all(dest.parent().unwrap()).await?;
        tokio::fs::copy(snapshot.join(&file.path), dest).await?;
    }
    tokio::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )
    .await?;
    verify(target).await?;
    tokio::fs::remove_file(target.join("manifest.json")).await?;
    let s = Drawer::open_maintenance(target).await?;
    s.pool.close().await;
    Ok(())
}
fn ctl(args: &[&str]) -> Result<()> {
    if !Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()?
        .success()
    {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}
fn units() -> Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!("backup schedules require Linux systemd");
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is required")?)
            .join(".config/systemd/user"),
    )
}
pub async fn cli(action: &BackupAction, data: &Path) -> Result<()> {
    match action {
        BackupAction::Create { directory } => {
            println!("{}", create(data, directory).await?.display())
        }
        BackupAction::Verify { snapshot } => {
            let manifest = verify(snapshot).await?;
            println!(
                "Verified {} files, created {}",
                manifest.files.len(),
                manifest.created_at
            );
        }
        BackupAction::Restore { snapshot, to } => {
            restore(snapshot, to).await?;
            println!("Restored verified snapshot to {}", to.display());
        }
        BackupAction::List { directory } => {
            let mut entries = tokio::fs::read_dir(directory).await?;
            while let Some(e) = entries.next_entry().await? {
                if e.file_type().await?.is_dir() && e.path().join("manifest.json").is_file() {
                    println!("{}", e.path().display());
                }
            }
        }
        BackupAction::Scheduled { directory } => {
            let active = Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", "drawer.service"])
                .status()?
                .success();
            if active {
                ctl(&["stop", "drawer.service"])?;
            }
            let result = create(data, directory).await;
            let restart = if active {
                ctl(&["start", "drawer.service"])
            } else {
                Ok(())
            };
            restart?;
            println!("{}", result?.display());
        }
        BackupAction::Schedule {
            directory,
            calendar,
            no_start,
        } => {
            let dir = units()?;
            std::fs::create_dir_all(&dir)?;
            let service = dir.join("drawer-backup.service");
            let timer = dir.join("drawer-backup.timer");
            if service.exists() || timer.exists() {
                bail!("backup units already exist; unschedule before replacing");
            }
            let quote = crate::background::quote;
            let content = format!(
                "[Unit]\nDescription=Drawer verified offline backup\n\n[Service]\nType=oneshot\nUMask=0077\nExecStart={} --data-dir {} backup scheduled --directory {}\n",
                quote(&std::env::current_exe()?)?,
                quote(&std::path::absolute(data)?)?,
                quote(&std::path::absolute(directory)?)?
            );
            use std::io::Write;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(service)?
                .write_all(content.as_bytes())?;
            let content = format!(
                "[Unit]\nDescription=Schedule Drawer backup\n\n[Timer]\nOnCalendar={}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n",
                calendar.name()
            );
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(timer)?
                .write_all(content.as_bytes())?;
            if !no_start {
                ctl(&["daemon-reload"])?;
                ctl(&["enable", "--now", "drawer-backup.timer"])?;
            }
            println!(
                "Installed {} backup timer. Each run briefly stops the user Drawer service.",
                calendar.name()
            );
        }
        BackupAction::Unschedule => {
            let dir = units()?;
            ctl(&["disable", "--now", "drawer-backup.timer"])?;
            std::fs::remove_file(dir.join("drawer-backup.timer"))?;
            std::fs::remove_file(dir.join("drawer-backup.service"))?;
            ctl(&["daemon-reload"])?;
        }
    }
    Ok(())
}
