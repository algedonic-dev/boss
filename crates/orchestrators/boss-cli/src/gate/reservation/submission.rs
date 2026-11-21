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

pub(super) async fn canonical_actor(http: &Doors<'_>, packet: &str) -> Result<String> {
    let actor = identity::apply(identity::signature_for(
        &reqwest::Method::POST,
        &format!("/api/jobs/{packet}/metadata/conditional"),
        http.caller.clone(),
    ))?;
    let actor = resolve_signer(&actor, registry_rows(http).await)?;
    Ok(match boss_policy_client::types::automation_slug(&actor) {
        Some(slug) => boss_core::actor::ActorId::Automation(slug.into()).to_string(),
        None => actor.parse::<boss_core::actor::ActorId>()?.to_string(),
    })
}

/// The agents registry, whole or refused: a page shorter than its own
/// `total` is not the registry.
async fn registry_rows(http: &Doors<'_>) -> Result<Vec<Value>> {
    let registry = api(http, reqwest::Method::GET, "/api/agents", None)
        .await?
        .context("canonical actor registry is unavailable")?;
    let total = registry
        .get("total")
        .and_then(Value::as_u64)
        .context("canonical actor registry lacks its full total")?;
    let agents = rows(Some(registry))?;
    if agents.len() as u64 != total {
        bail!("canonical actor registry is incomplete");
    }
    Ok(agents)
}

/// The actor a signer's name resolves to, the way the server's write
/// middleware (`validate_actor_bindings`) resolves it: through the
/// registry, for ANY alias, not only one carrying `@` (backlog 3f5dde02,
/// review 7e8a2723 N1).
/// - A login carrying `@` must resolve: an asserted email is not the actor
///   its original receipt records, so an unreadable registry, or a login no
///   single row names, refuses. The jobs API's login door still ADMITS such
///   a login while its migration window is open, so the refusal says so and
///   names the door that adds the row, rather than leaving the operator to
///   re-derive why the gate refuses what the API took (review 7e8a2723 N2,
///   held by review 6eb83ad0 P2). Only the message changed; the
///   refusal stays.
/// - Any other name resolves when exactly one row names it. A name no row
///   names, or a registry that cannot be read, passes as signed. The server
///   is the judge of that, and its refusal (409) writes nothing, so passing
///   is never a guess written down.
pub(super) fn resolve_signer(actor: &str, registry: Result<Vec<Value>>) -> Result<String> {
    if actor.contains('@') {
        let agents = registry.context("canonical actor registry is unavailable")?;
        return crate::dispatch::resolve_agent(&agents, actor).with_context(|| {
            format!(
                "canonical actor: no single agents registry row names the login {actor}. \
                 The jobs API still admits an unrowed login while its migration window is open, \
                 but a gate receipt records the actor, so the gate refuses it. \
                 Add the login to its agent's aliases in the tenant's seeds/agents.toml \
                 and run boss tenant publish (POST /api/agents/batch)"
            )
        });
    }
    Ok(registry
        .ok()
        .and_then(|agents| crate::dispatch::resolve_agent(&agents, actor))
        .unwrap_or_else(|| actor.to_string()))
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
    let actor = canonical_actor(http, &intent.packet).await?;
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
                &actor,
                claim["holder"]
                    .as_str()
                    .context("original source holder is unreadable")?,
                &mut source_reads,
            )
            .await?
        {
            bail!("requesting actor cannot borrow another worker's source run");
        }
        if let Some(claim) = &original {
            source_claim::bind_request(http, claim, &intent.packet, &actor, &mut source_reads)
                .await?;
        }
        Ok(original)
    }
    .await;
    let original_claim = match qualified {
        Ok(claim) => claim,
        Err(error) => {
            let evidence = retain(
                http,
                &intent.packet,
                &serde_json::to_vec(&source_claim::evidence(
                    "source-claim-admission-refusal",
                    &intent.packet,
                    &source_reads,
                    Some(format!("{error:#}")),
                ))?,
            )
            .await?;
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
        submitted_by: actor,
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
    let (submission, receipt, _) = accepted_on(http, packet).await?;
    Ok((submission, receipt))
}

/// [`original_on`] with the packet as it was read for it, so a reader
/// that also judges the packet does not read it twice (backlog 48b1b7b4).
pub(super) async fn accepted_on(
    http: &Doors<'_>,
    packet: &str,
) -> Result<(Submission, Receipt, Value)> {
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
    let current: boss_core::job::Job = serde_json::from_value(row.clone())?;
    model::acceptance(&current, &receipt).map_err(anyhow::Error::msg)?;
    Ok((submission, receipt, row))
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
    if canonical_actor(http, packet).await? != submission.supervisor {
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
    if canonical_actor(http, packet).await? != submission.supervisor {
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

#[cfg(test)]
mod resolve_signer_tests {
    use super::*;

    fn registry() -> Result<Vec<Value>> {
        Ok(vec![
            json!({"id": "agent-claude-b", "aliases": ["claude-b@algedonic.dev", "claude-b"]}),
            json!({"id": "agent-codex", "aliases": ["codex@algedonic.dev"]}),
        ])
    }

    #[test]
    fn a_plain_alias_resolves_through_the_registry_like_a_login() {
        assert_eq!(
            resolve_signer("claude-b", registry()).unwrap(),
            "agent-claude-b"
        );
        assert_eq!(
            resolve_signer("claude-b@algedonic.dev", registry()).unwrap(),
            "agent-claude-b"
        );
    }

    #[test]
    fn an_id_or_an_unnamed_plain_name_passes_as_signed() {
        assert_eq!(
            resolve_signer("agent-codex", registry()).unwrap(),
            "agent-codex"
        );
        assert_eq!(
            resolve_signer("emp-david", registry()).unwrap(),
            "emp-david"
        );
        assert_eq!(
            resolve_signer("emp-david", Err(anyhow::anyhow!("registry unreadable"))).unwrap(),
            "emp-david",
            "a plain name never waits on the registry; the server judges it"
        );
    }

    #[test]
    fn a_login_carrying_at_still_refuses_unresolved() {
        assert!(resolve_signer("nobody@algedonic.dev", registry()).is_err());
        assert!(resolve_signer("claude-b@algedonic.dev", Err(anyhow::anyhow!("down"))).is_err());
    }

    /// Review 7e8a2723 N2, held by review 6eb83ad0 P2: the refusal for a
    /// login no row names stays strict, and says why the jobs API admits
    /// what the gate refuses (the migration window) and which door adds
    /// the row, so the operator is not left to re-derive either.
    #[test]
    fn the_no_row_refusal_names_the_migration_window_and_the_registry_door() {
        let why = format!(
            "{:#}",
            resolve_signer("nobody@algedonic.dev", registry()).unwrap_err()
        );
        for needle in [
            "nobody@algedonic.dev",
            "agents registry",
            "migration window",
            "seeds/agents.toml",
            "boss tenant publish",
        ] {
            assert!(why.contains(needle), "refusal lacks {needle:?}: {why}");
        }
    }
}
