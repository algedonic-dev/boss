//! HMAC-signed session cookies.
//!
//! Wire format: `base64url(payload_json).base64url(hmac_sha256(payload_json, key))`
//!
//! Payload carries the authenticated username and an absolute expiry.
//! Verification is constant-time and rejects expired tokens.
//! The session key is a random 32-byte value loaded from disk at startup.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "boss_session";
// 24 hours (David, 2026-08-13, filed from the front door itself:
// "TTL on sign-in auth is too short... I think it should be 24 hrs
// for now"). Scope staleness (territory/reports baked at login)
// is now bounded by a day instead of a workday.
pub const DEFAULT_TTL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    /// Authenticated username.
    #[serde(rename = "u")]
    pub username: String,
    /// Absolute expiry, seconds since epoch.
    #[serde(rename = "e")]
    pub expiry: u64,
    /// Boss role (e.g., "cto", "service-tech"). None for unknown users.
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Boss employee ID. None for unknown users.
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    /// Access tier: "operator" (full system) or "user" (frontend only).
    /// Every login mints "user" — OIDC, password, guest and break-glass
    /// alike. PRIVATE, with the one writer of "operator" beside it
    /// ([`Session::into_operator`]), whose one caller is
    /// `crate::elevation::elevate`, reached only after a verified
    /// WebAuthn assertion from the platform owner's operator-tier
    /// passkey (backlog 3c92c5b8; until then this comment promised an
    /// elevation nothing did). Read it through [`Session::access_tier`],
    /// which says "operator" only when `elevated_at` is set too.
    #[serde(rename = "t", default = "default_tier")]
    access_tier: String,
    /// When the passkey assertion that elevated this session was
    /// verified, seconds since epoch. `None` on every session that was
    /// never elevated; serialised only when set. The elevation lives no
    /// longer than the cookie it rides: `expiry` is not extended.
    #[serde(rename = "ea", default, skip_serializing_if = "Option::is_none")]
    elevated_at: Option<u64>,
    /// Department for the authenticated employee (e.g. "executive").
    /// None for unknown users; serialised only when populated. Fed into
    /// `x-boss-user` so Department-scoped policy rules can match.
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Accounts the employee is accountable for — union of territory
    /// rep + account-team membership. Captured at login from
    /// `GET /api/people/{id}/scope`; serialised only when non-empty
    /// (empty covers every unrecognized user without cookie bloat).
    /// Staleness bounded by the 24h session TTL — a newly-assigned rep
    /// picks up their territory at next login.
    #[serde(rename = "tp", default, skip_serializing_if = "Vec::is_empty")]
    pub territory_account_ids: Vec<String>,
    /// Employees who report directly to this session's user. Captured
    /// alongside territory; same staleness bound.
    #[serde(rename = "dr", default, skip_serializing_if = "Vec::is_empty")]
    pub direct_report_ids: Vec<String>,
}

/// The two tiers a session can carry.
pub const USER_TIER: &str = "user";
pub const OPERATOR_TIER: &str = "operator";

fn default_tier() -> String {
    USER_TIER.to_string()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("malformed cookie")]
    Malformed,
    #[error("signature mismatch")]
    BadSignature,
    #[error("session expired")]
    Expired,
}

impl Session {
    pub fn new(username: impl Into<String>, ttl_seconds: u64) -> Self {
        Self {
            username: username.into(),
            expiry: now() + ttl_seconds,
            role: None,
            employee_id: None,
            access_tier: USER_TIER.to_string(),
            elevated_at: None,
            department: None,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
        }
    }

    /// The role this session acts as. A session with no role acts as a
    /// `visitor` — the least access, and on the read-only floor —
    /// through `boss_core::roles::effective_role`, the one fallback,
    /// because two readers here must agree on it: the role-header layer
    /// that tells every service who is calling, and the proxy that
    /// refuses a read-only session's writes before any service is
    /// called (backlog 07e797b4). Were they to disagree, a roleless
    /// session would be read-only downstream and a writer at the edge.
    /// It was `audit-readonly`, the widest read, until design 2830b6b7.
    pub fn effective_role(&self) -> &str {
        boss_core::roles::effective_role(self.role.as_deref())
    }

    /// The id this session acts as downstream: the signed `employee_id`
    /// when it has one, otherwise the username — a guest or break-glass
    /// session has no employee id by design. ONE definition, because two
    /// readers must agree on it: the role-header layer that writes it into
    /// `x-boss-user`, and `/api/session`, which hands it to the SPA so the
    /// web can ask policy about itself (`POST /api/policy/check`, whose
    /// self-arm admits only this id with this role — backlog 9dad102c).
    pub fn policy_id(&self) -> &str {
        self.employee_id.as_deref().unwrap_or(&self.username)
    }

    /// The tier every reader forwards: "operator" ONLY for a session
    /// that was elevated — the stored tier AND the elevation's instant —
    /// and "user" for everything else, whatever a cookie's `t` says. The
    /// two fields are written together by [`Session::into_operator`]
    /// alone, so the conjunction is belt and braces, not a second rule
    /// (adversarial review of car 0bde9b99, M2).
    pub fn access_tier(&self) -> &'static str {
        if self.access_tier == OPERATOR_TIER && self.elevated_at.is_some() {
            OPERATOR_TIER
        } else {
            USER_TIER
        }
    }

    /// When this session was elevated, if it was.
    pub fn elevated_at(&self) -> Option<u64> {
        self.elevated_at
    }

    /// THE ONE WRITER OF THE OPERATOR TIER: this session, elevated by an
    /// assertion verified at `asserted_at`, its expiry unchanged. Crate-
    /// private, and its one caller is `crate::elevation::elevate`, which
    /// decides whether it may be called — pinned there by a source scan
    /// of this function's call sites.
    pub(crate) fn into_operator(self, asserted_at: u64) -> Session {
        Session {
            access_tier: OPERATOR_TIER.to_string(),
            elevated_at: Some(asserted_at),
            ..self
        }
    }

    /// Encode and sign into a cookie value.
    pub fn encode(&self, key: &[u8]) -> String {
        let payload = serde_json::to_vec(self).expect("serialize Session");
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(payload_b64.as_bytes());
        let sig = mac.finalize().into_bytes();
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig);
        format!("{payload_b64}.{sig_b64}")
    }

    /// Verify signature and expiry, returning the decoded session.
    pub fn decode(cookie_value: &str, key: &[u8]) -> Result<Self, SessionError> {
        let (payload_b64, sig_b64) = cookie_value
            .split_once('.')
            .ok_or(SessionError::Malformed)?;
        let sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| SessionError::Malformed)?;
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(payload_b64.as_bytes());
        let expected = mac.finalize().into_bytes();
        if expected.ct_eq(&sig).unwrap_u8() != 1 {
            return Err(SessionError::BadSignature);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| SessionError::Malformed)?;
        let session: Session =
            serde_json::from_slice(&payload).map_err(|_| SessionError::Malformed)?;
        if session.expiry <= now() {
            return Err(SessionError::Expired);
        }
        Ok(session)
    }
}

/// Build a `Set-Cookie` header value with hardened flags.
///
/// `max_age` is the cookie's Max-Age in seconds. `path` scopes the cookie
/// (e.g., "/" for full-site).
pub fn set_cookie(name: &str, value: &str, max_age: u64, path: &str) -> String {
    format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax")
}

/// Extract a cookie value by name from a `Cookie:` header.
pub fn find_cookie<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    cookie_header.split(';').find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name).then_some(v)
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8; 32] = b"test-key-0123456789abcdef0123456";

    #[test]
    fn round_trip_encode_decode() {
        let s = Session::new("alice", 3600);
        let cookie = s.encode(KEY);
        let decoded = Session::decode(&cookie, KEY).unwrap();
        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.expiry, s.expiry);
    }

    /// Review M2: the tier every reader forwards is "operator" only for
    /// a session carrying the elevation's instant as well — a payload
    /// that says `t: operator` alone (none is minted so, and the HMAC
    /// guards the cookie, so this is belt and braces) reads as "user".
    #[test]
    fn a_stored_operator_tier_without_an_elevation_reads_user() {
        let bare: Session =
            serde_json::from_str(r#"{"u":"x","e":9999999999,"t":"operator"}"#).unwrap();
        assert_eq!(bare.access_tier(), USER_TIER);
        let elevated: Session =
            serde_json::from_str(r#"{"u":"x","e":9999999999,"t":"operator","ea":1}"#).unwrap();
        assert_eq!(elevated.access_tier(), OPERATOR_TIER);
        assert_eq!(Session::new("alice", 60).access_tier(), USER_TIER);
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let s = Session::new("alice", 3600);
        let cookie = s.encode(KEY);
        let (_, sig) = cookie.split_once('.').unwrap();
        let bad_payload = URL_SAFE_NO_PAD.encode(br#"{"u":"attacker","e":9999999999}"#);
        let forged = format!("{bad_payload}.{sig}");
        assert_eq!(
            Session::decode(&forged, KEY),
            Err(SessionError::BadSignature)
        );
    }

    #[test]
    fn wrong_key_is_rejected() {
        let s = Session::new("alice", 3600);
        let cookie = s.encode(KEY);
        assert_eq!(
            Session::decode(&cookie, b"different-key-0123456789abcdef01"),
            Err(SessionError::BadSignature)
        );
    }

    #[test]
    fn expired_session_is_rejected() {
        // TTL of zero: expiry == now, so the <= now check fails.
        let s = Session::new("alice", 0);
        let cookie = s.encode(KEY);
        assert_eq!(Session::decode(&cookie, KEY), Err(SessionError::Expired));
    }

    #[test]
    fn malformed_cookie_is_rejected() {
        assert_eq!(Session::decode("no-dot", KEY), Err(SessionError::Malformed));
        assert_eq!(
            Session::decode("bad_b64.bad_b64", KEY),
            Err(SessionError::BadSignature)
        );
    }

    #[test]
    fn find_cookie_extracts_single_value() {
        assert_eq!(find_cookie("boss_session=abc", "boss_session"), Some("abc"));
    }

    #[test]
    fn find_cookie_extracts_from_multiple() {
        assert_eq!(
            find_cookie("foo=bar; boss_session=abc; baz=qux", "boss_session"),
            Some("abc")
        );
    }

    #[test]
    fn find_cookie_returns_none_when_absent() {
        assert_eq!(find_cookie("foo=bar; baz=qux", "boss_session"), None);
    }

    #[test]
    fn set_cookie_has_security_flags() {
        let c = set_cookie("boss_session", "value", 3600, "/");
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("Secure"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Path=/"));
        assert!(c.contains("Max-Age=3600"));
    }
}
