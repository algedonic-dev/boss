//! Shared reservation projection; legacy stamps remain readable, but a
//! versioned reservation is authoritative once explicitly migrated.
use crate::conditional_metadata::{Command, Expected};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

pub const KEY: &str = "gate_reservation";
pub const INTENT: &str = "gate_launch_intent";
pub const OBSERVATION: &str = "gate_launch_observation";
pub const ATTEMPT: &str = "gate_launch_attempt";
pub const SUBMISSION: &str = "gate_submission";
pub const FIRST_RECORD_FIELDS: [&str; 4] = [INTENT, OBSERVATION, "gate_orphan_claim", SUBMISSION];
pub const TTL_SECONDS: i64 = 300;

/// A remote alias read cannot authorize an immutable actor assertion. Bind
/// each new record to the principal THIS request authenticated before commit.
/// An existing attempt's later no-child evidence retains its original actor,
/// and only that actor, authenticated now, may write it: checked against the
/// stored actor alone, any principal with update scope could rewrite another
/// actor's attempt while the record still named the original (backlog
/// 59ce3050, review 421d6960 F1).
///
/// An attempt is judged on the command's own `expected` attempt, which is
/// the value the compare-and-set holds the row to inside the transaction,
/// never on a read taken before it (review 5426da05 N2) — so an attempt
/// write must pin the attempt it continues or replaces, or its absence.
/// A different id is a replacement: it must name the pinned attempt as
/// its `previous`, and be FRESH, which [`refuse_a_written_attempt_id`]
/// judges inside the repository's transaction rather than here.
pub fn validate_actor_bindings(command: &Command, actor: &str) -> Result<(), String> {
    for (key, field) in [
        (SUBMISSION, "submitted_by"),
        ("gate_orphan_claim", "actor"),
        (ATTEMPT, "actor"),
    ] {
        let Some(value) = command.patch.get(key) else {
            continue;
        };
        let continued = if key == ATTEMPT {
            continued_attempt(command, value)?
        } else {
            None
        };
        let expected = match continued {
            Some(stored) if stored.get(field).and_then(Value::as_str) != Some(actor) => None,
            _ => Some(actor),
        };
        if expected.is_none() || value.get(field).and_then(Value::as_str) != expected {
            return Err(format!(
                "{key}.{field} must match its authenticated originating actor; no record was committed"
            ));
        }
    }
    Ok(())
}

/// The pinned attempt `next` continues under the same id, `None` for a
/// first or a replacement attempt, or the refusal of an attempt write the
/// compare-and-set does not hold, or a replacement that does not name the
/// attempt it replaces.
fn continued_attempt<'a>(command: &'a Command, next: &Value) -> Result<Option<&'a Value>, String> {
    let pinned = command.expected.get(ATTEMPT).ok_or_else(|| {
        format!(
            "{ATTEMPT} must be pinned in expected (the attempt it continues or replaces, or its absence); no record was committed"
        )
    })?;
    let stored = pinned.present.then_some(&pinned.value);
    let stored_id = stored.and_then(attempt_id);
    let id = attempt_id(next)
        .ok_or_else(|| format!("{ATTEMPT}.id must be a non-nil uuid; no record was committed"))?;
    if stored_id == Some(id) {
        return Ok(stored);
    }
    let previous = match next.get("previous") {
        None | Some(Value::Null) => None,
        Some(_) => Some(uuid_at(next, "previous").ok_or_else(|| {
            format!("{ATTEMPT}.previous must be a non-nil uuid; no record was committed")
        })?),
    };
    if previous != stored_id {
        return Err(format!(
            "{ATTEMPT}.previous must name the attempt it replaces ({}); no record was committed",
            stored_id.map_or("none".into(), |id| id.to_string())
        ));
    }
    Ok(None)
}

/// An id is a uuid, not its spelling: `X` upper-cased is still `X`.
fn attempt_id(value: &Value) -> Option<Uuid> {
    uuid_at(value, "id")
}

fn uuid_at(value: &Value, field: &str) -> Option<Uuid> {
    value
        .get(field)?
        .as_str()
        .and_then(|text| Uuid::parse_str(text).ok())
        .filter(|id| !id.is_nil())
}

/// Refuse a gate attempt write whose id is not the pinned attempt's and
/// was already written on this packet (`written`, from
/// [`attempt_ids_written`]). Without freshness a replaced id could be
/// posted back by a new caller and one attempt id would name two actors
/// (backlog 0471152b).
///
/// The repository calls this under the packet's row lock, AFTER its
/// replay check, with the history read in the same transaction (backlog
/// 301b9060, review f6890829). Read before the lock, the history missed
/// an id an accomplice committed in between, and a write pinned on the
/// value the accomplice left next brought that id back (N1); and a
/// concurrent duplicate of a write that did land was refused as "already
/// written" instead of being handed its receipt (N2).
pub fn refuse_a_written_attempt_id(
    command: &Command,
    written: &std::collections::BTreeSet<Uuid>,
) -> Result<(), String> {
    let Some(id) = command.patch.get(ATTEMPT).and_then(attempt_id) else {
        return Ok(());
    };
    let pinned = command
        .expected
        .get(ATTEMPT)
        .filter(|pinned| pinned.present)
        .and_then(|pinned| attempt_id(&pinned.value));
    if pinned == Some(id) || !written.contains(&id) {
        return Ok(());
    }
    Err(format!(
        "{ATTEMPT}.id {id} was already written on this packet; a replacement of the pinned attempt ({}) names a fresh id; no record was committed",
        pinned.map_or("none".into(), |id| id.to_string())
    ))
}

/// Every attempt id this packet's conditional door ever committed, read
/// from the patches of its retained receipts — every actor's.
pub fn attempt_ids_written(
    receipts: &[crate::conditional_metadata::Receipt],
) -> std::collections::BTreeSet<Uuid> {
    receipts
        .iter()
        .filter_map(|receipt| receipt.command.patch.get(ATTEMPT))
        .filter_map(attempt_id)
        .collect()
}

/// The retained native request is data, not a reconstructed send permit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchIntent {
    pub contract_version: u32,
    pub packet: String,
    pub namespace: String,
    pub name: String,
    pub head: String,
    pub mode: String,
    #[serde(skip)]
    pub transition_generation: Option<Uuid>,
    pub file_id: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// One immutable source object is authoritative for resident consumers.
/// Raw metadata is only the legacy protocol's input, never a fallback
/// from unreadable or unbound resident presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub contract_version: u32,
    pub command: Uuid,
    pub submitted_by: String,
    pub supervisor: String,
    pub intent: LaunchIntent,
    pub source: serde_json::Map<String, Value>,
}

pub fn submission(metadata: &Value) -> Result<Option<Submission>, String> {
    let Some(value) = metadata.get(SUBMISSION) else {
        return Ok(None);
    };
    let source: Submission = serde_json::from_value(value.clone())
        .map_err(|error| format!("durable submission is unreadable: {error}"))?;
    if source.contract_version != 1
        || source.command.is_nil()
        || source.submitted_by.is_empty()
        || source.supervisor.is_empty()
        || source.intent.contract_version != 1
    {
        return Err("durable submission contract/identity is invalid".into());
    }
    crate::metadata_immutability::view(metadata, SUBMISSION, "/source", 1, &[])?;
    Ok(Some(source))
}

pub fn source_metadata(metadata: &Value) -> Result<&Value, String> {
    if submission(metadata)?.is_some() {
        crate::metadata_immutability::view(metadata, SUBMISSION, "/source", 1, SOURCE_LOCATORS)
    } else {
        Ok(metadata)
    }
}

/// The original server receipt, never a client clock or copied raw
/// timestamp, establishes an accepted request's identity and order.
/// This pure check is shared by actuators and read-only projections.
pub fn acceptance(
    current: &boss_core::job::Job,
    receipt: &crate::conditional_metadata::Receipt,
) -> Result<Submission, String> {
    source_metadata(&current.metadata)?;
    let submission = submission(&current.metadata)?
        .ok_or("packet has explicit legacy ownership, not a resident acceptance")?;
    let encoded = serde_json::to_value(&submission).map_err(|error| error.to_string())?;
    if current.kind != "gate-run"
        || receipt.job.kind != current.kind
        || receipt.job_id != current.id
        || receipt.job.id != current.id
        || submission.intent.packet != current.id.to_string()
        || current.workflow_version != receipt.job.workflow_version
        || receipt.contract_version != 2
        || receipt.command.contract_version != 2
        || receipt.command.idempotency_key != submission.command
        || receipt.actor != submission.submitted_by
        || receipt.event_id.is_nil()
        || receipt
            .receipt_event_id
            .is_none_or(|id| id.is_nil() || id == receipt.event_id)
        || !receipt
            .command
            .immutable_keys
            .iter()
            .any(|key| key == SUBMISSION)
        || receipt.command.patch.get(SUBMISSION) != Some(&encoded)
        || receipt.job.metadata.get(SUBMISSION) != Some(&encoded)
        || receipt
            .job
            .metadata
            .pointer("/_immutable_metadata/gate_submission")
            != Some(&encoded)
    {
        return Err(
            "durable submission lacks its exact original acceptance/current protocol/index binding"
                .into(),
        );
    }
    Ok(submission)
}

/// Even malformed presence prevents a client-expiry or negative native
/// inference. It cannot grant a claim or a native permission.
pub fn durable_presence(metadata: &Value) -> bool {
    metadata.get(SUBMISSION).is_some()
}
/// Resident ownership never expires with a client heartbeat. Unreadable
/// presence remains unresolved until the protected native record is proven.
pub fn waiting_for_native(metadata: &Value) -> bool {
    durable_presence(metadata) && !native_observed(metadata)
}
pub const SOURCE_LOCATORS: &[&str] = &["branch", "sha", "agent_run", "worktree"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub contract_version: u32,
    pub generation: Uuid,
    pub queued_at: String,
    pub heartbeat_at: String,
    pub consumed: bool,
}

pub fn instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

pub fn read(metadata: &Value) -> Result<Option<Reservation>, String> {
    let Some(value) = metadata.get(KEY) else {
        return Ok(None);
    };
    let row: Reservation = serde_json::from_value(value.clone())
        .map_err(|e| format!("unreadable gate reservation: {e}"))?;
    if row.contract_version != 1
        || row.generation.is_nil()
        || instant(&row.queued_at).is_none()
        || instant(&row.heartbeat_at).is_none()
    {
        return Err("unreadable gate reservation identity or timestamps".into());
    }
    Ok(Some(row))
}

fn queued_at(metadata: &Value) -> Option<&str> {
    if metadata.get(KEY).is_some() {
        if native_observed(metadata) {
            return None;
        }
        let row = read(metadata).ok()??;
        return (!row.consumed)
            .then(|| metadata.get(KEY)?.get("queued_at")?.as_str())
            .flatten();
    }
    metadata
        .get("queued_at")?
        .as_str()
        .filter(|s| !s.is_empty())
}

pub fn native_observed(metadata: &Value) -> bool {
    let Some(observed) = metadata.get(OBSERVATION) else {
        return false;
    };
    let Some(intent) = metadata.get(INTENT) else {
        return false;
    };
    let Ok(bindings) = crate::metadata_immutability::bindings(metadata) else {
        return false;
    };
    if bindings.get(INTENT) != Some(intent) || bindings.get(OBSERVATION) != Some(observed) {
        return false;
    }
    if durable_presence(metadata) {
        let Ok(Some(submission)) = submission(metadata) else {
            return false;
        };
        if source_metadata(metadata).is_err()
            || serde_json::to_value(&submission.intent).ok().as_ref() != Some(intent)
        {
            return false;
        }
    }
    observed
        .get("uid")
        .and_then(Value::as_str)
        .is_some_and(|uid| !uid.is_empty())
        && observed
            .get("request_sha256")
            .and_then(Value::as_str)
            .is_some_and(|sha| Some(sha) == intent.get("sha256").and_then(Value::as_str))
        && observed
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(instant)
            .is_some()
}

fn heartbeat(metadata: &Value) -> Option<&str> {
    if metadata.get(KEY).is_some() {
        read(metadata).ok()??;
        return metadata.get(KEY)?.get("heartbeat_at")?.as_str();
    }
    metadata
        .get("queue_heartbeat_at")
        .and_then(Value::as_str)
        .or_else(|| queued_at(metadata))
}

/// Presence includes malformed intent: absence of evidence cannot
/// authorize an orphan's negative terminal or safe withdrawal.
pub fn unresolved_intent(metadata: &Value) -> bool {
    metadata.get(INTENT).is_some()
        || durable_presence(metadata)
        || metadata.get("gate_launch_attempt").is_some()
}

pub fn stopped_reason(metadata: &Value, now: Option<DateTime<Utc>>) -> Option<&'static str> {
    if durable_presence(metadata) {
        return source_metadata(metadata).is_err().then_some("Durable queue request is unreadable or its index locators disagree; reconcile the original receipt");
    }
    metadata.get(KEY)?;
    let row = match read(metadata) {
        Ok(Some(row)) => row,
        _ => return Some("Gate reservation is unreadable; reconciliation requires attention"),
    };
    if metadata
        .get("launched_at")
        .and_then(Value::as_str)
        .and_then(instant)
        .is_some()
    {
        return None;
    }
    if row.consumed {
        return Some(
            "Native launch identity was observed; its launch stamp still needs reconciliation",
        );
    }
    if now.is_some_and(|now| !held(metadata, now)) {
        return Some(if unresolved_intent(metadata) {
            "Launch outcome UNKNOWN; inspect the exact recorded native Job before resuming"
        } else {
            "Queue holder expired; no bay or queue place is held"
        });
    }
    None
}

fn held(metadata: &Value, now: DateTime<Utc>) -> bool {
    queued_at(metadata).is_some()
        && heartbeat(metadata)
            .and_then(instant)
            .is_some_and(|beat| (now - beat).num_seconds() <= TTL_SECONDS)
}

fn expected(metadata: &Value, key: &str) -> Expected {
    Expected {
        present: metadata.get(key).is_some(),
        value: metadata.get(key).cloned().unwrap_or(Value::Null),
    }
}

/// The current generation's complete value is a CAS predicate; a
/// heartbeat racing this command produces a conflict and fresh read.
pub fn renew(metadata: &Value, now: DateTime<Utc>) -> Result<Command, String> {
    if metadata.get("gate_orphan_claim").is_some() {
        return Err("an original guarded negative decision prevents queue renewal".into());
    }
    if durable_presence(metadata) {
        return Err("resident owns the accepted request; a reader cannot renew or adopt it".into());
    }
    if native_observed(metadata) {
        return Err(
            "native launch identity already observed; attach rather than renew its queue place"
                .into(),
        );
    }
    let current = read(metadata)?;
    let stamp = now.to_rfc3339_opts(SecondsFormat::Secs, true);
    let keep = held(metadata, now);
    let row = Reservation {
        contract_version: 1,
        generation: current
            .as_ref()
            .filter(|_| keep)
            .map_or_else(Uuid::new_v4, |r| r.generation),
        queued_at: if keep {
            queued_at(metadata)
                .ok_or("held reservation lost its ordering stamp")?
                .to_owned()
        } else {
            stamp.clone()
        },
        heartbeat_at: stamp.clone(),
        consumed: false,
    };
    let mut guard = [
        KEY,
        SUBMISSION,
        OBSERVATION,
        "gate_orphan_claim",
        INTENT,
        ATTEMPT,
    ]
    .into_iter()
    .map(|key| (key.into(), expected(metadata, key)))
    .collect::<std::collections::BTreeMap<_, _>>();
    if current.is_none() {
        for key in ["queued_at", "queue_heartbeat_at"] {
            guard.insert(key.into(), expected(metadata, key));
        }
    }
    Ok(Command {
        contract_version: 1,
        idempotency_key: Uuid::new_v4(),
        expected: guard,
        patch: serde_json::from_value(
            json!({KEY:row,"queued_at":row.queued_at,"queue_heartbeat_at":stamp}),
        )
        .map_err(|e| e.to_string())?,
        immutable_keys: vec![],
        guarded_keys: vec![],
    })
}

pub fn consume(metadata: &Value, generation: Uuid) -> Result<Command, String> {
    if metadata.get("gate_orphan_claim").is_some() {
        return Err("an original guarded negative decision prevents native consumption".into());
    }
    let mut row = read(metadata)?.ok_or("no versioned reservation to consume")?;
    if row.generation != generation || row.consumed {
        return Err("reservation generation changed or was already consumed".into());
    }
    row.consumed = true;
    Ok(Command {
        contract_version: 1,
        idempotency_key: Uuid::new_v4(),
        expected: [KEY, OBSERVATION, "gate_orphan_claim"]
            .into_iter()
            .map(|key| (key.into(), expected(metadata, key)))
            .collect(),
        patch: serde_json::from_value(json!({KEY:row,"queued_at":null}))
            .map_err(|e| e.to_string())?,
        immutable_keys: vec![],
        guarded_keys: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn durable_owner_does_not_expire_with_its_submitter() {
        let first = renew(&json!({}), at("2026-10-15T12:00:00Z")).unwrap();
        let mut md = Value::Object(first.patch);
        md["gate_submission"] = json!({"unreadable":"still not permission to expire or resend"});
        assert!(
            !stopped_reason(&md, Some(at("2026-10-16T12:00:00Z")))
                .is_some_and(|reason| reason.contains("expired"))
        );
        assert!(
            renew(&md, at("2026-10-16T12:00:00Z")).is_err(),
            "a resumed client cannot renew durable ownership"
        );
    }
    #[test]
    fn stale_queue_commands_cannot_cross_new_submission_observation_or_negative_claim() {
        let first = renew(&json!({}), at("2026-10-16T06:00:00Z")).unwrap();
        let metadata = Value::Object(first.patch);
        let heartbeat = renew(&metadata, at("2026-10-16T06:01:00Z")).unwrap();
        let consumed = consume(&metadata, read(&metadata).unwrap().unwrap().generation).unwrap();
        for key in [SUBMISSION, OBSERVATION, "gate_orphan_claim"] {
            let mut job = test_job();
            job.metadata = metadata.clone();
            job.metadata[key] = json!({"original":"another committed transition"});
            assert!(!heartbeat.matches(&job), "a stale heartbeat crossed {key}");
        }
        for key in [OBSERVATION, "gate_orphan_claim"] {
            let mut job = test_job();
            job.metadata = metadata.clone();
            job.metadata[key] = json!({"original":"another committed transition"});
            assert!(!consumed.matches(&job), "a stale consume crossed {key}");
        }
    }

    /// WHY (backlog 59ce3050, review 421d6960 F1). A same-id attempt
    /// update was checked against the STORED actor only, so any principal
    /// holding update_job on the packet could rewrite another actor's
    /// attempt (not_started back to sending, say) while the record still
    /// named the original actor: the write was misattributed. The update
    /// must be made by the actor the record names, as authenticated now.
    #[test]
    fn a_same_id_attempt_update_is_bound_to_the_authenticated_actor_too() {
        let id = Uuid::new_v4();
        let stored = json!({"id": id, "actor": "agent-original", "state": "sending"});
        let written = BTreeSet::from([id]);
        let update = attempt_command(
            Some(stored.clone()),
            json!({"id": id, "actor": "agent-original", "state": "not_started"}),
        );
        assert_eq!(
            validate_actor_bindings(&update, "agent-original"),
            Ok(()),
            "the owner's own no-child evidence still lands"
        );
        assert_eq!(
            refuse_a_written_attempt_id(&update, &written),
            Ok(()),
            "continuing the pinned attempt's own id is not a reuse"
        );
        let refused = validate_actor_bindings(&update, "agent-other")
            .expect_err("another principal rewrote an attempt that names agent-original");
        assert!(refused.contains("gate_launch_attempt.actor"), "{refused}");
        // Nor can the other principal relabel the attempt as its own.
        let relabelled = attempt_command(
            Some(stored),
            json!({"id": id, "actor": "agent-other", "state": "not_started"}),
        );
        assert!(validate_actor_bindings(&relabelled, "agent-other").is_err());
    }

    /// WHY (backlog 0471152b, reviewer run 5426da05 of car b34f114c).
    /// agent-other replaced attempt X (agent-fixture's) with Y naming
    /// itself, then posted X back pinned on Y and got 200: X took the
    /// "new id" branch and bound to the caller, so one attempt id named
    /// two actors while any native-create evidence kept for X named the
    /// first. An id this packet's door ever committed is not fresh, and
    /// naming the stored attempt as `previous` does not make it so.
    #[test]
    fn a_replaced_attempt_id_posted_back_under_a_new_actor_is_refused() {
        let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
        let written = BTreeSet::from([x, y]);
        let stored_y =
            json!({"id": y, "actor": "agent-other", "previous": x, "state": "not_started"});
        for reposted in [
            json!({"id": x, "actor": "agent-other", "state": "sending"}),
            json!({"id": x, "actor": "agent-other", "previous": y, "state": "sending"}),
            // An id is a uuid, not its spelling: the upper-case form of X
            // is X, so it cannot pass as fresh either.
            json!({"id": x.to_string().to_uppercase(), "actor": "agent-other",
                   "previous": y, "state": "sending"}),
        ] {
            let command = attempt_command(Some(stored_y.clone()), reposted.clone());
            let refused = refuse_a_written_attempt_id(&command, &written)
                .expect_err("a replaced attempt id came back under a new actor");
            // It names the reused id AND the pinned attempt it would have
            // replaced (review f6890829 N3).
            assert!(
                refused.contains("gate_launch_attempt.id")
                    && refused.contains(&x.to_string())
                    && refused.contains(&y.to_string()),
                "the refusal names the reused id and the pinned attempt: {refused}"
            );
        }
        // The same packet's genuinely fresh replacement still lands.
        let fresh = attempt_command(
            Some(stored_y),
            json!({"id": Uuid::new_v4(), "actor": "agent-other", "previous": y, "state": "sending"}),
        );
        assert_eq!(refuse_a_written_attempt_id(&fresh, &written), Ok(()));
        assert_eq!(validate_actor_bindings(&fresh, "agent-other"), Ok(()));
    }

    /// A replacement names the attempt it replaces, so the row's chain is
    /// a fact the server checked rather than a field it stored; a first
    /// attempt names none.
    #[test]
    fn a_replacement_names_the_stored_attempt_as_its_previous() {
        let y = Uuid::new_v4();
        let stored = json!({"id": y, "actor": "agent-a", "state": "not_started"});
        for previous in [Value::Null, json!(Uuid::new_v4())] {
            let command = attempt_command(
                Some(stored.clone()),
                json!({"id": Uuid::new_v4(), "actor": "agent-a", "previous": previous}),
            );
            let refused = validate_actor_bindings(&command, "agent-a")
                .expect_err("a replacement that does not name the stored attempt");
            assert!(
                refused.contains("gate_launch_attempt.previous"),
                "{refused}"
            );
        }
        let first = attempt_command(None, json!({"id": Uuid::new_v4(), "actor": "agent-a"}));
        assert_eq!(validate_actor_bindings(&first, "agent-a"), Ok(()));
        let orphaned = attempt_command(
            None,
            json!({"id": Uuid::new_v4(), "actor": "agent-a", "previous": y}),
        );
        assert!(validate_actor_bindings(&orphaned, "agent-a").is_err());
    }

    /// N2 of the same review: the actor check read the row BEFORE the
    /// transaction while the compare-and-set compared inside it. The
    /// check now judges the command's own `expected` attempt, which is
    /// the value the compare-and-set holds the row to, so an attempt
    /// write that pins no attempt (nor its absence) is refused.
    #[test]
    fn an_attempt_write_is_judged_on_the_value_its_compare_and_set_compares() {
        let id = Uuid::new_v4();
        let mut unpinned = attempt_command(None, json!({"id": id, "actor": "agent-a"}));
        unpinned.expected.clear();
        unpinned.expected.insert(
            "other".into(),
            Expected {
                present: false,
                value: Value::Null,
            },
        );
        let refused = validate_actor_bindings(&unpinned, "agent-a")
            .expect_err("an attempt write the compare-and-set does not hold");
        assert!(refused.contains("expected"), "{refused}");
        // The stored actor the check judges is the PINNED one: pinned on
        // an attempt naming agent-a, agent-b's same-id write is refused
        // whatever the row held when the handler read it.
        let pinned = attempt_command(
            Some(json!({"id": id, "actor": "agent-a", "state": "sending"})),
            json!({"id": id, "actor": "agent-b", "state": "not_started"}),
        );
        assert!(validate_actor_bindings(&pinned, "agent-b").is_err());
    }

    fn attempt_command(stored: Option<Value>, next: Value) -> Command {
        Command {
            contract_version: 2,
            idempotency_key: Uuid::new_v4(),
            expected: std::collections::BTreeMap::from([(
                ATTEMPT.to_owned(),
                Expected {
                    present: stored.is_some(),
                    value: stored.unwrap_or(Value::Null),
                },
            )]),
            patch: serde_json::Map::from_iter([(ATTEMPT.to_owned(), next)]),
            immutable_keys: vec![],
            guarded_keys: vec![],
        }
    }

    #[test]
    fn the_attempt_ids_written_are_read_from_every_receipt_patch() {
        let (x, y) = (Uuid::new_v4(), Uuid::new_v4());
        let receipt = |patch: Value| crate::conditional_metadata::Receipt {
            contract_version: 2,
            job_id: test_job().id,
            actor: "agent-a".into(),
            command: Command {
                patch: serde_json::from_value(patch).unwrap(),
                ..attempt_command(None, json!({}))
            },
            applied_at: chrono::Utc::now(),
            event_id: Uuid::new_v4(),
            receipt_event_id: None,
            job: test_job(),
        };
        let receipts = [
            receipt(json!({ATTEMPT: {"id": x}})),
            receipt(json!({KEY: {"generation": Uuid::new_v4()}})),
            receipt(json!({ATTEMPT: {"id": y.to_string().to_uppercase()}})),
        ];
        assert_eq!(attempt_ids_written(&receipts), BTreeSet::from([x, y]));
    }

    #[test]
    fn authored_gate_protocol_declares_all_native_first_records() {
        let workflows =
            crate::seed_loader::load_workflows(crate::registry::platform_bundle_path()).unwrap();
        let gate = workflows.iter().find(|row| row.kind == "gate-run").unwrap();
        assert_eq!(
            gate.metadata_schema.get("immutable_fields"),
            Some(&json!(FIRST_RECORD_FIELDS)),
            "the authored protocol must carry the first-write contract required by launchers"
        );
    }
    fn at(s: &str) -> DateTime<Utc> {
        instant(s).unwrap()
    }
    #[test]
    fn valid_legacy_resume_keeps_bytes_but_expired_resume_does_not_revive_age() {
        let old = json!({"queued_at":"2026-10-15T12:36:47+00:00","queue_heartbeat_at":"2026-10-15T12:38:48Z"});
        let resumed = renew(&old, at("2026-10-15T12:41:19Z")).unwrap();
        assert_eq!(resumed.patch[KEY]["queued_at"], old["queued_at"]);
        assert_eq!(resumed.patch[KEY]["heartbeat_at"], "2026-10-15T12:41:19Z");
        let expired = renew(&old, at("2026-10-15T12:45:19Z")).unwrap();
        assert_eq!(expired.patch[KEY]["queued_at"], "2026-10-15T12:45:19Z");
    }
    #[test]
    fn stale_consume_cannot_delete_a_replacement_and_legacy_overlay_is_not_authority() {
        let first = renew(&json!({}), at("2026-10-15T12:00:00Z")).unwrap();
        let mut md = Value::Object(first.patch);
        let generation = read(&md).unwrap().unwrap().generation;
        let stale = consume(&md, generation).unwrap();
        md["queued_at"] = json!("2020-01-01T00:00:00Z");
        assert_eq!(queued_at(&md), Some("2026-10-15T12:00:00Z"));
        let replacement = renew(&md, at("2026-10-15T13:00:00Z")).unwrap();
        let job = boss_core::job::Job {
            metadata: Value::Object(replacement.patch),
            ..test_job()
        };
        assert!(!stale.matches(&job));
    }
    fn test_job() -> boss_core::job::Job {
        use boss_core::job::{Job, JobStatus, Priority, Subject};
        Job {
            status: JobStatus::Open,
            ..Job::new(
                "gate-run",
                Subject::new("custom", "test"),
                "test",
                "test",
                Priority::Standard,
                at("2026-10-15T12:00:00Z").date_naive(),
            )
        }
    }
}
