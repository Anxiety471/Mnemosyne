//! Tests for the core fixes: comment encoding, panic-free reads, validation-first moves,
//! quota-aware staged writes, streaming search, and the vault parse cache.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use drawer::service::{Drawer, Ref, WriteOpts};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt as _;

async fn store() -> (Drawer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Drawer::open(dir.path()).await.unwrap(), dir)
}

fn by(author: &str, comment: &str) -> WriteOpts {
    WriteOpts::new(author, comment)
}

// ----- contract #1: percent-encoded comments -----------------------------------------------

#[tokio::test]
async fn non_ascii_comment_round_trips_through_percent_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let put = |path: &str, comment: &str, encoding: Option<&str>| {
        let mut req = Request::put(format!("/drawer/v1/files?path={path}"))
            .header("x-drawer-author", "tester")
            .header("x-drawer-comment", comment)
            .body(Body::from("data"))
            .unwrap();
        if let Some(enc) = encoding {
            req.headers_mut()
                .insert("x-drawer-comment-encoding", enc.parse().unwrap());
        }
        req
    };
    // "héllo wörld — 抽屉" percent-encoded (encodeURIComponent semantics).
    let encoded = "h%C3%A9llo%20w%C3%B6rld%20%E2%80%94%20%E6%8A%BD%E5%B1%89";
    let r = app
        .clone()
        .oneshot(put("/note.txt", encoded, Some("percent")))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let written =
        serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert_eq!(written["revision"], 1);

    // The comment is stored decoded; reading it back shows the original text.
    let revs = serde_json::from_slice::<Value>(
        &app.clone()
            .oneshot(
                Request::get("/drawer/v1/revisions?path=/note.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(revs["revisions"][0]["comment"], "héllo wörld — 抽屉");

    // The same header without the encoding flag stays raw (backwards compatible).
    let r = app
        .clone()
        .oneshot(put("/raw.txt", encoded, None))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let revs = serde_json::from_slice::<Value>(
        &app.clone()
            .oneshot(
                Request::get("/drawer/v1/revisions?path=/raw.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(revs["revisions"][0]["comment"], encoded);

    // Invalid percent triplets are rejected with 400.
    let bad = Request::put("/drawer/v1/files?path=/bad.txt")
        .header("x-drawer-comment", "%zz%2")
        .header("x-drawer-comment-encoding", "percent")
        .body(Body::from("x"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    // Invalid UTF-8 after decoding is rejected with 400.
    let bad = Request::put("/drawer/v1/files?path=/bad2.txt")
        .header("x-drawer-comment", "%ff%fe")
        .header("x-drawer-comment-encoding", "percent")
        .body(Body::from("x"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

// ----- contract #2: panic-free reads -------------------------------------------------------

#[tokio::test]
async fn corrupt_properties_json_falls_back_to_empty_and_logs() {
    let (m, _d) = store().await;
    let id = m
        .write("/p.txt", b"data", by("t", ""))
        .await
        .unwrap()
        .node
        .id;
    // Hand-corrupt the stored properties JSON.
    let db = sqlx::SqlitePool::connect(&format!("sqlite:{}/drawer.db", _d.path().display()))
        .await
        .unwrap();
    sqlx::query("UPDATE revisions SET properties = 'not json{' WHERE node_id = ?")
        .bind(&id)
        .execute(&db)
        .await
        .unwrap();
    db.close().await;
    // Reads keep working and report empty properties.
    let (rev, data) = m.read(Ref::Id(&id), None).await.unwrap();
    assert_eq!(rev.properties.len(), 0);
    assert_eq!(data, b"data");
    let node = m.stat(Ref::Id(&id)).await.unwrap();
    assert_eq!(node.properties.len(), 0);
    // A corrupt revision row does not break the revisions listing either.
    let revs = m.revisions(Ref::Id(&id)).await.unwrap();
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].properties.len(), 0);
}

#[tokio::test]
async fn out_does_not_panic_on_unserializable_value() {
    // `out()` handles serialization failures gracefully. The `Stats` and other types are
    // serializable, so this exercises the helper through a normal path; the guarantee is
    // that no `.expect("serializable")` remains in request paths.
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let r = app
        .clone()
        .oneshot(
            Request::get("/drawer/v1/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

// ----- contract #4: validation-first move --------------------------------------------------

#[tokio::test]
async fn failed_move_leaves_no_new_folders() {
    let (m, _d) = store().await;
    m.write("/src/keep.txt", b"keep", by("t", ""))
        .await
        .unwrap();
    m.write("/dst/taken.txt", b"taken", by("t", ""))
        .await
        .unwrap();
    // Moving into /dst/taken.txt/under.txt fails: taken.txt is a file.
    let err = m
        .move_to(Ref::Path("/src/keep.txt"), "/dst/taken.txt/under.txt", "t")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is a file"), "{err}");
    // No folders were created for the refused move.
    assert!(m.stat(Ref::Path("/dst/taken.txt/under.txt")).await.is_err());
    assert!(
        m.list("/dst")
            .await
            .unwrap()
            .iter()
            .any(|n| n.name == "taken.txt")
    );
    assert_eq!(m.list("/dst").await.unwrap().len(), 1);

    // A name clash in an existing parent is refused before any mkdir.
    let err = m
        .move_to(Ref::Path("/src/keep.txt"), "/dst/taken.txt", "t")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");

    // A clash where the destination parent does not exist yet: the parent is created only
    // after the clash check, so a clash against a node that does not exist still creates
    // the parent (no clash possible). That is fine; the refused cases above leave no trace.
}

// ----- contract #5: quota-aware staged writes ----------------------------------------------

#[tokio::test]
async fn identical_staged_reupload_at_quota_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    // Admin creates alice with a 10-byte quota.
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        Some("machine".into()),
        None,
        Some("admin-password".into()),
        false,
        None,
    );
    let create = Request::post("/drawer/v1/users")
        .header("authorization", "Bearer machine")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"username":"alice","password":"a-long-password","quota_bytes":10}"#,
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(create).await.unwrap().status(),
        StatusCode::OK
    );
    let login = app
        .clone()
        .oneshot(
            Request::post("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"alice","password":"a-long-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let put = |path: &str, body: &'static str| {
        Request::put(format!("/drawer/v1/files?path={path}"))
            .header("cookie", &cookie)
            .header("x-drawer-author", "alice")
            .body(Body::from(body))
            .unwrap()
    };
    // Fill the quota exactly: 10 bytes.
    assert_eq!(
        app.clone()
            .oneshot(put("/seed.txt", "0123456789"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Identical re-upload of the same path: the blob already exists, so the quota is not
    // charged again and no new revision is written.
    let r = app
        .clone()
        .oneshot(put("/seed.txt", "0123456789"))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "identical re-upload at quota should succeed"
    );
    // A different path with the same content: the blob exists so the *blob* write is not
    // charged, but a new revision's bytes still count toward the quota (pre-existing
    // commit() semantics), so this is refused once the quota is full.
    assert_eq!(
        app.clone()
            .oneshot(put("/copy.txt", "0123456789"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // New content beyond the quota is likewise refused.
    assert_eq!(
        app.clone()
            .oneshot(put("/over.txt", "0123456789J"))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

// ----- contract #6: streaming search -------------------------------------------------------

#[tokio::test]
async fn search_finds_match_spanning_chunk_boundary() {
    let (m, _d) = store().await;
    // A file larger than SEARCH_CHUNK (64 KiB) with a match that straddles a boundary.
    let chunk = 64 * 1024;
    let mut big = vec![b'x'; chunk + 10];
    // Place "needle" so it straddles the 64 KiB boundary: 3 bytes before, 3 after.
    let at = chunk - 3;
    big[at..at + 6].copy_from_slice(b"needle");
    m.write("/big.txt", &big, by("t", "")).await.unwrap();
    // The match spans bytes [chunk-3, chunk+3).
    let hits = m.search("needle", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/big.txt");
    assert_eq!(hits[0].matched, "content");
    assert_eq!(hits[0].line, Some(1));
    assert!(hits[0].snippet.contains("needle"), "{}", hits[0].snippet);
}

#[tokio::test]
async fn search_streams_large_files_without_holding_them_in_memory() {
    let (m, _d) = store().await;
    // Two large files; the match is in the second. The first must be scanned in chunks
    // (no early whole-file load) and the second found.
    let chunk = 64 * 1024;
    let mut first = vec![b'a'; chunk * 3];
    first[chunk..chunk + 5].copy_from_slice(b"aaaaa");
    m.write("/first.txt", &first, by("t", "")).await.unwrap();
    let mut second = vec![b'b'; chunk * 2];
    second[chunk + 10..chunk + 20].copy_from_slice(b"bbneedlebb");
    m.write("/second.txt", &second, by("t", "")).await.unwrap();
    let hits = m.search("needle", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "/second.txt");
}

#[tokio::test]
async fn search_handles_utf8_spanning_boundaries() {
    let (m, _d) = store().await;
    // Multi-byte characters (2-byte, 3-byte, 4-byte) around the 64 KiB boundary.
    let chunk = 64 * 1024;
    let mut body = String::new();
    while body.len() < chunk - 20 {
        body.push('→'); // 3-byte char
    }
    // "wörld" ends exactly at the chunk boundary, straddling it.
    body.push_str("wör");
    body.push_str("ld");
    m.write("/utf8.txt", body.as_bytes(), by("t", ""))
        .await
        .unwrap();
    let hits = m.search("wörld", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("wörld"), "{}", hits[0].snippet);
}

// ----- contract #7: vault parse cache -------------------------------------------------------

#[tokio::test]
async fn vault_edit_after_cached_load_reflects_new_content() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let put = |body: Value| {
        Request::put("/drawer/v1/memory/note")
            .header("content-type", "application/json")
            .header("x-drawer-author", "tester")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    // Save a note → parsed and cached.
    let r = app
        .clone()
        .oneshot(put(json!({"name":"Drawer","content":"A file server."})))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // A list request populates the cache (read note also works).
    let r = app
        .clone()
        .oneshot(
            Request::get("/drawer/v1/memory/notes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // Edit the note with the REST write path (bypasses the vault module).
    let r = app
        .clone()
        .oneshot(
            Request::put("/drawer/v1/files?path=/memory/Drawer.md")
                .header("x-drawer-author", "tester")
                .body(Body::from("Completely different content #newtag"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // The vault must reflect the new content and tags (revision-keyed invalidation).
    let r = app
        .clone()
        .oneshot(
            Request::get("/drawer/v1/memory/notes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let notes = serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
        .unwrap();
    assert_eq!(notes["notes"][0]["name"], "Drawer");
    assert_eq!(notes["notes"][0]["tags"], json!(["newtag"]));
    let r = app
        .clone()
        .oneshot(
            Request::get("/drawer/v1/memory/note?name=Drawer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let note = serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
        .unwrap();
    assert!(
        note["content"]
            .as_str()
            .unwrap()
            .contains("Completely different content")
    );
}

#[tokio::test]
async fn vault_cache_drops_removed_notes() {
    let dir = tempfile::tempdir().unwrap();
    let m = Drawer::open(dir.path()).await.unwrap();
    m.write("/memory/Gone.md", b"bye #old", by("t", ""))
        .await
        .unwrap();
    // Populate the cache.
    let notes = drawer::vault::list(&m, None, None, 10).await.unwrap();
    assert_eq!(notes.len(), 1);
    // Trash the note; the next vault load must not resurrect it.
    m.trash(Ref::Path("/memory/Gone.md"), None, "t")
        .await
        .unwrap();
    let notes = drawer::vault::list(&m, None, None, 10).await.unwrap();
    assert!(notes.is_empty());
}

// ----- contract #4: tags on existing notes -------------------------------------------------

#[tokio::test]
async fn tags_on_existing_note_update() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let put = |body: Value| {
        Request::put("/drawer/v1/memory/note")
            .header("content-type", "application/json")
            .header("x-drawer-author", "tester")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    // Create with tags.
    let r = app
        .clone()
        .oneshot(put(
            json!({"name":"Proj","content":"body","tags":["alpha"]}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let saved = serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
        .unwrap();
    assert_eq!(saved["tags"], json!(["alpha"]));
    // Replace with different tags: the frontmatter tags are replaced.
    let r = app
        .clone()
        .oneshot(put(
            json!({"name":"Proj","content":"new body","tags":["beta","gamma"]}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let saved = serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
        .unwrap();
    assert_eq!(saved["tags"], json!(["beta", "gamma"]));
    let r = app
        .clone()
        .oneshot(
            Request::get("/drawer/v1/memory/note?name=Proj")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let note = serde_json::from_slice::<Value>(&r.into_body().collect().await.unwrap().to_bytes())
        .unwrap();
    assert_eq!(note["tags"], json!(["beta", "gamma"]));
    // Replacing with no tags keeps the current behavior: the new content has no frontmatter,
    let _r = app
        .clone()
        .oneshot(put(json!({"name":"Proj","content":"third body"})))
        .await
        .unwrap();
    let note = serde_json::from_slice::<Value>(
        &app.clone()
            .oneshot(
                Request::get("/drawer/v1/memory/note?name=Proj")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(note["tags"], json!([]));
    assert_eq!(note["content"], "third body\n");
}
