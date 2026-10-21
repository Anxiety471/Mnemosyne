//! Content-addressed blob storage on disk: `<root>/ab/cdef...` named by sha256.
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn hash(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    pub(crate) fn path_for(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid("bad blob hash".into()));
        }
        Ok(self.root.join(&hash[..2]).join(&hash[2..]))
    }

    /// Store bytes, returning their hash. Identical content is stored once.
    pub async fn put(&self, data: &[u8]) -> Result<String> {
        let hash = Self::hash(data);
        let path = self.path_for(&hash)?;
        if tokio::fs::try_exists(&path).await? {
            return Ok(hash);
        }
        let dir = path.parent().expect("blob path has a parent");
        tokio::fs::create_dir_all(dir).await?;
        let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), &hash[..16]));
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(hash)
    }

    /// Stream a staged file into content-addressed storage without loading it into RAM.
    /// Returns the hash, the length, and whether the blob was newly created (already-stored
    /// content reports `false`, so callers can skip charging it against a quota).
    pub async fn put_file(&self, staged: &Path) -> Result<(String, i64, bool)> {
        self.put_file_checked(staged, None).await
    }

    pub async fn put_file_checked(
        &self,
        staged: &Path,
        expected: Option<&str>,
    ) -> Result<(String, i64, bool)> {
        let mut file = tokio::fs::File::open(staged).await?;
        let mut sha = Sha256::new();
        let mut length = 0i64;
        let mut chunk = vec![0u8; 256 * 1024];
        loop {
            let n = file.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            sha.update(&chunk[..n]);
            length += n as i64;
        }
        let hash = hex::encode(sha.finalize());
        if expected.is_some_and(|expected| expected != hash) {
            return Err(Error::Invalid(
                "uploaded file SHA-256 checksum mismatch".into(),
            ));
        }
        let path = self.path_for(&hash)?;
        let created = !tokio::fs::try_exists(&path).await?;
        if created {
            tokio::fs::create_dir_all(path.parent().expect("blob directory")).await?;
            let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
            tokio::fs::copy(staged, &tmp).await?;
            tokio::fs::rename(&tmp, &path).await?;
        }
        Ok((hash, length, created))
    }

    /// Whether the blob for `hash` is on disk.
    pub async fn contains(&self, hash: &str) -> Result<bool> {
        Ok(tokio::fs::try_exists(self.path_for(hash)?).await?)
    }

    pub async fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.path_for(hash)?;
        tokio::fs::read(&path).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::Internal(format!("blob {hash} missing")),
            _ => e.into(),
        })
    }

    /// Stream a blob back in `chunk`-byte pieces so a consumer can stop at the first match
    /// instead of holding the whole blob in memory.
    pub async fn stream_chunks(&self, hash: &str, chunk: usize) -> Result<ChunkReader> {
        let file = tokio::fs::File::open(self.path_for(hash)?).await?;
        Ok(ChunkReader {
            file,
            chunk: chunk.max(1),
            carried: Vec::new(),
        })
    }
}

/// Reads a blob in fixed-size pieces, newest chunk last. A trailing partial UTF-8 sequence
/// is held back so every chunk is valid text on its own; the consumer's overlap carry makes
/// sure no match is split at a boundary.
pub struct ChunkReader {
    file: tokio::fs::File,
    chunk: usize,
    carried: Vec<u8>,
}

impl tokio::io::AsyncRead for ChunkReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let me = &mut *self;
        let want = buf.remaining().min(me.chunk);
        if want == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        // Drain bytes held back from the previous round first.
        if !me.carried.is_empty() {
            let n = me.carried.len().min(want);
            buf.put_slice(&me.carried[..n]);
            me.carried.drain(..n);
            return std::task::Poll::Ready(Ok(()));
        }
        let mut tmp = vec![0u8; want];
        let mut sub = tokio::io::ReadBuf::new(&mut tmp);
        match std::pin::Pin::new(&mut me.file).poll_read(cx, &mut sub) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Ok(())) => {
                let n = sub.filled().len();
                if n == 0 {
                    return std::task::Poll::Ready(Ok(()));
                }
                // Keep only whole characters; the incomplete tail is carried into the next chunk.
                let good = match std::str::from_utf8(&tmp[..n]) {
                    Ok(_) => n,
                    Err(e) => e.valid_up_to(),
                };
                buf.put_slice(&tmp[..good]);
                me.carried.extend_from_slice(&tmp[good..n]);
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
        }
    }
}
