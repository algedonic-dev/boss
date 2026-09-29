//! A stand-in for the GitHub REST API's App surface, for the tests that
//! drive the credential broker's GitHub App issuer through its REAL
//! adapter (`GitHubApi`) — the forge stub's precedent (`forge_stub.rs`):
//! the contract that matters (a JWT GitHub would accept, a 201 and only
//! a 201, a token that works only while it lives) sits in the adapter,
//! where an in-memory issuer cannot see it.
//!
//! It VERIFIES what GitHub verifies before it mints: the Bearer is an
//! RS256 JWT signed by the App's key (checked against its public half),
//! `iss` is the App id, `exp - iat` is at most ten minutes and `iat` is
//! not in the future. It answers as GitHub does:
//!   POST   /app/installations/{id}/access_tokens  201 {token, expires_at,
//!          permissions, repositories} — or `mint_status` when set
//!   GET    /repos/{owner}/{repo}                   200 while the token
//!          lives and the repo is readable to it, else 401 / 404
//!   DELETE /installation/token                     204 for a live token
//!          (and it dies), 401 for a dead one
//!   GET    /installation/repositories              200 live, 401 dead
//!
//! The private key is GENERATED once per test binary — a checked-in PEM
//! would trip the public mirror's secret scanning, and a test key is the
//! one thing no test needs to share with the tree.
//!
//! Test-only: declared `#[cfg(test)]` in `handlers/mod.rs`.

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::{Value, json};
use std::sync::{Arc, LazyLock, Mutex};

pub(crate) const APP_ID: u64 = 424242;
pub(crate) const INSTALLATION_ID: u64 = 7_000_123;

/// One RSA key per test binary. 2048 bits, the size GitHub issues.
pub(crate) static APP_KEY: LazyLock<rsa::RsaPrivateKey> = LazyLock::new(|| {
    rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).expect("generate a test App key")
});

/// The test key as GitHub downloads it: PKCS#1 PEM.
pub(crate) fn app_key_pem() -> String {
    use rsa::pkcs1::EncodeRsaPrivateKey as _;
    APP_KEY
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .expect("encode the test key")
        .to_string()
}

#[derive(Default)]
pub(crate) struct GitHubState {
    /// Every token the stub has minted and not seen revoked.
    pub live: Vec<String>,
    /// Tokens ended by DELETE /installation/token.
    pub revoked: Vec<String>,
    /// Repos a live token may read (`owner/name`).
    pub readable: Vec<String>,
    /// Installations of the App besides the root's own — a second
    /// organisation that installed it.
    pub other_installations: Vec<u64>,
    /// Answer the exchange with this status instead of 201.
    pub mint_status: Option<u16>,
    /// Mint tokens that then read NOTHING — the verify-by-effect failure.
    pub mints_useless_tokens: bool,
    /// Every exchange body the broker sent.
    pub mint_bodies: Vec<Value>,
    /// Why a JWT was refused, when one was.
    pub jwt_refusals: Vec<String>,
    /// Every request, as `"<METHOD> <path>"`.
    pub requests: Vec<String>,
    next: u64,
}

#[derive(Clone)]
pub(crate) struct GitHubStub {
    pub url: String,
    pub state: Arc<Mutex<GitHubState>>,
}

impl GitHubStub {
    pub fn minted(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.starts_with("POST /app/installations/"))
            .count()
    }
}

/// Serve a stub whose live tokens may read `readable`.
pub(crate) async fn serve(readable: &[&str]) -> GitHubStub {
    let state = Arc::new(Mutex::new(GitHubState {
        readable: readable.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }));
    let st = state.clone();
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let st = st.clone();
            async move { answer(&st, method, uri, headers, body) }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    GitHubStub {
        url: format!("http://{addr}"),
        state,
    }
}

fn b64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()
}

/// GitHub's own checks on an App JWT, against the test key's public half.
pub(crate) fn judge_jwt(jwt: &str) -> Result<(), String> {
    use rsa::signature::Verifier as _;
    let parts: Vec<&str> = jwt.split('.').collect();
    let [h, c, s] = parts.as_slice() else {
        return Err("not three segments".into());
    };
    let header: Value =
        serde_json::from_slice(&b64(h).ok_or("header b64")?).map_err(|_| "header json")?;
    if header["alg"] != "RS256" {
        return Err(format!("alg {}", header["alg"]));
    }
    let key = rsa::pkcs1v15::VerifyingKey::<rsa::sha2::Sha256>::new(APP_KEY.to_public_key());
    let sig = rsa::pkcs1v15::Signature::try_from(b64(s).ok_or("sig b64")?.as_slice())
        .map_err(|_| "signature shape")?;
    key.verify(format!("{h}.{c}").as_bytes(), &sig)
        .map_err(|_| "signature does not verify against the App key")?;
    let claims: Value =
        serde_json::from_slice(&b64(c).ok_or("claims b64")?).map_err(|_| "claims json")?;
    if claims["iss"] != json!(APP_ID) {
        return Err(format!("iss {}", claims["iss"]));
    }
    let (iat, exp) = (
        claims["iat"].as_i64().ok_or("no iat")?,
        claims["exp"].as_i64().ok_or("no exp")?,
    );
    let now = chrono::Utc::now().timestamp();
    if exp - iat > 600 {
        return Err(format!(
            "exp - iat = {}s, over GitHub's ten minutes",
            exp - iat
        ));
    }
    if iat > now {
        return Err("iat in the future".into());
    }
    if exp <= now {
        return Err("expired".into());
    }
    Ok(())
}

fn token_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("token "))
        .map(str::to_string)
}

fn answer(
    st: &Mutex<GitHubState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut s = st.lock().unwrap();
    let path = uri.path().to_string();
    s.requests.push(format!("{method} {path}"));
    if headers.get("user-agent").is_none() {
        // GitHub's own refusal of an anonymous client.
        return (StatusCode::FORBIDDEN, "user-agent required").into_response();
    }
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), segs.as_slice()) {
        ("POST", ["app", "installations", id, "access_tokens"]) => {
            let bearer = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default()
                .to_string();
            if let Err(why) = judge_jwt(&bearer) {
                s.jwt_refusals.push(why);
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({"message": "A JSON web token could not be decoded"})),
                )
                    .into_response();
            }
            let known = std::iter::once(INSTALLATION_ID)
                .chain(s.other_installations.iter().copied())
                .any(|i| *id == i.to_string());
            if !known {
                return (
                    StatusCode::NOT_FOUND,
                    axum::Json(json!({"message": "Not Found"})),
                )
                    .into_response();
            }
            let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            s.mint_bodies.push(sent.clone());
            if let Some(code) = s.mint_status {
                let status = StatusCode::from_u16(code).unwrap();
                return (
                    status,
                    axum::Json(json!({"message": "stub refusal", "documentation_url": "x"})),
                )
                    .into_response();
            }
            s.next += 1;
            let token = format!("ghs_stub{:032}", s.next);
            if !s.mints_useless_tokens {
                s.live.push(token.clone());
            }
            let expires = chrono::Utc::now() + chrono::Duration::hours(1);
            let permissions = sent.get("permissions").cloned().unwrap_or(
                json!({"contents": "write", "metadata": "read", "administration": "write"}),
            );
            let repositories: Vec<Value> = sent
                .get("repositories")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|n| json!({"name": n}))
                .collect();
            (
                StatusCode::CREATED,
                axum::Json(json!({
                    "token": token,
                    "expires_at": expires.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "permissions": permissions,
                    "repository_selection": if repositories.is_empty() { "all" } else { "selected" },
                    "repositories": repositories,
                })),
            )
                .into_response()
        }
        ("GET", ["repos", owner, name]) => {
            let Some(t) = token_of(&headers) else {
                return StatusCode::UNAUTHORIZED.into_response();
            };
            if !s.live.contains(&t) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            if s.readable.contains(&format!("{owner}/{name}")) {
                axum::Json(json!({"full_name": format!("{owner}/{name}")})).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        ("DELETE", ["installation", "token"]) => {
            let t = token_of(&headers).unwrap_or_default();
            if let Some(i) = s.live.iter().position(|x| *x == t) {
                s.live.remove(i);
                s.revoked.push(t);
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
        ("GET", ["installation", "repositories"]) => {
            let t = token_of(&headers).unwrap_or_default();
            if s.live.contains(&t) {
                axum::Json(json!({"total_count": 1, "repositories": []})).into_response()
            } else {
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
