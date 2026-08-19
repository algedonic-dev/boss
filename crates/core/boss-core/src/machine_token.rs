//! The machine door's shared secret (feedback 7fcd78fa, phase 1).
//!
//! The jobs API's machine door reads identity from the caller-supplied
//! `x-boss-user` header and trusts it verbatim — measured from a
//! laptop with a made-up header, any host that can route to the port
//! may declare itself any role at any tier. David accepted the phased
//! close: a static token required on WRITES when configured (this),
//! reads join once every legitimate caller carries it, mTLS only if
//! the fabric ever grows callers outside the WireGuard net.
//!
//! One definition for both sides. Writers attach via [`attach`] (or
//! get it for free through `http_client::base`); the jobs API enforces
//! via [`verify`]. The env var is read at process start by callers —
//! the token value is administered by David and never appears in
//! repo, transcript, or test fixture.
//!
//! DEPLOY-ORDER SAFETY: everything is inert until the env var is set.
//! Enforcement with no token configured admits everything; attachment
//! with no token configured adds no header. So the code lands on every
//! writer and the door, and the token turning on is a pure ops action
//! — set it everywhere, restart, done; unset it to roll back.

/// Header the token rides in. Lowercase because axum/reqwest header
/// names are; the gateway's edge strip covers it via the `x-boss-`
/// prefix rule, so a browser can never smuggle one through.
pub const HEADER: &str = "x-boss-machine-token";

/// The env var both sides read. One name, so an activation runbook is
/// "set this everywhere" rather than a mapping table.
pub const ENV: &str = "BOSS_MACHINE_TOKEN";

/// The configured token, if any. Whitespace-trimmed; an empty or
/// blank value reads as unconfigured rather than as a token every
/// empty header would match.
pub fn from_env() -> Option<String> {
    std::env::var(ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Insert the token header when the process has one configured.
/// Callers hand this their client's `default_headers` map so every
/// request the client ever makes carries the token — attaching
/// per-request is how one call site gets missed.
pub fn attach(headers: &mut reqwest::header::HeaderMap) {
    if let Some(token) = from_env()
        && let Ok(v) = reqwest::header::HeaderValue::from_str(&token)
    {
        headers.insert(HEADER, v);
    }
}

/// Does the provided header value match the expected token?
///
/// Byte-wise constant-time over the compared length: the accumulator
/// folds every byte rather than returning at the first mismatch, so
/// response timing does not leak a prefix. The length check short-
/// circuits, which leaks only the token's length — acceptable for a
/// high-entropy random value.
pub fn verify(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_requires_exact_match() {
        assert!(verify("s3cret", Some("s3cret")));
        assert!(!verify("s3cret", Some("s3creT")));
        assert!(!verify("s3cret", Some("s3cre")));
        assert!(!verify("s3cret", Some("")));
        assert!(!verify("s3cret", None));
    }

    #[test]
    fn attach_without_env_adds_nothing() {
        // The test process has no BOSS_MACHINE_TOKEN (the value is
        // David-administered and never appears in fixtures), so this
        // doubles as the deploy-order-safety check: unconfigured means
        // inert.
        assert!(from_env().is_none());
        let mut h = reqwest::header::HeaderMap::new();
        attach(&mut h);
        assert!(!h.contains_key(HEADER));
    }
}
