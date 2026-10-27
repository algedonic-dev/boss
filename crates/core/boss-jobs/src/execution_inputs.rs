//! EXECUTION INPUTS — which of a packet's recorded values an executor's
//! START prints in full, and which it hands over as verified originals
//! instead (design e72dc7c9, answering backlog 40f0436c; Q1 approved
//! 2026-10-15).
//!
//! MEASURED. A dispatched START printed every job metadata value and every
//! completed step's, uncurated, and asked the worker to read all of it
//! before its own receipt. On SOURCE 266f06e8 that was 1,003,270 bytes, one
//! 836,756-byte line of it an Oct 11 gate-window observation, with the
//! pinned protocol at byte 959,711 and the run's own identity at
//! 1,000,374. Conservation was already solved (a large brief rides as a
//! verified attachment); attention was not, and the obligation to read the
//! whole START was honest only for a worker that actually did.
//!
//! THE CONTRACT, opt-in per Workflow row and pinned with the packet's
//! version: a row whose `metadata` carries [`DECLARATION_KEY`] runs
//! [`CONTRACT`]. Every value of the packet is then one of three [`Class`]es
//! — printed REQUIRED input, a MANDATORY original the worker still reads in
//! full before `--started`, or an AVAILABLE historical original it is not
//! claimed to have read. Two things may classify a value below REQUIRED:
//!
//! - the Workflow row's own `selectors` (registry data, so publish
//!   authority already guards them), by exact pointer; and
//! - an operator's classification of ONE FROZEN VALUE, appended to the
//!   packet's reserved [`CLASSIFICATIONS_KEY`] list through the one door
//!   that asks publish on `workflow` (`POST
//!   /api/jobs/{id}/input-classifications`, `boss classify`). It binds the
//!   packet id, the pointer and the value's sha256, so a changed value, a
//!   copied entry or a new correction returns the value to REQUIRED.
//!
//! The contract's own complete classification record is a MANDATORY
//! original, or REQUIRED when the Workflow asks for that stronger form;
//! no selector or operator can make it optional. Everything else is
//! REQUIRED unless classified, never inferred from age, size or prose.
//! Operators cannot demote structural facts ([`protected`]): reserved
//! records (corrections, re-pins, the outcome, who opened it, the trust
//! boundary, these classifications), the item's `description`, every `decided_*`
//! decision, any field a correction names, and every value of a step the
//! current step's predicate reads. When classes from several sources
//! apply, the STRONGEST wins, so no source can weaken another.
//!
//! Pure: the door in `http::jobs` and the CLI's renderer both call it over
//! the same job JSON (the job GET's shape), so the server's refusal and the
//! executor's START cannot disagree about which value a pointer names.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// The contract a declaring Workflow row opts its packets into.
pub const CONTRACT: &str = "execution-inputs/v1";

/// The Workflow-row `metadata` key that declares [`CONTRACT`].
pub const DECLARATION_KEY: &str = "execution_inputs";

/// The reserved, append-only job-metadata key operator classifications
/// live under. Written only through the classification door; creation and
/// generic metadata PATCH refuse it and the job PUT carries it forward.
pub const CLASSIFICATIONS_KEY: &str = "input_classifications";

/// The whole emitted START, rules and own-run included, in UTF-8 bytes: a
/// measured attention bound, not a token estimate (design e72dc7c9).
pub const START_BYTES: usize = 128 * 1024;

/// How many originals a START may list individually.
pub const MANIFEST_ENTRIES: usize = 256;

/// The `worker_start_expectation` schema a bounded run's building step
/// carries. Schema 1 is the whole-START receipt and 2 a launcher
/// observation; a reader that knows neither refuses 3, which is what
/// keeps an older CLI from acknowledging a contract it cannot state.
pub const START_SCHEMA: u64 = 3;

/// The refusal the generic job metadata PATCH answers this key with.
pub const PATCH_REFUSAL: &str = "`input_classifications` is a reserved, append-only list: it \
     is written only through POST /api/jobs/{id}/input-classifications (`boss classify`), which \
     asks publish on workflow and binds each entry to the exact value it classifies. A metadata \
     patch could rewrite or erase it";

/// Job-metadata keys that are never classified below REQUIRED: the
/// reserved records every reader of the packet is owed, and the item's
/// own statement of its scope.
const PROTECTED_JOB_KEYS: [&str; 7] = [
    crate::corrections::CORRECTIONS_KEY,
    crate::repin::REPINS_KEY,
    crate::opened_by::OPENED_BY_KEY,
    crate::job_outcome::OUTCOME_KEY,
    CLASSIFICATIONS_KEY,
    "trust_boundary",
    "description",
];

/// The `decided_<date>_<source>` convention every design decision on an
/// item is recorded under (backlog 4f71e608, 46b55136).
const DECISION_PREFIX: &str = "decided_";

/// What a value is to the executor, weakest first so `max` is the
/// strongest claim any source makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Retained verbatim and retrievable; not required reading, not
    /// claimed read, and never a current verdict.
    AvailableHistorical,
    /// Not printed, but read in full before the own-run receipt.
    MandatoryOriginal,
    /// Printed in the START verbatim.
    Required,
}

impl Class {
    pub fn word(self) -> &'static str {
        match self {
            Class::AvailableHistorical => "available_historical",
            Class::MandatoryOriginal => "mandatory_original",
            Class::Required => "required",
        }
    }

    pub fn parse(word: &str) -> Option<Class> {
        match word {
            "available_historical" => Some(Class::AvailableHistorical),
            "mandatory_original" => Some(Class::MandatoryOriginal),
            "required" => Some(Class::Required),
            _ => None,
        }
    }
}

/// Where a value sits: one key of the packet's own metadata, or one key of
/// a completed step's, the step named by its `spec_slug` (else its id).
/// One key deep on purpose: a key's value is printed or listed whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Job { key: String },
    Step { slug: String, key: String },
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn unescape(segment: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = segment.chars();
    while let Some(c) = chars.next() {
        if c != '~' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('0') => out.push('~'),
            Some('1') => out.push('/'),
            _ => return None,
        }
    }
    Some(out)
}

impl Source {
    /// The RFC 6901 pointer a manifest, a selector and a classification
    /// spell this source with.
    pub fn pointer(&self) -> String {
        match self {
            Source::Job { key } => format!("/metadata/{}", escape(key)),
            Source::Step { slug, key } => {
                format!("/steps/{}/metadata/{}", escape(slug), escape(key))
            }
        }
    }

    /// The source a pointer names, or `None` for any other shape — a
    /// deeper path, an envelope field, a malformed escape.
    pub fn parse(pointer: &str) -> Option<Source> {
        let rest = pointer.strip_prefix('/')?;
        let parts: Vec<&str> = rest.split('/').collect();
        let parts: Vec<String> = parts
            .into_iter()
            .map(unescape)
            .collect::<Option<Vec<_>>>()?;
        if parts.iter().any(String::is_empty) {
            return None;
        }
        match parts.as_slice() {
            [m, key] if m == "metadata" => Some(Source::Job { key: key.clone() }),
            [s, slug, m, key] if s == "steps" && m == "metadata" => Some(Source::Step {
                slug: slug.clone(),
                key: key.clone(),
            }),
            _ => None,
        }
    }
}

/// The bytes a value's digest and size are taken over: a string as its
/// UTF-8, anything else as compact JSON with every object's keys sorted —
/// so the door, the renderer and a retrieval agree whatever map order a
/// parse produced.
pub fn value_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::String(s) => s.as_bytes().to_vec(),
        other => serde_json::to_vec(&sorted(other)).unwrap_or_default(),
    }
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sorted(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Lowercase hex sha256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A 64-character lowercase hex digest.
pub fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One classifiable value of a packet, as the job JSON holds it.
#[derive(Debug, Clone)]
pub struct Leaf<'a> {
    pub source: Source,
    pub pointer: String,
    pub value: &'a Value,
    /// Two completed steps answer to this slug, so the pointer cannot name
    /// one of them; such a value is never classified.
    pub ambiguous: bool,
}

fn job_of(job: &Value) -> &Value {
    job.get("data").filter(|d| d.is_object()).unwrap_or(job)
}

fn steps_of(job: &Value) -> Vec<&Value> {
    job_of(job)
        .get("steps")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn step_slug(step: &Value) -> Option<&str> {
    text(step, "spec_slug").or_else(|| text(step, "id"))
}

fn completed(step: &Value) -> bool {
    text(step, "status") == Some("completed")
}

/// Every value the packet section of a START prints: each key of the
/// job's metadata, then each key of each completed step's, in the job's
/// step order, keys sorted.
///
/// The classification record is included too: the contract lists that
/// whole reserved value as a mandatory original, unless the Workflow
/// requires it inline. Operators cannot weaken its read obligation.
pub fn leaves(job: &Value) -> Vec<Leaf<'_>> {
    let mut out = Vec::new();
    if let Some(md) = job_of(job).get("metadata").and_then(Value::as_object) {
        let mut keys: Vec<&String> = md.keys().collect();
        keys.sort();
        for key in keys {
            let source = Source::Job { key: key.clone() };
            out.push(Leaf {
                pointer: source.pointer(),
                source,
                value: &md[key],
                ambiguous: false,
            });
        }
    }
    let steps: Vec<&Value> = steps_of(job).into_iter().filter(|s| completed(s)).collect();
    for step in &steps {
        let Some(slug) = step_slug(step) else {
            continue;
        };
        let twins = steps.iter().filter(|s| step_slug(s) == Some(slug)).count();
        let Some(md) = step.get("metadata").and_then(Value::as_object) else {
            continue;
        };
        let mut keys: Vec<&String> = md.keys().collect();
        keys.sort();
        for key in keys {
            let source = Source::Step {
                slug: slug.to_string(),
                key: key.clone(),
            };
            out.push(Leaf {
                pointer: source.pointer(),
                source,
                value: &md[key],
                ambiguous: twins > 1,
            });
        }
    }
    out
}

/// The value a pointer names in this job JSON, if exactly one does.
pub fn resolve<'a>(job: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut found = leaves(job).into_iter().filter(|l| l.pointer == pointer);
    let leaf = found.next()?;
    if leaf.ambiguous || found.next().is_some() {
        return None;
    }
    Some(leaf.value)
}

/// Why a value can never be classified below REQUIRED, if it cannot.
pub fn protected(job: &Value, source: &Source) -> Option<String> {
    match source {
        Source::Job { key } => {
            if PROTECTED_JOB_KEYS.contains(&key.as_str()) {
                return Some(format!(
                    "`{key}` is a reserved record or the item's own scope, which every \
                     executor reads"
                ));
            }
            key.starts_with(DECISION_PREFIX)
                .then(|| format!("`{key}` records a decision on this item"))
        }
        Source::Step { slug, key } => {
            let steps = steps_of(job);
            let ids: Vec<&str> = steps
                .iter()
                .filter(|s| step_slug(s) == Some(slug.as_str()))
                .filter_map(|s| text(s, "id"))
                .collect();
            let corrected =
                crate::corrections::list(job_of(job).get("metadata").unwrap_or(&Value::Null))
                    .iter()
                    .any(|c| {
                        text(c, "field") == Some(key.as_str())
                            && text(c, "step").is_some_and(|s| ids.contains(&s))
                    });
            if corrected {
                return Some(format!(
                    "a correction names `{key}` on step `{slug}`, and a correction is read \
                     beside what it corrects"
                ));
            }
            let read_by_now = steps
                .iter()
                .filter(|s| matches!(text(s, "status"), Some("ready" | "active")))
                .flat_map(|s| {
                    s.get("blocked_by")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                        .unwrap_or_default()
                })
                .any(|upstream| ids.contains(&upstream));
            read_by_now.then(|| {
                format!(
                    "the current step's predicate reads step `{slug}`, so what it recorded may \
                     decide the current work"
                )
            })
        }
    }
}

/// A Workflow row's declaration of [`CONTRACT`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub contract: String,
    #[serde(default)]
    pub selectors: Vec<Selector>,
}

/// One value the Workflow classifies, by exact pointer.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub pointer: String,
    pub class: Class,
    pub purpose: String,
}

impl Declaration {
    /// The declaration a Workflow row carries: `Ok(None)` when it declares
    /// none (the legacy whole-START contract), and a refusal naming what is
    /// wrong when it declares one this reader cannot apply honestly — a
    /// malformed declaration never makes anything optional.
    pub fn of_row(row: &Value) -> Result<Option<Declaration>, String> {
        let Some(raw) = row.get("metadata").and_then(|m| m.get(DECLARATION_KEY)) else {
            return Ok(None);
        };
        let declared: Declaration = serde_json::from_value(raw.clone()).map_err(|e| {
            format!("the Workflow row's `{DECLARATION_KEY}` declaration is malformed: {e}")
        })?;
        if declared.contract != CONTRACT {
            return Err(format!(
                "the Workflow row declares execution-input contract `{}`, which this CLI does \
                 not implement (it implements `{CONTRACT}`)",
                declared.contract
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for s in &declared.selectors {
            if Source::parse(&s.pointer).is_none() {
                return Err(format!(
                    "selector `{}` is not a classifiable pointer — `/metadata/<key>` or \
                     `/steps/<slug>/metadata/<key>`",
                    s.pointer
                ));
            }
            if s.purpose.trim().is_empty() {
                return Err(format!("selector `{}` states no purpose", s.pointer));
            }
            if seen.contains(&s.pointer.as_str()) {
                return Err(format!(
                    "selector `{}` is declared twice, so its class is ambiguous",
                    s.pointer
                ));
            }
            seen.push(&s.pointer);
        }
        Ok(Some(declared))
    }

    /// The digest the run records beside the manifest: which declaration
    /// this START was rendered under.
    pub fn sha256(&self) -> String {
        sha256_hex(&value_bytes(
            &serde_json::to_value(self).unwrap_or(Value::Null),
        ))
    }
}

/// What a caller sends the classification door. Every key optional on the
/// wire so a missing one is refused by NAME.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClassificationRequest {
    pub pointer: Option<String>,
    pub sha256: Option<String>,
    pub class: Option<String>,
    pub purpose: Option<String>,
    pub reason: Option<String>,
}

/// Why the door refused, one sentence a caller can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Missing(&'static str),
    NotASource(String),
    Absent(String),
    Ambiguous(String),
    Protected(String),
    BadClass(String),
    ValueChanged { pointer: String, current: String },
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Refusal::Missing(key) => format!(
                "`{key}` is required and must not be blank — a classification names the value \
                 (pointer and sha256), its class, its purpose and the reason"
            ),
            Refusal::NotASource(p) => format!(
                "`{p}` is not a classifiable pointer: name one key of the packet's metadata \
                 (`/metadata/<key>`) or of a completed step's (`/steps/<slug>/metadata/<key>`)"
            ),
            Refusal::Absent(p) => format!(
                "this packet holds no value at `{p}` — only the packet's own metadata and a \
                 COMPLETED step's can be classified; the current step's specification is \
                 always printed"
            ),
            Refusal::Ambiguous(p) => format!(
                "two completed steps answer to the slug in `{p}`, so it cannot name one value"
            ),
            Refusal::Protected(why) => {
                format!("that value can never be classified below required: {why}")
            }
            Refusal::BadClass(c) => format!(
                "class `{c}` cannot be recorded: a classification is `mandatory_original` or \
                 `available_historical` (required is the default and needs no entry)"
            ),
            Refusal::ValueChanged { pointer, current } => format!(
                "the value at `{pointer}` does not hash to the sha256 sent (it hashes to \
                 {current} now) — a classification binds the exact value you read; read it \
                 again and classify what is there"
            ),
        }
    }
}

/// Judge a request against the job as it stands (the job GET's JSON) and
/// build the entry to append. `by` is the signing actor, `at` the write's
/// timestamp.
pub fn entry_for(
    job: &Value,
    req: &ClassificationRequest,
    by: &str,
    at: &str,
) -> Result<Value, Refusal> {
    let given = |v: &Option<String>, key: &'static str| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(Refusal::Missing(key))
    };
    let pointer = given(&req.pointer, "pointer")?;
    let sha256 = given(&req.sha256, "sha256")?;
    let class_word = given(&req.class, "class")?;
    let purpose = given(&req.purpose, "purpose")?;
    let reason = given(&req.reason, "reason")?;
    let source = Source::parse(&pointer).ok_or_else(|| Refusal::NotASource(pointer.clone()))?;
    let found: Vec<Leaf> = leaves(job)
        .into_iter()
        .filter(|l| l.pointer == pointer)
        .collect();
    let leaf = match found.as_slice() {
        [] => return Err(Refusal::Absent(pointer)),
        [one] if !one.ambiguous => one,
        _ => return Err(Refusal::Ambiguous(pointer)),
    };
    if let Some(why) = protected(job, &source) {
        return Err(Refusal::Protected(why));
    }
    let class = match Class::parse(&class_word) {
        Some(c @ (Class::MandatoryOriginal | Class::AvailableHistorical)) => c,
        _ => return Err(Refusal::BadClass(class_word)),
    };
    let bytes = value_bytes(leaf.value);
    let current = sha256_hex(&bytes);
    if current != sha256 {
        return Err(Refusal::ValueChanged { pointer, current });
    }
    Ok(json!({
        "job": text(job_of(job), "id"),
        "pointer": pointer,
        "sha256": sha256,
        "size_bytes": bytes.len(),
        "class": class.word(),
        "purpose": purpose,
        "reason": reason,
        "by": by,
        "at": at,
    }))
}

/// The `jobs.job.input_classified` payload, built once for both adapters.
pub fn classified_payload(job_id: &str, entry: &Value, index: usize) -> Value {
    json!({ "job_id": job_id, "index": index, "classification": entry })
}

/// `metadata` with `entry` appended to the classification list — the
/// in-memory adapter's append and the rule the Pg adapter's one UPDATE
/// states in SQL.
pub fn appended(metadata: &Value, entry: &Value) -> (Value, usize) {
    let mut md = metadata.as_object().cloned().unwrap_or_default();
    let mut items = md
        .get(CLASSIFICATIONS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items.push(entry.clone());
    let index = items.len() - 1;
    md.insert(CLASSIFICATIONS_KEY.to_string(), Value::Array(items));
    (Value::Object(md), index)
}

/// One value as the evaluation classes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Input {
    pub pointer: String,
    pub class: Class,
    pub size_bytes: u64,
    pub sha256: String,
    /// What the classifying source says the value is for.
    pub purpose: Option<String>,
    /// Which source decided the class, in one sentence.
    pub provenance: String,
}

/// Every value of a packet, classed; and every classification that no
/// longer applies, named.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Evaluation {
    pub inputs: Vec<Input>,
    pub unapplied: Vec<String>,
}

impl Evaluation {
    /// The values a START prints in full.
    pub fn required(&self) -> impl Iterator<Item = &Input> {
        self.inputs.iter().filter(|i| i.class == Class::Required)
    }

    /// The values a START lists instead of printing.
    pub fn listed(&self) -> impl Iterator<Item = &Input> {
        self.inputs.iter().filter(|i| i.class != Class::Required)
    }
}

/// Class every value of `job` under `declared`. Pure, total, and fail
/// closed: a value no source classifies is REQUIRED, and a classification
/// that does not bind THIS packet's CURRENT value is not applied and is
/// named in `unapplied`.
pub fn evaluate(job: &Value, declared: &Declaration) -> Evaluation {
    let job_id = text(job_of(job), "id");
    let entries = job_of(job)
        .get("metadata")
        .and_then(|m| m.get(CLASSIFICATIONS_KEY))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Evaluation::default();
    let mut used = vec![false; entries.len()];
    for leaf in leaves(job) {
        let bytes = value_bytes(leaf.value);
        let sha256 = sha256_hex(&bytes);
        let mut input = Input {
            pointer: leaf.pointer.clone(),
            class: Class::Required,
            size_bytes: bytes.len() as u64,
            sha256: sha256.clone(),
            purpose: None,
            provenance: "no source classifies it, so it is required".to_string(),
        };
        let classification_record =
            matches!(&leaf.source, Source::Job { key } if key == CLASSIFICATIONS_KEY);
        let fixed = if classification_record {
            None
        } else if leaf.ambiguous {
            Some("two completed steps answer to its slug, so no pointer names it".to_string())
        } else {
            protected(job, &leaf.source)
        };
        let mut claims: Vec<(Class, String, String)> = Vec::new();
        if classification_record {
            claims.push((
                Class::MandatoryOriginal,
                "the complete recorded classification history".to_string(),
                "execution-inputs/v1 requires this reserved record in full before START is acknowledged".to_string(),
            ));
        }
        if let Some(s) = declared
            .selectors
            .iter()
            .find(|s| s.pointer == leaf.pointer)
        {
            claims.push((
                s.class,
                s.purpose.clone(),
                "the pinned Workflow row's selector".to_string(),
            ));
        }
        for (i, e) in entries.iter().enumerate() {
            if text(e, "pointer") != Some(leaf.pointer.as_str()) {
                continue;
            }
            used[i] = true;
            let class = text(e, "class").and_then(Class::parse);
            let why_not = if classification_record {
                Some(
                    "the reserved classification record's read obligation is fixed by the contract"
                        .to_string(),
                )
            } else if text(e, "job") != job_id || job_id.is_none() {
                Some("it names another packet".to_string())
            } else if text(e, "sha256") != Some(sha256.as_str()) {
                Some(format!("the value changed since (sha256 now {sha256})"))
            } else if !matches!(
                class,
                Some(Class::MandatoryOriginal | Class::AvailableHistorical)
            ) {
                Some("its class is not one a classification records".to_string())
            } else {
                fixed.clone()
            };
            if let Some(why) = why_not {
                out.unapplied.push(format!(
                    "classification [{i}] of {} is not applied: {why}; the value is required",
                    leaf.pointer
                ));
                continue;
            }
            if let Some(class) = class {
                claims.push((
                    class,
                    text(e, "purpose").unwrap_or("").to_string(),
                    format!(
                        "classification [{i}] by {} at {}: {}",
                        text(e, "by").unwrap_or("?"),
                        text(e, "at").unwrap_or("?"),
                        text(e, "reason").unwrap_or("")
                    ),
                ));
            }
        }
        if let Some(why) = fixed {
            input.provenance = format!("never classified below required: {why}");
        } else if let Some((class, purpose, _)) = claims.iter().max_by_key(|c| c.0) {
            input.class = *class;
            input.purpose = Some(purpose.clone());
            // The strongest requirement wins, but that does not erase
            // another valid actor's recorded classification (9743e81f).
            input.provenance = claims
                .iter()
                .map(|c| c.2.as_str())
                .collect::<Vec<_>>()
                .join("; ");
        }
        out.inputs.push(input);
    }
    for (i, e) in entries.iter().enumerate() {
        if !used[i] {
            out.unapplied.push(format!(
                "classification [{i}] of {} is not applied: this packet holds no value there",
                text(e, "pointer").unwrap_or("?")
            ));
        }
    }
    out
}

/// A bounded run's building-step start expectation: the contract, the
/// digest and size of the exact START it was handed, and the digest of its
/// manifest of originals.
pub fn start_expectation(start: &str, manifest: &Value, minutes: i64) -> Value {
    json!({
        "schema": START_SCHEMA,
        "minutes": minutes,
        "contract": CONTRACT,
        "start_sha256": sha256_hex(start.as_bytes()),
        "start_bytes": start.len(),
        "manifest_sha256": sha256_hex(&value_bytes(manifest)),
    })
}

/// Whether `expected` is a well-formed [`START_SCHEMA`] expectation.
pub fn start_expectation_valid(expected: &Value) -> bool {
    expected.as_object().is_some_and(|m| m.len() == 6)
        && expected["schema"] == START_SCHEMA
        && expected["minutes"].as_i64().is_some_and(|n| n > 0)
        && expected["contract"] == CONTRACT
        && expected["start_bytes"]
            .as_u64()
            .is_some_and(|n| n > 0 && n <= START_BYTES as u64)
        && ["start_sha256", "manifest_sha256"]
            .iter()
            .all(|k| expected[*k].as_str().is_some_and(is_digest))
}

/// The receipt the worker's own `--started` records for a bounded run:
/// that it read this exact START and every mandatory original of this
/// exact manifest. It claims nothing about available originals.
pub fn start_receipt(expected: &Value, run: &str, actor: &str) -> Option<Value> {
    start_expectation_valid(expected).then(|| {
        json!({
            "schema": START_SCHEMA,
            "run": run,
            "actor": actor,
            "contract": CONTRACT,
            "start_sha256": expected["start_sha256"],
            "manifest_sha256": expected["manifest_sha256"],
        })
    })
}

/// Whether `receipt` is exactly the receipt `expected` asks of `actor`.
pub fn start_receipt_matches(expected: &Value, receipt: &Value, run: &str, actor: &str) -> bool {
    !actor.is_empty() && start_receipt(expected, run, actor).as_ref() == Some(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = "39d0b528-ff69-4cb8-ba82-408b641da66c";

    fn packet() -> Value {
        json!({
            "id": JOB,
            "metadata": {
                "description": "the scope",
                "decided_2026-10-15_e72dc7c9": "a decision",
                "old_observation": {"verdict": "green", "receipt": "x".repeat(4000)},
                "note/with~slash": "small",
            },
            "steps": [
                {"id": "s-triage", "spec_slug": "triage", "status": "completed",
                 "metadata": {"evidence": "measured", "procedure": "how to triage"}},
                {"id": "s-design", "spec_slug": "design-review", "status": "completed",
                 "metadata": {"verdict": "approved"}},
                {"id": "s-build", "spec_slug": "build", "status": "active",
                 "blocked_by": ["s-design"], "metadata": {"procedure": "build it"}},
            ]
        })
    }

    fn declared(selectors: Value) -> Declaration {
        Declaration::of_row(&json!({"metadata": {"execution_inputs": {
            "contract": CONTRACT, "selectors": selectors }}}))
        .unwrap()
        .unwrap()
    }

    fn classify(job: &mut Value, pointer: &str, class: &str) -> Result<Value, Refusal> {
        let value = resolve(job, pointer).cloned().unwrap_or(Value::Null);
        let req = ClassificationRequest {
            pointer: Some(pointer.into()),
            sha256: Some(sha256_hex(&value_bytes(&value))),
            class: Some(class.into()),
            purpose: Some("an Oct 11 observation".into()),
            reason: Some("superseded by the current repair".into()),
        };
        let entry = entry_for(job, &req, "emp-david", "2026-10-18T00:00:00Z")?;
        let (md, _) = appended(&job["metadata"], &entry);
        job["metadata"] = md;
        Ok(entry)
    }

    fn class_of(e: &Evaluation, pointer: &str) -> Class {
        e.inputs
            .iter()
            .find(|i| i.pointer == pointer)
            .unwrap()
            .class
    }

    #[test]
    fn pointers_round_trip_with_escapes_and_refuse_every_other_shape() {
        for s in [
            Source::Job {
                key: "note/with~slash".into(),
            },
            Source::Step {
                slug: "triage".into(),
                key: "evidence".into(),
            },
        ] {
            assert_eq!(Source::parse(&s.pointer()), Some(s));
        }
        assert_eq!(
            Source::Job {
                key: "a/b~c".into()
            }
            .pointer(),
            "/metadata/a~1b~0c"
        );
        for bad in [
            "",
            "metadata/x",
            "/metadata",
            "/metadata/",
            "/metadata/x/deeper",
            "/title",
            "/steps/triage/evidence",
            "/metadata/bad~2escape",
        ] {
            assert_eq!(Source::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn with_no_classification_every_value_is_required() {
        let e = evaluate(&packet(), &declared(json!([])));
        assert!(e.inputs.iter().all(|i| i.class == Class::Required), "{e:?}");
        assert_eq!(
            e.inputs.len(),
            7,
            "four job keys and three completed-step keys"
        );
        assert!(e.unapplied.is_empty());
    }

    #[test]
    fn a_classified_value_is_listed_and_a_changed_one_returns_to_required() {
        let mut job = packet();
        classify(
            &mut job,
            "/metadata/old_observation",
            "available_historical",
        )
        .unwrap();
        let e = evaluate(&job, &declared(json!([])));
        assert_eq!(
            class_of(&e, "/metadata/old_observation"),
            Class::AvailableHistorical
        );
        assert_eq!(
            e.listed().map(|i| i.pointer.as_str()).collect::<Vec<_>>(),
            vec![
                "/metadata/input_classifications",
                "/metadata/old_observation"
            ],
            "the classified value and its complete mandatory audit record"
        );

        job["metadata"]["old_observation"]["verdict"] = json!("red");
        let e = evaluate(&job, &declared(json!([])));
        assert_eq!(class_of(&e, "/metadata/old_observation"), Class::Required);
        assert!(
            e.unapplied[0].contains("the value changed since"),
            "{:?}",
            e.unapplied
        );
    }

    #[test]
    fn a_classification_copied_to_another_packet_is_not_applied() {
        let mut job = packet();
        classify(
            &mut job,
            "/metadata/old_observation",
            "available_historical",
        )
        .unwrap();
        job["id"] = json!("00000000-0000-4000-8000-000000000002");
        let e = evaluate(&job, &declared(json!([])));
        assert_eq!(class_of(&e, "/metadata/old_observation"), Class::Required);
        assert!(
            e.unapplied[0].contains("another packet"),
            "{:?}",
            e.unapplied
        );
    }

    #[test]
    fn protected_material_is_refused_at_the_door_and_never_demoted_by_a_selector() {
        let mut job = packet();
        for pointer in [
            "/metadata/description",
            "/metadata/decided_2026-10-15_e72dc7c9",
            "/steps/design-review/metadata/verdict",
        ] {
            assert!(
                matches!(
                    classify(&mut job, pointer, "available_historical"),
                    Err(Refusal::Protected(_))
                ),
                "{pointer}"
            );
        }
        let e = evaluate(
            &job,
            &declared(json!([
                {"pointer": "/metadata/description", "class": "available_historical", "purpose": "p"},
                {"pointer": "/steps/design-review/metadata/verdict", "class": "available_historical", "purpose": "p"},
            ])),
        );
        assert_eq!(class_of(&e, "/metadata/description"), Class::Required);
        assert_eq!(
            class_of(&e, "/steps/design-review/metadata/verdict"),
            Class::Required
        );
    }

    #[test]
    fn a_new_correction_withdraws_a_classification_of_the_field_it_names() {
        let mut job = packet();
        classify(
            &mut job,
            "/steps/triage/metadata/evidence",
            "available_historical",
        )
        .unwrap();
        assert_eq!(
            class_of(
                &evaluate(&job, &declared(json!([]))),
                "/steps/triage/metadata/evidence"
            ),
            Class::AvailableHistorical
        );
        job["metadata"]["corrections"] =
            json!([{"step": "s-triage", "field": "evidence", "reads": "measured"}]);
        let e = evaluate(&job, &declared(json!([])));
        assert_eq!(
            class_of(&e, "/steps/triage/metadata/evidence"),
            Class::Required
        );
        assert!(
            e.unapplied.iter().any(|u| u.contains("correction")),
            "{e:?}"
        );
    }

    #[test]
    fn the_strongest_class_wins_so_no_source_weakens_another() {
        let mut job = packet();
        classify(
            &mut job,
            "/steps/triage/metadata/procedure",
            "available_historical",
        )
        .unwrap();
        let e = evaluate(
            &job,
            &declared(json!([{"pointer": "/steps/triage/metadata/procedure",
                "class": "mandatory_original", "purpose": "how the item was measured"}])),
        );
        assert_eq!(
            class_of(&e, "/steps/triage/metadata/procedure"),
            Class::MandatoryOriginal
        );
        let e = evaluate(
            &job,
            &declared(json!([{"pointer": "/steps/triage/metadata/procedure",
                "class": "required", "purpose": "the workflow keeps it"}])),
        );
        assert_eq!(
            class_of(&e, "/steps/triage/metadata/procedure"),
            Class::Required
        );
    }

    #[test]
    fn every_applied_classification_keeps_its_provenance_when_another_source_wins() {
        let mut job = packet();
        classify(
            &mut job,
            "/metadata/old_observation",
            "available_historical",
        )
        .unwrap();
        classify(&mut job, "/metadata/old_observation", "mandatory_original").unwrap();
        for selectors in [
            json!([]),
            json!([{"pointer": "/metadata/old_observation",
            "class": "required", "purpose": "the current workflow requires it"}]),
        ] {
            let e = evaluate(&job, &declared(selectors.clone()));
            let input = e
                .inputs
                .iter()
                .find(|i| i.pointer == "/metadata/old_observation")
                .unwrap();
            assert!(
                input.provenance.contains("classification [0] by emp-david"),
                "{}",
                input.provenance
            );
            assert!(
                input.provenance.contains("classification [1] by emp-david"),
                "{}",
                input.provenance
            );
            if selectors.as_array().unwrap().is_empty() {
                assert_eq!(input.class, Class::MandatoryOriginal);
            } else {
                assert_eq!(input.class, Class::Required);
                assert!(input.provenance.contains("pinned Workflow row"));
            }
        }
    }

    #[test]
    fn the_classification_record_cannot_be_made_optional() {
        let mut job = packet();
        classify(
            &mut job,
            "/metadata/old_observation",
            "available_historical",
        )
        .unwrap();
        for selectors in [
            json!([]),
            json!([{"pointer": "/metadata/input_classifications",
            "class": "available_historical", "purpose": "attempted demotion"}]),
        ] {
            let e = evaluate(&job, &declared(selectors));
            assert_eq!(
                class_of(&e, "/metadata/input_classifications"),
                Class::MandatoryOriginal
            );
        }
        let e = evaluate(
            &job,
            &declared(json!([{"pointer": "/metadata/input_classifications",
            "class": "required", "purpose": "stronger inline obligation"}])),
        );
        assert_eq!(
            class_of(&e, "/metadata/input_classifications"),
            Class::Required
        );
        assert!(
            classify(
                &mut job,
                "/metadata/input_classifications",
                "available_historical"
            )
            .is_err()
        );
    }

    #[test]
    fn the_door_refuses_what_would_make_it_a_guess() {
        let mut job = packet();
        let base = ClassificationRequest {
            pointer: Some("/metadata/old_observation".into()),
            sha256: Some("0".repeat(64)),
            class: Some("available_historical".into()),
            purpose: Some("p".into()),
            reason: Some("r".into()),
        };
        assert!(matches!(
            entry_for(&job, &base, "a", "t"),
            Err(Refusal::ValueChanged { .. })
        ));
        for (edit, want) in [
            ("pointer", "/title"),
            ("pointer", "/metadata/absent"),
            ("pointer", "/steps/build/metadata/procedure"),
            ("class", "required"),
            ("reason", " "),
        ] {
            let mut req = base.clone();
            let v = Some(want.to_string());
            match edit {
                "pointer" => req.pointer = v,
                "class" => req.class = v,
                _ => req.reason = v,
            }
            assert!(entry_for(&job, &req, "a", "t").is_err(), "{edit}={want}");
        }
        job["steps"][1]["spec_slug"] = json!("triage");
        assert!(matches!(
            classify(
                &mut job,
                "/steps/triage/metadata/evidence",
                "available_historical"
            ),
            Err(Refusal::Ambiguous(_))
        ));
    }

    #[test]
    fn a_malformed_or_unknown_declaration_refuses_rather_than_making_anything_optional() {
        assert_eq!(Declaration::of_row(&json!({"metadata": {}})), Ok(None));
        for raw in [
            json!({"contract": "execution-inputs/v9"}),
            json!({"contract": CONTRACT, "extra": 1}),
            json!({"contract": CONTRACT, "selectors": [{"pointer": "/title", "class": "required", "purpose": "p"}]}),
            json!({"contract": CONTRACT, "selectors": [{"pointer": "/metadata/a", "class": "optional", "purpose": "p"}]}),
            json!({"contract": CONTRACT, "selectors": [{"pointer": "/metadata/a", "class": "required", "purpose": " "}]}),
            json!({"contract": CONTRACT, "selectors": [
                {"pointer": "/metadata/a", "class": "required", "purpose": "p"},
                {"pointer": "/metadata/a", "class": "available_historical", "purpose": "p"}]}),
        ] {
            assert!(
                Declaration::of_row(&json!({"metadata": {"execution_inputs": raw}})).is_err(),
                "{raw}"
            );
        }
    }

    #[test]
    fn value_digests_ignore_map_order_and_keep_strings_as_their_bytes() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":"é\r\n"}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":"é\r\n","d":2},"b":1}"#).unwrap();
        assert_eq!(value_bytes(&a), value_bytes(&b));
        assert_eq!(value_bytes(&json!("é\r\n")), "é\r\n".as_bytes());
    }

    #[test]
    fn a_bounded_receipt_binds_the_exact_start_and_manifest_and_nothing_else() {
        let manifest = json!([{"n": 1}]);
        let e = start_expectation("the start", &manifest, 15);
        assert!(start_expectation_valid(&e), "{e}");
        let r = start_receipt(&e, "run", "agent-claude-b").unwrap();
        assert!(start_receipt_matches(&e, &r, "run", "agent-claude-b"));
        assert!(!start_receipt_matches(&e, &r, "run", "other"));
        assert!(!start_receipt_matches(
            &e,
            &r,
            "other-run",
            "agent-claude-b"
        ));
        let legacy = json!({"schema": 1, "run": "run", "actor": "agent-claude-b"});
        assert!(!start_receipt_matches(&e, &legacy, "run", "agent-claude-b"));
        let mut big = e.clone();
        big["start_bytes"] = json!(START_BYTES + 1);
        assert!(!start_expectation_valid(&big));
    }
}
