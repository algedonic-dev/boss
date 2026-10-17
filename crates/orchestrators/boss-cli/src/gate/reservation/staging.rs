//! Explicit rollout selection. Errors never select a less protected writer.
use super::*;

pub(super) async fn policy_on(doors: &Doors<'_>) -> Result<Value> {
    let row = api(
        doors,
        reqwest::Method::GET,
        "/api/delivery/policy/train-conductor",
        None,
    )
    .await?
    .context("gate queue ownership policy is unavailable")?;
    let row = row.get("data").unwrap_or(&row).clone();
    row.get("gate_queue_owner")
        .and_then(Value::as_str)
        .context("gate_queue_owner is absent or malformed; compatible API rollout is required")?;
    let parsed: boss_jobs::delivery::DeliveryPolicyRow = serde_json::from_value(row.clone())?;
    if parsed.name != "train-conductor" || parsed.version <= 0 {
        bail!("gate queue policy identity is invalid");
    }
    boss_jobs::delivery::check_policy(&parsed).map_err(anyhow::Error::msg)?;
    let pinned = api(
        doors,
        reqwest::Method::GET,
        &format!(
            "/api/delivery/policy/train-conductor/versions/{}",
            parsed.version
        ),
        None,
    )
    .await?
    .context("pinned gate queue policy is unavailable")?;
    if pinned.get("data").unwrap_or(&pinned) != &row {
        bail!("gate queue active and pinned whole policy differ");
    }
    Ok(row)
}

fn fields(schema: &Value, name: &str) -> Result<Vec<String>> {
    let Some(value) = schema.get(name) else {
        return Ok(vec![]);
    };
    let mut names = std::collections::HashSet::new();
    value
        .as_array()
        .context("gate protocol field declaration is not an array")?
        .iter()
        .map(|v| {
            let name = v
                .as_str()
                .filter(|s| !s.is_empty())
                .context("gate protocol field name is malformed")?;
            if !names.insert(name) {
                bail!("gate protocol field declaration repeats {name}");
            }
            Ok(name.to_owned())
        })
        .collect()
}

pub(super) async fn legacy_on(doors: &Doors<'_>, row: &Value, policy: &Value) -> Result<bool> {
    let legacy = protocol_legacy_on(doors, row).await?;
    if legacy && policy.get("gate_queue_owner").and_then(Value::as_str) != Some("legacy") {
        bail!("resident ownership requires the modern published gate protocol");
    }
    Ok(legacy)
}

pub(super) async fn protocol_legacy_on(doors: &Doors<'_>, row: &Value) -> Result<bool> {
    let version = row
        .get("workflow_version")
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .context("gate packet lacks a valid pinned protocol")?;
    if row.get("kind").and_then(Value::as_str) != Some("gate-run") {
        bail!("not a gate-run packet");
    }
    let spec = api(
        doors,
        reqwest::Method::GET,
        &format!("/api/workflows/gate-run/versions/{version}"),
        None,
    )
    .await?
    .context("pinned gate protocol is unavailable")?;
    if spec.get("kind").and_then(Value::as_str) != Some("gate-run")
        || spec.get("version").and_then(Value::as_i64) != Some(version)
        || !matches!(
            spec.get("status").and_then(Value::as_str),
            Some("active" | "retired")
        )
    {
        bail!("gate protocol identity or published status is invalid");
    }
    let schema = spec
        .get("metadata_schema")
        .filter(|v| v.is_object())
        .context("gate protocol metadata schema is malformed")?;
    let immutable = fields(schema, "immutable_fields")?;
    let guarded = fields(schema, "guarded_fields")?;
    let conditional = fields(schema, "conditional_fields")?;
    let modern = [
        model::KEY,
        model::INTENT,
        model::OBSERVATION,
        model::SUBMISSION,
        model::ATTEMPT,
        "gate_orphan_claim",
    ];
    let any = immutable
        .iter()
        .chain(&guarded)
        .chain(&conditional)
        .any(|f| modern.contains(&f.as_str()));
    if any {
        require_protocol(doors, row).await?;
        return Ok(false);
    }
    let md = row
        .get("metadata")
        .and_then(Value::as_object)
        .context("gate metadata is malformed")?;
    if modern
        .iter()
        .chain([&"_immutable_metadata", &"_guarded_metadata"])
        .any(|key| md.contains_key(*key))
    {
        bail!("legacy pinned gate contains modern markers or bindings; no writer fallback");
    }
    Ok(true)
}
