//! Bounded-memory ZIP downloads. Revisions are snapshotted before the response starts.
use crate::{
    error::{Error, Result},
    service::{Drawer, MAX_WALK, Ref},
};
use async_zip::{Compression, ZipEntryBuilder};
use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf};
use tokio::io::{AsyncReadExt, AsyncWrite};
use tokio_util::compat::FuturesAsyncWriteCompatExt;

type Entry = (String, Option<PathBuf>);
#[derive(Deserialize)]
pub struct Selection {
    ids: String,
}

pub async fn download(
    State(s): State<Drawer>,
    Query(selection): Query<Selection>,
    headers: HeaderMap,
) -> Result<Response> {
    let ids: Vec<_> = selection
        .ids
        .split(',')
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() || ids.len() > 200 {
        return Err(Error::Invalid("select 1 to 200 files or folders".into()));
    }
    let mut nodes = BTreeMap::new();
    let _guard = s.write_lock.lock().await;
    for id in ids {
        let root = s.stat(Ref::Id(id)).await?;
        if root.trashed || s.stat(Ref::Path(&root.path)).await?.id != id {
            return Err(Error::NotFound("live selection".into()));
        }
        if root.kind == "folder" {
            let (children, truncated) = s.walk(&root.path, i64::MAX as usize).await?;
            if truncated {
                return Err(Error::Invalid(
                    "folder is too large for one archive; select smaller subfolders".into(),
                ));
            }
            for child in children {
                nodes.insert(child.id.clone(), child);
            }
        }
        if root.path != "/" {
            nodes.insert(root.id.clone(), root);
        }
        if nodes.len() > MAX_WALK {
            return Err(Error::Invalid("archive exceeds 5000 entries".into()));
        }
    }
    let mut entries = Vec::with_capacity(nodes.len());
    for node in nodes.values() {
        let name = node.path.trim_start_matches('/');
        if name.contains(['\\', ':'])
            || name
                .split('/')
                .any(|part| part.trim_end_matches(['.', ' ']).is_empty())
        {
            return Err(Error::Invalid("archive contains a filename that an extractor could treat as a path; rename it before downloading".into()));
        }
        if node.kind == "folder" {
            entries.push((format!("{name}/"), None));
        } else {
            let (_, path) = s
                .read_file_path(Ref::Id(&node.id), node.head_revision)
                .await?;
            entries.push((name.to_owned(), Some(path)));
        }
    }
    drop(_guard);
    let actor = headers
        .get("x-drawer-author")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("api");
    s.record_download(actor, "selection.zip", 0).await?;
    let (reader, writer) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move { write_zip(writer, entries).await });
    let stream =
        futures_lite::stream::unfold((reader, Some(task)), |(mut reader, task)| async move {
            let mut buffer = vec![0u8; 64 * 1024];
            match reader.read(&mut buffer).await {
                Ok(0) => match task {
                    Some(task) => match task.await {
                        Ok(Ok(())) => None,
                        result => Some((
                            Err::<Bytes, _>(std::io::Error::other(format!(
                                "archive failed: {result:?}"
                            ))),
                            (reader, None),
                        )),
                    },
                    None => None,
                },
                Ok(n) => {
                    buffer.truncate(n);
                    Some((Ok(Bytes::from(buffer)), (reader, task)))
                }
                Err(error) => Some((Err(error), (reader, None))),
            }
        });
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=selection.zip",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn write_zip<W: AsyncWrite + Unpin>(output: W, entries: Vec<Entry>) -> anyhow::Result<()> {
    let mut zip = async_zip::base::write::ZipFileWriter::with_tokio(output).force_zip64();
    for (name, path) in entries {
        let options = ZipEntryBuilder::new(name.into(), Compression::Deflate);
        let mut entry = zip.write_entry_stream(options).await?;
        if let Some(path) = path {
            let mut file = tokio::fs::File::open(path).await?;
            tokio::io::copy(&mut file, &mut (&mut entry).compat_write()).await?;
        }
        entry.close().await?;
    }
    zip.close().await?;
    Ok(())
}
