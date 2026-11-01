//! Guarded negative decisions share the native sender arbitration.
use super::*;
use boss_jobs::conditional_metadata::Expected;
use std::collections::BTreeMap;

pub(super) async fn claim_on(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    verdict_receipt: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let row = read_on(http, packet).await?;
    if let Some(why) = launch_target_refusal(packet, &row) {
        bail!(why);
    }
    require_protocol(http, &row).await?;
    let md = row
        .get("metadata")
        .context("negative claim target lacks metadata")?;
    model::source_metadata(md).map_err(anyhow::Error::msg)?;
    if let Some(original) = md.get("gate_orphan_claim") {
        if md.pointer("/_immutable_metadata/gate_orphan_claim") != Some(original)
            || original.get("contract_version").and_then(Value::as_u64) != Some(1)
            || original.get("packet").and_then(Value::as_str) != Some(packet)
            || original.get("namespace").and_then(Value::as_str) != Some(namespace)
            || original.get("verdict_receipt") != Some(verdict_receipt)
        {
            bail!(
                "negative claim is unbound or belongs to another exact decision; original retained"
            );
        }
        return Ok(());
    }
    if md.get(model::OBSERVATION).is_some() {
        bail!("native acceptance is already recorded; no negative claim");
    }
    let previous = attempt::read(md)?;
    match (md.get(model::INTENT), previous) {
        (None, None) => {}
        (Some(value), Some(previous)) if previous.state == attempt::State::NotStarted => {
            if md.pointer("/_immutable_metadata/gate_launch_intent") != Some(value) {
                bail!("no-child intent lacks its immutable binding");
            }
            let intent: LaunchIntent = serde_json::from_value(value.clone())?;
            if intent.packet != packet || intent.namespace != namespace {
                bail!("no-child intent belongs to another target");
            }
            verify_previous_no_child(http, &intent, &previous).await?;
        }
        _ => bail!(
            "native outcome UNKNOWN: a possibly effective attempt cannot acquire a negative claim"
        ),
    }
    let census = native_census_on(
        http,
        namespace,
        &format!("boss.dev/packet={packet}"),
        packet,
    )
    .await?;
    if !census.list["items"]
        .as_array()
        .context("negative census lacks full items")?
        .is_empty()
    {
        bail!(
            "a retained native Job exists; no protected negative decision or deletion is authorized"
        );
    }
    let id = Uuid::new_v4();
    let actor = submission::canonical_actor(http, packet).await?;
    let claim = json!({"contract_version":1,"id":id,"packet":packet,"actor":actor,
        "namespace":namespace,"observed_at":now,"verdict_receipt":verdict_receipt,
        "native_census":{"file_id":census.artifact.file_id,"sha256":census.artifact.sha256,"size_bytes":census.artifact.size_bytes}});
    let command = negative_command(md, id, &claim)?;
    let receipt = apply_on(http, packet, &command).await?;
    if receipt.actor != actor
        || receipt.event_id.is_nil()
        || receipt.receipt_event_id.is_none_or(|id| id.is_nil())
        || receipt.job.metadata.get("gate_orphan_claim") != Some(&claim)
        || receipt
            .job
            .metadata
            .pointer("/_immutable_metadata/gate_orphan_claim")
            != Some(&claim)
    {
        bail!(
            "negative claim has no exact original committed receipt; no negative effect authorized"
        );
    }
    Ok(())
}

/// The guarded write that records a negative claim: bind
/// `gate_orphan_claim` on the condition that the state the claim was
/// judged against has not moved.
///
/// WHAT IT CONDITIONS ON (backlog 011560bb). Every key that can still
/// move: the six the claim arbitrates on, absent or exact, and every
/// other present key exactly — a beat refreshed between this read and
/// the write still refuses it. It leaves out only what the server
/// already holds still: a key bound in `_immutable_metadata` with this
/// very value, which no later write may change or remove
/// (`metadata_immutability::preserve`), and the binding record itself,
/// which grows only by the first write of a field the gate-run protocol
/// declares immutable — `gate_submission`, `gate_launch_intent`,
/// `gate_launch_observation`, `gate_orphan_claim`, all four among the six,
/// each conditioned absent while absent. Expecting those values guarded
/// nothing and cost the bus bound: the receipt event already carries
/// the whole job, so a claim that also expected every key carried a
/// resident packet's submission four times, and gate-run dec70bf9
/// (~500 KB, 2026-10-19) staged an event over the 960 KiB bound and
/// could be neither withdrawn nor settled through this guard.
fn negative_command(md: &Value, id: Uuid, claim: &Value) -> Result<Command> {
    let fields = md
        .as_object()
        .context("negative claim metadata is not an object")?;
    let bound = md.get(boss_jobs::metadata_immutability::KEY);
    let held_still = |key: &str| {
        key == boss_jobs::metadata_immutability::KEY
            || bound
                .and_then(|bindings| bindings.get(key))
                .is_some_and(|value| fields.get(key) == Some(value))
    };
    let mut expected = BTreeMap::new();
    for key in [
        model::KEY,
        model::SUBMISSION,
        model::INTENT,
        model::OBSERVATION,
        attempt::KEY,
        "gate_orphan_claim",
    ] {
        if !fields.contains_key(key) {
            expected.insert(
                key.into(),
                Expected {
                    present: false,
                    value: Value::Null,
                },
            );
        }
    }
    for (key, value) in fields {
        if !held_still(key) {
            expected.insert(
                key.clone(),
                Expected {
                    present: true,
                    value: value.clone(),
                },
            );
        }
    }
    Ok(Command {
        contract_version: 2,
        idempotency_key: id,
        expected,
        patch: serde_json::Map::from_iter([("gate_orphan_claim".into(), claim.clone())]),
        immutable_keys: vec!["gate_orphan_claim".into()],
        guarded_keys: vec![],
    })
}

/// Older pinned protocols retain their explicit legacy lifecycle. A
/// read failure, partial protocol, or any modern marker cannot select it.
pub(super) async fn guard_on(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    receipt: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let row = read_on(http, packet).await?;
    if staging::protocol_legacy_on(http, &row).await? {
        eprintln!(
            "gate settlement {packet}: explicit whole legacy pinned protocol; no modern reservation is adopted"
        );
        return Ok(());
    }
    claim_on(http, packet, namespace, receipt, now).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Job, JobStatus, Priority, Subject};

    /// What the bus can carry, as the outbox measures it before staging.
    const BOUND: usize = boss_events::outbox::MAX_STAGED_EVENT_BYTES;

    /// dec70bf9's shape (read live 2026-10-19, ~500 KB): a resident gate
    /// packet whose submission, bound immutable, the row holds twice — at
    /// its own key and again in `_immutable_metadata` — with the source
    /// reads riding inside it.
    fn resident(reads: usize) -> Job {
        let submission = json!({"contract_version":1,"command":Uuid::nil(),
            "submitted_by":"agent-claude","supervisor":"automation:train-conductor",
            "intent":{"packet":"fixture","head":"0".repeat(40)},
            "source":{"branch":"fix/oversized","sha":"0".repeat(40),
                "gate_source_reads":["r".repeat(reads)]}});
        let mut job = Job::new(
            "gate-run",
            Subject::new("branch", "fix/oversized"),
            "Gate: fix/oversized",
            "agent-claude",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
        );
        job.status = JobStatus::Open;
        job.metadata = json!({
            model::KEY: {"contract_version":1,"consumed":false,"generation":Uuid::nil(),
                "heartbeat_at":"2026-10-19T05:41:50Z","queued_at":"2026-10-19T05:41:50Z"},
            model::SUBMISSION: submission,
            boss_jobs::metadata_immutability::KEY: {model::SUBMISSION: submission},
            "branch":"fix/oversized","sha":"0".repeat(40),
            "queued_at":"2026-10-19T05:41:50Z","queue_heartbeat_at":"2026-10-19T05:41:50Z",
            "park_summary":"s".repeat(1_200),
        });
        job
    }

    /// The two events one guarded write stages, encoded as the bus would.
    fn staged(job: &Job, command: &Command) -> Vec<usize> {
        let stamp = boss_core::publisher::EventStamp::new(
            "boss-jobs",
            "automation:train-conductor".parse().unwrap(),
        );
        let after = command.apply(job).unwrap();
        let (_, events) =
            boss_jobs::conditional_metadata::recorded_pair(&stamp, command, after).unwrap();
        events
            .iter()
            .map(|event| serde_json::to_vec(event).unwrap().len())
            .collect()
    }

    /// Backlog 011560bb: every guarded write already carries the whole job
    /// in its receipt, so a negative claim that ALSO expected every key
    /// carried it twice, and gate-run dec70bf9 (~500 KB, never launched)
    /// could be neither withdrawn nor settled through this guard.
    #[test]
    fn a_gate_packet_over_half_the_bus_bound_can_still_be_settled() {
        let job = resident(300_000);
        let size = serde_json::to_vec(&job).unwrap().len();
        assert!(
            size > BOUND / 2 && size < BOUND,
            "the fixture must sit over half the bound and under it: {size}"
        );
        let claim = json!({"contract_version":1,"verdict_receipt":{"verdict":"lost"}});
        let command = negative_command(&job.metadata, Uuid::new_v4(), &claim).unwrap();
        command.validate().unwrap();
        assert!(
            command.matches(&job),
            "the claim is judged against this state"
        );
        for bytes in staged(&job, &command) {
            assert!(
                bytes <= BOUND,
                "a {size}-byte packet stages a {bytes}-byte event, over the {BOUND}-byte bound"
            );
        }
    }

    /// The guard stays a real precondition: anything that can still move
    /// on the packet between the claim's read and its write refuses it.
    #[test]
    fn the_negative_claim_still_refuses_a_state_that_moved() {
        let job = resident(1_000);
        let claim = json!({"contract_version":1,"verdict_receipt":{"verdict":"lost"}});
        let command = negative_command(&job.metadata, Uuid::new_v4(), &claim).unwrap();
        assert!(command.matches(&job));
        for (key, value) in [
            ("queue_heartbeat_at", json!("2026-10-19T05:42:50Z")),
            ("sha", json!("1".repeat(40))),
            (model::KEY, json!({"consumed":true})),
            (model::INTENT, json!({"packet":"fixture"})),
            (model::OBSERVATION, json!({"uid":"u"})),
            (attempt::KEY, json!({"state":"sending"})),
            ("gate_orphan_claim", json!({"id":"another"})),
        ] {
            let mut moved = job.clone();
            moved.metadata[key] = value;
            assert!(!command.matches(&moved), "{key} moved and the claim held");
        }
        let mut gone = job.clone();
        gone.metadata
            .as_object_mut()
            .unwrap()
            .remove("queue_heartbeat_at");
        assert!(!command.matches(&gone), "a removed key moved the state");
        let mut closed = job.clone();
        closed.status = JobStatus::Closed;
        assert!(!command.matches(&closed));
    }
}
