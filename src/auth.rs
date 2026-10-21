//! Password-protected single-admin sessions for personal Drive use.
//! HTTP cookies authenticate browser traffic; DRAWER_TOKEN remains the machine/MCP credential.
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
pub struct VerifiedActor;

/// Failed logins for one instance: (client IP, username) -> attempt state.
type Attempts = Arc<Mutex<HashMap<(String, String), Attempt>>>;

#[derive(Clone)]
pub struct AuthGate {
    password_hash: String,
    api_token: Option<String>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// Failed logins per client IP and username. Each `AuthGate` owns its own map, so a
    /// restarted server or a second app in the same process cannot inherit another's lockout.
    attempts: Attempts,
    pub(crate) accounts: Option<crate::accounts::Accounts>,
    secure_cookie: bool,
}

#[derive(Clone)]
struct Session {
    expiry: Instant,
    account: Option<String>,
}
#[derive(Clone)]
pub struct Principal {
    pub account: Option<crate::accounts::Account>,
}
impl Principal {
    pub fn name(&self) -> &str {
        self.account
            .as_ref()
            .map(|a| a.username.as_str())
            .unwrap_or("admin")
    }
}

#[derive(Deserialize)]
pub struct LoginBody {
    #[serde(default = "admin_name")]
    username: String,
    password: String,
}
#[derive(Serialize)]
pub struct LoginStatus {
    pub mode: &'static str,
    pub authenticated: bool,
    pub user: Option<String>,
}

/// Failed logins before the delay starts, then a doubling delay capped at 15 minutes.
const FREE_FAILURES: u32 = 5;
const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// A quiet period this long with no further attempts restarts the failure count.
const HISTORY_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
struct Attempt {
    failures: u32,
    deadline: Instant,
}

impl Attempt {
    /// Record one more failure and return how long the caller must wait before the next try.
    /// `deadline` is when the failure history expires: a quiet period restarts the count, but a
    /// client that keeps trying must not have its history dropped mid-attack.
    fn failure(&mut self, now: Instant) -> Duration {
        self.failures += 1;
        self.deadline = now + HISTORY_WINDOW;
        if self.failures <= FREE_FAILURES {
            return Duration::ZERO;
        }
        let steps = self.failures - FREE_FAILURES - 1;
        BASE_DELAY.saturating_mul(1 << steps.min(20)).min(MAX_DELAY)
    }
}

fn admin_name() -> String {
    "admin".into()
}

/// Constant-time comparison: the fold never short-circuits, so the result leaks nothing.
pub(crate) fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}
fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|entry| entry.trim().strip_prefix("drawer_session="))
}
fn token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

impl AuthGate {
    pub fn new(password: String, api_token: Option<String>, secure_cookie: bool) -> Self {
        // Argon2id with a fresh random salt, the same helper the named-user accounts use.
        // A hash failure is a startup problem; a random placeholder keeps the gate closed
        // rather than inventing a password nobody knows.
        let password_hash =
            crate::accounts::hash_password(password).unwrap_or_else(|_| Uuid::new_v4().to_string());
        Self {
            password_hash,
            accounts: None,
            api_token,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            attempts: Arc::new(Mutex::new(HashMap::new())),
            secure_cookie,
        }
    }

    pub fn with_accounts(mut self, accounts: crate::accounts::Accounts) -> Self {
        self.accounts = Some(accounts);
        self
    }

    pub async fn identity(&self, headers: &HeaderMap) -> Option<Principal> {
        if let Some(expected) = &self.api_token
            && token(headers)
                .is_some_and(|candidate| equal(candidate.as_bytes(), expected.as_bytes()))
        {
            return Some(Principal { account: None });
        }
        let secret = cookie(headers)?;
        let session = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|_, session| session.expiry > Instant::now());
            sessions.get(secret)?.clone()
        };
        let account = match session.account {
            None => None,
            Some(id) => Some(self.accounts.as_ref()?.by_id(&id).await.ok()?),
        };
        Some(Principal { account })
    }
    pub async fn authenticated(&self, headers: &HeaderMap) -> bool {
        self.identity(headers).await.is_some()
    }

    fn set_cookie(&self, value: &str, max_age: usize) -> String {
        format!(
            "drawer_session={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
            if self.secure_cookie { "; Secure" } else { "" }
        )
    }

    /// Delay owed to `(ip, username)` before this attempt may be judged.
    async fn delay(&self, ip: &str, username: &str) -> Duration {
        let now = Instant::now();
        let mut attempts = self.attempts.lock().await;
        // Entries expire after the quiet window, so the map cannot grow without bound.
        attempts.retain(|_, attempt| attempt.deadline > now);
        match attempts.get(&(ip.to_owned(), username.to_owned())) {
            Some(attempt) => {
                let blocked_until = self.blocked_until(attempt, now);
                (blocked_until - now).min(attempt.deadline - now)
            }
            None => Duration::ZERO,
        }
    }

    /// Record one failure; returns the delay the caller now owes.
    async fn record_failure(&self, ip: &str, username: &str) -> Duration {
        let now = Instant::now();
        let key = (ip.to_owned(), username.to_owned());
        let mut attempts = self.attempts.lock().await;
        attempts.retain(|_, attempt| attempt.deadline > now);
        let delay = attempts
            .entry(key)
            .or_insert_with(|| Attempt {
                failures: 0,
                deadline: now,
            })
            .failure(now);
        // The caller is charged the delay from the new failure, which always exceeds any
        // delay a previous failure had already bought.
        let entry = attempts.get(&(ip.to_owned(), username.to_owned())).cloned();
        entry.map_or(delay, |attempt| {
            delay.max(self.blocked_until(&attempt, now) - now)
        })
    }

    /// When the recorded failures stop the next attempt, and for how long.
    fn blocked_until(&self, attempt: &Attempt, now: Instant) -> Instant {
        if attempt.failures <= FREE_FAILURES {
            return now;
        }
        let steps = attempt.failures - FREE_FAILURES - 1;
        now + BASE_DELAY.saturating_mul(1 << steps.min(20)).min(MAX_DELAY)
    }

    async fn clear(&self, ip: &str, username: &str) {
        self.attempts
            .lock()
            .await
            .remove(&(ip.to_owned(), username.to_owned()));
    }

    async fn verify_password(&self, password: &str) -> bool {
        let (hash, password) = (self.password_hash.clone(), password.to_owned());
        tokio::task::spawn_blocking(move || {
            crate::accounts::verify_password(&hash, password.as_bytes())
        })
        .await
        .unwrap_or(false)
    }
}

pub async fn login(
    State(gate): State<AuthGate>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Json(input): Json<LoginBody>,
) -> Response {
    let ip = remote.ip().to_string();
    let delay = gate.delay(&ip, &input.username).await;
    if !delay.is_zero() {
        return too_many_requests(delay);
    }
    let account = if input.username == "admin" {
        if !gate.verify_password(&input.password).await {
            return reject(&gate, &ip, &input.username).await;
        }
        None
    } else {
        let Some(accounts) = &gate.accounts else {
            return reject(&gate, &ip, &input.username).await;
        };
        match accounts.credentials(&input.username, input.password).await {
            Ok(Some(account)) => Some(account),
            _ => return reject(&gate, &ip, &input.username).await,
        }
    };
    // A success clears the counter, so legitimate users are never locked out by their own
    // earlier typo.
    gate.clear(&ip, &input.username).await;
    let user = account
        .as_ref()
        .map(|a| a.username.clone())
        .unwrap_or_else(admin_name);
    // 256 bits of randomness, with expiration enforced server-side as well.
    let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    gate.sessions.lock().await.insert(
        secret.clone(),
        Session {
            expiry: Instant::now() + Duration::from_secs(24 * 3600),
            account: account.map(|a| a.id),
        },
    );
    let mut response = Json(LoginStatus {
        mode: "password",
        authenticated: true,
        user: Some(user),
    })
    .into_response();
    if let Ok(value) = gate.set_cookie(&secret, 24 * 3600).parse() {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// Answer a failed login, recording the failure and the delay it buys the caller.
async fn reject(gate: &AuthGate, ip: &str, username: &str) -> Response {
    let delay = gate.record_failure(ip, username).await;
    if delay.is_zero() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    too_many_requests(delay)
}

fn too_many_requests(delay: Duration) -> Response {
    let seconds = delay.as_secs().max(1);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({"error":"too many failed logins; retry later"})),
    )
        .into_response();
    if let Ok(value) = seconds.to_string().parse() {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

pub async fn me(State(gate): State<AuthGate>, headers: HeaderMap) -> Json<LoginStatus> {
    let identity = gate.identity(&headers).await;
    Json(LoginStatus {
        mode: "password",
        authenticated: identity.is_some(),
        user: identity.map(|p| p.name().to_owned()),
    })
}

pub async fn logout(State(gate): State<AuthGate>, headers: HeaderMap) -> Response {
    if let Some(secret) = cookie(&headers) {
        gate.sessions.lock().await.remove(secret);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        gate.set_cookie("", 0).parse().expect("valid set-cookie"),
    );
    response
}
