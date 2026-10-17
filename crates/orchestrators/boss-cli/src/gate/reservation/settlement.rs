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
    let mut expected = BTreeMap::new();
    for key in [
        model::KEY,
        model::SUBMISSION,
        model::INTENT,
        model::OBSERVATION,
        attempt::KEY,
        "gate_orphan_claim",
    ] {
        expected.insert(
            key.into(),
            Expected {
                present: md.get(key).is_some(),
                value: md.get(key).cloned().unwrap_or(Value::Null),
            },
        );
    }
    for key in md
        .as_object()
        .context("negative claim metadata is not an object")?
        .keys()
    {
        expected.entry(key.clone()).or_insert_with(|| Expected {
            present: true,
            value: md[key].clone(),
        });
    }
    let id = Uuid::new_v4();
    let actor = submission::canonical_actor(http, packet)?;
    let claim = json!({"contract_version":1,"id":id,"packet":packet,"actor":actor,
        "namespace":namespace,"observed_at":now,"verdict_receipt":verdict_receipt,
        "native_census":{"file_id":census.artifact.file_id,"sha256":census.artifact.sha256,"size_bytes":census.artifact.size_bytes}});
    let command = Command {
        contract_version: 2,
        idempotency_key: id,
        expected,
        patch: serde_json::Map::from_iter([("gate_orphan_claim".into(), claim.clone())]),
        immutable_keys: vec!["gate_orphan_claim".into()],
        guarded_keys: vec![],
    };
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
