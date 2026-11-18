//! The presence ticket — ONE definition for the side that mints it and
//! every side that grants on it.
//!
//! The gateway's passkey ceremony (docs/design/presence.md) ends in a
//! short-lived HMAC ticket: a verified WebAuthn assertion, bound to one
//! step's id and shape hash, one employee and one single-use nonce. The
//! jobs API grants `Assurance::Presence` on it.
//!
//! WHY THIS LIVES IN boss-core (backlog 72fe3640, 2026-09-24). The type
//! used to live in boss-gateway, the only place its HMAC was checked;
//! the gateway then re-injected the ticket's fields as PLAIN JSON in
//! `x-boss-presence`, and the jobs API granted presence on that JSON.
//! The gateway strips every inbound `x-boss-*` header, so through the
//! front door that held — but the jobs API's machine door (:7900)
//! trusts headers verbatim, and every machine-token holder (every
//! agent's `boss-api`, the conductor, the dispatcher, the ops runner)
//! could write the JSON by hand and stamp a passkey's presence on any
//! step as any person. So the header now carries the SIGNED ticket
//! itself, and the jobs API verifies it with [`PresenceTicket::decode`]
//! — this function, not a copy of it, so the check at the grant can
//! never be weaker than the check at the mint (CLAUDE.md §9a).
//!
//! The key is the gateway's session key. Both processes run in the one
//! `boss` container and read the same read-only Secret mount, named by
//! [`SESSION_KEY_ENV`]; [`parse_session_key`] is the one reading of that
//! file's format.
//!
//! A TICKET IS SIGNED WITH ITS OWN KEY (backlog 19f00800, design
//! c22f36c6, decided 2026-09-26). The session cookie is the same
//! construction — `b64(json).b64(hmac(key, b64(json)))` — and both were
//! keyed by the session key itself, so a cookie failed as a ticket only
//! because its JSON lacked `s h n`: a session payload that also carried
//! them, signed by the session codec, verified as presence. Tickets are
//! now MAC'd under [`derived_key`]`(session key,`
//! [`PRESENCE_TICKET_KEY_LABEL`]`)`, so no cookie verifies as a ticket
//! and no ticket as a cookie, whatever their fields say. The session
//! cookie stays on the root key, so nobody is signed out; the version
//! lives in the label rather than on the wire. [`PresenceTicket::encode`]
//! and [`PresenceTicket::decode`] still take the SESSION key and derive
//! inside, so no caller can sign a ticket with the root key by mistake.
//!
//! WHAT STILL RESTS ON THE ONE CONTAINER. Every service runs as one uid
//! in the one `boss` container with the key file mounted, so any process
//! in it can read the session key and mint any gateway session, operator
//! tier included — a taken-over jobs API among them. Nothing here
//! changes that; the container is the boundary. The derived key is what
//! would make a split effective for presence: a jobs API handed only the
//! ticket key cannot run a one-way derivation backwards. It is not
//! enough on its own today, because the jobs API also verifies gateway
//! SESSIONS for signer writes (`boss-jobs` `http/signer.rs`, f623e425)
//! with the root key, and an HMAC verifier is a signer.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// The header the jobs API reads a presence claim from. The gateway
/// writes it only after verifying a ticket, and its VALUE is that
/// signed ticket — so the jobs API can, and does, verify it again.
pub const HEADER: &str = "x-boss-presence";

/// The env var naming the session-key FILE (a path, not a key — see
/// infra/lint/session-key-persists.sh for what confusing the two cost).
pub const SESSION_KEY_ENV: &str = "BOSS_SESSION_KEY";

/// Where the key file is when [`SESSION_KEY_ENV`] is unset.
pub const DEFAULT_SESSION_KEY_PATH: &str = "/var/lib/boss-gateway/session.key";

/// The shortest key the file may hold, in bytes.
pub const MIN_SESSION_KEY_BYTES: usize = 32;

/// The derivation label of the key presence tickets are MAC'd with.
/// Changing it (to `v2`) invalidates every ticket in flight, and nothing
/// else — the reason the version lives here and not on the wire.
pub const PRESENCE_TICKET_KEY_LABEL: &str = "boss presence-ticket v1";

/// The derivation label of the key the gateway's OIDC sign-in state
/// cookie is MAC'd with (design c22f36c6 item 2: the third format the
/// session key signed). Every label is spelled here, beside the other,
/// so two purposes cannot pick the same one unseen.
pub const OIDC_STATE_KEY_LABEL: &str = "boss oidc-state v1";

/// HMAC-SHA256 over the concatenation of `parts` under `key`.
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Option<Vec<u8>> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).ok()?;
    parts.iter().for_each(|p| mac.update(p));
    Some(mac.finalize().into_bytes().to_vec())
}

/// HKDF-SHA256 (RFC 5869): extract a pseudorandom key from `ikm` under
/// `salt` (empty means HashLen zeros), then expand `info` to `len` bytes.
/// `None` past the RFC's bound of 255 blocks. Written on the `hmac` this
/// crate already uses, pinned by the RFC's own vectors, because the
/// `hkdf` crate in Cargo.lock is on the older `digest` line and would
/// pull a second `sha2` into boss-core (design c22f36c6 item 3).
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Option<Vec<u8>> {
    const HASH_LEN: usize = 32;
    let blocks = u8::try_from(len.div_ceil(HASH_LEN)).ok()?;
    let salt = if salt.is_empty() {
        &[0u8; HASH_LEN][..]
    } else {
        salt
    };
    let prk = hmac_sha256(salt, &[ikm])?;
    let (mut okm, _) = (1..=blocks).try_fold((Vec::new(), Vec::new()), |(okm, prev), i| {
        let t = hmac_sha256(&prk, &[&prev, info, &[i]])?;
        Some(([okm, t.clone()].concat(), t))
    })?;
    okm.truncate(len);
    Some(okm)
}

/// The 32-byte key for one PURPOSE, derived one-way from the session key:
/// `HKDF-SHA256(salt = none, ikm = session key, info = label)`. Holding it
/// does not give the session key back, and a MAC under it verifies under
/// no other purpose's key.
pub fn derived_key(session_key: &[u8], label: &str) -> Option<Vec<u8>> {
    hkdf_sha256(&[], session_key, label.as_bytes(), 32)
}

/// The key file's path, as every reader resolves it.
pub fn session_key_path() -> PathBuf {
    std::env::var(SESSION_KEY_ENV)
        .unwrap_or_else(|_| DEFAULT_SESSION_KEY_PATH.into())
        .into()
}

/// Why a key file's contents are not a key.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SessionKeyError {
    #[error("session key file is not valid hex")]
    NotHex,
    #[error("session key must be at least {MIN_SESSION_KEY_BYTES} bytes")]
    TooShort,
}

/// The key file's format, read once for every reader: hex, surrounding
/// whitespace ignored, at least [`MIN_SESSION_KEY_BYTES`] bytes.
pub fn parse_session_key(text: &str) -> Result<Vec<u8>, SessionKeyError> {
    let bytes = hex::decode(text.trim()).map_err(|_| SessionKeyError::NotHex)?;
    if bytes.len() < MIN_SESSION_KEY_BYTES {
        return Err(SessionKeyError::TooShort);
    }
    Ok(bytes)
}

/// Seconds since the epoch, by the wall clock — the clock the ticket's
/// expiry is minted against, never the simulation clock.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The fingerprint of a stored WebAuthn public key: lowercase hex of
/// SHA-256 over the exact bytes `webauthn_credentials.public_key` holds.
/// A `passkey-promotion` packet carries it in the step the owner signs,
/// and boss-people's promote compares it with the row it flips, so a key
/// deleted and re-registered under the same credential id with other key
/// material promotes nothing (review of car b3f6f5b4, 2026-09-28). One
/// definition, because the gateway that files the packet and the people
/// service that checks it must compute the same thing (CLAUDE.md §9a).
pub fn public_key_fingerprint(public_key: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(Sha256::digest(public_key))
}

/// A verified assertion, portable for as long as its `e` allows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresenceTicket {
    /// employee id the assertion verified for
    pub i: String,
    /// step id the challenge was minted for
    pub s: String,
    /// shape hash at mint time
    pub h: String,
    /// server nonce — recorded on the stamp for single-use audit
    pub n: String,
    /// absolute expiry, seconds since epoch
    pub e: u64,
}

/// HMAC-SHA256 of `payload_b64` under `key`. Crate-visible so the
/// passkey-promotion ticket (`crate::passkey_promotion`) is signed with
/// the same primitive under a domain prefix of its own.
pub(crate) fn mac(key: &[u8], payload_b64: &str) -> Option<Vec<u8>> {
    hmac_sha256(key, &[payload_b64.as_bytes()])
}

/// The key a ticket is MAC'd with, from the session key.
fn ticket_key(session_key: &[u8]) -> Option<Vec<u8>> {
    derived_key(session_key, PRESENCE_TICKET_KEY_LABEL)
}

impl PresenceTicket {
    /// `<base64url(json)>.<base64url(hmac_sha256(ticket key, base64url(json)))>`,
    /// the ticket key derived from `session_key` (module doc). `None` only
    /// if the ticket cannot be serialized or keyed, which HMAC-SHA256 over
    /// a plain struct never is.
    pub fn encode(&self, session_key: &[u8]) -> Option<String> {
        let payload = serde_json::to_vec(self).ok()?;
        let payload_b64 = URL_SAFE_NO_PAD.encode(&payload);
        let sig_b64 = URL_SAFE_NO_PAD.encode(mac(&ticket_key(session_key)?, &payload_b64)?);
        Some(format!("{payload_b64}.{sig_b64}"))
    }

    /// The ticket, iff `value` was signed with the ticket key derived from
    /// `session_key` and is unexpired at `now_epoch`. The signature is
    /// compared in constant time and checked BEFORE the payload is parsed,
    /// so nothing an unsigned value says is ever read. A value MAC'd with
    /// the session key itself — every session cookie, and every ticket
    /// minted before backlog 19f00800 — is refused here.
    pub fn decode(value: &str, session_key: &[u8], now_epoch: u64) -> Option<Self> {
        let (payload_b64, sig_b64) = value.split_once('.')?;
        let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
        let expected = mac(&ticket_key(session_key)?, payload_b64)?;
        if expected.ct_eq(&sig).unwrap_u8() != 1 {
            return None;
        }
        let ticket: PresenceTicket =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
        (ticket.e > now_epoch).then_some(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test-session-key";

    /// The published SHA-256 of "abc" (FIPS 180-2), so the fingerprint is
    /// the plain digest of the stored bytes and nothing else.
    #[test]
    fn a_public_key_fingerprint_is_the_sha256_of_its_bytes() {
        assert_eq!(
            public_key_fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn ticket(e: u64) -> PresenceTicket {
        PresenceTicket {
            i: "emp-1".into(),
            s: "step-1".into(),
            h: "hash".into(),
            n: "nonce".into(),
            e,
        }
    }

    #[test]
    fn ticket_round_trips_and_expires() {
        let t = ticket(now_epoch() + 60);
        let enc = t.encode(KEY).unwrap();
        let dec = PresenceTicket::decode(&enc, KEY, now_epoch()).expect("valid ticket decodes");
        assert_eq!(dec, t);
        // Expired by clock: decode refuses.
        assert!(PresenceTicket::decode(&enc, KEY, t.e + 1).is_none());
        // Wrong key: decode refuses.
        assert!(PresenceTicket::decode(&enc, b"other-key", now_epoch()).is_none());
        // Tampered payload: decode refuses.
        let mut forged = enc.clone();
        forged.replace_range(0..1, if enc.starts_with('A') { "B" } else { "A" });
        assert!(PresenceTicket::decode(&forged, KEY, now_epoch()).is_none());
    }

    /// The shape the jobs API used to accept (backlog 72fe3640): the
    /// ticket's fields as plain JSON, no signature. It must not decode,
    /// whatever it says.
    #[test]
    fn plain_json_is_not_a_ticket() {
        let json = serde_json::json!({
            "employee_id": "emp-1", "step_id": "step-1",
            "shape_hash": "hash", "nonce": "nonce",
        })
        .to_string();
        assert!(PresenceTicket::decode(&json, KEY, now_epoch()).is_none());
        // Nor a payload with a made-up signature.
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ticket(u64::MAX)).unwrap());
        let made_up = format!("{payload}.{}", URL_SAFE_NO_PAD.encode([0u8; 32]));
        assert!(PresenceTicket::decode(&made_up, KEY, now_epoch()).is_none());
    }

    fn unhex(s: &str) -> Vec<u8> {
        hex::decode(s).unwrap()
    }

    /// RFC 5869 appendix A.1 and A.3 (SHA-256): the derivation is HKDF and
    /// nothing like it — A.3 is the empty-salt, empty-info form
    /// [`derived_key`] relies on for its salt.
    #[test]
    fn hkdf_is_the_rfc_5869_function() {
        let okm = hkdf_sha256(
            &unhex("000102030405060708090a0b0c"),
            &[0x0b; 22],
            &unhex("f0f1f2f3f4f5f6f7f8f9"),
            42,
        );
        assert_eq!(
            okm,
            Some(unhex(
                "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
            ))
        );
        assert_eq!(
            hkdf_sha256(&[], &[0x0b; 22], &[], 42),
            Some(unhex(
                "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
            ))
        );
        assert_eq!(hkdf_sha256(&[], KEY, &[], 255 * 32 + 1), None);
        assert_eq!(
            hkdf_sha256(&[], KEY, &[], 255 * 32).map(|k| k.len()),
            Some(8160)
        );
    }

    /// Each purpose has its own key, none of them the session key.
    #[test]
    fn every_purpose_derives_its_own_key() {
        assert_ne!(PRESENCE_TICKET_KEY_LABEL, OIDC_STATE_KEY_LABEL);
        let ticket = derived_key(KEY, PRESENCE_TICKET_KEY_LABEL).unwrap();
        let oidc = derived_key(KEY, OIDC_STATE_KEY_LABEL).unwrap();
        assert_eq!(ticket.len(), 32);
        assert_ne!(ticket, oidc);
        assert_ne!(ticket.as_slice(), KEY);
        assert_ne!(
            ticket,
            derived_key(b"other-key", PRESENCE_TICKET_KEY_LABEL).unwrap()
        );
    }

    /// THE CUTOVER, stated as a test (backlog 19f00800). A ticket MAC'd
    /// with the session key itself — every ticket minted before this
    /// change, and every session cookie — is refused. There is no window
    /// accepting the old form: accepting it IS accepting a session cookie
    /// that carries ticket fields, the defect itself, and a ticket lives
    /// 120 seconds, so the deploy costs a ceremony re-run at most.
    #[test]
    fn a_ticket_macd_with_the_bare_session_key_is_refused() {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ticket(u64::MAX)).unwrap());
        let old_form = format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(mac(KEY, &payload).unwrap())
        );
        assert!(PresenceTicket::decode(&old_form, KEY, now_epoch()).is_none());
    }

    /// The reverse hostile case, here because boss-core holds the one
    /// session verifier (`crate::session_claims::verify`): a payload that
    /// carries EVERY session field as well as the ticket's, MAC'd as a
    /// ticket, is no session — refused on its signature.
    #[test]
    fn a_ticket_carrying_session_fields_is_not_a_session() {
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "u": "david", "e": u64::MAX, "i": "emp-david", "t": "operator", "ea": 1,
                "s": "step-1", "h": "hash", "n": "nonce",
            }))
            .unwrap(),
        );
        let sig = mac(&ticket_key(KEY).unwrap(), &payload).unwrap();
        let hostile = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(sig));
        assert!(
            PresenceTicket::decode(&hostile, KEY, now_epoch()).is_some(),
            "the fixture is a genuine ticket"
        );
        assert_eq!(
            crate::session_claims::verify(&hostile, KEY, now_epoch()).err(),
            Some(crate::session_claims::SessionError::BadSignature)
        );
    }

    #[test]
    fn the_key_file_is_hex_of_at_least_thirty_two_bytes() {
        let hex64 = "ab".repeat(32);
        assert_eq!(
            parse_session_key(&format!("  {hex64}\n")).unwrap(),
            vec![0xab; 32]
        );
        assert_eq!(parse_session_key("abc"), Err(SessionKeyError::NotHex));
        assert_eq!(parse_session_key("zz"), Err(SessionKeyError::NotHex));
        assert_eq!(
            parse_session_key(&"ab".repeat(31)),
            Err(SessionKeyError::TooShort)
        );
    }
}
