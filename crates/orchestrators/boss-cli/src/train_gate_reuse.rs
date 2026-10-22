//! A ONE-CAR TRAIN MAY CARRY ITS CAR'S GREEN — when nothing differs.
//!
//! Design cdf626ac Q1 (David, 2026-10-15), backlog bbc13a4e. A train
//! whose assembled tree is byte-identical to the tree its one car was
//! gated on, under the same runner, image, toolchain and gate inputs,
//! would re-run a deterministic test on identical input: a repeat, not a
//! check. Measured for the design (M5, trains opened 2026-10-01..15): 48
//! of 216 comparable single-car trains (22.2%) were tree-identical, 48/48
//! green, 1227 of 9936 train-gate minutes (12.4%). Tree equality is
//! NECESSARY there, never shown SUFFICIENT: the complete environment is
//! what this module judges, and single-car count alone is never evidence.
//!
//! WHAT A QUALIFIED REUSE DOES. Nothing is copied into a new record and
//! nothing is retyped: the train's `train_gate_run` is bound to the car's
//! ORIGINAL gate-run, so every reader of the train gate — the first
//! judgement (`train_gate::combined_verdict`), the judged re-read
//! (`judged_verdict`), the merge's second lock, the red-train alert's
//! `fails`, the relaunch arm — reads the one packet the runner's carrier
//! recorded, and no reader can disagree with another about which verdict
//! is in force. The binding is written once, beside an assessment that
//! names the source gate, head, tree, runner and images
//! ([`KEY_REUSE`]). The car's gate-run is never written.
//!
//! WHAT IT NEVER DOES. It does not touch forge CI (the image build, the
//! locomotive, the web job and the delivery plan still judge the train),
//! the consist check's live-registry lints at boarding, the merge or the
//! converge. It does not retire the train gate (that is Q2's queue-owned
//! gate, its own design). It does not accept a scoped or `--auto` car
//! green: only a FULL car receipt covers what the train's `--auto` gate
//! would select, and selection is not licensed on car gates (Q2).
//!
//! FAIL-CLOSED, IN THREE LAYERS. (1) The mode is OFF unless the
//! conductor's environment says otherwise ([`MODE_ENV`]); OFF reads
//! nothing and writes nothing, so a conductor running this code behaves
//! exactly as before. (2) Every input is an [`Observed`]: an input the
//! record cannot supply is Unknown, and Unknown refuses exactly as
//! unequal does — no evidence is not a pass. (3) [`UNJUDGED_INPUTS`]
//! names the contract inputs no comparison exists for yet; while it is
//! non-empty nothing qualifies, whatever the mode. The car that removes
//! an entry is the car that adds its comparison.
//!
//! THE ENVIRONMENT IS FOUR COMPARISONS: the runner's sha256, each
//! container's resolved image digest, the tree (which fixes the
//! toolchain file), and the whole launch request — env, the handed edit
//! level, resources, sidecars, volumes, deadlines — read back as the
//! bytes the car gate's immutable launch intent names and set beside the
//! request the train gate would submit ([`launch_refusals`]). Only the
//! packet, branch, head and selection mode may differ between the two.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The conductor's switch. Absent, empty or `off`: the code is inert.
/// `shadow`: assess every train and stamp the assessment, but always file
/// the ordinary train gate — the measurement before activation. The one
/// spelling that binds is [`BIND_WORD`]; anything else is read as off,
/// and said so, rather than guessed at.
pub(crate) const MODE_ENV: &str = "BOSS_TRAIN_GATE_REUSE";
pub(crate) const SHADOW_WORD: &str = "shadow";
pub(crate) const BIND_WORD: &str = "exact-full";

/// The train metadata key that carries the assessment (and, when bound,
/// the provenance of the receipt the train reads).
pub(crate) const KEY_REUSE: &str = "train_gate_reuse";

/// Contract inputs this module cannot compare yet. While this is
/// non-empty NOTHING qualifies, in any mode — `shadow` still records
/// what else would have refused, which is the measurement the activation
/// owes. Each entry says what would judge it.
///
/// EMPTY SINCE THE LAUNCH REQUEST IS COMPARED (backlog bbc13a4e, the
/// second car). Its one entry was the car gate's launch manifest — env,
/// resources, sidecars, the handed edit level — and [`launch_refusals`]
/// now judges it against the request the train gate would submit. An
/// empty list is not an open door: the shipped manifest's tag-pinned
/// images and ConfigMap runner are Unknown on the train side, and the
/// mode is off unless the conductor's environment says otherwise.
pub(crate) const UNJUDGED_INPUTS: &[&str] = &[];

/// The values that differ between ANY two launches by design: which
/// packet, which branch, which head, which selection mode. The gate
/// executor's env carries all four; the Job's name, its generated name
/// and two labels carry the packet and the branch. Nothing else in a
/// launch request may differ between the car's gate and the train's.
const PER_RUN_ENV: [&str; 4] = ["GATE_BRANCH", "GATE_RUN_JOB_ID", "GATE_MODE", "GATE_SHA"];
const PER_RUN_LABELS: [&str; 2] = ["boss.dev/packet", "boss.dev/branch"];
/// Server- and renderer-assigned identity, never an input to the run.
const SERVER_IDENTITY: [&str; 6] = [
    "name",
    "generateName",
    "creationTimestamp",
    "uid",
    "resourceVersion",
    "managedFields",
];

/// The pod spec's container lists, both of which a gate request uses
/// (the judged-init layout runs `gate` as an init container).
fn containers(doc: &Value) -> impl Iterator<Item = &Value> {
    ["initContainers", "containers"]
        .into_iter()
        .filter_map(|list| doc.pointer(&format!("/spec/template/spec/{list}")))
        .filter_map(Value::as_array)
        .flatten()
}

/// The values a request binds to `name` across every container's env.
fn env_values<'a>(doc: &'a Value, name: &str) -> Vec<&'a str> {
    containers(doc)
        .filter_map(|c| c.get("env").and_then(Value::as_array))
        .flatten()
        .filter(|e| e.get("name").and_then(Value::as_str) == Some(name))
        .map(|e| e.get("value").and_then(Value::as_str).unwrap_or_default())
        .collect()
}

/// PURE: a launch request with its per-run values and server identity
/// taken out — what is left is the environment the run is given.
fn environment_of(doc: &Value) -> Result<Value, String> {
    if doc.get("kind").and_then(Value::as_str) != Some("Job") {
        return Err("is not one Job".into());
    }
    let mut v = doc.clone();
    let md = v
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .ok_or("has no metadata")?;
    for key in SERVER_IDENTITY {
        md.remove(key);
    }
    if let Some(labels) = md.get_mut("labels").and_then(Value::as_object_mut) {
        for label in PER_RUN_LABELS {
            labels.remove(label);
        }
    }
    if let Some(tmd) = v
        .pointer_mut("/spec/template/metadata")
        .and_then(Value::as_object_mut)
    {
        tmd.remove("creationTimestamp");
    }
    for list in ["initContainers", "containers"] {
        let Some(cs) = v
            .pointer_mut(&format!("/spec/template/spec/{list}"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for env in cs
            .iter_mut()
            .filter_map(|c| c.get_mut("env").and_then(Value::as_array_mut))
        {
            env.retain(|e| {
                !e.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| PER_RUN_ENV.contains(&n))
            });
        }
    }
    Ok(v)
}

/// The JSON pointers at which two values differ, in a fixed order.
fn differing(a: &Value, b: &Value, at: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                let p = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                match (x.get(k), y.get(k)) {
                    (Some(m), Some(n)) => differing(m, n, &p, out),
                    _ => out.push(p),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (n, (m, o)) in x.iter().zip(y).enumerate() {
                differing(m, o, &format!("{at}/{n}"), out);
            }
        }
        _ if a == b => {}
        _ => out.push(if at.is_empty() { "/".into() } else { at.into() }),
    }
}

/// PURE: why the train gate's would-be launch request is not the car
/// gate's retained one, or nothing when they give the run the same
/// environment.
///
/// THE CAR'S SIDE IS THE RETAINED REQUEST ITSELF: the bytes must hash to
/// the sha256 the gate-run's immutable `gate_launch_intent` names, so no
/// re-rendering or description of it stands in; and the request must bind
/// the boarded head and the full (empty) runner argument, so it is the
/// launch the receipt vouches for. The train's side is the request the
/// conductor's own native path renders and does not send. Both are then
/// compared whole after [`environment_of`]: images per container,
/// commands, env (the handed edit level among it), resources, volumes,
/// sidecars, deadlines, labels. A difference names where.
pub(crate) fn launch_refusals(
    car: &Observed,
    train: &Observed,
    intent_sha256: Option<&str>,
    head: &str,
) -> Vec<String> {
    let (car, train) = match (car, train) {
        (Observed::Unknown(why), _) => {
            return vec![format!("the car gate's launch request is unknown: {why}")];
        }
        (_, Observed::Unknown(why)) => {
            return vec![format!("the train gate's launch request is unknown: {why}")];
        }
        (Observed::Known(c), Observed::Known(t)) => (c, t),
    };
    let mut out = Vec::new();
    if intent_sha256 != Some(sha256_hex(car.as_bytes()).as_str()) {
        out.push(format!(
            "the car gate's launch request is not the bytes the launch intent names \
             (sha256 {})",
            intent_sha256.unwrap_or("absent")
        ));
    }
    let parsed = |side: &str, text: &str| {
        serde_json::from_str::<Value>(text)
            .map_err(|e| format!("the {side} gate's launch request is unreadable: {e}"))
            .and_then(|doc| {
                environment_of(&doc)
                    .map(|env| (doc, env))
                    .map_err(|why| format!("the {side} gate's launch request {why}"))
            })
    };
    let (car_doc, car_env) = match parsed("car", car) {
        Ok(p) => p,
        Err(why) => {
            out.push(why);
            return out;
        }
    };
    if env_values(&car_doc, "GATE_SHA") != [head] {
        out.push(format!(
            "the car gate's launch request binds GATE_SHA {:?}, not the boarded head {head}",
            env_values(&car_doc, "GATE_SHA")
        ));
    }
    if env_values(&car_doc, "GATE_MODE") != [""] {
        out.push(format!(
            "the car gate's launch request binds GATE_MODE {:?}, not a full run",
            env_values(&car_doc, "GATE_MODE")
        ));
    }
    let train_env = match parsed("train", train) {
        Ok((_, env)) => env,
        Err(why) => {
            out.push(why);
            return out;
        }
    };
    let mut paths = Vec::new();
    differing(&train_env, &car_env, "", &mut paths);
    if !paths.is_empty() {
        const SHOWN: usize = 6;
        out.push(format!(
            "the launch request differs from the car gate's at {} place(s): {}{}",
            paths.len(),
            paths[..paths.len().min(SHOWN)].join(", "),
            if paths.len() > SHOWN { ", ..." } else { "" }
        ));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Shadow,
    Bind,
}

impl Mode {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Shadow => SHADOW_WORD,
            Mode::Bind => BIND_WORD,
        }
    }
}

/// PURE: the mode an environment value selects, and a line to log when
/// the value was not one this code knows (read as off, never guessed).
pub(crate) fn mode_from(raw: Option<&str>) -> (Mode, Option<String>) {
    match raw.map(str::trim) {
        None | Some("") | Some("off") => (Mode::Off, None),
        Some(SHADOW_WORD) => (Mode::Shadow, None),
        Some(BIND_WORD) => (Mode::Bind, None),
        Some(other) => (
            Mode::Off,
            Some(format!(
                "{MODE_ENV}={other:?} is not a reuse mode (off | {SHADOW_WORD} | {BIND_WORD}); \
                 reading it as off"
            )),
        ),
    }
}

/// One input as the record holds it: a value, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Observed {
    Known(String),
    Unknown(String),
}

impl Observed {
    fn known(&self) -> Option<&str> {
        match self {
            Observed::Known(v) => Some(v),
            Observed::Unknown(_) => None,
        }
    }
}

/// What the conductor observes from its clone and its manifest, as
/// opposed to what it reads off the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observations {
    pub train_head: Observed,
    pub train_tree: Observed,
    pub car_tree: Observed,
    pub train_runner: Observed,
    pub train_images: Observed,
    /// The car gate's retained launch request: the exact bytes its
    /// `gate_launch_intent` names, read back from the file store.
    pub car_launch: Observed,
    /// The request a launch of the train gate would submit now, rendered
    /// by the same native path and never sent.
    pub train_launch: Observed,
}

/// Everything the assessment reads. The conductor gathers it; this
/// module judges it without I/O.
pub(crate) struct Inputs<'a> {
    pub mode: Mode,
    /// The train packet as read this pass.
    pub train: &'a Value,
    /// How many train-marked gate-runs the complete census found for this
    /// train, or why the census could not be completed.
    pub prior_attempts: Result<usize, String>,
    /// The one boarded car, or why it could not be read.
    pub car: Result<&'a Value, String>,
    /// The gate-run the car's receipt names, or why it could not be read.
    pub gate_run: Result<&'a Value, String>,
    /// The assembled train branch's full sha and tree (conductor clone).
    pub train_head: Observed,
    pub train_tree: Observed,
    /// The tree of the head the car's receipt vouches for.
    pub car_tree: Observed,
    /// sha256 of the run.sh the train gate would execute.
    pub train_runner: Observed,
    /// The image digests the train gate would run (sorted, comma-joined).
    pub train_images: Observed,
    /// The car gate's retained launch request, as text.
    pub car_launch: Observed,
    /// The train gate's would-be launch request, as text.
    pub train_launch: Observed,
    /// [`UNJUDGED_INPUTS`] in production.
    pub unjudged: &'a [&'a str],
}

/// The judgement, with the record it stamps on the train.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Assessment {
    pub mode: Mode,
    /// Empty exactly when the train may carry the car's receipt.
    pub refusals: Vec<String>,
    /// The car's gate-run, when one was identified.
    pub source_gate_run: Option<String>,
    pub record: Value,
}

impl Assessment {
    pub(crate) fn qualified(&self) -> bool {
        self.refusals.is_empty() && self.source_gate_run.is_some()
    }

    /// Bind only in bind mode and only when qualified.
    pub(crate) fn binds(&self) -> bool {
        self.mode == Mode::Bind && self.qualified()
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

fn text<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

/// PURE: the gate-run packet a carrier-recorded receipt names: the Job is
/// `gate-<packet uuid, simple form>` (gate/reservation.rs `job_name`), and
/// the conductor wrote that name into `recorded_by` from the cluster, not
/// from the receipt's own words.
pub(crate) fn gate_run_named_by(receipt: &Value) -> Option<String> {
    if text(receipt, "/recorded_by/carrier") != Some(crate::train::CARRIER_POD_LOG) {
        return None;
    }
    let job = text(receipt, "/recorded_by/job")?;
    let simple = job.strip_prefix("gate-")?;
    uuid::Uuid::try_parse(simple)
        .ok()
        .filter(|id| id.simple().to_string() == simple)
        .map(|id| id.to_string())
}

/// PURE: the resolved image digests a carrier-recorded receipt names
/// (`recorded_by.image_ids`, the kubelet's `imageID` per container),
/// sorted and comma-joined — or why there are none.
pub(crate) fn receipt_images(receipt: &Value) -> Observed {
    let Some(ids) = receipt
        .pointer("/recorded_by/image_ids")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    else {
        return Observed::Unknown(
            "the car gate's receipt records no resolved image digest (recorded_by.image_ids)"
                .into(),
        );
    };
    let mut digests = Vec::new();
    for (container, id) in ids {
        match id.as_str().and_then(digest_of) {
            Some(d) => digests.push(d),
            None => {
                return Observed::Unknown(format!(
                    "the car gate's container {container} names no sha256 image digest"
                ));
            }
        }
    }
    digests.sort();
    digests.dedup();
    Observed::Known(digests.join(","))
}

/// The `sha256:<64 hex>` part of an image reference or kubelet imageID.
fn digest_of(reference: &str) -> Option<String> {
    let (_, digest) = reference.rsplit_once('@')?;
    let hex = digest.strip_prefix("sha256:")?;
    (hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_hexdigit())).then(|| digest.to_string())
}

/// The `key: value` lines of a manifest whose key is `key`, comments and
/// quotes stripped. Line-based on purpose: the manifest is the
/// launcher's text and boss-cli parses no YAML.
fn manifest_values(job_doc: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    job_doc
        .lines()
        .map(str::trim)
        .map(|l| l.strip_prefix("- ").unwrap_or(l))
        .filter_map(|l| l.strip_prefix(prefix.as_str()))
        .map(|v| v.split(" #").next().unwrap_or_default().trim())
        .map(|v| v.trim_matches(|c| c == '"' || c == '\'').to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

/// PURE: the image digests the train gate's Job would run, read from the
/// conductor's own rendered manifest. Known only when EVERY image is
/// pinned by digest: a tag (`boss-ci:rust1.96`, pulled `Always`) resolves
/// only when a pod pulls it, so the image a train gate would run is not
/// known before it runs.
pub(crate) fn manifest_images(job_doc: &str) -> Observed {
    let images = manifest_values(job_doc, "image");
    if images.is_empty() {
        return Observed::Unknown("the train gate's runner manifest names no image".into());
    }
    let mut digests = Vec::new();
    for image in &images {
        match digest_of(image) {
            Some(d) => digests.push(d),
            None => {
                return Observed::Unknown(format!(
                    "the train gate's runner manifest names the mutable image {image}: no \
                     digest is resolved until a pod pulls it"
                ));
            }
        }
    }
    digests.sort();
    digests.dedup();
    Observed::Known(digests.join(","))
}

/// PURE: the sha256 of the run.sh the train gate would execute. The pod
/// runs what its gate container's command names under `/repo-script/`:
/// the bootstrap fetches the GATED SHA's own run.sh (design 1d7a1669), so
/// for the train that is `tree_run_sh` — the train tree's blob. The
/// ConfigMap's run.sh is rewritten from main by the forge converge and
/// the conductor's Role cannot read it, so that runner is Unknown.
pub(crate) fn manifest_runner(job_doc: &str, tree_run_sh: &Observed) -> Observed {
    let commands: Vec<String> = manifest_values(job_doc, "command")
        .into_iter()
        .filter(|c| c.contains("/repo-script/"))
        .collect();
    // Every script but the bootstrap is the ConfigMap's — the gate's
    // run.sh, and since the judged-init layout (design b4bf8bba) the
    // `seed-refresh` helper's script beside it, which maintains the seed
    // disk later gates start from. Any one of them is an input the
    // conductor cannot read.
    let from_configmap: Vec<&str> = commands
        .iter()
        .filter(|c| !c.contains("/repo-script/bootstrap.sh"))
        .filter_map(|c| c.split("/repo-script/").nth(1))
        .map(|s| s.split(['"', '\'', ' ', ',', ']']).next().unwrap_or(s))
        .collect();
    match commands.as_slice() {
        [] => Observed::Unknown("the train gate's runner manifest names no runner script".into()),
        _ if !from_configmap.is_empty() => Observed::Unknown(format!(
            "the train gate would run {} from ConfigMap gate-runner-script, which the \
             forge converge rewrites from main and the conductor's Role cannot read",
            from_configmap.join(" and ")
        )),
        [_bootstrap] => tree_run_sh.clone(),
        _ => Observed::Unknown(format!(
            "the train gate's runner manifest names {} runner commands; expected one",
            commands.len()
        )),
    }
}

/// PURE: sha256 of bytes, hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The receipt fields that say a green was partial, refused or taken on
/// something other than the committed tree. Each present-and-wrong value
/// refuses; `estate` and `refused_because` refuse by being present.
fn receipt_refusals(r: &Value, head: &str, out: &mut Vec<String>) {
    let s = |p: &str| text(r, p);
    if s("/verdict") != Some("green") {
        out.push(format!(
            "the car's receipt verdict is {:?}, not green",
            s("/verdict")
        ));
    }
    if s("/head") != Some(head) {
        out.push(format!(
            "the car's receipt vouches for {:?}, not the boarded head {head}",
            s("/head")
        ));
    }
    if s("/mode") != Some("full") {
        out.push(format!(
            "the car's gate ran mode {:?}: only a FULL car green covers what the train gate selects",
            s("/mode")
        ));
    }
    if !s("/scope").is_some_and(|v| v.trim().is_empty()) {
        out.push("the car's receipt names a scope: a selected run is not a full run".into());
    }
    if r.get("dirty") != Some(&Value::Bool(false)) {
        out.push("the car's receipt does not state a clean tree (dirty: false)".into());
    }
    if r.get("ci") != Some(&Value::Bool(false)) {
        out.push("the car's receipt does not state the cluster gate (ci: false)".into());
    }
    if r.get("estate").is_some() {
        out.push("the car's gate declared no estate: its live lints did not run".into());
    }
    if r.get("refused_because").is_some() {
        out.push("the car's receipt carries a refusal".into());
    }
    if r.get("runner_fallback_reason").is_some() {
        out.push("the car's gate ran a fallback runner".into());
    }
    let empty = |p: &str| {
        r.get(p)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    };
    if !empty("unverifiable") {
        out.push("the car's receipt lists unverifiable paths".into());
    }
    if !empty("fails") {
        out.push("the car's receipt lists failures".into());
    }
    let checks = r.get("checks").and_then(Value::as_array);
    match checks {
        Some(c) if !c.is_empty() => {
            let not_pass: Vec<&str> = c
                .iter()
                .filter(|k| k.get("result").and_then(Value::as_str) != Some("pass"))
                .filter_map(|k| k.get("name").and_then(Value::as_str))
                .collect();
            if !not_pass.is_empty() || c.iter().any(|k| k.get("name").is_none()) {
                out.push(format!(
                    "the car's receipt has checks that did not pass: {}",
                    not_pass.join(", ")
                ));
            }
        }
        _ => out.push("the car's receipt names no checks".into()),
    }
    if text(r, "/recorded_by/carrier") != Some(crate::train::CARRIER_POD_LOG)
        || r.pointer("/recorded_by/head_matches_launch") != Some(&Value::Bool(true))
        || text(r, "/recorded_by/launched_sha") != Some(head)
    {
        out.push(
            "the car's receipt was not recorded by the conductor's carrier for the head it launched"
                .into(),
        );
    }
}

/// PURE: the assessment. Refusals are listed in a fixed order so the
/// same inputs always stamp the same record (determinism; the stamp is
/// rewritten only when it changes).
pub(crate) fn assess(i: &Inputs<'_>, now: chrono::DateTime<chrono::Utc>) -> Assessment {
    use crate::train_gate as tg;
    let mut out: Vec<String> = Vec::new();
    let md = |k: &str| i.train.pointer(&format!("/metadata/{k}"));

    // THE TRAIN: one car, no gate of its own yet.
    let boarded: Vec<&str> = md("boarded_jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if boarded.len() != 1 {
        out.push(format!(
            "the train carries {} cars; only a one-car train can carry its car's receipt",
            boarded.len()
        ));
    }
    if md(tg::KEY_RUN).is_some_and(|v| !v.is_null()) {
        out.push("the train already names a gate-run".into());
    }
    if md(tg::KEY_RELAUNCHES).and_then(Value::as_u64).unwrap_or(0) > 0
        || md(tg::KEY_REFUSALS)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        out.push("the train's gate has been filed before; its original attempts rule".into());
    }
    match &i.prior_attempts {
        Ok(0) => {}
        Ok(n) => out.push(format!(
            "{n} original train gate(s) exist for this train; an original is never hidden"
        )),
        Err(why) => out.push(format!("the train gate census is incomplete: {why}")),
    }

    // THE CAR and the receipt it boarded on.
    let mut head = String::new();
    let mut receipt = Value::Null;
    let mut source: Option<String> = None;
    match &i.car {
        Err(why) => out.push(format!("the car could not be read: {why}")),
        Ok(car) => {
            if text(car, "/id") != boarded.first().copied() {
                out.push("the car read is not the train's boarded car".into());
            }
            head = text(car, "/metadata/boarded_head")
                .unwrap_or_default()
                .to_string();
            if !is_sha(&head) {
                out.push("the car records no full boarded head".into());
            }
            match crate::receipt::select_receipt(car) {
                None => out.push("the car carries no machine receipt".into()),
                Some(r) => {
                    receipt_refusals(&r, &head, &mut out);
                    source = gate_run_named_by(&r);
                    if source.is_none() {
                        out.push("the car's receipt names no carrier-recorded gate-run".into());
                    }
                    receipt = r;
                }
            }
        }
    }

    // THE ORIGINAL GATE-RUN: the receipt is a copy of its record.
    let mut intent_sha256: Option<String> = None;
    match &i.gate_run {
        Err(why) => out.push(format!("the car's gate-run could not be read: {why}")),
        Ok(run) => {
            let md = run.get("metadata").cloned().unwrap_or(Value::Null);
            if text(run, "/id").map(str::to_string) != source {
                out.push("the gate-run read is not the one the receipt names".into());
            }
            if boss_jobs::stranded::is_train_gate(&md)
                || boss_jobs::full_gate_on_main::is_main_gate(&md)
            {
                out.push("the named gate-run is not a car's gate".into());
            }
            // Read through the submission's immutable view where the packet
            // has one, as every other reader of a gate-run's source does.
            let launched = boss_jobs::gate_reservation::source_metadata(&md)
                .ok()
                .and_then(|m| text(m, "/sha"));
            if launched != Some(head.as_str()) {
                out.push("the gate-run was not launched on the boarded head".into());
            }
            if text(run, "/status") != Some("closed") || tg::standing(run) != tg::Standing::Green {
                out.push("the gate-run is not a closed green".into());
            }
            if tg::receipt(run).as_ref() != Some(&receipt) || receipt.is_null() {
                out.push("the car's receipt is not the gate-run's recorded receipt".into());
            }
            match boss_jobs::metadata_immutability::view(
                &md,
                boss_jobs::gate_reservation::INTENT,
                "",
                1,
                &[],
            ) {
                Ok(intent)
                    if md.get(boss_jobs::gate_reservation::INTENT).is_some()
                        && text(intent, "/mode") == Some(crate::gate::RECORDED_FULL_MODE)
                        && text(intent, "/head") == Some(head.as_str()) =>
                {
                    intent_sha256 = text(intent, "/sha256").map(str::to_string);
                }
                Ok(_) => out.push(
                    "the gate-run's immutable launch intent is absent or not a full launch of the boarded head"
                        .into(),
                ),
                Err(why) => out.push(format!("the gate-run's launch intent is unreadable: {why}")),
            }
        }
    }

    // THE TREE: the assembled train against the car's gated head.
    let pair = |what: &str, a: &Observed, b: &Observed, out: &mut Vec<String>| match (a, b) {
        (Observed::Unknown(why), _) | (_, Observed::Unknown(why)) => {
            out.push(format!("{what} unknown: {why}"))
        }
        (Observed::Known(x), Observed::Known(y)) if x != y => {
            out.push(format!("{what} differs: the train's {x}, the car's {y}"))
        }
        _ => {}
    };
    let short = crate::train::train_ref_of(i.train)
        .and_then(|r| r.split_once('@'))
        .map(|(_, s)| s.to_string())
        .unwrap_or_default();
    match i.train_head.known() {
        Some(h) if is_sha(h) && !short.is_empty() && h.starts_with(&short) => {}
        Some(h) => out.push(format!(
            "the assembled head {h} is not the one the assemble step recorded ({short})"
        )),
        None => pair("assembled head", &i.train_head, &i.train_head, &mut out),
    }
    pair("tree", &i.train_tree, &i.car_tree, &mut out);

    // THE ENVIRONMENT: runner, images; the toolchain follows from both.
    let car_runner = match text(&receipt, "/runner_sha256") {
        Some(s) if s.len() == 64 => Observed::Known(s.to_string()),
        _ => Observed::Unknown("the car's receipt names no runner sha256".into()),
    };
    pair("runner", &i.train_runner, &car_runner, &mut out);
    pair(
        "image",
        &i.train_images,
        &receipt_images(&receipt),
        &mut out,
    );
    // THE LAUNCH: everything else the run was given.
    out.extend(launch_refusals(
        &i.car_launch,
        &i.train_launch,
        intent_sha256.as_deref(),
        &head,
    ));
    out.extend(i.unjudged.iter().map(|s| s.to_string()));

    let record = json!({
        "contract_version": 1,
        "mode": i.mode.word(),
        "decision": if out.is_empty() && source.is_some() {
            if i.mode == Mode::Bind { "reused" } else { "would reuse" }
        } else {
            "regate"
        },
        "refusals": out,
        "source": {
            "gate_run": source,
            "car": boarded.first(),
            "head": (!head.is_empty()).then_some(head.as_str()),
            "receipt_sha256": text(&receipt, "/recorded_by/sha256"),
            "job": text(&receipt, "/recorded_by/job"),
            "job_uid": text(&receipt, "/recorded_by/job_uid"),
            "tree": i.car_tree.known(),
            "runner_sha256": car_runner.known(),
            "image_digests": receipt_images(&receipt).known(),
            "launch_request_sha256": intent_sha256,
        },
        "train": {
            "head": i.train_head.known(),
            "tree": i.train_tree.known(),
            "runner_sha256": i.train_runner.known(),
            "image_digests": i.train_images.known(),
            "launch_request_sha256": i.train_launch.known().map(|t| sha256_hex(t.as_bytes())),
        },
        "toolchain": "determined by the equal image digests and the tree's rust-toolchain.toml",
        "assessed_at": crate::sor_client::stamp(now),
    });
    Assessment {
        mode: i.mode,
        refusals: out,
        source_gate_run: source,
        record,
    }
}

/// PURE: does the train already carry this assessment? Compared without
/// its timestamp, so a train waiting for a bay is not an audit event per
/// pass.
pub(crate) fn already_stamped(train: &Value, record: &Value) -> bool {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(m) = v.as_object_mut() {
            m.remove("assessed_at");
        }
        v
    };
    train
        .pointer(&format!("/metadata/{KEY_REUSE}"))
        .is_some_and(|stamped| strip(stamped) == strip(record))
}

/// PURE: the words the ci step's `train_gate` line adds when the train
/// carries its car's receipt — only when the stamped binding names the
/// gate-run the train names, so a reader is never told a reuse happened
/// that the verdict does not rest on.
pub(crate) fn reused_line(train: &Value) -> Option<String> {
    let stamp = train.pointer(&format!("/metadata/{KEY_REUSE}"))?;
    let run = text(train, &format!("/metadata/{}", crate::train_gate::KEY_RUN))?;
    (text(stamp, "/decision") == Some("reused") && text(stamp, "/source/gate_run") == Some(run))
        .then(|| {
            format!(
                "the car's own FULL gate-run {}, reused: assembled tree {} equals the gated tree, \
                 runner and images equal (design cdf626ac Q1)",
                &run[..8.min(run.len())],
                text(stamp, "/train/tree").map_or("?", |t| &t[..12.min(t.len())])
            )
        })
}

/// The qualifying one-car train, shared with the conductor's tests: a
/// FULL carrier-recorded green on the car's gate-run, copied verbatim
/// onto the car, the car boarded at that head, a train assembled from it.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub(crate) const HEAD: &str = "1197325fdb71a51d22636e8fabd78d52842de3b7";
    pub(crate) const GATE: &str = "9d6b3ae3-cf1e-411e-ab6d-f2a0b7780f18";
    pub(crate) const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    pub(crate) const RUNNER: &str =
        "a5281011a40b1cd09ca0ec1363436c82b0bc9dd44a20410ab6dec260e468127a";
    pub(crate) const DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    pub(crate) fn receipt() -> Value {
        json!({
            "verdict": "green", "mode": "full", "scope": "", "head": HEAD,
            "dirty": false, "ci": false, "unverifiable": [], "fails": [],
            "checks": [{"name": "test", "result": "pass", "seconds": 600}],
            "runner_source": "branch", "runner_sha256": RUNNER,
            "recorded_by": {
                "carrier": "pod-log", "job": "gate-9d6b3ae3cf1e411eab6df2a0b7780f18",
                "job_uid": "uid-1", "launched_sha": HEAD, "head_matches_launch": true,
                "sha256": "abc",
                "image_ids": {"gate": format!("registry.example/boss-ci@{DIGEST}")}
            }
        })
    }

    pub(crate) fn car(r: &Value) -> Value {
        json!({"id": "car-1", "kind": "ship-a-change",
               "metadata": {"boarded_head": HEAD, "regate_receipt": r.to_string()},
               "steps": []})
    }

    /// A launch request in the shape the native renderer retains (see a
    /// live one: gate-run ec7b1f01's `gate_launch_intent` file, 2026-10-19):
    /// the per-run values are the packet, the branch, the head and the
    /// selection mode; everything else is the environment.
    pub(crate) fn launch(head: &str, branch: &str, packet: &str, mode: &str) -> Value {
        json!({"apiVersion": "batch/v1", "kind": "Job",
               "metadata": {"name": format!("gate-{}", packet.replace('-', "")),
                            "namespace": "boss-dev",
                            "labels": {"app": "gate-runner", "boss.dev/packet": packet,
                                       "boss.dev/branch": branch.replace('/', "-"),
                                       "boss.dev/verdict-carrier": "pod-log"}},
               "spec": {"backoffLimit": 0, "activeDeadlineSeconds": 10800,
                        "template": {"metadata": {"labels": {"app": "gate-runner"}},
                                     "spec": {
            "initContainers": [
                {"name": "postgres", "image": format!("registry.example/postgres@{DIGEST}"),
                 "resources": {"limits": {"cpu": "2", "memory": "9Gi"}}},
                {"name": "gate", "image": format!("registry.example/boss-ci@{DIGEST}"),
                 "command": ["bash", "/repo-script/bootstrap.sh"],
                 "env": [{"name": "GATE_BRANCH", "value": branch},
                         {"name": "GATE_RUN_JOB_ID", "value": packet},
                         {"name": "GATE_MODE", "value": mode},
                         {"name": "GATE_VERDICT_CARRIER", "value": "pod-log"},
                         {"name": "BOSS_EDIT_LEVEL_ANSWER", "value": "{\"edit_level\":null}"},
                         {"name": "GATE_SHA", "value": head}],
                 "resources": {"limits": {"cpu": "8", "memory": "24Gi"}}}],
            "volumes": [{"name": "gate-disk",
                         "persistentVolumeClaim": {"claimName": "gate"}}]}}}})
    }

    /// The car gate's retained request, as the file store holds it.
    pub(crate) fn car_launch_text() -> String {
        launch(HEAD, "feat/x", GATE, "").to_string()
    }

    /// The train gate's would-be request: another packet, branch, head
    /// and mode, the same environment.
    pub(crate) fn train_launch_text() -> String {
        launch(
            "2222222222222222222222222222222222222222",
            "train/20261019-1200",
            "7f0c3a52-0d4e-4d7e-9a35-6d4f5b1e2c11",
            "--auto",
        )
        .to_string()
    }

    pub(crate) fn gate_run(r: &Value) -> Value {
        let bytes = car_launch_text();
        // Mode `full`, as the launcher records a full launch
        // (gate/reservation.rs `execution_mode`): 12 of the 200 newest
        // gate-runs on 2026-10-19 carried it, and none carried "".
        let intent = json!({"contract_version": 1, "mode": "full", "head": HEAD,
                            "packet": GATE, "namespace": "boss-dev",
                            "name": "gate-9d6b3ae3cf1e411eab6df2a0b7780f18",
                            "file_id": "b99637a4-49da-4f05-8515-97fdf5e26818",
                            "sha256": sha256_hex(bytes.as_bytes()),
                            "size_bytes": bytes.len()});
        json!({"id": GATE, "kind": "gate-run", "status": "closed",
               "metadata": {"sha": HEAD, "branch": "feat/x",
                            "gate_launch_intent": intent,
                            "_immutable_metadata": {"gate_launch_intent": intent}},
               "steps": [{"spec_slug": "record-verdict", "status": "completed",
                          "metadata": {"verdict": "green", "receipt": r.to_string()}}]})
    }

    /// The observations a qualifying pass makes: the assembled head and
    /// tree, the car's gated tree, the bootstrap runner and pinned images.
    pub(crate) fn observed() -> Observations {
        Observations {
            train_head: Observed::Known(HEAD.into()),
            train_tree: Observed::Known(TREE.into()),
            car_tree: Observed::Known(TREE.into()),
            train_runner: Observed::Known(RUNNER.into()),
            train_images: Observed::Known(DIGEST.into()),
            car_launch: Observed::Known(car_launch_text()),
            train_launch: Observed::Known(train_launch_text()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-17T23:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn train() -> Value {
        json!({"id": "t1", "metadata": {"boarded_jobs": ["car-1"]},
               "steps": [{"spec_slug": "assemble", "status": "completed",
                          "metadata": {"train_ref": "train/x@1197325fd"}}]})
    }

    fn inputs<'a>(t: &'a Value, c: &'a Value, g: &'a Value, unjudged: &'a [&'a str]) -> Inputs<'a> {
        Inputs {
            mode: Mode::Bind,
            train: t,
            prior_attempts: Ok(0),
            car: Ok(c),
            gate_run: Ok(g),
            train_head: Observed::Known(HEAD.into()),
            train_tree: Observed::Known(TREE.into()),
            car_tree: Observed::Known(TREE.into()),
            train_runner: Observed::Known(RUNNER.into()),
            train_images: Observed::Known(DIGEST.into()),
            car_launch: Observed::Known(car_launch_text()),
            train_launch: Observed::Known(train_launch_text()),
            unjudged,
        }
    }

    /// THE POSITIVE CASE, under a contract with nothing left unjudged:
    /// identical tree, a FULL carrier-recorded green copied verbatim from
    /// its gate-run, the same runner and image. It binds the ORIGINAL
    /// gate-run and records where everything came from.
    #[test]
    fn an_exact_tree_and_environment_match_reuses_the_original_gate_run() {
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        let a = assess(&inputs(&t, &c, &g, &[]), now());
        assert_eq!(a.refusals, Vec::<String>::new());
        assert!(a.qualified() && a.binds());
        assert_eq!(a.source_gate_run.as_deref(), Some(GATE));
        assert_eq!(a.record["decision"], "reused");
        assert_eq!(a.record["source"]["gate_run"], GATE);
        assert_eq!(a.record["source"]["head"], HEAD);
        assert_eq!(a.record["source"]["tree"], TREE);
        assert_eq!(a.record["source"]["runner_sha256"], RUNNER);
        assert_eq!(a.record["source"]["image_digests"], DIGEST);
        assert_eq!(
            a.record["source"]["launch_request_sha256"],
            sha256_hex(car_launch_text().as_bytes())
        );
        assert_eq!(
            a.record["train"]["launch_request_sha256"],
            sha256_hex(train_launch_text().as_bytes())
        );
        assert_eq!(a.record["train"]["tree"], TREE);
        // Shadow assesses the same and binds nothing.
        let mut i = inputs(&t, &c, &g, &[]);
        i.mode = Mode::Shadow;
        let s = assess(&i, now());
        assert!(s.qualified() && !s.binds());
        assert_eq!(s.record["decision"], "would reuse");
    }

    /// THE MECHANISM STAYS: an input a later contract names before it
    /// has a comparison re-gates the very match above, in bind mode too,
    /// and says why. The launch request was the last such input; it is
    /// compared now, so production's list is empty, and what still keeps
    /// reuse shut as shipped is real Unknowns — a tag-pinned image and the
    /// ConfigMap runner (`the_shipped_runner_manifest_leaves_the_train_side_unknown`).
    #[test]
    fn nothing_qualifies_while_any_contract_input_is_unjudged() {
        assert_eq!(UNJUDGED_INPUTS, &[] as &[&str]);
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        let pending = ["sidecar inputs unjudged: a later contract input"];
        let a = assess(&inputs(&t, &c, &g, &pending), now());
        assert!(!a.qualified() && !a.binds());
        assert_eq!(a.record["decision"], "regate");
        assert_eq!(a.refusals, vec![pending[0].to_string()]);
    }

    /// THE LAUNCHER RECORDS A FULL LAUNCH AS `full`. The first cut
    /// required "" and its fixture said "", so no real full car could
    /// ever qualify: measured 2026-10-19, the 200 newest gate-runs held
    /// 12 intents of mode `full` and none of "". The reader takes the
    /// writer's own constant, so the two cannot disagree again.
    #[test]
    fn a_full_launch_intent_reads_as_the_launcher_records_it() {
        assert_eq!(crate::gate::RECORDED_FULL_MODE, "full");
        let r = receipt();
        let (t, c) = (train(), car(&r));
        for (mode, qualifies) in [("full", true), ("", false), ("--auto", false)] {
            let mut g = gate_run(&r);
            g["metadata"]["gate_launch_intent"]["mode"] = json!(mode);
            g["metadata"]["_immutable_metadata"]["gate_launch_intent"]["mode"] = json!(mode);
            let a = assess(&inputs(&t, &c, &g, &[]), now());
            assert_eq!(a.qualified(), qualifies, "{mode:?}: {:?}", a.refusals);
        }
    }

    /// THE LAUNCH REQUEST IS THE ENVIRONMENT (the input UNJUDGED_INPUTS
    /// named until this car). The car's retained request and the train's
    /// would-be request differ in exactly four per-run values — packet,
    /// branch, head, selection mode — and in nothing else, or the train
    /// re-gates. One mutation at a time, on either side.
    #[test]
    fn any_launch_difference_beyond_the_per_run_values_requires_the_train_gate() {
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        // The per-run values alone differ in the fixtures: that qualifies.
        assert!(assess(&inputs(&t, &c, &g, &[]), now()).qualified());
        type Edit = Box<dyn Fn(&mut Value)>;
        let edits: Vec<(&str, Edit)> = vec![
            (
                "sidecar image",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["initContainers"][0]["image"] =
                        json!("registry.example/postgres:17")
                }),
            ),
            (
                "gate resources",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["initContainers"][1]["resources"]["limits"]["cpu"] =
                        json!("4")
                }),
            ),
            (
                "handed edit level",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["initContainers"][1]["env"][4]["value"] =
                        json!("{\"edit_level\":\"data\"}")
                }),
            ),
            (
                "edit level not handed",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["initContainers"][1]["env"]
                        .as_array_mut()
                        .unwrap()
                        .remove(4);
                }),
            ),
            (
                "extra sidecar",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["containers"] =
                        json!([{"name": "seed-refresh", "image": "x"}])
                }),
            ),
            (
                "volume",
                Box::new(|d| {
                    d["spec"]["template"]["spec"]["volumes"][0]["persistentVolumeClaim"]["claimName"] =
                        json!("gate-2")
                }),
            ),
            (
                "namespace",
                Box::new(|d| d["metadata"]["namespace"] = json!("boss-play")),
            ),
            (
                "deadline",
                Box::new(|d| d["spec"]["activeDeadlineSeconds"] = json!(600)),
            ),
            (
                "another label",
                Box::new(|d| d["metadata"]["labels"]["boss.dev/gate-layout"] = json!("x")),
            ),
        ];
        for (name, edit) in &edits {
            for side in ["car", "train"] {
                let mut i = inputs(&t, &c, &g, &[]);
                if side == "car" {
                    // The car's side is bound to its intent's sha256, so an
                    // edit there must carry a gate-run that names it — or
                    // the hash, not the difference, is what refuses.
                    let mut doc: Value = serde_json::from_str(&car_launch_text()).unwrap();
                    edit(&mut doc);
                    i.car_launch = Observed::Known(doc.to_string());
                } else {
                    let mut doc: Value = serde_json::from_str(&train_launch_text()).unwrap();
                    edit(&mut doc);
                    i.train_launch = Observed::Known(doc.to_string());
                }
                let mut g2 = g.clone();
                if let Observed::Known(text) = &i.car_launch {
                    let sha = sha256_hex(text.as_bytes());
                    g2["metadata"]["gate_launch_intent"]["sha256"] = json!(sha);
                    g2["metadata"]["_immutable_metadata"]["gate_launch_intent"]["sha256"] =
                        json!(sha);
                }
                i.gate_run = Ok(&g2);
                let a = assess(&i, now());
                assert!(!a.qualified(), "{name} on the {side}: {:?}", a.refusals);
                assert!(
                    a.refusals
                        .iter()
                        .any(|s| s.contains("launch request differs")),
                    "{name} on the {side}: {:?}",
                    a.refusals
                );
            }
        }
    }

    /// The car's side is the RETAINED request, not a description of it:
    /// bytes that do not hash to the intent's sha256 refuse, as do
    /// unreadable or unknown requests on either side, and a car request
    /// whose bound head or mode is not the full launch of the boarded head.
    #[test]
    fn an_unproven_or_unknown_launch_request_requires_the_train_gate() {
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        let refused = |what: &str, i: &Inputs, needle: &str| {
            let a = assess(i, now());
            assert!(!a.qualified(), "{what}: {:?}", a.refusals);
            assert!(
                a.refusals.iter().any(|s| s.contains(needle)),
                "{what}: wanted {needle:?} in {:?}",
                a.refusals
            );
        };
        // Equal content, other bytes: not the retained request.
        let mut i = inputs(&t, &c, &g, &[]);
        let pretty = serde_json::to_string_pretty(&launch(HEAD, "feat/x", GATE, "")).unwrap();
        i.car_launch = Observed::Known(pretty);
        refused("re-serialized", &i, "not the bytes the launch intent names");
        let mut i = inputs(&t, &c, &g, &[]);
        i.car_launch = Observed::Unknown("file store refused".into());
        refused("car unknown", &i, "file store refused");
        let mut i = inputs(&t, &c, &g, &[]);
        i.train_launch = Observed::Unknown("kubectl dry-run failed".into());
        refused("train unknown", &i, "kubectl dry-run failed");
        let mut i = inputs(&t, &c, &g, &[]);
        i.train_launch = Observed::Known("not json".into());
        refused("train unreadable", &i, "launch request");
        // A car request bound to another head, or to a selected mode.
        for (what, doc) in [
            ("other head", launch(&"3".repeat(40), "feat/x", GATE, "")),
            ("auto mode", launch(HEAD, "feat/x", GATE, "--auto")),
        ] {
            let text = doc.to_string();
            let mut g2 = g.clone();
            let sha = sha256_hex(text.as_bytes());
            g2["metadata"]["gate_launch_intent"]["sha256"] = json!(sha);
            g2["metadata"]["_immutable_metadata"]["gate_launch_intent"]["sha256"] = json!(sha);
            let mut i = inputs(&t, &c, &g2, &[]);
            i.car_launch = Observed::Known(text);
            refused(what, &i, "the car gate's launch request");
        }
    }

    /// Every unequal or unknown input re-gates; none is a single-car
    /// bypass. One mutation at a time from the qualifying case.
    #[test]
    fn every_unequal_or_unknown_input_requires_the_ordinary_train_gate() {
        type Packets = Box<dyn Fn(&mut Value, &mut Value)>;
        type Observations = Box<dyn Fn(&mut Inputs)>;
        let refused = |name: &str, a: Assessment| {
            assert!(
                !a.qualified(),
                "{name}: must re-gate, refusals {:?}",
                a.refusals
            );
            assert!(!a.binds(), "{name}");
            assert_eq!(a.record["decision"], "regate", "{name}");
        };
        // What the record says: the receipt (copied onto car and gate-run
        // alike, so only its content differs) and the train.
        let packets: Vec<(&str, Packets)> = vec![
            ("auto mode", Box::new(|r, _| r["mode"] = json!("auto"))),
            ("scoped", Box::new(|r, _| r["scope"] = json!("boss-cli"))),
            ("dirty", Box::new(|r, _| r["dirty"] = json!(true))),
            (
                "no estate",
                Box::new(|r, _| r["estate"] = json!({"declared": "none"})),
            ),
            (
                "unverifiable",
                Box::new(|r, _| r["unverifiable"] = json!(["x.sql"])),
            ),
            (
                "skipped check",
                Box::new(|r, _| r["checks"][0]["result"] = json!("skip")),
            ),
            (
                "fallback runner",
                Box::new(|r, _| r["runner_fallback_reason"] = json!("gone")),
            ),
            (
                "runner unnamed",
                Box::new(|r, _| r["runner_sha256"] = Value::Null),
            ),
            (
                "no image ids",
                Box::new(|r, _| r["recorded_by"]["image_ids"] = json!({})),
            ),
            (
                "not carrier",
                Box::new(|r, _| r["recorded_by"]["carrier"] = json!("self")),
            ),
            (
                "launch moved",
                Box::new(|r, _| r["recorded_by"]["head_matches_launch"] = json!(false)),
            ),
            ("red", Box::new(|r, _| r["verdict"] = json!("failed"))),
            (
                "two cars",
                Box::new(|_, t| t["metadata"]["boarded_jobs"] = json!(["car-1", "car-2"])),
            ),
            (
                "relaunched",
                Box::new(|_, t| t["metadata"]["train_gate_relaunches"] = json!(1)),
            ),
            (
                "already named",
                Box::new(|_, t| t["metadata"]["train_gate_run"] = json!("g")),
            ),
        ];
        for (name, mutate) in packets {
            let (mut r, mut t) = (receipt(), train());
            mutate(&mut r, &mut t);
            let (c, g) = (car(&r), gate_run(&r));
            refused(name, assess(&inputs(&t, &c, &g, &[]), now()));
        }
        // What the conductor observed this pass.
        let observations: Vec<(&str, Observations)> = vec![
            (
                "tree differs",
                Box::new(|i| i.car_tree = Observed::Known("f".repeat(40))),
            ),
            (
                "tree unknown",
                Box::new(|i| i.train_tree = Observed::Unknown("no clone".into())),
            ),
            (
                "runner differs",
                Box::new(|i| i.train_runner = Observed::Known("0".repeat(64))),
            ),
            (
                "runner unknown",
                Box::new(|i| i.train_runner = Observed::Unknown("configmap".into())),
            ),
            (
                "image unknown",
                Box::new(|i| i.train_images = Observed::Unknown("tag".into())),
            ),
            (
                "image differs",
                Box::new(|i| i.train_images = Observed::Known("sha256:22".into())),
            ),
            (
                "census open",
                Box::new(|i| i.prior_attempts = Err("list refused".into())),
            ),
            ("original exists", Box::new(|i| i.prior_attempts = Ok(1))),
            ("car unread", Box::new(|i| i.car = Err("404".into()))),
            ("gate unread", Box::new(|i| i.gate_run = Err("404".into()))),
            (
                "head not assembled",
                Box::new(|i| i.train_head = Observed::Known("2".repeat(40))),
            ),
            (
                "head unknown",
                Box::new(|i| i.train_head = Observed::Unknown("no ref".into())),
            ),
        ];
        for (name, mutate) in observations {
            let r = receipt();
            let (t, c, g) = (train(), car(&r), gate_run(&r));
            let mut i = inputs(&t, &c, &g, &[]);
            mutate(&mut i);
            refused(name, assess(&i, now()));
        }
    }

    /// A car receipt that is not the gate-run's own record, word for
    /// word, is a retyped claim and never carries a train.
    #[test]
    fn a_receipt_that_is_not_the_gate_runs_record_is_refused() {
        let r = receipt();
        let mut other = r.clone();
        other["checks"][0]["seconds"] = json!(1);
        let (t, c, g) = (train(), car(&other), gate_run(&r));
        let a = assess(&inputs(&t, &c, &g, &[]), now());
        assert!(
            a.refusals
                .iter()
                .any(|s| s.contains("not the gate-run's recorded receipt")),
            "{:?}",
            a.refusals
        );
        // And a train gate or main's gate is never a car's source.
        let mut tg_run = gate_run(&r);
        tg_run["metadata"]["train_gate"] = json!(true);
        let a = assess(&inputs(&t, &car(&r), &tg_run, &[]), now());
        assert!(
            a.refusals.iter().any(|s| s.contains("not a car's gate")),
            "{:?}",
            a.refusals
        );
        // An auto launch intent behind a receipt claiming full refuses.
        let mut auto = gate_run(&r);
        auto["metadata"]["gate_launch_intent"]["mode"] = json!("--auto");
        auto["metadata"]["_immutable_metadata"]["gate_launch_intent"]["mode"] = json!("--auto");
        let a = assess(&inputs(&t, &car(&r), &auto, &[]), now());
        assert!(
            a.refusals.iter().any(|s| s.contains("launch intent")),
            "{:?}",
            a.refusals
        );
    }

    /// THE JUDGED-INIT LAYOUT (design b4bf8bba) runs a second script from
    /// the same ConfigMap: the `seed-refresh` helper, which maintains the
    /// seed disk every later gate starts from. Each ConfigMap script is an
    /// input the conductor cannot read, so the runner is Unknown for THAT
    /// reason, and a bootstrap gate beside a ConfigMap helper still is.
    #[test]
    fn a_configmap_helper_beside_the_gate_leaves_the_runner_unknown() {
        let blob = Observed::Known(RUNNER.into());
        let judged = "initContainers:\n  - name: gate\n    command: [\"bash\", \"/repo-script/run.sh\"]\ncontainers:\n  - name: seed-refresh\n    command: [\"bash\", \"/repo-script/seed-refresh.sh\", \"maintain\"]\n";
        assert!(
            matches!(manifest_runner(judged, &blob), Observed::Unknown(ref w) if w.contains("ConfigMap")),
            "{:?}",
            manifest_runner(judged, &blob)
        );
        let bootstrapped = judged.replace("/repo-script/run.sh", "/repo-script/bootstrap.sh");
        assert!(
            matches!(manifest_runner(&bootstrapped, &blob), Observed::Unknown(ref w) if w.contains("seed-refresh.sh") && w.contains("ConfigMap")),
            "{:?}",
            manifest_runner(&bootstrapped, &blob)
        );
        let twice = "containers:\n  - name: a\n    command: [\"bash\", \"/repo-script/bootstrap.sh\"]\n  - name: b\n    command: [\"bash\", \"/repo-script/bootstrap.sh\"]\n";
        assert!(
            matches!(manifest_runner(twice, &blob), Observed::Unknown(ref w) if w.contains("expected one"))
        );
    }

    #[test]
    fn the_mode_is_off_unless_a_known_word_says_otherwise() {
        assert_eq!(mode_from(None), (Mode::Off, None));
        assert_eq!(mode_from(Some("")), (Mode::Off, None));
        assert_eq!(mode_from(Some("off")), (Mode::Off, None));
        assert_eq!(mode_from(Some(" shadow ")), (Mode::Shadow, None));
        assert_eq!(mode_from(Some("exact-full")), (Mode::Bind, None));
        let (m, warn) = mode_from(Some("1"));
        assert_eq!(m, Mode::Off);
        assert!(warn.unwrap().contains("reading it as off"));
    }

    /// The train side of the environment, read from the conductor's own
    /// manifest: a mutable tag is Unknown; the ConfigMap runner is
    /// Unknown; only the bootstrap runner ties the runner to the tree.
    #[test]
    fn the_train_side_environment_is_known_only_when_pinned() {
        let tagged = "containers:\n  - name: gate\n    image: registry.example/boss-ci:rust1.96\n    command: [\"bash\", \"/repo-script/run.sh\"]\n  - name: postgres\n    image: registry.example/postgres:16\n";
        assert!(
            matches!(manifest_images(tagged), Observed::Unknown(w) if w.contains("mutable image"))
        );
        let blob = Observed::Known(RUNNER.into());
        assert!(
            matches!(manifest_runner(tagged, &blob), Observed::Unknown(w) if w.contains("ConfigMap"))
        );
        let pinned = format!(
            "containers:\n  - name: gate\n    image: \"boss-ci@{DIGEST}\" # pinned\n    command: [\"bash\", \"/repo-script/bootstrap.sh\"]\n    # image: commented:out\n  - name: pg\n    image: postgres@{DIGEST}\n"
        );
        assert_eq!(manifest_images(&pinned), Observed::Known(DIGEST.into()));
        assert_eq!(manifest_runner(&pinned, &blob), blob);
        assert!(matches!(
            manifest_runner("kind: Job\n", &blob),
            Observed::Unknown(_)
        ));
    }

    /// THE TREE'S OWN MANIFEST, as the conductor renders it for a train
    /// gate, is not yet one reuse can be judged against: its images are
    /// tags and its runner is the ConfigMap's. When a car pins the images
    /// or points the pod at the bootstrap, this fails, and the reviewer of
    /// that car sees the reuse contract's inputs move with it.
    #[test]
    fn the_shipped_runner_manifest_leaves_the_train_side_unknown() {
        let path = boss_testing::repo_root().join("infra/gate-runner/gate-runner.yaml");
        let text = std::fs::read_to_string(&path).expect("the runner manifest");
        let doc = crate::gate::render_job(
            &text,
            "train/20261017-2139",
            GATE,
            crate::gate::Requester::Train.mode(),
        )
        .expect("renders");
        assert!(
            matches!(manifest_images(&doc), Observed::Unknown(w) if w.contains("mutable image")),
            "{:?}",
            manifest_images(&doc)
        );
        assert!(
            matches!(
                manifest_runner(&doc, &Observed::Known(RUNNER.into())),
                Observed::Unknown(w) if w.contains("ConfigMap")
            ),
            "{:?}",
            manifest_runner(&doc, &Observed::Known(RUNNER.into()))
        );
    }

    #[test]
    fn the_receipt_names_its_gate_run_only_through_the_carrier() {
        assert_eq!(gate_run_named_by(&receipt()).as_deref(), Some(GATE));
        let mut r = receipt();
        r["recorded_by"]["job"] = json!("gate-not-a-uuid");
        assert_eq!(gate_run_named_by(&r), None);
        r["recorded_by"]["job"] = json!("gate-9d6b3ae3cf1e411eab6df2a0b7780f18");
        r["recorded_by"]["carrier"] = json!("other");
        assert_eq!(gate_run_named_by(&r), None);
    }

    /// Idempotence: the same assessment an hour later is the stamp already
    /// carried; a changed one is not.
    #[test]
    fn a_repeated_assessment_is_not_restamped() {
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        let pending = ["an input still unjudged"];
        let a = assess(&inputs(&t, &c, &g, &pending), now());
        let mut stamped = t.clone();
        stamped["metadata"][KEY_REUSE] = a.record.clone();
        let later = assess(
            &inputs(&t, &c, &g, &pending),
            now() + chrono::Duration::hours(1),
        );
        assert!(already_stamped(&stamped, &later.record));
        let changed = assess(&inputs(&t, &c, &g, &[]), now());
        assert!(!already_stamped(&stamped, &changed.record));
    }

    #[test]
    fn the_ci_line_names_a_reuse_only_when_the_train_rests_on_it() {
        let r = receipt();
        let (t, c, g) = (train(), car(&r), gate_run(&r));
        let a = assess(&inputs(&t, &c, &g, &[]), now());
        let mut bound = t.clone();
        bound["metadata"][KEY_REUSE] = a.record.clone();
        bound["metadata"][crate::train_gate::KEY_RUN] = json!(GATE);
        let line = reused_line(&bound).expect("bound");
        assert!(
            line.contains("9d6b3ae3") && line.contains("reused"),
            "{line}"
        );
        bound["metadata"][crate::train_gate::KEY_RUN] = json!("another-run");
        assert_eq!(reused_line(&bound), None);
        assert_eq!(reused_line(&t), None);
    }
}
