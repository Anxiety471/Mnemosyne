//! Named users get independent stores, so every path/id query and MCP tool is isolated.
use crate::{
    error::{Error, Result},
    service::Drawer,
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
#[derive(Clone, Serialize, sqlx::FromRow)]
pub struct Account {
    pub id: String,
    pub username: String,
    pub quota_bytes: i64,
    pub disabled: bool,
}
#[derive(Deserialize)]
pub struct NewAccount {
    username: String,
    password: String,
    quota_bytes: i64,
}
#[derive(Deserialize)]
pub struct Change {
    quota_bytes: Option<i64>,
    disabled: Option<bool>,
}
#[derive(Clone)]
pub struct Accounts {
    pub root: Drawer,
    cache: Arc<Mutex<HashMap<String, (Drawer, Router)>>>,
}
impl Accounts {
    pub fn new(root: Drawer) -> Self {
        Self {
            root,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub async fn tenant(&self, account: &Account) -> Result<(Drawer, Router)> {
        let mut cache = self.cache.lock().await;
        if let Some((s, router)) = cache.get(&account.id) {
            s.quota
                .store(account.quota_bytes, std::sync::atomic::Ordering::Release);
            return Ok((s.clone(), router.clone()));
        }
        let directory = self.root.data_dir.join("users").join(&account.id);
        tokio::fs::create_dir_all(&directory).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).await?;
        }
        let mut s = Drawer::open(&directory).await?;
        s.quota
            .store(account.quota_bytes, std::sync::atomic::Ordering::Release);
        s.public_prefix = format!("/u/{}", account.id);
        let router = crate::private_app(s.clone(), account.username.clone());
        cache.insert(account.id.clone(), (s.clone(), router.clone()));
        Ok((s, router))
    }
    pub async fn by_id(&self, id: &str) -> Result<Account> {
        sqlx::query_as(
            "SELECT id,username,quota_bytes,disabled FROM user_accounts WHERE id=? AND disabled=0",
        )
        .bind(id)
        .fetch_optional(&self.root.pool)
        .await?
        .ok_or_else(|| Error::NotFound("active user".into()))
    }
    pub async fn credentials(&self, name: &str, password: String) -> Result<Option<Account>> {
        let row: Option<(String,String,i64,bool,String)>=sqlx::query_as("SELECT id,username,quota_bytes,disabled,password_hash FROM user_accounts WHERE username=?").bind(name).fetch_optional(&self.root.pool).await?;
        let Some((id, username, quota_bytes, disabled, hash)) = row else {
            return Ok(None);
        };
        if disabled {
            return Ok(None);
        }
        let verified =
            tokio::task::spawn_blocking(move || verify_password(&hash, password.as_bytes()))
                .await
                .map_err(|e| Error::Internal(e.to_string()))?;
        Ok(verified.then_some(Account {
            id,
            username,
            quota_bytes,
            disabled,
        }))
    }
}
/// Header check shared by the account-management routes.
fn admin(headers: &HeaderMap) -> Result<()> {
    if headers.get("x-drawer-author").and_then(|v| v.to_str().ok()) != Some("admin") {
        return Err(Error::Forbidden("administrator required".into()));
    }
    Ok(())
}

pub async fn list(State(s): State<Drawer>, headers: HeaderMap) -> Result<Json<Vec<Account>>> {
    admin(&headers)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT id,username,quota_bytes,disabled FROM user_accounts ORDER BY username",
        )
        .fetch_all(&s.pool)
        .await?,
    ))
}
pub async fn create(
    State(s): State<Drawer>,
    headers: HeaderMap,
    Json(b): Json<NewAccount>,
) -> Result<Json<Account>> {
    admin(&headers)?;
    if b.username == "admin"
        || b.username.is_empty()
        || b.username.len() > 32
        || !b
            .username
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
        || !(12..=1024).contains(&b.password.len())
        || b.quota_bytes <= 0
    {
        return Err(Error::Invalid("lowercase username (1–32), password (12–1024 bytes) and positive quota required; admin is reserved".into()));
    }
    let password = b.password;
    let hash = hash_password(password)?;
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        username: b.username,
        quota_bytes: b.quota_bytes,
        disabled: false,
    };
    sqlx::query("INSERT INTO user_accounts(id,username,password_hash,quota_bytes) VALUES(?,?,?,?)")
        .bind(&account.id)
        .bind(&account.username)
        .bind(hash)
        .bind(account.quota_bytes)
        .execute(&s.pool)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                Error::Conflict("username already exists".into())
            } else {
                e.into()
            }
        })?;
    Ok(Json(account))
}
pub async fn change(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<Change>,
) -> Result<Json<Value>> {
    admin(&headers)?;
    if b.quota_bytes.is_some_and(|n| n <= 0) {
        return Err(Error::Invalid("quota must be positive".into()));
    }
    let rows=sqlx::query("UPDATE user_accounts SET quota_bytes=COALESCE(?,quota_bytes),disabled=COALESCE(?,disabled) WHERE id=?").bind(b.quota_bytes).bind(b.disabled).bind(id).execute(&s.pool).await?.rows_affected();
    if rows == 0 {
        return Err(Error::NotFound("user".into()));
    }
    Ok(Json(json!({"ok":true})))
}

pub async fn shared(
    State(accounts): State<Accounts>,
    Path((id, token)): Path<(String, String)>,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    let result = crate::api::download_shared(State(s.clone()), Path(token)).await;
    public_event(&s, "GET", "/u/{id}/s/{token}", result).await
}
pub async fn landing(
    State(accounts): State<Accounts>,
    Path((id, token)): Path<(String, String)>,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    crate::deposits::landing(State(s), Path(token)).await
}
pub async fn upload(
    State(accounts): State<Accounts>,
    Path((id, token)): Path<(String, String)>,
    q: axum::extract::Query<crate::deposits::Filename>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    use axum::response::IntoResponse;
    let result = crate::deposits::upload(State(s.clone()), Path(token), q, headers, body)
        .await
        .map(IntoResponse::into_response);
    public_event(&s, "POST", "/u/{id}/drop/{token}", result).await
}
/// A named user's deposit link uses the same chunked session as the admin's, against the
/// tenant's own store, so the public page works unchanged on `/u/{id}/drop/{token}`.
pub async fn drop_start(
    State(accounts): State<Accounts>,
    Path((id, token)): Path<(String, String)>,
    body: Json<crate::deposits::ChunkStart>,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    let result = crate::deposits::start_core(&s, &token, body.0)
        .await
        .map(axum::response::IntoResponse::into_response);
    public_event(&s, "POST", "/u/{id}/drop/{token}/start", result).await
}
pub async fn drop_append(
    State(accounts): State<Accounts>,
    Path((id, token, chunk)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    let result =
        crate::deposits::append_core(&s, &token, &chunk, headers.get("x-upload-offset"), body)
            .await
            .map(axum::response::IntoResponse::into_response);
    public_event(&s, "PUT", "/u/{id}/drop/{token}/append/{chunk}", result).await
}
pub async fn drop_complete(
    State(accounts): State<Accounts>,
    Path((id, token, chunk)): Path<(String, String, String)>,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    let result = crate::deposits::complete_core(&s, &token, &chunk)
        .await
        .map(axum::response::IntoResponse::into_response);
    public_event(&s, "POST", "/u/{id}/drop/{token}/complete/{chunk}", result).await
}
pub async fn drop_cancel(
    State(accounts): State<Accounts>,
    Path((id, token, chunk)): Path<(String, String, String)>,
) -> Result<axum::response::Response> {
    let account = accounts.by_id(&id).await?;
    let (s, _) = accounts.tenant(&account).await?;
    let result = crate::deposits::cancel_core(&s, &token, &chunk)
        .await
        .map(axum::response::IntoResponse::into_response);
    public_event(&s, "DELETE", "/u/{id}/drop/{token}/cancel/{chunk}", result).await
}

async fn public_event(
    s: &Drawer,
    method: &str,
    route: &str,
    result: Result<axum::response::Response>,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    if let Err(error)=sqlx::query("INSERT INTO activity(actor,method,route,status,created_at) VALUES('anonymous',?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))").bind(method).bind(route).bind(i64::from(response.status().as_u16())).execute(&s.pool).await { tracing::warn!(%error,"could not record public workspace activity"); }
    Ok(response)
}
/// Hash a password with Argon2id and a fresh random salt. Both the admin password and
/// named-user passwords use this, so the two paths can never drift apart.
pub(crate) fn hash_password(password: String) -> Result<String> {
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes())
        .map_err(|e| Error::Internal(e.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| Error::Internal(e.to_string()))
}
/// Constant-time Argon2id verification. `None` when the stored hash is malformed, so a
/// corrupted row is indistinguishable from a wrong password.
pub(crate) fn verify_password(hash: &str, password: &[u8]) -> bool {
    PasswordHash::new(hash)
        .ok()
        .is_some_and(|hash| Argon2::default().verify_password(password, &hash).is_ok())
}
