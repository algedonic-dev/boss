//! One bounded pass over accepted requests, owned by the existing conductor.
use super::*;

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use axum::response::IntoResponse;
    use boss_content::files::{FileRepository, FileStorage};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Real content addressing is global, but attachment authority is per packet.
    /// Two forge reads of one unchanged ref must remain retainable on both packets.
    #[tokio::test]
    async fn identical_forge_answers_are_retained_on_each_packet_and_retries_reuse() {
        let repo = Arc::new(boss_content::files::InMemoryFileRepository::new());
        let storage = Arc::new(boss_content::files::InMemoryFileStorage::new());
        let lose_ack = Arc::new(AtomicBool::new(true));
        let fault = lose_ack.clone();
        let app = boss_content::files::http::router(boss_content::files::http::FilesApiState {
            repo: repo.clone(),
            storage: storage.clone(),
            publisher: None,
            policy: Arc::new(boss_policy_client::PermissivePolicyClient),
            bucket: "fixture".into(),
            pool: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let fault = fault.clone();
                async move {
                    let upload = request.method() == reqwest::Method::POST;
                    let response = next.run(request).await;
                    if upload
                        && response.status().is_success()
                        && fault.swap(false, Ordering::SeqCst)
                    {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        response
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let checkout = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(checkout.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "--initial-branch=main"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "Source",
        ]);
        git(&["remote", "add", "origin", checkout.path().to_str().unwrap()]);
        let head = git(&["rev-parse", "HEAD"]);
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder().no_proxy())
                .unwrap();
        let doors = Doors {
            http: &client,
            jobs_base: address.clone(),
            content_base: address,
            caller: Some(identity::Caller {
                id: identity::CONDUCTOR.into(),
                source: identity::Source::Env,
            }),
            native_args: Vec::new(),
            create_program: None,
        };
        let packets = [Uuid::new_v4().to_string(), Uuid::new_v4().to_string()];
        let mut held = Vec::new();
        for packet in &packets {
            let mut report = Report::default();
            exact_branch(&doors, checkout.path(), packet, "main", &head, &mut report)
                .await
                .expect("each packet must retain its own identical forge answer");
            let first = report.reads[0].clone();
            // A caller restarting after an upload acknowledgement was lost must find
            // its same-target durable row, not create another attachment or send a Job.
            let mut retry = Report::default();
            exact_branch(&doors, checkout.path(), packet, "main", &head, &mut retry)
                .await
                .unwrap();
            assert_eq!(first, retry.reads[0]);
            let target = boss_content::files::ResourceRef {
                kind: boss_content::files::ResourceKind::Job,
                id: packet.clone(),
            };
            let rows = repo.list_for(&target).await.unwrap();
            assert_eq!(rows.len(), 1, "one durable attachment per packet");
            let raw = storage.get(&rows[0].object_key).await.unwrap();
            let evidence: Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(evidence["packet"], *packet);
            assert_eq!(evidence["reference"], "refs/heads/main");
            assert_eq!(evidence["status"], 0);
            let output: Vec<u8> = serde_json::from_value(evidence["stdout"].clone()).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("{head}\tHEAD\n{head}\trefs/heads/main\n")
            );
            assert_eq!(evidence["stderr"], json!([]));
            held.push(rows[0].id);
        }
        assert!(
            !lose_ack.load(Ordering::SeqCst),
            "committed first upload reply was lost"
        );
        assert_ne!(held[0], held[1], "never borrow another packet's attachment");
        server.abort();
    }
}

#[derive(Default, Debug, Serialize)]
pub(super) struct Report {
    pub launched: Vec<String>,
    pub observed: Vec<String>,
    pub retained: Vec<Value>,
    pub reads: Vec<Value>,
    /// Requests this pass closed because it PROVED they will never
    /// launch, each with the cause and the evidence it read (backlog
    /// 5db91d0e, 48b1b7b4). Never a request waiting for a bay, never an
    /// UNKNOWN.
    pub settled: Vec<Value>,
    /// The `gate_queue_retained` writes this pass made on gate-run
    /// packets — `{packet, reason}`, a null reason for a deletion — and
    /// any that failed, with the error (backlog 9f510bcf).
    pub annotated: Vec<Value>,
    /// Requests this pass kept waiting ONLY for a bay, in the bay line's
    /// order: `{packet, position, reason}` (backlog 64d960e7).
    pub waiting: Vec<Value>,
    /// The `gate_queue_place` writes this pass made — `{packet, place}`,
    /// a null place for a deletion — and any that failed, with the error.
    pub placed: Vec<Value>,
}

/// The retained reason the supervisor of one namespace gives a request
/// another namespace's supervisor owns. Never written onto the packet: it
/// is not why that packet waits, and two supervisors would overwrite each
/// other's reason on every pass.
const OTHER_NAMESPACE: &str = "submission belongs to a different namespace";

/// PURE: the metadata writes that make each open gate-run packet say why
/// the pass kept it in line (backlog 9f510bcf). On 2026-11-08 four gates
/// sat 1.7-2.7 h behind a free bay and their reasons were readable only
/// inside the train-dock-refresh cadence firing; the packets read
/// `queued` like any healthy wait. A packet retained with a reason it
/// does not already carry gets `{reason, since: now}`; one that carries a
/// reason and was not retained this pass (it launched, was settled, or
/// waits only for a bay) gets null, deleting it. An unchanged reason is
/// not rewritten, so `since` is when the pass first saw it. A packet whose
/// submission belongs to another namespace is left alone.
fn retained_writes(
    gates: &[Value],
    report: &Report,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(String, Value)> {
    let mut reasons: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for entry in &report.retained {
        if let (Some(packet), Some(reason)) = (
            entry.get("packet").and_then(Value::as_str),
            entry.get("reason").and_then(Value::as_str),
        ) {
            reasons.entry(packet).or_default().push(reason);
        }
    }
    gates
        .iter()
        .filter_map(|row| {
            let packet = row.get("id").and_then(Value::as_str)?;
            let carried = row
                .pointer(&format!("/metadata/{}", crate::gate::QUEUE_RETAINED))
                .filter(|v| !v.is_null());
            let reason = reasons.get(packet).map(|r| r.join("; "));
            if reason.as_deref() == Some(OTHER_NAMESPACE) {
                return None;
            }
            let body = match (reason, carried) {
                (Some(reason), Some(carried))
                    if carried.get("reason").and_then(Value::as_str) == Some(reason.as_str()) =>
                {
                    return None;
                }
                (Some(reason), _) => json!({"reason": reason, "since": now}),
                (None, Some(_)) => Value::Null,
                (None, None) => return None,
            };
            Some((
                packet.to_owned(),
                json!({ crate::gate::QUEUE_RETAINED: body }),
            ))
        })
        .collect()
}

/// PURE: the metadata writes that make each waiting request say where it
/// stands in the bay line and why (backlog 64d960e7): `{position,
/// reason}` under `gate_queue_place`, written when it changes, deleted
/// once the packet no longer waits for a bay (it launched, was settled,
/// or is retained with a reason of its own). An unchanged place is not
/// rewritten, so the audit log records each move, not each pass. Another
/// namespace's packet is left alone, as [`retained_writes`] leaves it.
fn place_writes(gates: &[Value], report: &Report) -> Vec<(String, Value)> {
    let key = boss_jobs::yard::QUEUE_PLACE;
    let foreign: std::collections::HashSet<&str> = report
        .retained
        .iter()
        .filter(|e| e.get("reason").and_then(Value::as_str) == Some(OTHER_NAMESPACE))
        .filter_map(|e| e.get("packet").and_then(Value::as_str))
        .collect();
    gates
        .iter()
        .filter_map(|row| {
            let packet = row.get("id").and_then(Value::as_str)?;
            if foreign.contains(packet) {
                return None;
            }
            let carried = row
                .pointer(&format!("/metadata/{key}"))
                .filter(|v| !v.is_null());
            let place = report
                .waiting
                .iter()
                .find(|w| w.get("packet").and_then(Value::as_str) == Some(packet))
                .map(|w| json!({"position": w["position"], "reason": w["reason"]}));
            let body = match (place, carried) {
                (Some(place), Some(carried)) if &place == carried => return None,
                (Some(place), _) => place,
                (None, Some(_)) => Value::Null,
                (None, None) => return None,
            };
            Some((packet.to_owned(), json!({ key: body })))
        })
        .collect()
}

/// Apply [`retained_writes`] through the metadata merge door. A failed
/// write is recorded on the report and never fails the pass: the line is
/// decided, and its annotation is a reading of it.
async fn annotate(
    http: &Doors<'_>,
    gates: &[Value],
    report: &mut Report,
    now: chrono::DateTime<chrono::Utc>,
) {
    for (packet, body) in retained_writes(gates, report, now) {
        let reason = body[crate::gate::QUEUE_RETAINED]
            .get("reason")
            .cloned()
            .unwrap_or(Value::Null);
        let written = api(
            http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(body),
        )
        .await;
        report.annotated.push(match written {
            Ok(_) => json!({"packet": packet, "reason": reason}),
            Err(error) => {
                json!({"packet": packet, "reason": reason, "error": format!("{error:#}")})
            }
        });
    }
    for (packet, body) in place_writes(gates, report) {
        let place = body[boss_jobs::yard::QUEUE_PLACE].clone();
        let written = api(
            http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(body),
        )
        .await;
        report.placed.push(match written {
            Ok(_) => json!({"packet": packet, "place": place}),
            Err(error) => {
                json!({"packet": packet, "place": place, "error": format!("{error:#}")})
            }
        });
    }
}

/// The drain rank (`top_board::priority_rank`) of each item the waiting
/// requests answer, read once per item for the bay line's order (backlog
/// 64d960e7). NEVER FATAL, as the dock walk's own `item_ranks` is not:
/// ordering decides who goes first, not who goes, so an item that cannot
/// be read is recorded on the report and its request ranks standard.
async fn item_ranks_on(
    http: &Doors<'_>,
    rows: &[&Value],
    cars: &[Value],
    report: &mut Report,
) -> std::collections::HashMap<String, u8> {
    let mut ranks = std::collections::HashMap::new();
    for row in rows {
        let car = super::line_order::car_of(row, cars);
        let Some(item) = super::line_order::answered_item(row, car) else {
            continue;
        };
        if ranks.contains_key(item) {
            continue;
        }
        match api(http, reqwest::Method::GET, &format!("/api/jobs/{item}"), None).await {
            Ok(Some(packet)) => {
                ranks.insert(item.to_owned(), crate::top_board::priority_rank(&packet));
            }
            Ok(None) => report.reads.push(
                json!({"operation":"bay-order-item","item":item,"error":"no document; ranked standard"}),
            ),
            Err(error) => report.reads.push(json!({"operation":"bay-order-item","item":item,
                "error":format!("{error:#}; ranked standard")})),
        }
    }
    ranks
}

async fn pages(http: &Doors<'_>, query: &str, report: &mut Report) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut total = None;
    let mut ids = std::collections::HashSet::new();
    for _ in 0..1000 {
        let path = format!("/api/jobs?{query}&full=true&limit=50&offset={}", rows.len());
        let page = api(http, reqwest::Method::GET, &path, None)
            .await?
            .context("queue list returned no document")?;
        // Its digest, not the page: the report rides the cadence outcome
        // POST, and whole pages made it 6.3 MB live (e1943ef0).
        let mut read = read_digest("queue-list-page", &page);
        read["path"] = json!(path);
        report.reads.push(read);
        let count = page
            .get("total")
            .and_then(Value::as_u64)
            .context("queue list has no declared total")?;
        if total.is_some_and(|expected| expected != count) {
            bail!("queue list changed its declared total during pagination");
        }
        total = Some(count);
        let data = page
            .get("data")
            .and_then(Value::as_array)
            .context("queue list has no complete data array")?;
        for row in data {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .context("queue list has a row without identity")?;
            if !ids.insert(id.to_owned()) {
                bail!("queue list repeated a packet while paginating");
            }
            rows.push(row.clone());
        }
        if rows.len() as u64 > count {
            bail!("queue list exceeds its declared total");
        }
        if rows.len() as u64 == count {
            return Ok(rows);
        }
        if data.is_empty() {
            bail!("queue list ended before its declared total");
        }
    }
    bail!("queue list exceeded the bounded complete-read budget; no truncated list is usable")
}

/// A rule's last firing as the pass keeps it: the three fields the
/// liveness decision reads (`rule_keeps_claims`: `firing_id`, `fired_at`,
/// `rc`) and the size and digest of the whole reply — never the reply.
///
/// WHY (backlog 443023a4). The train-reconcile firing carries this pass's
/// own previous observation, so a whole copy embedded the previous report,
/// which embedded the one before it: with a train waiting and a submission
/// queued every pass grew by one pass, and the live firing reached
/// 6,654,578 bytes at 07:34Z on 2026-10-19 — past the outcome door's
/// 2 MiB, so every pass recorded `gate_queue` unknown (413) and left a
/// file that size in the conductor's temp dir. A never-fired rule (`null`)
/// is recorded as null: absence is a reading, not a digest.
fn firing_digest(operation: &str, reply: Option<&Value>) -> Value {
    match reply {
        None | Some(Value::Null) => json!({"operation":operation,"reply":null}),
        Some(reply) => {
            let mut read = read_digest(operation, reply);
            for key in ["firing_id", "fired_at", "rc"] {
                read[key] = reply.get(key).cloned().unwrap_or(Value::Null);
            }
            read
        }
    }
}

async fn exact_branch(
    http: &Doors<'_>,
    checkout: &Path,
    packet: &str,
    branch: &str,
    head: &str,
    report: &mut Report,
) -> Result<()> {
    if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) || branch.is_empty() {
        bail!("submission lacks an exact full source head/branch");
    }
    let reference = format!("refs/heads/{branch}");
    let mut command = crate::git_auth::command();
    // The forge's own HEAD rides the same read as its CONTROL (backlog
    // 5db91d0e): a branch missing from an answer that also lacks HEAD is
    // a wrong or empty target speaking, not a deleted branch.
    command
        .current_dir(checkout)
        .args(["ls-remote", "origin", "HEAD", reference.as_str()]);
    let output = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .output()
        .await?;
    // The content store's digest is global, but attachment reuse is deliberately
    // target-scoped. Retain the packet provenance, not an unbound identical read
    // that a previous gate already owns (74778aad, measured c9 HTTP409).
    let artifact=retain(http,packet,&serde_json::to_vec(&json!({"operation":"gate-queue-exact-branch","packet":packet,
        "reference":reference,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr}))?).await?;
    let evidence = json!({"operation":"gate-queue-exact-branch","reference":reference,
        "file_id":artifact.file_id,"sha256":artifact.sha256,"size_bytes":artifact.size_bytes});
    report.reads.push(evidence.clone());
    if !output.status.success() {
        bail!(
            "exact source branch read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let because = match branch_answer(text, &reference, head) {
        BranchAnswer::AtHead => return Ok(()),
        BranchAnswer::Unknown(why) => bail!(
            "source branch answer is UNKNOWN ({why}); immutable request is retained, not settled"
        ),
        BranchAnswer::Moved(tip) => format!(
            "{branch} moved to {tip} after this request was accepted at {head}; a resident \
             request never follows its tip, so it can never launch"
        ),
        BranchAnswer::Absent => format!(
            "{branch} is absent from the forge (its HEAD answered on the same read) after \
             this request was accepted at {head}, so it can never launch"
        ),
    };
    Err(NeverLaunches { because, evidence }.into())
}

/// A refusal the pass PROVED permanent, with the read that proved it
/// (backlog 5db91d0e). Three build one: a positive forge answer, a
/// source head that disagrees with the immutable request, and a car
/// already green at the request's exact head (backlog 48b1b7b4). Every
/// other refusal stays a plain error and its request is retained.
#[derive(Debug)]
struct NeverLaunches {
    because: String,
    evidence: Value,
}
impl std::fmt::Display for NeverLaunches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never launches: {}", self.because)
    }
}
impl std::error::Error for NeverLaunches {}

#[derive(Debug, PartialEq, Eq)]
enum BranchAnswer {
    AtHead,
    /// The branch carries another head — the tip it moved to.
    Moved(String),
    /// The branch is not listed, and the forge's HEAD is.
    Absent,
    Unknown(String),
}

/// PURE: what one `ls-remote origin HEAD <reference>` answer says of the
/// accepted `head`. Absence counts only beside the control (`HEAD`
/// listed on the same read), because a wrong or empty remote answers an
/// empty list with exit 0 rather than an error.
fn branch_answer(text: &str, reference: &str, head: &str) -> BranchAnswer {
    let mut control = false;
    let mut tips = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [sha, name] = fields[..] else {
            return BranchAnswer::Unknown(format!("unparsed line {line:?}"));
        };
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return BranchAnswer::Unknown(format!("unparsed object name {sha:?}"));
        }
        match name {
            "HEAD" => control = true,
            n if n == reference => tips.push(sha),
            _ => {}
        }
    }
    match tips[..] {
        [tip] if tip == head => BranchAnswer::AtHead,
        [tip] => BranchAnswer::Moved(tip.to_owned()),
        [] if control => BranchAnswer::Absent,
        [] => {
            BranchAnswer::Unknown("the forge listed neither the branch nor its own HEAD".to_owned())
        }
        _ => BranchAnswer::Unknown("the branch was listed more than once".to_owned()),
    }
}

/// Put the guarded decision where its owner reads work (6024b8da).
/// The immutable submission names the parent; a later edit to the dead
/// run must not redirect this write. One metadata key per gate avoids a
/// read/append race between gates of the same item. Write and read back
/// before the terminal, so a failed notification retries next pass.
async fn notify_source_owner(
    http: &Doors<'_>,
    packet: &str,
    row: &Value,
    receipt: &Value,
) -> Result<()> {
    if receipt
        .pointer("/evidence/operation")
        .and_then(Value::as_str)
        != Some("original-source-run")
    {
        return Ok(());
    }
    let source = model::source_metadata(row.get("metadata").context("gate lost metadata")?)
        .map_err(anyhow::Error::msg)?;
    let claim = source
        .get("gate_source_claim")
        .context("settled source lacks its original claim")?;
    let run = receipt
        .pointer("/evidence/run")
        .and_then(Value::as_str)
        .context("settled source receipt lacks its run")?;
    if claim.get("run").and_then(Value::as_str) != Some(run) {
        bail!("settled source receipt and immutable claim name different runs");
    }
    let parent = claim
        .get("packet")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 36 && Uuid::parse_str(id).is_ok())
        .context("settled source claim lacks its exact parent")?;
    let owner = read_on(http, parent).await?;
    if owner.get("id").and_then(Value::as_str) != Some(parent) {
        bail!("settled source owner read returned a different packet");
    }
    let key = format!("gate_settlement_{packet}");
    let note = json!({"gate_run": packet, "source_run": run,
        "decision": "cannot-launch", "receipt": receipt,
        "next_action": "Re-gate the head from a live run."});
    if owner.get("metadata").and_then(|m| m.get(&key)) == Some(&note) {
        return Ok(());
    }
    api(
        http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{parent}/metadata"),
        Some(json!({key.clone(): note.clone()})),
    )
    .await
    .context("could not notify the settled gate's owner")?;
    let observed = read_on(http, parent).await?;
    if observed.get("id").and_then(Value::as_str) != Some(parent)
        || observed.get("metadata").and_then(|m| m.get(&key)) != Some(&note)
    {
        bail!("settled gate's owner note did not read back");
    }
    Ok(())
}

/// Close a request this pass proved will never launch: the guarded
/// negative claim first (`settlement::claim_on`, which refuses a packet
/// that may have a native Job or a possibly effective send), then the
/// verdict through the same two writes `boss gate --withdraw` takes. A
/// claim an earlier pass committed is the decision of record, so a
/// settle interrupted between the claim and the verdict resumes with
/// that receipt rather than a second one the immutable claim refuses.
async fn settle_never_launches(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    never: &NeverLaunches,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value> {
    let row = read_on(http, packet).await?;
    if let Some(why) = withdraw_refusal(packet, &row) {
        bail!(why);
    }
    let prior = row
        .pointer("/metadata/gate_orphan_claim/verdict_receipt")
        .filter(|r| r.get("settled_by").and_then(Value::as_str) == Some(SETTLED_BY));
    let receipt = match prior {
        Some(prior) => prior.clone(),
        None => {
            let mut receipt = withdrawal_receipt(&never.because, None);
            receipt["settled_by"] = json!(SETTLED_BY);
            receipt["evidence"] = never.evidence.clone();
            receipt
        }
    };
    settlement::claim_on(http, packet, namespace, &receipt, now).await?;
    notify_source_owner(http, packet, &row, &receipt).await?;
    let step_id = crate::train::find_step(&row, "record-verdict", "Record the receipt")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .context("gate-run has no verdict step")?;
    for (method, path, body) in verdict_writes(packet, step_id, &receipt)? {
        api(http, method, &path, Some(body)).await?;
    }
    Ok(receipt)
}

/// Who a supervisor settle names as its writer, in the receipt itself.
const SETTLED_BY: &str = "resident-supervisor";

/// A source run that has FINISHED is a refusal as permanent as a moved
/// branch: the supervisor launches only under the exact OPEN run, and a
/// closed packet never reopens (backlog 36352452). Measured 2026-10-21
/// (backlog 599aed02): seven places accepted under builder runs the hourly
/// clock later closed as `died` were retained on every pass for eleven
/// hours with "original source run is not the exact open agent-run",
/// holding their places while newer gates launched. Every other source
/// refusal is returned as it came and its request is retained.
fn source_run_ended(error: anyhow::Error) -> anyhow::Error {
    let Some(ended) = error.downcast_ref::<source_claim::SourceRunEnded>() else {
        return error;
    };
    let outcome = ended
        .outcome
        .as_deref()
        .map(|o| format!(" with outcome {o}"))
        .unwrap_or_default();
    NeverLaunches {
        because: format!(
            "source run {} is {}{outcome}; a resident request launches only under its exact \
             open agent-run and a closed run never reopens, so it can never launch — re-gate \
             the head from a live run",
            ended.run, ended.status
        ),
        evidence: json!({"operation":"original-source-run","run":ended.run,
            "status":ended.status,"outcome":ended.outcome,"read":ended.read}),
    }
    .into()
}

/// PURE: under a gates hold (backlog 95401d82), keep in `accepted` only
/// the requests whose gate-run was launched `--despite-hold` of THIS hold
/// (`hold`, its id — an override of an earlier hold crosses nothing), and return
/// a retained entry — the hold's own line as its reason — for every
/// other one. They keep their place; nothing is settled.
fn hold_back<A, S>(
    accepted: &mut Vec<(A, String, S)>,
    gates: &[Value],
    hold: &str,
    held: &str,
) -> Vec<Value> {
    let despite: std::collections::HashSet<&str> = gates
        .iter()
        .filter(|g| {
            g.get("metadata")
                .is_some_and(|m| crate::gate_hold::launched_despite(m, hold))
        })
        .filter_map(|g| g.get("id").and_then(Value::as_str))
        .collect();
    let mut retained = Vec::new();
    accepted.retain(|(_, packet, _)| {
        let through = despite.contains(packet.as_str());
        if !through {
            retained.push(json!({"packet": packet, "reason": held}));
        }
        through
    });
    retained
}

pub(super) async fn observation_on(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut report = Report::default();
    let result = pass_inner(http, checkout, namespace, max, now, &mut report).await;
    json!({"contract_version":1,"state":if result.is_ok(){"completed"}else{"unknown"},
        "observed_at":now,"source_build":crate::built_from::built_from(),"report":report,
        "error":result.err().map(|error|format!("{error:#}"))})
}

#[cfg(test)]
pub(super) async fn pass_on(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Report> {
    let mut report = Report::default();
    let result = pass_inner(http, checkout, namespace, max, now, &mut report).await;
    if let Err(error) = result {
        bail!(
            "resident gate supervision UNKNOWN: {error:#}; complete observation {}",
            serde_json::to_string(&report)?
        );
    }
    Ok(report)
}

/// One pass: the line, then each packet's retained reason written onto
/// it — only when the line was decided, since an UNKNOWN pass has not
/// judged every packet and would delete reasons it never re-read.
async fn pass_inner(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
    report: &mut Report,
) -> Result<()> {
    if max == 0 {
        bail!("gate capacity is zero");
    }
    let gates = pages(http, "kind=gate-run&status=open", report).await?;
    pass_line(http, checkout, namespace, max, now, report, &gates).await?;
    annotate(http, &gates, report, now).await;
    Ok(())
}

async fn pass_line(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
    report: &mut Report,
    gates: &[Value],
) -> Result<()> {
    let mut accepted = Vec::new();
    for row in gates {
        let Some(metadata) = row.get("metadata") else {
            bail!("open gate packet has no metadata");
        };
        if metadata.get(model::SUBMISSION).is_none() {
            continue;
        }
        let packet = row
            .get("id")
            .and_then(Value::as_str)
            .context("gate packet lacks identity")?;
        match submission::original_on(http,packet).await {
            Ok((submission,receipt))=>{
                // The acceptance's identity and the receipt's digest; the
                // receipt itself carries the whole job (e1943ef0).
                report.reads.push(json!({"operation":"original-submission-acceptance","packet":packet,
                    "command":submission.command,"applied_at":receipt.applied_at,"actor":receipt.actor,
                    "event_id":receipt.event_id,
                    "receipt":read_digest("acceptance-receipt",&serde_json::to_value(&receipt)?)}));
                if submission.intent.namespace!=namespace {report.retained.push(json!({"packet":packet,"reason":OTHER_NAMESPACE}));continue;}
                accepted.push((receipt.applied_at,packet.to_owned(),submission));
            }
            Err(error)=>report.retained.push(json!({"packet":packet,"reason":format!("unreadable original submission: {error:#}")})),
        }
    }
    accepted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    if accepted.is_empty() {
        return Ok(());
    }
    // THE GATES HOLD (backlog 95401d82): a declared quiet window on the
    // gate volume. Every accepted request stays in line, its retained
    // reason the hold's own line, except one gated `--despite-hold`, which
    // carries its reason on the packet. Unreadable is UNKNOWN, as the
    // policy read below is — never a launch.
    let hold = crate::gate_hold::standing(&crate::train::rows(
        api(http, reqwest::Method::GET, &crate::gate_hold::path(), None).await?,
    )?);
    if let Some(hold) = &hold {
        let held = crate::gate_hold::held_line(hold);
        report
            .reads
            .push(json!({"operation":"gate-hold","hold":hold.id,"line":held}));
        let kept_back = hold_back(&mut accepted, gates, &hold.id, &held);
        report.retained.extend(kept_back);
    }
    let Some(anchor) = accepted.first().map(|(_, packet, _)| packet.clone()) else {
        return Ok(());
    };
    let policy = staging::policy_on(http).await?;
    report
        .reads
        .push(json!({"operation":"current-gate-queue-policy","policy":policy}));
    // The capacity is the gates station's bays (item 0a78f8f8), read as
    // strictly as the policy: unreadable is UNKNOWN, never a compiled
    // fallback admission.
    let gates_row = staging::gates_on(http).await?;
    let bays = crate::gate::bays_of(&gates_row).context("current gates capacity is invalid")?;
    let max = max.min(bays);
    report
        .reads
        .push(json!({"operation":"current-gate-bays","station":gates_row}));
    let trains = pages(http, "kind=pr-train&status=open", report).await?;
    let cars = pages(http, "kind=ship-a-change&status=open", report).await?;
    // A failed or stale priority read cannot be demoted to zero. A train
    // whose own original gate-run is in `gates` already stands in the
    // line as that packet's native reservation, which the census below
    // counts; its train metadata is not a second place (backlog 279a388e).
    let train_claims = trains_claiming(&trains, gates);
    if !train_claims.is_empty() {
        let firing = api(
            http,
            reqwest::Method::GET,
            &format!("/api/cadence/rules/{TRAIN_RECONCILE_RULE}/last-firing"),
            None,
        )
        .await?;
        report
            .reads
            .push(firing_digest("train-priority-liveness", firing.as_ref()));
        let firing: Option<boss_jobs::cadence::LastFiring> =
            firing.map(serde_json::from_value).transpose()?;
        trains_waiting(&trains, gates, firing.as_ref(), now, QUEUE_PLACE_TTL_SECS)
            .map_err(anyhow::Error::msg)?;
    }
    // Read now, judged after the census: whether a claim counts while a
    // train holds the track depends on whether that train's gate Job is
    // still live (backlog 4ec28c34).
    let dock_firing = if dock_claims(&cars) > 0 {
        let firing = api(
            http,
            reqwest::Method::GET,
            &format!("/api/cadence/rules/{DOCK_REFRESH_RULE}/last-firing"),
            None,
        )
        .await?;
        report
            .reads
            .push(firing_digest("dock-priority-liveness", firing.as_ref()));
        let firing: Option<boss_jobs::cadence::LastFiring> =
            firing.map(serde_json::from_value).transpose()?;
        Some(firing)
    } else {
        None
    };
    let census = native_census_on(http, namespace, "app=gate-runner", &anchor).await?;
    // The retained artifact names the census; the list is not copied in
    // here again (e1943ef0).
    report.reads.push(
        json!({"operation":"native-capacity-census","file_id":census.artifact.file_id,
        "sha256":census.artifact.sha256,"size_bytes":census.artifact.size_bytes}),
    );
    let native = census.list["items"]
        .as_array()
        .context("native census lost items")?;
    if native.iter().any(|job| {
        job.pointer("/metadata/labels/app").and_then(Value::as_str) != Some("gate-runner")
    }) {
        bail!("native capacity census returned a Job outside its declared selector");
    }
    // The bay's own liveness rule (backlog ac8d3157), so the supervisor's
    // count and the reservation that guards its create cannot disagree.
    let live_gates: Vec<String> = native
        .iter()
        .filter(|job| super::bay::job_live(job))
        .filter_map(|job| job.pointer("/metadata/name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let mut occupied = native
        .iter()
        .filter(|job| super::bay::job_live(job))
        .count();
    let dock = match dock_firing {
        Some(firing) => dock_waiting(
            &cars,
            &trains,
            &live_gates,
            firing.as_ref(),
            now,
            QUEUE_PLACE_TTL_SECS,
        )
        .map_err(anyhow::Error::msg)?,
        None => 0,
    };
    // A possibly sent invocation still owns one admission even when a
    // current list cannot prove whether its delayed create will arrive.
    for row in gates {
        let Some(md) = row.get("metadata") else {
            bail!("gate capacity row lacks metadata");
        };
        if md.get(model::OBSERVATION).is_some()
            || (md.get(model::INTENT).is_none() && md.get(attempt::KEY).is_none())
            || native
                .iter()
                .any(|job| job.pointer("/metadata/labels/boss.dev~1packet") == row.get("id"))
        {
            continue;
        }
        let no_child: Result<()> = async {
            let previous = attempt::read(md)?.context("native intent has no guarded attempt")?;
            if previous.state != attempt::State::NotStarted {
                bail!("native send may still take effect");
            }
            let value = md
                .get(model::INTENT)
                .context("no-child attempt lacks its intent")?;
            if md.pointer("/_immutable_metadata/gate_launch_intent") != Some(value) {
                bail!("no-child intent lacks its immutable binding");
            }
            let intent: LaunchIntent = serde_json::from_value(value.clone())?;
            if row.get("id").and_then(Value::as_str) != Some(intent.packet.as_str())
                || intent.namespace != namespace
            {
                bail!("no-child proof belongs to another native target");
            }
            verify_previous_no_child(http, &intent, &previous).await
        }
        .await;
        if let Err(error) = no_child {
            occupied += 1;
            report.retained.push(json!({"packet":row["id"],"reason":format!("possible native occupancy retained: {error:#}")}));
        }
    }
    let legacy: Vec<_> = gates
        .iter()
        .filter(|row| row.pointer("/metadata/gate_submission").is_none())
        .cloned()
        .collect();
    // ONE LINE, BOTH OWNERS (backlog b56bfc29): a legacy place is ahead
    // of a resident request only when it was taken before that request's
    // original acceptance — the comparison a legacy waiter makes the
    // other way round (`mixed_line`). Counting the whole legacy line
    // ahead of every resident let gates queued later take freed bays.
    // Since 64d960e7 that comparison is made within a tier
    // (`line_order::legacy_ahead`).
    let legacy = queue_places(&legacy, now, QUEUE_PLACE_TTL_SECS);
    // THE BAY LINE'S ORDER (backlog 64d960e7): behind the trains' and the
    // dock's claims, urgent work and anything past the age cap first, then
    // cars released at the dock, then the rest by age — each request's
    // standing read off its own packet, its car and the item it answers.
    let row_of = |packet: &str| {
        gates
            .iter()
            .find(|g| g.get("id").and_then(Value::as_str) == Some(packet))
    };
    let ranked_rows: Vec<&Value> = accepted
        .iter()
        .map(|(_, packet, _)| packet.as_str())
        .chain(legacy.iter().map(|(_, packet)| packet.as_str()))
        .filter_map(row_of)
        .collect();
    let ranks = item_ranks_on(http, &ranked_rows, &cars, report).await;
    let standing_of = |packet: &str, at: chrono::DateTime<chrono::Utc>| match row_of(packet) {
        Some(row) => super::line_order::standing(row, &cars, &ranks, at, now),
        None => super::line_order::Standing {
            tier: super::line_order::Tier::Arrival,
            why: "its packet was not in this pass's read, so by age".into(),
        },
    };
    let legacy: Vec<_> = legacy
        .into_iter()
        .map(|(at, packet)| (standing_of(&packet, at).tier, at, packet))
        .collect();
    let mut line: Vec<_> = accepted
        .into_iter()
        .map(|(at, packet, submission)| (standing_of(&packet, at), at, packet, submission))
        .collect();
    line.sort_by(|a, b| (a.0.tier, a.1, &a.2).cmp(&(b.0.tier, b.1, &b.2)));
    for (standing, accepted_at, packet, submission) in line {
        let earlier =
            super::line_order::legacy_ahead(&legacy, (standing.tier, accepted_at, &packet));
        let ahead = train_claims.len() + dock + earlier;
        let mut launch_recorded = false;
        let attempt: Result<bool> = async {
            let current = read_on(http, &packet).await?;
            launch_recorded = current.get("metadata").is_some_and(model::native_observed);
            let head = submission.intent.head.as_str();
            match off_packet(&packet, &current, head)? {
                None => {}
                Some(OffPacket::Refused(why)) => bail!(why),
                // Both halves are immutable, so this never changes:
                // settled like a moved branch (backlog 48b1b7b4).
                Some(OffPacket::HeadDisagrees { source }) => {
                    return Err(NeverLaunches {
                        because: format!(
                            "source head {} disagrees with the immutable request's head {head}; \
                             both were written once at acceptance, so it can never launch",
                            source.as_deref().unwrap_or("(none)")
                        ),
                        evidence: json!({"operation":"immutable-submission","packet":packet,
                            "command":submission.command,"source_sha":source,"intent_head":head}),
                    }
                    .into());
                }
            }
            let source =
                model::source_metadata(current.get("metadata").context("gate lost metadata")?)
                    .map_err(anyhow::Error::msg)?;
            let branch = source
                .get("branch")
                .and_then(Value::as_str)
                .context("durable source lacks branch")?;
            if let Some(name) = existing_on(http, namespace, &packet).await? {
                report.observed.push(packet.clone());
                report
                    .reads
                    .push(json!({"operation":"observed-native","packet":packet,"name":name}));
                return Ok(false);
            }
            source_claim::verify(http, source, &submission.submitted_by, &mut report.reads)
                .await
                .map_err(source_run_ended)?;
            exact_branch(
                http,
                checkout,
                &packet,
                branch,
                &submission.intent.head,
                report,
            )
            .await?;
            match gated_car_guard(
                branch,
                &submission.intent.head,
                &cars,
                source.get("force_regate_reason").and_then(Value::as_str),
            ) {
                GatedGuard::Proceed => {}
                GatedGuard::Forced(note) => report
                    .reads
                    .push(json!({"operation":"original-force-regate","packet":packet,"note":note})),
                // A green that never moved its car is not a twin: the
                // request launches, and the read says why (64d417ca).
                GatedGuard::Wedged(note) => report
                    .reads
                    .push(json!({"operation":"gated-car-wedged","packet":packet,"note":note})),
                // A car already green at this exact head leaves the
                // request nothing to decide — the withdrawal the guard's
                // own message prescribes — so it is settled with the car
                // that vouches, not retained (backlog 48b1b7b4).
                GatedGuard::Refuse(reason) => {
                    let Some(vouch) = vouching_car(branch, head, &cars) else {
                        bail!(reason);
                    };
                    return Err(NeverLaunches {
                        because: format!(
                            "{branch}@{head} is already green and car {} ({}) carries it; \
                             this request has nothing left to decide, so it never launches",
                            vouch.car, vouch.where_it_is
                        ),
                        evidence: json!({"operation":"gated-car-guard","packet":packet,
                            "car":vouch.car,"head":vouch.head,"where":vouch.where_it_is}),
                    }
                    .into());
                }
            }
            if !admits(occupied, ahead, max, Requester::Car) {
                // Waiting only for a bay: its place and why, for its packet.
                let before = report.waiting.len();
                let position = ahead + before + 1;
                let reason = super::line_order::place_text(
                    position,
                    train_claims.len(),
                    dock,
                    earlier + before,
                    &standing.why,
                );
                report
                    .waiting
                    .push(json!({"packet": packet, "position": position, "reason": reason}));
                return Ok(false);
            }
            submission::launch_on(http, &packet, now).await?;
            Ok(true)
        }
        .await;
        match attempt {
            Ok(true) => {
                report.launched.push(packet);
                occupied += 1;
            }
            Ok(false) => {}
            // PROVED never to launch: settled here, by the pass that read
            // the proof, with the cause and the evidence on the packet
            // (backlog 5db91d0e). Before this it was retained on every
            // pass, forever — the conductor's clock no longer settles a
            // resident still in line (b56bfc29) — holding its place
            // ahead of later waiters until someone withdrew it by hand.
            // A settle that cannot complete is retained, never guessed.
            Err(error) => match error.downcast_ref::<NeverLaunches>() {
                Some(never) => {
                    match settle_never_launches(http, &packet, namespace, never, now).await {
                        Ok(receipt) => {
                            report.settled.push(json!({"packet":packet,
                            "verdict":WITHDRAWN_VERDICT,"because":never.because,"receipt":receipt}))
                        }
                        Err(settle) => {
                            let state = if launch_recorded {
                                "native launch already recorded; request retained for reconciliation"
                            } else {
                                "request retained for reconciliation"
                            };
                            report.retained.push(json!({"packet":packet,
                                "reason":format!("{state}; withdrawal was not completed: {settle:#}; current eligibility evidence: {}", never.evidence)}));
                        }
                    }
                }
                None => report
                    .retained
                    .push(json!({"packet":packet,"reason":format!("{error:#}")})),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE RESIDENT LINE UNDER A GATES HOLD (backlog 95401d82): every
    /// accepted request stays in line with the hold's line as its reason,
    /// except one gated `--despite-hold`, which still launches.
    #[test]
    fn the_resident_line_holds_back_every_request_but_one_gated_despite_the_hold() {
        let gates = vec![
            json!({"id": "a", "metadata": {}}),
            json!({"id": "b", "metadata": {
                crate::gate_hold::DESPITE_KEY: "the move's own probe",
                crate::gate_hold::DESPITE_OF_KEY: "hold-A"}}),
            json!({"id": "c", "metadata": {crate::gate_hold::DESPITE_KEY: "  "}}),
        ];
        let mut accepted = vec![
            (1u8, "a".to_string(), ()),
            (2, "b".into(), ()),
            (3, "c".into(), ()),
        ];
        let retained = hold_back(
            &mut accepted,
            &gates,
            "hold-A",
            "held: retire-a-user-volume on w-1",
        );
        assert_eq!(
            accepted
                .iter()
                .map(|(_, p, _)| p.as_str())
                .collect::<Vec<_>>(),
            vec!["b"]
        );
        assert_eq!(
            retained,
            vec![
                json!({"packet": "a", "reason": "held: retire-a-user-volume on w-1"}),
                json!({"packet": "c", "reason": "held: retire-a-user-volume on w-1"}),
            ]
        );
    }

    /// REVIEW R1 (car 0d2e0449): an override names the hold it crossed.
    /// A request gated `--despite-hold` of hold A that is still queued
    /// when A is released and hold B is placed has no reason recorded to
    /// cross B, so it stays in line; one recorded against B goes through.
    #[test]
    fn an_override_for_an_old_hold_does_not_authorize_the_next_hold() {
        let gates = vec![
            json!({"id": "queued-A", "metadata": {
                crate::gate_hold::DESPITE_KEY: "probe specifically for hold A",
                crate::gate_hold::DESPITE_OF_KEY: "hold-A"}}),
            json!({"id": "queued-B", "metadata": {
                crate::gate_hold::DESPITE_KEY: "probe specifically for hold B",
                crate::gate_hold::DESPITE_OF_KEY: "hold-B"}}),
            json!({"id": "unbound", "metadata": {
                crate::gate_hold::DESPITE_KEY: "a reason naming no hold"}}),
        ];
        let mut accepted = vec![
            (1u8, "queued-A".to_string(), ()),
            (2, "queued-B".into(), ()),
            (3, "unbound".into(), ()),
        ];
        let retained = hold_back(&mut accepted, &gates, "hold-B", "held: quiet window B");
        assert_eq!(
            accepted
                .iter()
                .map(|(_, p, _)| p.as_str())
                .collect::<Vec<_>>(),
            vec!["queued-B"]
        );
        assert_eq!(
            retained,
            vec![
                json!({"packet": "queued-A", "reason": "held: quiet window B"}),
                json!({"packet": "unbound", "reason": "held: quiet window B"}),
            ]
        );
    }

    /// Backlog 9f510bcf: the packet itself says why the pass kept it in
    /// line. A new or changed reason is written with when it was first
    /// seen; an unchanged one is not rewritten; a packet no longer
    /// retained (launched, settled, or waiting only for a bay) has its
    /// reason deleted; another namespace's packet is left alone.
    #[test]
    fn each_retained_packet_carries_its_reason_and_loses_it_when_released() {
        let now = model::instant("2026-11-08T03:00:00Z").unwrap();
        let carrying = |reason: &str| {
            json!({crate::gate::QUEUE_RETAINED:
                {"reason": reason, "since": "2026-11-08T01:00:00Z"}})
        };
        let gates = vec![
            json!({"id": "new", "metadata": {}}),
            json!({"id": "same", "metadata": carrying("held: quiet window")}),
            json!({"id": "changed", "metadata": carrying("an older reason")}),
            json!({"id": "released", "metadata": carrying("an older reason")}),
            json!({"id": "never", "metadata": {}}),
            json!({"id": "elsewhere", "metadata": carrying("someone else's reason")}),
        ];
        let report = Report {
            retained: vec![
                json!({"packet": "new", "reason": "original parent step no longer links this source run"}),
                json!({"packet": "same", "reason": "held: quiet window"}),
                json!({"packet": "changed", "reason": "exact source branch read failed"}),
                json!({"packet": "elsewhere", "reason": OTHER_NAMESPACE}),
            ],
            launched: vec!["released".into()],
            ..Report::default()
        };
        let key = crate::gate::QUEUE_RETAINED;
        assert_eq!(
            retained_writes(&gates, &report, now),
            vec![
                (
                    "new".to_string(),
                    json!({key: {"reason": "original parent step no longer links this source run",
                        "since": now}})
                ),
                (
                    "changed".to_string(),
                    json!({key: {"reason": "exact source branch read failed", "since": now}})
                ),
                ("released".to_string(), json!({key: null})),
            ]
        );
    }

    /// Backlog 64d960e7: a request waiting only for a bay carries its
    /// place and why; a moved place is rewritten, an unchanged one is
    /// not, a launched or retained one loses it, and another namespace's
    /// packet is left alone.
    #[test]
    fn each_waiting_request_carries_its_place_and_loses_it_when_it_stops_waiting() {
        let key = boss_jobs::yard::QUEUE_PLACE;
        let place = |position: u64, reason: &str| json!({"position": position, "reason": reason});
        let carrying = |p: Value| json!({ key: p });
        let urgent = "position 2: behind 1 train gate; urgent: answers urgent item 47601829";
        let first =
            "position 3: behind 1 train gate and 1 gate ahead in line; a first gate, by age";
        let gates = vec![
            json!({"id": "new", "metadata": {}}),
            json!({"id": "same", "metadata": carrying(place(2, urgent))}),
            json!({"id": "moved", "metadata": carrying(place(4, first))}),
            json!({"id": "launched", "metadata": carrying(place(2, first))}),
            json!({"id": "retained", "metadata": carrying(place(3, first))}),
            json!({"id": "elsewhere", "metadata": carrying(place(1, first))}),
        ];
        let report = Report {
            waiting: vec![
                json!({"packet": "same", "position": 2, "reason": urgent}),
                json!({"packet": "moved", "position": 3, "reason": first}),
                json!({"packet": "new", "position": 4,
                    "reason": "position 4: behind 1 train gate and 2 gates ahead in line; a first gate, by age"}),
            ],
            launched: vec!["launched".into()],
            retained: vec![
                json!({"packet": "retained", "reason": "exact source branch read failed"}),
                json!({"packet": "elsewhere", "reason": OTHER_NAMESPACE}),
            ],
            ..Report::default()
        };
        assert_eq!(
            place_writes(&gates, &report),
            vec![
                (
                    "new".to_string(),
                    carrying(place(
                        4,
                        "position 4: behind 1 train gate and 2 gates ahead in line; a first gate, by age"
                    ))
                ),
                ("moved".to_string(), carrying(place(3, first))),
                ("launched".to_string(), json!({ key: null })),
                ("retained".to_string(), json!({ key: null })),
            ]
        );
    }

    const REF: &str = "refs/heads/fix/x";
    const HEAD: &str = "097e370a27a0c773cf1ce015e242215ee200f8ec";
    const TIP: &str = "1111111111111111111111111111111111111111";

    /// Backlog 5db91d0e: only a positive answer is permanent. A moved
    /// tip is; a missing branch is only beside the forge's own HEAD on
    /// the same read; silence, a malformed line or a doubled ref holds.
    #[test]
    fn only_a_positive_forge_answer_proves_a_request_will_never_launch() {
        let at = |lines: &[(&str, &str)]| {
            let text: String = lines.iter().map(|(s, r)| format!("{s}\t{r}\n")).collect();
            branch_answer(&text, REF, HEAD)
        };
        assert_eq!(at(&[(TIP, "HEAD"), (HEAD, REF)]), BranchAnswer::AtHead);
        assert_eq!(at(&[(HEAD, REF)]), BranchAnswer::AtHead);
        assert_eq!(
            at(&[(HEAD, "HEAD"), (TIP, REF)]),
            BranchAnswer::Moved(TIP.into())
        );
        assert_eq!(at(&[(TIP, "HEAD")]), BranchAnswer::Absent);
        assert!(matches!(at(&[]), BranchAnswer::Unknown(_)));
        // A ref that only ends like HEAD is not the forge's HEAD.
        assert!(matches!(
            at(&[(TIP, "refs/remotes/origin/HEAD")]),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            at(&[(TIP, "HEAD"), (HEAD, REF), (TIP, REF)]),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            branch_answer("not-a-sha\tHEAD\n", REF, HEAD),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            branch_answer(&format!("{TIP}\n"), REF, HEAD),
            BranchAnswer::Unknown(_)
        ));
    }
}
