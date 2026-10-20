//! Drawer: versioned filesystem storage for agents, over REST and MCP.
pub mod accounts;
pub mod activity;
pub mod api;
pub mod archive;
pub mod auth;
pub mod background;
pub mod backup;
pub mod blob;
pub mod cleanup;
pub mod cli;
pub mod deposits;
pub mod error;
pub mod links;
pub mod mcp;
pub mod mounts;
pub mod service;
pub mod storage;
pub mod sync;
pub mod transfers;
pub mod update;
pub mod vault;

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use std::{path::PathBuf, sync::Arc};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
};

use service::Drawer;

/// Largest request body accepted, so uploads of PDFs, Office files and media fit.
pub const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

/// Build the full HTTP app: REST at /drawer/v1, MCP at /mcp, and the UI if `ui_dir` exists.
pub fn app(svc: Drawer, token: Option<String>, ui_dir: Option<PathBuf>) -> Router {
    app_with_auth(svc, token, ui_dir, None, false, None)
}

/// Password-authenticated browser sessions. A configured API token remains available for MCP/CLI.
/// With password auth enabled, request-controlled author headers can no longer impersonate actors.
/// `cors_origins` is the parsed `DRAWER_CORS_ORIGINS` list; `None` keeps the permissive default
/// for an open server, and is ignored entirely when a password is configured.
pub fn app_with_auth(
    svc: Drawer,
    token: Option<String>,
    ui_dir: Option<PathBuf>,
    admin_password: Option<String>,
    secure_cookie: bool,
    cors_origins: Option<Vec<String>>,
) -> Router {
    let password_mode = admin_password.as_ref().is_some_and(|s| !s.is_empty());
    let token_mode = token.is_some();
    let accounts = accounts::Accounts::new(svc.clone());
    let audit_svc = svc.clone();
    let mcp_svc = svc.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::DrawerMcp::new(mcp_svc.clone()).with_verified_admin(password_mode)),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    // Top level, ahead of the UI fallback, so `/health` is never answered with index.html.
    let mut app = Router::new()
        .route("/health", axum::routing::get(api::health))
        .route(
            "/u/{id}/s/{token}",
            axum::routing::get(accounts::shared).with_state(accounts.clone()),
        )
        .route(
            "/u/{id}/drop/{token}",
            axum::routing::get(accounts::landing)
                .post(accounts::upload)
                .with_state(accounts.clone())
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/u/{id}/drop/{token}/start",
            axum::routing::post(accounts::drop_start).with_state(accounts.clone()),
        )
        .route(
            "/u/{id}/drop/{token}/append/{chunk}",
            axum::routing::put(accounts::drop_append)
                .with_state(accounts.clone())
                .layer(DefaultBodyLimit::max(deposits::MAX_CHUNK_BYTES)),
        )
        .route(
            "/u/{id}/drop/{token}/complete/{chunk}",
            axum::routing::post(accounts::drop_complete).with_state(accounts.clone()),
        )
        .route(
            "/u/{id}/drop/{token}/cancel/{chunk}",
            axum::routing::delete(accounts::drop_cancel).with_state(accounts.clone()),
        )
        .route(
            "/drop/{token}",
            axum::routing::get(deposits::landing)
                .post(deposits::upload)
                .with_state(svc.clone())
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route(
            "/drop/{token}/start",
            axum::routing::post(deposits::start).with_state(svc.clone()),
        )
        .route(
            "/drop/{token}/append/{chunk}",
            axum::routing::put(deposits::append)
                .with_state(svc.clone())
                .layer(DefaultBodyLimit::max(deposits::MAX_CHUNK_BYTES)),
        )
        .route(
            "/drop/{token}/complete/{chunk}",
            axum::routing::post(deposits::complete).with_state(svc.clone()),
        )
        .route(
            "/drop/{token}/cancel/{chunk}",
            axum::routing::delete(deposits::cancel).with_state(svc.clone()),
        )
        .route(
            "/s/{token}",
            axum::routing::get(api::download_shared).with_state(svc.clone()),
        )
        .nest(
            "/drawer/v1",
            api::router()
                .with_state(svc)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn(session_delete_ok))
        .layer(middleware::from_fn_with_state(
            audit_svc.clone(),
            activity::record,
        ));
    if let Some(password) = admin_password.filter(|s| !s.is_empty()) {
        let gate = auth::AuthGate::new(password, token, secure_cookie).with_accounts(accounts);
        app = app.merge(
            Router::new()
                .route("/auth/login", axum::routing::post(auth::login))
                .route("/auth/logout", axum::routing::post(auth::logout))
                .route("/auth/me", axum::routing::get(auth::me))
                .with_state(gate.clone())
                .layer(connect_info())
                .layer(middleware::from_fn_with_state(audit_svc, activity::record)),
        );
        app = app.layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let gate = gate.clone();
            async move {
                let path = req.uri().path();
                let restricted = path.starts_with("/drawer/v1") || path.starts_with("/mcp");
                if restricted {
                    let Some(identity) = gate.identity(req.headers()).await else {
                        return StatusCode::UNAUTHORIZED.into_response();
                    };
                    // The session name is chosen by us, not the caller; an unparsable value
                    // is skipped rather than allowed to fail the request.
                    if let Ok(name) = identity.name().parse() {
                        req.headers_mut().insert("x-drawer-author", name);
                    }
                    req.extensions_mut().insert(auth::VerifiedActor);
                    if let Some(account) = identity.account {
                        if path_admin(req.uri().path()) {
                            return StatusCode::FORBIDDEN.into_response();
                        }
                        let Some(accounts) = gate.accounts.as_ref() else {
                            return StatusCode::UNAUTHORIZED.into_response();
                        };
                        match accounts.tenant(&account).await {
                            Ok((_, router)) => {
                                use tower::ServiceExt;
                                req.extensions_mut().clear();
                                req.extensions_mut().insert(auth::VerifiedActor);
                                return router.oneshot(req).await.into_response();
                            }
                            Err(error) => return error.into_response(),
                        }
                    }
                }
                next.run(req).await
            }
        }));
    } else if let Some(token) = token {
        app = app.layer(middleware::from_fn(move |req: Request, next: Next| {
            let token = token.clone();
            async move { check_token(&token, req, next).await }
        }));
    }
    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").exists()) {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }
    // Cookie auth must never be combined with cross-origin access, and the open mode is
    // cross-origin only when the operator lists origins in DRAWER_CORS_ORIGINS.
    match cors_layer(password_mode, token_mode, cors_origins) {
        Some(layer) => app.layer(layer),
        None => app,
    }
}

/// The CORS policy: no headers when a password protects the instance, and no headers in the
/// open/no-auth mode either unless `DRAWER_CORS_ORIGINS` lists the origins that may call the
/// API. Token mode keeps the permissive default its callers already expect.
fn cors_layer(
    password_mode: bool,
    token_mode: bool,
    cors_origins: Option<Vec<String>>,
) -> Option<CorsLayer> {
    if password_mode {
        return None;
    }
    if cors_origins.is_none() && !token_mode {
        return None;
    }
    let mut layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            axum::http::HeaderName::from_static("x-upload-offset"),
        ]);
    match cors_origins {
        // Explicit allow-list, or the documented `*` default for an open server.
        Some(origins) if origins.iter().any(|origin| origin == "*") => {
            layer = layer.allow_origin(tower_http::cors::Any);
        }
        Some(origins) => {
            let values: Vec<_> = origins
                .iter()
                .filter_map(|origin| origin.parse().ok())
                .collect();
            layer = layer.allow_origin(values);
        }
        None => {
            layer = layer.allow_origin(tower_http::cors::Any);
        }
    }
    Some(layer)
}

/// rmcp answers a client's session-closing `DELETE /mcp` with 202, which the Python MCP SDK
/// (used by Hermes and many other harnesses) logs as a failed termination. The session is
/// closed either way, so report it as 200.
async fn session_delete_ok(req: Request, next: Next) -> Response {
    let closing = req.method() == Method::DELETE && req.uri().path() == "/mcp";
    let mut resp = next.run(req).await;
    if closing && resp.status() == StatusCode::ACCEPTED {
        *resp.status_mut() = StatusCode::OK;
    }
    resp
}

/// Public capabilities bypass the instance-wide bearer token: the download link, the deposit
/// link and its chunked session, and the tenant-scoped equivalents. Nothing else is exempt.
async fn check_token(token: &str, req: Request, next: Next) -> Response {
    if is_public_capability(req.method(), req.uri().path()) {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| auth::equal(t.as_bytes(), token.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// `GET /s/{token}` and every method on a deposit capability. The first path segment after
/// `/drop/` is the 64-hex token, so `/start`, `/append/{id}`, `/complete/{id}` and `/cancel/{id}`
/// are exempt too; the tenant prefix `/u/{id}/` is optional. Nothing else is.
fn is_public_capability(method: &Method, path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let (kind, token, step) =
        if parts.len() >= 5 && parts[1] == "u" && uuid::Uuid::parse_str(parts[2]).is_ok() {
            (
                parts[3],
                parts[4],
                parts.get(5).copied().unwrap_or_default(),
            )
        } else if parts.len() >= 3 {
            (
                parts[1],
                parts[2],
                parts.get(3).copied().unwrap_or_default(),
            )
        } else {
            return false;
        };
    let hex = token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit());
    match kind {
        "s" => method == Method::GET && step.is_empty() && hex,
        "drop" => {
            hex && match step {
                "" => matches!(*method, Method::GET | Method::POST),
                "start" => method == Method::POST,
                "append" => method == Method::PUT,
                "complete" => method == Method::POST,
                "cancel" => method == Method::DELETE,
                _ => false,
            }
        }
        _ => false,
    }
}

fn path_admin(path: &str) -> bool {
    ["/drawer/v1/users", "/drawer/v1/mounts", "/drawer/v1/update"]
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}
/// Tenant routers persist for the session manager lifetime; never recreate MCP per request.
fn private_app(svc: Drawer, actor: String) -> Router {
    let mcp_svc = svc.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::DrawerMcp::new(mcp_svc.clone()).with_verified_actor(actor.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    Router::new()
        .nest(
            "/drawer/v1",
            api::router()
                .with_state(svc.clone())
                .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn(session_delete_ok))
        .layer(middleware::from_fn_with_state(svc, activity::record))
}

/// `ConnectInfo<SocketAddr>` is only populated when the server is built with
/// `into_make_service_with_connect_info`, so tests and plain `axum::serve` get a placeholder
/// instead of a missing extension. The login limiter then keys on a stable address.
fn connect_info() -> axum::middleware::FromFnLayer<
    impl FnMut(Request, Next) -> ConnectInfoFuture + Clone + Send + Sync + 'static,
    (),
    (Request,),
> {
    middleware::from_fn(move |mut req: Request, next: Next| {
        Box::pin(async move {
            if req
                .extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .is_none()
            {
                req.extensions_mut().insert(axum::extract::ConnectInfo(
                    std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
                ));
            }
            next.run(req).await
        }) as ConnectInfoFuture
    })
}

type ConnectInfoFuture = std::pin::Pin<Box<dyn Future<Output = Response> + Send>>;
