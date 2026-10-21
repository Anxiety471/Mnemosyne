//! Read-only host capabilities. HTTP clients never choose an ambient filesystem root.
use crate::{
    error::{Error, Result},
    service::{Drawer, WriteOpts, WriteOutcome},
};
use axum::{
    Json,
    extract::{Path as RoutePath, Query, State},
    http::HeaderMap,
};
use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(clap::Subcommand)]
pub enum MountAction {
    /// Register an existing host directory as a read-only mount.
    Add { name: String, directory: PathBuf },
    /// List registered roots (including host paths, local CLI only).
    List,
    /// Remove a mount registration. Imported files and host files are preserved.
    Remove { name: String },
}
#[derive(Serialize, sqlx::FromRow)]
pub struct Mount {
    pub id: String,
    pub name: String,
}
#[derive(Serialize)]
pub struct HostEntry {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
    pub size: u64,
}
#[derive(Deserialize)]
pub struct Browse {
    #[serde(default)]
    path: String,
}
#[derive(Deserialize)]
pub struct Import {
    path: String,
    destination: String,
}

fn relative(path: &str) -> Result<PathBuf> {
    // Disallow platform-dependent interpretations even when serving on Unix.
    if path.contains('\\')
        || path.contains(':')
        || path.contains('\0')
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::Invalid(
            "use a relative mount path without parent components".into(),
        ));
    }
    Ok(if path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(path)
    })
}
fn open_root(root: &str) -> Result<Dir> {
    Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|_| Error::NotFound("registered host directory is unavailable".into()))
}
fn host_error(_: std::io::Error) -> Error {
    Error::Invalid("host path is unavailable or outside the registered mount".into())
}
async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
}
impl Drawer {
    pub async fn register_mount(&self, name: &str, directory: &Path) -> Result<Mount> {
        if name.trim().is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "mount name must contain 1 to 100 characters".into(),
            ));
        }
        let root = tokio::fs::canonicalize(directory).await?;
        if !tokio::fs::metadata(&root).await?.is_dir() {
            return Err(Error::Invalid("mount root must be a directory".into()));
        }
        let root = root
            .to_str()
            .ok_or_else(|| Error::Invalid("mount root must be UTF-8".into()))?;
        let mount = Mount {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
        };
        sqlx::query("INSERT INTO host_mounts(id,name,root) VALUES(?,?,?)")
            .bind(&mount.id)
            .bind(name)
            .bind(root)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                if e.as_database_error()
                    .is_some_and(|e| e.is_unique_violation())
                {
                    Error::Conflict("mount name already registered".into())
                } else {
                    e.into()
                }
            })?;
        Ok(mount)
    }
    async fn mount_root(&self, id: &str) -> Result<String> {
        sqlx::query_scalar("SELECT root FROM host_mounts WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound("mount".into()))
    }
}
pub async fn cli(s: &Drawer, action: MountAction) -> anyhow::Result<()> {
    match action {
        MountAction::Add { name, directory } => println!(
            "{}",
            serde_json::to_string_pretty(&s.register_mount(&name, &directory).await?)?
        ),
        MountAction::List => {
            let rows: Vec<(String, String, String)> =
                sqlx::query_as("SELECT id,name,root FROM host_mounts ORDER BY name")
                    .fetch_all(&s.pool)
                    .await?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        MountAction::Remove { name } => {
            if sqlx::query("DELETE FROM host_mounts WHERE name=?")
                .bind(name)
                .execute(&s.pool)
                .await?
                .rows_affected()
                == 0
            {
                anyhow::bail!("mount not found");
            }
        }
    }
    Ok(())
}
pub async fn list(State(s): State<Drawer>) -> Result<Json<Vec<Mount>>> {
    Ok(Json(
        sqlx::query_as("SELECT id,name FROM host_mounts ORDER BY name")
            .fetch_all(&s.pool)
            .await?,
    ))
}
pub async fn browse(
    State(s): State<Drawer>,
    RoutePath(id): RoutePath<String>,
    Query(q): Query<Browse>,
) -> Result<Json<Vec<HostEntry>>> {
    let path = relative(&q.path)?;
    let root = s.mount_root(&id).await?;
    Ok(Json(
        blocking(move || {
            let dir = open_root(&root)?.open_dir(&path).map_err(host_error)?;
            let mut result = Vec::new();
            for entry in dir.entries().map_err(host_error)? {
                let entry = entry.map_err(host_error)?;
                let kind = entry.file_type().map_err(host_error)?;
                if !kind.is_file() && !kind.is_dir() {
                    continue;
                }
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if result.len() >= 5000 {
                    return Err(Error::Invalid(
                        "host folder exceeds 5000 entries; use a smaller folder".into(),
                    ));
                }
                // Validate the returned path against the same portable grammar as incoming requests.
                let joined = path.join(&name).to_string_lossy().into_owned();
                if relative(&joined).is_err() {
                    continue;
                }
                result.push(HostEntry {
                    name,
                    path: joined,
                    kind: if kind.is_dir() { "folder" } else { "file" },
                    size: entry.metadata().map_err(host_error)?.len(),
                });
            }
            result.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(result)
        })
        .await?,
    ))
}
pub async fn import(
    State(s): State<Drawer>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    Json(body): Json<Import>,
) -> Result<Json<WriteOutcome>> {
    let path = relative(&body.path)?;
    let root = s.mount_root(&id).await?;
    tokio::fs::create_dir_all(&s.transfer_dir).await?;
    let staged = s
        .transfer_dir
        .join(format!("host-{}.part", uuid::Uuid::new_v4()));
    let output = staged.clone();
    let copied = blocking(move || {
        let dir = open_root(&root)?;
        if !dir.metadata(&path).map_err(host_error)?.is_file() {
            return Err(Error::Invalid("choose a regular file to import".into()));
        }
        let file = dir.open(&path).map_err(host_error)?;
        if !file.metadata().map_err(host_error)?.is_file() {
            return Err(Error::Invalid("choose a regular file to import".into()));
        }
        if file.metadata().map_err(host_error)?.len() > crate::MAX_UPLOAD_BYTES as u64 {
            return Err(Error::Invalid(
                "host file exceeds 512 MiB import limit".into(),
            ));
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?;
        let count = std::io::copy(
            &mut file.take(crate::MAX_UPLOAD_BYTES as u64 + 1),
            &mut output,
        )?;
        if count > crate::MAX_UPLOAD_BYTES as u64 {
            return Err(Error::Invalid("host file grew beyond import limit".into()));
        }
        Ok(())
    })
    .await;
    let result = match copied {
        Err(e) => Err(e),
        Ok(()) => {
            let actor = headers
                .get("x-drawer-author")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("api");
            let mut opts = WriteOpts::new(actor, "Imported from host mount");
            opts.if_head_revision = Some(0); // Never overwrite an existing Drawer file.
            s.write_staged(&body.destination, &staged, opts, None)
                .await
                .map(Json)
        }
    };
    let _ = tokio::fs::remove_file(&staged).await;
    result
}
