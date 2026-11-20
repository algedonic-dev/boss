//! `boss retire <item> <stale|duplicate|decline>` — an item that stands
//! PAST triage, retired onto its withdrawal terminal with the evidence
//! on the record (backlog 7506fced).
//!
//! WHY. Measured 2026-11-19 after UI step 1 (car 4a7ba0bb, train #1838,
//! 1f3a9725e) deleted the pages nine backlog items were filed against.
//! Design 01fa5bdf closes such items as superseded when the step lands.
//! The two still at triage closed through `boss triage <id> stale`; the
//! nine standing at `build` could not, because `boss triage` refuses a
//! packet whose triage is done ("triage is completed — now at build")
//! and the `stale` terminal is pending until something completes the
//! step that routes to it. No supported verb did.
//!
//! THE ROW ALREADY ROUTES IT — so this is a door, not a row change.
//! backlog-item's `build` declares `disposition =
//! stale|duplicate|decline|delivered` and the `stale`, `duplicate` and
//! `declined` terminals each read `steps.build.metadata.disposition`
//! (6c114a23), exactly as they read `measure`'s. What was missing is the
//! write. The generic `boss step complete --step build --field
//! disposition=stale` reaches the terminal, and gets three things wrong
//! on the way:
//!
//! 1. `build` declares no `evidence`, so the generic door refuses the
//!    measurement as an undeclared name — a withdrawal with nothing
//!    behind it, the class B write `boss triage` exists to stop;
//! 2. it completes a claim another actor holds and leaves that holder
//!    named on the completed step, so the record credits the build to
//!    the actor who did not do it;
//! 3. the step keeps its `agent_run` edge, and completing it fires
//!    `agent-run-delivers-when-its-step-is-done`, which lands the run
//!    that held it `delivered` — a run that delivered nothing.
//!
//! A row change could not reach these nine anyway: an in-flight packet
//! stays pinned to the version it was admitted under, and backlog-item
//! is a held kind whose publish is David's. The verb reads whatever the
//! pinned step declares.
//!
//! JUDGED BEFORE THE FIRST WRITE. The step is the one open step past
//! triage whose declared `disposition` admits the value; an open
//! agent-run naming the item, a run named on the step that is not
//! closed or cannot be read, and an open car answering the item each
//! refuse; the completion is judged by the registry's own validator.
//! The step is judged as ONE versioned read, taken before the run and
//! car reads, and the retirement is ONE conditional write (`POST
//! .../complete-if`): the disposition, the evidence, the landing commit,
//! the run edge cleared and the `released` record a hand release writes,
//! landed with the completion only while the step still reads at the
//! status, holder and version judged — so a claim, a run or a write that
//! arrives after the reads refuses it whole, and keeps what it wrote
//! (review 956f882f finding 1). Read back, as every verb here.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::{
    Wire, as_it_will_stand, confirm_completed, contract_check, declared_fields, enum_values,
    is_open, release_patch, short, standing, status_of, step_after, step_id, title_of,
};

/// The dispositions that WITHDRAW an item. `delivered` is not one — it
/// opens `prove-delivery`, which closes on a live read-back — and
/// `build`, `design` and `verify` are routes, which are triage's and
/// measure's to choose.
pub(super) const RETIREMENTS: [&str; 3] = ["stale", "duplicate", "decline"];

fn slug_of(step: &Value) -> &str {
    step.get("spec_slug").and_then(Value::as_str).unwrap_or("?")
}

fn text<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn id8(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The step a retirement completes: the ONE open step past triage whose
/// declared `disposition` admits `disposition`. Refused, naming where
/// the packet stands, for anything else — a packet still at triage is
/// `boss triage`'s, and a step whose row routes no refutation (a review,
/// `prove-delivery`) has no withdrawal for this verb to write.
pub(super) fn retirable<'a>(packet: &'a Value, disposition: &str) -> Result<&'a Value, String> {
    if !RETIREMENTS.contains(&disposition) {
        return Err(format!(
            "`{disposition}` is not a retirement — one of [{}]. `delivered` opens \
             prove-delivery, and a route (`build`, `design`, `verify`) is the triage's or \
             the measurement's to choose",
            RETIREMENTS.join(", ")
        ));
    }
    let status = packet.get("status").and_then(Value::as_str).unwrap_or("?");
    if status != "open" {
        return Err(format!(
            "packet {} is {status} — nothing to retire; {}",
            short(packet),
            standing(packet)
        ));
    }
    let steps = crate::envelope::steps(packet);
    if steps.iter().any(|s| slug_of(s) == "triage" && is_open(s)) {
        return Err(format!(
            "packet {} still stands at `triage`, whose door is `boss triage {} {disposition} \
             --evidence-file <PATH>`",
            short(packet),
            short(packet)
        ));
    }
    let admits = |s: &Value| {
        declared_fields(s)
            .iter()
            .find(|f| f.name == "disposition")
            .and_then(|f| enum_values(&f.field_type).map(|v| v.contains(&disposition)))
            .unwrap_or(false)
    };
    let found: Vec<&Value> = steps
        .into_iter()
        .filter(|s| is_open(s) && slug_of(s) != "triage" && admits(s))
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "packet {} has no open step whose declared `disposition` admits `{disposition}` — \
             {}. The row decides where a refutation may land, and none stands open here",
            short(packet),
            standing(packet)
        )),
        many => Err(format!(
            "packet {} has {} open steps that admit `{disposition}` ({}) — refusing to guess \
             which one the evidence answers",
            short(packet),
            many.len(),
            many.iter()
                .map(|s| slug_of(s))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// What the retirement records on the step — pure, so each refusal is
/// pinned by name. The keys are the ones a triage writes (`disposition`,
/// `evidence`, `duplicate_of`), so a reader finds a withdrawal the same
/// way on whichever step routed it, plus `landed_in`: the commit on the
/// forge's main that superseded the claim, already proved landed by the
/// verb's edge.
pub(super) fn retire_writes(
    disposition: &str,
    evidence: &str,
    duplicate_of: Option<&str>,
    landed_in: Option<&str>,
) -> Result<Map<String, Value>, String> {
    if evidence.trim().is_empty() {
        return Err(
            "the evidence is blank — a withdrawal with nothing behind it is the class B write \
             this verb exists to stop; the next reader measures against it"
                .to_string(),
        );
    }
    let mut writes = Map::new();
    writes.insert("disposition".into(), json!(disposition));
    writes.insert("evidence".into(), json!(evidence.trim()));
    match (disposition == "duplicate", duplicate_of) {
        (true, Some(of)) => {
            writes.insert("duplicate_of".into(), json!(of));
        }
        (true, None) => {
            return Err(
                "`duplicate` needs --of <packet>: the original is recorded as `duplicate_of`, \
                 and a duplicate that names nothing sends the next reader hunting for it"
                    .to_string(),
            );
        }
        (false, Some(_)) => {
            return Err(format!(
                "--of only accompanies `duplicate` — `{disposition}` records no original"
            ));
        }
        (false, None) => {}
    }
    if let Some(sha) = landed_in {
        writes.insert("landed_in".into(), json!(sha));
    }
    Ok(writes)
}

/// The refusal when work is still running on the item: an OPEN
/// agent-run that names it (`metadata.packet`, re-read off each row,
/// since a server that ignored the containment filter would answer the
/// unfiltered page), or the run the step's own `agent_run` edge names
/// when that run is not closed — or could not be read, because an
/// unread run is not a dead one. `None` when nothing runs.
pub(super) fn run_refusal(
    item: &str,
    runs: &[Value],
    edge: Option<(&str, Result<Value, String>)>,
) -> Option<String> {
    let open: Vec<String> = runs
        .iter()
        .filter(|r| text(r, "/status") == "open" && text(r, "/metadata/packet") == item)
        .map(|r| {
            format!(
                "{} (on `{}`)",
                text(r, "/id"),
                match text(r, "/metadata/step") {
                    "" => "?",
                    s => s,
                }
            )
        })
        .collect();
    if !open.is_empty() {
        return Some(format!(
            "{} open agent-run(s) name {}: {}. Retiring the item under a working executor \
             takes the step out from under it. If the run is working, let it finish or report; \
             if it is dead, end it on the record first, then retire",
            open.len(),
            id8(item),
            open.join(", ")
        ));
    }
    match edge {
        Some((id, Ok(run))) => {
            let status = text(&run, "/status");
            (!matches!(status, "closed" | "cancelled")).then(|| {
                format!(
                    "the step names run {id} (`agent_run`), whose status is {} — refused rather \
                     than assume it is over; end it on the record first",
                    if status.is_empty() {
                        "unreadable"
                    } else {
                        status
                    }
                )
            })
        }
        Some((id, Err(why))) => Some(format!(
            "the step names run {id} (`agent_run`), which could not be read ({why}) — an unread \
             run is not a dead one"
        )),
        None => None,
    }
}

/// The refusal when an OPEN car answers the item (`backlog_item`, or
/// `also_answers`): the item is that car's, and closes when it lands.
/// Stricter than the `build` contract (`item-awaits-its-car-v1`), which
/// holds only a car waiting on its proof — a docked or boarding car
/// would land onto an item already withdrawn.
pub(super) fn car_refusal(item: &str, cars: &[Value]) -> Option<String> {
    let answering: Vec<String> = cars
        .iter()
        .filter(|c| text(c, "/kind") == "ship-a-change" && text(c, "/status") == "open")
        .filter(|c| {
            text(c, "/metadata/backlog_item") == item
                || c.pointer("/metadata/also_answers")
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(item)))
        })
        .map(|c| format!("{} ({})", id8(text(c, "/id")), text(c, "/metadata/branch")))
        .collect();
    (!answering.is_empty()).then(|| {
        format!(
            "open car(s) answer {}: {} — the item is that car's and closes when it lands. If \
             the car is abandoned, end it first",
            id8(item),
            answering.join(", ")
        )
    })
}

/// The landing commit, PROVED landed: `given` resolves to a commit in
/// `repo` and that commit is an ancestor of `main` (the forge's, read by
/// the caller). The full sha is what gets recorded — never the
/// characters that were typed.
pub(super) fn landed_commit(
    repo: &std::path::Path,
    given: &str,
    main: &str,
) -> Result<String, String> {
    let git = crate::car_retire::git;
    let sha = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{given}^{{commit}}"),
        ],
    )
    .map_err(|_| format!("--landed-in {given} is not a commit in this clone"))?;
    git(repo, &["merge-base", "--is-ancestor", &sha, main]).map_err(|_| {
        format!(
            "--landed-in {given} ({}) is not on the forge's main {} — a change that has not \
             landed supersedes nothing",
            id8(&sha),
            &main[..main.len().min(12)]
        )
    })?;
    Ok(sha)
}

/// [`landed_commit`] against the forge's main, fetched into the clone
/// the verb runs in when it lags (`car_retire::forge_main`).
pub(super) fn landed_on_forge_main(repo: &std::path::Path, given: &str) -> Result<String> {
    let main = crate::car_retire::forge_main(repo)?;
    landed_commit(repo, given, &main).map_err(|e| anyhow!("boss retire: REFUSED — {e}"))
}

/// THE ONE GUARDED WRITE (review 956f882f finding 1). The first cut
/// wrote the evidence, deleted the run edge and recorded `released`
/// through the merge door, unconditionally, and only then sent the
/// completing PUT: a new executor admitted between the verb's reads and
/// that PATCH lost its run edge before anything refused the stale
/// caller, and a process death between the two writes left a retirement
/// on the record under a live claim. The jobs API's conditional
/// completion door (`POST .../complete-if`, `conditional_completion.rs`)
/// is the boundary that cannot do either: it lands the evidence (a null
/// deletes) and the completion in ONE write, only on a row still at the
/// status, holder and version the verb judged — under the row's own
/// version compare — and writes nothing otherwise. Its operation id
/// makes a retry of the same attempt a replay of its receipt, never a
/// second write.
pub(super) fn completion_request(
    operation_id: &uuid::Uuid,
    judged: &Value,
    version: &str,
    evidence: &Map<String, Value>,
) -> Value {
    json!({
        "operation_id": operation_id,
        "expected": {
            "status": judged.get("status").cloned().unwrap_or(Value::Null),
            "assignee_id": judged.get("assignee_id").cloned().unwrap_or(Value::Null),
            "version": version,
        },
        "evidence": evidence,
    })
}

/// What the conditional door answered, read off the call's result.
#[derive(Debug, PartialEq)]
pub(super) enum Answer {
    /// Completed now, or replayed from this operation id's receipt.
    Done(Value),
    /// A definitive refusal: the door wrote nothing.
    Refused(String),
    /// No answer that says what happened — a transport failure, a 5xx,
    /// a body that is not the door's. The write may have landed.
    Unknown(String),
}

/// PURE: the door's outcome, from the body it answered or the error the
/// wire raised (`<service> POST <path> -> <status>: <body>`).
pub(super) fn answer_of(result: &Result<Option<Value>>) -> Answer {
    let outcome = |body: &Value| -> Option<Answer> {
        let receipt = || body.get("receipt").cloned().unwrap_or(Value::Null);
        Some(match body.get("outcome")?.as_str()? {
            "completed" | "replayed" => Answer::Done(receipt()),
            "precondition_failed" => Answer::Refused(
                "the step moved after it was judged — its status, holder or version is not \
                 what this retirement read (a claim, a run or a write landed in between), and \
                 whoever holds it now keeps it"
                    .into(),
            ),
            "terminal_conflict" => Answer::Refused(
                "the step was completed by another write after it was judged".into(),
            ),
            "unsupported_capability" => Answer::Refused(format!(
                "this step's kind has no conditional completion: {}",
                body.get("error").and_then(Value::as_str).unwrap_or("?")
            )),
            "not_found" => Answer::Refused("the step is gone".into()),
            _ => return None,
        })
    };
    match result {
        Ok(Some(body)) => outcome(body).unwrap_or_else(|| {
            Answer::Unknown(format!("an answer that is not the door's: {body}"))
        }),
        Ok(None) => Answer::Unknown("an empty answer".into()),
        Err(e) => {
            let said = format!("{e:#}");
            let body = said.find('{').and_then(|at| {
                serde_json::Deserializer::from_str(&said[at..])
                    .into_iter::<Value>()
                    .next()?
                    .ok()
            });
            if let Some(answer) = body.as_ref().and_then(outcome) {
                return answer;
            }
            // A 4xx the door raised before judging (policy, a bad body)
            // wrote nothing either; anything else says nothing.
            match said.split(" -> ").nth(1).map(str::trim_start) {
                Some(status) if status.starts_with('4') => Answer::Refused(said),
                _ => Answer::Unknown(said),
            }
        }
    }
}

/// The `metadata=` containment filter for a flat document — the only
/// shape the server accepts (`boss_jobs::metadata_containment::RULE`).
fn containment(doc: Value) -> String {
    let doc = doc.to_string();
    format!(
        "metadata={}",
        percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
    )
}

/// Every open row of a filtered list, narrowed by the caller's own
/// re-read of each row. `filter` is the query's own narrowing —
/// [`containment`], or a `metadata_has=` key-presence read.
///
/// ONE STATEMENT, NOT A PAGE WALK: each answer is an absence that
/// permits the retirement's write, and no read paged on offset can
/// prove one — the rows before the offset can change between pages.
/// When unrelated cars left the set, the answering car moved below the
/// offset unseen (review 24ab880a); when as many older cars took the
/// key as left it, the total, every page's size and every id's
/// uniqueness all held and it slipped past just the same (review
/// ac23854d). So the set is asked for in ONE request at the server's
/// largest limit: the page is one `SELECT` (`postgres.rs` `list_jobs`),
/// one snapshot, and is whole exactly when it returned fewer rows than
/// the limit the server APPLIED (echoed as `limit`) and as many as its
/// `total`. Anything else refuses before any write — a set that needs
/// a second page is not read as an absence, and the `total` (a second
/// statement) disagreeing with the snapshot is a set still moving.
async fn list(wire: &Wire, kind: &str, filter: &str) -> Result<Vec<Value>> {
    let path = format!("/api/jobs?kind={kind}&status=open&{filter}&limit={ONE_STATEMENT}");
    let body = wire
        .call(reqwest::Method::GET, &path, None)
        .await?
        .ok_or_else(|| anyhow!("{path} answered no body"))?;
    let total = crate::train::list_total(&body)?;
    let applied = body
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|l| usize::try_from(l).ok())
        .ok_or_else(|| {
            anyhow!("{path} echoed no applied `limit`; its page cannot be judged whole")
        })?;
    let rows = crate::train::rows(Some(body))?;
    if rows.len() >= applied || rows.len() != total {
        bail!(
            "the open {kind} rows could not be read in one statement: {} returned under an \
             applied limit of {applied}, of a total of {total} — an absence read across pages, \
             or from a set still moving, is not an absence",
            rows.len()
        );
    }
    Ok(rows)
}

/// The largest `limit` the jobs API applies (`MAX_LIMIT` in
/// `boss-jobs` `http/mod.rs`). Not a pin: [`list`] judges the page by
/// the limit the server ECHOES, so a server that clamps lower is
/// refused by its own number, never read as whole.
const ONE_STATEMENT: usize = 1000;

pub(crate) async fn retire(
    wire: &Wire,
    item: &str,
    disposition: &str,
    evidence: &str,
    of: Option<&str>,
    landed_in: Option<&str>,
) -> Result<()> {
    let packet = wire.resolve(item, None).await?;
    let step =
        retirable(&packet, disposition).map_err(|e| anyhow!("boss retire: REFUSED — {e}"))?;
    let jid = crate::envelope::job_id(&packet).context("the packet has no id")?;
    let sid = step_id(step)?;
    let slug = slug_of(step).to_string();
    let original = match of {
        Some(given) => Some(wire.resolve(given, None).await.with_context(|| {
            format!("--of {given}: the original a duplicate names must be a packet")
        })?),
        None => None,
    };
    let original_id = original
        .as_ref()
        .map(|o| crate::envelope::job_id(o).context("the original has no id"))
        .transpose()?;
    // A DUPLICATE OF ITSELF names no original to carry the claim (review
    // 956f882f finding 2) — judged on the full id the --of resolved to,
    // so eight characters and the uuid are refused alike.
    if original_id == Some(jid) {
        bail!(
            "boss retire: REFUSED — --of resolves to {} itself; a duplicate names a DIFFERENT \
             packet that carries the claim; nothing was written",
            short(&packet)
        );
    }
    let mut writes = retire_writes(disposition, evidence, original_id, landed_in)
        .map_err(|e| anyhow!("boss retire: REFUSED — {}: {e}", short(&packet)))?;
    contract_check(step, &as_it_will_stand(step, &writes))
        .map_err(|e| anyhow!("boss retire: REFUSED — {} `{slug}`: {e}", short(&packet)))?;

    // THE JUDGED ROW, with its version, read BEFORE the run and car
    // reads: every check below is judged against this row, and the one
    // write lands only while the step still reads exactly so. A run
    // claimed, an edge written or a holder changed after this read moves
    // the version, and the write is refused with nothing on the record.
    let versioned = wire
        .call(
            reqwest::Method::GET,
            &format!("/api/jobs/{jid}/steps/{sid}/version"),
            None,
        )
        .await?
        .context("the step's version read back empty")?;
    let version = versioned
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .context("the step's version read carries no version token")?
        .to_string();
    let judged = versioned
        .get("step")
        .filter(|s| is_open(s) && slug_of(s) == slug)
        .with_context(|| {
            format!(
                "boss retire: REFUSED — {}'s `{slug}` is no longer open; nothing was written",
                short(&packet)
            )
        })?;
    let step = judged;

    let runs = list(
        wire,
        crate::dispatch::RUN_KIND,
        &containment(json!({ "packet": jid })),
    )
    .await
    .context("reading the open agent-runs that name this item — an unread run is not a dead one")?;
    let edge = step
        .pointer(&format!("/metadata/{}", boss_jobs::agent_runs::EDGE_KEY))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    // Read fresh by id, never inferred from the list: a row the list
    // returned for another item says nothing about this edge.
    let edge_read = match edge {
        Some(id) => Some((id, wire.packet(id).await.map_err(|e| format!("{e:#}")))),
        None => None,
    };
    if let Some(why) = run_refusal(jid, &runs, edge_read) {
        bail!("boss retire: REFUSED — {why}; nothing was written");
    }
    let mut cars = list(
        wire,
        "ship-a-change",
        &containment(json!({ "backlog_item": jid })),
    )
    .await
    .context("reading the open cars that answer this item")?;
    // `also_answers` is a LIST of full item ids (job_edges
    // `job_id_list`), and containment accepts only flat strings
    // (9afea697): the array document this sent was a 400 on every
    // retirement (backlog 559b37a4). Read every open car carrying the
    // key through the key-presence door, as `prior_work::read_cars`
    // does, and let `car_refusal` judge membership off each whole row.
    // Slim steps: a listed job's own metadata is whole without
    // `full=true`, and whole steps would only swell the one page.
    cars.extend(
        list(
            wire,
            "ship-a-change",
            &format!("metadata_has={}", boss_jobs::car::ALSO_ANSWERS),
        )
        .await
        .context("reading the open cars that also answer this item")?,
    );
    if let Some(why) = car_refusal(jid, &cars) {
        bail!("boss retire: REFUSED — {why}; nothing was written");
    }

    // THE CLAIM GOES WITH ITS REASON: the run edge cleared and the
    // `released` record `boss step release` writes, in the same write as
    // the evidence, whenever there is a holder or a run to take it from.
    // Through the one sanctioned wall source, as the release. The holder
    // stays NAMED on the completed step — the conditional door completes
    // and does not reassign — so the record reads: held by them, taken
    // from them for this reason (`released`), completed by the retirer.
    let held_by = step
        .get("assignee_id")
        .and_then(Value::as_str)
        .filter(|h| !h.trim().is_empty())
        .map(str::to_string);
    if held_by.is_some() || edge.is_some() {
        let why = format!("retired `{disposition}` by boss retire — the evidence is on this step");
        let patch = release_patch(
            step,
            &why,
            wire.caller_id(),
            &boss_clock_client::wall_now().to_rfc3339(),
        );
        if let Value::Object(p) = patch {
            writes.extend(p);
        }
    }

    // ONE WRITE, GUARDED, AND RETRIED ONLY AS ITSELF. An answer that
    // says nothing (a transport failure, a 5xx) is retried once with the
    // SAME operation id, which the door answers with the receipt of a
    // write that landed, or judges afresh against the same expectation
    // if none did — never a second retirement.
    let operation = uuid::Uuid::new_v4();
    let request = completion_request(&operation, step, &version, &writes);
    let path = format!("/api/jobs/{jid}/steps/{sid}/complete-if");
    let mut answer = answer_of(
        &wire
            .call(reqwest::Method::POST, &path, Some(request.clone()))
            .await,
    );
    if let Answer::Unknown(first) = &answer {
        eprintln!(
            "boss retire: the completion's answer said nothing ({first}) — asking again as \
             operation {operation}"
        );
        answer = answer_of(&wire.call(reqwest::Method::POST, &path, Some(request)).await);
    }
    let receipt = match answer {
        Answer::Done(receipt) => receipt,
        Answer::Refused(why) => {
            bail!(
                "boss retire: REFUSED — {}: {why}; nothing was written",
                short(&packet)
            )
        }
        Answer::Unknown(why) => bail!(
            "boss retire: UNKNOWN — {}'s `{slug}` completion (operation {operation}) got no \
             answer twice ({why}). It either landed whole or not at all: `boss job get {}` \
             shows `{slug}` completed with this evidence, or still {} — then run this again",
            short(&packet),
            short(&packet),
            status_of(step)
        ),
    };

    // The receipt is the fact of the write; the read-back confirms what
    // it holds. A read-back that fails does not unsay the receipt.
    let receipt_line = format!(
        "receipt: operation {operation}, event {}",
        receipt
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("?")
    );
    let after = wire.packet(jid).await.with_context(|| {
        format!("`{slug}` COMPLETED ({receipt_line}), but the packet could not be read back")
    })?;
    let now = step_after(&after, sid)?;
    confirm_completed(now, &writes)
        .with_context(|| format!("`{slug}` answered completed ({receipt_line})"))?;
    let closed = matches!(
        after.get("status").and_then(Value::as_str),
        Some("closed" | "cancelled")
    );
    let at_terminal = crate::envelope::steps(&after)
        .into_iter()
        .any(|s| is_open(s) && s.get("kind").and_then(Value::as_str) == Some("outcome"));
    println!(
        "boss retire: {} \"{}\" — `{slug}` completed: {disposition}{}{}{}\n  {}",
        short(&packet),
        title_of(&packet),
        original
            .as_ref()
            .map(|o| format!(" of {} \"{}\"", short(o), title_of(o)))
            .unwrap_or_default(),
        landed_in
            .map(|sha| format!(", landed in {}", &sha[..sha.len().min(12)]))
            .unwrap_or_default(),
        held_by
            .map(|h| format!(", released from {h}"))
            .unwrap_or_default(),
        standing(&after)
    );
    println!("  {receipt_line}");
    if !closed && !at_terminal {
        eprintln!(
            "boss retire: WARNING — {} reads neither closed nor at a terminal step after the \
             write; the row this packet is pinned to may route `{disposition}` from `{slug}` \
             nowhere. Read it: boss job get {}",
            short(&after),
            short(&after)
        );
    }
    Ok(())
}
