//! Offline snapshots: a verified round trip into a fresh store, the manifest's own
//! guarantees, the private user stores that live beside the root database, and the
//! `backup` subcommands as a person or an agent runs them.

use std::path::Path;

use drawer::{
    error::Error,
    service::{Drawer, NodeUpdate, Ref, WriteOpts},
};

/// A scratch directory that is removed only when the test says so, so a snapshot can be
/// created after the live store handle is gone.
struct Keep(std::path::PathBuf);
impl Keep {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("drawer-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Keep {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn by(author: &str) -> WriteOpts {
    WriteOpts::new(author, "")
}

/// The blob file for `hash` inside `root`.
fn blob_path(root: &Path, hash: &str) -> std::path::PathBuf {
    root.join("blobs").join(&hash[..2]).join(&hash[2..])
}

/// Every blob under `root/blobs` as `<hash> -> bytes`, plus the count of files that are
/// not valid blob names.
fn blobs_on_disk(root: &Path) -> (std::collections::HashMap<String, Vec<u8>>, usize) {
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

/// The directories under `root` that look like published snapshots.
fn snapshots(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .filter(|e| {
            let path = e.as_ref().unwrap().path();
            path.is_dir() && path.join("manifest.json").is_file()
        })
        .map(|e| e.unwrap().path())
        .collect();
    found.sort();
    found
}

/// The directories under `root` that are abandoned staging areas, not snapshots.
fn staging(root: &Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .starts_with(".stage-")
        })
        .map(|e| e.unwrap().path())
        .collect();
    found.sort();
    found
}

/// Read one text value out of a store without opening it as a Drawer.
async fn text(root: &Path, query: &str) -> String {
    let pool = open_store(root).await;
    let value: String = sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_string()))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    value
}

/// Read one integer out of a store without opening it as a Drawer.
async fn integer(root: &Path, query: &str) -> i64 {
    let pool = open_store(root).await;
    let value: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query.to_string()))
        .fetch_one(&pool)
        .await
        .unwrap();
    pool.close().await;
    value
}

/// A read-only handle to a store's database.
async fn open_store(root: &Path) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(root.join("drawer.db"))
                .read_only(true)
                .pragma("query_only", "ON"),
        )
        .await
        .unwrap()
}

/// The message of a failed verification, without requiring `Manifest: Debug`.
async fn verify_err(snapshot: &Path) -> String {
    match drawer::backup::verify(snapshot).await {
        Ok(_) => panic!("verification should have failed"),
        Err(error) => error.to_string(),
    }
}

/// Every regular file under `root`, as a `/`-separated path relative to `root`.
fn collect_files(root: &Path, dir: &Path, out: &mut std::collections::BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            out.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
    }
}

fn drawer(data: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--data-dir")
        .arg(data)
        .arg("--author")
        .arg("tester")
        .args(args)
        .output()
        .unwrap()
}
fn out(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn snapshot_round_trip_restores_contents_and_manifest_into_a_new_store() {
    let keep = Keep::new("round-trip");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");

    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/notes/todo.md", b"# todo\n", by("alice"))
            .await
            .unwrap();
        s.write("/notes/todo.md", b"# todo\n- ship it\n", by("alice"))
            .await
            .unwrap();
        s.write("/notes/ascii.txt", b"plain", by("bob"))
            .await
            .unwrap();
        s.write(
            "/notes/ünïcode 名.txt",
            "inhalte mit Ümläuten ✓".as_bytes(),
            by("carol"),
        )
        .await
        .unwrap();
        s.write("/bin/data.bin", &[0u8, 0xff, 0x00, 0x7f], by("dave"))
            .await
            .unwrap();
        s.mkdir("/notes/sub", "alice").await.unwrap();
        s.write("/notes/sub/deep.md", b"deep", by("alice"))
            .await
            .unwrap();
        // The same bytes are one blob, shared by two files.
        s.write("/shared/copy.txt", b"plain", by("bob"))
            .await
            .unwrap();
        s.write(
            "/notes/todo.md",
            b"# todo\n- ship it\n- done\n",
            by("alice"),
        )
        .await
        .unwrap();
    }

    let snapshot = drawer::backup::create(&data, &backups).await.unwrap();
    assert!(
        snapshot
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("drawer-")
    );
    assert!(
        staging(&backups).is_empty(),
        "no staging directory survives a successful backup"
    );
    assert_eq!(snapshots(&backups), vec![snapshot.clone()]);

    // The manifest is a complete, self-consistent description of the snapshot: every file
    // listed is present with the recorded size and SHA-256, and nothing else is in the tree.
    drawer::backup::verify(&snapshot)
        .await
        .expect("a fresh snapshot verifies");
    let manifest = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read(snapshot.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["version"], 1);
    assert!(
        manifest["created_at"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    let files = manifest["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["path"] == "drawer.db"));
    let mut listed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in files {
        let path = file["path"].as_str().unwrap().to_string();
        assert!(listed.insert(path.clone()), "manifest lists {path} once");
        let bytes = std::fs::read(snapshot.join(&path)).unwrap();
        assert_eq!(
            bytes.len() as u64,
            file["bytes"].as_u64().unwrap(),
            "{path} size"
        );
        assert_eq!(
            drawer::blob::BlobStore::hash(&bytes),
            file["sha256"].as_str().unwrap(),
            "{path} hash"
        );
    }
    // Every file in the snapshot is in the manifest, except the manifest itself.
    let mut on_disk = std::collections::BTreeSet::new();
    collect_files(&snapshot, &snapshot, &mut on_disk);
    on_disk.remove("manifest.json");
    assert_eq!(
        on_disk, listed,
        "the manifest covers exactly the snapshot's files"
    );
    assert!(files.len() > 1, "the manifest lists more than the database");

    // Restoring never touches the store the snapshot came from.
    let before = blobs_on_disk(&data);
    let target = keep.path().join("restored");
    drawer::backup::restore(&snapshot, &target).await.unwrap();
    assert_eq!(
        before,
        blobs_on_disk(&data),
        "the original store is untouched"
    );
    // The manifest is not part of the store, and neither is the live WAL.
    assert!(!target.join("manifest.json").exists());
    assert!(!target.join("drawer.db-wal").exists() && !target.join("drawer.db-shm").exists());

    let restored = Drawer::open(&target).await.unwrap();
    let (head, data) = restored
        .read(Ref::Path("/notes/todo.md"), None)
        .await
        .unwrap();
    assert_eq!(head.comment, "");
    assert_eq!(data, b"# todo\n- ship it\n- done\n");
    // Every revision the store had is still readable, with its own content.
    assert_eq!(
        restored
            .revisions(Ref::Path("/notes/todo.md"))
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        restored
            .read(Ref::Path("/notes/todo.md"), Some(1))
            .await
            .unwrap()
            .1,
        b"# todo\n"
    );
    assert_eq!(
        restored
            .read(Ref::Path("/notes/todo.md"), Some(2))
            .await
            .unwrap()
            .1,
        b"# todo\n- ship it\n"
    );
    // Non-ASCII names and binary content survive byte for byte.
    assert_eq!(
        restored
            .read(Ref::Path("/notes/ünïcode 名.txt"), None)
            .await
            .unwrap()
            .1,
        "inhalte mit Ümläuten ✓".as_bytes()
    );
    assert_eq!(
        restored
            .read(Ref::Path("/bin/data.bin"), None)
            .await
            .unwrap()
            .1,
        &[0u8, 0xff, 0x00, 0x7f]
    );
    assert_eq!(
        restored
            .read(Ref::Path("/notes/sub/deep.md"), None)
            .await
            .unwrap()
            .1,
        b"deep"
    );
    // `sub` is a folder, so /notes holds four entries: three files and a folder.
    assert_eq!(restored.list("/notes").await.unwrap().len(), 4);
    assert_eq!(
        restored
            .list("/notes")
            .await
            .unwrap()
            .iter()
            .filter(|n| n.kind == "file")
            .count(),
        3
    );
    // Deduplicated content stays deduplicated, and the shared blob is readable.
    assert!(blob_path(&target, &drawer::blob::BlobStore::hash(b"plain")).is_file());
    assert_eq!(
        restored
            .read(Ref::Path("/shared/copy.txt"), None)
            .await
            .unwrap()
            .1,
        b"plain"
    );

    // A restored store is a store: it takes revisions of its own, and its own snapshot.
    let next = restored
        .write("/notes/todo.md", b"# todo\n- new\n", by("alice"))
        .await
        .unwrap();
    assert_eq!(next.revision, 4);
    assert_eq!(
        restored
            .read(Ref::Path("/notes/todo.md"), Some(3))
            .await
            .unwrap()
            .1,
        b"# todo\n- ship it\n- done\n"
    );
    assert_eq!(
        restored
            .revisions(Ref::Path("/notes/todo.md"))
            .await
            .unwrap()
            .len(),
        4
    );

    // Restoring over an existing store is refused, so a restore can never clobber one.
    let err = match drawer::backup::restore(&snapshot, &target).await {
        Ok(_) => panic!("a second restore must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(err.contains("restore destination must not exist"), "{err}");
    assert_eq!(
        Drawer::open(&target)
            .await
            .unwrap()
            .read(Ref::Path("/notes/todo.md"), None)
            .await
            .unwrap()
            .1,
        b"# todo\n- new\n"
    );
    drop(restored);
}

#[tokio::test]
async fn tampered_snapshots_fail_verification_instead_of_restoring() {
    let keep = Keep::new("tamper");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/keep.txt", b"keep", by("alice")).await.unwrap();
        s.write("/keep.txt", b"keep two", by("alice"))
            .await
            .unwrap();
    }
    let snapshot = drawer::backup::create(&data, &backups).await.unwrap();
    drawer::backup::verify(&snapshot)
        .await
        .expect("a fresh snapshot verifies");

    // Rewriting a blob's bytes is caught by the manifest checksum.
    let keep_blob = blob_path(&snapshot, &drawer::blob::BlobStore::hash(b"keep"));
    assert!(keep_blob.is_file());
    std::fs::write(&keep_blob, b"tampered").unwrap();
    let err = verify_err(&snapshot).await;
    assert!(err.to_string().contains("checksum mismatch"), "{err}");
    assert!(
        drawer::backup::restore(&snapshot, &keep.path().join("restored-a"))
            .await
            .is_err()
    );

    // A missing referenced blob is caught too, and cannot be restored.
    std::fs::remove_file(&keep_blob).unwrap();
    let err = verify_err(&snapshot).await;
    assert!(
        err.contains("referenced blob missing") || err.to_lowercase().contains("no such file"),
        "{err}"
    );
    assert!(
        drawer::backup::restore(&snapshot, &keep.path().join("restored-b"))
            .await
            .is_err()
    );

    // Swapping in another revision's blob defeats the checksum rather than passing.
    let other = blob_path(&snapshot, &drawer::blob::BlobStore::hash(b"keep two"));
    std::fs::copy(&other, &keep_blob).unwrap();
    assert!(drawer::backup::verify(&snapshot).await.is_err());

    // Truncating the database is caught by SQLite's own integrity check: a snapshot whose
    // database cannot be read is never accepted.
    let db = std::fs::read(snapshot.join("drawer.db")).unwrap();
    std::fs::write(snapshot.join("drawer.db"), &db[..db.len() / 2]).unwrap();
    let err = verify_err(&snapshot).await;
    assert!(
        err.to_lowercase().contains("integrity")
            || err.to_lowercase().contains("not a database")
            || err.to_lowercase().contains("checksum mismatch"),
        "{err}"
    );

    // A manifest that omits a referenced file is refused, so the restore cannot ship a
    // file that would read as missing.
    let good = drawer::backup::create(&data, &backups).await.unwrap();
    let blob = blob_path(&good, &drawer::blob::BlobStore::hash(b"keep"));
    std::fs::remove_file(&blob).unwrap();
    let manifest = std::fs::read_to_string(good.join("manifest.json")).unwrap();
    let mut stripped: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let keep_path = blob
        .strip_prefix(&good)
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "/");
    stripped["files"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["path"] != keep_path);
    std::fs::write(
        good.join("manifest.json"),
        serde_json::to_vec_pretty(&stripped).unwrap(),
    )
    .unwrap();
    let err = verify_err(&good).await;
    assert!(
        err.contains("referenced blob missing") || err.to_lowercase().contains("no such file"),
        "{err}"
    );

    // A store that does not exist is reported, not half-snapshotted.
    let empty = keep.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(drawer::backup::create(&empty, &backups).await.is_err());
    assert!(
        staging(&backups).is_empty(),
        "a failed backup leaves no staging directory"
    );
}

#[tokio::test]
async fn snapshot_covers_private_user_stores_and_detects_a_missing_referenced_blob() {
    let keep = Keep::new("users");
    let data = keep.path().join("root");
    let backups = keep.path().join("backups");
    // The root store must exist before a user account can be registered in it.
    std::fs::create_dir_all(&data).unwrap();
    drop(Drawer::open(&data).await.unwrap());
    let id = uuid::Uuid::new_v4().to_string();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(data.join("drawer.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_accounts (id, username, password_hash, quota_bytes) VALUES (?, 'alice', 'test-only', 100)")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    std::fs::create_dir_all(data.join("users").join(&id)).unwrap();
    let tenant = Drawer::open(&data.join("users").join(&id)).await.unwrap();
    tenant
        .write("/secret", b"private backup", by("alice"))
        .await
        .unwrap();
    drop(tenant);

    let snapshot = drawer::backup::create(&data, &backups).await.unwrap();
    drawer::backup::verify(&snapshot)
        .await
        .expect("the private store is part of a valid snapshot");
    let tenant_db = snapshot.join("users").join(&id).join("drawer.db");
    assert!(
        tenant_db.is_file(),
        "the private store is part of the snapshot"
    );

    let target = keep.path().join("restored");
    drawer::backup::restore(&snapshot, &target).await.unwrap();
    let restored = Drawer::open(&target.join("users").join(&id)).await.unwrap();
    assert_eq!(
        restored.read(Ref::Path("/secret"), None).await.unwrap().1,
        b"private backup"
    );

    // A referenced blob that is gone from the snapshot cannot be restored as if present.
    let lost = drawer::backup::create(&data, &backups).await.unwrap();
    // The tenant's blobs live under its own store inside the snapshot.
    let lost_tenant = lost.join("users").join(&id);
    let hash = text(
        &lost_tenant,
        "SELECT hash FROM revisions WHERE node_id=(SELECT id FROM nodes WHERE name='secret')",
    )
    .await;
    // A snapshot keeps each tenant's blobs beside its own database, exactly as in the store.
    let candidates = [
        lost_tenant.join("blobs").join(&hash[..2]).join(&hash[2..]),
        lost.join("blobs").join(&hash[..2]).join(&hash[2..]),
    ];
    let lost_blob = candidates
        .iter()
        .find(|p| p.is_file())
        .expect("the tenant blob is in the snapshot");
    std::fs::remove_file(lost_blob).unwrap();
    let err = verify_err(&lost).await;
    assert!(
        err.contains("referenced blob missing") || err.to_lowercase().contains("no such file"),
        "{err}"
    );
}

#[tokio::test]
async fn snapshot_is_a_copy_of_the_tree_and_reads_without_the_original_store() {
    let keep = Keep::new("readonly");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/a/one.txt", b"one", by("alice")).await.unwrap();
        s.write("/a/two.txt", b"two", by("alice")).await.unwrap();
        s.write("/b/three.txt", b"three", by("alice"))
            .await
            .unwrap();
    }

    // Snapshotting takes the exclusive lease, so a held store refuses offline maintenance.
    let holder = Drawer::open(&data).await.unwrap();
    assert!(
        drawer::backup::create(&data, &backups).await.is_err(),
        "a held store refuses offline maintenance"
    );
    drop(holder);

    let snapshot = drawer::backup::create(&data, &backups).await.unwrap();
    let copy = Drawer::open(&snapshot).await.unwrap();
    assert_eq!(
        copy.read(Ref::Path("/a/one.txt"), None).await.unwrap().1,
        b"one"
    );
    assert_eq!(
        copy.read(Ref::Path("/b/three.txt"), None).await.unwrap().1,
        b"three"
    );
    // walk includes the folder itself, so /, /a, /b and the three files: 6 entries.
    assert_eq!(copy.walk("/", 32).await.unwrap().0.len(), 5);
    assert_eq!(
        integer(&snapshot, "SELECT COUNT(*) FROM nodes WHERE kind='file'").await,
        3
    );
    drop(copy);

    // New work in the original store is not in the snapshot.
    let s = Drawer::open(&data).await.unwrap();
    s.write("/a/four.txt", b"four", by("alice")).await.unwrap();
    drop(s);
    let copy = Drawer::open(&snapshot).await.unwrap();
    assert!(matches!(
        copy.read(Ref::Path("/a/four.txt"), None).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn restored_store_keeps_its_history_ids_and_health() {
    let keep = Keep::new("usable");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/keep/one.txt", b"one", by("alice")).await.unwrap();
        s.write("/keep/one.txt", b"two", by("alice")).await.unwrap();
        s.write("/keep/one.txt", b"three", by("alice"))
            .await
            .unwrap();
    }
    let snapshot = drawer::backup::create(&data, &backups).await.unwrap();
    let target = keep.path().join("restored");
    drawer::backup::restore(&snapshot, &target).await.unwrap();
    let s = Drawer::open(&target).await.unwrap();
    let out = s
        .write("/keep/one.txt", b"four", by("alice"))
        .await
        .unwrap();
    assert_eq!(out.revision, 4);
    assert_eq!(
        s.revisions(Ref::Path("/keep/one.txt")).await.unwrap().len(),
        4
    );
    assert_eq!(s.integrity_check().await.unwrap(), "ok");
    assert_eq!(integer(&target, "SELECT COUNT(*) FROM revisions WHERE node_id=(SELECT id FROM nodes WHERE name='one.txt')").await, 4);

    // Node ids, and the trash/restore path that needs them, survive a restore.
    let node = s.stat(Ref::Path("/keep/one.txt")).await.unwrap();
    let id = node.id.clone();
    s.trash(Ref::Id(&id), None, "alice").await.unwrap();
    assert_eq!(s.trash_list().await.unwrap().len(), 1);
    s.update(
        &id,
        NodeUpdate {
            trashed: Some(false),
            ..NodeUpdate::default()
        },
        "alice",
    )
    .await
    .unwrap();
    assert_eq!(
        s.read(Ref::Path("/keep/one.txt"), None).await.unwrap().1,
        b"four"
    );
    // The restored store's own row still points at its own node id.
    assert_eq!(
        integer(
            &target,
            &format!("SELECT head_revision FROM nodes WHERE id='{id}'")
        )
        .await,
        4
    );
}

#[tokio::test]
async fn each_snapshot_is_independent_and_complete() {
    let keep = Keep::new("second");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/one.txt", b"one", by("alice")).await.unwrap();
    }
    let first = drawer::backup::create(&data, &backups).await.unwrap();
    {
        let s = Drawer::open(&data).await.unwrap();
        s.write("/two.txt", b"two", by("alice")).await.unwrap();
    }
    let second = drawer::backup::create(&data, &backups).await.unwrap();
    assert_ne!(first, second, "each snapshot is its own directory");
    assert_eq!(snapshots(&backups).len(), 2);

    // The first snapshot never gains the second store's work, and both still verify.
    drawer::backup::verify(&first).await.unwrap();
    assert!(matches!(
        Drawer::open(&first)
            .await
            .unwrap()
            .read(Ref::Path("/two.txt"), None)
            .await,
        Err(Error::NotFound(_))
    ));
    drawer::backup::restore(&second, &keep.path().join("restored"))
        .await
        .unwrap();
    let restored = Drawer::open(&keep.path().join("restored")).await.unwrap();
    assert_eq!(
        restored.read(Ref::Path("/one.txt"), None).await.unwrap().1,
        b"one"
    );
    assert_eq!(
        restored.read(Ref::Path("/two.txt"), None).await.unwrap().1,
        b"two"
    );
}

#[test]
fn backup_commands_run_against_a_store_the_way_a_person_does() {
    let keep = Keep::new("cli");
    let data = keep.path().join("store");
    let backups = keep.path().join("backups");

    let note = keep.path().join("note.txt");
    std::fs::write(&note, "a note").unwrap();
    assert_eq!(
        out(drawer(&data, &["put", note.to_str().unwrap(), "/note.txt"])).trim(),
        "/note.txt saved as revision 1"
    );

    let created = out(drawer(
        &data,
        &["backup", "create", "--directory", backups.to_str().unwrap()],
    ));
    let created = created.trim();
    assert!(
        created.starts_with(&backups.display().to_string()),
        "{created}"
    );
    let snapshot = Path::new(created);
    assert!(snapshot.is_dir());

    let verified = out(drawer(
        &data,
        &["backup", "verify", snapshot.to_str().unwrap()],
    ));
    assert!(verified.contains("Verified"), "{verified}");
    assert!(
        out(drawer(
            &data,
            &["backup", "list", "--directory", backups.to_str().unwrap()]
        ))
        .contains("drawer-")
    );

    let target = keep.path().join("restored");
    let restored = out(drawer(
        &data,
        &[
            "backup",
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            target.to_str().unwrap(),
        ],
    ));
    assert!(
        restored.contains("Restored verified snapshot"),
        "{restored}"
    );
    assert_eq!(out(drawer(&target, &["cat", "/note.txt"])), "a note");

    // A second restore onto the same destination is refused.
    let second = drawer(
        &data,
        &[
            "backup",
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            target.to_str().unwrap(),
        ],
    );
    assert!(
        !second.status.success(),
        "restoring over an existing store must fail"
    );

    // A snapshot with a corrupted blob is reported, never restored as if it were sound.
    let blob = blob_path(snapshot, &drawer::blob::BlobStore::hash(b"a note"));
    std::fs::write(&blob, b"not the note").unwrap();
    let tampered = drawer(&data, &["backup", "verify", snapshot.to_str().unwrap()]);
    assert!(
        !tampered.status.success(),
        "a tampered snapshot must fail verification"
    );
    let failed = drawer(
        &data,
        &[
            "backup",
            "restore",
            snapshot.to_str().unwrap(),
            "--to",
            keep.path().join("refused").to_str().unwrap(),
        ],
    );
    assert!(
        !failed.status.success(),
        "a tampered snapshot cannot be restored"
    );
}
