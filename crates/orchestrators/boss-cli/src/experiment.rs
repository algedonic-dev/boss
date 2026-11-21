//! `boss experiment start` / `boss experiment stop` — the operator's
//! writes for a Tier 2 split, as verbs (backlog 75296c37).
//!
//! WHY. Split admission (boss-jobs `experiments.rs`, built for
//! 6ea5a12a) worked the first time it met live traffic —
//! protocol-experiment 4aa60dc9, an A/A on agent-run, 2026-10-16: every
//! agent-run admitted while the split was in force was stamped with an
//! arm and pinned to that arm's version. But every write the OPERATOR
//! made to get there was a raw `boss-api` call. The candidate draft was
//! a hand-built `PUT /api/workflows/{kind}` (two 422s to learn the row
//! needs `version`, `status` and `created_at`, and nothing compared the
//! draft to the body intended); the split went in force by a hand-typed
//! metadata PATCH (nothing checked that both versions existed, that no
//! other experiment governed the kind, or that `run`'s `started_at`
//! matched); it came out by a PATCH of nulls and a DELETE. Two of those
//! writes stopped on the permission classifier for David to approve by
//! hand, while the same session's `boss workflow publish` and `boss job
//! convert` ran unprompted because they are verbs. The experiment is
//! the sanctioned way a draft meets traffic (design d8771dec), and it
//! was the one protocol lever with no door.
//!
//! `start` REFUSES BEFORE IT WRITES: a packet that is not an open
//! protocol-experiment, one that already declares a split, a `run` step
//! that is not the next one, a share outside 0..=100, another open
//! experiment naming the kind, no active version to be the control, or
//! a draft already in the registry (publish promotes whatever draft
//! exists, so the candidate must be the only one). The candidate row is
//! read with `boss workflow publish`'s own loader, typed as the server
//! types it — the 422 is answered here, naming the field — linted, made
//! a DRAFT, and read back and compared to what was sent with the drift
//! lint's own comparator (`bootstrap::workflow_changes`). The four keys
//! then go on in ONE merge beside a `split_started` record, are read
//! back, and `run` is completed with `started_at` = the instant of that
//! write. A failed write after the draft discards the draft it made.
//!
//! `stop` takes the split out of force FIRST, whatever it then decides
//! about the candidate, in one merge that also writes the
//! `split_stopped` record, then settles the candidate by the `decide`
//! step's verdict — or, when that settle is refused (a candidate another
//! hand published, a verdict carrying a correction), records it
//! `unsettled` and says why (review 5308927f). `promote` publishes it — refused when
//! another draft would be promoted in its place, or when the control is
//! no longer the active version (the experiment never measured what it
//! would replace) — and reads the active row back. Any other verdict,
//! or none yet, withdraws it: discarded when no packet ran it, else
//! REJECTED through backlog 50ded63d's door, because a draft packets
//! ran refuses the discard (ce8b7d66). On a server without that door
//! the verb refuses by name and the draft stays; the split is already
//! out. The candidate's status is read back, never assumed, and written
//! into the record, and an interrupted stop resumes from it.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::steps::{Given, Wire};
use boss_jobs::experiments::EXPERIMENT_KIND;

/// The record `start` leaves on the experiment packet.
pub(crate) const STARTED_KEY: &str = "split_started";
/// The record `stop` leaves on the experiment packet.
pub(crate) const STOPPED_KEY: &str = "split_stopped";

/// The four job-metadata keys split admission reads. `experiments.rs`
/// owns the contract and spells them inline; the pin is the first test
/// below, which hands what this module writes to admission's own
/// reader rather than comparing two lists (CLAUDE.md §9a).
const KIND_KEY: &str = "kind_under_test";
const CONTROL_KEY: &str = "control_version";
const CANDIDATE_KEY: &str = "candidate_version";
const SPLIT_KEY: &str = "split";
const DECLARATION_KEYS: [&str; 4] = [KIND_KEY, CONTROL_KEY, CANDIDATE_KEY, SPLIT_KEY];

/// What the candidate IS once a stop has settled it. Anything else the
/// record says (`pending`, `left-draft`) is a stop still owed.
const SETTLED: [&str; 4] = ["published", "discarded", "rejected", "retired"];

/// One split: the kind whose admission splits, its two arms, and the
/// candidate's share in percent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub kind: String,
    pub control: i32,
    pub candidate: i32,
    pub split: u8,
}

/// The merge that puts `d` in force.
pub(crate) fn declaration_body(d: &Declaration) -> Map<String, Value> {
    [
        (KIND_KEY, json!(d.kind)),
        (CONTROL_KEY, json!(d.control)),
        (CANDIDATE_KEY, json!(d.candidate)),
        (SPLIT_KEY, json!(d.split)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The merge that takes a split out of force: each key sent as null,
/// which the merge door reads as a delete. Nothing else is touched.
pub(crate) fn withdrawal_body() -> Map<String, Value> {
    DECLARATION_KEYS
        .iter()
        .map(|k| (k.to_string(), Value::Null))
        .collect()
}

/// The split `metadata` declares, read by ADMISSION'S OWN reader
/// (`governing_experiment`) on a probe packet, so what this verb calls
/// the split is what admission calls it — numeric strings and the
/// default share included. `Ok(None)` when no key is there; a refusal
/// naming every key when some are there and admission reads no split
/// from them, because a stop must not guess which split that was.
pub(crate) fn declared(metadata: &Value) -> Result<Option<Declaration>, String> {
    let held = |k: &str| metadata.get(k).filter(|v| !v.is_null());
    if DECLARATION_KEYS.iter().all(|k| held(k).is_none()) {
        return Ok(None);
    }
    let read = held(KIND_KEY).and_then(Value::as_str).and_then(|kind| {
        let mut probe = boss_core::job::Job::new(
            EXPERIMENT_KIND,
            boss_core::job::Subject::new("custom", "split"),
            "probe",
            "probe",
            boss_core::job::Priority::Standard,
            // A probe, never stored: its date orders nothing here.
            chrono::NaiveDate::default(),
        );
        probe.status = boss_core::job::JobStatus::Open;
        probe.metadata = metadata.clone();
        boss_jobs::experiments::governing_experiment(std::slice::from_ref(&probe), kind).map(|s| {
            Declaration {
                kind: kind.to_string(),
                control: s.control_version,
                candidate: s.candidate_version,
                split: s.split,
            }
        })
    });
    read.map(Some).ok_or_else(|| {
        let listing = DECLARATION_KEYS
            .iter()
            .map(|k| {
                format!(
                    "{k}={}",
                    held(k).map_or_else(|| "absent".to_string(), Value::to_string)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "the packet holds {listing}, and admission reads no split from that (both \
             versions must be numbers, `split` 0-100) — this verb will not guess which split \
             it was"
        )
    })
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The status `version` holds in a version list, if it is there.
fn status_of(versions: &[Value], version: i32) -> Option<&str> {
    versions
        .iter()
        .find(|v| v.get("version").and_then(Value::as_i64) == Some(i64::from(version)))
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
}

fn active_version(versions: &[Value]) -> Vec<i32> {
    versions
        .iter()
        .filter(|v| v.get("status").and_then(Value::as_str) == Some("active"))
        .filter_map(|v| v.get("version").and_then(Value::as_i64))
        .filter_map(|v| i32::try_from(v).ok())
        .collect()
}

/// Everything `start` refuses, judged before its first write, on the
/// packet, every open experiment and the kind's version list. Answers
/// the control: the kind's one active version.
pub(crate) fn start_refusal(
    packet: &Value,
    kind: &str,
    split: u8,
    open_experiments: &[Value],
    versions: &[Value],
) -> Result<i32, String> {
    let id = crate::envelope::job_id(packet).unwrap_or("?");
    if field(packet, "kind") != EXPERIMENT_KIND {
        return Err(format!(
            "packet {} is a `{}`, not a {EXPERIMENT_KIND} — a split is declared on an \
             experiment packet only",
            short(id),
            field(packet, "kind")
        ));
    }
    if field(packet, "status") != "open" {
        return Err(format!(
            "packet {} is {}: admission consults OPEN experiments only, so a split declared \
             on it would never be in force",
            short(id),
            field(packet, "status")
        ));
    }
    let md = packet.get("metadata").cloned().unwrap_or(Value::Null);
    if let Some(k) = DECLARATION_KEYS
        .iter()
        .find(|k| md.get(**k).is_some_and(|v| !v.is_null()))
    {
        return Err(format!(
            "packet {} already declares `{k}` — one experiment, one split; `boss experiment \
             stop` it first",
            short(id)
        ));
    }
    crate::steps::open_step(packet, "run")
        .map_err(|e| format!("`run` is the step this verb completes, and {e}"))?;
    if split > 100 {
        return Err(format!(
            "--split {split} is not a share: it is the percent of admissions sent to the \
             candidate, 0-100"
        ));
    }
    if let Some(other) = open_experiments
        .iter()
        .filter(|e| crate::envelope::job_id(e) != Some(id))
        .find(|e| {
            e.pointer(&format!("/metadata/{KIND_KEY}"))
                .and_then(Value::as_str)
                == Some(kind)
        })
    {
        return Err(format!(
            "open experiment {} already names {kind} as its {KIND_KEY} — admission follows \
             whichever opened first, so a second split on the kind measures nothing it \
             declared",
            short(crate::envelope::job_id(other).unwrap_or("?"))
        ));
    }
    let control = match active_version(versions).as_slice() {
        [v] => *v,
        [] => {
            return Err(format!(
                "{kind} has no active version — a split needs a control to measure against"
            ));
        }
        many => return Err(format!("{kind} reads {} active versions", many.len())),
    };
    if let Some(stale) = crate::workflow::blocking_draft(versions, None) {
        return Err(format!(
            "a draft of {kind} v{stale} is already in the registry. The candidate must be the \
             only draft: publish takes no body and promotes whatever draft exists, so a promote \
             could put v{stale} live in the candidate's place. Settle it first (`boss workflow \
             discard {kind} {stale}` if it is no one's)"
        ));
    }
    Ok(control)
}

/// What `stop` does with the candidate once the split is out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Publish the draft: the verdict is promote.
    Publish,
    /// Discard it, or reject it when packets ran it.
    Withdraw,
    /// Already settled — what it IS, for the record.
    Already(&'static str),
}

/// The candidate's fate, decided by the verdict and the version list as
/// they stand. Refuses every shape in which acting would put the wrong
/// text live or undo a hand it cannot see.
pub(crate) fn stop_plan(
    verdict: Option<&str>,
    d: &Declaration,
    versions: &[Value],
) -> Result<Stop, String> {
    let (kind, c) = (&d.kind, d.candidate);
    let status = status_of(versions, c);
    if verdict == Some("promote") {
        return match status {
            Some("draft") => {
                if let Some(other) = crate::workflow::blocking_draft(versions, Some(c)) {
                    return Err(format!(
                        "a second draft, {kind} v{other}, sits beside the candidate v{c}: \
                         publish takes no body and promotes the newest draft, so it could put \
                         v{other} live in the candidate's place. Settle v{other} first"
                    ));
                }
                match active_version(versions).as_slice() {
                    [a] if *a == d.control => Ok(Stop::Publish),
                    [a] => Err(format!(
                        "the active version of {kind} is v{a}, not the control v{} this \
                         experiment measured against — promoting v{c} would replace a version \
                         it never measured",
                        d.control
                    )),
                    _ => Err(format!(
                        "{kind} has no single active version for v{c} to supersede"
                    )),
                }
            }
            Some("active") => Ok(Stop::Already("published")),
            other => Err(format!(
                "the verdict is promote, but {kind} v{c} is {} — there is no draft left to \
                 publish",
                other.unwrap_or("absent")
            )),
        };
    }
    match status {
        Some("draft") => Ok(Stop::Withdraw),
        None => Ok(Stop::Already("discarded")),
        Some("rejected") => Ok(Stop::Already("rejected")),
        Some("retired") => Ok(Stop::Already("retired")),
        Some("active") => Err(format!(
            "{kind} v{c} is LIVE although the verdict is {} — some other hand published it. \
             This verb does not roll a kind back: the control is v{}, and publishing its text \
             with `boss workflow publish` is the rollback",
            verdict.unwrap_or("not yet given"),
            d.control
        )),
        Some(s) => Err(format!(
            "{kind} v{c} reads status `{s}`, which this verb does not know how to settle"
        )),
    }
}

/// The refusal when the candidate can be neither discarded (packets ran
/// it) nor rejected (this server predates backlog 50ded63d's door).
pub(crate) fn reject_door_absent(d: &Declaration) -> String {
    let (kind, c) = (&d.kind, d.candidate);
    format!(
        "{kind} v{c} could not be withdrawn: the discard refused it because packets ran it \
         (ce8b7d66), and this server has no reject door (POST \
         /api/workflows/{kind}/versions/{c}/reject, backlog 50ded63d). The split is out of \
         force — no new packet is admitted to v{c} — but v{c} stays a DRAFT, so `boss workflow \
         publish` refuses every {kind} spec while it sits there and a raw publish would put it \
         live. Re-run `boss experiment stop` on this packet once 50ded63d is deployed; it \
         resumes from the {STOPPED_KEY} record."
    )
}

/// Did the jobs API answer `e` with HTTP `code`? The signed door's
/// error reads `<service> <METHOD> <path> -> <code> <reason>: <body>`.
fn refused_with(e: &anyhow::Error, code: u16) -> bool {
    format!("{e:#}").contains(&format!("-> {code} "))
}

/// The clock the verbs stamp with, handed in from the CLI boundary
/// (`main.rs`) and read at the moment of the write it dates — never a
/// wallclock read in here (the no-wallclock lint, #68).
pub(crate) type Clock = fn() -> chrono::DateTime<chrono::Utc>;

fn stamp(clock: Clock) -> String {
    clock().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Every version of `kind`. A kind the registry has never held answers
/// 404, which is an empty list here and a refusal in `start_refusal`.
async fn versions_of(wire: &Wire, kind: &str) -> Result<Vec<Value>> {
    match wire
        .call(
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}/versions"),
            None,
        )
        .await
    {
        Ok(body) => crate::train::rows(body),
        Err(e) if refused_with(&e, 404) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

async fn version_row(wire: &Wire, kind: &str, version: i32) -> Result<Value> {
    let path = format!("/api/workflows/{kind}/versions/{version}");
    crate::workflow::workflow_row(
        wire.call(reqwest::Method::GET, &path, None)
            .await?
            .unwrap_or(Value::Null),
        &format!("GET {path}"),
    )
}

/// Every OPEN experiment, paged on `total` — a limit is not a filter.
async fn open_experiments(wire: &Wire) -> Result<Vec<Value>> {
    crate::train::list_all_pages(|offset| {
        let path = format!(
            "/api/jobs?kind={EXPERIMENT_KIND}&status=open&limit={}&offset={offset}",
            crate::train::PAGE_LIMIT
        );
        async move { wire.call(reqwest::Method::GET, &path, None).await }
    })
    .await
}

/// Why the draft read back is not the candidate sent, if it is not.
fn draft_differs(
    row: &Value,
    sent: &boss_jobs::registry::WorkflowSpec,
    version: i32,
) -> Option<String> {
    if field(row, "status") != "draft" {
        return Some(format!("it reads status `{}`", field(row, "status")));
    }
    if row.get("version").and_then(Value::as_i64) != Some(i64::from(version)) {
        return Some(format!("it reads version {}", row["version"]));
    }
    if field(row, "owning_team") != sent.owning_team {
        return Some(format!(
            "it is owned by `{}`, the candidate by `{}`",
            field(row, "owning_team"),
            sent.owning_team
        ));
    }
    let changes = boss_jobs::bootstrap::workflow_changes(row, sent);
    (!changes.is_empty()).then(|| {
        format!(
            "it differs on {}",
            changes
                .iter()
                .map(|c| c.field.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Discard a draft THIS verb made, and say whether it is gone.
async fn discard_own(wire: &Wire, kind: &str, version: i32) -> String {
    let gone = async {
        wire.call(
            reqwest::Method::DELETE,
            &format!("/api/workflows/{kind}/versions/{version}"),
            None,
        )
        .await?;
        if status_of(&versions_of(wire, kind).await?, version).is_some() {
            bail!("the DELETE answered but v{version} is still in the version list");
        }
        Ok(())
    }
    .await;
    match gone {
        Ok(()) => {
            format!("the draft {kind} v{version} this verb made was discarded — confirmed gone")
        }
        Err(e) => format!(
            "the draft {kind} v{version} this verb made could NOT be discarded ({e:#}) — \
             `boss workflow discard {kind} {version}` before anything publishes {kind}"
        ),
    }
}

/// Lint the row the way the publish path will (`POST _validate`).
async fn lint(wire: &Wire, spec: &Value) -> Result<Vec<Value>> {
    Ok(wire
        .call(
            reqwest::Method::POST,
            "/api/workflows/_validate",
            Some(spec.clone()),
        )
        .await?
        .as_ref()
        .and_then(|v| v.get("problems"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Merge `body` into the packet's metadata, then read it back: a 204 is
/// a claim, the packet is the fact.
async fn merge_confirmed(wire: &Wire, id: &str, body: Map<String, Value>) -> Result<()> {
    wire.patch_job_metadata(id, Value::Object(body.clone()))
        .await?;
    let after = wire.packet(id).await?;
    let (report, all_took) = crate::job::confirm_patch(
        after.get("metadata").unwrap_or(&Value::Null),
        &Value::Object(body),
    );
    if !all_took {
        bail!("the metadata merge answered but packet {id} does not hold what was sent:\n{report}");
    }
    Ok(())
}

pub(crate) async fn start(
    wire: &Wire,
    packet_ref: &str,
    kind: &str,
    candidate: &std::path::Path,
    split: u8,
    dry: bool,
    clock: Clock,
) -> Result<()> {
    let packet = wire.resolve(packet_ref, Some(EXPERIMENT_KIND)).await?;
    let id = crate::envelope::job_id(&packet)
        .context("the packet has no id")?
        .to_string();
    let open = open_experiments(wire).await?;
    let versions = versions_of(wire, kind).await?;
    let control = start_refusal(&packet, kind, split, &open, &versions)
        .map_err(|e| anyhow!("boss experiment start: REFUSED, nothing written — {e}"))?;
    let active = crate::workflow::active_row(
        kind,
        wire.call(
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}"),
            None,
        )
        .await,
    )?
    .with_context(|| format!("{kind} answered no active row; nothing written"))?;
    if active.get("version").and_then(Value::as_i64) != Some(i64::from(control)) {
        bail!(
            "the registry moved between two reads — the version list says {kind} v{control} is \
             active, the active row says {}; nothing written, run it again",
            active["version"]
        );
    }

    // The candidate, as `boss workflow publish` reads and fills it, then
    // TYPED as the PUT will type it: a missing field is named here, not
    // learned from a 422 after the fact.
    let mut spec = crate::workflow::load_spec(kind, candidate)?;
    match spec.get("kind").and_then(Value::as_str) {
        Some(named) if named != kind => bail!(
            "{} declares kind `{named}`, not `{kind}` — refusing to draft one kind's text as \
             another's",
            candidate.display()
        ),
        Some(_) => {}
        None => {
            if let Some(o) = spec.as_object_mut() {
                o.insert("kind".into(), json!(kind));
            }
        }
    }
    crate::workflow::carry_forward(&mut spec, Some(&active));
    let typed: boss_jobs::registry::WorkflowSpec =
        serde_json::from_value(spec.clone()).map_err(|e| {
            anyhow!(
                "{} is not a row the registry takes ({e}) — the PUT would refuse it; nothing \
                 written",
                candidate.display()
            )
        })?;
    let titles = crate::workflow::titles(&spec);
    if titles.is_empty() {
        bail!("the candidate declares no steps — nothing written");
    }
    let problems = lint(wire, &spec).await?;
    if !problems.is_empty() {
        bail!("the candidate does not lint clean, so nothing was written:\n  {problems:#?}");
    }
    let same_text = boss_jobs::bootstrap::workflow_changes(&active, &typed).is_empty();
    println!(
        "boss experiment: the {kind} candidate in {} lints clean ({} steps){}",
        candidate.display(),
        titles.len(),
        if same_text {
            " — its text equals the control's: an A/A"
        } else {
            ""
        }
    );
    if dry {
        println!(
            "boss experiment: DRY — would draft the candidate, put the split in force on {} \
             (control {kind} v{control}, {split}% to the candidate) and complete `run`; \
             nothing written",
            short(&id)
        );
        return Ok(());
    }

    // 1. The draft, read back and compared to what was sent.
    let created = crate::workflow::workflow_row(
        wire.call(
            reqwest::Method::PUT,
            &format!("/api/workflows/{kind}"),
            Some(spec.clone()),
        )
        .await?
        .unwrap_or(Value::Null),
        &format!("PUT /api/workflows/{kind}"),
    )?;
    let cand = created
        .get("version")
        .and_then(Value::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .context("the draft create answered no version")?;
    let mismatch = match version_row(wire, kind, cand).await {
        Ok(row) => draft_differs(&row, &typed, cand),
        Err(e) => Some(format!("it could not be read back ({e:#})")),
    };
    if let Some(why) = mismatch {
        let undo = discard_own(wire, kind, cand).await;
        bail!("{kind} v{cand} was drafted but is not the candidate sent: {why}\n  {undo}");
    }
    println!(
        "boss experiment: {kind} v{cand} drafted and read back equal to {} — verified, not \
         assumed",
        candidate.display()
    );

    // 2. The split, in force, in one merge beside its record.
    let at = stamp(clock);
    let d = Declaration {
        kind: kind.to_string(),
        control,
        candidate: cand,
        split,
    };
    let mut body = declaration_body(&d);
    body.insert(
        STARTED_KEY.into(),
        json!({
            "at": at,
            "by": "boss experiment start",
            KIND_KEY: kind,
            CONTROL_KEY: control,
            CANDIDATE_KEY: cand,
            SPLIT_KEY: split,
            "candidate_source": candidate.display().to_string(),
        }),
    );
    if let Err(e) = wire
        .patch_job_metadata(&id, Value::Object(body.clone()))
        .await
    {
        // An errored merge may still have landed (a timeout after the
        // write), so "not in force" is read off the packet before the
        // draft is discarded under it (review 5308927f F4).
        let holds = wire
            .packet(&id)
            .await
            .map(|p| declared(p.get("metadata").unwrap_or(&Value::Null)));
        match holds {
            Ok(Ok(None)) => {
                let undo = discard_own(wire, kind, cand).await;
                bail!(
                    "the split was NOT put in force on {id} (read back: the packet declares \
                     none): {e:#}\n  {undo}"
                );
            }
            Ok(said) => bail!(
                "the merge errored ({e:#}) but packet {id} now reads {said:?}, so admission may \
                 be reading a split — read the packet before anything else; the draft {kind} \
                 v{cand} is kept"
            ),
            Err(r) => bail!(
                "the merge errored ({e:#}) and packet {id} could not be read back ({r:#}), so \
                 whether the split is in force is unknown — read the packet before anything \
                 else; the draft {kind} v{cand} is kept"
            ),
        }
    }
    let after = wire.packet(&id).await?;
    let (report, all_took) = crate::job::confirm_patch(
        after.get("metadata").unwrap_or(&Value::Null),
        &Value::Object(body),
    );
    if !all_took {
        bail!(
            "the merge answered but packet {id} does not hold the split as sent, so what \
             admission reads now is uncertain — read the packet before anything else; the draft \
             {kind} v{cand} is kept:\n{report}"
        );
    }
    println!(
        "boss experiment: split in force on {} since {at} — {kind} control v{control}, \
         candidate v{cand}, {split}% to the candidate; read back from the packet",
        short(&id)
    );

    // 3. `run`, dated by that instant.
    let method = format!(
        "Tier 2 split put in force by boss experiment start: {kind} control v{control} (the \
         active row) against candidate v{cand} (a draft made from {}, read back equal to it), \
         {split}% of admissions to the candidate by a fixed hash of each packet's own id, from \
         {at} until boss experiment stop takes it out.",
        candidate.display()
    );
    crate::steps::complete(
        wire,
        &id,
        "run",
        &[
            Given {
                name: "started_at".into(),
                value: at.clone(),
            },
            Given {
                name: "method".into(),
                value: method,
            },
        ],
    )
    .await
    .with_context(|| {
        format!(
            "the split IS in force (since {at}) but `run` was not completed — complete it with \
             started_at={at} so the record's window matches"
        )
    })
}

/// The `decide` step's verdict, once it is completed.
fn verdict_of(packet: &Value) -> Option<String> {
    crate::envelope::steps(packet)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("decide"))
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
        .and_then(|s| s.pointer("/metadata/verdict"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A live correction of `decide`'s verdict, said as the refusal it earns
/// — or `None`. A correction (design 4105b020) is handed to every reader
/// of the step, while the workflow's own verdict forks still route on
/// the stored value, so two readings of one verdict are live at once
/// (review 5308927f F3). This verb settles the candidate on neither: the
/// split still comes out, and the candidate waits for the operator. A
/// correction a later entry withdraws is no correction.
fn verdict_correction(packet: &Value) -> Option<String> {
    let decide = crate::envelope::steps(packet)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("decide"))?;
    let all = decide.get("corrections").and_then(Value::as_array)?;
    let index = |c: &Value| c.get("index").and_then(Value::as_u64);
    let withdrawn = |i: u64| {
        all.iter()
            .any(|w| w.get("withdraws").and_then(Value::as_u64) == Some(i))
    };
    let live = all.iter().find(|c| {
        c.get("field").and_then(Value::as_str) == Some("verdict")
            && c.get("withdraws").is_none()
            && !index(c).is_some_and(withdrawn)
    })?;
    let s = |k: &str| live.get(k).and_then(Value::as_str).unwrap_or("");
    Some(format!(
        "decide's verdict carries correction [{}] by {} (reads `{}`, should read `{}`): the \
         workflow's forks route on the stored verdict and every reader is handed the \
         correction, so this verb settles the candidate on neither. Settle it by hand — `boss \
         workflow publish` or `boss workflow discard` — or withdraw the correction and re-run \
         `boss experiment stop`",
        index(live).map_or_else(|| "?".to_string(), |i| i.to_string()),
        s("by"),
        s("reads"),
        s("should_read")
    ))
}

/// Publish the candidate through `boss workflow publish`'s checks: no
/// other draft to be promoted instead, the draft read first, the active
/// row read back after, and what the tree says about a drift-publish hold
/// on the kind printed as that verb prints it (review 5308927f F6).
/// `source` is the candidate file `start` recorded, which places the
/// checkout whose holds are asked; the working directory otherwise.
async fn publish_candidate(wire: &Wire, d: &Declaration, source: &std::path::Path) -> Result<()> {
    let (kind, c) = (&d.kind, d.candidate);
    if let Some(other) = crate::workflow::blocking_draft(&versions_of(wire, kind).await?, Some(c)) {
        bail!(
            "a second draft, {kind} v{other}, appeared beside the candidate v{c}; publish would \
             promote the newest — nothing published"
        );
    }
    let draft = version_row(wire, kind, c).await?;
    let titles = crate::workflow::titles(&draft);
    wire.call(
        reqwest::Method::POST,
        &format!("/api/workflows/{kind}/publish"),
        None,
    )
    .await?;
    let live = crate::workflow::workflow_row(
        wire.call(
            reqwest::Method::GET,
            &format!("/api/workflows/{kind}"),
            None,
        )
        .await?
        .unwrap_or(Value::Null),
        &format!("GET /api/workflows/{kind}"),
    )?;
    crate::workflow::confirm(&live, c, &titles)?;
    let hold = crate::workflow::read_drift_hold(kind, source);
    for line in crate::workflow::drift_hold_lines(kind, hold.as_ref()) {
        println!("{line}");
    }
    Ok(())
}

/// Discard the candidate; when packets ran it the discard refuses, and
/// it is rejected instead. Answers what it IS afterwards, read back.
async fn withdraw_candidate(wire: &Wire, d: &Declaration) -> Result<String> {
    let (kind, c) = (&d.kind, d.candidate);
    let at = format!("/api/workflows/{kind}/versions/{c}");
    match wire.call(reqwest::Method::DELETE, &at, None).await {
        Ok(_) => {}
        Err(discard) if refused_with(&discard, 409) => {
            match wire
                .call(reqwest::Method::POST, &format!("{at}/reject"), None)
                .await
            {
                Ok(_) => {}
                // A 404 is the missing door only while the version is
                // still a draft; one gone or moved in between (another
                // hand) is read back below as what it is (review
                // 5308927f F5).
                Err(e) if refused_with(&e, 404) => {
                    if status_of(&versions_of(wire, kind).await?, c) == Some("draft") {
                        bail!("{}", reject_door_absent(d));
                    }
                }
                Err(e) => {
                    return Err(e.context(format!(
                        "the discard refused {kind} v{c} ({discard:#}) and so did the reject"
                    )));
                }
            }
        }
        Err(e) => return Err(e.context(format!("discarding {kind} v{c}"))),
    }
    match status_of(&versions_of(wire, kind).await?, c) {
        None => Ok("discarded".into()),
        Some("rejected") => Ok("rejected".into()),
        Some(s) => bail!(
            "the withdrawal answered but {kind} v{c} still reads `{s}` — refusing to call it \
             withdrawn"
        ),
    }
}

pub(crate) async fn stop(wire: &Wire, packet_ref: &str, dry: bool, clock: Clock) -> Result<()> {
    let packet = wire.resolve(packet_ref, Some(EXPERIMENT_KIND)).await?;
    let id = crate::envelope::job_id(&packet)
        .context("the packet has no id")?
        .to_string();
    if field(&packet, "kind") != EXPERIMENT_KIND {
        bail!(
            "boss experiment stop: REFUSED — packet {} is a `{}`, not a {EXPERIMENT_KIND}",
            short(&id),
            field(&packet, "kind")
        );
    }
    let md = packet.get("metadata").cloned().unwrap_or(Value::Null);
    let refused = |e: String| anyhow!("boss experiment stop: REFUSED, nothing written — {e}");
    let in_force = declared(&md).map_err(refused)?;
    let record = md.get(STOPPED_KEY).filter(|r| r.is_object());
    let (d, at) = match (&in_force, record) {
        (Some(d), _) => (d.clone(), stamp(clock)),
        (None, Some(r)) => {
            let d = declared(r)
                .map_err(refused)?
                .ok_or_else(|| refused(format!("its {STOPPED_KEY} record names no split")))?;
            let settled = field(r, "candidate");
            if SETTLED.contains(&settled) {
                // Settled before a `promote` was given — or published on
                // one that no longer stands — is not "nothing to do":
                // the record and `decide` disagree about what the kind
                // runs (review 5308927f F2). This verb does not re-settle
                // a candidate; the operator does, and is told how.
                let now = verdict_of(&packet);
                if (now.as_deref() == Some("promote")) != (settled == "published") {
                    let then = r.get("verdict").and_then(Value::as_str);
                    let remedy = if settled == "published" {
                        format!(
                            "the control is v{}; publishing its text with `boss workflow \
                             publish {}` is the rollback",
                            d.control, d.kind
                        )
                    } else {
                        let source = r
                            .get("candidate_source")
                            .or_else(|| md.pointer(&format!("/{STARTED_KEY}/candidate_source")))
                            .and_then(Value::as_str)
                            .unwrap_or("the candidate's file");
                        format!(
                            "to promote it, publish its text with `boss workflow publish {} \
                             {source}`",
                            d.kind
                        )
                    };
                    return Err(anyhow!(
                        "boss experiment stop: REFUSED, nothing written — {} was stopped at {} \
                         on verdict {} and {} v{} is {settled}, but decide now says {}. The \
                         candidate was settled before that verdict and this verb does not \
                         re-settle it: {remedy}",
                        short(&id),
                        field(r, "at"),
                        then.unwrap_or("not yet given"),
                        d.kind,
                        d.candidate,
                        now.as_deref().unwrap_or("not yet given")
                    ));
                }
                println!(
                    "boss experiment: {} was stopped at {} — {} v{} {settled}; nothing to do",
                    short(&id),
                    field(r, "at"),
                    d.kind,
                    d.candidate
                );
                return Ok(());
            }
            println!(
                "boss experiment: resuming the stop recorded at {} (candidate {settled})",
                field(r, "at")
            );
            (d, field(r, "at").to_string())
        }
        (None, None) => {
            return Err(refused(format!(
                "packet {} declares no split and carries no {STOPPED_KEY} record — there is \
                 nothing to take out of force",
                short(&id)
            )));
        }
    };
    let verdict = verdict_of(&packet);
    let plan_for = |versions: &[Value]| match verdict_correction(&packet) {
        Some(why) => Err(why),
        None => stop_plan(verdict.as_deref(), &d, versions),
    };
    let source = md
        .pointer(&format!("/{STARTED_KEY}/candidate_source"))
        .and_then(Value::as_str)
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    if dry {
        let shown = match plan_for(&versions_of(wire, &d.kind).await?) {
            Ok(plan) => format!("settle {} v{} ({plan:?}", d.kind, d.candidate),
            Err(why) => format!("leave {} v{} unsettled ({why}", d.kind, d.candidate),
        };
        println!(
            "boss experiment: DRY — would take the split out of force on {} and {shown}, \
             verdict {}); nothing written",
            short(&id),
            verdict.as_deref().unwrap_or("not yet given")
        );
        return Ok(());
    }
    let record = |candidate: &str, why: Option<&str>| -> Value {
        let mut r = declaration_body(&d);
        r.insert("at".into(), json!(at));
        r.insert("by".into(), json!("boss experiment stop"));
        r.insert("verdict".into(), json!(verdict));
        r.insert("candidate".into(), json!(candidate));
        if let Some(why) = why {
            r.insert("why".into(), json!(why));
        }
        Value::Object(r)
    };

    // 1. Out of force, FIRST and UNCONDITIONALLY: nothing settled below
    //    can change what a newly admitted packet runs once this has
    //    landed. The plan is judged after it, never before — a refused
    //    plan used to return here with the split still in force, pinning
    //    new packets to a control the kind had retired, and the only way
    //    out was the raw PATCH this verb exists to retire (review
    //    5308927f F1).
    if in_force.is_some() {
        let mut body = withdrawal_body();
        body.insert(STOPPED_KEY.into(), record("pending", None));
        merge_confirmed(wire, &id, body).await?;
        println!(
            "boss experiment: split out of force on {} since {at} — new {} packets run the \
             active version; read back from the packet",
            short(&id),
            d.kind
        );
    }

    // 2. The candidate, settled by the verdict, read back. A plan that
    //    refuses — or a version list that cannot be read — leaves it
    //    `unsettled` (not a SETTLED word, so a re-run plans again); a
    //    settle that fails leaves it `left-draft`.
    let settled: Result<String, (&str, anyhow::Error)> = match versions_of(wire, &d.kind).await {
        Err(e) => Err(("unsettled", e)),
        Ok(versions) => match plan_for(&versions) {
            Err(why) => Err((
                "unsettled",
                anyhow!(
                    "boss experiment stop: REFUSED to settle {} v{} — {why}",
                    d.kind,
                    d.candidate
                ),
            )),
            Ok(Stop::Already(s)) => Ok(s.to_string()),
            Ok(Stop::Publish) => publish_candidate(wire, &d, &source)
                .await
                .map(|()| "published".to_string())
                .map_err(|e| ("left-draft", e)),
            Ok(Stop::Withdraw) => withdraw_candidate(wire, &d)
                .await
                .map_err(|e| ("left-draft", e)),
        },
    };
    let (outcome, failure) = match &settled {
        Ok(s) => (s.clone(), None),
        Err((left, e)) => (left.to_string(), Some(format!("{e:#}"))),
    };
    let settled = settled.map_err(|(_, e)| {
        e.context(format!(
            "the split on {} is out of force (read back), and {} v{} is not settled",
            short(&id),
            d.kind,
            d.candidate
        ))
    });
    let mut body = Map::new();
    body.insert(STOPPED_KEY.into(), record(&outcome, failure.as_deref()));
    let recorded = merge_confirmed(wire, &id, body).await;
    match (settled, recorded) {
        (Ok(s), Ok(())) => {
            println!(
                "boss experiment: {} v{} {s} (verdict {}) — recorded on {} as {STOPPED_KEY}",
                d.kind,
                d.candidate,
                verdict.as_deref().unwrap_or("not yet given"),
                short(&id)
            );
            Ok(())
        }
        (Ok(s), Err(e)) => Err(e.context(format!(
            "{} v{} is {s}, but the record on {} could not say so",
            d.kind,
            d.candidate,
            short(&id)
        ))),
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(r)) => Err(e.context(format!("and the record could not be written: {r:#}"))),
    }
}

/// Top-level registration — see `merged::Cmd` for why the variant lives
/// in its verb's module (84f9fbc0).
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// A Tier 2 split: an open protocol-experiment packet admitting a
    /// share of one kind's new packets to a candidate draft (backlog
    /// 75296c37). The verbs for the operator's writes, each refusing
    /// before it writes and reading back what it wrote.
    Experiment {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Put a split in force: draft the candidate from a file and read it
    /// back, declare the split on the packet and read it back, complete
    /// `run` with started_at = that instant. Refused when the packet is
    /// not an open experiment at `run`, when another open experiment
    /// names the kind, or when any draft of the kind already exists.
    Start {
        /// The protocol-experiment packet: full uuid, or 8+ characters of an open one.
        packet: String,
        /// The workflow kind whose admission splits; its active version is the control.
        #[arg(long)]
        kind: String,
        /// The candidate's text, read as `boss workflow publish` reads a spec: a JSON
        /// row, a bundle `.toml`, or the bundle directory.
        #[arg(long)]
        candidate_file: std::path::PathBuf,
        /// Percent of new packets admitted to the candidate, 0-100.
        #[arg(long, default_value_t = 50)]
        split: u8,
        /// Read, check and lint; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Take a split out of force, then settle the candidate by the
    /// `decide` verdict: promote publishes it (read back as the active
    /// row); any other verdict, or none yet, discards it, or rejects it
    /// when packets ran it. Recorded on the packet as `split_stopped`;
    /// an interrupted stop resumes from that record.
    Stop {
        /// The protocol-experiment packet: full uuid, or 8+ characters of an open one.
        packet: String,
        /// Read and decide; write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn dispatch(cmd: Cmd, clock: Clock) -> Result<()> {
    let wire = Wire::live()?;
    match cmd {
        Cmd::Experiment {
            action:
                Action::Start {
                    packet,
                    kind,
                    candidate_file,
                    split,
                    dry_run,
                },
        } => {
            start(
                &wire,
                &packet,
                &kind,
                &candidate_file,
                split,
                dry_run,
                clock,
            )
            .await
        }
        Cmd::Experiment {
            action: Action::Stop { packet, dry_run },
        } => stop(&wire, &packet, dry_run, clock).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Job, JobStatus, Priority, Subject};
    use boss_jobs::experiments::{EXPERIMENT_KIND, governing_experiment};
    use serde_json::json;

    fn decl() -> Declaration {
        Declaration {
            kind: "agent-run".into(),
            control: 9,
            candidate: 10,
            split: 25,
        }
    }

    fn experiment_job(metadata: Value) -> Job {
        let mut j = Job::new(
            EXPERIMENT_KIND,
            Subject::new("custom", "bosspipeline"),
            "Experiment",
            "emp-ceo",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        );
        j.status = JobStatus::Open;
        j.metadata = metadata;
        j
    }

    fn merged(base: &Value, body: &Map<String, Value>) -> Value {
        let mut md = base.as_object().cloned().unwrap_or_default();
        for (k, v) in body {
            if v.is_null() {
                md.remove(k);
            } else {
                md.insert(k.clone(), v.clone());
            }
        }
        Value::Object(md)
    }

    /// §9a: the four keys live twice — written here, read by split
    /// admission (boss-jobs `experiments.rs`). Pinned by BEHAVIOUR, not
    /// by a list: what this verb writes is what admission reads as a
    /// split in force, and what `stop` sends takes it out again.
    #[test]
    fn the_declaration_this_verb_writes_is_the_one_admission_reads() {
        let on = merged(&json!({"protocol": "x"}), &declaration_body(&decl()));
        let job = experiment_job(on.clone());
        let got = governing_experiment(std::slice::from_ref(&job), "agent-run")
            .expect("the declaration this verb writes puts a split in force");
        assert_eq!(
            (got.control_version, got.candidate_version, got.split),
            (9, 10, 25)
        );
        assert_eq!(declared(&on), Ok(Some(decl())));

        let off = merged(&on, &withdrawal_body());
        let job = experiment_job(off.clone());
        assert_eq!(
            governing_experiment(std::slice::from_ref(&job), "agent-run"),
            None,
            "the withdrawal takes it out of force"
        );
        assert_eq!(declared(&off), Ok(None));
        assert_eq!(off["protocol"], "x", "the withdrawal touches nothing else");
    }

    /// Half a declaration is an operator's hand-write the machine cannot
    /// read — admission ignores it, and `stop` must not guess which
    /// split it was. Refused, naming what is there and what is missing.
    #[test]
    fn half_a_declaration_is_refused_naming_its_keys() {
        let md = json!({"kind_under_test": "agent-run", "control_version": 9});
        let why = declared(&md).unwrap_err();
        assert!(why.contains("candidate_version"), "{why}");
        let md = json!({"kind_under_test": "agent-run", "control_version": 9,
                        "candidate_version": "ten"});
        assert!(declared(&md).unwrap_err().contains("candidate_version"));
    }

    fn packet(status: &str, metadata: Value, run: &str) -> Value {
        json!({
            "id": "4aa60dc9-71d7-45f8-aafa-9ff4be28c269",
            "kind": EXPERIMENT_KIND,
            "status": status,
            "title": "Experiment",
            "metadata": metadata,
            "steps": [
                {"id": "s-state", "spec_slug": "state", "status": "completed"},
                {"id": "s-run", "spec_slug": "run", "status": run},
            ],
        })
    }

    fn versions(rows: &[(i32, &str)]) -> Vec<Value> {
        rows.iter()
            .map(|(v, s)| json!({"version": v, "status": s}))
            .collect()
    }

    fn other_experiment(kind: &str) -> Value {
        json!({"id": "2dbb417d-0000-0000-0000-000000000000", "kind": EXPERIMENT_KIND,
               "status": "open", "metadata": {"kind_under_test": kind}})
    }

    #[test]
    fn start_answers_the_control_when_nothing_refuses() {
        let p = packet("open", json!({}), "ready");
        let live = versions(&[(8, "retired"), (9, "active")]);
        assert_eq!(
            start_refusal(&p, "agent-run", 50, std::slice::from_ref(&p), &live),
            Ok(9)
        );
        // Another experiment on ANOTHER kind is no business of this one.
        let others = [p.clone(), other_experiment("backlog-item")];
        assert_eq!(start_refusal(&p, "agent-run", 50, &others, &live), Ok(9));
    }

    #[test]
    fn start_refuses_before_any_write() {
        let p = packet("open", json!({}), "ready");
        let live = versions(&[(9, "active")]);
        let refused = |packet: &Value, split: u8, open: &[Value], vs: &[Value], needle: &str| {
            let why = start_refusal(packet, "agent-run", split, open, vs).expect_err("refused");
            assert!(why.contains(needle), "expected `{needle}` in: {why}");
        };
        // Not an experiment.
        let mut wrong = p.clone();
        wrong["kind"] = json!("backlog-item");
        refused(&wrong, 50, &[], &live, "protocol-experiment");
        // Closed: the window IS the open interval.
        refused(
            &packet("closed", json!({}), "ready"),
            50,
            &[],
            &live,
            "closed",
        );
        // Already declares a split.
        let declared_already = packet("open", json!({"kind_under_test": "agent-run"}), "ready");
        refused(&declared_already, 50, &[], &live, "kind_under_test");
        // `run` is the step this verb completes; not open, not ours.
        refused(
            &packet("open", json!({}), "pending"),
            50,
            &[],
            &live,
            "`run`",
        );
        // A share outside 0..=100.
        refused(&p, 101, &[], &live, "101");
        // Another open experiment already governs the kind.
        refused(
            &p,
            50,
            &[p.clone(), other_experiment("agent-run")],
            &live,
            "2dbb417d",
        );
        // No control: nothing active to measure against.
        refused(
            &p,
            50,
            &[],
            &versions(&[(9, "retired")]),
            "no active version",
        );
        // A draft already sits there: publish would promote the wrong one.
        refused(
            &p,
            50,
            &[],
            &versions(&[(9, "active"), (10, "draft")]),
            "v10",
        );
    }

    #[test]
    fn stop_publishes_only_a_promoted_draft_against_its_own_control() {
        let d = decl();
        let live = versions(&[(9, "active"), (10, "draft")]);
        assert_eq!(stop_plan(Some("promote"), &d, &live), Ok(Stop::Publish));
        // Already live (a resumed stop): nothing to do, said as published.
        let done = versions(&[(9, "retired"), (10, "active")]);
        assert_eq!(
            stop_plan(Some("promote"), &d, &done),
            Ok(Stop::Already("published"))
        );
        // A newer draft would be what publish promotes.
        let crowded = versions(&[(9, "active"), (10, "draft"), (11, "draft")]);
        assert!(
            stop_plan(Some("promote"), &d, &crowded)
                .unwrap_err()
                .contains("v11")
        );
        // The control moved under the experiment: promoting would replace
        // a version it never measured.
        let moved = versions(&[(9, "retired"), (10, "draft"), (12, "active")]);
        assert!(
            stop_plan(Some("promote"), &d, &moved)
                .unwrap_err()
                .contains("v12")
        );
        // Promote with no draft left to publish.
        let gone = versions(&[(9, "active")]);
        assert!(
            stop_plan(Some("promote"), &d, &gone)
                .unwrap_err()
                .contains("absent")
        );
    }

    #[test]
    fn every_other_verdict_withdraws_the_candidate_and_never_publishes_it() {
        let d = decl();
        let live = versions(&[(9, "active"), (10, "draft")]);
        for verdict in [Some("retire"), Some("inconclusive"), None] {
            assert_eq!(
                stop_plan(verdict, &d, &live),
                Ok(Stop::Withdraw),
                "{verdict:?}"
            );
        }
        assert_eq!(
            stop_plan(Some("retire"), &d, &versions(&[(9, "active")])),
            Ok(Stop::Already("discarded"))
        );
        assert_eq!(
            stop_plan(
                Some("retire"),
                &d,
                &versions(&[(9, "active"), (10, "rejected")])
            ),
            Ok(Stop::Already("rejected"))
        );
        // A loser someone published anyway is not this verb's to undo.
        let why = stop_plan(
            Some("retire"),
            &d,
            &versions(&[(9, "retired"), (10, "active")]),
        )
        .unwrap_err();
        assert!(why.contains("LIVE") && why.contains("v9"), "{why}");
    }

    /// Composition with backlog 50ded63d: until the reject door is on
    /// the server, a candidate packets ran cannot be discarded or
    /// rejected. The refusal names that car, says the split is OUT of
    /// force, and says the re-run is the resolution.
    #[test]
    fn a_missing_reject_door_is_said_by_name() {
        let said = reject_door_absent(&decl());
        for needle in [
            "50ded63d",
            "agent-run v10",
            "out of force",
            "boss experiment stop",
        ] {
            assert!(said.contains(needle), "expected `{needle}` in:\n{said}");
        }
    }
}

#[cfg(test)]
mod door_tests {
    //! Both verbs against the REAL jobs router over the in-memory
    //! adapters — the handlers production mounts, the split admission
    //! that reads what `start` writes, the discard that refuses a
    //! pinned draft — driven with a named caller as a value.

    use super::*;
    use crate::steps::Given;
    use boss_jobs::registry::{StepSpec, Terminal, WorkflowStatus, seedable_platform_workflows};
    use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
    use boss_policy_client::{FakePolicyClient, PolicyClient};
    use serde_json::json;
    use std::sync::Arc;

    const KIND: &str = "versioned";
    const LOGIN: &str = "emp-operator";

    fn step(title: &str, kind: &str, ready_when: &str, terminal: bool) -> StepSpec {
        StepSpec {
            title: title.into(),
            kind: kind.into(),
            ready_when: ready_when.into(),
            title_template: title.into(),
            terminal: terminal.then(|| Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        }
    }

    /// v1, the control: open -> closed.
    fn control() -> boss_jobs::registry::WorkflowSpec {
        boss_jobs::registry::WorkflowSpec::platform_seed(
            KIND,
            "Versioned",
            "test",
            vec!["custom".into()],
            vec![
                step("open", "trigger", "true", false),
                step("closed", "outcome", "steps.open.done", true),
            ],
        )
    }

    /// The candidate's text: one step more.
    fn candidate_file(name: &str) -> std::path::PathBuf {
        let mut spec = control();
        spec.steps = vec![
            step("open", "trigger", "true", false),
            step("work", "task", "steps.open.done", false),
            step("closed", "outcome", "steps.work.done", true),
        ];
        let path = boss_testing::scratch_dir(name).join("candidate.json");
        boss_testing::write_file(&path, &serde_json::to_string(&spec).unwrap());
        path
    }

    async fn serve() -> String {
        let jobs = Arc::new(InMemoryJobs::new());
        let policy: Arc<dyn PolicyClient> =
            Arc::new(FakePolicyClient::builder().with_default_rules().build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let publisher = boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs");
        let kinds = Arc::new(InMemoryWorkflows::for_fixture().with_packets(jobs.clone()));
        for spec in seedable_platform_workflows()
            .into_iter()
            .filter(|s| s.kind == boss_jobs::experiments::EXPERIMENT_KIND)
        {
            kinds.seed(spec).unwrap();
        }
        let mut v1 = control();
        v1.status = WorkflowStatus::Active;
        kinds.seed(v1).unwrap();
        let state = boss_jobs::http::JobsApiState {
            kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
            ..boss_jobs::http::JobsApiState::minimal(
                jobs,
                bus,
                publisher,
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        };
        let app = boss_jobs::http::router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn wire(base: String) -> Wire {
        Wire::at(
            base,
            Some(crate::identity::Caller {
                id: LOGIN.into(),
                source: crate::identity::Source::Env,
            }),
        )
        .unwrap()
    }

    async fn get(w: &Wire, path: &str) -> Value {
        w.call(reqwest::Method::GET, path, None)
            .await
            .unwrap()
            .unwrap_or(Value::Null)
    }

    async fn admit(w: &Wire, kind: &str, metadata: Value) -> String {
        let body = crate::job::envelope(kind, "A packet", None, None, LOGIN, Some(metadata));
        let made = w
            .call(reqwest::Method::POST, "/api/jobs", Some(body))
            .await
            .unwrap()
            .unwrap();
        made["id"].as_str().unwrap().to_string()
    }

    async fn finish(w: &Wire, id: &str, slug: &str, fields: &[(&str, &str)]) {
        let given: Vec<Given> = fields
            .iter()
            .map(|(n, v)| Given {
                name: (*n).into(),
                value: (*v).into(),
            })
            .collect();
        crate::steps::complete(w, id, slug, &given).await.unwrap();
    }

    fn step_of<'a>(packet: &'a Value, slug: &str) -> &'a Value {
        crate::envelope::steps(packet)
            .into_iter()
            .find(|s| s["spec_slug"] == slug)
            .unwrap()
    }

    /// An experiment packet with its hypothesis stated: `run` is next.
    async fn stated(w: &Wire) -> String {
        let id = admit(
            w,
            boss_jobs::experiments::EXPERIMENT_KIND,
            json!({"protocol": "versioned v1 against one step more"}),
        )
        .await;
        let p = get(w, &format!("/api/jobs/{id}")).await;
        if crate::steps::open_step(&p, "opened").is_ok() {
            finish(w, &id, "opened", &[]).await;
        }
        finish(
            w,
            &id,
            "state",
            &[
                ("hypothesis", "one more step costs nothing"),
                ("metric", "cycle time"),
                ("control", "versioned v1"),
                ("candidate", "versioned v2"),
                ("decision_rule", "not worse"),
                ("sample_floor", "one"),
            ],
        )
        .await;
        id
    }

    async fn decided(w: &Wire, id: &str, verdict: &str) {
        finish(
            w,
            id,
            "measure",
            &[
                ("control_result", "c"),
                ("candidate_result", "c"),
                ("samples", "1"),
                ("source", "terminal report"),
            ],
        )
        .await;
        finish(
            w,
            id,
            "decide",
            &[
                ("verdict", verdict),
                ("decision", "d"),
                ("against_stated_rule", "held"),
                ("confounds", "none"),
                ("elapsed", "1m"),
            ],
        )
        .await;
    }

    async fn statuses(w: &Wire) -> Vec<(i64, String)> {
        crate::train::rows(Some(
            get(w, &format!("/api/workflows/{KIND}/versions")).await,
        ))
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_i64().unwrap(),
                v["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
    }

    #[tokio::test]
    async fn start_makes_the_draft_puts_the_split_in_force_and_runs_the_step() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-start"),
            100,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();

        assert_eq!(
            statuses(&w).await,
            vec![(1, "active".to_string()), (2, "draft".to_string())]
        );
        let draft = get(&w, &format!("/api/workflows/{KIND}/versions/2")).await;
        assert_eq!(crate::workflow::titles(&draft), ["open", "work", "closed"]);

        let p = get(&w, &format!("/api/jobs/{id}")).await;
        let md = &p["metadata"];
        assert_eq!(md["kind_under_test"], KIND);
        assert_eq!(md["control_version"], 1);
        assert_eq!(md["candidate_version"], 2);
        assert_eq!(md["split"], 100);
        let run = step_of(&p, "run");
        assert_eq!(run["status"], "completed");
        assert_eq!(
            run["metadata"]["started_at"], md[STARTED_KEY]["at"],
            "the run step's started_at is the instant the split was put in force"
        );
        let method = run["metadata"]["method"].as_str().unwrap();
        assert!(method.contains("v1") && method.contains("v2"), "{method}");

        // And admission reads it: at split 100 the next packet of the kind
        // runs the candidate, stamped with this experiment.
        let admitted = admit(&w, KIND, json!({})).await;
        let a = get(&w, &format!("/api/jobs/{admitted}")).await;
        assert_eq!(a["workflow_version"], 2);
        assert_eq!(a["metadata"]["experiment_arm"], "candidate");
        assert_eq!(a["metadata"]["experiment_id"], id.as_str());
    }

    #[tokio::test]
    async fn start_refuses_a_second_split_on_the_kind_and_writes_nothing() {
        let w = wire(serve().await);
        let first = stated(&w).await;
        start(
            &w,
            &first,
            KIND,
            &candidate_file("experiment-first"),
            50,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        let second = stated(&w).await;
        let why = start(
            &w,
            &second,
            KIND,
            &candidate_file("experiment-second"),
            50,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap_err();
        let why = format!("{why:#}");
        assert!(why.contains(&first[..8]), "{why}");
        assert_eq!(
            statuses(&w).await.len(),
            2,
            "no second draft was made: {why}"
        );
        let p = get(&w, &format!("/api/jobs/{second}")).await;
        assert!(p["metadata"].get("kind_under_test").is_none());
        assert_ne!(step_of(&p, "run")["status"], "completed");
    }

    #[tokio::test]
    async fn a_dry_start_writes_nothing() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-dry"),
            50,
            true,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        assert_eq!(statuses(&w).await, vec![(1, "active".to_string())]);
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        assert!(p["metadata"].get("kind_under_test").is_none());
    }

    #[tokio::test]
    async fn a_promote_publishes_the_candidate_and_a_second_stop_is_a_no_op() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-promote"),
            50,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        decided(&w, &id, "promote").await;
        stop(&w, &id, false, chrono::Utc::now).await.unwrap();

        assert_eq!(
            statuses(&w).await,
            vec![(1, "retired".to_string()), (2, "active".to_string())]
        );
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        for k in [
            "kind_under_test",
            "control_version",
            "candidate_version",
            "split",
        ] {
            assert!(p["metadata"].get(k).is_none(), "{k} still in force");
        }
        assert_eq!(p["metadata"][STOPPED_KEY]["candidate"], "published");
        assert_eq!(p["metadata"][STOPPED_KEY]["verdict"], "promote");

        stop(&w, &id, false, chrono::Utc::now).await.unwrap();
        assert_eq!(statuses(&w).await.len(), 2);
    }

    #[tokio::test]
    async fn a_retire_discards_a_candidate_no_packet_ran() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-retire"),
            0,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        decided(&w, &id, "retire").await;
        stop(&w, &id, false, chrono::Utc::now).await.unwrap();
        assert_eq!(statuses(&w).await, vec![(1, "active".to_string())]);
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        assert!(p["metadata"].get("kind_under_test").is_none());
        assert_eq!(p["metadata"][STOPPED_KEY]["candidate"], "discarded");
    }

    /// A candidate packets ran cannot be discarded (ce8b7d66). With the
    /// reject door (backlog 50ded63d) on the server it is rejected; on a
    /// server without it the verb refuses, naming that car, with the
    /// split already out of force and the record saying so. Both worlds
    /// are asserted whole, because this car and that one compose in
    /// either landing order.
    #[tokio::test]
    async fn a_retire_on_a_candidate_packets_ran_takes_the_split_out_and_never_publishes() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-pinned"),
            100,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        let ran = admit(&w, KIND, json!({})).await;
        assert_eq!(
            get(&w, &format!("/api/jobs/{ran}")).await["workflow_version"],
            2
        );
        decided(&w, &id, "retire").await;

        let said = stop(&w, &id, false, chrono::Utc::now).await;
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        assert!(
            p["metadata"].get("kind_under_test").is_none(),
            "out of force either way"
        );
        let record = &p["metadata"][STOPPED_KEY];
        match said {
            Ok(()) => {
                assert_eq!(
                    statuses(&w).await,
                    vec![(1, "active".to_string()), (2, "rejected".to_string())]
                );
                assert_eq!(record["candidate"], "rejected");
            }
            Err(e) => {
                let e = format!("{e:#}");
                assert!(e.contains("50ded63d"), "{e}");
                assert_eq!(
                    statuses(&w).await,
                    vec![(1, "active".to_string()), (2, "draft".to_string())]
                );
                assert_eq!(record["candidate"], "left-draft");
                assert!(record["why"].as_str().unwrap().contains("50ded63d"));
            }
        }
    }

    /// The four declaration keys are absent from the packet: admission
    /// reads no split there.
    fn out_of_force(p: &Value) {
        for k in DECLARATION_KEYS {
            assert!(p["metadata"].get(k).is_none(), "{k} still in force");
        }
    }

    /// Review 5308927f F1, its probe A: a candidate some other hand
    /// published, then a `retire`. The plan refuses — this verb does not
    /// roll a kind back — and the refusal used to come BEFORE the
    /// withdrawal, so the split stayed in force and admission kept
    /// pinning new packets to a retired control, with a raw PATCH the
    /// only way out. The split comes out whatever the plan says.
    #[tokio::test]
    async fn a_refused_settle_still_takes_the_split_out_of_force() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-raw-publish"),
            50,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        w.call(
            reqwest::Method::POST,
            &format!("/api/workflows/{KIND}/publish"),
            None,
        )
        .await
        .unwrap();
        decided(&w, &id, "retire").await;

        let why = format!(
            "{:#}",
            stop(&w, &id, false, chrono::Utc::now).await.unwrap_err()
        );
        assert!(why.contains("LIVE"), "{why}");
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        out_of_force(&p);
        let record = &p["metadata"][STOPPED_KEY];
        assert_eq!(record["candidate"], "unsettled", "{record}");
        assert!(record["why"].as_str().unwrap().contains("LIVE"));
        assert_eq!(
            statuses(&w).await,
            vec![(1, "retired".to_string()), (2, "active".to_string())],
            "nothing was rolled back or withdrawn"
        );
    }

    /// Review 5308927f F3: `decide`'s verdict corrected (design
    /// 4105b020). The workflow's forks route on the stored verdict while
    /// every reader is handed the correction, so the verb settles the
    /// candidate on neither — and, per F1, still takes the split out.
    #[tokio::test]
    async fn a_corrected_verdict_takes_the_split_out_and_settles_nothing() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-corrected"),
            50,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        decided(&w, &id, "retire").await;
        crate::correct::correct(
            &w,
            &id,
            "decide",
            json!({"field": "verdict", "reads": "retire", "should_read": "promote",
                   "why": "the measure was read backwards"}),
        )
        .await
        .unwrap();

        let why = format!(
            "{:#}",
            stop(&w, &id, false, chrono::Utc::now).await.unwrap_err()
        );
        assert!(why.contains("correction [0]"), "{why}");
        let p = get(&w, &format!("/api/jobs/{id}")).await;
        out_of_force(&p);
        assert_eq!(p["metadata"][STOPPED_KEY]["candidate"], "unsettled");
        assert_eq!(
            statuses(&w).await,
            vec![(1, "active".to_string()), (2, "draft".to_string())],
            "neither published nor discarded"
        );
    }

    /// A withdrawn correction is no correction: the verdict stands.
    #[test]
    fn only_a_live_correction_of_the_verdict_holds_the_settle() {
        let decide = |corrections: Value| {
            json!({"steps": [{"spec_slug": "decide", "status": "completed",
                              "metadata": {"verdict": "retire"},
                              "corrections": corrections}]})
        };
        let fix = json!({"index": 0, "field": "verdict", "reads": "retire",
                         "should_read": "promote", "by": "emp-ceo"});
        assert!(verdict_correction(&decide(json!([fix.clone()]))).is_some());
        let undo = json!({"index": 1, "field": "verdict", "withdraws": 0, "why": "no"});
        assert_eq!(verdict_correction(&decide(json!([fix, undo]))), None);
        let other = json!({"index": 0, "field": "decision", "reads": "d",
                           "should_read": "e"});
        assert_eq!(verdict_correction(&decide(json!([other]))), None);
        assert_eq!(verdict_correction(&decide(json!([]))), None);
    }

    /// Review 5308927f F2: a stop before `decide` discards the candidate;
    /// a `promote` given afterwards used to meet "nothing to do" and exit
    /// 0, the record saying discarded while decide said promote. It is
    /// refused, naming the verb that publishes the candidate's text.
    #[tokio::test]
    async fn a_promote_after_the_candidate_was_withdrawn_is_refused_not_a_no_op() {
        let w = wire(serve().await);
        let id = stated(&w).await;
        start(
            &w,
            &id,
            KIND,
            &candidate_file("experiment-early-stop"),
            0,
            false,
            chrono::Utc::now,
        )
        .await
        .unwrap();
        stop(&w, &id, false, chrono::Utc::now).await.unwrap();
        decided(&w, &id, "promote").await;

        let why = format!(
            "{:#}",
            stop(&w, &id, false, chrono::Utc::now).await.unwrap_err()
        );
        assert!(why.contains("promote"), "{why}");
        assert!(why.contains("boss workflow publish"), "{why}");
        assert_eq!(statuses(&w).await, vec![(1, "active".to_string())]);
    }
}
