//! Admin password hashing, login rate limiting, and cross-origin policy.
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use drawer::service::Drawer;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt as _;

async fn send(app: &Router, req: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(req).await.unwrap()
}

async fn body(resp: axum::response::Response) -> Value {
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn json_post(uri: &str, value: Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

fn origin(value: &'static str) -> Request<Body> {
    Request::get("/health")
        .header(header::ORIGIN, value)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn admin_login_accepts_the_password_and_rejects_anything_else() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        None,
        None,
        Some("correct horse battery staple".into()),
        false,
        None,
    );

    let wrong = send(&app, json_post("/auth/login", json!({"password":"wrong"}))).await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let right = send(
        &app,
        json_post(
            "/auth/login",
            json!({"password":"correct horse battery staple"}),
        ),
    )
    .await;
    assert_eq!(right.status(), StatusCode::OK);
    assert!(
        right.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("HttpOnly")
    );
    assert_eq!(body(right).await["user"], "admin");
}

#[tokio::test]
async fn repeated_failures_return_429_with_retry_after_and_success_resets() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        None,
        None,
        Some("correct horse battery staple".into()),
        false,
        None,
    );

    // Five failures are still answered with 401; the sixth is refused with the delay.
    for _ in 0..5 {
        assert_eq!(
            send(&app, json_post("/auth/login", json!({"password":"wrong"})))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let locked = send(&app, json_post("/auth/login", json!({"password":"wrong"}))).await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(locked.headers()["retry-after"], "1");
    // A correct password is refused too while the delay is owed, so the limiter cannot be
    // bypassed by guessing and retrying.
    assert_eq!(
        send(
            &app,
            json_post(
                "/auth/login",
                json!({"password":"correct horse battery staple"})
            )
        )
        .await
        .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn successful_login_resets_the_failure_counter() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        None,
        None,
        Some("correct horse battery staple".into()),
        false,
        None,
    );

    for _ in 0..4 {
        assert_eq!(
            send(&app, json_post("/auth/login", json!({"password":"wrong"})))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    // A success clears the counter, so the next five failures are still free.
    assert_eq!(
        send(
            &app,
            json_post(
                "/auth/login",
                json!({"password":"correct horse battery staple"})
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    for _ in 0..5 {
        assert_eq!(
            send(&app, json_post("/auth/login", json!({"password":"wrong"})))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn open_mode_answers_no_cors_headers_without_a_configured_origin_list() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    let response = send(&app, origin("https://example.com")).await;
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "an open server without DRAWER_CORS_ORIGINS must not advertise cross-origin access"
    );
}

#[tokio::test]
async fn configured_origins_allow_only_the_listed_origin() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        None,
        None,
        None,
        false,
        Some(vec!["https://drawer.example.com".into()]),
    );

    let allowed = send(&app, origin("https://drawer.example.com")).await;
    assert_eq!(
        allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        HeaderValue::from_static("https://drawer.example.com")
    );

    let refused = send(&app, origin("https://evil.example.com")).await;
    assert!(
        refused
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn password_mode_never_sends_cors_headers() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        None,
        None,
        Some("correct horse battery staple".into()),
        false,
        None,
    );

    let response = send(&app, origin("https://example.com")).await;
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn token_mode_keeps_permissive_cors() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(
        Drawer::open(dir.path()).await.unwrap(),
        Some("secret".into()),
        None,
    );

    let response = send(&app, origin("https://example.com")).await;
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}
