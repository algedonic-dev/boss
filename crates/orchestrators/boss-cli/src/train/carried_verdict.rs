//! A gate verdict the RUNNER LEFT and the conductor records.
//!
//! WHY (backlog 934ccad1; design bdc60b65, question `gate-verdict`,
//! decided by David 2026-10-06). The gate runner Job runs a car's branch
//! — its build scripts, its tests, its `infra/gate.sh` — and reported its
//! own verdict to the jobs API with no machine token: GET the packet,
//! PATCH the step, PUT the step (203 facts each from 147 peers in the
//! 72 h window read that day). A pod that runs branch code must never
//! hold the estate token (design c395e62c), so once the machine door
//! enforces, those writes are refused and every gate verdict is lost.
//! The decision: the runner writes nothing; it leaves its receipt where a
//! trusted holder reads it, and the conductor — which holds the gates
//! Role and never runs branch code — records the verdict on the packet.
//!
//! THE CARRIER IS THE POD'S OWN STDOUT. `run.sh` already prints
//! `gate-runner: receipt <one line of JSON>` (the third copy, cf0021ae);
//! it now prints a trailer on the next line carrying the Job's name, the
//! verdict word, the payload's byte count and its sha256. Not the gate
//! volume: every gate mounts `/gate-runs` read-write (review 0b9c02f1,
//! F2; 5e77f216), so a file there can be written by any concurrent
//! branch's code, where a pod's stdout is written only by that pod. Not
//! the termination message: it holds 4096 bytes and a receipt measured
//! 65 KB at the median (77 KB at the largest of the 100 newest closed
//! gate-runs, 2026-10-06). The log lives as long as the Job
//! (`ttlSecondsAfterFinished: 86400`).
//!
//! WHAT THE RECEIPT MAY AND MAY NOT DECIDE. It is written by branch
//! code, so it is input, never authority:
//!   - WHICH PACKET is the Job's `boss.dev/packet` label, written by the
//!     launcher; the pod holds no ServiceAccount token and cannot change
//!     it. Nothing the receipt says about itself names a packet or a
//!     branch.
//!   - WHICH RUN is the trailer's `job=`, which must be the Job whose
//!     log is being read — a frame echoed from another run (a test
//!     fixture replayed into the log, a copied line) is not this Job's.
//!   - COMPLETE OR NOT is the byte count and the digest: a payload the
//!     log cut short does not match its trailer, and a payload with no
//!     trailer is not a receipt.
//!   - THE VERDICT is one of the four words the runner may say. The
//!     payload must be a JSON object within [`MAX_RECEIPT_BYTES`]; it is
//!     stored as a string and nothing in it is executed or followed.
//!
//! A branch could always lie about its OWN verdict — it runs the checks.
//! This does not widen that. WHAT IT NARROWS, AND WHEN (review 47319ee7,
//! S1) — the machine door decides, not this code:
//!   - BEFORE this layout, and today while the door only REPORTS: a gate
//!     pod, with no token, can PATCH and PUT the verdict step of ANY open
//!     gate-run, its own included (and its own before the conductor
//!     reads it: the step is then done and the conductor writes nothing).
//!   - WITH this layout and the door still reporting: exactly the same.
//!     The runner no longer makes those writes; nothing stops branch
//!     code in the pod from making them.
//!   - ONCE THE DOOR ENFORCES: a tokenless pod's write is refused, so a
//!     gate pod can lie only about its own packet, through its own
//!     frame. This layout is what makes enforcing survivable (no verdict
//!     depends on a pod's write); enforcing is what narrows the forgery.
//!
//! WHICH FRAME, AND WHEN: THE TERMINATION MESSAGE SAYS (the runner half
//! of 934ccad1, the car that renders the label). The first version took
//! the FIRST frame the log held for the Job, at once, running or not —
//! so a line printed into the pod's stdout before the runner's own
//! decided the verdict, and decided it while the checks still ran
//! (review 7e5356f7 measured both). Now `run.sh` also writes its trailer
//! line to the gate container's termination message
//! (`/dev/termination-log`, 4096 bytes: room for the trailer, not for the
//! receipt), and this reader:
//!   - records NOTHING until the gate container has ended — the kubelet
//!     publishes the message only then, so no verdict is taken from a
//!     pod whose checks may still be running;
//!   - takes the frame the message NAMES — the same job, verdict, byte
//!     count and sha256 — wherever it sits in the log. A frame printed
//!     before or after it that says anything else is not the receipt;
//!   - settles `lost`, never green and never `failed`, a container that
//!     ended with no message, a message that is not a trailer for this
//!     Job, a message whose receipt the log does not hold whole (cut,
//!     rotated, never printed), and a Job whose pod is gone.
//! The message is a second channel the log's length cannot cut, and it is
//! last-writer-wins, written by `run.sh` after the checks have returned.
//! IT IS STILL NOT AN AUTHOR: the checks run in the same container and
//! uid as `run.sh`, so branch code can write that file as easily as the
//! log, and needs neither — `infra/gate.sh` is the branch's own and may
//! simply write a green receipt. A green no branch can produce needs the
//! checks in another container or uid and a gate.sh the branch does not
//! supply: a different design (review 7e5356f7).
//!
//! LIVE WHERE A JOB CARRIES THE LABEL. Only a Job labelled
//! [`CARRIER_LABEL`]`=`[`CARRIER_POD_LOG`] is read. The manifest is
//! rendered from the LAUNCHER's tree (a builder's branch, the
//! conductor's clone), so a branch cut before the label landed renders
//! none: its runner is handed no `GATE_VERDICT_CARRIER`, reports for
//! itself as before, and is never touched here.

use super::*;

/// The Job label that says "this runner leaves its verdict in its pod
/// log and writes nothing to the jobs API", and its one value.
pub(crate) const CARRIER_LABEL: &str = "boss.dev/verdict-carrier";
pub(crate) const CARRIER_POD_LOG: &str = "pod-log";

/// The largest receipt recorded. Seven times the largest measured
/// (77,354 bytes over the 100 newest closed gate-runs, 2026-10-06), and
/// far under what a step's metadata is asked to hold elsewhere.
pub(crate) const MAX_RECEIPT_BYTES: usize = 512 * 1024;

/// The most pod log read for one Job. The kubelet rotates a container
/// log at 10 Mi by default, so a larger answer is not a gate's log.
pub(crate) const MAX_LOG_BYTES: usize = 32 * 1024 * 1024;

/// How long the carrier keeps a finished run's log: the gate Job's own
/// `ttlSecondsAfterFinished` (gate-runner.yaml). Stated on a `lost`
/// receipt so its reader knows how long the evidence lasts.
pub(crate) const LOG_RETENTION_HOURS: i64 = 24;

/// How long after a Job's finish a missing receipt is still only
/// "not yet" (review e0ecb2f6, question 4). `lost` is irreversible — the
/// step is done, so the real verdict can never land — and a second
/// reader will call this from a 30 s poll, so a finish younger than this
/// waits, and so does a finish the cluster has not dated.
pub(crate) const LOST_FLOOR_SECS: i64 = 60;

/// The bound this process keeps on kubectl itself (review e0ecb2f6, F5):
/// a client that hangs anywhere — a dial that never returns, a slow
/// answer, a stuck exec plugin — is killed at it, because reconcile is
/// the verb that merges trains and this pass runs at the top of every
/// one.
///
/// IT IS THE ONLY BOUND, ON PURPOSE (backlog 06ae925a). The same review
/// also passed `--request-timeout=20s`, kubectl's own, and that flag is a
/// client-config OVERRIDE: with one set and no kubeconfig, kubectl
/// v1.33.3 does not fall back to the pod's ServiceAccount and dials
/// `localhost:8080`. So in the conductor's pod every read of this pass
/// was refused from 2026-10-07T08:56:24Z until this landed, and no gate
/// of the pod-log layout had its verdict recorded.
pub(crate) const KUBECTL_PROCESS_TIMEOUT_SECS: u64 = 30;

/// How often a failure that has not changed is said again: the cluster
/// read's ([`cluster_read_line`]) and a packet's waiting verdict
/// ([`waiting_line`]). The pass runs every two minutes, so "once per
/// outage" kept the journal short — and kept the outage above silent for
/// a day after its first line (backlog 06ae925a). An hour is 24 lines a
/// day from a read that stays dark, against 720 unlatched, and it is no
/// longer than one stalled train waits before somebody looks.
pub(crate) const RESAY_MINUTES: i64 = 60;

/// The job-metadata key the conductor writes on an open gate-run while
/// it cannot read the gate Jobs ([`blocked_note`]), and how long a run
/// must have been quiet before it is told: a gate launched a moment ago
/// owes nobody a verdict yet, and the shortest train gate is minutes.
pub(crate) const RECORDER_BLOCKED_KEY: &str = "verdict_recorder_blocked";
pub(crate) const BLOCKED_NOTE_AFTER_MINS: i64 = 15;

/// How many characters of pod-log text a reason may quote (review
/// e0ecb2f6, F1). Every reason below ends up in a receipt the CONDUCTOR
/// signs, and the text is the branch's: uncapped, one trailer line with
/// an 8 MiB `job=` made an 8 MB `lost` receipt, and unescaped, a
/// no-break space let a sentence of the branch's read as the
/// conductor's own prose.
pub(crate) const QUOTED_CHARS: usize = 80;

/// The most a `lost` receipt may weigh. With every quoted token at its
/// cap it is about a third of this; `lost_receipt` cuts at it anyway, so
/// the ceiling holds whatever a later reason forgets to quote, and a
/// test asserts both the ceiling and that no reason needed the cut.
pub(crate) const MAX_LOST_RECEIPT_BYTES: usize = 4096;

/// The most a termination message may weigh: the kubelet's own cap. A
/// longer one did not come through the kubelet.
pub(crate) const MAX_TERMINATION_BYTES: usize = 4096;

/// The container of the gate pod whose end and message are read.
const GATE_CONTAINER: &str = "gate";

const RECEIPT_PREFIX: &str = "gate-runner: receipt ";
const TRAILER_PREFIX: &str = "gate-runner: receipt-end v1 ";

/// The words a runner may say. `withdrawn` is not one: it is reached
/// only before any pod exists (gate-run.toml).
const RUNNER_VERDICTS: [&str; 4] = ["green", "failed", "lost", "refused"];

/// One gate Job, as the cluster holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateJob {
    pub name: String,
    pub uid: String,
    /// The `boss.dev/packet` label the launcher wrote.
    pub packet: String,
    /// `metadata.creationTimestamp`, RFC 3339 UTC — sorts as text.
    pub created: String,
    /// Does it carry [`CARRIER_LABEL`]`=`[`CARRIER_POD_LOG`]?
    pub carrier: bool,
    /// `None` while neither `succeeded` nor `failed` is set.
    pub finished: Option<Finished>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finished {
    pub failed: bool,
    /// The `Failed`/`Complete` condition's reason and time, when present.
    pub reason: String,
    pub at: String,
}

/// PURE: `kubectl get jobs -l app=gate-runner -o json`, as facts. A Job
/// with no packet label is not a gate anyone can record and is dropped.
pub(crate) fn gate_jobs_from_json(list: &Value) -> Vec<GateJob> {
    let text = |v: &Value, p: &str| {
        v.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let count = |v: &Value, p: &str| v.pointer(p).and_then(Value::as_i64).unwrap_or(0);
    list.get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|j| {
            let labels = j.pointer("/metadata/labels")?;
            let packet = labels.get("boss.dev/packet")?.as_str()?.to_string();
            let (succeeded, failed) = (count(j, "/status/succeeded"), count(j, "/status/failed"));
            let finished = (succeeded > 0 || failed > 0).then(|| {
                let cond = j
                    .pointer("/status/conditions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find(|c| {
                        matches!(
                            c.get("type").and_then(Value::as_str),
                            Some("Failed" | "Complete")
                        )
                    });
                Finished {
                    failed: failed > 0,
                    reason: cond.map(|c| text(c, "/reason")).unwrap_or_default(),
                    at: cond
                        .map(|c| text(c, "/lastTransitionTime"))
                        .unwrap_or_default(),
                }
            });
            Some(GateJob {
                name: text(j, "/metadata/name"),
                uid: text(j, "/metadata/uid"),
                packet,
                created: text(j, "/metadata/creationTimestamp"),
                carrier: labels.get(CARRIER_LABEL).and_then(Value::as_str) == Some(CARRIER_POD_LOG),
                finished,
            })
        })
        .collect()
}

/// One gate Job's pod, as far as the recorder reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GatePod {
    /// How many pods the Job's selector matched. Zero on a finished Job
    /// means the pod is gone (evicted and collected, its node reset, the
    /// Job's deadline deleting it) and nothing of the run can be read.
    pub pods: usize,
    /// The `gate` container's end, once it has one.
    pub ended: Option<Ended>,
    /// What the cluster says of a pod whose gate container has NOT ended:
    /// the pod's phase and reason and the gate container's waiting
    /// reason, as far as each is stated (`Failed, Evicted; gate waiting:
    /// PodInitializing`). Untrusted text, for a `lost` receipt.
    pub seen: String,
}

/// How the `gate` container ended: the kubelet's own account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Ended {
    pub exit_code: i64,
    /// `Completed`, `Error`, `OOMKilled`, … — the kubelet's word.
    pub reason: String,
    /// RFC 3339, the kubelet's.
    pub finished_at: String,
    /// The termination message: what `run.sh` wrote to
    /// `/dev/termination-log`. Untrusted text, capped by the kubelet.
    pub message: String,
}

/// PURE: `kubectl get pods -l job-name=<job> -o json`, as facts. The
/// NEWEST pod is the run (`backoffLimit: 0` makes one; a second would be
/// a retry this manifest does not ask for).
pub(crate) fn gate_pod_from_json(list: &Value) -> GatePod {
    let items: Vec<&Value> = list
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let text = |v: &Value, p: &str| {
        v.pointer(p)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let newest = items
        .iter()
        .max_by_key(|p| text(p, "/metadata/creationTimestamp"));
    let ended = newest.and_then(|pod| {
        let gate = pod
            .pointer("/status/containerStatuses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|c| c.get("name").and_then(Value::as_str) == Some(GATE_CONTAINER))?;
        let t = gate.pointer("/state/terminated")?;
        Some(Ended {
            exit_code: t.get("exitCode").and_then(Value::as_i64).unwrap_or(-1),
            reason: text(t, "/reason"),
            finished_at: text(t, "/finishedAt"),
            message: text(t, "/message"),
        })
    });
    let seen = newest
        .map(|pod| {
            let waiting = pod
                .pointer("/status/containerStatuses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(GATE_CONTAINER))
                .map(|c| text(c, "/state/waiting/reason"))
                .unwrap_or_default();
            [
                text(pod, "/status/phase"),
                text(pod, "/status/reason"),
                if waiting.is_empty() {
                    String::new()
                } else {
                    format!("gate waiting: {waiting}")
                },
            ]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
        })
        .unwrap_or_default();
    GatePod {
        pods: items.len(),
        ended,
        seen,
    }
}

/// PURE: is the pod log worth reading for this pod? Only when the gate
/// container ended and left a message — every other state is decided
/// without it, and a running gate's log is megabytes read for nothing.
pub(crate) fn needs_log(pod: &GatePod) -> bool {
    pod.ended
        .as_ref()
        .is_some_and(|e| !e.message.trim().is_empty())
}

/// PURE: of the Jobs on ONE packet, the one whose log may settle it —
/// or why none may.
///
/// The NEWEST Job is the run the packet is waiting on (a relaunch reuses
/// an open packet). If it is an old-layout runner it reports for itself.
/// And no OTHER Job may still be running: packet e6845e06 was settled
/// from a dead sibling eleven minutes into a live run on the same packet
/// (backlog 53b9a103), the rule the estate observer's settle keeps.
pub(crate) fn carrier_subject(jobs: &[GateJob]) -> std::result::Result<&GateJob, String> {
    let newest = jobs
        .iter()
        .max_by(|a, b| (&a.created, &a.name).cmp(&(&b.created, &b.name)))
        .ok_or_else(|| "no gate Job carries this packet".to_string())?;
    if !newest.carrier {
        return Err(format!(
            "its newest Job {} is an old-layout runner, which records its own verdict",
            newest.name
        ));
    }
    if let Some(live) = jobs
        .iter()
        .find(|j| j.name != newest.name && j.finished.is_none())
    {
        return Err(format!(
            "Job {} is still running on this packet beside {}",
            live.name, newest.name
        ));
    }
    Ok(newest)
}

/// PURE: does this packet still owe a verdict — open, with its
/// `record-verdict` step not yet completed? `Err` says why not. A packet
/// already settled (by an old-layout runner, the observer, a withdrawal,
/// or an earlier pass of this one) is never written again.
pub(crate) fn verdict_owed(run: &Value) -> std::result::Result<(), String> {
    // A Job's label names a job id, and only a gate-run's `record-verdict`
    // is this recorder's to write (review e0ecb2f6, F3).
    let kind = run.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "gate-run" {
        return Err(format!("the packet's kind is `{kind}`, not gate-run"));
    }
    let status = run.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "open" {
        return Err(format!("the packet is {status}, not open"));
    }
    let step = find_step(run, "record-verdict", "Record the gate verdict")
        .ok_or_else(|| "the packet has no record-verdict step".to_string())?;
    if step_done(Some(step)) {
        return Err(format!(
            "its record-verdict step already carries `{}`",
            step.pointer("/metadata/verdict")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ));
    }
    Ok(())
}

/// What one Job's log holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A receipt line and its trailer, for this Job, that agree.
    Complete {
        verdict: String,
        receipt: String,
        sha256: String,
    },
    /// Something receipt-shaped that is not a complete receipt — why.
    Broken(String),
}

/// PURE: pod-log text made safe to quote in a reason — the ONE way
/// branch-derived text enters a `Broken` reason, and so a `lost` receipt.
///
/// At most [`QUOTED_CHARS`] characters; only printable ASCII other than
/// the quote and the backslash passes as itself, and everything else —
/// a space, a control character, anything not ASCII — is written as its
/// `\u{..}` escape, so the quoted text cannot contain a word boundary of
/// its own. It is delimited and labelled, with its true length, so a
/// reader of the receipt can see where the conductor's words stop.
pub(crate) fn quoted(untrusted: &str) -> String {
    let mut shown = String::new();
    for c in untrusted.chars().take(QUOTED_CHARS) {
        if c.is_ascii_graphic() && c != '"' && c != '\\' {
            shown.push(c);
        } else {
            shown.extend(c.escape_unicode());
        }
    }
    let cut = if untrusted.chars().nth(QUOTED_CHARS).is_some() {
        ", cut"
    } else {
        ""
    };
    format!(
        "<untrusted pod-log text, {} byte(s){cut}: \"{shown}\">",
        untrusted.len()
    )
}

/// PURE: the frame the gate container's termination message NAMES.
///
/// The message must be one trailer line, whole, for this Job: the line
/// `run.sh` printed after its receipt and then wrote to
/// `/dev/termination-log`. The log is searched for that exact LINE (a
/// trailer inside a longer line is not one) with the receipt line
/// directly before it, and the pair is judged as any frame is. Every
/// other frame in the log — printed earlier by a test, replayed later by
/// the failed-check replay, forged — is not what the runner ended on.
pub(crate) fn named_frame(log: &str, job: &str, message: &str) -> Frame {
    if message.len() > MAX_TERMINATION_BYTES {
        return Frame::Broken(format!(
            "a termination message of {} bytes, over the {MAX_TERMINATION_BYTES} the kubelet \
             keeps — it is not the runner's trailer",
            message.len()
        ));
    }
    let named = message.strip_suffix('\n').unwrap_or(message);
    let Some(fields) = named.strip_prefix(TRAILER_PREFIX) else {
        return Frame::Broken(format!(
            "a termination message that is not the runner's receipt trailer: {}",
            quoted(named)
        ));
    };
    if named.contains('\n') || named.contains('\r') {
        return Frame::Broken(format!(
            "a termination message of more than one line: {}",
            quoted(named)
        ));
    }
    let t = match trailer_fields(fields) {
        Ok(t) => t,
        Err(why) => return Frame::Broken(format!("the termination message holds {why}")),
    };
    if t.job != job {
        return Frame::Broken(format!(
            "a termination message whose trailer is for another Job: {}",
            quoted(t.job)
        ));
    }
    let mut previous: Option<&str> = None;
    let mut seen: Option<Frame> = None;
    for line in log.lines() {
        if line == named {
            match frame_from(previous, &t) {
                whole @ Frame::Complete { .. } => return whole,
                other => seen = seen.or(Some(other)),
            }
        }
        previous = Some(line);
    }
    let why = match seen {
        Some(Frame::Broken(why)) => why,
        _ => "no line of the log is that trailer".to_string(),
    };
    Frame::Broken(format!(
        "the termination message names a receipt (verdict `{}`, {} bytes, sha256 {}) that the \
         pod log does not hold whole — {why}",
        t.verdict, t.bytes, t.sha256
    ))
}

/// The trailer's four fields, exactly: `job= verdict= bytes= sha256=`.
struct Trailer<'a> {
    job: &'a str,
    verdict: &'a str,
    bytes: usize,
    sha256: &'a str,
}

/// STRICT: each field once, no field this reader does not know, `bytes`
/// a plain number, `sha256` sixty-four lowercase hex digits.
fn trailer_fields(fields: &str) -> std::result::Result<Trailer<'_>, String> {
    let (mut job, mut verdict, mut bytes, mut sha256) = (None, None, None, None);
    for field in fields.split(' ') {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| format!("a receipt trailer field with no `=`: {}", quoted(field)))?;
        let slot = match key {
            "job" => &mut job,
            "verdict" => &mut verdict,
            "bytes" => &mut bytes,
            "sha256" => &mut sha256,
            _ => {
                return Err(format!(
                    "a receipt trailer names a field this reader does not know: {}",
                    quoted(key)
                ));
            }
        };
        if slot.replace(value).is_some() {
            return Err(format!("a receipt trailer states `{key}` twice"));
        }
    }
    fn need<'a>(v: Option<&'a str>, key: &str) -> std::result::Result<&'a str, String> {
        v.ok_or_else(|| format!("a receipt trailer without `{key}`"))
    }
    let (job, verdict, bytes, sha256) = (
        need(job, "job")?,
        need(verdict, "verdict")?,
        need(bytes, "bytes")?,
        need(sha256, "sha256")?,
    );
    if !RUNNER_VERDICTS.contains(&verdict) {
        return Err(format!(
            "a receipt trailer whose verdict {} is not one a runner may say ({})",
            quoted(verdict),
            RUNNER_VERDICTS.join("|")
        ));
    }
    let not_a_number = || {
        format!(
            "a receipt trailer whose bytes {} is not a number",
            quoted(bytes)
        )
    };
    if bytes.is_empty() || !bytes.bytes().all(|b| b.is_ascii_digit()) {
        return Err(not_a_number());
    }
    let bytes: usize = bytes.parse().map_err(|_| not_a_number())?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("a receipt trailer whose sha256 is not sixty-four lowercase hex digits".into());
    }
    Ok(Trailer {
        job,
        verdict,
        bytes,
        sha256,
    })
}

/// The line before a trailer, judged against it.
fn frame_from(previous: Option<&str>, t: &Trailer<'_>) -> Frame {
    use sha2::{Digest, Sha256};
    let Some(payload) = previous.and_then(|l| l.strip_prefix(RECEIPT_PREFIX)) else {
        return Frame::Broken("a receipt trailer with no receipt line before it".into());
    };
    if payload.len() > MAX_RECEIPT_BYTES || t.bytes > MAX_RECEIPT_BYTES {
        return Frame::Broken(format!(
            "a receipt of {} bytes (its trailer says {}), over the {MAX_RECEIPT_BYTES} a packet is \
             asked to keep",
            payload.len(),
            t.bytes
        ));
    }
    if payload.len() != t.bytes {
        return Frame::Broken(format!(
            "a receipt line of {} bytes whose trailer says {} bytes — the log cut it",
            payload.len(),
            t.bytes
        ));
    }
    let digest = hex::encode(Sha256::digest(payload.as_bytes()));
    if digest != t.sha256 {
        return Frame::Broken(format!(
            "a receipt line whose sha256 {digest} is not its trailer's {}",
            t.sha256
        ));
    }
    let Ok(Value::Object(body)) = serde_json::from_str::<Value>(payload) else {
        return Frame::Broken("a receipt that is whole but is not a JSON object".into());
    };
    if let Err(why) = verdicts_agree(t.verdict, body.get("verdict")) {
        return Frame::Broken(why);
    }
    Frame::Complete {
        verdict: t.verdict.to_string(),
        receipt: payload.to_string(),
        sha256: digest,
    }
}

/// PURE: do the trailer's word and the receipt's own `verdict` tell one
/// story (review e0ecb2f6, F2)?
///
/// The step takes the trailer's word and keeps the receipt beside it,
/// and the readers split: `gate::own_verdict` and auto-park read the
/// step, `train_gate::standing` reads the receipt's `refused` first. A
/// verdict with two sources is read by every consumer as both (CLAUDE.md
/// §Diagnosis), so a pair run.sh cannot print is not recorded as either
/// word — it is `Broken`, and a finished Job's is `lost`, by name.
///
/// WHAT run.sh CAN PRINT, read from the script and not assumed:
///   - the same word twice (run.sh takes VERDICT from the receipt);
///   - `failed` over a receipt that says `refused`: the failure-detail
///     block rewrites the receipt to a refusal AFTER the word is taken,
///     when every failed check could not reach the network — the shape
///     the strike rule and the yard read as infrastructure;
///   - `failed`, `lost` or `refused` over a receipt whose `verdict` is
///     absent or is not a runner's word (`unreadable`): the fallback for
///     a receipt gate.sh never wrote or wrote unparseably.
/// NOT `green` over anything but `green`. run.sh's exit-status fallback
/// can say it — gate.sh exited 0 and left no readable receipt — and that
/// green vouches for a head and checks nobody can read; recorded by a
/// trusted hand it would park a car. It is refused here, which is
/// stricter than the runner's own report was.
fn verdicts_agree(trailer: &str, receipt: Option<&Value>) -> std::result::Result<(), String> {
    let own = receipt.and_then(Value::as_str);
    let runner_word = own.is_some_and(|w| RUNNER_VERDICTS.contains(&w));
    let agree = match (trailer, own) {
        (t, Some(w)) if t == w => true,
        ("failed", Some("refused")) => true,
        ("green", _) => false,
        _ => !runner_word,
    };
    if agree {
        return Ok(());
    }
    Err(format!(
        "a receipt whose trailer says `{trailer}` while the receipt's own verdict is {} — two \
         readers of this packet would read two verdicts, so neither is recorded",
        match (own, receipt) {
            (Some(w), _) => quoted(w),
            (None, Some(_)) => "not a string".to_string(),
            (None, None) => "absent".to_string(),
        }
    ))
}

/// What the recorder does with one packet this pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Carried {
    /// Record the runner's verdict with its receipt, annotated.
    Record { verdict: String, receipt: String },
    /// The Job ended and left no complete receipt: `lost`, with why.
    Lost { receipt: String },
    /// Nothing to record yet — why.
    Wait(String),
}

/// PURE: the decision for one packet that owes a verdict, from the Job
/// that carries it, that Job's pod, and (when [`needs_log`]) its log.
///
/// THE ORDER: the pod first. No gate container that has ended, nothing
/// recorded — a frame in a running pod's log is not a verdict yet. Ended,
/// its termination message names the frame; no message, no verdict.
/// `lost` is irreversible, so each `lost` waits the floor.
pub(crate) fn judge(
    run: &Value,
    job: &GateJob,
    pod: &GatePod,
    log: &str,
    namespace: &str,
    now: DateTime<Utc>,
) -> Carried {
    let observed_at = crate::gate::stamp(now);
    let lost = |found: String| match lost_floor(job, pod, now) {
        Err(why) => Carried::Wait(why),
        Ok(()) => Carried::Lost {
            receipt: lost_receipt(run, job, &found, namespace),
        },
    };
    let Some(ended) = &pod.ended else {
        if pod.pods == 0 && job.finished.is_some() {
            return lost(
                "the Job has finished and its pod no longer exists, so neither the gate \
                 container's termination message nor its log can be read"
                    .into(),
            );
        }
        // FINISHED, ITS POD STILL LISTED, AND NO END ON THE GATE CONTAINER
        // (review 47319ee7, B2): the pod failed in its sidecar phase, or
        // was evicted before the gate container started. The Job's finish
        // is the cluster's own statement that nothing is running, so this
        // is not "still running" — and the estate observer leaves carrier
        // Jobs to this recorder, so nothing else would settle it before
        // the three-hour clock.
        if job.finished.is_some() {
            return lost(format!(
                "the Job has finished and its pod is still listed ({}), but its gate container \
                 never reached `terminated` — the pod ended before or outside the gate \
                 container, so the runner left no verdict",
                if pod.seen.is_empty() {
                    "the cluster states no phase for it".to_string()
                } else {
                    quoted(&pod.seen)
                }
            ));
        }
        return Carried::Wait(format!(
            "Job {} is still running: its gate container has not ended, and a verdict is read \
             only from a container that has",
            job.name
        ));
    };
    let how = format!(
        "the gate container ended with exit code {}{}",
        ended.exit_code,
        if ended.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", quoted(&ended.reason))
        }
    );
    if ended.message.trim().is_empty() {
        return lost(format!(
            "{how} and left no termination message — it died before the runner left a verdict"
        ));
    }
    let (verdict, payload, sha256) = match named_frame(log, &job.name, &ended.message) {
        Frame::Complete {
            verdict,
            receipt,
            sha256,
        } => (verdict, receipt, sha256),
        Frame::Broken(why) => return lost(format!("{how}; {why}")),
    };
    // Parsed once already by `frame_from`; an object by construction.
    let mut body: Map<String, Value> = match serde_json::from_str(&payload) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let launched = metadata_map(run)
        .get("sha")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let head = body.get("head").and_then(Value::as_str);
    // WHO RECORDED THIS, AND FROM WHAT — the recorder's own facts, beside
    // the runner's and never in place of them. `launched_sha` is the
    // packet's (the launcher's), not the receipt's `head`: the two differ
    // when a branch moved between the launch and the runner's clone, and
    // `boss gate` already takes the runner's clone as the tree that was
    // judged (gate.rs corrects the packet's sha from the receipt), so a
    // difference is stated here rather than refused.
    body.insert(
        "recorded_by".into(),
        json!({
            "recorder": "boss train cadence (conductor reconcile)",
            "carrier": CARRIER_POD_LOG,
            // COPIED, NOT RETYPED — and from where. The verdict and every
            // field of the receipt are the runner's frame; nothing here
            // was judged by the recorder.
            "copied_from": format!(
                "the receipt frame in the pod log of Job {} that its gate container's \
                 termination message names",
                job.name
            ),
            // The kubelet's account of the end, beside the runner's word:
            // a fact, never a second verdict (a green whose container was
            // killed during the seed refresh is still that green).
            "exit_code": ended.exit_code,
            "ended": ended.reason,
            "ended_at": ended.finished_at,
            "job": job.name,
            "job_uid": job.uid,
            "bytes": payload.len(),
            "sha256": sha256,
            "launched_sha": launched,
            "head_matches_launch": head.map(|h| !launched.is_empty() && h == launched),
            "observed_at": observed_at,
        }),
    );
    Carried::Record {
        verdict,
        receipt: Value::Object(body).to_string(),
    }
}

/// PURE: has this run been over for [`LOST_FLOOR_SECS`]? `Err` says why
/// a missing receipt is not yet `lost`: the end is too young, or nothing
/// has put a readable time on it — and an end nobody dated is not one to
/// settle an irreversible verdict on. The Job's finish when the cluster
/// dated it (the later of the two instants), the gate container's
/// otherwise.
fn lost_floor(job: &GateJob, pod: &GatePod, now: DateTime<Utc>) -> std::result::Result<(), String> {
    let ended = [
        job.finished.as_ref().map(|f| f.at.as_str()),
        pod.ended.as_ref().map(|e| e.finished_at.as_str()),
    ]
    .into_iter()
    .flatten()
    .find_map(|at| DateTime::parse_from_rfc3339(at).ok());
    let Some(finished) = ended else {
        return Err(format!(
            "Job {} has ended but the cluster has put no readable time on its end, so a \
             missing receipt is not yet lost",
            job.name
        ));
    };
    let age = (now - finished.with_timezone(&Utc)).num_seconds();
    if age < LOST_FLOOR_SECS {
        return Err(format!(
            "Job {} ended {age}s ago, inside the {LOST_FLOOR_SECS}s a missing receipt waits \
             before it is lost",
            job.name
        ));
    }
    Ok(())
}

/// The receipt a finished Job with no complete receipt is settled with:
/// what was read, what it means, and where the rest of the evidence is
/// and for how long. Prose, as the conductor's other `lost` settles are.
fn lost_receipt(run: &Value, job: &GateJob, found: &str, namespace: &str) -> String {
    let branch = metadata_map(run)
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("(no branch)")
        .to_string();
    let ended = match &job.finished {
        Some(f) => format!(
            "{}{}{}",
            if f.failed { "Failed" } else { "Complete" },
            if f.reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", f.reason)
            },
            if f.at.is_empty() {
                String::new()
            } else {
                format!(" at {}", f.at)
            }
        ),
        None => "not finished".into(),
    };
    let receipt = format!(
        "NO VERDICT WAS PRODUCED. Gate Job {name} (uid {uid}, namespace {namespace}) ended \
         {ended} and left no verdict the recorder may copy: {found}. The runner leaves its \
         verdict in its pod log, named by its container's termination message, and writes \
         nothing to the system of record, so with no complete receipt there is no verdict to \
         record. Settled as LOST by the conductor's reconcile: \
         this run says nothing about {branch}, and an infrastructure death is not a consist \
         failure. What the runner did say is in `kubectl -n {namespace} logs job/{name} -c gate`, \
         kept for {LOG_RETENTION_HOURS}h after the Job ended. Re-gate for a real verdict — with \
         its --park-* flags (or --park-file), because a closed packet is not reused.",
        name = job.name,
        uid = job.uid,
    );
    if receipt.len() <= MAX_LOST_RECEIPT_BYTES {
        return receipt;
    }
    // THE CEILING, whatever was quoted (review e0ecb2f6, F1). Cut on a
    // character boundary, and said: a receipt that ends mid-sentence
    // with no word about why reads as a write that failed.
    const CUT: &str = " … [cut: this receipt is capped]";
    let mut end = MAX_LOST_RECEIPT_BYTES - CUT.len();
    while !receipt.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT}", &receipt[..end])
}

/// The cluster, as the recorder needs it — a port, so every decision is
/// driven by a test and production is `kubectl` under the gates Role.
/// Async, because both reads wait on the network and the pass runs
/// inside reconcile's runtime (review e0ecb2f6, F5).
#[async_trait]
pub(crate) trait GateCluster: Send + Sync {
    async fn gate_jobs(&self, namespace: &str) -> Result<Vec<GateJob>>;
    async fn gate_pod(&self, namespace: &str, job: &str) -> Result<GatePod>;
    async fn gate_log(&self, namespace: &str, job: &str) -> Result<String>;
}

/// `kubectl`, through the gates Role the conductor's ServiceAccount is
/// bound to (jobs get/list, pods get/list, pods/log get).
///
/// BOUNDED TWICE, AND OFF THE RUNTIME (review e0ecb2f6, F5). Until this
/// pass, reconcile reached kubectl only for an orphaned run; now it does
/// at the top of every pass, and the first version called
/// `std::process::Command::output()` there — a blocking wait on a worker
/// of the runtime, bounded by nothing this code set. So: the child is a
/// `tokio::process` one, awaited rather than blocked on; and the wait
/// itself is under `tokio::time::timeout`, with `kill_on_drop`, so a
/// client that hangs is killed and costs the pass
/// [`KUBECTL_PROCESS_TIMEOUT_SECS`] and nothing more.
///
/// NO CLIENT-CONFIG FLAG BUT `-n` (backlog 06ae925a). This adapter has no
/// kubeconfig: it reaches the cluster by kubectl's in-cluster fallback,
/// which kubectl abandons when any override flag is set. Measured in the
/// conductor's pod, 2026-10-08, kubectl v1.33.3, `get jobs -o name`:
/// `--request-timeout` and `--as` dial `localhost:8080`, `--server`
/// prompts for a username, `--kubeconfig`, `--context`, `--cluster` and
/// `--user` need the file or entry they name; `--namespace`,
/// `--certificate-authority`, `--tls-server-name` and
/// `--insecure-skip-tls-verify` answered. The pin beside the tests
/// refuses the whole family rather than keep that list current.
/// (`consist_job.rs` passes `--request-timeout` safely: it states its
/// own `--kubeconfig`, so nothing falls back.)
pub(crate) struct Kubectl {
    /// The program run — `kubectl`; a test names a stand-in.
    pub program: String,
    /// The bound on one whole invocation.
    pub bound: std::time::Duration,
}

impl Default for Kubectl {
    fn default() -> Self {
        Self {
            program: "kubectl".into(),
            bound: std::time::Duration::from_secs(KUBECTL_PROCESS_TIMEOUT_SECS),
        }
    }
}

impl Kubectl {
    /// One bounded read: stdout on success, the client's own words on a
    /// refusal, and a line naming the bound when it ran out.
    async fn read(&self, namespace: &str, args: &[&str]) -> Result<Vec<u8>> {
        let what = args.join(" ");
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(["-n", namespace])
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = match tokio::time::timeout(self.bound, cmd.output()).await {
            Ok(out) => out.with_context(|| format!("running {} {what}", self.program))?,
            Err(_) => bail!(
                "{} {what} did not answer within {}s and was killed",
                self.program,
                self.bound.as_secs_f32()
            ),
        };
        if !out.status.success() {
            bail!(
                "{} {what} failed: {}",
                self.program,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }
}

#[async_trait]
impl GateCluster for Kubectl {
    async fn gate_jobs(&self, namespace: &str) -> Result<Vec<GateJob>> {
        let out = self
            .read(
                namespace,
                &["get", "jobs", "-l", "app=gate-runner", "-o", "json"],
            )
            .await?;
        let list: Value =
            serde_json::from_slice(&out).context("kubectl get jobs did not answer JSON")?;
        Ok(gate_jobs_from_json(&list))
    }

    async fn gate_pod(&self, namespace: &str, job: &str) -> Result<GatePod> {
        let out = self
            .read(
                namespace,
                &[
                    "get",
                    "pods",
                    "-l",
                    &format!("job-name={job}"),
                    "-o",
                    "json",
                ],
            )
            .await?;
        let list: Value =
            serde_json::from_slice(&out).context("kubectl get pods did not answer JSON")?;
        if list.get("items").and_then(Value::as_array).is_none() {
            bail!("kubectl get pods -l job-name={job} answered no `items` list");
        }
        Ok(gate_pod_from_json(&list))
    }

    async fn gate_log(&self, namespace: &str, job: &str) -> Result<String> {
        let out = self
            .read(
                namespace,
                &["logs", &format!("job/{job}"), "-c", "gate", "--tail=-1"],
            )
            .await?;
        if out.len() > MAX_LOG_BYTES {
            bail!(
                "the log of job/{job} is {} bytes, over the {MAX_LOG_BYTES} this reader takes",
                out.len()
            );
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// PURE: what makes two failures of the cluster read the SAME failure —
/// the first line of the client's words with every run of digits and of
/// blanks folded to one mark. kubectl's first line is a klog record that
/// opens with the time and its pid (`E1007 08:56:24.143705      69
/// memcache.go:265] …`), so the words themselves differ on every pass of
/// one unchanged outage; folded, they differ only when the cause does.
pub(crate) fn cause_of(why: &str) -> String {
    let mut out = String::new();
    let mut last = ' ';
    for c in why.lines().next().unwrap_or_default().chars().take(400) {
        let c = match c {
            c if c.is_ascii_digit() => '#',
            c if c.is_whitespace() => ' ',
            c => c,
        };
        if !(matches!(c, '#' | ' ') && c == last) {
            out.push(c);
        }
        last = c;
    }
    out.trim().to_string()
}

/// What the latch file holds: when the outage was first seen, when it
/// was last said, and the cause last said.
struct Outage {
    since: String,
    said: Option<DateTime<Utc>>,
    cause: Option<String>,
}

/// The latch, read. Two shapes: this version's — `since=<t> said=<t>`,
/// then `cause=<folded first line>`, then the failure's words — and the
/// one written before it, `<t> <words>`, which is the shape of the file
/// the outage of 2026-10-07 left on the conductor's volume. That one
/// has no `said`, so a read still failing when this lands is said at
/// once, and its first-seen time is kept either way.
fn outage_from(text: &str) -> Outage {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let Some(rest) = first.strip_prefix("since=") else {
        return Outage {
            since: first.split_whitespace().next().unwrap_or_default().into(),
            said: None,
            cause: None,
        };
    };
    let mut words = rest.split_whitespace();
    let since = words.next().unwrap_or_default().to_string();
    let said = words
        .next()
        .and_then(|w| w.strip_prefix("said="))
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc));
    let cause = lines
        .next()
        .and_then(|l| l.strip_prefix("cause="))
        .map(str::to_string);
    Outage { since, said, cause }
}

/// The outage a latch names — when it was first seen and its folded
/// cause — for the note the conductor leaves on the gate-runs it is
/// keeping waiting ([`blocked_note`]). `None` when no read is failing,
/// or the latch could not be written.
pub(crate) fn outage(latch: &Path) -> Option<(String, String)> {
    let o = outage_from(&fs::read_to_string(latch).ok()?);
    Some((o.since, o.cause?))
}

/// PURE but for the one file it keeps: what, if anything, the pass says
/// about a cluster read that failed or recovered — when it starts, AGAIN
/// every [`RESAY_MINUTES`] while it lasts, at once when its cause
/// changes, and when it ends. Not once per pass (review e0ecb2f6, F5),
/// and not once per outage either (backlog 06ae925a).
///
/// Every reconcile is its own process (the cadence loop spawns the verb),
/// so "already said" cannot live in memory: it is a latch file in the
/// conductor's home, written on the first failure with that failure's
/// time and words, and removed on the first read that works.
///
/// ONCE PER OUTAGE WAS A DAY OF SILENCE. The first version said a failure
/// when it began and nothing until it ended. The read then failed on
/// EVERY pass from the day the recorder landed (the request-timeout flag,
/// `Kubectl`), so the one line was printed on 2026-10-07T08:56Z, scrolled
/// away, and a day of passes that recorded no verdict said nothing: a
/// check nobody reads. The latch would have hidden a second, different
/// failure behind the first just the same. So every line of a continuing
/// outage carries the time it was FIRST seen, which the latch keeps
/// through every rewrite, and the recovery line states it.
///
/// A latch that cannot be written or removed costs only the dedup: the
/// line is printed, which is the safe direction.
pub(crate) fn cluster_read_line(
    latch: &Path,
    failure: Option<&str>,
    now: DateTime<Utc>,
) -> Option<String> {
    let held = fs::read_to_string(latch).ok().map(|t| outage_from(&t));
    let write = |since: &str, why: &str| {
        let _ = latch.parent().map(fs::create_dir_all);
        let _ = fs::write(
            latch,
            format!(
                "since={since} said={}\ncause={}\n{why}\n",
                crate::gate::stamp(now),
                cause_of(why)
            ),
        );
    };
    let tail = format!(
        "non-fatal; said again every {RESAY_MINUTES} min and when the cause changes, until a \
         read works — {}",
        latch.display()
    );
    match (failure, held) {
        (Some(why), None) => {
            write(&crate::gate::stamp(now), why);
            Some(format!(
                "reconcile: the gate Jobs could not be read, so no verdict is recorded from a \
                 pod log this pass ({tail}): {why}"
            ))
        }
        (Some(why), Some(held)) => {
            let changed = held.cause.as_deref().filter(|was| *was != cause_of(why));
            let due = held
                .said
                .is_none_or(|said| (now - said).num_minutes() >= RESAY_MINUTES);
            if changed.is_none() && !due {
                return None;
            }
            write(&held.since, why);
            let how = match changed {
                Some(was) => format!("and the cause CHANGED — it was: {was}"),
                None => "said again on the interval".to_string(),
            };
            Some(format!(
                "reconcile: the gate Jobs STILL cannot be read — failing since {}, {how}; no \
                 verdict has been recorded from a pod log in that time ({tail}): {why}",
                held.since
            ))
        }
        (None, Some(held)) => {
            let _ = fs::remove_file(latch);
            let lasted = DateTime::parse_from_rfc3339(&held.since)
                .ok()
                .map(|t| (now - t.with_timezone(&Utc)).num_minutes())
                .map(|m| format!(" ({}h{:02}m)", m / 60, m % 60))
                .unwrap_or_default();
            Some(format!(
                "reconcile: the gate Jobs are readable again — the read had been failing since \
                 {}{lasted}",
                held.since
            ))
        }
        (None, None) => None,
    }
}

/// PURE: the change, if any, to the note an open gate-run carries about
/// a recorder that cannot read the cluster — `Some(note)` to set it,
/// `Some(Null)` to remove it, `None` to leave the packet alone.
///
/// THE FAILURE MUST REACH A READER (backlog 06ae925a). A line in the
/// conductor's pod log is read by nobody waiting on a gate; the packet
/// is what `boss gate --wait` and the yard hold. So while the gate Jobs
/// cannot be read (`blocked` names the outage: first seen, folded
/// cause), every gate-run that still owes a verdict and has been quiet
/// [`BLOCKED_NOTE_AFTER_MINS`] is told so, once per outage and cause —
/// the merge door records an event per write, so a note that already
/// says this is not written again. When the read works (`blocked:
/// None`), a note still standing is removed.
///
/// It says what is known and no more: the Jobs cannot be listed, so
/// whether THIS run's runner leaves its verdict in a pod log is not
/// known here. An old-layout runner reports for itself and is unaffected.
pub(crate) fn blocked_note(
    run: &Value,
    blocked: Option<(&str, &str)>,
    quiet_minutes: Option<i64>,
    now: DateTime<Utc>,
) -> Option<Value> {
    let held = run
        .pointer(&format!("/metadata/{RECORDER_BLOCKED_KEY}"))
        .filter(|v| !v.is_null());
    let Some((since, cause)) = blocked else {
        return held.map(|_| Value::Null);
    };
    if verdict_owed(run).is_err() || quiet_minutes.is_none_or(|m| m < BLOCKED_NOTE_AFTER_MINS) {
        return None;
    }
    let same = held.is_some_and(|h| {
        h.get("since").and_then(Value::as_str) == Some(since)
            && h.get("cause").and_then(Value::as_str) == Some(cause)
    });
    (!same).then(|| {
        serde_json::json!({
            "since": since,
            "cause": cause,
            "noted_at": crate::gate::stamp(now),
            "says": "The conductor cannot read the gate Jobs, so a verdict a gate runner left \
                     in its pod log is NOT being recorded. This run's verdict may be waiting \
                     there (kubectl logs job/<its gate Job> -c gate keeps it for a day). \
                     Removed by the conductor when the read works.",
        })
    })
}

/// PURE: the line a reader of a gate-run shows for [`blocked_note`] —
/// `boss gate --wait` prints it while the packet is silent.
pub(crate) fn recorder_blocked_line(run: &Value) -> Option<String> {
    let note = run
        .pointer(&format!("/metadata/{RECORDER_BLOCKED_KEY}"))
        .filter(|v| v.is_object())?;
    let text = |k: &str| note.get(k).and_then(Value::as_str).unwrap_or("?");
    Some(format!(
        "verdict waiting: the recorder cannot read the cluster — the conductor's read of the \
         gate Jobs has been failing since {} ({})",
        text("since"),
        text("cause")
    ))
}

/// What, if anything, the pass says about a packet whose verdict is
/// WAITING ON A READ — said once per state and again every
/// [`RESAY_MINUTES`] while it stays so, not once per pass (review
/// 47319ee7, N1). The pass runs every two minutes and each of these
/// lines was printed on every one, per packet, for as long as the read
/// kept failing: up to ninety lines before the three-hour clock. Said
/// only once, it was the same day-long silence as the cluster read's,
/// three hours at a time (backlog 06ae925a).
///
/// The same shape as [`cluster_read_line`], for the same reason (every
/// reconcile is its own process): one small file per packet under
/// `dir`, holding the `state` last said, and its modification time is
/// when. `Some(line)` when the state is new or was last said an interval
/// ago, `None` otherwise; `state: None` — the packet is settled, or the
/// read worked — forgets it. A packet id that is not a plain id is never
/// made a file name: its line is simply printed.
pub(crate) fn waiting_line(
    dir: &Path,
    packet: &str,
    state: Option<(&str, String)>,
) -> Option<String> {
    let plain = !packet.is_empty()
        && packet.len() <= 64
        && packet
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let latch = dir.join(packet);
    match state {
        None => {
            if plain {
                let _ = fs::remove_file(&latch);
            }
            None
        }
        Some((kind, line)) => {
            // A time that cannot be read, or one in the future, is not
            // "recently": the line is printed.
            let recent = || {
                fs::metadata(&latch)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age.as_secs() < RESAY_MINUTES as u64 * 60)
            };
            if plain && fs::read_to_string(&latch).ok().as_deref() == Some(kind) && recent() {
                return None;
            }
            if plain {
                let _ = fs::create_dir_all(dir);
                let _ = fs::write(&latch, kind);
            }
            Some(format!(
                "{line} (said again every {RESAY_MINUTES} min while this stays so; the pass \
                 keeps trying)"
            ))
        }
    }
}

/// What the tests of this recorder and of its adapter (conductor.rs)
/// build a pod from.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// The first trailer line a log holds — what an honest `run.sh`
    /// writes to its termination message — or nothing.
    pub(crate) fn first_trailer(log: &str) -> String {
        log.lines()
            .find(|l| l.starts_with(TRAILER_PREFIX))
            .unwrap_or_default()
            .to_string()
    }

    /// A pod whose gate container ended on `message`.
    pub(crate) fn ended_on(message: &str, exit_code: i64, reason: &str) -> GatePod {
        GatePod {
            pods: 1,
            ended: Some(Ended {
                exit_code,
                reason: reason.into(),
                finished_at: String::new(),
                message: message.into(),
            }),
            seen: String::new(),
        }
    }

    /// A pod that ended as an honest runner of this log would: its
    /// message is the trailer the log holds, or empty when it holds none.
    pub(crate) fn ended(log: &str) -> GatePod {
        ended_on(&first_trailer(log), 0, "Completed")
    }

    /// A pod whose gate container is still running.
    pub(crate) fn running() -> GatePod {
        GatePod {
            pods: 1,
            ended: None,
            seen: "Running".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use sha2::{Digest, Sha256};

    /// The frame an honest runner of this log ended on: the one its
    /// first trailer names (an empty message, when the log holds none).
    fn parse_frame(log: &str, job: &str) -> Frame {
        named_frame(log, job, &first_trailer(log))
    }

    const JOB: &str = "gate-fix-x-abc12";
    const HEAD: &str = "26b7e3081a52923879dd07cda85c2f3f74e7abdf";

    fn trailer(job: &str, verdict: &str, payload: &str) -> String {
        format!(
            "gate-runner: receipt-end v1 job={job} verdict={verdict} bytes={} sha256={}",
            payload.len(),
            hex::encode(Sha256::digest(payload.as_bytes()))
        )
    }

    fn framed(job: &str, verdict: &str, payload: &str) -> String {
        format!(
            "gate-runner: disk /dev/nvme1n1 ...\ngate-runner: receipt {payload}\n{}\n=== gate-runner: replaying failed checks from gate.log ===\n",
            trailer(job, verdict, payload)
        )
    }

    fn green() -> String {
        json!({"verdict": "green", "head": HEAD, "mode": "auto", "dirty": false,
               "checks": [{"name": "fmt", "result": "pass", "seconds": 3}], "fails": []})
        .to_string()
    }

    fn red() -> String {
        json!({"verdict": "failed", "head": HEAD,
               "checks": [{"name": "test", "result": "fail", "seconds": 400}],
               "fails": ["test: boss-jobs::a_thing - FAILED"],
               "fails_excerpt": {"test": "thread 'a_thing' panicked at src/x.rs:9"}})
        .to_string()
    }

    fn job(finished: Option<bool>) -> GateJob {
        GateJob {
            name: JOB.into(),
            uid: "uid-1".into(),
            packet: "run-1".into(),
            created: "2026-10-06T19:00:00Z".into(),
            carrier: true,
            finished: finished.map(|failed| Finished {
                failed,
                reason: if failed {
                    "BackoffLimitExceeded".into()
                } else {
                    String::new()
                },
                at: "2026-10-06T19:40:00Z".into(),
            }),
        }
    }

    /// The instant every `judge` below is asked at: one minute after
    /// the fixture Job's finish, which is exactly the lost floor.
    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T19:41:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn open_run() -> Value {
        json!({
            "id": "run-1", "kind": "gate-run", "status": "open",
            "metadata": {"branch": "fix/x", "sha": HEAD},
            "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                       "title": "Record the gate verdict", "status": "ready", "metadata": {}}]
        })
    }

    #[test]
    fn a_complete_frame_is_read_whole() {
        let payload = red();
        let got = parse_frame(&framed(JOB, "failed", &payload), JOB);
        assert_eq!(
            got,
            Frame::Complete {
                verdict: "failed".into(),
                receipt: payload.clone(),
                sha256: hex::encode(Sha256::digest(payload.as_bytes())),
            }
        );
    }

    /// The log cut the payload short (a rotation, a reader that stopped):
    /// the trailer's count and digest no longer match, so it is not a
    /// receipt — and it is told apart from one that never appeared.
    #[test]
    fn a_truncated_receipt_is_not_a_receipt() {
        let payload = green();
        let cut = &payload[..payload.len() - 7];
        let log = format!(
            "gate-runner: receipt {cut}\n{}\n",
            trailer(JOB, "green", &payload)
        );
        let Frame::Broken(why) = parse_frame(&log, JOB) else {
            panic!("a cut payload must not read as complete");
        };
        assert!(why.contains("the log cut it"), "{why}");
        // The payload with no trailer at all, and so no termination
        // message: the runner died between the two lines.
        let bare = format!("gate-runner: receipt {payload}\n");
        let Carried::Lost { receipt } = judge(
            &open_run(),
            &job(Some(true)),
            &ended(&bare),
            &bare,
            "boss-dev",
            at(),
        ) else {
            panic!("a receipt line with no trailer must not read as complete");
        };
        assert!(receipt.contains("no termination message"), "{receipt}");
        // Same length, different bytes: only the digest can tell.
        let swapped = payload.replace("green", "greeN");
        let log = format!(
            "gate-runner: receipt {swapped}\n{}\n",
            trailer(JOB, "green", &payload)
        );
        let Frame::Broken(why) = parse_frame(&log, JOB) else {
            panic!("a payload that does not match its digest must not read as complete");
        };
        assert!(why.contains("is not its trailer's"), "{why}");
    }

    #[test]
    fn an_oversized_receipt_is_refused_by_its_size() {
        let payload = json!({"verdict": "green", "head": HEAD,
                             "pad": "x".repeat(MAX_RECEIPT_BYTES)})
        .to_string();
        let Frame::Broken(why) = parse_frame(&framed(JOB, "green", &payload), JOB) else {
            panic!("an oversized receipt must not be recorded");
        };
        assert!(why.contains(&MAX_RECEIPT_BYTES.to_string()), "{why}");
    }

    /// The trailer names the Job it was printed in. A frame for another
    /// Job — a fixture a failed test echoed into the replay, a line
    /// copied from another run — is not this Job's receipt.
    #[test]
    fn a_frame_naming_another_job_is_not_this_jobs() {
        let payload = green();
        assert_eq!(
            parse_frame(&framed("gate-other-zzzzz", "green", &payload), JOB),
            Frame::Broken(
                "a termination message whose trailer is for another Job: <untrusted pod-log \
                 text, 16 byte(s): \"gate-other-zzzzz\">"
                    .into()
            )
        );
        // An empty name (a manifest from before NATIVE_JOB_NAME) never
        // matches either.
        assert!(matches!(
            parse_frame(&framed("", "green", &payload), JOB),
            Frame::Broken(_)
        ));
    }

    /// THE FRAME THE TERMINATION MESSAGE NAMES, WHEREVER IT SITS. The
    /// first version took the first frame in the log, so a green printed
    /// before the runner's own `failed` was recorded green (review
    /// 7e5356f7's first probe). run.sh prints its frame and then replays
    /// what failed, and a failed test of this very reader prints
    /// frame-shaped lines too: neither position decides.
    #[test]
    fn the_frame_the_termination_message_names_wins_wherever_it_sits() {
        let (real, forged) = (red(), green());
        let log = format!(
            "gate-runner: receipt {forged}\n{}\n{}gate-runner: receipt {forged}\n{}\n",
            trailer(JOB, "green", &forged),
            framed(JOB, "failed", &real),
            trailer(JOB, "green", &forged)
        );
        let named = trailer(JOB, "failed", &real);
        assert!(
            matches!(named_frame(&log, JOB, &named), Frame::Complete { verdict, receipt, .. }
                if verdict == "failed" && receipt == real),
            "a frame printed before the runner's own decided the verdict"
        );
        // With its newline, as `printf '%s\n'` writes the file.
        assert!(matches!(
            named_frame(&log, JOB, &format!("{named}\n")),
            Frame::Complete { .. }
        ));
        let Carried::Record { verdict, .. } = judge(
            &open_run(),
            &job(Some(true)),
            &ended_on(&named, 1, "Error"),
            &log,
            "boss-dev",
            at(),
        ) else {
            panic!("the named frame is recorded");
        };
        assert_eq!(verdict, "failed");
        // THE RESIDUAL, PINNED SO NOBODY READS THIS AS MORE THAN IT IS:
        // the message is written by the same uid that runs the checks. A
        // branch that writes the termination message too is believed.
        assert!(matches!(
            named_frame(&log, JOB, &trailer(JOB, "green", &forged)),
            Frame::Complete { verdict, .. } if verdict == "green"
        ));
    }

    /// NOTHING IS RECORDED FROM A CONTAINER THAT HAS NOT ENDED (review
    /// 7e5356f7: "a forged green while the Job still runs records green
    /// at once"). The log may hold a whole, well-formed green frame.
    #[test]
    fn a_frame_in_a_running_pods_log_is_not_a_verdict() {
        let log = framed(JOB, "green", &green());
        let Carried::Wait(why) = judge(&open_run(), &job(None), &running(), &log, "boss-dev", at())
        else {
            panic!("a verdict was taken from a pod whose gate container has not ended");
        };
        assert!(why.contains("has not ended"), "{why}");
        // The Job finished over a gate container with no end (B2): lost
        // or waiting by the floor — and never the frame in the log.
        for when in [at() - chrono::Duration::seconds(30), at()] {
            assert!(
                !matches!(
                    judge(
                        &open_run(),
                        &job(Some(false)),
                        &running(),
                        &log,
                        "boss-dev",
                        when
                    ),
                    Carried::Record { .. }
                ),
                "a verdict was taken from a gate container that never ended"
            );
        }
        assert!(!needs_log(&running()), "a running gate's log is not read");
        assert!(needs_log(&ended(&log)));
        assert!(!needs_log(&ended("")));
    }

    /// NO EVIDENCE IS NOT A PASS. A container that ended with no message
    /// (OOM, the deadline, an eviction) left no verdict, whatever its log
    /// says — a whole green frame included — and whatever its exit code.
    #[test]
    fn a_container_that_ended_without_a_message_is_lost_never_green() {
        let log = framed(JOB, "green", &green());
        for (exit, reason) in [(137, "OOMKilled"), (0, "Completed"), (1, "Error")] {
            for message in ["", "\n", "  "] {
                let got = judge(
                    &open_run(),
                    &job(Some(exit != 0)),
                    &ended_on(message, exit, reason),
                    &log,
                    "boss-dev",
                    at(),
                );
                let Carried::Lost { receipt } = got else {
                    panic!("exit {exit} with no termination message was read as {got:?}");
                };
                assert!(
                    receipt.contains(&format!("exit code {exit}"))
                        && receipt.contains(reason)
                        && receipt.contains("no termination message")
                        && receipt.contains("not a consist failure"),
                    "{receipt}"
                );
            }
        }
    }

    /// The message names a receipt by its digest. A log that does not
    /// hold THAT receipt whole — it was rotated out and another frame
    /// remains, the log was cut, the line was never printed — has no
    /// verdict in it, however green the frames it does hold.
    #[test]
    fn a_message_whose_receipt_the_log_does_not_hold_is_lost() {
        let (real, other) = (red(), green());
        let named = trailer(JOB, "failed", &real);
        for log in [
            String::new(),
            framed(JOB, "green", &other),
            // r05: the trailer inside a longer line is not the trailer.
            format!("gate-runner: receipt {real}\ntest stdout: {named}\n"),
            format!("gate-runner: receipt {real}\n{named} trailing\n"),
            // r04: text glued in front of the receipt line.
            format!("xx gate-runner: receipt {real}\n{named}\n"),
            // r08: the receipt line is not the line before the trailer.
            format!("gate-runner: receipt {real}\nsomething between\n{named}\n"),
            // the trailer with nothing before it at all
            format!("{named}\n"),
        ] {
            let Frame::Broken(why) = named_frame(&log, JOB, &named) else {
                panic!("a log that does not hold the named receipt was read:\n{log}");
            };
            assert!(why.contains("does not hold whole"), "{why}");
            assert!(matches!(
                judge(
                    &open_run(),
                    &job(Some(true)),
                    &ended_on(&named, 1, "Error"),
                    &log,
                    "boss-dev",
                    at()
                ),
                Carried::Lost { .. }
            ));
        }
        // The same receipt printed twice is the same receipt.
        let twice = format!("{0}{0}", framed(JOB, "failed", &real));
        assert!(matches!(
            named_frame(&twice, JOB, &named),
            Frame::Complete { .. }
        ));
    }

    /// The message is untrusted text under the kubelet's cap: one line,
    /// one trailer, this Job's.
    #[test]
    fn a_termination_message_that_is_not_one_trailer_for_this_job_names_nothing() {
        let payload = green();
        let log = framed(JOB, "green", &payload);
        let named = trailer(JOB, "green", &payload);
        for (message, must) in [
            (
                "gate passed".to_string(),
                "not the runner's receipt trailer",
            ),
            (format!("{named}\n{named}"), "more than one line"),
            (format!("{named}\r"), "more than one line"),
            (format!(" {named}"), "not the runner's receipt trailer"),
            (
                trailer("gate-other-zzzzz", "green", &payload),
                "for another Job",
            ),
            (
                format!("{named} {}", "x".repeat(MAX_TERMINATION_BYTES)),
                "over the 4096",
            ),
            (format!("{named} packet=other"), "does not know"),
        ] {
            let Frame::Broken(why) = named_frame(&log, JOB, &message) else {
                panic!("read a verdict from the message {message:?}");
            };
            assert!(why.contains(must), "{why}");
        }
    }

    /// A finished Job whose pod is gone can never be read: `lost`, at
    /// the floor, rather than three silent hours (review 7e5356f7,
    /// condition 4, for the case the cluster can answer). A Job still
    /// running with no pod yet is only starting.
    #[test]
    fn a_finished_job_whose_pod_is_gone_is_lost_and_a_starting_one_waits() {
        let gone = GatePod::default();
        let Carried::Lost { receipt } =
            judge(&open_run(), &job(Some(true)), &gone, "", "boss-dev", at())
        else {
            panic!("a finished Job with no pod is settled lost");
        };
        assert!(receipt.contains("its pod no longer exists"), "{receipt}");
        assert!(matches!(
            judge(&open_run(), &job(None), &gone, "", "boss-dev", at()),
            Carried::Wait(_)
        ));
    }

    /// B2 of review 47319ee7. The cluster has FINISHED the Job (failed)
    /// and its pod is still listed, but the gate container never reached
    /// `terminated` — the pod failed in its sidecar phase, or was evicted
    /// before the gate container started. The Job's finish is the
    /// cluster's own statement that nothing is running, so "still
    /// running" is untrue, and since the estate observer leaves carrier
    /// Jobs to this recorder nothing else settles it before the 3 h
    /// clock (it was 15 minutes). `lost`, after the floor, saying what
    /// the pod was seen as. A Job the cluster has NOT finished still waits.
    #[test]
    fn a_finished_job_whose_gate_container_never_ended_is_lost_not_still_running() {
        let never_ended = GatePod {
            pods: 1,
            ended: None,
            seen: "Failed, Evicted, gate waiting: PodInitializing".into(),
        };
        let two_hours_on = at() + chrono::Duration::hours(2);
        let got = judge(
            &open_run(),
            &job(Some(true)),
            &never_ended,
            "",
            "boss-dev",
            two_hours_on,
        );
        let Carried::Lost { receipt } = got else {
            panic!("a finished Job whose gate container never ended was answered {got:?}");
        };
        assert!(
            receipt.contains("never reached `terminated`")
                && receipt.contains(
                    r"Failed,\u{20}Evicted,\u{20}gate\u{20}waiting:\u{20}PodInitializing"
                )
                && receipt.contains("BackoffLimitExceeded")
                && receipt.contains("not a consist failure"),
            "{receipt}"
        );
        assert!(!receipt.contains("still running"), "{receipt}");
        // Inside the floor it waits, and says the floor — not "running".
        let Carried::Wait(why) = judge(
            &open_run(),
            &job(Some(true)),
            &never_ended,
            "",
            "boss-dev",
            at() - chrono::Duration::seconds(30),
        ) else {
            panic!("a finish younger than the floor is not yet lost");
        };
        assert!(why.contains("before it is lost"), "{why}");
        // The Job NOT finished: the container may yet start. Still waits.
        let Carried::Wait(why) = judge(
            &open_run(),
            &job(None),
            &never_ended,
            "",
            "boss-dev",
            two_hours_on,
        ) else {
            panic!("an unfinished Job whose gate container has not ended waits");
        };
        assert!(why.contains("still running"), "{why}");
    }

    #[test]
    fn the_pod_list_is_read_into_the_gate_containers_end() {
        let list = json!({"items": [
            {"metadata": {"name": "old", "creationTimestamp": "2026-10-06T19:00:00Z"},
             "status": {"containerStatuses": [{"name": "gate", "state": {"running": {}}}]}},
            {"metadata": {"name": "new", "creationTimestamp": "2026-10-06T19:05:00Z"},
             "status": {
                "initContainerStatuses": [{"name": "postgres", "state": {"terminated":
                    {"exitCode": 0, "reason": "Completed", "message": "not the gate's"}}}],
                "containerStatuses": [
                    {"name": "sidecar", "state": {"terminated": {"exitCode": 9, "message": "no"}}},
                    {"name": "gate", "state": {"terminated": {
                        "exitCode": 1, "reason": "Error",
                        "finishedAt": "2026-10-06T19:40:00Z", "message": "the trailer\n"}}}]}}
        ]});
        assert_eq!(
            gate_pod_from_json(&list),
            GatePod {
                pods: 2,
                ended: Some(Ended {
                    exit_code: 1,
                    reason: "Error".into(),
                    finished_at: "2026-10-06T19:40:00Z".into(),
                    message: "the trailer\n".into(),
                }),
                seen: String::new(),
            }
        );
        // A pod that failed before its gate container started: what the
        // cluster says of it, for the `lost` receipt.
        let evicted = json!({"items": [{"metadata": {"name": "p"}, "status": {
            "phase": "Failed", "reason": "Evicted",
            "containerStatuses": [{"name": "gate", "state": {"waiting": {"reason": "PodInitializing"}}}]}}]});
        assert_eq!(
            gate_pod_from_json(&evicted),
            GatePod {
                pods: 1,
                ended: None,
                seen: "Failed, Evicted, gate waiting: PodInitializing".into(),
            }
        );
        let running = json!({"items": [{"metadata": {"name": "p"}, "status": {"phase": "Running",
            "containerStatuses": [{"name": "gate", "state": {"running": {"startedAt": "t"}}}]}}]});
        assert_eq!(gate_pod_from_json(&running), super::fixtures::running());
        assert_eq!(
            gate_pod_from_json(&json!({"items": []})),
            GatePod::default()
        );
    }

    #[test]
    fn only_the_runners_four_words_and_a_json_object_are_a_receipt() {
        let payload = green();
        for word in ["withdrawn", "GREEN", "passed", ""] {
            assert!(
                matches!(
                    parse_frame(&framed(JOB, word, &payload), JOB),
                    Frame::Broken(_)
                ),
                "`{word}` is not a word a runner may say"
            );
            // Refused as a WORD, over a receipt that states no verdict
            // too — where the two-verdicts rule has nothing to compare
            // and would let it through.
            let silent = json!({"head": HEAD}).to_string();
            let Frame::Broken(why) = parse_frame(&framed(JOB, word, &silent), JOB) else {
                panic!("`{word}` was read as a verdict over a receipt that states none");
            };
            assert!(why.contains("not one a runner may say"), "{why}");
        }
        for not_object in ["[1,2]", "\"green\"", "not json"] {
            // Under `failed` as well as `green`: the two-verdicts rule
            // refuses a green over anything, and must not be the only
            // thing standing between an array and the record.
            for word in ["green", "failed"] {
                let Frame::Broken(why) = parse_frame(&framed(JOB, word, not_object), JOB) else {
                    panic!("{not_object} under `{word}` was read as a receipt object");
                };
                assert!(why.contains("not a JSON object"), "{why}");
            }
        }
        // A trailer with a field this reader does not know is refused,
        // not guessed at.
        let log = format!(
            "gate-runner: receipt {payload}\n{} packet=other\n",
            trailer(JOB, "green", &payload)
        );
        assert!(matches!(parse_frame(&log, JOB), Frame::Broken(_)));
        let Frame::Broken(why) = parse_frame("gate-runner: swept 0\n", JOB) else {
            panic!("a log with no frame was read as one");
        };
        assert!(why.contains("not the runner's receipt trailer"), "{why}");
    }

    /// The receipt is recorded on the packet the JOB names. What it says
    /// about itself — a packet, a branch — rides along as text and
    /// decides nothing; the launcher's sha is recorded beside the head
    /// the runner claims, from the packet, not from the receipt.
    #[test]
    fn a_recorded_receipt_keeps_every_field_and_says_who_recorded_it() {
        let payload =
            json!({"verdict": "green", "head": "ffffffffffffffffffffffffffffffffffffffff",
                             "packet": "someone-elses", "branch": "main",
                             "checks": [{"name": "fmt", "result": "pass", "seconds": 3}]})
            .to_string();
        let log = framed(JOB, "green", &payload);
        let Carried::Record { verdict, receipt } = judge(
            &open_run(),
            &job(None),
            &ended_on(&first_trailer(&log), 0, "Completed"),
            &log,
            "boss-dev",
            at(),
        ) else {
            panic!("a complete frame on a packet that owes a verdict is recorded");
        };
        assert_eq!(verdict, "green");
        let got: Value = serde_json::from_str(&receipt).unwrap();
        let carried: Value = serde_json::from_str(&payload).unwrap();
        for (k, v) in carried.as_object().unwrap() {
            assert_eq!(&got[k], v, "the receipt lost or changed `{k}`");
        }
        let by = &got["recorded_by"];
        assert_eq!(by["job"], JOB);
        assert_eq!(by["job_uid"], "uid-1");
        assert_eq!(by["carrier"], CARRIER_POD_LOG);
        assert_eq!(by["launched_sha"], HEAD);
        assert_eq!(by["head_matches_launch"], false);
        assert_eq!(by["bytes"], payload.len());
        assert_eq!(
            by["sha256"],
            hex::encode(Sha256::digest(payload.as_bytes()))
        );
        assert_eq!(by["observed_at"], crate::gate::stamp(at()));
        // RECEIPT COPIED, NOT RETYPED: the record says where from, and
        // keeps the kubelet's account of the end beside the runner's word.
        assert!(
            by["copied_from"].as_str().unwrap().contains(JOB)
                && by["copied_from"]
                    .as_str()
                    .unwrap()
                    .contains("termination message"),
            "{by}"
        );
        assert_eq!(by["exit_code"], 0);
        assert_eq!(by["ended"], "Completed");
    }

    /// AN INFRASTRUCTURE REFUSAL IS NOT A CONSIST FAILURE. A Job that
    /// ended with no complete receipt judged nothing: `lost`, never
    /// `failed`, naming the Job, its condition and where the log is.
    #[test]
    fn a_job_that_ends_with_no_receipt_is_lost_not_failed() {
        for log in [
            "gate-runner: swept 0 dead gate workspace(s)\n".to_string(),
            format!("gate-runner: receipt {}\n", green()),
        ] {
            let Carried::Lost { receipt } = judge(
                &open_run(),
                &job(Some(true)),
                &ended(&log),
                &log,
                "boss-dev",
                at(),
            ) else {
                panic!("a finished Job with no complete receipt is settled lost");
            };
            for must in [
                JOB,
                "uid-1",
                "BackoffLimitExceeded",
                "2026-10-06T19:40:00Z",
                "NO VERDICT WAS PRODUCED",
                "not a consist failure",
                "kubectl -n boss-dev logs job/gate-fix-x-abc12",
            ] {
                assert!(
                    receipt.contains(must),
                    "the receipt does not name `{must}`: {receipt}"
                );
            }
        }
    }

    /// While the Job runs, an absent or half-printed frame is not yet
    /// anything: the runner may be between its two lines.
    #[test]
    fn a_running_job_with_no_complete_receipt_waits() {
        for log in [String::new(), format!("gate-runner: receipt {}\n", green())] {
            // And it says so as what it is: a RUNNING Job, not a finish
            // the cluster forgot to date (the floor's own wait).
            let Carried::Wait(why) =
                judge(&open_run(), &job(None), &running(), &log, "boss-dev", at())
            else {
                panic!("a running Job with no complete receipt waits");
            };
            assert!(why.contains("still running"), "{why}");
        }
    }

    #[test]
    fn a_packet_already_settled_owes_nothing() {
        assert_eq!(verdict_owed(&open_run()), Ok(()));
        // Arriving twice: the first recording completed the step.
        let mut twice = open_run();
        twice["steps"][0]["status"] = json!("completed");
        twice["steps"][0]["metadata"] = json!({"verdict": "green"});
        assert!(verdict_owed(&twice).unwrap_err().contains("green"));
        // Arriving after the packet was withdrawn and closed.
        let mut withdrawn = twice.clone();
        withdrawn["status"] = json!("closed");
        withdrawn["steps"][0]["metadata"] = json!({"verdict": "withdrawn"});
        assert!(verdict_owed(&withdrawn).unwrap_err().contains("closed"));
    }

    #[test]
    fn only_the_newest_job_settles_and_never_beside_a_live_sibling() {
        let old_layout = GateJob {
            carrier: false,
            ..job(Some(false))
        };
        assert!(
            carrier_subject(std::slice::from_ref(&old_layout))
                .unwrap_err()
                .contains("old-layout")
        );
        assert_eq!(carrier_subject(&[job(Some(true))]).unwrap().name, JOB);
        // A relaunch onto the same packet: the dead first Job does not
        // settle the packet under the run still going (53b9a103).
        let relaunch = GateJob {
            name: "gate-fix-x-new99".into(),
            created: "2026-10-06T19:50:00Z".into(),
            finished: None,
            ..job(None)
        };
        assert_eq!(
            carrier_subject(&[job(Some(true)), relaunch.clone()])
                .unwrap()
                .name,
            "gate-fix-x-new99"
        );
        let newest_done = GateJob {
            finished: job(Some(true)).finished,
            ..relaunch
        };
        let live_older = job(None);
        assert!(
            carrier_subject(&[live_older, newest_done])
                .unwrap_err()
                .contains("still running")
        );
        // A newer old-layout Job on the packet reports for itself.
        let newer_old = GateJob {
            name: "gate-fix-x-zzz00".into(),
            created: "2026-10-06T20:00:00Z".into(),
            ..old_layout
        };
        assert!(carrier_subject(&[job(Some(true)), newer_old]).is_err());
    }

    #[test]
    fn the_cluster_list_is_read_into_facts() {
        let list = json!({"items": [
            {"metadata": {"name": "gate-a-11111", "uid": "u-a",
                          "creationTimestamp": "2026-10-06T19:00:00Z",
                          "labels": {"app": "gate-runner", "boss.dev/packet": "p-a",
                                     "boss.dev/verdict-carrier": "pod-log"}},
             "status": {"failed": 1, "conditions": [
                {"type": "FailureTarget", "reason": "BackoffLimitExceeded", "lastTransitionTime": "t0"},
                {"type": "Failed", "reason": "BackoffLimitExceeded", "lastTransitionTime": "t1"}]}},
            {"metadata": {"name": "gate-b-22222", "uid": "u-b",
                          "creationTimestamp": "2026-10-06T19:05:00Z",
                          "labels": {"app": "gate-runner", "boss.dev/packet": "p-b"}},
             "status": {"active": 1}},
            {"metadata": {"name": "gate-c-33333", "labels": {"app": "gate-runner"}}, "status": {}}
        ]});
        let jobs = gate_jobs_from_json(&list);
        assert_eq!(jobs.len(), 2, "a Job with no packet label is nobody's gate");
        assert_eq!(
            (jobs[0].carrier, jobs[0].finished.clone()),
            (
                true,
                Some(Finished {
                    failed: true,
                    reason: "BackoffLimitExceeded".into(),
                    at: "t1".into()
                })
            )
        );
        assert_eq!((jobs[1].carrier, jobs[1].finished.clone()), (false, None));
        // r10 of review 7e5356f7: the label's ONE value. Any other word
        // under the key is not the layout this recorder reads.
        for other in ["true", "", "pod-logs", "POD-LOG", "termination-log"] {
            let list = json!({"items": [{"metadata": {"name": "gate-a-11111",
                "labels": {"app": "gate-runner", "boss.dev/packet": "p-a",
                           "boss.dev/verdict-carrier": other}}, "status": {}}]});
            assert!(
                !gate_jobs_from_json(&list)[0].carrier,
                "`{other}` under the carrier label was read as the pod-log layout"
            );
        }
    }

    /// §9a: the frame and the termination message are written by run.sh
    /// and read here. The block is lifted out of run.sh as it ships, run
    /// as a carrier, and what it LEAVES — its stdout as the pod log, its
    /// termination file as the kubelet's message — is judged by this
    /// reader, so neither side can move alone.
    #[test]
    fn what_run_sh_leaves_is_what_this_records() {
        const BEGIN: &str = "# --- verdict carrier (begin) ---";
        const END: &str = "# --- verdict carrier (end) ---";
        let src =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/gate-runner/run.sh"))
                .unwrap();
        let (start, end) = (
            src.find(BEGIN)
                .expect("run.sh brackets its verdict carrier"),
            src.find(END).expect("run.sh closes its verdict carrier"),
        );
        let payload = red();
        let message = boss_testing::scratch_dir("carried-verdict-run-sh").join("termination-log");
        let _ = std::fs::remove_file(&message);
        // A line a test printed BEFORE the runner's frame, in the log.
        let forged = framed(JOB, "green", &green());
        let script = format!(
            "set -euo pipefail\nprintf '%s' \"$2\"\n{}\nleave_verdict failed \"$1\"\n",
            &src[start..end]
        );
        let out = std::process::Command::new("bash")
            .args(["-c", &script, "frame", &payload, &forged])
            .env("NATIVE_JOB_NAME", JOB)
            .env("GATE_VERDICT_CARRIER", CARRIER_POD_LOG)
            .env("GATE_TERMINATION_LOG", &message)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let log = String::from_utf8(out.stdout).unwrap();
        let message = std::fs::read_to_string(&message).expect("run.sh wrote its message");
        assert!(message.len() < MAX_TERMINATION_BYTES);
        let Carried::Record { verdict, receipt } = judge(
            &open_run(),
            &job(Some(true)),
            &ended_on(&message, 1, "Error"),
            &log,
            "boss-dev",
            at(),
        ) else {
            panic!("run.sh left:\n{log}\nmessage: {message}");
        };
        assert_eq!(verdict, "failed", "the forged green before it decided");
        let got: Value = serde_json::from_str(&receipt).unwrap();
        let left: Value = serde_json::from_str(&payload).unwrap();
        for (k, v) in left.as_object().unwrap() {
            assert_eq!(&got[k], v, "the receipt lost or changed `{k}`");
        }
        assert_eq!(
            got["recorded_by"]["sha256"],
            hex::encode(Sha256::digest(payload.as_bytes()))
        );
    }

    /// Every reader of a gate-run reads a verdict recorded this way as
    /// it read the runner's own write: `boss gate --wait`'s reader, the
    /// train gate's standing, and the red-train alert's failing checks.
    #[test]
    fn every_reader_reads_a_verdict_recorded_from_the_carrier() {
        let recorded = |payload: &str, word: &str| {
            let log = framed(JOB, word, payload);
            let Carried::Record { verdict, receipt } = judge(
                &open_run(),
                &job(Some(word != "green")),
                &ended(&log),
                &log,
                "boss-dev",
                at(),
            ) else {
                panic!("recorded");
            };
            let mut run = open_run();
            run["steps"][0]["status"] = json!("completed");
            run["steps"][0]["metadata"] = json!({"verdict": verdict, "receipt": receipt});
            run
        };
        let g = recorded(&green(), "green");
        assert_eq!(
            crate::gate::own_verdict("run-1", &g).unwrap().as_deref(),
            Some("green")
        );
        assert_eq!(
            crate::train_gate::standing(&g),
            crate::train_gate::Standing::Green
        );
        let r = recorded(&red(), "failed");
        assert_eq!(
            crate::gate::own_verdict("run-1", &r).unwrap().as_deref(),
            Some("failed")
        );
        assert_eq!(
            crate::train_gate::standing(&r),
            crate::train_gate::Standing::Failed
        );
        assert_eq!(
            crate::train_gate::fails(&r),
            vec!["test: boss-jobs::a_thing - FAILED".to_string()]
        );
        let refusal =
            json!({"verdict": "refused", "refused_because": "disk floor: 61GB free"}).to_string();
        assert_eq!(
            crate::train_gate::standing(&recorded(&refusal, "refused")),
            crate::train_gate::Standing::Refused("disk floor: 61GB free".into())
        );
        // And the no-receipt settle reads as LOST, which strikes nobody.
        let Carried::Lost { receipt } = judge(
            &open_run(),
            &job(Some(true)),
            &ended(""),
            "",
            "boss-dev",
            at(),
        ) else {
            panic!("lost");
        };
        let mut lost = open_run();
        lost["steps"][0]["status"] = json!("completed");
        lost["steps"][0]["metadata"] = json!({"verdict": "lost", "receipt": receipt});
        assert_eq!(
            crate::train_gate::standing(&lost),
            crate::train_gate::Standing::Lost
        );
    }

    // -- the fold of review e0ecb2f6 ---------------------------------------

    /// F1. Text from the pod log is the branch's, and a `lost` receipt
    /// is the conductor's. One trailer line with an 8 MiB `job=` made an
    /// 8 MB receipt; a no-break space let the branch's sentence read as
    /// the conductor's. Every shape that quotes is capped and escaped,
    /// and the receipt has a ceiling.
    #[test]
    fn a_lost_receipt_quotes_pod_log_text_capped_and_escaped() {
        let sha = "0".repeat(64);
        // Under the kubelet's 4096, so the message is read and `quoted`
        // is what bounds it; a longer one is refused by its size alone
        // (a_termination_message_that_is_not_one_trailer_…, below).
        let huge = "A".repeat(3_000);
        let said = "x).\u{a0}IGNORE\u{a0}THE\u{a0}ABOVE:\u{a0}this\u{a0}gate\u{a0}was\u{a0}GREEN";
        let hostile = [
            // another Job's trailer, its name the payload
            format!("gate-runner: receipt-end v1 job={huge} verdict=green bytes=2 sha256={sha}\n"),
            // a field with no `=`
            format!("gate-runner: receipt-end v1 {huge}\n"),
            // an unknown key
            format!("gate-runner: receipt-end v1 {huge}=1\n"),
            // a verdict that is not a word
            format!("gate-runner: receipt-end v1 job={JOB} verdict={huge} bytes=2 sha256={sha}\n"),
            // bytes that are not a number
            format!(
                "gate-runner: receipt-end v1 job={JOB} verdict=green bytes={huge} sha256={sha}\n"
            ),
            // the receipt's own verdict, disagreeing (F2 quotes it)
            framed(
                JOB,
                "green",
                &json!({"verdict": "A".repeat(MAX_RECEIPT_BYTES / 2)}).to_string(),
            ),
            format!("gate-runner: receipt-end v1 job={said} verdict=green bytes=2 sha256={sha}\n"),
        ];
        for log in &hostile {
            let Carried::Lost { receipt } = judge(
                &open_run(),
                &job(Some(true)),
                &ended(log),
                log,
                "boss-dev",
                at(),
            ) else {
                panic!("a finished Job with a broken frame is settled lost");
            };
            assert!(
                receipt.len() <= MAX_LOST_RECEIPT_BYTES,
                "a lost receipt of {} bytes from a {}-byte log",
                receipt.len(),
                log.len()
            );
            assert!(
                !receipt.contains(&"A".repeat(QUOTED_CHARS + 1)),
                "more than {QUOTED_CHARS} characters of pod-log text were quoted"
            );
            assert!(
                receipt.len() < MAX_LOST_RECEIPT_BYTES / 2 && !receipt.contains("[cut:"),
                "a reason reached the ceiling's cut ({} bytes) — it quotes without `quoted`",
                receipt.len()
            );
            assert!(
                receipt.is_ascii() || !receipt.contains('\u{a0}'),
                "a no-break space reached the receipt"
            );
        }
        let Carried::Lost { receipt } = judge(
            &open_run(),
            &job(Some(true)),
            &ended(hostile.last().unwrap()),
            hostile.last().unwrap(),
            "boss-dev",
            at(),
        ) else {
            panic!("lost");
        };
        assert!(
            !receipt.contains("IGNORE THE ABOVE") && !receipt.contains("IGNORE\u{a0}THE"),
            "the branch's sentence reads as prose in the conductor's receipt: {receipt}"
        );
        assert!(
            receipt.contains(r#"<untrusted pod-log text, "#)
                && receipt.contains(r"x).\u{a0}IGNORE\u{a0}THE"),
            "the quoted text is delimited, labelled and escaped: {receipt}"
        );
        // The ceiling itself, on text no reason quotes: a packet whose
        // branch is 10 KB still gets a receipt under it, cut and said.
        let mut long_branch = open_run();
        long_branch["metadata"]["branch"] = json!("é".repeat(5_000));
        let Carried::Lost { receipt } = judge(
            &long_branch,
            &job(Some(true)),
            &ended(""),
            "",
            "boss-dev",
            at(),
        ) else {
            panic!("lost");
        };
        assert!(
            receipt.len() <= MAX_LOST_RECEIPT_BYTES
                && receipt.ends_with("[cut: this receipt is capped]"),
            "{} bytes",
            receipt.len()
        );
        // The quoting itself: the cap, the true length, and no way to
        // close the delimiter from inside.
        assert_eq!(
            quoted("a b\"c\\"),
            r#"<untrusted pod-log text, 6 byte(s): "a\u{20}b\u{22}c\u{5c}">"#
        );
        let cut = quoted(&"z".repeat(500));
        assert!(cut.contains("500 byte(s), cut") && cut.matches('z').count() == QUOTED_CHARS);
    }

    /// F2. The step takes the trailer's word; `train_gate::standing`
    /// reads the receipt's. A pair run.sh cannot print is neither
    /// verdict — and the three it can print are recorded as it reported
    /// them.
    #[test]
    fn a_trailer_and_a_receipt_that_disagree_are_neither_verdict() {
        let frame =
            |word: &str, receipt: Value| parse_frame(&framed(JOB, word, &receipt.to_string()), JOB);
        for (word, own) in [
            ("green", json!("failed")),
            ("green", json!("refused")),
            ("green", json!("lost")),
            ("failed", json!("green")),
            ("refused", json!("green")),
            ("lost", json!("failed")),
            ("refused", json!("failed")),
            // a green nobody can read is not a green
            ("green", json!("unreadable")),
            ("green", Value::Null),
            ("green", json!(7)),
        ] {
            let Frame::Broken(why) = frame(word, json!({"verdict": own, "head": HEAD})) else {
                panic!("trailer `{word}` over receipt verdict {own} was read as a receipt");
            };
            assert!(why.contains("two verdicts"), "{why}");
        }
        assert!(
            matches!(frame("green", json!({"head": HEAD})), Frame::Broken(_)),
            "a green whose receipt states no verdict"
        );
        // What run.sh prints: the same word; `failed` over its own
        // network-refusal rewrite; and the fallback for a receipt gate.sh
        // never wrote.
        for (word, receipt) in [
            (
                "failed",
                json!({"verdict": "refused", "refused_because": "no route"}),
            ),
            (
                "failed",
                json!({"verdict": "unreadable", "head": HEAD, "error": "x"}),
            ),
            ("failed", json!({"head": HEAD})),
            ("lost", json!({"verdict": "lost"})),
        ] {
            assert!(
                matches!(frame(word, receipt.clone()), Frame::Complete { verdict, .. } if verdict == word),
                "run.sh prints `{word}` over {receipt}"
            );
        }
        // And on a finished Job the disagreement is `lost`, by name.
        let disagreeing = framed(JOB, "green", &json!({"verdict": "refused"}).to_string());
        let Carried::Lost { receipt } = judge(
            &open_run(),
            &job(Some(false)),
            &ended(&disagreeing),
            &disagreeing,
            "boss-dev",
            at(),
        ) else {
            panic!("a disagreement on a finished Job is lost, never green and never refused");
        };
        assert!(receipt.contains("two verdicts"), "{receipt}");
    }

    /// F3. A Job's label names a job id; only a gate-run is written.
    #[test]
    fn only_a_gate_run_owes_this_recorder_a_verdict() {
        let mut other = open_run();
        other["kind"] = json!("backlog-item");
        assert!(verdict_owed(&other).unwrap_err().contains("backlog-item"));
        other.as_object_mut().unwrap().remove("kind");
        assert!(
            verdict_owed(&other).is_err(),
            "a packet that states no kind"
        );
    }

    /// `lost` cannot be taken back. A finish younger than the floor, or
    /// one the cluster has not dated, waits; at the floor it settles.
    #[test]
    fn a_missing_receipt_is_not_lost_until_the_finish_is_a_minute_old() {
        let dated = |at_: &str| GateJob {
            finished: Some(Finished {
                failed: true,
                reason: "BackoffLimitExceeded".into(),
                at: at_.into(),
            }),
            ..job(None)
        };
        for young in [
            "2026-10-06T19:40:01Z",
            "2026-10-06T19:40:59Z",
            "2026-10-06T19:41:30Z",
            "",
            "soon",
        ] {
            for log in ["", "gate-runner: receipt {}\n"] {
                assert!(
                    matches!(
                        judge(
                            &open_run(),
                            &dated(young),
                            &ended(log),
                            log,
                            "boss-dev",
                            at()
                        ),
                        Carried::Wait(_)
                    ),
                    "a finish at `{young}` was settled lost at 19:41:00"
                );
            }
        }
        assert!(matches!(
            judge(
                &open_run(),
                &dated("2026-10-06T19:40:00Z"),
                &ended(""),
                "",
                "boss-dev",
                at()
            ),
            Carried::Lost { .. }
        ));
        // The floor is for a MISSING receipt: a complete one is recorded
        // the moment it is read.
        assert!(matches!(
            judge(
                &open_run(),
                &dated(""),
                &ended(&framed(JOB, "green", &green())),
                &framed(JOB, "green", &green()),
                "boss-dev",
                at()
            ),
            Carried::Record { .. }
        ));
    }

    /// m13. Each trailer field once: a second `job=` or `verdict=` is not
    /// a choice this reader makes.
    #[test]
    fn a_trailer_field_stated_twice_is_refused() {
        let payload = green();
        for again in [
            format!("job={JOB}"),
            "verdict=failed".to_string(),
            "bytes=1".into(),
        ] {
            let log = format!(
                "gate-runner: receipt {payload}\n{} {again}\n",
                trailer(JOB, "green", &payload)
            );
            let Frame::Broken(why) = parse_frame(&log, JOB) else {
                panic!("a trailer stating `{again}` a second time was read");
            };
            assert!(why.contains("twice"), "{why}");
        }
    }

    /// m09. `recorded_by` is the recorder's block. One the payload
    /// brought is the branch's text under the conductor's name: nothing
    /// of it survives, under that key or beside it.
    #[test]
    fn a_recorded_by_the_payload_brought_never_survives() {
        let payload = json!({"verdict": "green", "head": HEAD,
                             "recorded_by": {"recorder": "spoofed-by-the-branch",
                                             "job": "gate-someone-else", "head_matches_launch": true}})
        .to_string();
        let log = framed(JOB, "green", &payload);
        let Carried::Record { receipt, .. } = judge(
            &open_run(),
            &job(None),
            &ended(&log),
            &log,
            "boss-dev",
            at(),
        ) else {
            panic!("recorded");
        };
        let got: Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(got["recorded_by"]["job"], JOB);
        assert_eq!(got["recorded_by"]["carrier"], CARRIER_POD_LOG);
        assert!(
            !receipt.contains("spoofed-by-the-branch") && !receipt.contains("gate-someone-else"),
            "the payload's own recorded_by reached the record: {receipt}"
        );
    }

    /// A stand-in for kubectl: a script in this test's own scratch
    /// directory. Written through `write_exec`, never by this process:
    /// it is exec'ed directly, and a file this process held open for
    /// writing is briefly open in every child a sibling test spawns, so
    /// the exec loses with ETXTBSY (backlog 6eaef658). The first version
    /// wrote and chmod'ed it here and redded gate-run 34f51ca0 on the
    /// tree-wide pin.
    fn stand_in(name: &str, body: &str) -> (std::path::PathBuf, String) {
        let dir = boss_testing::scratch_dir(name);
        let program = dir.join("kubectl");
        boss_testing::write_exec(&program, &format!("#!/bin/sh\n{body}\n"));
        let path = program.display().to_string();
        (dir, path)
    }

    /// Every flag of kubectl's that overrides the client config, by
    /// name and without its dashes (so this list is not itself a hit of
    /// the source pin below). `kubectl options`, v1.33.3.
    const CLIENT_CONFIG_OVERRIDES: [&str; 19] = [
        "request-timeout",
        "server",
        "token",
        "context",
        "cluster",
        "user",
        "username",
        "password",
        "as",
        "as-group",
        "as-uid",
        "as-user-extra",
        "insecure-skip-tls-verify",
        "certificate-authority",
        "client-certificate",
        "client-key",
        "kubeconfig",
        "tls-server-name",
        "disable-compression",
    ];

    /// Backlog 06ae925a: THE READ REACHES THE CLUSTER. The adapter has no
    /// kubeconfig, and kubectl gives up its in-cluster fallback when a
    /// client-config override is set, so the argv of every read is held
    /// here, argument by argument: `-n <namespace>` first, and no
    /// override flag at all — long, short (`-s`), with `=` or without.
    #[tokio::test]
    async fn no_cluster_read_carries_a_client_config_override() {
        let (dir, program) = stand_in(
            "carried-verdict-kubectl-args",
            r#"for a in "$@"; do printf '%s\n' "$a"; done >> "$(dirname "$0")/args"; echo '{"items":[]}'"#,
        );
        let _ = std::fs::remove_file(dir.join("args"));
        let k = Kubectl {
            program,
            ..Kubectl::default()
        };
        assert!(k.gate_jobs("boss-dev").await.unwrap().is_empty());
        k.gate_log("boss-dev", "gate-x-1").await.unwrap();
        assert_eq!(
            k.gate_pod("boss-dev", "gate-x-1").await.unwrap(),
            GatePod::default()
        );
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        let argv: Vec<&str> = args.lines().collect();
        assert_eq!(
            argv,
            [
                ["-n", "boss-dev", "get", "jobs", "-l", "app=gate-runner"].as_slice(),
                &["-o", "json"],
                &["-n", "boss-dev", "logs", "job/gate-x-1", "-c", "gate"],
                &["--tail=-1"],
                &["-n", "boss-dev", "get", "pods", "-l", "job-name=gate-x-1"],
                &["-o", "json"],
            ]
            .concat(),
            "the three reads, whole"
        );
        for arg in argv {
            let name = arg.trim_start_matches('-').split('=').next().unwrap();
            assert!(
                !(arg.starts_with("--") && CLIENT_CONFIG_OVERRIDES.contains(&name)) && arg != "-s",
                "`{arg}` overrides the client config: with no kubeconfig kubectl then ignores \
                 the pod's ServiceAccount and dials localhost:8080 (backlog 06ae925a)"
            );
        }
    }

    /// The same rule over the rest of this crate's source: an override
    /// flag is written as an argument in ONE file, `consist_job.rs`,
    /// whose every command states its own `--kubeconfig` (so nothing
    /// there falls back, and its `--request-timeout` is kubectl's bound
    /// on a config it was handed — measured answering, 2026-10-08).
    /// Every other kubectl this crate builds (`gate::kubectl`, this
    /// adapter, `credential pull`) rides the ambient config or the
    /// in-cluster fallback, and a flag from this family breaks the second.
    #[test]
    fn an_override_flag_is_an_argument_only_where_a_kubeconfig_is_stated() {
        fn walk(dir: &Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, hits);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs")
                    || path.file_name().is_some_and(|n| n == "consist_job.rs")
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    for flag in CLIENT_CONFIG_OVERRIDES {
                        let (bare, with_value) = (format!("\"--{flag}\""), format!("\"--{flag}="));
                        if line.contains(&bare) || line.contains(&with_value) {
                            hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                        }
                    }
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        walk(&src, &mut hits);
        assert!(
            hits.is_empty(),
            "a client-config override flag outside consist_job.rs — if it is a kubectl \
             argument, it breaks the in-cluster fallback (backlog 06ae925a):\n{}",
            hits.join("\n")
        );
        let consist = std::fs::read_to_string(src.join("train/consist_job.rs")).unwrap();
        assert!(
            consist.contains(".arg(\"--kubeconfig\")"),
            "consist_job.rs no longer states its own kubeconfig, so its request timeouts fall \
             back to nothing"
        );
    }

    /// r07 of review 7e5356f7: the log bound. A log over it is refused by
    /// its size, not read into a receipt; one at it is read.
    #[tokio::test]
    async fn a_log_over_the_bound_is_refused_and_one_at_it_is_read() {
        for (name, bytes, ok) in [
            ("carried-verdict-kubectl-log-over", MAX_LOG_BYTES + 1, false),
            ("carried-verdict-kubectl-log-at", MAX_LOG_BYTES, true),
        ] {
            let (_dir, program) =
                stand_in(name, &format!("head -c {bytes} /dev/zero | tr '\\0' x"));
            let k = Kubectl {
                program,
                ..Kubectl::default()
            };
            match k.gate_log("boss-dev", "gate-x-1").await {
                Ok(log) => assert!(ok && log.len() == bytes, "{} bytes were read", log.len()),
                Err(e) => assert!(
                    !ok && format!("{e:#}").contains(&format!("over the {MAX_LOG_BYTES}")),
                    "{e:#}"
                ),
            }
        }
    }

    /// F5. A client that hangs is killed at the bound and reads as an
    /// error — and the wait is awaited, not blocked on: a timer on the
    /// same single-threaded runtime keeps firing while the read hangs.
    #[tokio::test(flavor = "current_thread")]
    async fn a_hung_cluster_read_is_killed_at_its_bound_and_does_not_block_the_runtime() {
        let (_dir, program) = stand_in("carried-verdict-kubectl-hang", "exec sleep 60");
        let k = Kubectl {
            program,
            bound: std::time::Duration::from_millis(400),
        };
        let ticks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t = ticks.clone();
        let ticker = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let started = std::time::Instant::now();
        let err = k.gate_jobs("boss-dev").await.unwrap_err();
        let took = started.elapsed();
        ticker.abort();
        assert!(
            format!("{err:#}").contains("did not answer within"),
            "{err:#}"
        );
        assert!(
            took < std::time::Duration::from_secs(10),
            "a hung client held the pass {took:?}"
        );
        assert!(
            ticks.load(std::sync::atomic::Ordering::SeqCst) >= 5,
            "the runtime was blocked while the read hung ({} ticks)",
            ticks.load(std::sync::atomic::Ordering::SeqCst)
        );
        let log_err = k.gate_log("boss-dev", "gate-x-1").await.unwrap_err();
        assert!(format!("{log_err:#}").contains("did not answer within"));
    }

    /// N1 of review 47319ee7. A verdict waiting on a read is said once
    /// per state — across processes — and said again when the state
    /// changes or comes back after the packet moved on.
    #[test]
    fn a_verdict_waiting_on_a_read_is_said_once_per_state() {
        let dir = boss_testing::scratch_dir("carried-verdict-waiting");
        let _ = std::fs::remove_dir_all(&dir);
        let say = |kind: &'static str| {
            waiting_line(
                &dir,
                "0f3c-run-1",
                Some((kind, format!("gate-run x: {kind}"))),
            )
        };
        let first = say("pod-unreadable").expect("the first pass says it");
        assert!(
            first.contains("pod-unreadable") && first.contains("said again every 60 min"),
            "{first}"
        );
        assert_eq!(say("pod-unreadable"), None, "the same state was said again");
        // Backlog 06ae925a: not silent for as long as it lasts. The latch's
        // own time is when it was last said; an interval on, it is said
        // again, and that saying starts the next interval.
        let aged = |minutes: u64| {
            std::fs::File::options()
                .append(true)
                .open(dir.join("0f3c-run-1"))
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(minutes * 60),
                )
                .unwrap()
        };
        aged(RESAY_MINUTES as u64 - 2);
        assert_eq!(
            say("pod-unreadable"),
            None,
            "said again inside the interval"
        );
        aged(RESAY_MINUTES as u64 + 1);
        assert!(say("pod-unreadable").is_some(), "silent past the interval");
        assert_eq!(
            say("pod-unreadable"),
            None,
            "the re-saying did not restart it"
        );
        assert!(say("log-unreadable").is_some(), "a NEW state is said");
        assert_eq!(say("log-unreadable"), None);
        // Another packet has its own latch.
        assert!(waiting_line(&dir, "0f3c-run-2", Some(("pod-unreadable", "y".into()))).is_some());
        // Settled, or the read worked: forgotten, so a later outage speaks.
        assert_eq!(waiting_line(&dir, "0f3c-run-1", None), None);
        assert!(say("log-unreadable").is_some());
        // An id that is not a plain id is never a file name: always said.
        for odd in ["../x", "a/b", "", "a b"] {
            for _ in 0..2 {
                assert!(waiting_line(&dir, odd, Some(("k", "z".into()))).is_some());
            }
            assert_eq!(waiting_line(&dir, odd, None), None);
        }
        assert!(!dir.parent().unwrap().join("x").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F5, and backlog 06ae925a. A dark cluster is said when it goes,
    /// AGAIN every interval while it stays dark, and when it comes back —
    /// across PROCESSES, since every reconcile is its own — and never
    /// once per pass. Every line after the first names the time the
    /// outage was FIRST seen.
    #[test]
    fn a_failing_cluster_read_is_said_on_an_interval_and_never_per_pass() {
        let latch =
            boss_testing::scratch_dir("carried-verdict-latch").join("gate-jobs-read.failing");
        let _ = std::fs::remove_file(&latch);
        let after = |minutes: i64| at() + chrono::Duration::minutes(minutes);
        let first_seen = crate::gate::stamp(at());
        assert_eq!(
            cluster_read_line(&latch, None, at()),
            None,
            "nothing to say while it works"
        );
        let why = "kubectl did not answer within 30s and was killed";
        let first = cluster_read_line(&latch, Some(why), at()).expect("the first failure is said");
        assert!(
            first.contains("did not answer within 30s") && first.contains("said again every 60"),
            "{first}"
        );
        // Every two minutes for the rest of the hour: nothing.
        for pass in 1..30 {
            assert_eq!(
                cluster_read_line(&latch, Some(why), after(pass * 2)),
                None,
                "the same outage was said again {} min in",
                pass * 2
            );
        }
        // The hour turns: said, with since when — and then quiet again.
        let again = cluster_read_line(&latch, Some(why), after(60)).expect("silent past an hour");
        assert!(
            again.contains("STILL cannot be read")
                && again.contains(&format!("failing since {first_seen}"))
                && again.contains(why),
            "{again}"
        );
        assert_eq!(cluster_read_line(&latch, Some(why), after(62)), None);
        assert_eq!(cluster_read_line(&latch, Some(why), after(118)), None);
        let third = cluster_read_line(&latch, Some(why), after(120)).expect("the second hour");
        assert!(
            third.contains(&format!("failing since {first_seen}")),
            "{third}"
        );
        // Recovery states the ORIGINAL first-seen time and how long.
        let back = cluster_read_line(&latch, None, after(125)).expect("the recovery is said");
        assert!(
            back.contains("readable again")
                && back.contains(&format!("failing since {first_seen} (2h05m)")),
            "{back}"
        );
        assert!(!latch.exists());
        assert_eq!(cluster_read_line(&latch, None, after(127)), None);
        assert!(
            cluster_read_line(&latch, Some("dark again"), after(129)).is_some(),
            "a NEW outage is said"
        );
        let _ = std::fs::remove_file(&latch);
    }

    /// Backlog 06ae925a: the latch must not hide a changed cause behind
    /// the first one. kubectl's first line opens with a time and a pid,
    /// so the SAME cause reads differently on every pass: it is compared
    /// folded, and only a different failure speaks inside the interval.
    #[test]
    fn a_changed_cause_is_said_at_once_and_the_same_one_is_not() {
        let latch =
            boss_testing::scratch_dir("carried-verdict-latch-cause").join("gate-jobs-read.failing");
        let _ = std::fs::remove_file(&latch);
        let after = |minutes: i64| at() + chrono::Duration::minutes(minutes);
        let refused = |time: &str, pid: &str| {
            format!(
                "kubectl get jobs -l app=gate-runner -o json failed: E1007 {time} {pid} \
                 memcache.go:265] \"Unhandled Error\" err=\"couldn't get current server API \
                 group list: Get \\\"http://localhost:8080/api?timeout=20s\\\": dial tcp \
                 [::1]:8080: connect: connection refused\"\nThe connection to the server \
                 localhost:8080 was refused - did you specify the right host or port?"
            )
        };
        let (a, b) = (
            refused("08:56:24.143705", "     69"),
            refused("08:58:31.900017", "   8281"),
        );
        assert_ne!(a.lines().next(), b.lines().next());
        assert_eq!(cause_of(&a), cause_of(&b), "one cause, two passes");
        assert!(cluster_read_line(&latch, Some(&a), at()).is_some());
        assert_eq!(
            cluster_read_line(&latch, Some(&b), after(2)),
            None,
            "a new time and pid on the same refusal is not a new cause"
        );
        let forbidden = "kubectl get jobs -l app=gate-runner -o json failed: Error from server \
                         (Forbidden): jobs.batch is forbidden";
        let changed = cluster_read_line(&latch, Some(forbidden), after(4))
            .expect("a different failure is said inside the interval");
        assert!(
            changed.contains("the cause CHANGED")
                && changed.contains("connection refused")
                && changed.contains("Forbidden")
                && changed.contains(&format!("failing since {}", crate::gate::stamp(at()))),
            "{changed}"
        );
        assert_eq!(cluster_read_line(&latch, Some(forbidden), after(6)), None);
        assert_eq!(
            outage(&latch),
            Some((crate::gate::stamp(at()), cause_of(forbidden))),
            "the latch keeps the first-seen time through a changed cause"
        );
        let _ = std::fs::remove_file(&latch);
    }

    /// The latch the outage of 2026-10-07 left on the conductor's volume,
    /// in the shape the first version wrote (`<time> <words>`, read from
    /// /var/lib/boss-train/gate-jobs-read.failing on 2026-10-08). A read
    /// still failing is said at once, since that day; the first read that
    /// works says "readable again … failing since" THAT time, and clears it.
    #[test]
    fn the_latch_left_by_the_first_version_keeps_its_first_seen_time() {
        let latch = boss_testing::scratch_dir("carried-verdict-latch-legacy")
            .join("gate-jobs-read.failing");
        let left = "2026-10-07T08:56:24Z kubectl get jobs -l app=gate-runner -o json failed: \
                    E1007 08:56:24.143705      69 memcache.go:265] \"Unhandled Error\" \
                    err=\"couldn't get current server API group list\"\nThe connection to the \
                    server localhost:8080 was refused - did you specify the right host or port?";
        let now = DateTime::parse_from_rfc3339("2026-10-08T05:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        std::fs::write(&latch, left).unwrap();
        let still = cluster_read_line(&latch, Some("still refused"), now)
            .expect("a day-old outage in the old shape is said on the first pass");
        assert!(
            still.contains("failing since 2026-10-07T08:56:24Z") && still.contains("still refused"),
            "{still}"
        );
        assert_eq!(
            cluster_read_line(&latch, Some("still refused"), now),
            None,
            "and then on the interval"
        );
        for shape in [left.to_string(), std::fs::read_to_string(&latch).unwrap()] {
            std::fs::write(&latch, shape).unwrap();
            let back = cluster_read_line(&latch, None, now).expect("the recovery is said");
            assert!(
                back.contains("readable again")
                    && back.contains("failing since 2026-10-07T08:56:24Z (20h03m)"),
                "{back}"
            );
            assert!(!latch.exists(), "the latch outlived the recovery");
        }
    }

    /// Backlog 06ae925a: the failure reaches the PACKET. While the Jobs
    /// cannot be read, a gate-run that owes a verdict and has been quiet
    /// long enough is told — once per outage and cause — and the note is
    /// removed when the read works. A settled run, a fresh one, and one
    /// whose quiet cannot be dated are left alone.
    #[test]
    fn a_gate_run_kept_waiting_by_an_unreadable_cluster_is_told_once() {
        let blocked = Some(("2026-10-07T08:56:24Z", "connection refused"));
        let quiet = Some(BLOCKED_NOTE_AFTER_MINS);
        let note = blocked_note(&open_run(), blocked, quiet, at()).expect("an owing run is told");
        assert_eq!(note["since"], "2026-10-07T08:56:24Z");
        assert_eq!(note["cause"], "connection refused");
        assert_eq!(note["noted_at"], crate::gate::stamp(at()));
        assert!(
            note["says"]
                .as_str()
                .unwrap()
                .contains("NOT being recorded")
        );

        let mut told = open_run();
        told["metadata"][RECORDER_BLOCKED_KEY] = note;
        assert_eq!(
            blocked_note(&told, blocked, quiet, at()),
            None,
            "the same outage and cause was written twice"
        );
        let changed = Some(("2026-10-07T08:56:24Z", "Forbidden"));
        assert_eq!(
            blocked_note(&told, changed, quiet, at()).unwrap()["cause"],
            "Forbidden"
        );
        assert_eq!(
            blocked_note(&told, None, quiet, at()),
            Some(Value::Null),
            "the read works: the note goes"
        );
        assert_eq!(blocked_note(&open_run(), None, quiet, at()), None);

        for (quiet, why) in [
            (Some(BLOCKED_NOTE_AFTER_MINS - 1), "a run just launched"),
            (None, "a run whose quiet cannot be dated"),
        ] {
            assert_eq!(
                blocked_note(&open_run(), blocked, quiet, at()),
                None,
                "{why}"
            );
        }
        let mut settled = open_run();
        settled["status"] = json!("closed");
        assert_eq!(blocked_note(&settled, blocked, quiet, at()), None);

        let line = recorder_blocked_line(&told).expect("a reader has a line for it");
        assert!(
            line.starts_with("verdict waiting: the recorder cannot read the cluster")
                && line.contains("since 2026-10-07T08:56:24Z")
                && line.contains("connection refused"),
            "{line}"
        );
        assert_eq!(recorder_blocked_line(&open_run()), None);
    }
}
