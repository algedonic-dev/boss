//! Who filed a packet — `metadata.opened_by`, stamped at admission from
//! the actor that SIGNED the create (backlog 958edca6).
//!
//! WHY. Measured 2026-09-27: all 984 ops-requests closed in two days read
//! owner `emp-david`, including the ones `agent-claude` filed and the ones
//! a dispatcher rule spawned. Admission is right to do that to the OWNER:
//! subject-model Q7 makes the owner the responsible HUMAN
//! ([`crate::owner_resolution`]), and the owner is what the Self/Team
//! policy scope, the `notify_on_done` wait-is-over signal and the
//! terminal notification all route on. But the owner was also the only
//! name on the packet, so read off the packet a person was credited with
//! an agent's or a rule's filing, and who filed it survived only as the
//! create event's actor. `boss job file` began stamping `opened_by` on
//! the same day (item_source.rs), which covered one door of many: `boss
//! ops`, the dispatcher's spawns, the web and every other caller of
//! `POST /api/jobs` still filed without it.
//!
//! THE RULE. Admission stamps the key on every packet, whatever the door,
//! from the same actor the `jobs.job.created` event names — so the packet
//! and its log agree, and a rule reads back as the log spells it
//! (`automation:rule:<name>`). The key is server-owned: a body naming a
//! different filer is refused (it is claiming a filing it did not make;
//! naming its own signer, as `boss job file` does, is admitted), the job
//! PUT carries the stored value forward, and the metadata PATCH refuses
//! the key.

use boss_core::actor::ActorId;
use serde_json::{Map, Value, json};

/// The job-metadata key the filer is recorded under. The same key `boss
/// job file` already wrote (`boss-cli` item_source.rs `OPENED_BY_KEY`),
/// so the packets it filed read the same as every packet filed since.
pub const OPENED_BY_KEY: &str = "opened_by";

/// The refusal the generic job metadata PATCH answers an `opened_by` key
/// with.
pub const PATCH_REFUSAL: &str = "`opened_by` records who filed the packet: it is stamped once, \
     at admission, from the actor that signed the create, and no later write may set or \
     erase it";

/// Why a create body was refused over its `opened_by`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The body names someone other than the signer as the filer.
    AnotherFiler { said: Value, signer: String },
    /// The body's metadata is not an object, so it cannot carry the key.
    MetadataNotAnObject,
}

impl Refusal {
    /// The 422 body the create handler answers with.
    pub fn body(&self) -> Value {
        match self {
            Self::AnotherFiler { said, signer } => json!({
                "error": "a packet's filer is the actor that signs its create",
                "refused_keys": [OPENED_BY_KEY],
                "opened_by": said,
                "signed_as": signer,
                "hint": "drop `opened_by` from the metadata: admission stamps it from the signed caller",
            }),
            Self::MetadataNotAnObject => json!({
                "error": "a packet's metadata is a JSON object: it carries who filed the packet",
                "refused_keys": ["metadata"],
            }),
        }
    }
}

/// Judge a create body's metadata against the signer, before anything is
/// written. Admitted: no key, a null key, or a key naming the signer —
/// by the spelling the log uses or the id it signed with (`rule:<name>`
/// is `automation:rule:<name>` in the log; both name one rule).
pub fn check(metadata: &Value, signer: &ActorId, signed_id: &str) -> Result<(), Refusal> {
    let md = match metadata {
        Value::Null => return Ok(()),
        Value::Object(md) => md,
        _ => return Err(Refusal::MetadataNotAnObject),
    };
    match md.get(OPENED_BY_KEY) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if *s == signer.to_string() || s == signed_id => Ok(()),
        Some(said) => Err(Refusal::AnotherFiler {
            said: said.clone(),
            signer: signer.to_string(),
        }),
    }
}

/// Stamp the signer as the filer. Run after [`check`], so a value already
/// present names the signer and is overwritten with the log's spelling.
pub fn stamp(metadata: &mut Value, signer: &ActorId) {
    if metadata.is_null() {
        *metadata = Value::Object(Map::new());
    }
    if let Value::Object(md) = metadata {
        md.insert(OPENED_BY_KEY.to_string(), json!(signer.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> ActorId {
        "agent-claude".parse().expect("infallible")
    }

    #[test]
    fn an_absent_or_null_key_is_admitted_and_stamped() {
        for mut md in [json!({}), Value::Null, json!({"opened_by": null})] {
            assert_eq!(check(&md, &agent(), "agent-claude"), Ok(()));
            stamp(&mut md, &agent());
            assert_eq!(md[OPENED_BY_KEY], "agent-claude");
        }
    }

    #[test]
    fn another_filer_is_refused_naming_both() {
        let md = json!({"opened_by": "emp-david"});
        let err = check(&md, &agent(), "agent-claude").expect_err("another filer");
        let body = err.body();
        assert_eq!(body["opened_by"], "emp-david");
        assert_eq!(body["signed_as"], "agent-claude");
    }

    #[test]
    fn a_rule_is_admitted_under_either_spelling_and_stamped_as_the_log_spells_it() {
        let rule = ActorId::Automation("rule:bill-approve".into());
        for said in ["rule:bill-approve", "automation:rule:bill-approve"] {
            let mut md = json!({"opened_by": said});
            assert_eq!(check(&md, &rule, "rule:bill-approve"), Ok(()));
            stamp(&mut md, &rule);
            assert_eq!(md[OPENED_BY_KEY], "automation:rule:bill-approve");
        }
    }

    #[test]
    fn a_non_object_metadata_is_refused() {
        assert_eq!(
            check(&json!(["x"]), &agent(), "agent-claude"),
            Err(Refusal::MetadataNotAnObject)
        );
    }
}
