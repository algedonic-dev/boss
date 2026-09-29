//! `boss recovery reprint --tree <dir>` — the daily check that the
//! paper way back in still says what the tree says, and the print job
//! when it does not (design 125d405d §6-7, car 3; backlog fd6d6c08).
//!
//! WHAT IS ON PAPER IS A SIGNATURE, NOT A BELIEF. The printed version is
//! the `fact_hash` on the `print` step of the newest
//! `reprint-recovery-sheet` packet whose `print` completed `approved`
//! with a live PRESENCE stamp — David's passkey over the machine-filled
//! statement "printed version X, destroyed version Y". Nothing else
//! records it: no file, no setting, no memory of having printed it. So
//! "the sheet in the drawer is current" is never a feeling about the
//! past; it is two hashes compared, and the answer is printed with both.
//!
//! WHAT THE CHECK DOES, in order, each branch a `verdict: ` line the
//! chore wrapper keeps on its packet:
//!   - the tree's version equals the signed one, and no copy of it was
//!     reported missing since: CURRENT. Then the 90-day existence check
//!     (David's Q2): when the newer of the signature and the last `kept`
//!     confirmation is [`EXISTENCE_CHECK_DAYS`] old and none is open, it
//!     files ONE `confirm-recovery-sheet-kept` packet;
//!   - different (or nothing ever signed — bootstrap is the normal path —
//!     or the signed copy reported missing), and no print job open: it
//!     renders the PDF, FILES one print job, attaches the PDF to its
//!     `print` step through file_refs, and records the fact-level diff;
//!   - different, and one print job already open: it REFRESHES that one
//!     in place — attaches the new PDF, rewrites the keys the passkey
//!     will sign and the diff, and appends the render it replaced to
//!     `superseded_renders` — so the queue holds one print job, never a
//!     pile (decision 7). An edit to the signed keys voids any stamp
//!     already on the step, which is the point: one signature, one
//!     render;
//!   - the open print job already asks for this version with its PDF
//!     attached: PENDING, and nothing is written.
//!
//! A stale sheet is not a failed run: the print job is the loud part,
//! in the platform owner's queue. A failed run is one that could not
//! render, print, read or write — and that is an error, so the chore
//! records `failed` with this verb's own words.
//!
//! CONFIRMATION OVER STATUS CODES (the rule `boss job patch` and `boss
//! attach` state). Every write is read back: the packet filed, the PDF
//! (`attach_at` hashes the bytes the store serves back), and each key
//! the passkey will sign. A packet whose keys did not land is refused
//! by name, never reported filed.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::attach::{Target, attach_at};
use crate::identity;
use crate::steps::Wire;

/// The print job's protocol and the step the passkey signs
/// (infra/platform/workflows/reprint-recovery-sheet.toml).
pub const REPRINT_KIND: &str = "reprint-recovery-sheet";
pub const PRINT_STEP: &str = "print";
/// The 90-day existence check's protocol and its signed step
/// (infra/platform/workflows/confirm-recovery-sheet-kept.toml).
pub const KEPT_KIND: &str = "confirm-recovery-sheet-kept";
pub const CONFIRM_STEP: &str = "confirm";

/// How old the newest proof that the paper exists may grow before the
/// check asks for another: David's answer to design 125d405d Q2,
/// 2026-09-28 — "reprint on every change, plus a 90-day existence
/// check", because a sheet nobody can find fails exactly like a stale
/// one. ONE constant; the protocol's header names it rather than
/// repeating it.
pub const EXISTENCE_CHECK_DAYS: i64 = 90;

/// The subject both protocols are filed about.
const SUBJECT: &str = "recovery-sheet";

/// The keys the passkey signs on `print`, each filled by this verb —
/// the step's `filled_by = "filer"` fields, bound from the packet at
/// filing and rewritten on the step at a refresh. `pdf_file_ref` joins
/// them once the PDF is attached.
const SIGNED: [&str; 7] = [
    "fact_hash",
    "version",
    "pdf_sha256",
    "origin_sha",
    "rendered_at",
    "replaces_version",
    "statement",
];

/// How many characters of the fact hash the paper prints as its
/// version, and so how the version is named everywhere a reader
/// compares it with the page in hand.
pub fn version_of(fact_hash: &str) -> &str {
    fact_hash.get(..super::VERSION_CHARS).unwrap_or(fact_hash)
}

/// The sha256 a fact set hashes to — the same join the renderer's
/// version is taken over (`Rendered::hash`).
pub fn hash_of(facts: &[String]) -> String {
    hex::encode(Sha256::digest(facts.join("\n").as_bytes()))
}

// ----- the record, read ------------------------------------------------

/// What is on paper, by the record: the signed print step.
#[derive(Debug, Clone, PartialEq)]
pub struct OnPaper {
    pub fact_hash: String,
    pub packet: String,
    pub signed_at: DateTime<Utc>,
    /// The fact set that version was rendered from — the old side of the
    /// next diff — only when the packet's copy hashes to the signed
    /// version; a set that does not is not used.
    pub facts: Option<Vec<String>>,
}

/// The one print job still open.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPrint {
    pub packet: String,
    pub print_step: String,
    /// The version its `print` step asks to be printed.
    pub fact_hash: String,
    /// Its `print` step carries the attached PDF's file id, and that
    /// PDF's sha256 is the one the packet names: a filing whose attach
    /// or read-back failed is refreshed, not left pending forever.
    pub whole: bool,
    /// The packet's metadata, for `superseded_renders`.
    pub metadata: Value,
}

/// The newest closed existence check.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirmation {
    pub packet: String,
    pub at: DateTime<Utc>,
    pub kept: bool,
    pub fact_hash: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    pub on_paper: Option<OnPaper>,
    pub open_print: Option<OpenPrint>,
    pub last_confirmation: Option<Confirmation>,
    /// The newest `kept` confirmation's time, for the 90-day clock.
    pub last_kept_at: Option<DateTime<Utc>>,
    pub open_confirmation: Option<String>,
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn status(job: &Value) -> &str {
    job.get("status").and_then(Value::as_str).unwrap_or("")
}

fn step_of(job: &Value, slug: &str) -> Result<boss_core::job::Step> {
    let id = crate::envelope::job_id(job).unwrap_or("?");
    let raw = crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
        .ok_or_else(|| anyhow!("packet {id} has no `{slug}` step"))?;
    serde_json::from_value(raw.clone())
        .with_context(|| format!("packet {id}: its `{slug}` step does not read as a step"))
}

/// A signed step: completed, `approved`, and carrying a LIVE presence
/// stamp — the stamp that proves the platform owner was there, on the
/// step's content as it stands (`Step::live_stamps`). The API refuses
/// the completion without one; this reads it back rather than trusting
/// that it did.
fn signed_at(step: &boss_core::job::Step) -> Option<DateTime<Utc>> {
    let approved = text(&step.metadata, "decision").as_deref() == Some("approved");
    let present = step
        .live_stamps()
        .any(|s| s.assurance == boss_core::job::Assurance::Presence);
    (step.status == boss_core::job::StepStatus::Completed && approved && present)
        .then_some(step.completed_at)
        .flatten()
}

/// Read the record from every packet of the two protocols. Pure over
/// the rows, so each rule is pinned without a socket. Refuses two open
/// print jobs (or two open checks) rather than guess which to refresh.
pub fn read_record(prints: &[Value], checks: &[Value]) -> Result<Record> {
    let mut record = Record::default();
    let mut open = Vec::new();
    for job in prints {
        let id = crate::envelope::job_id(job).unwrap_or("?").to_string();
        let step = step_of(job, PRINT_STEP)?;
        if let Some(at) = signed_at(&step) {
            let fact_hash = text(&step.metadata, "fact_hash")
                .ok_or_else(|| anyhow!("packet {id}: its signed `print` step has no fact_hash"))?;
            if record.on_paper.as_ref().is_none_or(|p| at > p.signed_at) {
                let md = job.get("metadata").cloned().unwrap_or(Value::Null);
                let facts = md
                    .get("facts")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .filter(|f| hash_of(f) == fact_hash);
                record.on_paper = Some(OnPaper {
                    fact_hash,
                    packet: id.clone(),
                    signed_at: at,
                    facts,
                });
            }
        }
        if status(job) == "open" {
            let md = job.get("metadata").cloned().unwrap_or(Value::Null);
            let fact_hash = text(&step.metadata, "fact_hash").unwrap_or_default();
            let whole = text(&step.metadata, "pdf_file_ref").is_some()
                && text(&step.metadata, "pdf_sha256").is_some()
                && text(&step.metadata, "pdf_sha256") == text(&md, "pdf_sha256");
            open.push(OpenPrint {
                packet: id,
                print_step: step.id.to_string(),
                fact_hash,
                whole,
                metadata: md,
            });
        }
    }
    if open.len() > 1 {
        let ids: Vec<&str> = open.iter().map(|o| o.packet.as_str()).collect();
        bail!(
            "{} {REPRINT_KIND} packets are open ({}); the check refreshes ONE and will not guess \
             which — decline all but one",
            open.len(),
            ids.join(", ")
        );
    }
    record.open_print = open.pop();

    let mut open_checks = Vec::new();
    for job in checks {
        let id = crate::envelope::job_id(job).unwrap_or("?").to_string();
        if status(job) == "open" {
            open_checks.push(id);
            continue;
        }
        let step = step_of(job, CONFIRM_STEP)?;
        let Some(at) = step.completed_at else {
            continue;
        };
        if step.status != boss_core::job::StepStatus::Completed {
            continue;
        }
        let kept = signed_at(&step).is_some();
        if kept && record.last_kept_at.is_none_or(|k| at > k) {
            record.last_kept_at = Some(at);
        }
        if record.last_confirmation.as_ref().is_none_or(|c| at > c.at) {
            record.last_confirmation = Some(Confirmation {
                packet: id,
                at,
                kept,
                fact_hash: text(&step.metadata, "fact_hash").unwrap_or_default(),
            });
        }
    }
    if open_checks.len() > 1 {
        bail!(
            "{} {KEPT_KIND} packets are open ({}); decline all but one",
            open_checks.len(),
            open_checks.join(", ")
        );
    }
    record.open_confirmation = open_checks.pop();
    Ok(record)
}

// ----- the decision ----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Reprint {
    /// The paper says what the tree says. `stray` is an open print job
    /// asking for a version the tree no longer says, named so it is
    /// declined rather than printed.
    Current { stray: Option<String> },
    /// No print job open: file one.
    File { why: String },
    /// The open print job asks for another render, or lost its PDF:
    /// refresh it in place.
    Refresh { why: String },
    /// The open print job already asks for this version, PDF attached.
    Pending,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Existence {
    /// Nothing is on paper by record, or a reprint is owed: there is no
    /// copy worth confirming.
    NotApplicable,
    /// One is open.
    Open(String),
    /// Not yet: due on this date.
    NotDue(DateTime<Utc>),
    /// Due since this date: file one.
    Due(DateTime<Utc>),
}

/// The copy on paper was reported missing: the newest existence check
/// closed `missing`, for the version on paper, after it was signed.
fn reported_missing(record: &Record) -> Option<&Confirmation> {
    let paper = record.on_paper.as_ref()?;
    record
        .last_confirmation
        .as_ref()
        .filter(|c| !c.kept && c.at > paper.signed_at && c.fact_hash == paper.fact_hash)
}

/// What the check does, from the record and the tree's version. Pure.
pub fn plan(record: &Record, tree_hash: &str, now: DateTime<Utc>) -> (Reprint, Existence) {
    let tree = version_of(tree_hash);
    let owed = match (&record.on_paper, reported_missing(record)) {
        (None, _) => Some("nothing is on paper by record — the first print job".to_string()),
        (Some(p), Some(m)) => Some(format!(
            "the copy of version {} was reported missing on {} (packet {})",
            version_of(&p.fact_hash),
            m.at.format("%Y-%m-%d"),
            short(&m.packet)
        )),
        (Some(p), None) if p.fact_hash != tree_hash => Some(format!(
            "the tree says version {tree}, the paper says version {}",
            version_of(&p.fact_hash)
        )),
        (Some(_), None) => None,
    };
    let reprint = match (owed, &record.open_print) {
        (None, open) => Reprint::Current {
            stray: open
                .as_ref()
                .filter(|o| o.fact_hash != tree_hash)
                .map(|o| o.packet.clone()),
        },
        (Some(why), None) => Reprint::File { why },
        (Some(why), Some(o)) if o.fact_hash != tree_hash => Reprint::Refresh {
            why: format!("{why}; it asked for version {}", version_of(&o.fact_hash)),
        },
        (Some(why), Some(o)) if !o.whole => Reprint::Refresh {
            why: format!("{why}; its PDF is not attached as the packet names it"),
        },
        (Some(_), Some(_)) => Reprint::Pending,
    };
    let existence = match (&reprint, &record.on_paper) {
        (Reprint::Current { .. }, Some(p)) => match &record.open_confirmation {
            Some(open) => Existence::Open(open.clone()),
            None => {
                let seen = record
                    .last_kept_at
                    .filter(|k| *k > p.signed_at)
                    .unwrap_or(p.signed_at);
                let due = seen + Duration::days(EXISTENCE_CHECK_DAYS);
                if now >= due {
                    Existence::Due(due)
                } else {
                    Existence::NotDue(due)
                }
            }
        },
        _ => Existence::NotApplicable,
    };
    (reprint, existence)
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The sentence the passkey signs on `print`. The machine writes it
/// from the record, so the signer types nothing (decision 8 of the
/// design: a human act is a signature on rendered bytes, never a
/// transcription).
pub fn statement(fact_hash: &str, record: &Record) -> String {
    let new = version_of(fact_hash);
    match (&record.on_paper, reported_missing(record)) {
        (None, _) => format!(
            "I printed version {new} of the recovery sheet and put it where the paper copy is \
             kept. No earlier printed version is on record."
        ),
        (Some(p), Some(m)) if p.fact_hash == fact_hash => format!(
            "I printed version {new} of the recovery sheet again and put it where the paper copy \
             is kept; the copy signed for on {} (packet {}) was reported missing on {} (packet {}).",
            p.signed_at.format("%Y-%m-%d"),
            short(&p.packet),
            m.at.format("%Y-%m-%d"),
            short(&m.packet)
        ),
        (Some(p), _) => format!(
            "I printed version {new} of the recovery sheet, put it where the paper copy is kept, \
             and destroyed the copy of version {} signed for on {} (packet {}).",
            version_of(&p.fact_hash),
            p.signed_at.format("%Y-%m-%d"),
            short(&p.packet)
        ),
    }
}

// ----- the diff --------------------------------------------------------

/// One fact-set line, placed: the road it belongs to (its id, without
/// the position that would shift when a road is inserted) and what it
/// says (without its own position, for the same reason).
fn placed(line: &str) -> (String, String) {
    let (slot, rest) = line.split_once(' ').unwrap_or((line, ""));
    let Some((road, at)) = slot.split_once('/') else {
        return ("(the sheet)".to_string(), line.to_string());
    };
    let road = road
        .split_once('-')
        .map_or(road, |(n, id)| {
            if n.bytes().all(|b| b.is_ascii_digit()) {
                id
            } else {
                road
            }
        })
        .to_string();
    let body = if at.bytes().all(|b| b.is_ascii_digit()) {
        rest.to_string()
    } else if at.starts_with("needs/") {
        format!("needs {rest}")
    } else {
        format!("{at} {rest}")
    };
    (road, body)
}

/// A fact line's `label | file#key` and its value.
fn fact_parts(body: &str) -> Option<(&str, &str)> {
    let f = body.strip_prefix("fact ")?;
    let (label, rest) = f.split_once(" | ")?;
    let (source, value) = rest.split_once(" = ")?;
    Some((&f[..label.len() + 3 + source.len()], value))
}

/// The fact-level diff from the version on paper to the tree's, grouped
/// by road: a fact whose value moved is `road: label | file#key: old ->
/// new`; anything else that left or arrived is `road: - …` / `road: + …`.
/// Positions are ignored, so an inserted line is one `+`, not a cascade.
pub fn diff(old: &[String], new: &[String]) -> Vec<String> {
    let set = |lines: &[String]| -> BTreeSet<(String, String)> {
        lines.iter().map(|l| placed(l)).collect()
    };
    let (old, new) = (set(old), set(new));
    let gone: Vec<&(String, String)> = old.difference(&new).collect();
    let came: Vec<&(String, String)> = new.difference(&old).collect();
    let mut by_road: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut paired_new = BTreeSet::new();
    for (road, body) in &gone {
        let moved = fact_parts(body).and_then(|(key, old_v)| {
            came.iter().find_map(|(r2, b2)| {
                let (k2, new_v) = fact_parts(b2)?;
                (r2 == road && k2 == key).then_some((key, old_v, new_v, (r2, b2)))
            })
        });
        let line = match moved {
            Some((key, old_v, new_v, both)) => {
                paired_new.insert(both);
                format!("{road}: {key}: {old_v} -> {new_v}")
            }
            None => format!("{road}: - {body}"),
        };
        by_road.entry(road.as_str()).or_default().push(line);
    }
    for (road, body) in &came {
        if !paired_new.contains(&(road, body)) {
            by_road
                .entry(road.as_str())
                .or_default()
                .push(format!("{road}: + {body}"));
        }
    }
    by_road.into_values().flatten().collect()
}

// ----- the writes ------------------------------------------------------

/// The tree's sheet as this run rendered it — everything the record
/// needs, and a way to print the PDF only when a print job needs one.
pub struct Tree {
    pub fact_hash: String,
    pub facts: Vec<String>,
    pub origin_sha: String,
    pub rendered_at: String,
}

/// A printed PDF: its bytes and their sha256.
pub struct Pdf {
    pub bytes: Vec<u8>,
    pub sha256: String,
}

/// What the check did, line by line, for the chore's record.
pub type Report = Vec<String>;

/// The metadata one render writes on the packet: the signed keys, the
/// diff and the fact set it was rendered from, and the case the sign-off
/// surface shows above the Approve button.
fn render_metadata(tree: &Tree, pdf: &Pdf, record: &Record) -> Map<String, Value> {
    let replaces = record.on_paper.as_ref().map_or_else(
        || "none on record".to_string(),
        |p| version_of(&p.fact_hash).to_string(),
    );
    let diff_lines = match record.on_paper.as_ref() {
        None => vec!["(the first print job: nothing is on paper by record)".to_string()],
        Some(OnPaper { facts: None, .. }) => vec![
            "(the fact set of the version on paper is not on its packet, so the change cannot be \
             listed line by line; the version moved)"
                .to_string(),
        ],
        Some(OnPaper {
            facts: Some(old), ..
        }) => {
            let d = diff(old, &tree.facts);
            if d.is_empty() {
                vec!["(no fact changed; this reprint replaces a copy reported missing)".into()]
            } else {
                d
            }
        }
    };
    let statement = statement(&tree.fact_hash, record);
    let version = version_of(&tree.fact_hash).to_string();
    let context = format!(
        "**Print the recovery sheet, version {version}.** Download the PDF attached to the \
         `print` step (`recovery-sheet-{version}.pdf`, sha256 `{}`), print it, put it where the \
         paper copy is kept, destroy the copy it replaces, and approve with your passkey.\n\n\
         **What you sign:** {statement}\n\n**What changed since the version on paper** \
         ({replaces}), from the tree at `{}`:\n\n{}",
        pdf.sha256,
        tree.origin_sha,
        diff_lines
            .iter()
            .map(|l| format!("- {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut md = Map::new();
    for (k, v) in [
        ("fact_hash", tree.fact_hash.clone()),
        ("version", version),
        ("pdf_sha256", pdf.sha256.clone()),
        ("origin_sha", tree.origin_sha.clone()),
        ("rendered_at", tree.rendered_at.clone()),
        ("replaces_version", replaces),
        ("statement", statement),
        ("diff", diff_lines.join("\n")),
        ("context_md", context),
    ] {
        md.insert(k.to_string(), json!(v));
    }
    md.insert("facts".into(), json!(tree.facts));
    md
}

fn signed_keys(md: &Map<String, Value>) -> Map<String, Value> {
    SIGNED
        .iter()
        .chain(["pdf_file_ref"].iter())
        .filter_map(|k| md.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect()
}

/// Every key `want` names holds that value on `have`, else the keys
/// that did not land.
fn missing_keys(have: &Value, want: &Map<String, Value>) -> Vec<String> {
    want.iter()
        .filter(|(k, v)| have.get(k.as_str()) != Some(v))
        .map(|(k, _)| k.clone())
        .collect()
}

async fn read_all(wire: &Wire, kind: &str) -> Result<Vec<Value>> {
    crate::train::list_all_pages(|offset| {
        let path = format!(
            "/api/jobs?kind={kind}&full=true&limit={}&offset={offset}",
            crate::train::PAGE_LIMIT
        );
        async move { wire.call(reqwest::Method::GET, &path, None).await }
    })
    .await
}

/// Attach the PDF to the `print` step, then write `md` onto the packet
/// and the signed keys onto the step, and read both back.
async fn attach_and_write(
    wire: &Wire,
    content_base: &str,
    packet: &str,
    print_step: &str,
    pdf: &Pdf,
    version: &str,
    mut md: Map<String, Value>,
) -> Result<String> {
    let dir = tempfile::tempdir().context("a directory for the PDF")?;
    let path = dir.path().join(format!("recovery-sheet-{version}.pdf"));
    std::fs::write(&path, &pdf.bytes).with_context(|| format!("writing {}", path.display()))?;
    let target = Target {
        kind: "step",
        id: print_step.to_string(),
        label: format!("step `{PRINT_STEP}` of packet {}", short(packet)),
    };
    let signature = identity::Signature::As(wire.signer(&reqwest::Method::POST, "/api/files")?);
    let attached = attach_at(
        &crate::gate::machine_client()?,
        content_base,
        &target,
        &path,
        signature,
    )
    .await?;
    if attached.sha256 != pdf.sha256 {
        bail!(
            "the store holds file {} as sha256 {}, and the PDF printed is {}",
            attached.file_id,
            attached.sha256,
            pdf.sha256
        );
    }
    md.insert("pdf_file_ref".into(), json!(attached.file_id));
    // THE STEP FIRST (review of car 3, run ff8ffd04, finding 6). The
    // step's keys are what the passkey signs; the packet's `context_md`
    // is the unsigned text shown above it. Written the other way round,
    // the surface named the new version for a moment while the step
    // still held the old one.
    let signed = signed_keys(&md);
    wire.call(
        reqwest::Method::PATCH,
        &format!("/api/jobs/{packet}/steps/{print_step}/metadata"),
        Some(Value::Object(signed.clone())),
    )
    .await?;
    wire.call(
        reqwest::Method::PATCH,
        &format!("/api/jobs/{packet}/metadata"),
        Some(Value::Object(md.clone())),
    )
    .await?;

    // The status codes said yes; the packet is the authority.
    let job = wire.packet(packet).await?;
    confirm_written(packet, &job, &md, &signed)?;
    one_pdf_on_the_step(wire, content_base, &target, &attached.file_id).await?;
    Ok(attached.file_id)
}

/// PURE: the packet read back holds `md`, and its `print` step holds
/// `signed` — else a refusal naming every key that did not land, on
/// each. A 204 is the API's claim; this is the fact.
pub fn confirm_written(
    packet: &str,
    job: &Value,
    md: &Map<String, Value>,
    signed: &Map<String, Value>,
) -> Result<()> {
    let on_job = missing_keys(job.get("metadata").unwrap_or(&Value::Null), md);
    let step = step_of(job, PRINT_STEP)?;
    let on_step = missing_keys(&step.metadata, signed);
    if !on_job.is_empty() || !on_step.is_empty() {
        bail!(
            "packet {packet} does not hold what was written — on the packet: [{}]; on its \
             `print` step: [{}]",
            on_job.join(", "),
            on_step.join(", ")
        );
    }
    Ok(())
}

/// ONE PDF ON THE STEP (review of car 3, finding 4). A refresh attaches
/// a new file_ref beside the replaced one, and a download list holding
/// `recovery-sheet-<old>.pdf` next to the new one lets the wrong page
/// be printed under a signature naming the new version. Every file on
/// the step but `keep` is soft-deleted (`DELETE /api/files/{id}`, which
/// records `content.file.detached`; the bytes and the row stay), and
/// the list is read back: exactly `keep`, or a refusal naming what is
/// still there.
async fn one_pdf_on_the_step(
    wire: &Wire,
    content_base: &str,
    target: &Target,
    keep: &str,
) -> Result<()> {
    let base = content_base.trim_end_matches('/');
    // A machine sender stamps the estate token (design 6805c764 car 2):
    // the content port refuses an unstamped `x-boss-user` once its gate
    // enforces, and this verb is a new sender, so it takes the door
    // every new one takes rather than joining the NOT_YET list.
    let http = boss_core::machine_token::Client::build(reqwest::Client::builder())
        .context("a client for the file store")?;
    let list_url = format!(
        "{base}/api/files?target_kind={}&target_id={}",
        target.kind, target.id
    );
    let ids = |body: &Value| -> Vec<String> {
        body.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.get("id").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let list = |signer: String| {
        let (http, url) = (http.clone(), list_url.clone());
        async move {
            let resp = http
                .get(&url)
                .header("x-boss-user", identity::header(&signer))
                .send()
                .await
                .with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            let body: Value = resp
                .json()
                .await
                .with_context(|| format!("GET {url} -> {status}, not JSON"))?;
            if !status.is_success() || !body.is_array() {
                bail!("GET {url} -> {status}: {body}");
            }
            Ok::<Value, anyhow::Error>(body)
        }
    };
    let reader = wire.signer(&reqwest::Method::GET, "/api/files")?;
    for id in ids(&list(reader.clone()).await?)
        .into_iter()
        .filter(|id| id != keep)
    {
        let url = format!("{base}/api/files/{id}");
        let signer = wire.signer(&reqwest::Method::DELETE, "/api/files")?;
        let resp = http
            .delete(&url)
            .header("x-boss-user", identity::header(&signer))
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!(
                "the replaced PDF {id} was not detached from {}: DELETE {url} -> {status}: {}",
                target.label,
                resp.text().await.unwrap_or_default().trim()
            );
        }
    }
    let left = ids(&list(reader).await?);
    if left != [keep] {
        bail!(
            "{} lists [{}] after the replaced PDFs were detached, not exactly the new one {keep}",
            target.label,
            left.join(", ")
        );
    }
    Ok(())
}

/// The check, on an explicit wire and file store, with the PDF printed
/// only when a print job needs one — the seam the test drives against
/// the real jobs and files routers.
pub async fn check(
    wire: &Wire,
    content_base: &str,
    tree: &Tree,
    print: impl FnOnce() -> Result<Pdf>,
    now: DateTime<Utc>,
) -> Result<Report> {
    let prints = read_all(wire, REPRINT_KIND).await?;
    let checks = read_all(wire, KEPT_KIND).await?;
    let record = read_record(&prints, &checks)?;
    let (reprint, existence) = plan(&record, &tree.fact_hash, now);
    let version = version_of(&tree.fact_hash).to_string();

    let mut out = vec![format!(
        "tree     version {version} ({}) at {}",
        tree.fact_hash, tree.origin_sha
    )];
    out.push(match &record.on_paper {
        Some(p) => format!(
            "on paper version {} ({}) signed {} on packet {}",
            version_of(&p.fact_hash),
            p.fact_hash,
            p.signed_at.to_rfc3339(),
            p.packet
        ),
        None => "on paper nothing on record".to_string(),
    });

    match reprint {
        Reprint::Current { stray } => {
            out.push(format!(
                "verdict: current — the paper and the tree both say version {version}"
            ));
            if let Some(s) = stray {
                out.push(format!(
                    "note: print job {s} is still open for a version the tree no longer says — \
                     decline it rather than print it"
                ));
            }
        }
        Reprint::Pending => {
            let open = record
                .open_print
                .as_ref()
                .map_or("?", |o| o.packet.as_str());
            out.push(format!(
                "verdict: print job pending — packet {open} already asks for version {version}, \
                 its PDF attached; nothing written"
            ));
        }
        Reprint::File { why } => {
            let pdf = print()?;
            let md = render_metadata(tree, &pdf, &record);
            let body = crate::job::envelope(
                REPRINT_KIND,
                "Print the recovery sheet: the tree says what the paper does not",
                None,
                Some(SUBJECT),
                &wire.signer(&reqwest::Method::POST, "/api/jobs")?,
                Some(Value::Object(md.clone())),
            );
            let created = wire
                .call(reqwest::Method::POST, "/api/jobs", Some(body))
                .await?
                .context("the create returned no body")?;
            let packet = crate::envelope::job_id(&created)
                .context("the create returned no id — refusing to call that filed")?
                .to_string();
            let job = wire.packet(&packet).await?;
            let step = step_of(&job, PRINT_STEP)?;
            let file = attach_and_write(
                wire,
                content_base,
                &packet,
                &step.id.to_string(),
                &pdf,
                &version,
                md.clone(),
            )
            .await?;
            out.push(format!(
                "verdict: print job filed — packet {packet} asks for version {version} ({why}); \
                 PDF file {file}, sha256 {}",
                pdf.sha256
            ));
            out.extend(diff_report(&md));
        }
        Reprint::Refresh { why } => {
            let open = record
                .open_print
                .as_ref()
                .context("a refresh with no open print job")?;
            let pdf = print()?;
            let mut md = render_metadata(tree, &pdf, &record);
            let mut superseded: Vec<Value> = open
                .metadata
                .get("superseded_renders")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let was = |k: &str| open.metadata.get(k).cloned().unwrap_or(Value::Null);
            superseded.push(json!({
                "fact_hash": was("fact_hash"),
                "version": was("version"),
                "pdf_sha256": was("pdf_sha256"),
                "pdf_file_ref": was("pdf_file_ref"),
                "rendered_at": was("rendered_at"),
                "superseded_at": now.to_rfc3339(),
            }));
            md.insert("superseded_renders".into(), Value::Array(superseded));
            let file = attach_and_write(
                wire,
                content_base,
                &open.packet,
                &open.print_step,
                &pdf,
                &version,
                md.clone(),
            )
            .await?;
            out.push(format!(
                "verdict: print job refreshed — packet {} now asks for version {version} ({why}); \
                 PDF file {file}, sha256 {}",
                open.packet, pdf.sha256
            ));
            out.extend(diff_report(&md));
        }
    }

    match existence {
        Existence::NotApplicable => {}
        Existence::Open(p) => out.push(format!("existence: check open on packet {p}")),
        Existence::NotDue(due) => out.push(format!(
            "existence: next check due {} ({EXISTENCE_CHECK_DAYS} days after the paper was last \
             confirmed)",
            due.format("%Y-%m-%d")
        )),
        Existence::Due(since) => {
            let paper = record
                .on_paper
                .as_ref()
                .context("an existence check with nothing on paper")?;
            let v = version_of(&paper.fact_hash).to_string();
            let md = json!({
                "fact_hash": paper.fact_hash,
                "version": v,
                "print_packet": paper.packet,
                "printed_at": paper.signed_at.to_rfc3339(),
                "statement": format!(
                    "The printed recovery sheet, version {v} (signed for on {} on packet {}), is \
                     where the paper copy is kept, and its footer says version {v}.",
                    paper.signed_at.format("%Y-%m-%d"),
                    short(&paper.packet)
                ),
            });
            let body = crate::job::envelope(
                KEPT_KIND,
                "Confirm the printed recovery sheet is where it is kept",
                None,
                Some(SUBJECT),
                &wire.signer(&reqwest::Method::POST, "/api/jobs")?,
                Some(md.clone()),
            );
            let created = wire
                .call(reqwest::Method::POST, "/api/jobs", Some(body))
                .await?
                .context("the create returned no body")?;
            let packet = crate::envelope::job_id(&created)
                .context("the create returned no id — refusing to call that filed")?
                .to_string();
            let job = wire.packet(&packet).await?;
            let step = step_of(&job, CONFIRM_STEP)?;
            let want: Map<String, Value> = md.as_object().cloned().unwrap_or_default();
            let gone = missing_keys(&step.metadata, &want);
            if !gone.is_empty() {
                bail!(
                    "packet {packet} was filed but its `{CONFIRM_STEP}` step does not hold [{}]",
                    gone.join(", ")
                );
            }
            out.push(format!(
                "verdict: existence check filed — packet {packet} asks that the paper copy of \
                 version {v} be confirmed where it is kept (due since {})",
                since.format("%Y-%m-%d")
            ));
        }
    }
    Ok(out)
}

fn diff_report(md: &Map<String, Value>) -> Vec<String> {
    md.get("diff")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .lines()
        .map(|l| format!("  changed {l}"))
        .collect()
}

#[cfg(test)]
mod tests;
