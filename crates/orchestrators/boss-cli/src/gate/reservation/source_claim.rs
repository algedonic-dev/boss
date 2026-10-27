//! Re-read the original registered claim, not the lifetime of its model process.
use super::*;

pub(super) async fn same_actor(
    http: &Doors<'_>,
    left: &str,
    right: &str,
    reads: &mut Vec<Value>,
) -> Result<bool> {
    if left == right {
        return Ok(true);
    }
    let registry = api(http, reqwest::Method::GET, "/api/agents", None)
        .await?
        .context("source holder aliases are unreadable")?;
    reads.push(read_digest("source-holder-registry", &registry));
    let total = registry
        .get("total")
        .and_then(Value::as_u64)
        .context("source holder registry lacks complete total")?;
    let agents = rows(Some(registry))?;
    if total != agents.len() as u64 {
        bail!("source holder registry is incomplete");
    }
    let left = crate::dispatch::resolve_agent(&agents, left)
        .context("source holder is ambiguous or unknown")?;
    let right = crate::dispatch::resolve_agent(&agents, right)
        .context("parent holder is ambiguous or unknown")?;
    Ok(left == right)
}
fn one_step<'a>(job: &'a Value, slug: &str) -> Result<&'a Value> {
    let steps: Vec<_> = crate::envelope::steps(job)
        .into_iter()
        .filter(|step| step.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .collect();
    if steps.len() != 1 {
        bail!("source claim requires exactly one declared {slug} step");
    }
    Ok(steps[0])
}
fn active_holder(step: &Value) -> Result<&str> {
    if step.get("status").and_then(Value::as_str) != Some("active") {
        bail!("source claim step is no longer active");
    }
    step.get("assignee_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("source claim has no current holder")
}

/// The source run read back as the exact agent-run, FINISHED: closed or
/// cancelled, which never reopens (backlog 36352452). Every reader still
/// refuses it with the sentence it always had; the type lets the resident
/// supervisor tell this permanent refusal from a transient one and settle
/// the request instead of retaining it every pass (backlog 599aed02).
#[derive(Debug)]
pub(super) struct SourceRunEnded {
    pub run: String,
    pub status: String,
    pub outcome: Option<String>,
    /// The digest of the run row the refusal was read from.
    pub read: Value,
}
impl std::fmt::Display for SourceRunEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "original source run is not the exact open agent-run: {} is {}",
            self.run, self.status
        )?;
        match &self.outcome {
            Some(outcome) => write!(f, " (outcome {outcome})"),
            None => Ok(()),
        }
    }
}
impl std::error::Error for SourceRunEnded {}

pub(super) async fn read_claim(
    http: &Doors<'_>,
    source: &Value,
    reads: &mut Vec<Value>,
) -> Result<Option<Value>> {
    let Some(value) = source.get("agent_run").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let run = value
        .as_str()
        .filter(|s| s.len() == 36 && Uuid::parse_str(s).is_ok())
        .context("original source run is not a full declared identity")?;
    let row = read_on(http, run).await?;
    let read = read_digest("original-source-run", &row);
    reads.push(read.clone());
    let exact = row.get("id").and_then(Value::as_str) == Some(run)
        && row.get("kind").and_then(Value::as_str) == Some("agent-run");
    let status = row.get("status").and_then(Value::as_str);
    if exact && matches!(status, Some("closed" | "cancelled")) {
        return Err(SourceRunEnded {
            run: run.to_owned(),
            status: status.unwrap_or_default().to_owned(),
            outcome: row
                .pointer("/metadata/outcome")
                .and_then(Value::as_str)
                .map(str::to_owned),
            read,
        }
        .into());
    }
    if !exact || status != Some("open") {
        bail!("original source run is not the exact open agent-run");
    }
    let packet = row
        .pointer("/metadata/packet")
        .and_then(Value::as_str)
        .filter(|s| s.len() == 36 && Uuid::parse_str(s).is_ok())
        .context("original source parent is unreadable")?;
    let slug = row
        .pointer("/metadata/step")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("original source step slug is unreadable")?;
    let building = one_step(&row, "building")?;
    let holder = active_holder(building)?;
    if let Some(expectation) = crate::dispatch_started::expectation(&row) {
        let receipt = row
            .pointer("/metadata/worker_started")
            .context("declared source worker receipt is missing")?;
        let matches = match expectation.get("schema").and_then(Value::as_u64) {
            Some(1) => {
                expectation
                    .get("minutes")
                    .and_then(Value::as_i64)
                    .is_some_and(|n| n > 0)
                    && receipt.get("schema").and_then(Value::as_u64) == Some(1)
                    && receipt.as_object().is_some_and(|fields| fields.len() == 3)
                    && receipt.get("run").and_then(Value::as_str) == Some(run)
            }
            Some(2) => boss_jobs::agent_budget::observer_start_receipt_matches(
                expectation,
                receipt,
                run,
                holder,
            ),
            _ => false,
        };
        if !matches
            || !same_actor(
                http,
                holder,
                receipt
                    .get("actor")
                    .and_then(Value::as_str)
                    .context("source worker receipt actor is unreadable")?,
                reads,
            )
            .await?
        {
            bail!("declared source worker receipt does not bind this run and holder");
        }
    }
    let parent = read_on(http, packet).await?;
    reads.push(read_digest("original-source-parent", &parent));
    if parent.get("id").and_then(Value::as_str) != Some(packet)
        || parent.get("status").and_then(Value::as_str) != Some("open")
    {
        bail!("original source parent is no longer open");
    }
    let step = one_step(&parent, slug)?;
    let parent_holder = active_holder(step)?;
    if !same_actor(http, holder, parent_holder, reads).await? {
        bail!("original parent step belongs to another holder");
    }
    if let Some(receipt) = row.pointer("/metadata/claim_receipt") {
        let original: boss_core::job::Step = serde_json::from_value(receipt.clone())
            .context("declared original source claim receipt is unreadable")?;
        if original.id.to_string() != step.get("id").and_then(Value::as_str).unwrap_or_default()
            || original.job_id.to_string() != packet
            || original.spec_slug.as_deref() != Some(slug)
            || original.status != boss_core::job::StepStatus::Active
            || !same_actor(
                http,
                original
                    .assignee_id
                    .as_deref()
                    .context("original claim receipt has no holder")?,
                holder,
                reads,
            )
            .await?
        {
            bail!(
                "declared original source claim receipt does not bind this parent step and holder"
            );
        }
    }
    let backlink = step.pointer("/metadata/agent_run");
    if backlink.is_some_and(|value| value.as_str() != Some(run)) {
        bail!("original parent step no longer links this source run");
    }
    Ok(Some(
        json!({"contract_version":1,"run":run,"packet":packet,"step":slug,
        "step_id":step.get("id").and_then(Value::as_str).filter(|s|s.len()==36 && Uuid::parse_str(s).is_ok()).context("parent step has no valid identity")?,
        "building_step":building.get("id").and_then(Value::as_str).filter(|s|s.len()==36 && Uuid::parse_str(s).is_ok()).context("source building has no valid identity")?,
        "holder":holder,"backlink":backlink}),
    ))
}

pub(super) async fn verify(
    http: &Doors<'_>,
    source: &Value,
    originator: &str,
    reads: &mut Vec<Value>,
) -> Result<()> {
    let current = read_claim(http, source, reads).await?;
    let original = source.get("gate_source_claim");
    match (original, current) {
        (None, None) => Ok(()),
        (Some(original), Some(current)) => {
            if !same_actor(
                http,
                originator,
                original
                    .get("holder")
                    .and_then(Value::as_str)
                    .context("original source holder is unreadable")?,
                reads,
            )
            .await?
            {
                bail!("original requesting actor does not own this source claim");
            }
            if original.get("contract_version").and_then(Value::as_u64) != Some(1)
                || [
                    "run",
                    "packet",
                    "step",
                    "step_id",
                    "building_step",
                    "backlink",
                ]
                .iter()
                .any(|key| original.get(*key) != current.get(*key))
            {
                bail!("original source linkage changed after immutable acceptance");
            }
            if !same_actor(
                http,
                original
                    .get("holder")
                    .and_then(Value::as_str)
                    .context("original source holder is unreadable")?,
                current["holder"]
                    .as_str()
                    .context("current source holder is unreadable")?,
                reads,
            )
            .await?
            {
                bail!("source holder changed after immutable acceptance");
            }
            Ok(())
        }
        _ => bail!("declared source lacks its original immutable claim linkage"),
    }
}
