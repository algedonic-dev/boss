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
//! gate, its own design). It does not accept a scoped (`-p <crate>`) car
//! green, chosen by hand.
//!
//! AN `--auto` CAR GREEN QUALIFIES (backlog 2647cf38, David 2026-11-03).
//! Car gates run `--auto` since David's 2026-11-02 decision, so the
//! FULL-only first cut never fired: every train on 2026-11-03 carried one
//! car, and train gates were 22.3 of ~88 bay-hours (11-01 12Z to 11-03
//! 12Z), retesting trees their cars had just passed. The train gate runs
//! `--auto` too, and auto selects from the diff against main, so on the
//! same base and the same tree it selects what the car's gate selected —
//! which is why the assembled head must be EXACTLY one merge of main at
//! departure and the gated head, with that main the car's base. This
//! narrows design 128b5496 (the train gate tests the assembled consist)
//! only where the consist is what was gated: a multi-car train, a moved
//! main or a car on another base still gates.
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
//! THE ENVIRONMENT IS FOUR COMPARISONS: the tree (which fixes the
//! toolchain file), the runner, each container's image, and the whole
//! launch request — env, the handed edit level, resources, sidecars,
//! volumes, deadlines — read back as the bytes the car gate's immutable
//! launch intent names and set beside the request the train gate would
//! submit ([`launch_refusals`]). Only the packet, branch, head and
//! selection mode may differ between the two, and the mode only as a full
//! car beside an `--auto` train.
//!
//! THE RUNNER (backlog 2647cf38). The first cut compared the train gate's
//! ConfigMap run.sh, which the conductor's Role cannot read, so it was
//! always Unknown. It is now the run.sh the assembled TREE carries: the
//! ConfigMap is written from main by the converge, so a car whose receipt
//! names its own tree's run.sh was judged by the runner main carries, and
//! a car gated before a run.sh change converged is refused (car 5ce50347
//! would have been, 2026-11-03).
//!
//! THE IMAGES ARE PROVEN BEFORE DEPARTURE OR THE TRAIN GATES (review
//! 93a363fa, agent-codex, CHANGES on 9085843c; design cdf626ac Q1 requires
//! image and toolchain equal). Every container's reference in the train's
//! request must be pinned by digest and equal the digest the car's
//! receipt records its pod resolving for that container
//! ([`image_refusals`]). An equal TAG is not proof — it resolves only on
//! pull and can be repushed between the car's gate and departure without
//! changing a request byte — and the conductor holds no registry read to
//! resolve one, so a tag is Unknown and refuses. The shipped gate images
//! are tags, so as shipped nothing binds until backlog 35a0de38 pins them
//! (`the_shipped_gate_images_are_mutable_so_reuse_cannot_fire_yet`).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The conductor's switch. Absent, empty or `off`: the code is inert.
/// `shadow`: assess every train and stamp the assessment, but always file
/// the ordinary train gate — the measurement before activation. The one
/// spelling that binds is [`BIND_WORD`]; anything else is read as off,
/// and said so, rather than guessed at.
pub(crate) const MODE_ENV: &str = "BOSS_TRAIN_GATE_REUSE";
pub(crate) const SHADOW_WORD: &str = "shadow";
/// `exact-full` until backlog 2647cf38, when an `--auto` green began to
/// qualify. The conductor's ConfigMap sets it (`boss-conductor.yaml`).
pub(crate) const BIND_WORD: &str = "exact";

/// The gate argument an `--auto` launch carries in GATE_MODE and records
/// as its launch intent's mode (gate.rs `normalize_mode`).
const AUTO_ARGUMENT: &str = "--auto";

/// PURE: the receipt modes that may carry a train, each with the
/// GATE_MODE it was launched with and the mode its launch intent records.
fn launch_of(receipt_mode: Option<&str>) -> Option<(&'static str, &'static str)> {
    match receipt_mode {
        Some("full") => Some(("", crate::gate::RECORDED_FULL_MODE)),
        Some("auto") => Some((AUTO_ARGUMENT, AUTO_ARGUMENT)),
        _ => None,
    }
}

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
/// empty list is not an open door: every other input still refuses when
/// unequal or Unknown — a mutable image reference among them.
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
/// the boarded head and the runner argument `mode` its receipt names (""
/// for full, `--auto`), so it is the launch the receipt vouches for. An
/// `--auto` car covers only a train whose request binds `--auto` too:
/// selection is a function of the mode as well as the diff. The train's side is the request the
/// conductor's own native path renders and does not send. Both are then
/// compared whole after [`environment_of`]: images per container,
/// commands, env (the handed edit level among it), resources, volumes,
/// sidecars, deadlines, labels. A difference names where.
pub(crate) fn launch_refusals(
    car: &Observed,
    train: &Observed,
    intent_sha256: Option<&str>,
    head: &str,
    mode: &str,
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
    if env_values(&car_doc, "GATE_MODE") != [mode] {
        out.push(format!(
            "the car gate's launch request binds GATE_MODE {:?}, not the {:?} its receipt names",
            env_values(&car_doc, "GATE_MODE"),
            mode
        ));
    }
    let (train_doc, train_env) = match parsed("train", train) {
        Ok(p) => p,
        Err(why) => {
            out.push(why);
            return out;
        }
    };
    if !mode.is_empty() && env_values(&train_doc, "GATE_MODE") != [mode] {
        out.push(format!(
            "the train gate would run GATE_MODE {:?}: an {mode} car green covers only an {mode} \
             train gate",
            env_values(&train_doc, "GATE_MODE")
        ));
    }
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
    /// The assembled head's parents, space-joined: main at departure, then
    /// the head the train merged.
    pub train_parents: Observed,
    /// The merge base of main at departure (the first parent) and the
    /// car's gated head.
    pub car_base: Observed,
    pub train_tree: Observed,
    pub car_tree: Observed,
    /// sha256 of the run.sh the assembled tree carries.
    pub train_runner: Observed,
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
    /// The assembled head's parents, space-joined (conductor clone).
    pub train_parents: Observed,
    /// merge-base(main at departure, the car's gated head).
    pub car_base: Observed,
    pub train_tree: Observed,
    /// The tree of the head the car's receipt vouches for.
    pub car_tree: Observed,
    /// sha256 of the run.sh the assembled tree carries — the runner main
    /// carries once its converge writes the ConfigMap, whichever runner
    /// layout the manifest names.
    pub train_runner: Observed,
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

/// PURE: why one container's image reference is not proven, before
/// departure, to be the image the car's pod ran — or nothing. Proof is a
/// digest-pinned reference equal to the digest the car's receipt records
/// the kubelet resolving for that same container. A tag is never proof:
/// it resolves only when a pod pulls it, and can be repushed between the
/// car's gate and departure without changing a byte of either launch
/// request (review 93a363fa, agent-codex, on 9085843c).
fn image_refusal(image: &str, container: &str, receipt: &Value) -> Option<String> {
    let Some(pinned) = digest_of(image) else {
        return Some(format!(
            "image unknown: container {container} names the mutable image {image}, resolved \
             only when a pod pulls it, so nothing before departure says it is the image the \
             car's gate ran (backlog 35a0de38 pins the gate images by digest)"
        ));
    };
    let resolved = receipt
        .pointer("/recorded_by/image_ids")
        .and_then(|ids| ids.get(container))
        .and_then(Value::as_str)
        .and_then(digest_of);
    match resolved {
        None => Some(format!(
            "image unknown: the car's receipt records no resolved image for container {container}"
        )),
        Some(r) if r != pinned => Some(format!(
            "image differs: container {container} pins {pinned}, the car's pod resolved {r}"
        )),
        Some(_) => None,
    }
}

/// PURE: the image refusals over every container of the train gate's
/// would-be launch request. That request equals the car's retained one
/// but for per-run values ([`launch_refusals`]), so its references are
/// the car's too; this binds each to what the car's pod resolved.
fn image_refusals(train: &Observed, receipt: &Value) -> Vec<String> {
    let doc = match train {
        Observed::Known(text) => serde_json::from_str::<Value>(text).ok(),
        Observed::Unknown(_) => None,
    };
    let Some(doc) = doc else {
        return vec!["image unknown: the train gate's launch request is not readable".into()];
    };
    if containers(&doc).next().is_none() {
        return vec!["image unknown: the train gate's launch request names no container".into()];
    }
    containers(&doc)
        .filter_map(|c| {
            let name = c.get("name").and_then(Value::as_str).unwrap_or("?");
            match c.get("image").and_then(Value::as_str) {
                Some(image) => image_refusal(image, name, receipt),
                None => Some(format!("image unknown: container {name} names no image")),
            }
        })
        .collect()
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
    // A full run selects nothing; an `--auto` run's scope is what its
    // selection derived from the diff against main, which the same base
    // and tree derive again for the train (backlog 2647cf38).
    match s("/mode") {
        Some("full") if !s("/scope").is_some_and(|v| v.trim().is_empty()) => out
            .push("the car's FULL receipt names a scope: a selected run is not a full run".into()),
        Some("full") | Some("auto") => {}
        other => out.push(format!(
            "the car's gate ran mode {other:?}: only a FULL or --auto car green covers what the \
             train gate selects"
        )),
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

    // THE ORIGINAL GATE-RUN: the receipt is a copy of its record, and its
    // launch intent and request record the mode the receipt names.
    let launched_as = launch_of(text(&receipt, "/mode"));
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
                        && launched_as.is_some_and(|(_, m)| text(intent, "/mode") == Some(m))
                        && text(intent, "/head") == Some(head.as_str()) =>
                {
                    intent_sha256 = text(intent, "/sha256").map(str::to_string);
                }
                Ok(_) => out.push(
                    "the gate-run's immutable launch intent is absent or not the receipt's full or \
                     --auto launch of the boarded head"
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

    // MAIN AT DEPARTURE PLUS THE GATED HEAD (backlog 2647cf38): the head
    // is one `--no-ff` merge whose first parent is main at departure and
    // whose second is the car's gated head, and that main is the car's
    // base — the diff an `--auto` selection reads is then the car's own.
    let mut base: Option<&str> = None;
    match (&i.train_parents, &i.car_base) {
        (Observed::Unknown(why), _) => out.push(format!("main at departure unknown: {why}")),
        (_, Observed::Unknown(why)) => out.push(format!("the car's base unknown: {why}")),
        (Observed::Known(parents), Observed::Known(car_base)) => {
            match parents.split_whitespace().collect::<Vec<_>>().as_slice() {
                [main, merged] if is_sha(main) && *merged == head => {
                    base = Some(*main);
                    if car_base.as_str() != *main {
                        out.push(format!(
                            "the car was not gated on main at departure {main}: its head's merge \
                             base with it is {car_base}"
                        ));
                    }
                }
                _ => out.push(format!(
                    "the assembled head is not one merge of main at departure and the gated head \
                     {head}: its parents are {parents:?}"
                )),
            }
        }
    }

    // THE ENVIRONMENT: the runner the car ran is the one its tree carries;
    // the toolchain follows from the tree and the images.
    let car_runner = match text(&receipt, "/runner_sha256") {
        Some(s) if s.len() == 64 => Observed::Known(s.to_string()),
        _ => Observed::Unknown("the car's receipt names no runner sha256".into()),
    };
    pair("runner", &i.train_runner, &car_runner, &mut out);
    // Each image the train would run is pinned to the digest the car's pod
    // resolved for that container; a tag refuses (review 93a363fa).
    let car_images = receipt_images(&receipt);
    if let Observed::Unknown(why) = &car_images {
        out.push(format!("image unknown: {why}"));
    }
    out.extend(image_refusals(&i.train_launch, &receipt));
    // THE LAUNCH: everything else the run was given.
    out.extend(launch_refusals(
        &i.car_launch,
        &i.train_launch,
        intent_sha256.as_deref(),
        &head,
        launched_as.map_or("", |(argument, _)| argument),
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
            "mode": text(&receipt, "/mode"),
            "scope": text(&receipt, "/scope"),
            "tree": i.car_tree.known(),
            "runner_sha256": car_runner.known(),
            "image_digests": car_images.known(),
            "launch_request_sha256": intent_sha256,
        },
        "train": {
            "head": i.train_head.known(),
            "base": base,
            "tree": i.train_tree.known(),
            "runner_sha256": i.train_runner.known(),
            "launch_request_sha256": i.train_launch.known().map(|t| sha256_hex(t.as_bytes())),
        },
        "toolchain": "determined by the tree's rust-toolchain.toml and the digest-pinned images the car's pods resolved",
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

/// PURE: the car's gate-run a train carries as its own gate — the one its
/// `train_gate_run` names, when its stamp records `reused` from exactly
/// that run. A bind writes both onto the TRAIN only; the car's gate-run
/// keeps no `train` mark, so this is how any reader that needs "is this
/// gate-run a train's gate" recognises one (strike release, backlog
/// cb2268d3). A shadow stamp (`would reuse`) or a stamp from another run
/// binds nothing.
pub(crate) fn reused_gate_run(train: &Value) -> Option<&str> {
    let stamp = train.pointer(&format!("/metadata/{KEY_REUSE}"))?;
    let run = text(train, &format!("/metadata/{}", crate::train_gate::KEY_RUN))?;
    (text(stamp, "/decision") == Some("reused") && text(stamp, "/source/gate_run") == Some(run))
        .then_some(run)
}

/// PURE: the words the ci step's `train_gate` line adds when the train
/// carries its car's receipt — only when the stamped binding names the
/// gate-run the train names ([`reused_gate_run`]), so a reader is never
/// told a reuse happened that the verdict does not rest on.
pub(crate) fn reused_line(train: &Value) -> Option<String> {
    let run = reused_gate_run(train)?;
    let stamp = train.pointer(&format!("/metadata/{KEY_REUSE}"))?;
    let short = |p: &str, n: usize| text(stamp, p).map_or("?", |t| &t[..n.min(t.len())]);
    Some(format!(
        "the car's own {} gate-run {}, reused: assembled tree {} is main at departure {} \
         merged with the gated head {}, runner, pinned images and launch request equal (backlog 2647cf38)",
        text(stamp, "/source/mode").unwrap_or("?"),
        &run[..8.min(run.len())],
        short("/train/tree", 12),
        short("/train/base", 9),
        short("/source/head", 9),
    ))
}

/// The qualifying one-car train, shared with the conductor's tests: a
/// FULL carrier-recorded green on the car's gate-run, copied verbatim
/// onto the car, the car boarded at that head, a train assembled from it.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub(crate) const HEAD: &str = "1197325fdb71a51d22636e8fabd78d52842de3b7";
    /// Main at departure: the assembled head's first parent, and an
    /// ancestor of the car's gated head.
    pub(crate) const BASE: &str = "d6d2df23f6d7d60ae55a84f02ccb7e9d2c7fac1d";
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
                "image_ids": {"gate": format!("registry.example/boss-ci@{DIGEST}"),
                              "postgres": format!("registry.example/postgres@{DIGEST}")}
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
        // Mode `full`, as the launcher records a full launch
        // (gate/reservation.rs `execution_mode`): 12 of the 200 newest
        // gate-runs on 2026-10-19 carried it, and none carried "".
        gate_run_launched(r, "full", &car_launch_text())
    }

    /// THE CAR AS IT IS GATED SINCE DAVID'S 2026-11-02 DECISION: `--auto`.
    /// The receipt says `auto` and names the scope the selection derived
    /// (car 5ce50347's said `boss-cli boss-testing`, 2026-11-03, and its
    /// train's own gate derived the same); the launch intent records
    /// `--auto`, as the launcher writes it, and so does GATE_MODE.
    pub(crate) fn auto_receipt() -> Value {
        let mut r = receipt();
        r["mode"] = json!("auto");
        r["scope"] = json!("boss-cli boss-testing");
        r
    }

    pub(crate) fn auto_car_launch_text() -> String {
        launch(HEAD, "feat/x", GATE, "--auto").to_string()
    }

    pub(crate) fn auto_gate_run(r: &Value) -> Value {
        gate_run_launched(r, "--auto", &auto_car_launch_text())
    }

    /// The observations of a qualifying pass over an `--auto` car.
    pub(crate) fn auto_observed() -> Observations {
        Observations {
            car_launch: Observed::Known(auto_car_launch_text()),
            ..observed()
        }
    }

    /// A car's gate-run whose immutable launch intent records `mode` and
    /// names `bytes` as the retained request.
    pub(crate) fn gate_run_launched(r: &Value, mode: &str, bytes: &str) -> Value {
        let intent = json!({"contract_version": 1, "mode": mode, "head": HEAD,
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

    /// The observations a qualifying pass makes: the assembled head, its
    /// parents (main at departure, the gated head) and tree, the car's
    /// base and gated tree, and the run.sh the tree carries.
    pub(crate) fn observed() -> Observations {
        Observations {
            train_head: Observed::Known(HEAD.into()),
            train_parents: Observed::Known(format!("{BASE} {HEAD}")),
            car_base: Observed::Known(BASE.into()),
            train_tree: Observed::Known(TREE.into()),
            car_tree: Observed::Known(TREE.into()),
            train_runner: Observed::Known(RUNNER.into()),
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
            train_parents: Observed::Known(format!("{BASE} {HEAD}")),
            car_base: Observed::Known(BASE.into()),
            train_tree: Observed::Known(TREE.into()),
            car_tree: Observed::Known(TREE.into()),
            train_runner: Observed::Known(RUNNER.into()),
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

    /// AN `--auto` CAR GREEN CARRIES ITS ONE-CAR TRAIN (backlog 2647cf38,
    /// David 2026-11-03). Car gates run `--auto` since 2026-11-02, so the
    /// FULL-only rule never fired, and every train on 2026-11-03 carried
    /// one car and retested its tree (train gates were 22.3 of ~88
    /// bay-hours, 11-01 12Z to 11-03 12Z). The train gate also runs
    /// `--auto`, and auto selects from the diff against main: on the same
    /// base and the same tree it selects what the car's gate selected.
    #[test]
    fn an_auto_car_green_on_main_at_departure_reuses_the_original_gate_run() {
        let r = auto_receipt();
        let (t, c, g) = (train(), car(&r), auto_gate_run(&r));
        let mut i = inputs(&t, &c, &g, &[]);
        i.car_launch = Observed::Known(auto_car_launch_text());
        let a = assess(&i, now());
        assert_eq!(a.refusals, Vec::<String>::new());
        assert!(a.qualified() && a.binds());
        assert_eq!(a.source_gate_run.as_deref(), Some(GATE));
        assert_eq!(a.record["decision"], "reused");
        assert_eq!(a.record["source"]["mode"], "auto");
        assert_eq!(a.record["source"]["scope"], "boss-cli boss-testing");
        assert_eq!(a.record["train"]["base"], BASE);
        // A full car still carries it, and says so.
        let full = assess(
            &inputs(&t, &car(&receipt()), &gate_run(&receipt()), &[]),
            now(),
        );
        assert!(full.qualified(), "{:?}", full.refusals);
        assert_eq!(full.record["source"]["mode"], "full");
    }

    /// SAME BASE, SAME CAR HEAD (backlog 2647cf38): the assembled head is
    /// exactly one merge of main at departure and the car's gated head,
    /// and that main is the base the car was gated on. Anything else — a
    /// car gated before main moved, another head merged, a head that is
    /// not one merge, a base the clone cannot answer — files the train
    /// gate, whatever the trees say.
    #[test]
    fn a_train_that_is_not_main_at_departure_plus_the_gated_head_requires_the_train_gate() {
        let r = auto_receipt();
        let (t, c, g) = (train(), car(&r), auto_gate_run(&r));
        type Mutation = Box<dyn Fn(&mut Inputs)>;
        let cases: Vec<(&str, Mutation, &str)> = vec![
            (
                "car gated on an older main",
                Box::new(|i| i.car_base = Observed::Known("4".repeat(40))),
                "was not gated on main at departure",
            ),
            (
                "another head merged",
                Box::new(move |i| {
                    i.train_parents = Observed::Known(format!("{BASE} {}", "3".repeat(40)))
                }),
                "is not one merge of main at departure and the gated head",
            ),
            (
                "not a merge",
                Box::new(|i| i.train_parents = Observed::Known(BASE.into())),
                "is not one merge of main at departure and the gated head",
            ),
            (
                "an octopus",
                Box::new(move |i| {
                    i.train_parents = Observed::Known(format!("{BASE} {HEAD} {}", "3".repeat(40)))
                }),
                "is not one merge of main at departure and the gated head",
            ),
            (
                "parents unknown",
                Box::new(|i| i.train_parents = Observed::Unknown("no clone".into())),
                "no clone",
            ),
            (
                "base unknown",
                Box::new(|i| i.car_base = Observed::Unknown("merge-base answered 1".into())),
                "merge-base answered 1",
            ),
        ];
        for (name, mutate, needle) in cases {
            let mut i = inputs(&t, &c, &g, &[]);
            i.car_launch = Observed::Known(auto_car_launch_text());
            mutate(&mut i);
            let a = assess(&i, now());
            assert!(!a.qualified() && !a.binds(), "{name}: {:?}", a.refusals);
            assert_eq!(a.record["decision"], "regate", "{name}");
            assert!(
                a.refusals.iter().any(|s| s.contains(needle)),
                "{name}: wanted {needle:?} in {:?}",
                a.refusals
            );
        }
    }

    /// ONLY FULL OR `--auto`, AND ONE MODE THROUGHOUT. A scoped car
    /// (`-p <crate>`) selected by hand and never carries a train; a receipt
    /// whose mode is not the one its launch intent and GATE_MODE record is
    /// not the launch it vouches for; and an `--auto` car covers an `--auto`
    /// train gate only — a train gate that would run full is not.
    #[test]
    fn a_scoped_or_mismatched_mode_never_carries_a_train() {
        let refused = |what: &str, a: Assessment| {
            assert!(!a.qualified(), "{what}: {:?}", a.refusals);
            assert_eq!(a.record["decision"], "regate", "{what}");
        };
        let t = train();
        // An auto receipt behind a full launch.
        let r = auto_receipt();
        let g = gate_run(&r);
        refused(
            "auto receipt, full intent",
            assess(&inputs(&t, &car(&r), &g, &[]), now()),
        );
        // A scoped launch, receipt and request alike.
        let mut r = receipt();
        r["mode"] = json!("scoped");
        r["scope"] = json!("boss-cli");
        let text = launch(HEAD, "feat/x", GATE, "-p boss-cli").to_string();
        let (c, g) = (car(&r), gate_run_launched(&r, "-p boss-cli", &text));
        let mut i = inputs(&t, &c, &g, &[]);
        i.car_launch = Observed::Known(text);
        refused("scoped", assess(&i, now()));
        // An auto car on a train whose gate would run full.
        let r = auto_receipt();
        let (c, g) = (car(&r), auto_gate_run(&r));
        let mut i = inputs(&t, &c, &g, &[]);
        i.car_launch = Observed::Known(auto_car_launch_text());
        i.train_launch = Observed::Known(
            launch(
                "2222222222222222222222222222222222222222",
                "train/20261019-1200",
                "7f0c3a52-0d4e-4d7e-9a35-6d4f5b1e2c11",
                "",
            )
            .to_string(),
        );
        let a = assess(&i, now());
        assert!(
            a.refusals
                .iter()
                .any(|s| s.contains("covers only an --auto train gate")),
            "{:?}",
            a.refusals
        );
        refused("full train", a);
    }

    /// THE CAR'S RUNNER IS THE ONE ITS TREE CARRIES. The ConfigMap the
    /// gate runs is written from main by the converge, so a car gated
    /// before a run.sh change converged ran another runner than its own
    /// tree names — measured on car 5ce50347 (2026-11-03): its receipt
    /// names run.sh 7035e6aa (main f087319b's) while its tree, on main
    /// d6d2df23, carries f339da37, which its train's gate then ran. That
    /// train re-gates; the comparison is knowable without the ConfigMap.
    #[test]
    fn a_car_gated_by_a_runner_its_tree_does_not_carry_requires_the_train_gate() {
        let mut r = auto_receipt();
        r["runner_sha256"] =
            json!("7035e6aaea85f5d58b0a2930127433c77c67eee36a4fa9dacd5b9f02d15670cd");
        let (t, c, g) = (train(), car(&r), auto_gate_run(&r));
        let mut i = inputs(&t, &c, &g, &[]);
        i.car_launch = Observed::Known(auto_car_launch_text());
        i.train_runner = Observed::Known(
            "f339da37ad6a9eb60feeb0757151b893c45ef85a96f8fcdb2d98dbe75689a970".into(),
        );
        let a = assess(&i, now());
        assert!(!a.qualified());
        assert!(
            a.refusals.iter().any(|s| s.starts_with("runner differs")),
            "{:?}",
            a.refusals
        );
    }

    /// AS SHIPPED, THE CONDUCTOR BINDS (backlog 2647cf38). The mode word in
    /// its ConfigMap is the one `mode_from` reads as Bind, so the manifest
    /// and this module cannot disagree about whether reuse is on.
    #[test]
    fn the_conductor_ships_with_reuse_bound() {
        let path = boss_testing::repo_root().join("infra/cluster/manifests/boss-conductor.yaml");
        let text = std::fs::read_to_string(&path).expect("the conductor manifest");
        let values: Vec<&str> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix(&format!("{MODE_ENV}:")))
            .map(|v| v.trim().trim_matches('"'))
            .collect();
        assert_eq!(values.len(), 1, "{MODE_ENV} set once: {values:?}");
        assert_eq!(mode_from(Some(values[0])), (Mode::Bind, None));
    }

    /// THE MECHANISM STAYS: an input a later contract names before it
    /// has a comparison re-gates the very match above, in bind mode too,
    /// and says why. The launch request was the last such input; it is
    /// compared now, so production's list is empty.
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
                Box::new(|i| i.train_runner = Observed::Unknown("no run.sh blob".into())),
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

    /// The modes this module reads are the ones the launcher writes: the
    /// `--auto` argument `normalize_mode` gives and the train gate runs,
    /// and the full launch's recorded word. Nothing else maps.
    #[test]
    fn the_modes_read_are_the_modes_the_launcher_writes() {
        assert_eq!(crate::gate::normalize_mode("auto").unwrap(), AUTO_ARGUMENT);
        assert_eq!(crate::gate::Requester::Train.mode(), AUTO_ARGUMENT);
        assert_eq!(
            launch_of(Some("full")),
            Some(("", crate::gate::RECORDED_FULL_MODE))
        );
        assert_eq!(
            launch_of(Some("auto")),
            Some((AUTO_ARGUMENT, AUTO_ARGUMENT))
        );
        for other in [None, Some(""), Some("scoped"), Some("-p boss-cli")] {
            assert_eq!(launch_of(other), None, "{other:?}");
        }
    }

    /// THE CONFIGMAP HELPER RUNS AFTER THE VERDICT. The first cut refused
    /// on the judged-init layout's `seed-refresh` script as an unreadable
    /// input. As shipped, the gate is an init container and the helper a
    /// regular one, so the helper starts only once the gate has exited:
    /// it maintains the seed later gates start from and cannot move this
    /// run's verdict. If a car moves the helper ahead of the gate, this
    /// fails and the reuse contract is reopened with it.
    #[test]
    fn the_shipped_layout_runs_the_configmap_helper_after_the_verdict() {
        let path = boss_testing::repo_root().join("infra/gate-runner/gate-runner.yaml");
        let text = std::fs::read_to_string(&path).expect("the runner manifest");
        let doc = crate::gate::render_job(
            &text,
            "train/20261017-2139",
            GATE,
            crate::gate::Requester::Train.mode(),
        )
        .expect("renders");
        let lines: Vec<&str> = doc.lines().map(str::trim).collect();
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle) && !l.starts_with('#'))
                .unwrap_or_else(|| panic!("{needle} in the rendered manifest"))
        };
        let (init, gate, regular, helper) = (
            at("initContainers:"),
            at("/repo-script/run.sh"),
            lines
                .iter()
                .position(|l| *l == "containers:")
                .expect("containers:"),
            at("/repo-script/seed-refresh.sh"),
        );
        assert!(
            init < gate && gate < regular && regular < helper,
            "init {init}, gate {gate}, containers {regular}, helper {helper}"
        );
    }

    /// REVIEW 93a363fa (agent-codex, CHANGES on 9085843c), its regression
    /// kept as written: two equal MUTABLE references (postgres:16,
    /// boss-ci:rust1.96, pulled Always) are equal bytes in both launch
    /// requests and prove nothing about the image either pod resolves — a
    /// tag can be repushed between the car's gate and departure. Design
    /// cdf626ac Q1 requires image and toolchain equal before departure.
    #[test]
    fn reviewer_mutable_image_references_without_equal_digest_proof_must_regate() {
        let r = auto_receipt();
        let t = train();
        let c = car(&r);
        let mut car_doc: Value = serde_json::from_str(&auto_car_launch_text()).unwrap();
        let mut train_doc: Value = serde_json::from_str(&train_launch_text()).unwrap();
        for doc in [&mut car_doc, &mut train_doc] {
            doc["spec"]["template"]["spec"]["initContainers"][0]["image"] =
                json!("registry.example/postgres:16");
            doc["spec"]["template"]["spec"]["initContainers"][1]["image"] =
                json!("registry.example/boss-ci:rust1.96");
            doc["spec"]["template"]["spec"]["initContainers"][1]["imagePullPolicy"] =
                json!("Always");
        }
        let car_text = car_doc.to_string();
        let g = gate_run_launched(&r, "--auto", &car_text);
        let mut i = inputs(&t, &c, &g, UNJUDGED_INPUTS);
        i.car_launch = Observed::Known(car_text);
        i.train_launch = Observed::Known(train_doc.to_string());
        let a = assess(&i, now());
        eprintln!("same mutable tags, only old car digest known: {}", a.record);
        assert!(
            !a.binds(),
            "image equality is unproven: mutable tags must not skip the train gate; got {:?}",
            a.refusals
        );
    }

    /// A DIGEST-PINNED REFERENCE IS BOUND TO WHAT THE CAR'S POD RESOLVED,
    /// container by container (backlog 2647cf38, repair of review
    /// 93a363fa). Pinned in both requests but resolved otherwise on the
    /// car's pod, or not recorded for that container at all: the train
    /// gates. One mutation at a time from the qualifying `--auto` case.
    #[test]
    fn a_pinned_image_the_cars_pod_did_not_resolve_requires_the_train_gate() {
        let t = train();
        type Edit = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Edit, &str)> = vec![
            (
                "the gate resolved another digest",
                Box::new(|r| {
                    r["recorded_by"]["image_ids"]["gate"] = json!(format!(
                        "registry.example/boss-ci@sha256:{}",
                        "2".repeat(64)
                    ))
                }),
                "image differs: container gate",
            ),
            (
                "postgres not recorded",
                Box::new(|r| {
                    r["recorded_by"]["image_ids"]
                        .as_object_mut()
                        .unwrap()
                        .remove("postgres");
                }),
                "image unknown: the car's receipt records no resolved image for container postgres",
            ),
        ];
        for (name, edit, needle) in cases {
            let mut r = auto_receipt();
            edit(&mut r);
            let (c, g) = (car(&r), auto_gate_run(&r));
            let mut i = inputs(&t, &c, &g, &[]);
            i.car_launch = Observed::Known(auto_car_launch_text());
            let a = assess(&i, now());
            assert!(!a.binds(), "{name}: {:?}", a.refusals);
            assert!(
                a.refusals.iter().any(|s| s.contains(needle)),
                "{name}: wanted {needle:?} in {:?}",
                a.refusals
            );
        }
    }

    /// AS SHIPPED, THE GATE IMAGES ARE TAGS, so under the bind word reuse
    /// stamps `regate` on every train and binds nothing until backlog
    /// 35a0de38 pins them by digest. When that car lands this fails, and
    /// its reviewer sees that pinning the images is what turns reuse on.
    #[test]
    fn the_shipped_gate_images_are_mutable_so_reuse_cannot_fire_yet() {
        let path = boss_testing::repo_root().join("infra/gate-runner/gate-runner.yaml");
        let text = std::fs::read_to_string(&path).expect("the runner manifest");
        let doc = crate::gate::render_job(
            &text,
            "train/20261017-2139",
            GATE,
            crate::gate::Requester::Train.mode(),
        )
        .expect("renders");
        let mutable: Vec<&str> = doc
            .lines()
            .map(str::trim)
            .map(|l| l.strip_prefix("- ").unwrap_or(l))
            .filter_map(|l| l.strip_prefix("image:"))
            .map(|v| v.trim().trim_matches('"'))
            .filter(|v| !v.contains("@sha256:"))
            .collect();
        assert!(!mutable.is_empty(), "the gate images are pinned now: {doc}");
        for image in mutable {
            assert!(
                image_refusal(image, "gate", &receipt()).is_some_and(|w| w.contains("mutable")),
                "{image}"
            );
        }
    }

    #[test]
    fn the_mode_is_off_unless_a_known_word_says_otherwise() {
        assert_eq!(mode_from(None), (Mode::Off, None));
        assert_eq!(mode_from(Some("")), (Mode::Off, None));
        assert_eq!(mode_from(Some("off")), (Mode::Off, None));
        assert_eq!(mode_from(Some(" shadow ")), (Mode::Shadow, None));
        assert_eq!(mode_from(Some("exact")), (Mode::Bind, None));
        // The full-only word retired with the full-only rule (2647cf38).
        let (m, warn) = mode_from(Some("exact-full"));
        assert_eq!(m, Mode::Off);
        assert!(warn.unwrap().contains("reading it as off"));
        let (m, warn) = mode_from(Some("1"));
        assert_eq!(m, Mode::Off);
        assert!(warn.unwrap().contains("reading it as off"));
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
