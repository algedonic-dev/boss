//! File-backed credential store + login/logout/me HTTP handlers.
//!
//! v1 OSS-quickstart auth. The contract:
//!
//! - Credentials live in a TOML file at `BOSS_AUTH_FILE` (default
//!   `/var/lib/boss/auth/credentials.toml`). One row per email
//!   with an Argon2id PHC hash, an optional one-time reset-token
//!   hash, and timestamps.
//! - Login: `POST /api/auth/login {email, password}` verifies
//!   against the file, looks up the Employee scope via
//!   `bootstrap_email`, mints a `boss_session` cookie via
//!   `session::Session::encode`.
//! - Logout: `POST /api/auth/logout` clears the cookie.
//! - Me: `GET /api/auth/me` decodes the session, returns the
//!   email + role + employee_id.
//!
//! Onboarding (admin-only):
//! - `POST /api/auth/onboard {email, password}` — creates a
//!   credential row. Caller must be authenticated as a role with
//!   `policy:auth-admin` (which platform-admin / ceo / coo carry).
//! - `POST /api/auth/issue-reset {email}` — issues a one-time
//!   reset token (returns it to the admin; admin shares with the
//!   user out-of-band).
//! - `POST /api/auth/reset {email, token, password}` — consumes
//!   the token, rotates the password.
//!
//! What's deliberately small:
//! - No CSRF token. Cookie is `SameSite=Strict`. Production-grade
//!   tenants front the gateway with a proxy that handles CSRF.
//! - No account lockout / brute-force protection. Lives at the
//!   proxy tier.
//! - No password policy enforcement. Operators choose; SPA
//!   surfaces a min-length nudge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::session::{self, Session};

// --------------------------------------------------------------------
// On-disk model.
// --------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credential {
    pub email: String,
    pub password_hash: String,
    pub created_at: DateTime<Utc>,
    pub last_rotated: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_token: Option<ResetToken>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetToken {
    /// Sha-256 hex of the token. The plaintext token is returned
    /// to the admin once at issue-time and never persisted.
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CredentialsFile {
    #[serde(default)]
    credential: Vec<Credential>,
}

// --------------------------------------------------------------------
// In-memory store.
// --------------------------------------------------------------------

#[derive(Clone)]
pub struct CredentialStore {
    inner: Arc<RwLock<Inner>>,
}

struct Inner {
    path: PathBuf,
    by_email: HashMap<String, Credential>,
}

impl CredentialStore {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let by_email = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let file: CredentialsFile =
                toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            file.credential
                .into_iter()
                .map(|c| (c.email.to_lowercase(), c))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(Self {
            inner: Arc::new(RwLock::new(Inner { path, by_email })),
        })
    }

    pub fn verify(&self, email: &str, password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let inner = self.inner.read().map_err(|_| anyhow!("store poisoned"))?;
        let cred = inner
            .by_email
            .get(&email)
            .ok_or_else(|| anyhow!("invalid credentials"))?;
        let hash = PasswordHash::new(&cred.password_hash)
            .map_err(|e| anyhow!("malformed password hash: {e}"))?;
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .map_err(|_| anyhow!("invalid credentials"))?;
        Ok(())
    }

    pub fn upsert(&self, email: &str, password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let now = Utc::now();
        let salt = generate_salt();
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow!("argon2 hash: {e}"))?
            .to_string();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .entry(email.clone())
            .or_insert_with(|| Credential {
                email: email.clone(),
                password_hash: String::new(),
                created_at: now,
                last_rotated: now,
                reset_token: None,
            });
        entry.password_hash = hash;
        entry.last_rotated = now;
        entry.reset_token = None;
        save_locked(&inner)
    }

    pub fn remove(&self, email: &str) -> Result<bool> {
        let email = email.to_lowercase();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let removed = inner.by_email.remove(&email).is_some();
        if removed {
            save_locked(&inner)?;
        }
        Ok(removed)
    }

    pub fn list_emails(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<String> = inner.by_email.keys().cloned().collect();
        out.sort();
        out
    }

    pub fn contains(&self, email: &str) -> bool {
        let email = email.to_lowercase();
        match self.inner.read() {
            Ok(g) => g.by_email.contains_key(&email),
            Err(_) => false,
        }
    }

    /// Issue a one-time reset token. Returns the plaintext token
    /// the admin shares out-of-band; only the sha256 hash is
    /// persisted. Tokens expire after `ttl_seconds`.
    pub fn issue_reset_token(&self, email: &str, ttl_seconds: i64) -> Result<String> {
        let email = email.to_lowercase();
        let token = random_token(24);
        let token_hash = sha256_hex(&token);
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_seconds);
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .get_mut(&email)
            .ok_or_else(|| anyhow!("no credential for {email} — onboard first"))?;
        entry.reset_token = Some(ResetToken {
            token_hash,
            expires_at,
        });
        save_locked(&inner)?;
        Ok(token)
    }

    /// Consume a one-time reset token + set a new password. Fails
    /// on missing/expired/wrong-token, with a generic error string
    /// (no enumeration leak).
    pub fn consume_reset_token(&self, email: &str, token: &str, new_password: &str) -> Result<()> {
        let email = email.to_lowercase();
        let token_hash = sha256_hex(token);
        // Re-hash the new password OUTSIDE the lock so we don't
        // hold the mutex across the (possibly slow) Argon2 call.
        let salt = generate_salt();
        let new_hash = Argon2::default()
            .hash_password(new_password.as_bytes(), &salt)
            .map_err(|e| anyhow!("argon2 hash: {e}"))?
            .to_string();
        let mut inner = self.inner.write().map_err(|_| anyhow!("store poisoned"))?;
        let entry = inner
            .by_email
            .get_mut(&email)
            .ok_or_else(|| anyhow!("invalid reset token"))?;
        let cur = entry
            .reset_token
            .as_ref()
            .ok_or_else(|| anyhow!("invalid reset token"))?;
        if cur.expires_at < Utc::now() {
            return Err(anyhow!("invalid reset token"));
        }
        if cur.token_hash != token_hash {
            return Err(anyhow!("invalid reset token"));
        }
        entry.password_hash = new_hash;
        entry.last_rotated = Utc::now();
        entry.reset_token = None;
        save_locked(&inner)?;
        Ok(())
    }
}

fn save_locked(inner: &Inner) -> Result<()> {
    let mut creds: Vec<Credential> = inner.by_email.values().cloned().collect();
    creds.sort_by(|a, b| a.email.cmp(&b.email));
    let file = CredentialsFile { credential: creds };
    let body = toml::to_string_pretty(&file).context("serialize credentials")?;
    if let Some(parent) = inner.path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = inner.path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp, perms)
            .with_context(|| format!("chmod {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, &inner.path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), inner.path.display()))?;
    Ok(())
}

fn generate_salt() -> SaltString {
    use rand::RngExt;
    let mut bytes = [0u8; 16];
    rand::rng().fill(&mut bytes[..]);
    SaltString::encode_b64(&bytes).expect("salt encode")
}

fn random_token(len: usize) -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ\
abcdefghijkmnopqrstuvwxyz23456789"; // ambiguous chars dropped
    let mut rng = rand::rng();
    (0..len)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

// --------------------------------------------------------------------
// Bootstrap-email lookup — resolves an authenticated email to the
// Employee row's id/role/scope by hitting boss-people's
// `/api/people/bootstrap-email` endpoint.
// --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BootstrapScope {
    id: String,
    role: String,
    department: Option<String>,
    #[serde(default)]
    territory_account_ids: Vec<String>,
    #[serde(default)]
    direct_report_ids: Vec<String>,
}

async fn bootstrap_email(http: &reqwest::Client, email: &str) -> Option<BootstrapScope> {
    let upstream =
        std::env::var("BOSS_PEOPLE_UPSTREAM").unwrap_or_else(|_| boss_ports::url("people"));
    let url = format!(
        "{}/api/people/by-email/{}/bootstrap",
        upstream.trim_end_matches('/'),
        email,
    );
    let resp = http
        .get(&url)
        .header(
            "x-boss-user",
            r#"{"id":"automation:account-provisioning","role":"system","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}"#,
        )
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<BootstrapScope>().await.ok()
}

// --------------------------------------------------------------------
// HTTP state.
// --------------------------------------------------------------------

#[derive(Clone)]
pub struct LocalAuthState {
    pub store: CredentialStore,
    pub session_key: Vec<u8>,
    pub http: reqwest::Client,
}

// --------------------------------------------------------------------
// HTTP handlers.
// --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub email: String,
    pub employee_id: Option<String>,
    pub role: Option<String>,
    pub access_tier: String,
}

/// `POST /api/auth/login` — verify credentials, look up scope,
/// mint the boss_session cookie. Returns the resolved identity.
pub async fn login(
    State(state): State<Arc<LocalAuthState>>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if let Err(e) = state.store.verify(&req.email, &req.password) {
        return (StatusCode::UNAUTHORIZED, format!("{e}")).into_response();
    }
    let email = req.email.to_lowercase();

    let mut sess = Session::new(&email, session::DEFAULT_TTL_SECONDS);
    // The platform-admin Employee is
    // provisioned by system initialization (boss-operator-baseline-
    // seed at bootstrap time) — the gateway never auto-creates one
    // on first login, because a running system with no platform-
    // admin can't perform a privileged action if one is needed in
    // the gap before the first human shows up. The single direct
    // write the system needs to bootstrap itself is the seed
    // binary's hire-insertion, not the gateway's login handler.
    //
    // If bootstrap_email returns None here, that means the
    // credential is valid but no Employee row matches — either
    // bootstrap didn't run, or BOSS_BOOTSTRAP_ADMIN_EMAIL wasn't
    // set when it did. Surface as 403 with the operator-facing
    // remediation path. Never silently downgrade to
    // audit-readonly, which was an earlier footgun.
    let scope = match bootstrap_email(&state.http, &email).await {
        Some(s) => s,
        None => {
            return (
                StatusCode::FORBIDDEN,
                format!(
                    "credential verified but no Employee record matches {email}. \
                     System initialization should have provisioned this row via \
                     boss-operator-baseline-seed with BOSS_BOOTSTRAP_ADMIN_EMAIL \
                     set to the operator email. Either rerun the bootstrap or \
                     POST /api/people manually to create the row."
                ),
            )
                .into_response();
        }
    };
    sess.employee_id = Some(scope.id);
    sess.role = Some(scope.role);
    sess.department = scope.department;
    sess.territory_account_ids = scope.territory_account_ids;
    sess.direct_report_ids = scope.direct_report_ids;

    let cookie_value = sess.encode(&state.session_key);
    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        session::DEFAULT_TTL_SECONDS,
        "/",
    );
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&set_cookie) {
        headers.insert(header::SET_COOKIE, v);
    }

    (
        StatusCode::OK,
        headers,
        Json(MeResponse {
            email,
            employee_id: sess.employee_id.clone(),
            role: sess.role.clone(),
            access_tier: sess.access_tier.clone(),
        }),
    )
        .into_response()
}

/// `POST /api/auth/logout` — clear the boss_session cookie.
pub async fn logout() -> Response {
    let cookie = session::set_cookie(session::COOKIE_NAME, "", 0, "/");
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    (StatusCode::NO_CONTENT, headers).into_response()
}

/// `GET /api/auth/me` — decode the cookie + return the identity
/// of the currently-logged-in BOSS user. 401 if no cookie, the
/// signature is invalid, OR the session is the demo-mode
/// audit-readonly mint (no employee_id, role=audit-readonly).
///
/// The demo-session 401 is what lets the LoginPage form actually
/// render: without it, demo mode auto-mints an anonymous session
/// for the GET, /me returns 200, and the SPA redirects to home
/// instead of showing the login form. The semantic is "are you
/// logged in via BOSS local-auth credentials?" — a demo session
/// is anonymous, so the answer is no.
pub async fn me(State(state): State<Arc<LocalAuthState>>, headers: HeaderMap) -> Response {
    let session = match extract_session(&headers, &state.session_key) {
        Some(s) => s,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };
    // Reject demo sessions: no employee_id + role=audit-readonly
    // is the signature of the demo-mode synthetic session. A real
    // local-auth login populates employee_id on the session.
    if session.employee_id.is_none() && session.role.as_deref() == Some("audit-readonly") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(MeResponse {
        email: session.username.clone(),
        employee_id: session.employee_id.clone(),
        role: session.role.clone(),
        access_tier: session.access_tier.clone(),
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct OnboardRequest {
    pub email: String,
    pub password: String,
}

/// `POST /api/auth/onboard` — admin-only. Creates a credential
/// row for an existing Employee email. Verified via the caller's
/// role (must be platform-admin / ceo / coo).
pub async fn onboard(
    State(state): State<Arc<LocalAuthState>>,
    headers: HeaderMap,
    Json(req): Json<OnboardRequest>,
) -> Response {
    if !is_admin(&headers, &state.session_key) {
        return (StatusCode::FORBIDDEN, "admin only").into_response();
    }
    if let Err(e) = state.store.upsert(&req.email, &req.password) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response();
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"email": req.email})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct IssueResetRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct IssueResetResponse {
    /// Plaintext token. Admin shares with user out-of-band; never
    /// persisted in the credential store.
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// `POST /api/auth/issue-reset` — admin-only. Returns a one-time
/// reset token to share with the user.
pub async fn issue_reset(
    State(state): State<Arc<LocalAuthState>>,
    headers: HeaderMap,
    Json(req): Json<IssueResetRequest>,
) -> Response {
    if !is_admin(&headers, &state.session_key) {
        return (StatusCode::FORBIDDEN, "admin only").into_response();
    }
    let ttl = 60 * 60; // 1h
    match state.store.issue_reset_token(&req.email, ttl) {
        Ok(token) => Json(IssueResetResponse {
            token,
            expires_at: Utc::now() + chrono::Duration::seconds(ttl),
        })
        .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    }
}

#[derive(Deserialize)]
pub struct ResetRequest {
    pub email: String,
    pub token: String,
    pub password: String,
}

/// `POST /api/auth/reset` — public. User consumes the token an
/// admin shared with them, sets a new password.
pub async fn reset(
    State(state): State<Arc<LocalAuthState>>,
    Json(req): Json<ResetRequest>,
) -> Response {
    if let Err(e) = state
        .store
        .consume_reset_token(&req.email, &req.token, &req.password)
    {
        return (StatusCode::UNAUTHORIZED, format!("{e}")).into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

// --------------------------------------------------------------------
// Helpers.
// --------------------------------------------------------------------

fn extract_session(headers: &HeaderMap, key: &[u8]) -> Option<Session> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    let raw = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
    Session::decode(raw, key).ok()
}

fn is_admin(headers: &HeaderMap, key: &[u8]) -> bool {
    let Some(s) = extract_session(headers, key) else {
        return false;
    };
    s.role
        .as_deref()
        .map(boss_core::roles::has_global_read)
        .unwrap_or(false)
}

// --------------------------------------------------------------------
// Tests.
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, CredentialStore) {
        let td = TempDir::new().unwrap();
        let path = td.path().join("credentials.toml");
        let store = CredentialStore::load(path).unwrap();
        (td, store)
    }

    #[test]
    fn upsert_then_verify_round_trip() {
        let (_td, store) = temp_store();
        store
            .upsert("op@example.com", "correct horse battery")
            .unwrap();
        assert!(
            store
                .verify("op@example.com", "correct horse battery")
                .is_ok()
        );
        assert!(store.verify("op@example.com", "wrong").is_err());
        assert!(store.verify("missing@example.com", "anything").is_err());
    }

    #[test]
    fn email_lookup_is_case_insensitive() {
        let (_td, store) = temp_store();
        store.upsert("Alice@Example.com", "pw").unwrap();
        assert!(store.verify("alice@example.com", "pw").is_ok());
        assert!(store.verify("ALICE@EXAMPLE.COM", "pw").is_ok());
    }

    #[test]
    fn reload_persists_across_instances() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("credentials.toml");
        let s1 = CredentialStore::load(&path).unwrap();
        s1.upsert("op@example.com", "pw").unwrap();
        let s2 = CredentialStore::load(&path).unwrap();
        assert!(s2.verify("op@example.com", "pw").is_ok());
    }

    #[test]
    fn remove_drops_the_credential() {
        let (_td, store) = temp_store();
        store.upsert("a@b.com", "pw").unwrap();
        assert!(store.contains("a@b.com"));
        assert!(store.remove("a@b.com").unwrap());
        assert!(!store.contains("a@b.com"));
        assert!(store.verify("a@b.com", "pw").is_err());
    }

    #[test]
    fn reset_token_round_trip() {
        let (_td, store) = temp_store();
        store.upsert("op@example.com", "old-pw").unwrap();
        let token = store.issue_reset_token("op@example.com", 60).unwrap();
        // Old password still works until the reset is consumed.
        assert!(store.verify("op@example.com", "old-pw").is_ok());
        // Wrong token rejected, doesn't burn the token.
        assert!(
            store
                .consume_reset_token("op@example.com", "wrong", "x")
                .is_err()
        );
        // Correct consumption rotates the password.
        store
            .consume_reset_token("op@example.com", &token, "new-pw")
            .unwrap();
        assert!(store.verify("op@example.com", "new-pw").is_ok());
        assert!(store.verify("op@example.com", "old-pw").is_err());
        // Token can't be reused.
        assert!(
            store
                .consume_reset_token("op@example.com", &token, "yet-pw")
                .is_err()
        );
    }

    #[test]
    fn reset_token_for_unknown_email_fails_cleanly() {
        let (_td, store) = temp_store();
        assert!(store.issue_reset_token("ghost@example.com", 60).is_err());
        assert!(
            store
                .consume_reset_token("ghost@example.com", "x", "y")
                .is_err()
        );
    }
}
