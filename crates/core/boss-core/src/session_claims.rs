//! The gateway session's shared wire and verifier (f623e425, oldest6c).
//! A wire value is untrusted data. Only HMAC verification creates verified
//! claims; issuance and operator elevation remain private to the gateway.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const COOKIE_NAME: &str = "boss_session";

/// Serialization data, never evidence that a session was authenticated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionWire {
    #[serde(rename = "u")]
    pub username: String,
    #[serde(rename = "e")]
    pub expiry: u64,
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub employee_id: Option<String>,
    #[serde(rename = "t", default = "user_tier")]
    pub access_tier: String,
    #[serde(rename = "ea", default, skip_serializing_if = "Option::is_none")]
    pub elevated_at: Option<u64>,
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    #[serde(rename = "tp", default, skip_serializing_if = "Vec::is_empty")]
    pub territory_account_ids: Vec<String>,
    #[serde(rename = "dr", default, skip_serializing_if = "Vec::is_empty")]
    pub direct_report_ids: Vec<String>,
}
fn user_tier() -> String {
    "user".into()
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

/// No Deserialize, public field, or constructor from a wire payload.
#[derive(Debug, Clone)]
pub struct VerifiedSession(SessionWire);
impl VerifiedSession {
    pub fn claims(&self) -> &SessionWire {
        &self.0
    }
    pub fn policy_id(&self) -> &str {
        self.0.employee_id.as_deref().unwrap_or(&self.0.username)
    }
    pub fn effective_role(&self) -> &str {
        crate::roles::effective_role(self.0.role.as_deref())
    }
    pub fn access_tier(&self) -> &'static str {
        if self.0.access_tier == "operator" && self.0.elevated_at.is_some() {
            "operator"
        } else {
            "user"
        }
    }
}

/// Preserve the deployed gateway signature: HMAC covers base64url payload.
pub fn verify(value: &str, key: &[u8], at: u64) -> Result<VerifiedSession, SessionError> {
    let (payload, signature) = value.split_once('.').ok_or(SessionError::Malformed)?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| SessionError::Malformed)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| SessionError::Malformed)?;
    mac.update(payload.as_bytes());
    if mac.finalize().into_bytes().ct_eq(&signature).unwrap_u8() != 1 {
        return Err(SessionError::BadSignature);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| SessionError::Malformed)?;
    let wire: SessionWire = serde_json::from_slice(&bytes).map_err(|_| SessionError::Malformed)?;
    if wire.expiry <= at {
        return Err(SessionError::Expired);
    }
    Ok(VerifiedSession(wire))
}

/// A signer request names exactly one session across all Cookie headers.
/// Selecting the first of duplicate cookies makes identity order-dependent.
pub fn unique_cookie<'a>(
    headers: impl IntoIterator<Item = &'a str>,
) -> Result<Option<&'a str>, SessionError> {
    let mut value = None;
    for header in headers {
        for pair in header.split(';') {
            if pair.trim() == COOKIE_NAME {
                return Err(SessionError::Malformed);
            }
            if let Some((name, candidate)) = pair.trim().split_once('=')
                && name == COOKIE_NAME
            {
                if value.is_some() || candidate.is_empty() {
                    return Err(SessionError::Malformed);
                }
                value = Some(candidate);
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_bare_named_session_never_selects_another_cookie() {
        for headers in [
            vec!["boss_session; boss_session=valid"],
            vec!["boss_session=valid; boss_session"],
            vec!["boss_session", "boss_session=valid"],
            vec!["boss_session=valid", "boss_session"],
        ] {
            assert_eq!(unique_cookie(headers), Err(SessionError::Malformed));
        }
        assert_eq!(
            unique_cookie(["unrelated; boss_session=valid"]),
            Ok(Some("valid"))
        );
    }
}
