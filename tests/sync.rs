//! Runs the desktop client against a real HTTP server, with isolated credentials/state.
use drawer::{
    blob::BlobStore,
    service::{Drawer, Ref, WriteOpts},
};
use serde_json::{Value, json};
use std::{path::Path, process::Output};

async fn server(s: Drawer) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = drawer::app_with_auth(
        s,
        Some("machine".into()),
        None,
        Some("password".into()),
        false,
        None,
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}
async fn sync(local: &Path, url: &str) -> Output {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_drawer"))
        .args([
            "sync",
            local.to_str().unwrap(),
            "--url",
            url,
            "--remote",
            "/Desktop",
        ])
        .env("DRAWER_TOKEN", "machine")
        .output()
        .await
        .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[tokio::test]
async fn desktop_sync_roundtrip_conflict_and_offline_recovery() {
    let store = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let s = Drawer::open(store.path()).await.unwrap();
    let (url, task) = server(s.clone()).await;
    std::fs::write(local.path().join("note.txt"), b"desktop first").unwrap();
    assert_eq!(success(sync(local.path(), &url).await)["uploaded"], 1);
    assert_eq!(
        s.read(Ref::Path("/Desktop/note.txt"), None)
            .await
            .unwrap()
            .1,
        b"desktop first"
    );
    s.write(
        "/Desktop/remote.txt",
        b"from server",
        WriteOpts::new("admin", ""),
    )
    .await
    .unwrap();
    assert_eq!(success(sync(local.path(), &url).await)["downloaded"], 1);
    assert_eq!(
        std::fs::read(local.path().join("remote.txt")).unwrap(),
        b"from server"
    );
    std::fs::write(local.path().join("note.txt"), b"desktop conflict").unwrap();
    s.write(
        "/Desktop/note.txt",
        b"server conflict",
        WriteOpts::new("admin", ""),
    )
    .await
    .unwrap();
    assert_eq!(success(sync(local.path(), &url).await)["conflicts"], 1);
    assert_eq!(
        std::fs::read(local.path().join("note.txt")).unwrap(),
        b"server conflict"
    );
    let conflict = std::fs::read_dir(local.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".conflict-")
        })
        .unwrap();
    assert_eq!(std::fs::read(conflict).unwrap(), b"desktop conflict");
    success(sync(local.path(), &url).await);
    assert!(
        s.list("/Desktop")
            .await
            .unwrap()
            .iter()
            .any(|n| n.name.contains(".conflict-"))
    );
    task.abort();
    let _ = task.await;
    std::fs::write(local.path().join("offline.txt"), b"kept while offline").unwrap();
    assert!(!sync(local.path(), &url).await.status.success());
    assert_eq!(
        std::fs::read(local.path().join("offline.txt")).unwrap(),
        b"kept while offline"
    );
    // Same origin is part of the durable binding; restart on the original address.
    let listener = tokio::net::TcpListener::bind(url.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let app = drawer::app_with_auth(
        s.clone(),
        Some("machine".into()),
        None,
        Some("password".into()),
        false,
        None,
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    success(sync(local.path(), &url).await);
    assert_eq!(
        s.read(Ref::Path("/Desktop/offline.txt"), None)
            .await
            .unwrap()
            .1,
        b"kept while offline"
    );
    task.abort();
}
#[tokio::test]
async fn desktop_sync_resumes_a_persisted_partial_upload() {
    let store = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let s = Drawer::open(store.path()).await.unwrap();
    let (url, task) = server(s.clone()).await;
    s.mkdir("/Desktop", "admin").await.unwrap();
    let bytes = vec![42u8; 9 * 1024 * 1024];
    let hash = BlobStore::hash(&bytes);
    std::fs::write(local.path().join("large.bin"), &bytes).unwrap();
    let client = reqwest::Client::new();
    let transfer:Value=client.post(format!("{url}/drawer/v1/transfers")).bearer_auth("machine").json(&json!({"path":"/Desktop/large.bin","size":bytes.len(),"sha256":hash,"if_head_revision":0})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let id = transfer["id"].as_str().unwrap();
    client
        .put(format!("{url}/drawer/v1/transfers/{id}"))
        .bearer_auth("machine")
        .header("x-upload-offset", "0")
        .body(bytes[..8 * 1024 * 1024].to_vec())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    std::fs::create_dir_all(local.path().join(".drawer-sync")).unwrap();
    std::fs::write(local.path().join(".drawer-sync/state.json"),serde_json::to_vec(&json!({"version":1,"binding":format!("{url}\n/Desktop\nadmin"),"entries":{},"pending":{"large.bin":{"id":id,"hash":hash,"size":bytes.len(),"head":0}}})).unwrap()).unwrap();
    assert_eq!(success(sync(local.path(), &url).await)["uploaded"], 1);
    assert_eq!(
        s.read(Ref::Path("/Desktop/large.bin"), None)
            .await
            .unwrap()
            .1,
        bytes
    );
    let history: Value = client
        .get(format!("{url}/drawer/v1/transfers"))
        .bearer_auth("machine")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["transfers"].as_array().unwrap().len(), 1);
    assert_eq!(history["transfers"][0]["id"], id);
    assert_eq!(history["transfers"][0]["status"], "completed");
    task.abort();
}
#[tokio::test]
async fn desktop_sync_uses_named_user_credentials() {
    let store = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let s = Drawer::open(store.path()).await.unwrap();
    let (url, task) = server(s.clone()).await;
    reqwest::Client::new()
        .post(format!("{url}/drawer/v1/users"))
        .bearer_auth("machine")
        .json(&json!({"username":"desktop","password":"a-long-password","quota_bytes":100}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    std::fs::write(local.path().join("private.txt"), b"private desktop").unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_drawer"))
        .args(["sync", local.path().to_str().unwrap(), "--url", &url])
        .env_remove("DRAWER_TOKEN")
        .env("DRAWER_SYNC_USERNAME", "desktop")
        .env("DRAWER_SYNC_PASSWORD", "a-long-password")
        .output()
        .await
        .unwrap();
    assert_eq!(success(output)["uploaded"], 1);
    assert!(s.stat(Ref::Path("/Desktop/private.txt")).await.is_err());
    task.abort();
}

#[tokio::test]
async fn desktop_sync_rejects_corrupt_download_and_removes_temporary_file() {
    let store = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let s = Drawer::open(store.path()).await.unwrap();
    s.write("/Desktop/corrupt.txt", b"abc", WriteOpts::new("admin", ""))
        .await
        .unwrap();
    let hash = BlobStore::hash(b"abc");
    std::fs::write(
        store.path().join("blobs").join(&hash[..2]).join(&hash[2..]),
        b"xyz",
    )
    .unwrap();
    let (url, task) = server(s).await;
    let output = sync(local.path(), &url).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum mismatch"));
    assert!(!local.path().join("corrupt.txt").exists());
    assert!(
        !std::fs::read_dir(local.path().join(".drawer-sync"))
            .unwrap()
            .any(|e| e.unwrap().path().extension().is_some_and(|e| e == "part"))
    );
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_sync_cannot_write_through_external_symlink() {
    let store = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), local.path().join("escape")).unwrap();
    let s = Drawer::open(store.path()).await.unwrap();
    s.write(
        "/Desktop/escape/secret.txt",
        b"stay inside",
        WriteOpts::new("admin", ""),
    )
    .await
    .unwrap();
    let (url, task) = server(s).await;
    assert!(!sync(local.path(), &url).await.status.success());
    assert!(!outside.path().join("secret.txt").exists());
    task.abort();
}
