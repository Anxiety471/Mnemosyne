use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use drawer::service::{Drawer, Ref};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt as _;

async fn send(app: &Router, req: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(req).await.unwrap()
}

async fn json_of(resp: axum::response::Response) -> Value {
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn bearer_post(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .header("authorization", "Bearer secret")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn put_chunk(uri: &str, offset: &str, data: &'static str) -> Request<Body> {
    Request::put(uri)
        .header("x-upload-offset", offset)
        .body(Body::from(data))
        .unwrap()
}

fn post_empty(uri: &str) -> Request<Body> {
    Request::post(uri).body(Body::empty()).unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::delete(uri).body(Body::empty()).unwrap()
}

async fn make_link(
    app: &Router,
    folder_id: &str,
    days: i64,
    max_files: i64,
    max_bytes: i64,
) -> (String, String) {
    let create = bearer_post(
        &format!("/drawer/v1/nodes/{folder_id}/upload-links"),
        json!({"days":days,"max_files":max_files,"max_bytes":max_bytes}),
    );
    let link = json_of(send(app, create).await).await;
    (
        link["id"].as_str().unwrap().to_string(),
        link["token"].as_str().unwrap().to_string(),
    )
}

fn sha256_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[tokio::test]
async fn chunked_deposit_completes_and_file_appears() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 5, 1048576).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"hello.txt","size":6,"sha256":sha256_of(b"abcdef")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();
    assert_eq!(started["received"].as_i64().unwrap(), 0);
    assert_eq!(started["size"].as_i64().unwrap(), 6);

    let r1 = json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "abc"),
        )
        .await,
    )
    .await;
    assert_eq!(r1["received"].as_i64().unwrap(), 3);
    let r2 = json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "3", "def"),
        )
        .await,
    )
    .await;
    assert_eq!(r2["received"].as_i64().unwrap(), 6);

    assert_eq!(
        send(&app, post_empty(&format!("{path}/complete/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );

    let nodes = s.list("/inbox").await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].name.ends_with("hello.txt"));
    assert_eq!(
        s.read(Ref::Id(&nodes[0].id), None).await.unwrap().1,
        b"abcdef"
    );
}

#[tokio::test]
async fn resume_after_partial_works() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 5, 1048576).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"data.bin","size":6,"sha256":sha256_of(b"abcdef")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();

    json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "abc"),
        )
        .await,
    )
    .await;

    let restart = json_post(
        &format!("{path}/start"),
        json!({"name":"data.bin","size":6,"sha256":sha256_of(b"abcdef")}),
    );
    let resumed = json_of(send(&app, restart).await).await;
    assert_eq!(resumed["id"].as_str().unwrap(), chunk_id);
    assert_eq!(resumed["received"].as_i64().unwrap(), 3);

    json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "3", "def"),
        )
        .await,
    )
    .await;
    assert_eq!(
        send(&app, post_empty(&format!("{path}/complete/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );

    let nodes = s.list("/inbox").await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        s.read(Ref::Id(&nodes[0].id), None).await.unwrap().1,
        b"abcdef"
    );
}

#[tokio::test]
async fn budget_exceeded_mid_chunks_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 1, 6).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"a.txt","size":6,"sha256":sha256_of(b"abc")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();

    json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "abc"),
        )
        .await,
    )
    .await;

    let start2 = json_post(
        &format!("{path}/start"),
        json!({"name":"b.txt","size":3,"sha256":sha256_of(b"abc")}),
    );
    assert_eq!(send(&app, start2).await.status(), StatusCode::CONFLICT);

    assert_eq!(
        send(&app, delete(&format!("{path}/cancel/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );

    let start3 = json_post(
        &format!("{path}/start"),
        json!({"name":"c.txt","size":3,"sha256":sha256_of(b"abc")}),
    );
    assert_eq!(send(&app, start3).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn expired_link_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (link_id, token) = make_link(&app, &folder.id, 1, 5, 1048576).await;

    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("drawer.db")),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE upload_links SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&link_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let path = format!("/drop/{token}");
    assert_eq!(send(&app, get(&path)).await.status(), StatusCode::NOT_FOUND);
    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"x.txt","size":1,"sha256":sha256_of(b"a")}),
    );
    assert_eq!(send(&app, start).await.status(), StatusCode::NOT_FOUND);
    let upload = axum::http::Request::post(format!("{path}?name=x.txt"))
        .header("content-length", "1")
        .body(Body::from("a"))
        .unwrap();
    assert_eq!(send(&app, upload).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_removes_staging_and_frees_budget() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 1, 100).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"big.bin","size":100,"sha256":sha256_of(b"partial")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();

    json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "partial"),
        )
        .await,
    )
    .await;

    assert_eq!(
        send(&app, delete(&format!("{path}/cancel/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );

    let start2 = json_post(
        &format!("{path}/start"),
        json!({"name":"small.txt","size":10,"sha256":sha256_of(b"partial")}),
    );
    assert_eq!(send(&app, start2).await.status(), StatusCode::OK);

    let staging = dir
        .path()
        .join("transfers")
        .join(format!("drop-{chunk_id}.part"));
    assert!(!staging.exists());
}

#[tokio::test]
async fn wrong_offset_rejected_on_append() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 5, 1048576).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"f.txt","size":10,"sha256":sha256_of(b"abc")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();

    assert_eq!(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "5", "abc")
        )
        .await
        .status(),
        StatusCode::PRECONDITION_FAILED
    );
}

#[tokio::test]
async fn invalid_sha256_rejected_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let folder = s.mkdir("/inbox", "api").await.unwrap();
    let app = drawer::app(s.clone(), Some("secret".into()), None);
    let (_id, token) = make_link(&app, &folder.id, 7, 5, 1048576).await;
    let path = format!("/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"bad.txt","size":3,"sha256":"nothex"}),
    );
    assert_eq!(send(&app, start).await.status(), StatusCode::BAD_REQUEST);
}

// ----- tenant-scoped chunked deposits ----------------------------------------------------

const USER_PASSWORD: &str = "a-long-password";

/// Create a named user through the admin's machine token and return its account id.
async fn named_user_id(app: &Router, name: &str) -> String {
    let create = Request::post("/drawer/v1/users")
        .header("authorization", "Bearer machine")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username":name,"password":USER_PASSWORD,"quota_bytes":1048576}).to_string(),
        ))
        .unwrap();
    let account = json_of(send(app, create).await).await;
    account["id"].as_str().unwrap().to_string()
}

/// Log in as `name` and create an upload link for `folder` in that user's own workspace.
/// Returns the link id and its one-time token.
async fn tenant_upload_link(
    app: &Router,
    user_id: &str,
    name: &str,
    folder: &str,
    max_files: i64,
    max_bytes: i64,
) -> (String, String) {
    let login = send(
        app,
        json_post(
            "/auth/login",
            json!({"username":name,"password":USER_PASSWORD}),
        ),
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK, "login as {name}");
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let mkdir = Request::post("/drawer/v1/folders")
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(Body::from(json!({"path":folder}).to_string()))
        .unwrap();
    let mkdir_json = json_of(send(app, mkdir).await).await;
    assert_eq!(
        mkdir_json["path"], folder,
        "folder {folder} in {name}'s store"
    );
    let folder_id = mkdir_json["id"].as_str().unwrap().to_string();
    let create = Request::post(format!("/drawer/v1/nodes/{folder_id}/upload-links"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"days":7,"max_files":max_files,"max_bytes":max_bytes}).to_string(),
        ))
        .unwrap();
    let link = json_of(send(app, create).await).await;
    assert!(
        link["url_path"]
            .as_str()
            .is_some_and(|p| p.starts_with(&format!("/u/{user_id}/"))),
        "tenant links are tenant-scoped: {}",
        link["url_path"]
    );
    (
        link["id"].as_str().unwrap().to_string(),
        link["token"].as_str().unwrap().to_string(),
    )
}

/// One chunked deposit against a named user's `/u/{id}/drop/{token}` route, with no auth.
/// The link was created in that user's own workspace, so the file lands there and nowhere else.
#[tokio::test]
async fn tenant_scoped_chunked_deposit_lands_in_the_users_workspace_only() {
    let dir = tempfile::tempdir().unwrap();
    let admin = Drawer::open(dir.path()).await.unwrap();
    let app = drawer::app_with_auth(
        admin.clone(),
        Some("machine".into()),
        None,
        Some("admin-password".into()),
        false,
        None,
    );
    let alice_id = named_user_id(&app, "alice").await;
    let (_, token) = tenant_upload_link(&app, &alice_id, "alice", "/inbox", 5, 1048576).await;
    let path = format!("/u/{alice_id}/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"hello.txt","size":6,"sha256":sha256_of(b"abcdef")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();
    assert_eq!(started["received"].as_i64().unwrap(), 0);

    let r1 = json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "abc"),
        )
        .await,
    )
    .await;
    assert_eq!(r1["received"].as_i64().unwrap(), 3);
    let r2 = json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "3", "def"),
        )
        .await,
    )
    .await;
    assert_eq!(r2["received"].as_i64().unwrap(), 6);
    assert_eq!(
        send(&app, post_empty(&format!("{path}/complete/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );

    // The tenant store holds the deposit, under the tenant prefix, with the declared bytes.
    let tenant_store = Drawer::open(&dir.path().join("users").join(&alice_id))
        .await
        .unwrap();
    let nodes = tenant_store.list("/inbox").await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].name.ends_with("hello.txt"));
    assert_eq!(
        tenant_store
            .read(Ref::Id(&nodes[0].id), None)
            .await
            .unwrap()
            .1,
        b"abcdef"
    );

    // The admin workspace never sees it: the root has no deposit, and the id is unknown.
    assert!(
        admin.list("/").await.unwrap().is_empty(),
        "a tenant deposit must not land in the admin workspace"
    );
    assert!(
        admin.stat(Ref::Id(&nodes[0].id)).await.is_err(),
        "the tenant deposit is not in the admin workspace"
    );
}

/// A tenant deposit that is abandoned gives its reserved bytes back, so the link budget is
/// usable again from the tenant route.
#[tokio::test]
async fn tenant_scoped_cancel_frees_the_link_budget() {
    let dir = tempfile::tempdir().unwrap();
    let admin = Drawer::open(dir.path()).await.unwrap();
    let app = drawer::app_with_auth(
        admin,
        Some("machine".into()),
        None,
        Some("admin-password".into()),
        false,
        None,
    );
    let alice_id = named_user_id(&app, "alice").await;
    let (link_id, token) = tenant_upload_link(&app, &alice_id, "alice", "/inbox", 1, 100).await;
    let path = format!("/u/{alice_id}/drop/{token}");

    let start = json_post(
        &format!("{path}/start"),
        json!({"name":"big.bin","size":100,"sha256":sha256_of(b"partial")}),
    );
    let started = json_of(send(&app, start).await).await;
    let chunk_id = started["id"].as_str().unwrap().to_string();
    json_of(
        send(
            &app,
            put_chunk(&format!("{path}/append/{chunk_id}"), "0", "partial"),
        )
        .await,
    )
    .await;

    assert_eq!(
        send(&app, delete(&format!("{path}/cancel/{chunk_id}")))
            .await
            .status(),
        StatusCode::OK
    );
    // The reserved 100 bytes are back in the tenant's own database after the cancel.
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.path().join("users").join(&alice_id).join("drawer.db")),
    )
    .await
    .unwrap();
    let budget: (i64, i64) =
        sqlx::query_as("SELECT files_uploaded, bytes_uploaded FROM upload_links WHERE id = ?")
            .bind(&link_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(budget, (0, 0), "cancel released the reserved budget");

    // A fresh start is accepted, and it spends the freed budget again.
    let start2 = json_post(
        &format!("{path}/start"),
        json!({"name":"small.txt","size":10,"sha256":sha256_of(b"partial")}),
    );
    assert_eq!(send(&app, start2).await.status(), StatusCode::OK);

    let budget: (i64, i64) =
        sqlx::query_as("SELECT files_uploaded, bytes_uploaded FROM upload_links WHERE id = ?")
            .bind(&link_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    assert_eq!(budget, (1, 10), "the restart consumed the freed budget");

    // The abandoned staging file is gone from the tenant store.
    let staging = dir
        .path()
        .join("users")
        .join(&alice_id)
        .join("transfers")
        .join(format!("drop-{chunk_id}.part"));
    assert!(!staging.exists());
}

/// A token minted for one user is worthless under another user's `/u/{id}/` prefix, and a link
/// minted in the admin workspace is not usable on any tenant route.
#[tokio::test]
async fn tenant_token_from_another_workspace_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let admin = Drawer::open(dir.path()).await.unwrap();
    let app = drawer::app_with_auth(
        admin.clone(),
        Some("machine".into()),
        None,
        Some("admin-password".into()),
        false,
        None,
    );
    let alice_id = named_user_id(&app, "alice").await;
    let bob_id = named_user_id(&app, "bob").await;
    let (alice_link_id, alice_token) =
        tenant_upload_link(&app, &alice_id, "alice", "/inbox", 5, 1048576).await;

    // Bob's prefix cannot spend alice's token, not even to render the landing page.
    assert_eq!(
        send(&app, get(&format!("/u/{bob_id}/drop/{alice_token}")))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let start = json_post(
        &format!("/u/{bob_id}/drop/{alice_token}/start"),
        json!({"name":"x.txt","size":1,"sha256":sha256_of(b"a")}),
    );
    assert_eq!(send(&app, start).await.status(), StatusCode::NOT_FOUND);

    // The same token still works on the tenant route Alice owns.
    let start = json_post(
        &format!("/u/{alice_id}/drop/{alice_token}/start"),
        json!({"name":"x.txt","size":1,"sha256":sha256_of(b"a")}),
    );
    assert_eq!(send(&app, start).await.status(), StatusCode::OK);

    // A link minted in the admin workspace is not usable on any tenant route.
    let admin_root = Drawer::open(dir.path()).await.unwrap();
    let folder = admin_root.mkdir("/inbox", "api").await.unwrap();
    let admin_cookie = {
        let login = send(
            &app,
            json_post(
                "/auth/login",
                json!({"username":"admin","password":"admin-password"}),
            ),
        )
        .await;
        assert_eq!(login.status(), StatusCode::OK);
        login.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };
    let create = Request::post(format!("/drawer/v1/nodes/{}/upload-links", folder.id))
        .header("cookie", &admin_cookie)
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"days":7,"max_files":5,"max_bytes":1048576}).to_string(),
        ))
        .unwrap();
    let link = json_of(send(&app, create).await).await;
    let admin_link_id = link["id"].as_str().unwrap().to_string();
    let admin_token = link["token"].as_str().unwrap().to_string();
    let start = json_post(
        &format!("/u/{alice_id}/drop/{admin_token}/start"),
        json!({"name":"y.txt","size":1,"sha256":sha256_of(b"a")}),
    );
    assert_eq!(send(&app, start).await.status(), StatusCode::NOT_FOUND);
    assert_ne!(admin_link_id, alice_link_id, "distinct links, distinct ids");
}
