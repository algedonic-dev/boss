//! A passkey-promotion packet, judged, and the ticket that spends it —
//! ONE definition for the gateway that runs the ceremony and the people
//! service that flips the row (design 2cb6256f, David 2026-09-28;
//! backlog 2a228d0c).
//!
//! WHY THIS LIVES IN boss-core. The gateway (Tier 1) judges the packet
//! at the /me finish before it asks for anything, and boss-people (Tier
//! 2) judges it AGAIN under its own read before it writes. Two copies of
//! the judge could disagree about what an approval is, and the weaker
//! one would decide (CLAUDE.md §9a) — so the judge moved here from
//! boss-people, where the first car of the design put it, when the
//! gateway became its second reader.
//!
//! THE TICKET. A copied owner cookie with LAN reach can enrol a software
//! key, file a packet for it, approve it with that key, and — while the
//! machine gate is off — POST to people claiming to be the gateway (the
//! review of car 293d5dc1). The one factor that copy cannot reach is a
//! break-glass hardware key (Q1 of the design), and only the gateway can
//! verify one. So the gateway, having verified BOTH assertions of the
//! ceremony (the key being promoted, and a break-glass key), signs a
//! [`PromotionTicket`] with the session key — the key the presence
//! ticket is signed with, which the gateway, the jobs API and people
//! read from the one Secret mount of the one `boss` container — and
//! people promotes nothing without one. The ticket binds the packet, the
//! employee, the credential, the break-glass key that vouched, a
//! single-use nonce and an expiry; people reads `vouched_by` from it and
//! never from a request body.
//!
//! DOMAIN-SEPARATED. The session key also signs session cookies and
//! presence tickets, so a promotion ticket's MAC covers a fixed prefix
//! ([`TICKET_DOMAIN`]) before its payload: no cookie or presence ticket
//! the gateway ever signed verifies as a promotion ticket, whatever its
//! fields happen to parse as.

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;

use crate::job::{Assurance, Step, StepStatus};

/// The protocol, and the step slugs a judge reads, of
/// `infra/platform/workflows/passkey-promotion.toml` (pinned there by
/// boss-jobs' `platform_bundle_passkey_promotion.rs`).
pub const PROMOTION_KIND: &str = "passkey-promotion";
pub const AUTHORISE_STEP: &str = "authorise";
pub const PROMOTE_STEP: &str = "promote";

/// How long a minted ticket may be presented. The gateway presents it
/// in the same request that minted it, so this only bounds a copy.
pub const TICKET_TTL_SECONDS: u64 = 120;

/// The prefix a promotion ticket's MAC covers and nothing else's does.
pub const TICKET_DOMAIN: &str = "boss/passkey-promotion-ticket/v1:";

/// Refusals carry their status and a sentence naming what failed.
pub type Refusal = (StatusCode, String);

/// The key an approved packet names, as the owner signed it: what the row
/// must still be, under the flip's lock, for the flip to happen.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovedKey {
    pub label: String,
    pub registered_at: DateTime<Utc>,
    /// `crate::presence::public_key_fingerprint` of the stored key.
    pub public_key_sha256: String,
}

/// JUDGE THE PACKET for promoting `credential_id` of `employee_id`. Pure,
/// so every refusal is pinned by a test that changes one thing on a
/// packet that otherwise authorises. The approved key only when every
/// check holds; otherwise the FIRST that fails, by name — 403 when the
/// packet does not authorise THIS key, 409 when it is not (or no longer)
/// in the state that authorises anything, or names the key incompletely.
pub fn judge_packet(
    job: &Value,
    employee_id: &str,
    credential_id: &str,
) -> Result<ApprovedKey, Refusal> {
    let conflict = |m: String| (StatusCode::CONFLICT, m);
    let forbidden = |m: String| (StatusCode::FORBIDDEN, m);
    let job_id = job["id"].as_str().unwrap_or("?");
    let kind = job["kind"].as_str().unwrap_or_default();
    if kind != PROMOTION_KIND {
        return Err(conflict(format!(
            "packet {job_id} is a `{kind}`, not a `{PROMOTION_KIND}` — only that protocol \
             authorises a promotion"
        )));
    }
    let status = job["status"].as_str().unwrap_or("unknown");
    if status != "open" {
        return Err(conflict(format!(
            "packet {job_id} is {status}, and only an open packet authorises a promotion"
        )));
    }
    let step = |slug: &str| -> Result<Step, Refusal> {
        let raw = job["steps"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
            .ok_or_else(|| conflict(format!("packet {job_id} has no `{slug}` step")))?;
        serde_json::from_value::<Step>(raw.clone())
            .map_err(|_| conflict(format!("packet {job_id}'s `{slug}` step is unreadable")))
    };
    let authorise = step(AUTHORISE_STEP)?;
    if authorise.status != StepStatus::Completed {
        return Err(conflict(format!(
            "packet {job_id}'s `{AUTHORISE_STEP}` step is not completed — the promotion is not \
             approved yet"
        )));
    }
    let decision = authorise.metadata["decision"].as_str().unwrap_or("none");
    if decision != "approved" {
        return Err(conflict(format!(
            "packet {job_id}'s decision is `{decision}`, and only `approved` authorises a \
             promotion"
        )));
    }
    let named = |key: &str| authorise.metadata[key].as_str().unwrap_or("").to_string();
    let (named_employee, named_credential) = (named("employee_id"), named("credential_id"));
    if named_employee != employee_id {
        return Err(forbidden(format!(
            "packet {job_id} authorises a key of `{named_employee}`, not of `{employee_id}`"
        )));
    }
    if named_credential != credential_id {
        return Err(forbidden(format!(
            "packet {job_id} authorises credential `{named_credential}`, not \
             `{credential_id}`"
        )));
    }
    // The rest of what the owner signed about the key: the flip compares
    // each with the row, so a key deleted and re-registered under the
    // same credential id is not the key he approved.
    let label = named("label");
    let public_key_sha256 = named("public_key_sha256");
    let registered_at = DateTime::parse_from_rfc3339(&named("registered_at"))
        .map(|t| t.with_timezone(&Utc))
        .ok();
    let (Some(registered_at), false, false) = (
        registered_at,
        label.is_empty(),
        public_key_sha256.is_empty(),
    ) else {
        return Err(conflict(format!(
            "packet {job_id}'s `{AUTHORISE_STEP}` step must name the key's `label`, \
             `registered_at` (RFC 3339) and `public_key_sha256`, and it does not — an \
             incompletely named key cannot be told from a substitute"
        )));
    };
    let signers: Vec<&str> = authorise
        .live_stamps()
        .filter(|st| st.assurance == Assurance::Presence)
        .map(|st| st.authority_id.as_str())
        .collect();
    if !signers.contains(&employee_id) {
        return Err(forbidden(if signers.is_empty() {
            format!(
                "packet {job_id}'s `{AUTHORISE_STEP}` step carries no live presence stamp — a \
                 session sign-off, or a passkey signature over content that has since changed, \
                 authorises nothing"
            )
        } else {
            format!(
                "packet {job_id} was presence-signed by {}, not by {employee_id}, whose key it \
                 promotes",
                signers.join(", ")
            )
        }));
    }
    let promote = step(PROMOTE_STEP)?;
    if matches!(promote.status, StepStatus::Completed | StepStatus::Skipped) {
        return Err(conflict(
            format!(
                "packet {job_id}'s `{PROMOTE_STEP}` step is already {:?} — an authorisation is \
             spent once",
                promote.status
            )
            .to_lowercase(),
        ));
    }
    Ok(ApprovedKey {
        label,
        registered_at,
        public_key_sha256,
    })
}

/// A finished promotion ceremony, as the gateway vouches for it. Every
/// field is named once, short, in the presence ticket's style; unknown
/// fields are refused so nothing else signed with the key parses as one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromotionTicket {
    /// the `passkey-promotion` packet the ceremony spends
    pub p: String,
    /// the employee whose key is promoted
    pub i: String,
    /// the credential id (base64url) being promoted — it asserted
    pub c: String,
    /// the label of the break-glass hardware key whose assertion vouched
    pub v: String,
    /// single-use nonce — people spends it with the flip
    pub n: String,
    /// absolute expiry, seconds since epoch
    pub e: u64,
}

fn ticket_mac(key: &[u8], payload_b64: &str) -> Option<Vec<u8>> {
    crate::presence::mac(key, &format!("{TICKET_DOMAIN}{payload_b64}"))
}

impl PromotionTicket {
    /// `<base64url(json)>.<base64url(hmac_sha256(key, DOMAIN || base64url(json)))>`.
    pub fn encode(&self, key: &[u8]) -> Option<String> {
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).ok()?);
        let sig_b64 = URL_SAFE_NO_PAD.encode(ticket_mac(key, &payload_b64)?);
        Some(format!("{payload_b64}.{sig_b64}"))
    }

    /// The ticket, iff `value` was signed with `key` as a promotion
    /// ticket and is unexpired at `now_epoch`. The signature is checked,
    /// in constant time, BEFORE the payload is parsed.
    pub fn decode(value: &str, key: &[u8], now_epoch: u64) -> Option<Self> {
        let (payload_b64, sig_b64) = value.split_once('.')?;
        let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
        let expected = ticket_mac(key, payload_b64)?;
        if expected.ct_eq(&sig).unwrap_u8() != 1 {
            return None;
        }
        let ticket: Self =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
        (ticket.e > now_epoch).then_some(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::{PresenceTicket, now_epoch};
    use serde_json::json;

    const PACKET: &str = "7a1d2c3b-0000-4000-8000-00000000c0de";
    const OWNER: &str = "emp-owner";
    const CRED: &str = "cHJvbW90ZS1jcmVkZW50aWFsLWlk";
    const FINGERPRINT: &str = "3f1c0e2d";
    const KEY: &[u8] = b"promotion-ticket-test-session-key";

    // ---- the ticket ------------------------------------------------------

    fn ticket(e: u64) -> PromotionTicket {
        PromotionTicket {
            p: PACKET.into(),
            i: OWNER.into(),
            c: CRED.into(),
            v: "primary".into(),
            n: "n0nce".into(),
            e,
        }
    }

    #[test]
    fn a_ticket_round_trips_and_expires() {
        let t = ticket(now_epoch() + 60);
        let enc = t.encode(KEY).unwrap();
        assert_eq!(
            PromotionTicket::decode(&enc, KEY, now_epoch()),
            Some(t.clone())
        );
        assert!(PromotionTicket::decode(&enc, KEY, t.e).is_none(), "expired");
        assert!(PromotionTicket::decode(&enc, b"another-key", now_epoch()).is_none());
        let (payload, sig) = enc.split_once('.').unwrap();
        let mut other = t.clone();
        other.v = "backup".into();
        let swapped = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&other).unwrap());
        assert_ne!(swapped, payload);
        assert!(
            PromotionTicket::decode(&format!("{swapped}.{sig}"), KEY, now_epoch()).is_none(),
            "a payload edited under a kept signature"
        );
    }

    /// The session key signs presence tickets too. A presence ticket's
    /// signature is over its bare payload; a promotion ticket's is over
    /// the domain prefix and its payload — so a presence ticket, or any
    /// value signed the presence way, never verifies as a promotion.
    #[test]
    fn nothing_else_the_key_signs_is_a_promotion_ticket() {
        let presence = PresenceTicket {
            i: OWNER.into(),
            s: "step".into(),
            h: "hash".into(),
            n: "n".into(),
            e: now_epoch() + 60,
        }
        .encode(KEY)
        .unwrap();
        assert!(PromotionTicket::decode(&presence, KEY, now_epoch()).is_none());
        // Even a promotion-shaped payload signed the presence way.
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ticket(now_epoch() + 60)).unwrap());
        let presence_signed = format!(
            "{payload}.{}",
            URL_SAFE_NO_PAD.encode(crate::presence::mac(KEY, &payload).unwrap())
        );
        assert!(PromotionTicket::decode(&presence_signed, KEY, now_epoch()).is_none());
        // And a promotion ticket is no presence ticket.
        let enc = ticket(now_epoch() + 60).encode(KEY).unwrap();
        assert!(PresenceTicket::decode(&enc, KEY, now_epoch()).is_none());
    }

    // ---- the judge (moved from boss-people with its tests) --------------

    /// A `passkey-promotion` packet the owner approved with his passkey
    /// over the authorise step's current content, `promote` waiting: the
    /// one state that authorises. Each test below changes ONE thing.
    fn packet() -> Value {
        let mut p = json!({
            "id": PACKET,
            "kind": PROMOTION_KIND,
            "status": "open",
            "steps": [
                {
                    "id": "7a1d2c3b-0000-4000-8000-0000000000a1",
                    "job_id": PACKET,
                    "spec_slug": AUTHORISE_STEP,
                    "kind": "sign-off",
                    "title": "Authorise making yubikey-5c an operator key",
                    "status": "completed",
                    "metadata": {
                        "employee_id": OWNER,
                        "credential_id": CRED,
                        "label": "yubikey-5c",
                        "registered_at": "2026-09-28T09:00:00Z",
                        "public_key_sha256": FINGERPRINT,
                        "decision": "approved",
                    },
                    "sign_offs": [],
                },
                {
                    "id": "7a1d2c3b-0000-4000-8000-0000000000a2",
                    "job_id": PACKET,
                    "spec_slug": PROMOTE_STEP,
                    "kind": "task",
                    "title": "Finish on /me",
                    "status": "ready",
                    "metadata": {},
                },
            ],
        });
        stamp(&mut p, OWNER, "presence");
        p
    }

    /// Replace the authorise step's stamps with one by `who`, over the
    /// step's CURRENT shape — so a test that edits the metadata and then
    /// re-stamps changes exactly what it names.
    fn stamp(p: &mut Value, who: &str, assurance: &str) {
        let step = &mut p["steps"][0];
        let hash = crate::job::step_shape_hash(step["title"].as_str().unwrap(), &step["metadata"]);
        step["sign_offs"] = json!([{
            "authority_id": who,
            "role": "platform-admin",
            "stamped_at": "2026-09-28T10:00:01Z",
            "shape_hash": hash,
            "assurance": assurance,
            "presence_nonce": "n0nce",
        }]);
    }

    fn refused(p: &Value) -> (StatusCode, String) {
        judge_packet(p, OWNER, CRED).expect_err("this packet must not authorise")
    }

    #[test]
    fn an_approved_packet_for_this_key_authorises_it() {
        let approved = judge_packet(&packet(), OWNER, CRED).expect("this packet authorises");
        assert_eq!(approved.label, "yubikey-5c");
        assert_eq!(approved.public_key_sha256, FINGERPRINT);
        assert_eq!(
            approved.registered_at.to_rfc3339(),
            "2026-09-28T09:00:00+00:00"
        );
    }

    /// The flip compares label, enrolment time and key fingerprint with
    /// the row; a packet that does not name all three could not be told
    /// from a substitute, so it authorises nothing.
    #[test]
    fn a_packet_that_names_the_key_incompletely_authorises_nothing() {
        for (key, value) in [
            ("public_key_sha256", json!("")),
            ("label", json!("")),
            ("registered_at", json!("yesterday")),
        ] {
            let mut p = packet();
            p["steps"][0]["metadata"][key] = value;
            stamp(&mut p, OWNER, "presence");
            let (s, m) = refused(&p);
            assert_eq!(s, StatusCode::CONFLICT, "{key}");
            assert!(m.contains("public_key_sha256"), "{key}: {m}");
        }
    }

    #[test]
    fn a_packet_of_another_protocol_authorises_nothing() {
        let mut p = packet();
        p["kind"] = json!("break-glass-enrolment");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(m.contains("not a `passkey-promotion`"), "{m}");
    }

    #[test]
    fn a_packet_no_longer_open_authorises_nothing() {
        let mut p = packet();
        p["status"] = json!("cancelled");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(m.contains("is cancelled"), "{m}");
    }

    #[test]
    fn an_authorisation_not_yet_completed_authorises_nothing() {
        let mut p = packet();
        p["steps"][0]["status"] = json!("active");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(m.contains("not completed"), "{m}");
    }

    #[test]
    fn a_rejected_authorisation_authorises_nothing() {
        let mut p = packet();
        p["steps"][0]["metadata"]["decision"] = json!("rejected");
        stamp(&mut p, OWNER, "presence");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(m.contains("`rejected`"), "{m}");
    }

    #[test]
    fn a_packet_for_another_employee_authorises_nothing_here() {
        let mut p = packet();
        p["steps"][0]["metadata"]["employee_id"] = json!("emp-other");
        stamp(&mut p, OWNER, "presence");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(m.contains("`emp-other`"), "{m}");
    }

    #[test]
    fn a_packet_for_another_credential_authorises_nothing_here() {
        let mut p = packet();
        p["steps"][0]["metadata"]["credential_id"] = json!("b3RoZXIta2V5");
        stamp(&mut p, OWNER, "presence");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(m.contains("`b3RoZXIta2V5`"), "{m}");
    }

    /// A session sign-off is not a passkey: `presence` is required.
    #[test]
    fn a_session_stamp_authorises_nothing() {
        let mut p = packet();
        stamp(&mut p, OWNER, "session");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(m.contains("no live presence stamp"), "{m}");
    }

    /// A stamp over content that has since changed is not live.
    #[test]
    fn a_stamp_over_changed_content_authorises_nothing() {
        let mut p = packet();
        p["steps"][0]["metadata"]["label"] = json!("renamed-after-signing");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(m.contains("no live presence stamp"), "{m}");
    }

    #[test]
    fn a_stamp_by_someone_else_authorises_nothing() {
        let mut p = packet();
        stamp(&mut p, "emp-other", "presence");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert!(m.contains("presence-signed by emp-other"), "{m}");
    }

    #[test]
    fn a_spent_authorisation_authorises_nothing() {
        let mut p = packet();
        p["steps"][1]["status"] = json!("completed");
        let (s, m) = refused(&p);
        assert_eq!(s, StatusCode::CONFLICT);
        assert!(m.contains("already completed"), "{m}");
    }
}
