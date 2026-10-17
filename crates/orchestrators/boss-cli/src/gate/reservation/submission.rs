//! Durable request ownership. This record conveys intent, never a send
//! permit. A later resident must originate its own guarded sending claim.
use super::*;
use boss_jobs::conditional_metadata::Expected;
use std::collections::BTreeMap;

pub(super) const KEY: &str = model::SUBMISSION;
use model::Submission;

fn source_keys() -> Vec<String> {
    [
        "branch",
        "sha",
        "agent_run",
        "mode",
        "hold",
        "runner",
        "worktree",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(
        ParkIntent::clear_patch()
            .as_object()
            .into_iter()
            .flat_map(|fields| fields.keys().cloned())
            .collect::<Vec<_>>(),
    )
    .collect()
}

pub(super) fn canonical_actor(http: &Doors<'_>, packet: &str) -> Result<String> {
    let actor = identity::apply(identity::signature_for(
        &reqwest::Method::POST,
        &format!("/api/jobs/{packet}/metadata/conditional"),
        http.caller.clone(),
    ))?;
    Ok(match boss_policy_client::types::automation_slug(&actor) {
        Some(slug) => boss_core::actor::ActorId::Automation(slug.into()).to_string(),
        None => actor.parse::<boss_core::actor::ActorId>()?.to_string(),
    })
}

pub(super) async fn submit_on(
    http: &Doors<'_>,
    intent: &LaunchIntent,
    overrides: &Value,
) -> Result<Receipt> {
    let row = read_on(http, &intent.packet).await?;
    if let Some(why) = launch_target_refusal(&intent.packet, &row) {
        bail!(why);
    }
    require_protocol(http, &row).await?;
    let md = row
        .get("metadata")
        .context("submission packet lacks metadata")?;
    if md.get(KEY).is_some() {
        bail!("durable submission already exists; reattach by packet without writing");
    }
    if md.get(model::INTENT).is_some()
        || md.get(attempt::KEY).is_some()
        || md.get("gate_orphan_claim").is_some()
    {
        bail!("prior native intent/attempt/settlement prevents a new durable submission");
    }
    let mut patch = overrides
        .as_object()
        .context("submission source patch is not an object")?
        .clone();
    let policy = match patch.get("gate_queue_policy") {
        Some(policy) => policy.clone(),
        None => staging::policy_on(http).await?,
    };
    policy
        .get("gate_queue_owner")
        .and_then(Value::as_str)
        .context("original gate_queue_owner is absent or malformed")?;
    let policy_row: boss_jobs::delivery::DeliveryPolicyRow =
        serde_json::from_value(policy.clone())?;
    if policy_row.name != "train-conductor" || policy_row.version <= 0 {
        bail!("original gate queue policy identity is invalid");
    }
    boss_jobs::delivery::check_policy(&policy_row).map_err(anyhow::Error::msg)?;
    patch.insert("gate_queue_policy".into(), policy);
    patch
        .entry("gate_queue_selection")
        .or_insert_with(|| json!("explicit-submit"));
    let mut current = md.clone();
    let fields = current
        .as_object_mut()
        .context("submission metadata is not an object")?;
    for (key, value) in &patch {
        if value.is_null() {
            fields.remove(key);
        } else {
            fields.insert(key.clone(), value.clone());
        }
    }
    let command = Uuid::new_v4();
    let mut expected = BTreeMap::new();
    for key in source_keys() {
        expected.insert(
            key.clone(),
            Expected {
                present: md.get(&key).is_some(),
                value: md.get(&key).cloned().unwrap_or(Value::Null),
            },
        );
    }
    for key in patch.keys() {
        expected.entry(key.clone()).or_insert_with(|| Expected {
            present: md.get(key).is_some(),
            value: md.get(key).cloned().unwrap_or(Value::Null),
        });
    }
    for key in [
        KEY,
        model::KEY,
        model::INTENT,
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
    let mut source = current
        .as_object()
        .cloned()
        .context("source metadata is not an object")?;
    for key in [
        model::KEY,
        KEY,
        model::INTENT,
        model::OBSERVATION,
        attempt::KEY,
        "gate_orphan_claim",
        "queued_at",
        "queue_heartbeat_at",
        "launching_at",
        "launched_at",
        boss_jobs::metadata_immutability::KEY,
        boss_jobs::metadata_immutability::GUARDED_KEY,
    ] {
        source.remove(key);
    }
    source.remove("gate_source_claim");
    source.remove("gate_source_reads");
    let mut source_reads = Vec::new();
    let qualified: Result<Option<Value>> = async {
        let original =
            source_claim::read_claim(http, &Value::Object(source.clone()), &mut source_reads)
                .await?;
        if let Some(claim) = &original
            && !source_claim::same_actor(
                http,
                &canonical_actor(http, &intent.packet)?,
                claim["holder"]
                    .as_str()
                    .context("original source holder is unreadable")?,
                &mut source_reads,
            )
            .await?
        {
            bail!("requesting actor cannot borrow another worker's source run");
        }
        Ok(original)
    }
    .await;
    let original_claim = match qualified {
        Ok(claim) => claim,
        Err(error) => {
            let evidence=retain(http,&intent.packet,&serde_json::to_vec(&json!({
                "operation":"source-claim-admission-refusal","reads":source_reads,"error":format!("{error:#}")
            }))?).await?;
            return Err(error.context(format!(
                "complete source qualification retained as {} sha256 {}",
                evidence.file_id, evidence.sha256
            )));
        }
    };
    if let Some(claim) = original_claim {
        source.insert("gate_source_claim".into(), claim);
    }
    source.insert("gate_source_reads".into(), json!(source_reads));
    let submission = Submission {
        contract_version: 1,
        command,
        submitted_by: canonical_actor(http, &intent.packet)?,
        supervisor: identity::CONDUCTOR.into(),
        intent: intent.clone(),
        source,
    };
    // These facts have no legacy consumer. Keep one authoritative copy.
    for key in [
        "gate_queue_policy",
        "gate_queue_selection",
        "force_regate_reason",
    ] {
        patch.remove(key);
    }
    patch.insert(KEY.into(), serde_json::to_value(&submission)?);
    let command = Command {
        contract_version: 2,
        idempotency_key: command,
        expected,
        patch,
        immutable_keys: vec![KEY.into()],
        guarded_keys: vec![],
    };
    let receipt = apply_on(http, &intent.packet, &command).await?;
    verify_receipt(&submission, &receipt)?;
    Ok(receipt)
}

fn verify_receipt(submission: &Submission, receipt: &Receipt) -> Result<()> {
    let original = model::acceptance(&receipt.job, receipt).map_err(anyhow::Error::msg)?;
    if submission.supervisor != identity::CONDUCTOR
        || serde_json::to_value(submission)? != serde_json::to_value(original)?
    {
        bail!("durable submission lacks its exact original committed acceptance receipt");
    }
    Ok(())
}

/// Read-only admission time and immutable intent. No POST, heartbeat,
/// claim, native command or current process identity can establish it.
pub(super) async fn original_on(http: &Doors<'_>, packet: &str) -> Result<(Submission, Receipt)> {
    let row = read_on(http, packet).await?;
    let submission = model::submission(row.get("metadata").context("packet has no metadata")?)
        .map_err(anyhow::Error::msg)?
        .context("packet has no durable submission; legacy ownership remains explicit")?;
    let mut url = reqwest::Url::parse(&format!(
        "{}/api/jobs/{packet}/metadata/conditional/{}",
        http.jobs_base, submission.command
    ))?;
    url.query_pairs_mut()
        .append_pair("actor", &submission.submitted_by);
    let path = format!("{}?{}", url.path(), url.query().unwrap_or_default());
    let receipt: Receipt = serde_json::from_value(
        api(http, reqwest::Method::GET, &path, None)
            .await?
            .context("original durable acceptance receipt is unavailable")?,
    )?;
    verify_receipt(&submission, &receipt)?;
    let current: boss_core::job::Job = serde_json::from_value(row)?;
    model::acceptance(&current, &receipt).map_err(anyhow::Error::msg)?;
    Ok((submission, receipt))
}

/// Every path capable of recording native acceptance or spending a
/// permit uses the same immutable resident declaration.
pub(super) async fn require_owner_on(
    http: &Doors<'_>,
    packet: &str,
    metadata: &Value,
) -> Result<Option<Submission>> {
    if metadata.get(KEY).is_none() {
        return Ok(None);
    }
    model::source_metadata(metadata).map_err(anyhow::Error::msg)?;
    let (submission, _) = original_on(http, packet).await?;
    if canonical_actor(http, packet)? != submission.supervisor {
        bail!("only the declared resident supervisor may originate or consume this native launch");
    }
    Ok(Some(submission))
}

pub(super) async fn launch_on(
    http: &Doors<'_>,
    packet: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let (submission, _receipt) = original_on(http, packet).await?;
    if canonical_actor(http, packet)? != submission.supervisor {
        bail!("only the declared resident supervisor may originate this launch");
    }
    let row = read_on(http, packet).await?;
    let md = row
        .get("metadata")
        .context("submitted packet lost its metadata")?;
    model::source_metadata(md).map_err(anyhow::Error::msg)?;
    let mut intent = submission.intent;
    intent.transition_generation = Some(reservation_generation(md)?);
    let bytes = request_bytes(http, &intent).await?;
    create_on(http, &intent, &bytes, now).await
}
