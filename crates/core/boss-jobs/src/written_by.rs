//! The `written_by` declaration on a step: the ONE machine actor whose
//! record the step believes. Every other automation or agent session is
//! refused a write to the step's declared fields while it is open; a
//! person is never refused (backlog aa816dd4, LOW-1 of review ea2ecfd4).
//!
//! WHY. ops-request's `nothing-to-do` terminal closes a request with no
//! passkey asked, gated on the seven record keys the runner writes (the
//! plan it rendered and everything it was rendered for) — required at
//! done, so a marker alone closes nothing. But any caller with Update on
//! the step could write those seven keys and then the marker, and the
//! dispatcher's marker rule would close the request on a plan no runner
//! ever rendered. Nothing ran, but a remedy someone was meant to sign
//! was closed with nobody asked, on a record that looks the runner's.
//!
//! WHAT IT IS, AND WHAT IT IS NOT. The signer here is the `x-boss-user`
//! id, which any holder of the machine token can type, so this refuses
//! the HONEST MISTAKE — an agent or a rule writing a record that is the
//! runner's to write — not a forger. That is the line
//! `credential.rotate.ops-runner`'s `DEPOSIT_ACTOR` draws for the same
//! reason (review 3930a3eb, N2). The forger-proof guard is the declared
//! field `writer` (design f623e425, [`crate::field_writer`]), which
//! believes only a server-resolved credential; it lands on a protocol
//! when the runner presents one, and no runner does yet
//! ([`crate::field_writer::RESOLVABLE_PRINCIPALS`] is empty), so a
//! `writer` declared today would lock the runner out. This is the guard
//! that can hold until then, and it can sit beside a `writer` after.
//!
//! NEVER ON A PERSON'S PATH (DR rule 62dac114). A signer that is not
//! machine-shaped ([`crate::owner_resolution::is_automation_shaped`]) is
//! never refused: David closing a request by hand is a decision, and
//! the record says it was his. Only an automation or agent session
//! other than the declared one is refused.
//!
//! THE DECLARATION IS PROTOCOL DATA, NOT A NEW FIELD, the channel
//! `human_only` rides: a Workflow step authors
//! `metadata_defaults.written_by`, materialisation carries it onto the
//! packet's step, and [`crate::step_metadata_write::PROTOCOL_KEYS`]
//! freezes it there, so a write cannot delete it and then write the
//! record. The merge door is the one writer of step metadata (a step
//! PUT carrying metadata is refused, e39a9d2a), so it is the one door
//! that asks.

use serde_json::Value;

/// The metadata key a Workflow step authors to name its one machine
/// writer.
pub const KEY: &str = "written_by";

/// The actor this step metadata declares as its one machine writer, or
/// `None` when it declares none (absent, or not a non-empty string).
pub fn declared(metadata: &Value) -> Option<&str> {
    metadata
        .get(KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// PURE: the declared fields of an open step that a write signed by
/// `actor` may not change — empty when the step declares no writer, the
/// signer is the declared writer, the signer is not machine-shaped (a
/// person is never refused), or the write changes no declared field.
/// `next` is the row as the write would leave it (a merge-door `null`
/// already applied). Sorted.
pub fn refused_keys(
    stored: &Value,
    next: &Value,
    fields: &[boss_core::job::StepField],
    actor: &str,
) -> Vec<String> {
    let Some(writer) = declared(stored) else {
        return Vec::new();
    };
    if actor == writer || !crate::owner_resolution::is_automation_shaped(actor) {
        return Vec::new();
    }
    let mut keys: Vec<String> = fields
        .iter()
        .filter(|f| stored.get(&f.name) != next.get(&f.name))
        .map(|f| f.name.clone())
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The sentence a refusal carries as its `rule`.
pub const RULE: &str = "metadata.written_by = <actor>: while the step is open, its declared \
                        fields are that actor's record; another automation or agent session \
                        may not write them, and a person may";

/// The refusal: names the step, the signer, the declared writer, the
/// keys refused and the door, so the caller learns what to do without
/// re-deriving it.
pub fn refusal_body(
    step_id: &str,
    step_title: &str,
    door: &str,
    actor_id: &str,
    writer: &str,
    refused_keys: &[String],
) -> Value {
    serde_json::json!({
        "error": "this step's record is written by its declared writer",
        "step_id": step_id,
        "step_title": step_title,
        "door": door,
        "actor_id": actor_id,
        "written_by": writer,
        "refused_keys": refused_keys,
        "rule": RULE,
        "hint": "the declared writer writes this record; annotate the packet through \
                 PATCH /api/jobs/{id}/metadata instead, or have a person write it signed in \
                 as themselves",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::StepField;
    use serde_json::json;

    fn field(name: &str) -> StepField {
        serde_json::from_value(json!({"name": name, "field_type": "string", "required": true}))
            .unwrap()
    }

    const RUNNER: &str = "automation:ops-runner";

    fn stored() -> Value {
        json!({"outcome_kind": "skipped", "written_by": RUNNER})
    }

    fn with_plan() -> Value {
        json!({"outcome_kind": "skipped", "written_by": RUNNER, "plan": "p"})
    }

    #[test]
    fn an_agent_writing_the_record_is_refused_and_named_keys_listed() {
        let fields = [field("plan"), field("verb")];
        assert_eq!(
            refused_keys(&stored(), &with_plan(), &fields, "agent-claude"),
            vec!["plan".to_string()]
        );
        assert_eq!(
            refused_keys(&stored(), &with_plan(), &fields, "automation:rule:x"),
            vec!["plan".to_string()]
        );
    }

    #[test]
    fn the_declared_writer_and_a_person_are_never_refused() {
        let fields = [field("plan")];
        assert!(refused_keys(&stored(), &with_plan(), &fields, RUNNER).is_empty());
        assert!(refused_keys(&stored(), &with_plan(), &fields, "emp-david").is_empty());
    }

    #[test]
    fn an_undeclared_step_or_a_key_that_is_not_a_field_is_not_judged() {
        let fields = [field("plan")];
        let bare = json!({"outcome_kind": "skipped"});
        let mut next = bare.clone();
        next["plan"] = json!("p");
        assert!(refused_keys(&bare, &next, &fields, "agent-claude").is_empty());
        let mut note = stored();
        note["note"] = json!("context");
        assert!(refused_keys(&stored(), &note, &fields, "agent-claude").is_empty());
        // An unchanged re-send changes nothing, so it is not refused.
        assert!(refused_keys(&with_plan(), &with_plan(), &fields, "agent-claude").is_empty());
    }

    #[test]
    fn the_declaration_reads_only_a_non_empty_string() {
        assert_eq!(declared(&stored()), Some(RUNNER));
        assert_eq!(declared(&json!({"written_by": ""})), None);
        assert_eq!(declared(&json!({"written_by": true})), None);
        assert_eq!(declared(&json!({})), None);
    }

    #[test]
    fn the_declaration_is_frozen_on_the_step() {
        assert!(crate::step_metadata_write::PROTOCOL_KEYS.contains(&KEY));
    }
}
