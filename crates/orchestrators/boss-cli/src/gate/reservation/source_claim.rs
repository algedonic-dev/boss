//! Re-read the original registered claim, not the lifetime of its model process.
use super::*;

/// One request per source run. This is request identity, not a bay claim
/// or evidence that the gate has accepted or launched anything.
pub(super) const GATE_REQUEST: &str = "gate_request";

fn binding_command(run: &str, packet: &str) -> Command {
    // One operation for this run, independent of the requested gate. The
    // conditional door retains its first command and refuses different
    // content under that identity, even if mutable metadata is changed.
    let identity = format!("boss.agent-run.gate-request.v1:{run}");
    Command {
        contract_version: 2,
        idempotency_key: Uuid::new_v5(&Uuid::NAMESPACE_OID, identity.as_bytes()),
        expected: [(
            GATE_REQUEST.into(),
            boss_jobs::conditional_metadata::Expected {
                present: false,
                value: Value::Null,
            },
        )]
        .into_iter()
        .collect(),
        patch: [(
            GATE_REQUEST.into(),
            json!({"contract_version":1,"gate_run":packet}),
        )]
        .into_iter()
        .collect(),
        immutable_keys: vec![],
        guarded_keys: vec![],
    }
}

fn receipt_gate<'a>(run: &str, actor: &str, receipt: &'a Receipt) -> Result<&'a str> {
    let binding = receipt
        .command
        .patch
        .get(GATE_REQUEST)
        .context("source binding receipt has no gate request")?;
    let gate = binding
        .get("gate_run")
        .and_then(Value::as_str)
        .filter(|gate| gate.len() == 36 && Uuid::parse_str(gate).is_ok_and(|id| !id.is_nil()))
        .context("source binding receipt has no full gate identity")?;
    if receipt.job_id.to_string() != run
        || receipt.job.id.to_string() != run
        || receipt.job.kind != "agent-run"
        || receipt.actor != actor
        || receipt.contract_version != 2
        || receipt.event_id.is_nil()
        || receipt.receipt_event_id.is_none_or(|id| id.is_nil())
        || receipt.command != binding_command(run, gate)
        || receipt.job.metadata.get(GATE_REQUEST) != Some(binding)
    {
        bail!("source gate binding does not match its retained conditional receipt");
    }
    Ok(gate)
}

/// Older queued gates have no request-binding receipt. Read the complete
/// population linked to this run before admitting its first new request.
async fn other_requests(
    http: &Doors<'_>,
    run: &str,
    packet: &str,
    reads: &mut Vec<Value>,
) -> Result<()> {
    let mut offset = 0usize;
    let mut expected_total = None;
    loop {
        let mut url = reqwest::Url::parse(&format!("{}/api/jobs", http.jobs_base))?;
        url.query_pairs_mut()
            .append_pair("kind", "gate-run")
            .append_pair("full", "true")
            .append_pair("limit", "100")
            .append_pair("offset", &offset.to_string())
            .append_pair(
                "metadata",
                &serde_json::to_string(&json!({"agent_run":run}))?,
            );
        let path = format!("{}?{}", url.path(), url.query().unwrap_or_default());
        let page = api(http, reqwest::Method::GET, &path, None)
            .await?
            .context("source run's other gate requests are unreadable")?;
        reads.push(read_digest("source-run-other-gates", &page));
        let total = page
            .get("total")
            .and_then(Value::as_u64)
            .and_then(|total| usize::try_from(total).ok())
            .context("source gate request list lacks its complete total")?;
        if expected_total.is_some_and(|before| before != total) {
            bail!("source gate request list changed while paging; no request admitted");
        }
        expected_total = Some(total);
        let rows = rows(Some(page))?;
        if offset + rows.len() > total || (rows.is_empty() && offset < total) {
            bail!("source gate request list is incomplete; no request admitted");
        }
        for row in &rows {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .context("source gate request list carries an unreadable packet")?;
            if row.get("kind").and_then(Value::as_str) != Some("gate-run")
                || row.pointer("/metadata/agent_run").and_then(Value::as_str) != Some(run)
            {
                bail!("source gate request list returned a packet outside its declared query");
            }
            if id != packet
                && row.get("metadata").is_some_and(|metadata| {
                    metadata.get(model::SUBMISSION).is_some()
                        || metadata.get(boss_jobs::yard::LAUNCHED_AT).is_some()
                })
            {
                bail!(
                    "source run {run} already bound to gate {id}; reattach that packet or dispatch a fresh run for another car"
                );
            }
        }
        offset += rows.len();
        if offset == total {
            return Ok(());
        }
    }
}

/// The retained first command is the binding, not its mutable projection
/// on the run. A different packet cannot reuse that operation identity;
/// an exact retry reads the first receipt without writing a second fact.
pub(super) async fn bind_request(
    http: &Doors<'_>,
    claim: &Value,
    packet: &str,
    actor: &str,
    reads: &mut Vec<Value>,
) -> Result<()> {
    let run = claim
        .get("run")
        .and_then(Value::as_str)
        .context("source claim names no run")?;
    let refuse_other = |other: &str| -> Result<()> {
        if other != packet {
            bail!(
                "source run {run} already bound to gate {other}; reattach that packet or dispatch a fresh run for another car"
            );
        }
        Ok(())
    };
    let row = read_on(http, run).await?;
    reads.push(read_digest("source-run-gate-binding", &row));
    if row.get("id").and_then(Value::as_str) != Some(run)
        || row.get("kind").and_then(Value::as_str) != Some("agent-run")
        || row.get("status").and_then(Value::as_str) != Some("open")
    {
        bail!("source run changed before binding its gate request");
    }
    let metadata = row.get("metadata").context("source run has no metadata")?;
    // Runs handed off before this first-write binding was introduced
    // already own a gate. Never replace their copied receipt.
    if metadata.get(boss_jobs::gate_handoff::KEY).is_some() {
        let stamp = boss_jobs::gate_handoff::stamp_for(metadata, run)
            .context("source run's existing gate hand-off is unreadable")?;
        let gate = stamp
            .get("gate_run")
            .and_then(Value::as_str)
            .context("source run's existing hand-off names no gate")?;
        refuse_other(gate)?;
    }
    let building = one_step(&row, "building")?;
    if let Some(record) = building.pointer("/metadata/gate_run") {
        let gate = record
            .get("car")
            .and_then(Value::as_str)
            .filter(|id| Uuid::parse_str(id).is_ok())
            .context("source building's existing gate record is unreadable")?;
        refuse_other(gate)?;
    }
    other_requests(http, run, packet, reads).await?;
    let command = binding_command(run, packet);
    let receipt = match apply_on(http, run, &command).await {
        Ok(receipt) => receipt,
        Err(error) if format!("{error:#}").contains("409") => {
            let mut url = reqwest::Url::parse(&format!(
                "{}/api/jobs/{run}/metadata/conditional/{}",
                http.jobs_base, command.idempotency_key
            ))?;
            url.query_pairs_mut().append_pair("actor", actor);
            let path = format!("{}?{}", url.path(), url.query().unwrap_or_default());
            let value = api(http, reqwest::Method::GET, &path, None).await
                    .with_context(|| format!("source gate binding refused ({error:#}); original binding receipt is unreadable"))?
                    .context("source gate binding refused and carries no retained receipt")?;
            serde_json::from_value(value).context("source gate binding receipt is unreadable")?
        }
        Err(error) => return Err(error),
    };
    let gate = receipt_gate(run, actor, &receipt)?;
    let mut proof = read_digest("source-run-gate-bound", &serde_json::to_value(&receipt)?);
    proof
        .as_object_mut()
        .context("source binding receipt digest is unreadable")?
        .extend([
            ("run".into(), json!(run)),
            ("gate_run".into(), json!(gate)),
            ("command".into(), json!(receipt.command.idempotency_key)),
            ("event_id".into(), json!(receipt.event_id)),
            ("receipt_event_id".into(), json!(receipt.receipt_event_id)),
            ("accepted_at".into(), json!(receipt.applied_at)),
        ]);
    reads.push(proof);
    refuse_other(gate)
}

/// The evidence a source-claim check retains on the gate-run packet,
/// NAMING THAT PACKET (backlog 2cd4e484). A request with no agent run has
/// no reads, so without the packet these bytes were the same 71 for every
/// run-less request; the content store keeps one row per digest and
/// answers an identical upload for another target 409, so from the first
/// such request on (3d4c562f, 2026-10-21) every later one failed this
/// retain on every resident pass and never launched. The exact-branch
/// read names its packet for the same reason (74778aad).
pub(super) fn evidence(
    operation: &str,
    packet: &str,
    reads: &[Value],
    error: Option<String>,
) -> Value {
    json!({"operation":operation,"packet":packet,"reads":reads,"error":error})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two packets' run-less checks with the same outcome retain
    /// different bytes, so neither can be refused as the other's upload.
    #[test]
    fn identical_source_claim_outcomes_on_two_packets_are_two_documents() {
        for (operation, error) in [
            ("source-claim-before-native-send", None),
            (
                "source-claim-admission-refusal",
                Some("original source run is not a full declared identity".to_owned()),
            ),
        ] {
            let one = evidence(operation, "packet-one", &[], error.clone());
            let two = evidence(operation, "packet-two", &[], error);
            assert_ne!(
                serde_json::to_vec(&one).unwrap(),
                serde_json::to_vec(&two).unwrap(),
                "{operation}"
            );
            assert_eq!(one["packet"], "packet-one");
        }
    }
}

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

/// The holder of `run`'s `building`: an ACTIVE building's, as always, or
/// a HANDED one's while the run still waits on its verdict (backlog
/// 87d1ae40).
///
/// WHY A HANDED BUILDING STILL AUTHORIZES ITS GATE. `boss gate` ends a
/// run's `building` as `handed` at the accepted submission — the builder
/// station's work is done and its robots:<class> claim exits — and the
/// run waits on `verdict` with no claim. The resident supervisor re-reads
/// this claim when a bay frees, often 30-80 minutes later, and launches
/// only under it. Read as "no longer active", every handed car would be
/// withdrawn at launch: the f6fefad6 failure (a correct gate withdrawn
/// after 2h20m because its run's state moved under it), made universal.
/// What the claim stands for is unchanged: the run is the exact OPEN
/// agent-run, its holder is the one that built it (a completed step
/// keeps its holder), and the verdict it waits on is still open — a green,
/// a red or a death closes `verdict`, and from then on this refuses
/// exactly as a completed `building` always did.
fn building_holder<'a>(run: &'a Value, building: &'a Value) -> Result<&'a str> {
    let handed = building.get("status").and_then(Value::as_str) == Some("completed")
        && building.pointer("/metadata/result").and_then(Value::as_str)
            == Some(boss_jobs::gate_handoff::HANDED);
    if !handed {
        return active_holder(building);
    }
    let verdict = one_step(run, boss_jobs::gate_handoff::VERDICT_SLUG)?;
    if !matches!(
        verdict.get("status").and_then(Value::as_str),
        Some("ready" | "active")
    ) {
        bail!("source claim step is no longer active: the handed run's verdict is recorded");
    }
    building
        .get("assignee_id")
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
    let holder = building_holder(&row, building)?;
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
    let sibling = match backlink.filter(|value| value.as_str() != Some(run)) {
        None => None,
        Some(other) => Some(sibling_run(http, other, packet, slug, holder, reads).await?),
    };
    let mut claim = json!({"contract_version":1,"run":run,"packet":packet,"step":slug,
        "step_id":step.get("id").and_then(Value::as_str).filter(|s|s.len()==36 && Uuid::parse_str(s).is_ok()).context("parent step has no valid identity")?,
        "building_step":building.get("id").and_then(Value::as_str).filter(|s|s.len()==36 && Uuid::parse_str(s).is_ok()).context("source building has no valid identity")?,
        "holder":holder,"backlink":backlink});
    // THE CLAIM ID THE HAND-OFF EXITS BY (backlog 87d1ae40). The step PUT
    // frees a claimed step's slot only when the exit names the claim the
    // executor read; this is that read, taken with the claim that
    // authorizes the gate and receipted with the submission, so `boss
    // gate` names exactly it — never a fresh read at the hand-off, which
    // a re-claim in between would make another executor's. Not one of the
    // linkage keys `verify` compares: a handed run's claim has exited.
    if let Some(id) = building
        .pointer(&format!(
            "/metadata/{}/id",
            boss_jobs::flow_resources::TOKEN_KEY
        ))
        .filter(|id| id.is_string())
    {
        claim[crate::gate::BUILDING_CLAIM] = id.clone();
    }
    if let Some(sibling) = sibling {
        claim[SIBLING_RUN] = json!(sibling);
    }
    Ok(Some(claim))
}

/// The refusal a parent step's backlink earns when it names neither this
/// run nor a sibling of it, worded as it always was.
const NO_LONGER_LINKS: &str = "original parent step no longer links this source run";

/// Where a claim records the sibling run its parent step links instead
/// of this one. Present only then, so every other claim keeps its bytes.
const SIBLING_RUN: &str = "sibling_run";

/// THE STEP MOVED ON TO A SIBLING RUN OF THE SAME HOLDER (backlog
/// 9f510bcf). `boss dispatch <item>` rewrites the parent step's
/// `agent_run` backlink to each new run, so a builder carrying several
/// cars from one item left every older run's gate unlaunchable: on
/// 2026-11-08 four gates sat 1.7-2.7 h behind a free bay, each retained
/// with the sentence above. A backlink naming ANOTHER run proves this run
/// died only when that run is not a sibling. It is one when it is the
/// exact OPEN agent-run, its parent is this same packet and step, and its
/// holder is this run's holder; then the claim stands, and the run-open
/// and holder checks already made still decide it. A closed run, another
/// step, another holder, or a backlink that is no run at all refuses as
/// before. Returns the sibling's id.
async fn sibling_run(
    http: &Doors<'_>,
    backlink: &Value,
    packet: &str,
    slug: &str,
    holder: &str,
    reads: &mut Vec<Value>,
) -> Result<String> {
    let Some(other) = backlink
        .as_str()
        .filter(|s| s.len() == 36 && Uuid::parse_str(s).is_ok())
    else {
        bail!(NO_LONGER_LINKS);
    };
    let row = read_on(http, other)
        .await
        .with_context(|| format!("{NO_LONGER_LINKS}: the run it links, {other}, is unreadable"))?;
    reads.push(read_digest("parent-step-sibling-run", &row));
    let sibling = row.get("id").and_then(Value::as_str) == Some(other)
        && row.get("kind").and_then(Value::as_str) == Some("agent-run")
        && row.get("status").and_then(Value::as_str) == Some("open")
        && row.pointer("/metadata/packet").and_then(Value::as_str) == Some(packet)
        && row.pointer("/metadata/step").and_then(Value::as_str) == Some(slug);
    let sibling_holder = one_step(&row, "building")
        .ok()
        .and_then(|step| step.get("assignee_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(theirs) = sibling_holder.filter(|_| sibling) else {
        bail!(NO_LONGER_LINKS);
    };
    if !same_actor(http, holder, theirs, reads)
        .await
        .with_context(|| format!("{NO_LONGER_LINKS}: the holder of {other} is unresolvable"))?
    {
        bail!(NO_LONGER_LINKS);
    }
    Ok(other.to_owned())
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
                .any(|key| {
                    original.get(*key) != current.get(*key)
                        // A backlink moved on to a sibling run was judged
                        // by `read_claim` just now (backlog 9f510bcf).
                        && !(*key == "backlink" && current.get(SIBLING_RUN).is_some())
                })
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
