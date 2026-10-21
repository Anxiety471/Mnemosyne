use drawer::{
    blob::BlobStore,
    error::Error,
    service::{Drawer, Edit, NodeUpdate, Properties, Ref, WriteOpts},
};

async fn store() -> (Drawer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Drawer::open(dir.path()).await.unwrap(), dir)
}

fn by(author: &str, comment: &str) -> WriteOpts {
    WriteOpts::new(author, comment)
}

fn guarded(expected: i64) -> WriteOpts {
    WriteOpts {
        if_head_revision: Some(expected),
        ..WriteOpts::new("t", "")
    }
}

fn props(pairs: &[(&str, &str)]) -> Properties {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn restore() -> NodeUpdate {
    NodeUpdate {
        trashed: Some(false),
        ..NodeUpdate::default()
    }
}

#[tokio::test]
async fn write_read_revisions_and_rollback() {
    let (m, _d) = store().await;
    let v1 = m
        .write("/notes/a.md", b"one", by("alice", "first"))
        .await
        .unwrap();
    assert!(v1.new_revision);
    assert_eq!(v1.revision, 1);
    let v2 = m
        .write("notes/a.md", b"two", by("bob", "second"))
        .await
        .unwrap();
    assert_eq!(v2.revision, 2);
    // same file, same id: a revision is not a new file
    assert_eq!(v2.node.id, v1.node.id);

    // identical content does not create a revision
    let same = m
        .write("/notes/a.md", b"two", by("bob", "again"))
        .await
        .unwrap();
    assert!(!same.new_revision);
    assert_eq!(same.revision, 2);

    assert_eq!(
        m.read(Ref::Path("/notes/a.md"), None).await.unwrap().1,
        b"two"
    );
    assert_eq!(
        m.read(Ref::Path("/notes/a.md"), Some(1)).await.unwrap().1,
        b"one"
    );

    let rb = m
        .rollback(Ref::Path("/notes/a.md"), 1, by("alice", ""))
        .await
        .unwrap();
    assert_eq!(rb.revision, 3);
    assert_eq!(rb.node.head_revision, Some(3));
    assert_eq!(
        m.read(Ref::Path("/notes/a.md"), None).await.unwrap().1,
        b"one"
    );

    let revs = m.revisions(Ref::Path("/notes/a.md")).await.unwrap();
    assert_eq!(
        revs.iter().map(|r| r.revision).collect::<Vec<_>>(),
        [3, 2, 1]
    );
    assert_eq!(revs[0].comment, "rollback to revision 1");
    // revisions 1 and 3 share one blob
    assert_eq!(revs[0].hash, revs[2].hash);
    // history is untouched: revision 2 is still readable
    assert_eq!(
        m.read(Ref::Path("/notes/a.md"), Some(2)).await.unwrap().1,
        b"two"
    );
}

#[tokio::test]
async fn folders_listing_and_conflicts() {
    let (m, _d) = store().await;
    m.write("/a/b/c.txt", b"x", by("t", "")).await.unwrap();
    m.mkdir("/a/empty", "t").await.unwrap();

    let root = m.list("/").await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].path, "/a");
    let a: Vec<_> = m
        .list("/a")
        .await
        .unwrap()
        .into_iter()
        .map(|n| (n.name, n.kind))
        .collect();
    assert_eq!(
        a,
        [("b".to_string(), "folder"), ("empty".to_string(), "folder")]
    );
    let b = m.list("/a/b").await.unwrap();
    assert_eq!(b[0].kind, "file");
    assert_eq!(b[0].size, Some(1));
    assert_eq!(b[0].head_revision, Some(1));

    assert!(matches!(
        m.mkdir("/a/b/c.txt", "t").await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        m.write("/a/b", b"x", by("t", "")).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        m.write("/a/b/c.txt/d", b"x", by("t", "")).await,
        Err(Error::Conflict(_))
    ));

    // trashing an empty folder hides it, as trashing any folder does
    m.trash(Ref::Path("/a/empty"), None, "t").await.unwrap();
    assert!(matches!(m.list("/a/empty").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn trash_restore_and_revive_keep_the_node() {
    let (m, _d) = store().await;
    let id = m
        .write("/f.txt", b"one", by("t", ""))
        .await
        .unwrap()
        .node
        .id;

    let t = m.trash(Ref::Path("/f.txt"), None, "t").await.unwrap();
    assert!(t.trashed);
    assert!(matches!(
        m.read(Ref::Path("/f.txt"), None).await,
        Err(Error::NotFound(_))
    ));
    assert!(m.list("/").await.unwrap().is_empty());
    let bin = m.trash_list().await.unwrap();
    assert_eq!(bin.len(), 1);
    assert_eq!(
        (bin[0].id.as_str(), bin[0].path.as_str()),
        (id.as_str(), "/f.txt")
    );

    m.update(&id, restore(), "t").await.unwrap();
    assert_eq!(m.read(Ref::Path("/f.txt"), None).await.unwrap().1, b"one");

    // trash again, then writing the path revives the same node with a new revision
    m.trash(Ref::Id(&id), None, "t").await.unwrap();
    let back = m.write("/f.txt", b"two", by("t", "back")).await.unwrap();
    assert_eq!(back.node.id, id);
    assert_eq!(back.revision, 2);
    assert!(!back.node.trashed);
    assert!(m.trash_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn trashing_a_folder_takes_its_contents_and_only_the_top_shows_in_the_trash() {
    let (m, _d) = store().await;
    let x_id = m
        .write("/d/x.txt", b"x", by("t", ""))
        .await
        .unwrap()
        .node
        .id;
    let d_id = m.stat(Ref::Path("/d")).await.unwrap().id;

    m.trash(Ref::Path("/d"), None, "t").await.unwrap();
    assert!(matches!(
        m.stat(Ref::Path("/d/x.txt")).await,
        Err(Error::NotFound(_))
    ));
    let bin = m.trash_list().await.unwrap();
    assert_eq!(
        bin.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        ["d"]
    );
    assert!(m.stat(Ref::Id(&x_id)).await.unwrap().trashed);

    // a child cannot come back while its folder is still in the trash
    assert!(matches!(
        m.update(&x_id, restore(), "t").await,
        Err(Error::Conflict(_))
    ));
    m.update(&d_id, restore(), "t").await.unwrap();
    assert_eq!(m.read(Ref::Path("/d/x.txt"), None).await.unwrap().1, b"x");
}

#[tokio::test]
async fn purge_needs_the_trash_and_removes_history() {
    let (m, _d) = store().await;
    let file_id = m
        .write("/gone/a.txt", b"bye", by("t", ""))
        .await
        .unwrap()
        .node
        .id;
    let gone = m.stat(Ref::Path("/gone")).await.unwrap().id;

    assert!(matches!(m.purge(&gone, "t").await, Err(Error::Conflict(_))));
    assert!(matches!(m.purge("root", "t").await, Err(Error::Invalid(_))));

    m.trash(Ref::Path("/gone"), None, "t").await.unwrap();
    m.purge(&gone, "t").await.unwrap();
    assert!(matches!(
        m.stat(Ref::Id(&gone)).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        m.stat(Ref::Id(&file_id)).await,
        Err(Error::NotFound(_))
    ));
    assert!(m.trash_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn create_only_precondition() {
    let (m, _d) = store().await;
    // 0 means "must not exist": the first create passes
    m.write("/p.txt", b"one", guarded(0)).await.unwrap();
    // a second create-only write is refused, leaving the file untouched
    let err = m.write("/p.txt", b"two", guarded(0)).await.unwrap_err();
    assert!(matches!(err, Error::PreconditionFailed(_)));
    assert_eq!(m.read(Ref::Path("/p.txt"), None).await.unwrap().1, b"one");
    assert_eq!(m.revisions(Ref::Path("/p.txt")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn stale_writer_is_refused() {
    let (m, _d) = store().await;
    m.write("/plan.md", b"v1", by("a", "")).await.unwrap();
    // agent A reads revision 1, agent B lands revision 2
    m.write("/plan.md", b"v2 by B", by("b", "")).await.unwrap();

    // A's write, guarded on what it read, must fail instead of clobbering B
    let err = m
        .write("/plan.md", b"v2 by A", guarded(1))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::PreconditionFailed(ref msg) if msg.contains("revision 2")));
    assert_eq!(
        m.read(Ref::Path("/plan.md"), None).await.unwrap().1,
        b"v2 by B"
    );

    // re-reading and guarding on the current revision goes through
    let ok = m.write("/plan.md", b"merged", guarded(2)).await.unwrap();
    assert_eq!(ok.revision, 3);
}

#[tokio::test]
async fn guarded_trash() {
    let (m, _d) = store().await;
    m.write("/d.txt", b"x", by("t", "")).await.unwrap();
    m.write("/d.txt", b"y", by("t", "")).await.unwrap();
    assert!(matches!(
        m.trash(Ref::Path("/d.txt"), Some(1), "t").await,
        Err(Error::PreconditionFailed(_))
    ));
    assert!(m.stat(Ref::Path("/d.txt")).await.is_ok());
    m.trash(Ref::Path("/d.txt"), Some(2), "t").await.unwrap();
}

#[tokio::test]
async fn properties_are_stored_per_revision() {
    let (m, _d) = store().await;
    let mut opts = by("t", "");
    opts.properties = props(&[("source", "web"), ("topic", "rust")]);
    let w = m.write("/m.md", b"same", opts).await.unwrap();
    assert_eq!(
        w.node.properties,
        props(&[("source", "web"), ("topic", "rust")])
    );

    // same bytes, different properties: that is a real change, so a new revision
    let mut opts = by("t", "");
    opts.properties = props(&[("source", "api")]);
    let w2 = m.write("/m.md", b"same", opts).await.unwrap();
    assert!(w2.new_revision);
    assert_eq!(
        m.read(Ref::Path("/m.md"), Some(1))
            .await
            .unwrap()
            .0
            .properties["source"],
        "web"
    );
    assert_eq!(
        m.stat(Ref::Path("/m.md")).await.unwrap().properties,
        props(&[("source", "api")])
    );

    // bad keys are refused before anything is stored
    let mut bad = by("t", "");
    bad.properties = props(&[("has space", "x")]);
    assert!(matches!(
        m.write("/m.md", b"z", bad).await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(m.revisions(Ref::Path("/m.md")).await.unwrap().len(), 2);
}

#[tokio::test]
async fn copy_shares_content_and_keeps_the_source() {
    let (m, _d) = store().await;
    let mut opts = by("t", "");
    opts.properties = props(&[("kind", "note")]);
    m.write("/src/a.md", b"alpha", opts).await.unwrap();
    m.write("/src/a.md", b"beta", by("t", "")).await.unwrap();

    let c = m
        .copy(
            Ref::Path("/src/a.md"),
            "/dst/a.md",
            Some(1),
            by("t", "copied"),
        )
        .await
        .unwrap();
    assert!(c.new_revision);
    assert_eq!(c.revision, 1);
    assert_eq!(c.node.properties, props(&[("kind", "note")]));
    assert_eq!(
        m.read(Ref::Path("/dst/a.md"), None).await.unwrap().1,
        b"alpha"
    );
    // the source is untouched, and the copy is a different node
    assert_eq!(
        m.read(Ref::Path("/src/a.md"), None).await.unwrap().1,
        b"beta"
    );
    assert_ne!(c.node.id, m.stat(Ref::Path("/src/a.md")).await.unwrap().id);

    // copying the same revision onto the same place changes nothing
    let again = m
        .copy(Ref::Path("/src/a.md"), "/dst/a.md", Some(1), by("t", ""))
        .await
        .unwrap();
    assert!(!again.new_revision);

    // a guarded copy respects the destination's head
    assert!(matches!(
        m.copy(Ref::Path("/src/a.md"), "/dst/a.md", None, guarded(0))
            .await,
        Err(Error::PreconditionFailed(_))
    ));
}

#[tokio::test]
async fn move_and_rename_keep_id_history_and_children() {
    let (m, _d) = store().await;
    let id = m
        .write("/inbox/note.md", b"hi", by("t", ""))
        .await
        .unwrap()
        .node
        .id;

    let moved = m
        .move_to(Ref::Id(&id), "/archive/2026/note.md", "t")
        .await
        .unwrap();
    assert_eq!(moved.id, id);
    assert_eq!(moved.path, "/archive/2026/note.md");
    assert_eq!(
        m.read(Ref::Path("/archive/2026/note.md"), None)
            .await
            .unwrap()
            .1,
        b"hi"
    );
    assert!(matches!(
        m.stat(Ref::Path("/inbox/note.md")).await,
        Err(Error::NotFound(_))
    ));
    assert_eq!(m.revisions(Ref::Id(&id)).await.unwrap().len(), 1);

    // rename in place
    m.move_to(Ref::Id(&id), "/archive/2026/renamed.md", "t")
        .await
        .unwrap();
    assert_eq!(
        m.stat(Ref::Id(&id)).await.unwrap().path,
        "/archive/2026/renamed.md"
    );

    // moving a folder moves what is under it; paths are derived, so nothing is rewritten
    m.write("/projects/atlas/plan.md", b"p", by("t", ""))
        .await
        .unwrap();
    let atlas = m.stat(Ref::Path("/projects/atlas")).await.unwrap().id;
    m.move_to(Ref::Id(&atlas), "/archive/atlas", "t")
        .await
        .unwrap();
    assert_eq!(
        m.stat(Ref::Path("/archive/atlas/plan.md"))
            .await
            .unwrap()
            .path,
        "/archive/atlas/plan.md"
    );

    // a folder cannot move into itself
    assert!(matches!(
        m.move_to(Ref::Id(&atlas), "/archive/atlas/inside", "t")
            .await,
        Err(Error::Invalid(_))
    ));
    // a name clash is refused
    m.write("/x/a.md", b"a", by("t", "")).await.unwrap();
    m.write("/x/b.md", b"b", by("t", "")).await.unwrap();
    assert!(matches!(
        m.move_to(Ref::Path("/x/a.md"), "/x/b.md", "t").await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn stars_list_live_items_only() {
    let (m, _d) = store().await;
    let id = m.write("/s/a.md", b"a", by("t", "")).await.unwrap().node.id;
    m.update(
        &id,
        NodeUpdate {
            starred: Some(true),
            ..NodeUpdate::default()
        },
        "t",
    )
    .await
    .unwrap();
    let starred = m.starred().await.unwrap();
    assert_eq!(
        starred.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
        ["/s/a.md"]
    );
    m.trash(Ref::Id(&id), None, "t").await.unwrap();
    assert!(m.starred().await.unwrap().is_empty());
}

#[tokio::test]
async fn grants_apply_to_everything_below_and_owners_always_pass() {
    let (m, _d) = store().await;
    m.write("/team/doc.md", b"v1", by("alice", ""))
        .await
        .unwrap();
    let team = m.stat(Ref::Path("/team")).await.unwrap().id;

    m.share(&team, "bob", "writer", "alice").await.unwrap();
    // bob can write under the shared folder
    m.write("/team/doc.md", b"v2", by("bob", "")).await.unwrap();
    // carol has no grant, so the folder is restricted to the grants and the owner
    assert!(matches!(
        m.write("/team/doc.md", b"x", by("carol", "")).await,
        Err(Error::Forbidden(_))
    ));
    m.share(&team, "carol", "reader", "alice").await.unwrap();
    // a reader cannot write
    assert!(matches!(
        m.write("/team/doc.md", b"x", by("carol", "")).await,
        Err(Error::Forbidden(_))
    ));
    // a writer cannot share
    assert!(matches!(
        m.share(&team, "dave", "writer", "bob").await,
        Err(Error::Forbidden(_))
    ));
    // the owner can revoke
    m.unshare(&team, "bob", "alice").await.unwrap();
    assert!(matches!(
        m.write("/team/doc.md", b"y", by("bob", "")).await,
        Err(Error::Forbidden(_))
    ));
    // outside the shared tree nothing is restricted
    m.write("/public.md", b"ok", by("carol", "")).await.unwrap();
}

#[tokio::test]
async fn search_names_and_contents() {
    let (m, _d) = store().await;
    m.write(
        "/docs/plan.md",
        b"ship the Rust rewrite\nlater",
        by("t", ""),
    )
    .await
    .unwrap();
    m.write("/docs/other.md", b"nothing here", by("t", ""))
        .await
        .unwrap();
    m.write("/blob.bin", &[0xff, 0xfe, 0x00], by("t", ""))
        .await
        .unwrap();

    let by_name = m.search("PLAN", 10).await.unwrap();
    assert_eq!(
        (by_name[0].path.as_str(), by_name[0].matched),
        ("/docs/plan.md", "name")
    );
    let by_content = m.search("rust rewrite", 10).await.unwrap();
    assert_eq!(by_content.len(), 1);
    assert_eq!(by_content[0].matched, "content");
    assert_eq!(by_content[0].snippet, "ship the Rust rewrite");
    // trashed files drop out of search
    let id = by_content[0].id.clone();
    m.trash(Ref::Id(&id), None, "t").await.unwrap();
    assert!(m.search("rust rewrite", 10).await.unwrap().is_empty());
    assert!(m.search("zzz", 10).await.unwrap().is_empty());
    assert!(m.search("  ", 10).await.is_err());
}

#[tokio::test]
async fn path_normalization_rejects_traversal() {
    assert_eq!(drawer::service::normalize("a//b/./c/").unwrap(), "/a/b/c");
    assert_eq!(drawer::service::normalize("").unwrap(), "/");
    assert!(drawer::service::normalize("/a/../b").is_err());
}

#[tokio::test]
async fn reopen_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
        let m = Drawer::open(dir.path()).await.unwrap();
        let mut opts = by("t", "");
        opts.properties = props(&[("k", "v")]);
        m.write("/p.txt", b"persist", opts).await.unwrap();
    }
    let m = Drawer::open(dir.path()).await.unwrap();
    let (rev, data) = m.read(Ref::Path("/p.txt"), None).await.unwrap();
    assert_eq!(data, b"persist");
    assert_eq!(rev.properties, props(&[("k", "v")]));
}

/// A store written by the pre-revision schema (path-keyed `files`, `folders`, `versions`) must
/// open, move into the node model, keep its history, and keep working.
#[tokio::test]
async fn legacy_store_is_migrated_in_place() {
    use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

    let dir = tempfile::tempdir().unwrap();
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let hash = blobs.put(b"old body").await.unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(dir.path().join("drawer.db"))
            .create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE folders (path TEXT PRIMARY KEY, parent TEXT NOT NULL, created_at TEXT NOT NULL);
         CREATE TABLE files (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE, parent TEXT NOT NULL,
             current_version INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE versions (id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL REFERENCES files(id),
             version INTEGER NOT NULL, hash TEXT NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL, author TEXT NOT NULL,
             comment TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE (file_id, version));
         INSERT INTO folders (path, parent, created_at) VALUES ('/docs', '/', 'x');
         INSERT INTO files (path, parent, current_version, deleted, created_at, updated_at) VALUES ('/docs/a.md', '/docs', 1, 0, 'x', 'x');
         INSERT INTO files (path, parent, current_version, deleted, created_at, updated_at) VALUES ('/gone.md', '/', 1, 1, 'x', 'x');",
    )
    .execute(&pool)
    .await
    .unwrap();
    for file_id in [1_i64, 2] {
        sqlx::query(
            "INSERT INTO versions (file_id, version, hash, size, mime, author, comment, created_at) \
             VALUES (?, 1, ?, 8, 'text/markdown', 'legacy', 'imported', 'x')",
        )
        .bind(file_id)
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    }
    pool.close().await;

    let m = Drawer::open(dir.path()).await.unwrap();
    let (rev, data) = m.read(Ref::Path("/docs/a.md"), None).await.unwrap();
    assert_eq!(data, b"old body");
    assert_eq!(rev.revision, 1);
    assert_eq!(rev.author, "legacy");
    assert!(rev.properties.is_empty());
    // the legacy trashed file is in the trash, and its folder is live
    let bin = m.trash_list().await.unwrap();
    assert_eq!(
        bin.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
        ["/gone.md"]
    );
    // new-style preconditions work on migrated data
    assert!(matches!(
        m.write("/docs/a.md", b"x", guarded(0)).await,
        Err(Error::PreconditionFailed(_))
    ));
    let w = m
        .write("/docs/a.md", b"new body", guarded(1))
        .await
        .unwrap();
    assert_eq!(w.revision, 2);
    // reopening an already-migrated store is a no-op
    drop(m);
    let again = Drawer::open(dir.path()).await.unwrap();
    assert_eq!(
        again.read(Ref::Path("/docs/a.md"), None).await.unwrap().1,
        b"new body"
    );
}

fn edit(old: &str, new: &str) -> Edit {
    Edit {
        old: old.into(),
        new: new.into(),
        replace_all: false,
    }
}

#[tokio::test]
async fn edit_replaces_exact_text_as_one_guarded_revision() {
    let (m, _d) = store().await;
    let mut opts = by("alice", "");
    opts.properties = props(&[("k", "v")]);
    m.write("/a.md", b"one\ntwo\ntwo\n", opts).await.unwrap();

    let out = m
        .edit("/a.md", &[edit("one", "uno")], by("bob", "spanish"))
        .await
        .unwrap();
    assert_eq!(out.revision, 2);
    let (rev, data) = m.read(Ref::Path("/a.md"), None).await.unwrap();
    assert_eq!(data, b"uno\ntwo\ntwo\n");
    assert_eq!(
        (rev.author.as_str(), rev.comment.as_str()),
        ("bob", "spanish")
    );
    // properties carry over from the edited revision
    assert_eq!(rev.properties, props(&[("k", "v")]));
    assert_eq!(out.node.modified_by.as_deref(), Some("bob"));

    // ambiguous and missing text are refused, and nothing is written
    assert!(
        matches!(m.edit("/a.md", &[edit("two", "dos")], by("t", "")).await, Err(Error::Invalid(m)) if m.contains("2 times"))
    );
    assert!(
        matches!(m.edit("/a.md", &[edit("zzz", "")], by("t", "")).await, Err(Error::Invalid(m)) if m.contains("not found"))
    );
    let all = Edit {
        replace_all: true,
        ..edit("two", "dos")
    };
    assert_eq!(
        m.edit("/a.md", &[all], by("t", "")).await.unwrap().revision,
        3
    );
    assert_eq!(
        m.read(Ref::Path("/a.md"), None).await.unwrap().1,
        b"uno\ndos\ndos\n"
    );

    // a stale precondition fails
    assert!(matches!(
        m.edit("/a.md", &[edit("uno", "1")], guarded(1)).await,
        Err(Error::PreconditionFailed(_))
    ));

    // CRLF files still match edits written with \n
    m.write("/win.txt", b"a\r\nb\r\n", by("t", ""))
        .await
        .unwrap();
    m.edit("/win.txt", &[edit("a\nb", "x\ny")], by("t", ""))
        .await
        .unwrap();
    assert_eq!(
        m.read(Ref::Path("/win.txt"), None).await.unwrap().1,
        b"x\r\ny\r\n"
    );

    // binary files and folders can't be edited
    m.write("/b.bin", &[0xff, 0x00], by("t", "")).await.unwrap();
    assert!(
        m.edit("/b.bin", &[edit("a", "b")], by("t", ""))
            .await
            .is_err()
    );
    m.mkdir("/dir", "t").await.unwrap();
    assert!(
        m.edit("/dir", &[edit("a", "b")], by("t", ""))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn diff_between_revisions() {
    let (m, _d) = store().await;
    m.write("/d.txt", b"a\nb\nc\n", by("t", "")).await.unwrap();
    m.write("/d.txt", b"a\nB\nc\nd\n", by("t", ""))
        .await
        .unwrap();

    let d = m.diff(Ref::Path("/d.txt"), None, None, 3).await.unwrap();
    assert_eq!((d.from, d.to, d.added, d.removed), (1, 2, 2, 1));
    assert!(d.diff.contains("-b\n+B\n"), "{}", d.diff);
    assert!(
        d.diff.starts_with("--- /d.txt@1\n+++ /d.txt@2\n"),
        "{}",
        d.diff
    );

    // revision 0 is an empty file, so the first revision diffs as all additions
    let first = m.diff(Ref::Path("/d.txt"), None, Some(1), 3).await.unwrap();
    assert_eq!((first.from, first.added, first.removed), (0, 3, 0));
    let same = m
        .diff(Ref::Path("/d.txt"), Some(2), Some(2), 3)
        .await
        .unwrap();
    assert!(same.diff.is_empty());
    assert!(m.diff(Ref::Path("/d.txt"), Some(9), None, 3).await.is_err());
}

#[tokio::test]
async fn walk_recent_and_batch_paths() {
    let (m, _d) = store().await;
    for p in ["/b/x.md", "/a/deep/er/y.md", "/a-sibling.md", "/a/z.md"] {
        m.write(p, p.as_bytes(), by("t", "")).await.unwrap();
    }
    let (all, truncated) = m.walk("/", 32).await.unwrap();
    assert!(!truncated);
    let paths: Vec<_> = all.iter().map(|n| n.path.as_str()).collect();
    // a folder's children follow it directly, even when a sibling name sorts in between
    assert_eq!(
        paths,
        [
            "/a",
            "/a/deep",
            "/a/deep/er",
            "/a/deep/er/y.md",
            "/a/z.md",
            "/a-sibling.md",
            "/b",
            "/b/x.md"
        ]
    );
    let (shallow, _) = m.walk("/a", 1).await.unwrap();
    assert_eq!(
        shallow.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
        ["/a/deep", "/a/z.md"]
    );
    assert!(m.walk("/a/z.md", 2).await.is_err());

    m.write("/b/x.md", b"newer", by("zed", "")).await.unwrap();
    let recent = m.recent(2).await.unwrap();
    assert_eq!(recent[0].path, "/b/x.md");
    assert_eq!(recent[0].modified_by.as_deref(), Some("zed"));
    assert_eq!(recent.len(), 2);

    // starred and trash resolve every path in one go
    for p in ["/a/deep/er/y.md", "/b"] {
        let id = m.stat(Ref::Path(p)).await.unwrap().id;
        m.update(
            &id,
            NodeUpdate {
                starred: Some(true),
                ..NodeUpdate::default()
            },
            "t",
        )
        .await
        .unwrap();
    }
    let starred = m.starred().await.unwrap();
    assert_eq!(
        starred.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
        ["/a/deep/er/y.md", "/b"]
    );
    m.trash(Ref::Path("/a/deep"), None, "t").await.unwrap();
    assert_eq!(m.trash_list().await.unwrap()[0].path, "/a/deep");
}

#[tokio::test]
async fn search_ranks_names_first_and_reports_lines() {
    let (m, _d) = store().await;
    m.write("/a.md", b"intro\nthe budget is due\n", by("t", ""))
        .await
        .unwrap();
    m.write("/z/budget.md", b"numbers", by("t", ""))
        .await
        .unwrap();
    let long = format!("{}needle{}", "x".repeat(400), "y".repeat(400));
    m.write("/long.txt", long.as_bytes(), by("t", ""))
        .await
        .unwrap();

    let hits = m.search("budget", 10).await.unwrap();
    assert_eq!(
        (hits[0].path.as_str(), hits[0].matched),
        ("/z/budget.md", "name")
    );
    assert_eq!((hits[1].path.as_str(), hits[1].line), ("/a.md", Some(2)));
    // long lines are cut around the match
    let hit = &m.search("needle", 10).await.unwrap()[0];
    assert!(
        hit.snippet.contains("needle") && hit.snippet.chars().count() < 200,
        "{}",
        hit.snippet
    );
}

#[tokio::test]
async fn public_link_can_pin_a_revision() {
    let (s, _dir) = store().await;
    let first = s
        .write("/share.txt", b"original", by("alice", ""))
        .await
        .unwrap();
    let fixed = s
        .create_download_link_with_revision(&first.node.id, "alice", 7, Some(1))
        .await
        .unwrap();
    let latest = s
        .create_download_link(&first.node.id, "alice", 7)
        .await
        .unwrap();
    s.write("/share.txt", b"new", by("alice", ""))
        .await
        .unwrap();
    let (_, _, path) = s.download_link_file(&fixed.token).await.unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"original");
    let (_, _, path) = s.download_link_file(&latest.token).await.unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"new");
    assert!(
        s.create_download_link_with_revision(&first.node.id, "alice", 7, Some(999))
            .await
            .is_err()
    );
    assert_eq!(
        s.list_download_links(&first.node.id, "alice")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn one_download_link_admits_only_one_concurrent_request() {
    let (s, _dir) = store().await;
    let file = s.write("/one.txt", b"one", by("alice", "")).await.unwrap();
    let link = s
        .create_download_link_with_options(&file.node.id, "alice", 7, None, Some(1))
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        s.download_link_file(&link.token),
        s.download_link_file(&link.token)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let listed = s.list_download_links(&file.node.id, "alice").await.unwrap();
    assert_eq!(listed[0].downloads, 1);
    assert!(
        s.create_download_link_with_options(&file.node.id, "alice", 7, None, Some(0))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn verified_backup_restore_and_exclusive_maintenance() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("store");
    let s = Drawer::open(&data).await.unwrap();
    s.write("/backup.txt", b"snapshot", WriteOpts::new("api", ""))
        .await
        .unwrap();
    assert!(Drawer::open_maintenance(&data).await.is_err());
    drop(s);
    let snapshot = drawer::backup::create(&data, &dir.path().join("backups"))
        .await
        .unwrap();
    drawer::backup::verify(&snapshot).await.unwrap();
    let restored = dir.path().join("restored");
    drawer::backup::restore(&snapshot, &restored).await.unwrap();
    let restored_s = Drawer::open(&restored).await.unwrap();
    assert_eq!(
        restored_s
            .read(Ref::Path("/backup.txt"), None)
            .await
            .unwrap()
            .1,
        b"snapshot"
    );
    assert!(drawer::backup::restore(&snapshot, &restored).await.is_err());
    std::fs::write(snapshot.join("drawer.db"), b"corrupt").unwrap();
    assert!(drawer::backup::verify(&snapshot).await.is_err());
}

#[tokio::test]
async fn cleanup_preview_preserves_pins_heads_and_shared_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let first = s.write("/history", b"one", by("api", "")).await.unwrap();
    s.create_download_link_with_revision(&first.node.id, "api", 7, Some(1))
        .await
        .unwrap();
    s.write("/history", b"two", by("api", "")).await.unwrap();
    s.write("/history", b"three", by("api", "")).await.unwrap();
    s.write("/history", b"four", by("api", "")).await.unwrap();
    s.write("/shared", b"two", by("api", "")).await.unwrap();
    let trash = s
        .write("/old-trash", b"trash", by("api", ""))
        .await
        .unwrap();
    s.trash(Ref::Id(&trash.node.id), None, "api").await.unwrap();
    drop(s);
    // Age the trash and physical blobs without changing their contents.
    let db = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        dir.path().join("drawer.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET modified_at='2000-01-01T00:00:00.000Z' WHERE trashed=1")
        .execute(&db)
        .await
        .unwrap();
    db.close().await;
    for folder in std::fs::read_dir(dir.path().join("blobs")).unwrap() {
        for file in std::fs::read_dir(folder.unwrap().path()).unwrap() {
            std::fs::File::open(file.unwrap().path())
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86400),
                ))
                .unwrap();
        }
    }
    let mut args = drawer::cleanup::CleanupArgs {
        keep_revisions: 1,
        trash_days: 1,
        orphan_days: 1,
        apply: false,
    };
    let preview = drawer::cleanup::cleanup(dir.path(), &args).await.unwrap();
    assert_eq!(preview.trash_nodes, 1);
    assert_eq!(preview.old_revisions, 2);
    assert_eq!(preview.orphan_blobs, 2);
    assert!(
        Drawer::open(dir.path())
            .await
            .unwrap()
            .read(Ref::Path("/history"), Some(3))
            .await
            .is_ok()
    );
    args.apply = true;
    drawer::cleanup::cleanup(dir.path(), &args).await.unwrap();
    let reopened = Drawer::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened
            .read(Ref::Path("/history"), Some(1))
            .await
            .unwrap()
            .1,
        b"one"
    );
    assert_eq!(
        reopened.read(Ref::Path("/history"), None).await.unwrap().1,
        b"four"
    );
    assert_eq!(
        reopened.read(Ref::Path("/shared"), None).await.unwrap().1,
        b"two"
    );
    assert!(reopened.read(Ref::Path("/history"), Some(3)).await.is_err());
    assert!(reopened.stat(Ref::Id(&trash.node.id)).await.is_err());
}

#[tokio::test]
async fn root_backup_includes_private_user_stores() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("root");
    let root = Drawer::open(&data).await.unwrap();
    drop(root);
    let id = uuid::Uuid::new_v4().to_string();
    let db = sqlx::SqlitePool::connect(&format!("sqlite:{}", data.join("drawer.db").display()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_accounts(id,username,password_hash,quota_bytes) VALUES(?,'alice','test-only',100)").bind(&id).execute(&db).await.unwrap();
    db.close().await;
    let private = Drawer::open(&data.join("users").join(&id)).await.unwrap();
    private
        .write("/secret", b"private-backup", by("alice", ""))
        .await
        .unwrap();
    drop(private);
    let snapshot = drawer::backup::create(&data, &dir.path().join("backups"))
        .await
        .unwrap();
    let restored = dir.path().join("restored");
    drawer::backup::restore(&snapshot, &restored).await.unwrap();
    let private = Drawer::open(&restored.join("users").join(&id))
        .await
        .unwrap();
    assert_eq!(
        private.read(Ref::Path("/secret"), None).await.unwrap().1,
        b"private-backup"
    );
}

#[tokio::test]
async fn cleanup_counts_remaining_rows_when_retention_increases() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let first = s.write("/history", b"one", by("api", "")).await.unwrap();
    let pin = s
        .create_download_link_with_revision(&first.node.id, "api", 7, Some(1))
        .await
        .unwrap();
    for bytes in [b"two".as_slice(), b"three".as_slice(), b"four".as_slice()] {
        s.write("/history", bytes, by("api", "")).await.unwrap();
    }
    drop(s);
    let mut policy = drawer::cleanup::CleanupArgs {
        keep_revisions: 1,
        trash_days: 30,
        orphan_days: 1,
        apply: true,
    };
    drawer::cleanup::cleanup(dir.path(), &policy).await.unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    s.revoke_download_link(&first.node.id, &pin.id, "api")
        .await
        .unwrap();
    drop(s);
    // Only revisions 1 and 4 remain. Raising the policy to three must retain both.
    policy.keep_revisions = 3;
    let report = drawer::cleanup::cleanup(dir.path(), &policy).await.unwrap();
    assert_eq!(report.old_revisions, 0);
    let s = Drawer::open(dir.path()).await.unwrap();
    assert_eq!(
        s.read(Ref::Path("/history"), Some(1)).await.unwrap().1,
        b"one"
    );
}
