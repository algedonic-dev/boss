//! THE BOUNDED START — the CLI half of the execution-inputs/v1 contract
//! (design e72dc7c9, answering backlog 40f0436c; Q1 approved 2026-10-15).
//!
//! WHY. `boss dispatch` printed every value a packet had ever recorded and
//! the worker receipt asked for the ENTIRE START to be read first: 1,002,073
//! bytes on SOURCE f08a8483 and 1,003,270 on 266f06e8, an executor still
//! reading Oct 10 gate-window observations at byte 280,000 of an urgent
//! repair. The large brief was already conserved as a verified attachment
//! (`dispatch_brief`); what nothing bounded was the reading.
//!
//! WHAT. A packet whose PINNED Workflow version declares the contract (see
//! `boss_jobs::execution_inputs`, which owns every rule about which value is
//! which) is dispatched with a START that prints its REQUIRED inputs in full
//! — the current scope, the step's specification, the rules, the own-run
//! section — and then a manifest of the values it does not print, each a
//! MANDATORY original the worker still reads in full before `--started`, or
//! an AVAILABLE original it is not claimed to have read. Nothing is reduced:
//! the packet GET's raw bytes and the complete legacy brief are both retained
//! as verified attachments, every manifest entry binds a pointer into that
//! snapshot with its exact size and sha256, and `boss dispatch <run>
//! --original <n|snapshot|legacy>` reads any of them back through the same
//! verifier. The START is bounded at 128 KiB including the rules and the
//! manifest, and a packet whose required material does not fit is REFUSED,
//! naming what is too large — never clipped.
//!
//! A packet whose row declares nothing, a handed prompt, and every run
//! admitted before this are untouched: legacy whole-START, as before.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;

use boss_jobs::execution_inputs::{self as ei, Class, Declaration, Evaluation};

/// The run-metadata key the admitted contract rides under: the manifest, the
/// two whole originals, and which declaration it was rendered with.
pub(crate) const RUN_KEY: &str = ei::DECLARATION_KEY;

/// The packet's pinned Workflow version, if it carries one.
pub(crate) fn pinned_version(job: &Value) -> Option<i64> {
    job.get("workflow_version").and_then(Value::as_i64)
}

/// The declaration the packet's PINNED version carries. `active` is the
/// registry's active row as the dispatch already read it; when it is not the
/// pinned version, `fetch` reads `/api/workflows/{kind}/versions/{v}`.
///
/// `Ok(None)` is the legacy contract: the row declares none, the packet
/// carries no pinned version, or the pinned row could not be read — and the
/// last is SAID, because it falls back to the whole START, which conserves
/// everything and bounds nothing. A row that declares the contract
/// malformed is an `Err`: a declaration this CLI cannot apply honestly never
/// makes anything optional and never quietly becomes legacy.
pub(crate) async fn declared_for<F, Fut>(
    job: &Value,
    active: Option<&Value>,
    fetch: F,
) -> Result<Option<(Declaration, i64)>>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let Some(pinned) = pinned_version(job) else {
        return Ok(None);
    };
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("-");
    let same = active.filter(|r| r.get("version").and_then(Value::as_i64) == Some(pinned));
    let row = match same {
        Some(row) => row.clone(),
        None => match fetch(format!("/api/workflows/{kind}/versions/{pinned}")).await {
            Ok(Some(row)) => row,
            Ok(None) | Err(_) => {
                eprintln!(
                    "boss dispatch: {kind} v{pinned} (this packet's pinned version) could not be \
                     read, so whether it declares {} is unknown — the run keeps the legacy \
                     whole-START contract",
                    ei::CONTRACT
                );
                return Ok(None);
            }
        },
    };
    let row = row.get("data").filter(|d| d.is_object()).unwrap_or(&row);
    Declaration::of_row(row)
        .map(|d| d.map(|d| (d, pinned)))
        .map_err(|e| anyhow::anyhow!("{kind} v{pinned}: {e}"))
}

/// The pointers a bounded packet section lists rather than prints.
pub(crate) fn listed(eval: &Evaluation) -> BTreeSet<String> {
    eval.listed().map(|i| i.pointer.clone()).collect()
}

/// The manifest: every listed value, MANDATORY first, numbered from 1 in
/// the order a reader meets them.
pub(crate) fn manifest(eval: &Evaluation) -> Value {
    let ordered = eval
        .inputs
        .iter()
        .filter(|i| i.class == Class::MandatoryOriginal)
        .chain(
            eval.inputs
                .iter()
                .filter(|i| i.class == Class::AvailableHistorical),
        );
    Value::Array(
        ordered
            .enumerate()
            .map(|(n, i)| {
                json!({
                    "n": n + 1,
                    "pointer": i.pointer,
                    "class": i.class.word(),
                    "size_bytes": i.size_bytes,
                    "sha256": i.sha256,
                    "purpose": i.purpose,
                    "provenance": i.provenance,
                })
            })
            .collect(),
    )
}

/// What the run records about its contract — the one object the START's
/// closing section is rendered from and `--original` reads back.
pub(crate) fn record(
    kind: &str,
    version: i64,
    declared: &Declaration,
    eval: &Evaluation,
    snapshot: Value,
    legacy: Value,
) -> Value {
    let manifest = manifest(eval);
    json!({
        "contract": ei::CONTRACT,
        "workflow": { "kind": kind, "version": version },
        "declaration_sha256": declared.sha256(),
        "start_bytes_bound": ei::START_BYTES,
        "snapshot": snapshot,
        "legacy_brief": legacy,
        "manifest_sha256": ei::sha256_hex(&ei::value_bytes(&manifest)),
        "manifest": manifest,
        "unapplied": eval.unapplied,
    })
}

/// The worker-receipt sentence for a bounded run: what `--started` will
/// state, and — as plainly — what it will not.
pub(crate) fn receipt_line(run_id: &str, worker_root: &str) -> String {
    format!(
        "\nWorker receipt: this START runs the {contract} contract. After reading this ENTIRE \
         START, and every MANDATORY original THE ORIGINALS lists below in full (`boss dispatch \
         {run_id} --original <n>` verifies each against the run's manifest and prints it), run \
         `BOSS_AGENT_RUN={run_id} boss dispatch {run_id} --started{worker_root}`. That receipt states \
         exactly that, bound to this START's sha256 and the manifest's; it claims nothing \
         about the AVAILABLE originals. When your own run's building step is READY and \
         assigned to your running actor, the command first takes it through the claim door, \
         which makes it ACTIVE. It carries no timestamp and claims neither physical execution \
         nor delivery. A coordinator claim, host or session cannot substitute; if the command \
         refuses because the step is assigned to another actor or to nobody, report that \
         blocker to the coordinator and claim nothing.\n",
        contract = ei::CONTRACT
    )
}

fn entry_line(run_id: &str, e: &Value) -> String {
    let s = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("?");
    format!(
        "  [{n}] {pointer} — {bytes} bytes — sha256 {sha}\n      purpose: {purpose}\n      \
         classified by: {by}\n      read it: boss dispatch {run_id} --original {n}\n",
        n = e.get("n").and_then(Value::as_u64).unwrap_or(0),
        pointer = s("pointer"),
        bytes = e.get("size_bytes").and_then(Value::as_u64).unwrap_or(0),
        sha = s("sha256"),
        purpose = s("purpose"),
        by = s("provenance"),
    )
}

fn whole_line(run_id: &str, name: &str, what: &str, r: &Value) -> String {
    format!(
        "  {name} — {what}: {} bytes, sha256 {}, file {} — boss dispatch {run_id} --original \
         {name}\n",
        r.get("size_bytes").and_then(Value::as_u64).unwrap_or(0),
        r.get("sha256").and_then(Value::as_str).unwrap_or("?"),
        r.get("file_id").and_then(Value::as_str).unwrap_or("?"),
    )
}

/// THE ORIGINALS — the START's last section, AFTER the run section on
/// purpose (design e72dc7c9: authority and the own-run identity are reached
/// before the historical inventory).
pub(crate) fn originals_section(run_id: &str, record: &Value) -> String {
    let manifest: Vec<&Value> = record
        .get("manifest")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let class = |e: &&Value| e.get("class").and_then(Value::as_str).map(str::to_string);
    let mandatory: Vec<&Value> = manifest
        .iter()
        .copied()
        .filter(|e| class(e).as_deref() == Some(Class::MandatoryOriginal.word()))
        .collect();
    let available: Vec<&Value> = manifest
        .iter()
        .copied()
        .filter(|e| class(e).as_deref() == Some(Class::AvailableHistorical.word()))
        .collect();
    let w = &record["workflow"];
    let mut out = format!(
        "\n== THE ORIGINALS — what this START lists instead of printing ({contract}) ==\n\n\
         {kind} v{version}, pinned at admission, declares {contract} (declaration sha256 {decl}). \
         Every value of the packet that is not listed here is printed above in full. Each \
         listed value is retained verbatim in the packet snapshot below, and the run's manifest \
         (sha256 {msha}) binds its pointer, size and sha256.\n",
        contract = ei::CONTRACT,
        kind = w.get("kind").and_then(Value::as_str).unwrap_or("?"),
        version = w.get("version").and_then(Value::as_i64).unwrap_or(0),
        decl = record
            .get("declaration_sha256")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        msha = record
            .get("manifest_sha256")
            .and_then(Value::as_str)
            .unwrap_or("?"),
    );
    out.push_str(&format!(
        "\nMANDATORY originals ({}) — read each IN FULL before your `--started` receipt:\n",
        mandatory.len()
    ));
    if mandatory.is_empty() {
        out.push_str("  (none)\n");
    }
    for e in &mandatory {
        out.push_str(&entry_line(run_id, e));
    }
    out.push_str(&format!(
        "\nAVAILABLE originals ({}) — retained verbatim and retrievable. NOT required reading \
         and NOT claimed read by your receipt; available is not current, not satisfied, not \
         green and not superseded — read one whenever the work turns on it:\n",
        available.len()
    ));
    if available.is_empty() {
        out.push_str("  (none)\n");
    }
    for e in &available {
        out.push_str(&entry_line(run_id, e));
    }
    out.push_str("\nWHOLE originals, verified byte for byte on every read:\n");
    out.push_str(&whole_line(
        run_id,
        "snapshot",
        "the packet GET exactly as the jobs API answered it at this dispatch",
        &record["snapshot"],
    ));
    out.push_str(&whole_line(
        run_id,
        "legacy",
        "the complete legacy brief, every value printed",
        &record["legacy_brief"],
    ));
    let unapplied: Vec<&str> = record
        .get("unapplied")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unapplied.is_empty() {
        out.push_str(
            "\nCLASSIFICATIONS NOT APPLIED — each value stays required and is printed above:\n",
        );
        for u in unapplied {
            out.push_str(&format!("  {u}\n"));
        }
    }
    out
}

/// The refusal when the START does not fit its bound or lists more
/// originals than the manifest holds individually — naming the largest
/// REQUIRED sources with their exact sizes, because those are the only
/// things a classification could move.
pub(crate) fn bound_refusal(start: &str, eval: &Evaluation, record: &Value) -> Option<String> {
    let listed = record
        .get("manifest")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if listed > ei::MANIFEST_ENTRIES {
        return Some(format!(
            "the START would list {listed} originals, over the {} a manifest holds \
             individually — refusing rather than hiding any of them in an inventory nobody \
             must read; no run was admitted",
            ei::MANIFEST_ENTRIES
        ));
    }
    if start.len() <= ei::START_BYTES {
        return None;
    }
    let mut required: Vec<_> = eval.required().collect();
    required.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));
    let largest: Vec<String> = required
        .iter()
        .take(10)
        .map(|i| {
            format!(
                "  {} — {} bytes — {}",
                i.pointer, i.size_bytes, i.provenance
            )
        })
        .collect();
    Some(format!(
        "the bounded START is {} bytes, over the {} byte bound of {} — refusing rather than \
         clipping required material; no run was admitted. The largest REQUIRED values:\n{}\n\
         A value that is historical can be classified as an original by a holder of publish \
         on workflow (`boss classify <packet> --pointer <p> --class available_historical`), \
         bound to its exact sha256; a value that still decides the current work stays \
         required.",
        start.len(),
        ei::START_BYTES,
        ei::CONTRACT,
        largest.join("\n")
    ))
}

/// What `--original` names: a manifest entry by number, or a whole original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Which {
    Entry(u64),
    Snapshot,
    Legacy,
}

impl Which {
    pub(crate) fn parse(text: &str) -> Result<Which> {
        match text.trim() {
            "snapshot" => Ok(Which::Snapshot),
            "legacy" => Ok(Which::Legacy),
            n => n
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(Which::Entry)
                .with_context(|| {
                    format!(
                        "--original takes a manifest number, `snapshot` or `legacy` — not {n:?}"
                    )
                }),
        }
    }
}

/// Read one original of a bounded run back, verified: a whole original
/// against its immutable attachment reference, a manifest entry against
/// the snapshot AND its own recorded size and sha256. Returns the exact
/// text and a one-line account of what was checked.
pub(crate) async fn original_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    content: &str,
    actor: &str,
    run_id: &str,
    which: &Which,
) -> Result<(String, String)> {
    let run = crate::sor_client::api_at_signed(
        http,
        base,
        reqwest::Method::GET,
        &format!("/api/jobs/{run_id}"),
        None,
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?
    .context("the run read returned no body")?;
    let run = run.get("data").filter(|d| d.is_object()).unwrap_or(&run);
    if run.get("kind").and_then(Value::as_str) != Some("agent-run") {
        bail!("{run_id} is not an agent-run");
    }
    let Some(record) = run.get("metadata").and_then(|m| m.get(RUN_KEY)) else {
        bail!(
            "run {run_id} was admitted under the legacy whole-START contract: its START is its \
             one original, and it lists none"
        );
    };
    if record.get("contract").and_then(Value::as_str) != Some(ei::CONTRACT) {
        bail!("run {run_id} records an execution-input contract this CLI does not implement");
    }
    let snapshot_ref = &record["snapshot"];
    let read_snapshot = crate::dispatch_brief::read_at(http, content, actor, snapshot_ref);
    match which {
        Which::Snapshot => {
            let text = read_snapshot.await?;
            Ok((text, whole_account("snapshot", snapshot_ref)))
        }
        Which::Legacy => {
            let legacy = &record["legacy_brief"];
            let text = crate::dispatch_brief::read_at(http, content, actor, legacy).await?;
            Ok((text, whole_account("legacy", legacy)))
        }
        Which::Entry(n) => {
            let entry = record
                .get("manifest")
                .and_then(Value::as_array)
                .and_then(|m| {
                    m.iter()
                        .find(|e| e.get("n").and_then(Value::as_u64) == Some(*n))
                })
                .with_context(|| format!("run {run_id}'s manifest lists no original [{n}]"))?;
            let pointer = entry
                .get("pointer")
                .and_then(Value::as_str)
                .context("the manifest entry names no pointer")?;
            let snapshot: Value = serde_json::from_str(&read_snapshot.await?)
                .context("the verified snapshot is not the JSON the jobs API answered")?;
            let value = ei::resolve(&snapshot, pointer).with_context(|| {
                format!("the verified snapshot holds no single value at {pointer}")
            })?;
            let bytes = ei::value_bytes(value);
            let sha = ei::sha256_hex(&bytes);
            if Some(bytes.len() as u64) != entry.get("size_bytes").and_then(Value::as_u64)
                || Some(sha.as_str()) != entry.get("sha256").and_then(Value::as_str)
            {
                bail!(
                    "original [{n}] at {pointer} does not match the run's manifest (size or \
                     sha256 disagrees) — not printed, and not readable as that original"
                );
            }
            let text = String::from_utf8(bytes)
                .context("the original is not UTF-8, so it has no readable representation")?;
            let class = entry.get("class").and_then(Value::as_str).unwrap_or("?");
            Ok((
                text,
                format!(
                    "original [{n}] {pointer} ({class}) — {} bytes, sha256 {sha}: matches the \
                     run's manifest and its verified snapshot",
                    entry["size_bytes"]
                ),
            ))
        }
    }
}

fn whole_account(name: &str, r: &Value) -> String {
    format!(
        "{name} — {} bytes, sha256 {}: verified against file {}",
        r.get("size_bytes").and_then(Value::as_u64).unwrap_or(0),
        r.get("sha256").and_then(Value::as_str).unwrap_or("?"),
        r.get("file_id").and_then(Value::as_str).unwrap_or("?"),
    )
}

/// `boss dispatch <run> --original <n|snapshot|legacy>`: the text on
/// stdout exactly, what was verified on stderr.
pub(crate) async fn original(run: Option<String>, which: String) -> Result<()> {
    let run = run.context("--original names the run: boss dispatch <run> --original <n>")?;
    let which = Which::parse(&which)?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let content = crate::tenant_publish::service_on_door(&base, "content")?;
    let actor = crate::identity::reader();
    let (text, account) = original_at(
        &crate::sor_client::machine_client()?,
        &base,
        &content,
        &actor,
        &run,
        &which,
    )
    .await?;
    print!("{text}");
    eprintln!("\nboss dispatch: {account}");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_nonworktree_bounded_receipt_keeps_its_own_started_instruction() {
        let line = receipt_line("own-run", "");
        assert!(line.contains("boss dispatch own-run --started`"));
        assert!(!line.contains("--local-root"));
    }

    #[test]
    fn which_parses_a_number_or_a_whole_original_and_nothing_else() {
        assert_eq!(Which::parse("3").unwrap(), Which::Entry(3));
        assert_eq!(Which::parse("snapshot").unwrap(), Which::Snapshot);
        assert_eq!(Which::parse("legacy").unwrap(), Which::Legacy);
        for bad in ["0", "-1", "all", "https://other.invalid/x"] {
            assert!(Which::parse(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn an_unreadable_pinned_row_is_legacy_and_said_and_a_malformed_one_refuses() {
        let job = json!({"kind": "backlog-item", "workflow_version": 16});
        let active = json!({"version": 17, "metadata": {}});
        let none = declared_for(&job, Some(&active), |_| async {
            Err(anyhow::anyhow!("unreadable"))
        })
        .await
        .unwrap();
        assert!(none.is_none());
        let pinned = json!({"version": 16, "metadata": {"execution_inputs": {"contract": "x"}}});
        assert!(
            declared_for(&job, Some(&pinned), |_| async { Ok(None) })
                .await
                .is_err()
        );
        let pinned =
            json!({"version": 16, "metadata": {"execution_inputs": {"contract": ei::CONTRACT}}});
        let (d, v) = declared_for(&job, Some(&active), |path| {
            assert_eq!(path, "/api/workflows/backlog-item/versions/16");
            let pinned = pinned.clone();
            async move { Ok(Some(pinned)) }
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!((d.contract.as_str(), v), (ei::CONTRACT, 16));
    }
}
