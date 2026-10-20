//! Offline retention: what a preview must not touch, what an apply must remove, and the
//! invariants a destructive maintenance pass has to preserve.

use drawer::{
    cleanup::{CleanupArgs, cleanup},
    error::Error,
    service::{Drawer, Ref, WriteOpts},
};

/// A scratch directory that is removed when the test ends.
struct Keep(std::path::PathBuf);
impl Keep {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("drawer-cleanup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Keep {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn by(author: &str, comment: &str) -> WriteOpts {
    WriteOpts::new(author, comment)
}

fn args(keep: i64, trash_days: i64, orphan_days: u64, apply: bool) -> CleanupArgs {
    CleanupArgs {
        keep_revisions: keep,
        trash_days,
        orphan_days,
        apply,
    }
}

fn blob_path(root: &std::path::Path, hash: &str) -> std::path::PathBuf {
    root.join("blobs").join(&hash[..2]).join(&hash[2..])
}

/// Every blob under `root/blobs` as `<hash> -> bytes`, plus the count of files that are
/// not valid blob names.
fn blobs_on_disk(root: &std::path::Path) -> (std::collections::HashMap<String, Vec<u8>>, usize) {
    let mut found = std::collections::HashMap::new();
    let mut other = 0;
    let Ok(buckets) = std::fs::read_dir(root.join("blobs")) else {
        return (found, 0);
    };
    for bucket in buckets {
        for file in std::fs::read_dir(bucket.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            let hash = format!(
                "{}{}",
                path.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            );
            let bytes = std::fs::read(&path).unwrap();
            if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                found.insert(hash, bytes);
            } else {
                other += 1;
            }
        }
    }
    (found, other)
}

/// Sets a file's mtime to `age` seconds in the past without touching its contents.
fn age_file(path: &std::path::Path, age: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
    std::fs::File::open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

/// Ages every blob under `root/blobs` by `age` seconds.
fn age_blobs(root: &std::path::Path, age: u64) {
    for hash in blobs_on_disk(root).0.keys() {
        age_file(&blob_path(root, hash), age);
    }
}

async fn open(root: &std::path::Path) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(root.join("drawer.db"))
                .create_if_missing(false),
        )
        .await
        .unwrap()
}

/// A count straight out of the store's database, without opening it as a Drawer.
async fn count(root: &std::path::Path, query: &str) -> i64 {
    let pool = open(root).await;
    let value: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_string()))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    value
}

/// Runs a statement that changes metadata, so retention windows can be moved in time.
/// The statement is a test literal, never user input.
async fn update(root: &std::path::Path, statement: &str) -> u64 {
    let pool = open(root).await;
    let changed = sqlx::query(sqlx::AssertSqlSafe(statement.to_string()))
        .execute(&pool)
        .await
        .unwrap()
        .rows_affected();
    pool.close().await;
    changed
}

#[tokio::test]
async fn preview_deletes_nothing_and_apply_removes_exactly_what_retention_selects() {
    let keep = Keep::new("dry-run");
    let data = keep.path().join("store");
    let gone_id: String;
    {
        let s = Drawer::open(&data).await.unwrap();
        for content in ["one", "two", "three", "four"] {
            s.write("/history", content.as_bytes(), by("api", content))
                .await
                .unwrap();
        }
        // Two files sharing one blob.
        s.write("/shared/a", b"two", by("api", "")).await.unwrap();
        s.write("/shared/b", b"two", by("api", "")).await.unwrap();
        // A trashed file, which is nothing but its own revisions.
        let gone = s.write("/gone", b"trash", by("api", "")).await.unwrap();
        gone_id = gone.node.id;
        s.trash(Ref::Id(&gone_id), None, "api").await.unwrap();
    }
    drop(Drawer::open(&data).await.unwrap());
    // Age the trash past the retention window and every blob past the orphan grace period.
    assert_eq!(
        update(
            &data,
            "UPDATE nodes SET modified_at='2000-01-01T00:00:00.000Z' WHERE trashed=1"
        )
        .await,
        1
    );
    age_blobs(&data, 4 * 86400);

    let before_blobs = blobs_on_disk(&data);
    let before_rev = count(&data, "SELECT COUNT(*) FROM revisions").await;
    let before_nodes = count(&data, "SELECT COUNT(*) FROM nodes").await;

    // Preview: nothing on disk or in the database moves.
    let report = cleanup(&data, &args(1, 1, 1, false)).await.unwrap();
    assert!(!report.applied);
    assert_eq!(report.trash_nodes, 1, "the trashed file is a candidate");
    // With one revision kept, only the head survives retention: revisions 1, 2 and 3 are
    // candidates, and the trashed file's own revision is purged with it.
    assert_eq!(
        report.old_revisions, 3,
        "revisions 1, 2 and 3; the head is always kept"
    );
    // Revision 2's blob is shared with /shared/a and /shared/b, so it is not an orphan;
    // revisions 1 and 3 and the trashed file's blob have nothing pointing at them.
    assert_eq!(
        report.orphan_blobs, 3,
        "revision 1, revision 3 and the trashed file's blob"
    );
    assert_eq!(report.orphan_bytes, 3 + 5 + 5);
    assert!(report.orphan_bytes > 0);
    assert_eq!(
        before_blobs,
        blobs_on_disk(&data),
        "preview must not unlink a blob"
    );
    assert_eq!(
        before_rev,
        count(&data, "SELECT COUNT(*) FROM revisions").await,
        "preview must not delete revisions"
    );
    assert_eq!(
        before_nodes,
        count(&data, "SELECT COUNT(*) FROM nodes").await,
        "preview must not delete nodes"
    );
    let preview = Drawer::open(&data).await.unwrap();
    assert_eq!(
        preview
            .read(Ref::Path("/history"), Some(1))
            .await
            .unwrap()
            .1,
        b"one"
    );
    assert_eq!(
        preview
            .read(Ref::Path("/history"), Some(3))
            .await
            .unwrap()
            .1,
        b"three"
    );
    assert_eq!(preview.trash_list().await.unwrap().len(), 1);
    drop(preview);

    // Apply: the same policy, really executed.
    let applied = cleanup(&data, &args(1, 1, 1, true)).await.unwrap();
    assert!(applied.applied);
    assert_eq!(applied.trash_nodes, report.trash_nodes);
    assert_eq!(applied.old_revisions, report.old_revisions);
    assert_eq!(applied.orphan_blobs, report.orphan_blobs);

    let s = Drawer::open(&data).await.unwrap();
    assert_eq!(
        s.stats().await.unwrap().revisions,
        before_rev - 4,
        "three old revisions plus the trashed file's revision"
    );
    assert_eq!(
        s.stats().await.unwrap().trashed,
        0,
        "expired trash is purged"
    );
    // The head is always kept; with one revision retained, revision 1 is not.
    assert_eq!(
        s.read(Ref::Path("/history"), None).await.unwrap().1,
        b"four"
    );
    assert_eq!(s.revisions(Ref::Path("/history")).await.unwrap().len(), 1);
    // What fell outside the window is really gone, not merely unreferenced.
    for gone in [1, 2, 3] {
        assert!(
            matches!(
                s.read(Ref::Path("/history"), Some(gone)).await,
                Err(Error::NotFound(_))
            ),
            "revision {gone} is gone"
        );
    }
    assert!(matches!(
        s.read(Ref::Path("/gone"), None).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        s.stat(Ref::Id(&gone_id)).await,
        Err(Error::NotFound(_))
    ));
    // Shared content is still readable, from both files.
    assert_eq!(
        s.read(Ref::Path("/shared/a"), None).await.unwrap().1,
        b"two"
    );
    assert_eq!(
        s.read(Ref::Path("/shared/b"), None).await.unwrap().1,
        b"two"
    );
    assert!(
        s.missing_blobs().await.unwrap().is_empty(),
        "no surviving revision points at a deleted blob"
    );
    // Only the head's blob remains: revision 1's blob was also unreferenced once its
    // revision went, and the shared "two" blob is still pointed at by /shared/*.
    let (after, other) = blobs_on_disk(&data);
    assert!(other == 0, "no non-blob files under blobs/");
    for rev in s.revisions(Ref::Path("/history")).await.unwrap() {
        assert!(
            after.contains_key(&rev.hash),
            "blob for surviving revision {}",
            rev.revision
        );
    }
    assert!(after.contains_key(&drawer::blob::BlobStore::hash(b"two")));
    assert_eq!(after.len(), 2, "the head's blob and the shared blob");
}

#[tokio::test]
async fn cleanup_never_removes_a_head_or_a_referenced_blob() {
    let keep = Keep::new("invariants");
    let data = keep.path().join("store");
    let (pinned_token, head_blob, history_id): (String, String, String);
    {
        let s = Drawer::open(&data).await.unwrap();
        for content in ["one", "two", "three"] {
            s.write("/history", content.as_bytes(), by("api", ""))
                .await
                .unwrap();
        }
        history_id = s.stat(Ref::Path("/history")).await.unwrap().id;
        // A link pins revision 1: its blob must survive even though retention would drop it.
        let link = s
            .create_download_link_with_revision(&history_id, "api", 7, Some(1))
            .await
            .unwrap();
        pinned_token = link.token;
        // A second file shares the head's bytes, so the blob is referenced twice.
        s.write("/twin", b"three", by("api", "")).await.unwrap();
        head_blob = s.read(Ref::Path("/history"), None).await.unwrap().0.hash;
        // A file with no history beyond its head, and a folder that holds something.
        s.write("/single", b"solo", by("api", "")).await.unwrap();
        s.write("/folder/inside", b"nested", by("api", ""))
            .await
            .unwrap();
    }
    drop(Drawer::open(&data).await.unwrap());
    age_blobs(&data, 10 * 86400);

    let applied = cleanup(&data, &args(1, 30, 1, true)).await.unwrap();
    // Nothing is trashed, so only revision 2 falls outside the window; revisions 1 and 3 stay.
    assert_eq!(applied.trash_nodes, 0);
    assert_eq!(applied.old_revisions, 1);
    assert_eq!(
        applied.orphan_blobs, 1,
        "revision 2's blob is unreferenced once its revision goes"
    );

    let s = Drawer::open(&data).await.unwrap();
    assert_eq!(
        s.read(Ref::Path("/history"), None).await.unwrap().1,
        b"three",
        "the head is never removed"
    );
    assert_eq!(
        s.read(Ref::Path("/history"), Some(1)).await.unwrap().1,
        b"one",
        "a pinned revision is kept"
    );
    assert!(matches!(
        s.read(Ref::Path("/history"), Some(2)).await,
        Err(Error::NotFound(_))
    ));
    // The link still serves the revision it was created for.
    let (_, _, path) = s.download_link_file(&pinned_token).await.unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"one");
    // The shared head blob survives because the twin still points at it.
    assert_eq!(s.read(Ref::Path("/twin"), None).await.unwrap().1, b"three");
    assert!(blob_path(&data, &head_blob).is_file());
    // Files with a single revision and folders with children are left alone.
    assert_eq!(s.read(Ref::Path("/single"), None).await.unwrap().1, b"solo");
    assert_eq!(
        s.read(Ref::Path("/folder/inside"), None).await.unwrap().1,
        b"nested"
    );
    assert_eq!(s.stats().await.unwrap().files, 4);
    assert!(s.missing_blobs().await.unwrap().is_empty());
    // A second pass is a no-op: cleanup is idempotent. The store handle must go first,
    // because maintenance takes the exclusive lease.
    drop(s);
    let again = cleanup(&data, &args(1, 30, 1, true)).await.unwrap();
    assert_eq!(again.old_revisions, 0);
    assert_eq!(again.orphan_blobs, 0);
}

#[tokio::test]
async fn a_live_download_link_pins_a_revision_and_an_expired_one_does_not() {
    let keep = Keep::new("pins");
    let data = keep.path().join("store");
    let (live_token, expired_id, expired_token, history_id): (String, String, String, String);
    {
        let s = Drawer::open(&data).await.unwrap();
        for content in ["one", "two", "three", "four"] {
            s.write("/history", content.as_bytes(), by("api", ""))
                .await
                .unwrap();
        }
        let id = s.stat(Ref::Path("/history")).await.unwrap().id;
        history_id = id.clone();
        let live = s
            .create_download_link_with_revision(&id, "api", 30, Some(1))
            .await
            .unwrap();
        let expired = s
            .create_download_link_with_revision(&id, "api", 30, Some(2))
            .await
            .unwrap();
        expired_id = expired.id;
        live_token = live.token;
        expired_token = expired.token;
    }
    drop(Drawer::open(&data).await.unwrap());
    age_blobs(&data, 10 * 86400);
    // The second link has lapsed, so its revision is fair game for retention.
    let pool = open(&data).await;
    sqlx::query("UPDATE download_links SET expires_at='2000-01-01T00:00:00.000Z' WHERE id=?")
        .bind(&expired_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let applied = cleanup(&data, &args(1, 30, 1, true)).await.unwrap();
    // Revisions 2 and 3 are unprotected: 3 is the newest inside the window, but the window
    // keeps only the newest non-head revision, so 3 goes with 2.
    assert_eq!(
        applied.old_revisions, 2,
        "revisions 2 and 3 are unprotected"
    );
    let s = Drawer::open(&data).await.unwrap();
    assert_eq!(
        s.read(Ref::Path("/history"), Some(1)).await.unwrap().1,
        b"one",
        "the live link keeps its revision"
    );
    assert!(
        matches!(
            s.read(Ref::Path("/history"), Some(2)).await,
            Err(Error::NotFound(_))
        ),
        "the expired link no longer pins anything"
    );
    assert!(matches!(
        s.read(Ref::Path("/history"), Some(3)).await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        s.read(Ref::Path("/history"), None).await.unwrap().1,
        b"four"
    );
    assert!(s.download_link_file(&live_token).await.is_ok());
    // The expired link is dead either way, and its revision did not survive for it.
    assert!(s.download_link_file(&expired_token).await.is_err());
    assert_eq!(
        s.list_download_links(&history_id, "api")
            .await
            .unwrap()
            .len(),
        2,
        "the link rows stay; only the pin is gone"
    );
}

#[tokio::test]
async fn orphan_blobs_are_collected_only_when_unreferenced_and_aged() {
    let keep = Keep::new("orphans");
    let data = keep.path().join("store");
    let live_hash = drawer::blob::BlobStore::hash(b"still referenced");
    let recent_hash = drawer::blob::BlobStore::hash(b"unreferenced but young");
    let aged_hash = drawer::blob::BlobStore::hash(b"unreferenced and old");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/keep", b"still referenced", by("api", ""))
            .await
            .unwrap();
        // Two blobs with no revision pointing at them: one fresh, one aged.
        drawer::blob::BlobStore::new(data.join("blobs"))
            .put(b"unreferenced but young")
            .await
            .unwrap();
        drawer::blob::BlobStore::new(data.join("blobs"))
            .put(b"unreferenced and old")
            .await
            .unwrap();
    }
    drop(Drawer::open(&data).await.unwrap());
    age_blobs(&data, 10 * 86400);
    // The live blob and the young orphan are made young again: cleanup must not collect
    // them, while the aged orphan has no revision pointing at it.
    age_file(&blob_path(&data, &live_hash), 0);
    let recent_path = blob_path(&data, &recent_hash);
    age_file(&recent_path, 0);

    let applied = cleanup(&data, &args(10, 30, 1, true)).await.unwrap();
    assert_eq!(applied.orphan_blobs, 1, "only the aged unreferenced blob");
    assert_eq!(applied.orphan_bytes, b"unreferenced and old".len() as u64);
    assert!(
        recent_path.is_file(),
        "a recent orphan is left for the next run"
    );
    assert!(
        blob_path(&data, &live_hash).is_file(),
        "a referenced blob is never collected"
    );
    assert!(
        !blob_path(&data, &aged_hash).exists(),
        "the aged orphan is gone"
    );

    // A file under blobs/ whose name is not a SHA-256 digest is not a blob at all.
    let stray = data.join("blobs").join("zz").join("not-a-hash");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(&stray, b"junk").unwrap();
    age_file(&stray, 30 * 86400);
    let report = cleanup(&data, &args(10, 30, 1, true)).await.unwrap();
    assert_eq!(
        report.orphan_blobs, 0,
        "a malformed blob name is never deleted"
    );
    assert!(stray.is_file());
}

#[tokio::test]
async fn expired_trash_takes_its_children_and_grants_while_recent_trash_survives() {
    let keep = Keep::new("trash");
    let data = keep.path().join("store");
    let (folder_id, child_id, recent_id): (String, String, String);
    {
        let s = Drawer::open(&data).await.unwrap();
        s.mkdir("/projects/old", "api").await.unwrap();
        let child = s
            .write("/projects/old/note.md", b"note", by("api", ""))
            .await
            .unwrap();
        let folder = s.stat(Ref::Path("/projects/old")).await.unwrap();
        s.share(&folder.id, "bob", "writer", "api").await.unwrap();
        s.write("/projects/old/note.md", b"note two", by("api", ""))
            .await
            .unwrap();
        s.trash(Ref::Path("/projects/old"), None, "api")
            .await
            .unwrap();
        // A file trashed at the same time, which must stay in the trash.
        let recent = s
            .write("/projects/recent", b"recent", by("api", ""))
            .await
            .unwrap();
        s.trash(Ref::Id(&recent.node.id), None, "api")
            .await
            .unwrap();
        folder_id = folder.id;
        child_id = child.node.id;
        recent_id = recent.node.id;
    }
    drop(Drawer::open(&data).await.unwrap());
    age_blobs(&data, 10 * 86400);
    // Only the folder subtree is past the trash retention; the file is still recent.
    let pool = open(&data).await;
    sqlx::query("UPDATE nodes SET modified_at='2000-01-01T00:00:00.000Z' WHERE id IN (SELECT id FROM nodes WHERE parent_id=? UNION SELECT ?)")
        .bind(&folder_id)
        .bind(&folder_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let applied = cleanup(&data, &args(1, 1, 1, true)).await.unwrap();
    assert_eq!(
        applied.trash_nodes, 2,
        "the folder and its child are expired trash"
    );
    let s = Drawer::open(&data).await.unwrap();
    assert!(
        matches!(s.stat(Ref::Id(&folder_id)).await, Err(Error::NotFound(_))),
        "the folder is purged"
    );
    assert!(
        matches!(s.stat(Ref::Id(&child_id)).await, Err(Error::NotFound(_))),
        "its child is purged with it"
    );
    assert_eq!(count(&data, "SELECT COUNT(*) FROM revisions WHERE node_id=(SELECT id FROM nodes WHERE name='note.md')").await, 0, "the child's revisions are purged");
    assert_eq!(
        count(
            &data,
            "SELECT COUNT(*) FROM permissions WHERE principal='bob'"
        )
        .await,
        0,
        "its grants are purged"
    );
    // The recent trashed file is still in the trash, revisions and all.
    assert!(s.stat(Ref::Id(&recent_id)).await.unwrap().trashed);
    assert_eq!(
        s.read(Ref::Id(&recent_id), None).await.unwrap().1,
        b"recent"
    );
    assert!(
        matches!(
            s.read(Ref::Path("/projects/recent"), None).await,
            Err(Error::NotFound(_))
        ),
        "trashed is not a live path"
    );
    assert!(
        s.missing_blobs().await.unwrap().is_empty(),
        "no surviving revision lost its blob"
    );
}

#[tokio::test]
async fn retention_is_validated_and_a_missing_store_is_reported() {
    let keep = Keep::new("invalid");
    let data = keep.path().join("store");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/a", b"x", by("api", "")).await.unwrap();
    }
    drop(Drawer::open(&data).await.unwrap());
    for bad in [
        args(0, 1, 1, false),
        args(1, 0, 1, false),
        args(1, 1, 0, false),
    ] {
        assert!(
            cleanup(&data, &bad).await.is_err(),
            "retention below one is refused"
        );
    }
    assert!(cleanup(&data, &args(1, 36501, 1, false)).await.is_err());
    assert!(
        cleanup(&keep.path().join("missing"), &args(1, 1, 1, false))
            .await
            .is_err()
    );
    assert!(!keep.path().join("missing").exists());
}
