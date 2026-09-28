//! Who authored a ledger write — taken from the actor that SIGNED the
//! request, never from its body (backlog 7bf42e2b, 975c228f).
//!
//! WHY. Measured 2026-09-27 at origin/main 756da393: the manual-entry
//! handler read `created_by` from the body (defaulting `admin`), the COGS
//! handler likewise (defaulting `ledger`), the period lock read
//! `locked_by` and the yearly close `closed_by` — so any caller could
//! name any author on a financial fact or a lock. The finance page sent
//! the literal `operator` on every lock, which the Periods table rendered
//! as "Locked by", and its Reverse button sent `created_by: null`, which
//! credited every reversal to `admin`. The event each write staged
//! already carried the true actor; the row and the fact payload did not.
//!
//! THE RULE is the one the jobs API applies to a packet's `opened_by`:
//! the author is the signed caller, spelled as the write's own event
//! stamp spells it ([`signer`] is the one place both read it). A body
//! that names ANOTHER author is refused 422 before anything is read or
//! written — it is claiming an authorship it does not hold — while a
//! body naming the signer (by the log's spelling or the id it signed
//! with) or naming no one is admitted.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_core::actor::ActorId;
use boss_policy_client::User;
use serde_json::json;

/// The actor a ledger write is signed as: the request's ambient actor,
/// or the `platform` automation for an unsigned call. `event_stamp`
/// stamps the write's event with this same actor, so the row, the fact
/// and the log name one author.
pub(crate) fn signer(user: &User) -> ActorId {
    user.ambient_actor()
        .unwrap_or_else(|| ActorId::Automation("platform".into()))
}

/// A body that named someone other than the signer as the author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnotherAuthor {
    key: &'static str,
    said: String,
    signer: String,
}

impl IntoResponse for AnotherAuthor {
    fn into_response(self) -> Response {
        let key = self.key;
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "a ledger write's author is the actor that signs it",
                "refused_keys": [key],
                key: self.said,
                "signed_as": self.signer,
                "hint": format!("drop `{key}` from the body: the ledger records the signed caller"),
            })),
        )
            .into_response()
    }
}

/// Judge a body's author field (`key` names it in the refusal) against
/// the signer and return the author to record. `said` is what the body
/// sent, `None` for an absent or null key.
pub(crate) fn author(
    user: &User,
    key: &'static str,
    said: Option<&str>,
) -> Result<String, AnotherAuthor> {
    let signer = signer(user).to_string();
    match said {
        None => Ok(signer),
        Some(s) if s == signer || s == user.id => Ok(signer),
        Some(s) => Err(AnotherAuthor {
            key,
            said: s.to_string(),
            signer,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> User {
        let mut u = User::anonymous();
        u.id = id.to_string();
        u
    }

    #[test]
    fn no_claim_is_credited_to_the_signer() {
        assert_eq!(
            author(&user("emp-cfo"), "created_by", None).ok(),
            Some("emp-cfo".into())
        );
    }

    #[test]
    fn a_claim_naming_the_signer_is_admitted_under_either_spelling() {
        let rule = user("rule:bill-approve");
        for said in ["rule:bill-approve", "automation:rule:bill-approve"] {
            assert_eq!(
                author(&rule, "created_by", Some(said)).ok(),
                Some("automation:rule:bill-approve".into())
            );
        }
    }

    #[test]
    fn a_claim_naming_another_author_is_refused_422() {
        let refused = author(&user("emp-cfo"), "locked_by", Some("operator")).expect_err("another");
        assert_eq!(
            refused.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn an_unsigned_call_is_the_platform_automation() {
        assert_eq!(
            author(&User::anonymous(), "created_by", None).ok(),
            Some("automation:platform".into())
        );
    }
}
