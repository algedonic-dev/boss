//! The conductor VERIFIES the gate.log the runner framed (backlog
//! 7c49bd38; design 5a274e7c car 2, first piece, decided 2026-11-13).
//!
//! WHY. Car 1 made `infra/gate-runner/runtime-evidence.py retain-log`
//! write the whole of gate.log to the gate pod's stdout as ordered,
//! hashed, gzipped `gate-runner-log:` frames and one `gate-runner:
//! log-end v1` trailer, and bind the same facts into the receipt as
//! `log_retention.gate_log`, BEFORE the failure-detail extractor reduces
//! it. Every receipt since says what the runner CLAIMS it framed — five
//! read on 2026-11-15 named 0.78 MB to 7.4 MB of gate.log in 60 KB to
//! 1.1 MB of gzip — and nothing trusted had checked one frame of it. The
//! runner is branch code and holds no token (designs c395e62c, bdc60b65),
//! so the conductor, which already reads the whole pod log to record the
//! verdict, is the one reader that can say the frames are whole and are
//! this Job's.
//!
//! WHAT IS CHECKED, IN ORDER, against the receipt's binding — the record
//! the termination message vouches for, never a frame's say-so:
//!   1. the binding is for THIS Job, and names the gate.log stream;
//!   2. a `log-end v1` trailer in the log states exactly the binding's
//!      job, sizes, hashes, frame count and state — a trailer printed
//!      earlier by a test, or forged, that says anything else is not it;
//!   3. the `frames` lines directly before that trailer are frames
//!      0..N-1 of N, in order, contiguous, each base64 chunk matching its
//!      own sha256;
//!   4. their bytes are the binding's `gz_bytes` and `gz_sha256`;
//!   5. they inflate to exactly `kept_bytes` with `kept_sha256`, read in
//!      blocks and refused the moment they pass the stated size (a
//!      decompression bomb costs nothing beyond its claim), and for
//!      `retained` the kept bytes ARE the whole.
//! Anything else is `refused` with its reason, and branch text in a
//! reason is `quoted`. The verdict is never touched: this rides beside
//! it in `recorded_by.gate_log`, the recorder's own facts.
//!
//! WHERE THE BYTES GO. This function stores nothing: it runs inside the
//! verdict's judge, which is pure, and the verdict is recorded first
//! (design 5a274e7c, `Failure`). [`verified_gate_log`] hands back the
//! gzip it checked, and `store.rs` — the second piece of car 2 — attaches
//! exactly those bytes to the verdict step on a later pass.

use std::io::Read;

use super::*;

mod store;
pub(crate) use store::*;

/// The frame and trailer lines `runtime-evidence.py retain-log` prints.
const LOG_FRAME_PREFIX: &str = "gate-runner-log: ";
const LOG_TRAILER_PREFIX: &str = "gate-runner: log-end v1 ";

/// The one stream the runner frames today.
const GATE_LOG_STREAM: &str = "gate.log";

/// The most inflated bytes the conductor will hash for one gate.log.
/// The runner's own bound is 4 MiB of GZIP; at the ratio measured on
/// 2026-11-15 (7,400,226 bytes in 1,143,613 of gzip) that is about 27
/// MB, and build chatter compresses further, so a real log sits far
/// under this. A claim past it is refused before a byte is inflated —
/// the frames are branch-written and gzip can claim a thousandfold.
pub(crate) const MAX_GATE_LOG_BYTES: u64 = 1 << 30;

/// The inflate block: the size `retain-log` reads gate.log in.
const INFLATE_BLOCK: usize = 64 * 1024;

/// PURE: the conductor's account of the gate.log frames in `log` for
/// Job `job`, judged against the receipt's `log_retention.gate_log`.
/// Always an object with a `state`: `verified`, `partial` (a verified
/// prefix of a stated whole), `refused`, `unavailable` (the runner
/// framed nothing and said why) or `absent` (the receipt carries no
/// binding — a runner older than car 1).
pub(crate) fn verify_gate_log(log: &str, job: &str, receipt: &Map<String, Value>) -> Value {
    verified_gate_log(log, job, receipt).account
}

/// What [`verified_gate_log`] found: the account [`verify_gate_log`]
/// records, and — only when that account is `verified` or `partial` —
/// the gzip bytes it checked. Those bytes are the only ones `store.rs`
/// may attach: nothing is re-read or re-framed between the check and the
/// upload.
pub(crate) struct VerifiedGateLog {
    pub(crate) account: Value,
    pub(crate) gzip: Option<Vec<u8>>,
}

/// PURE: [`verify_gate_log`], keeping the verified gzip.
pub(crate) fn verified_gate_log(
    log: &str,
    job: &str,
    receipt: &Map<String, Value>,
) -> VerifiedGateLog {
    let Some(binding) = receipt.get("log_retention").and_then(|r| r.get("gate_log")) else {
        return VerifiedGateLog {
            account: json!({
                "state": "absent",
                "reason": "the receipt carries no log_retention.gate_log: its runner did not \
                           frame gate.log (runtime-evidence.py retain-log, design 5a274e7c car 1)",
            }),
            gzip: None,
        };
    };
    match verified(log, job, binding) {
        Ok((account, gzip)) => VerifiedGateLog { account, gzip },
        Err(why) => VerifiedGateLog {
            account: json!({"state": "refused", "stream": GATE_LOG_STREAM, "reason": why}),
            gzip: None,
        },
    }
}

/// The binding's value for `key` as the trailer spells it: a string as
/// itself, a number in decimal. `None` for anything else.
fn word(binding: &Value, key: &str) -> Option<String> {
    match binding.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The trailer fields that must equal the binding's, as (trailer key,
/// binding key).
const BOUND_FIELDS: [(&str, &str); 9] = [
    ("job", "job"),
    ("bytes", "original_bytes"),
    ("sha256", "original_sha256"),
    ("kept_bytes", "kept_bytes"),
    ("kept_sha256", "kept_sha256"),
    ("gz_bytes", "gz_bytes"),
    ("gz_sha256", "gz_sha256"),
    ("frames", "frames"),
    ("state", "state"),
];

fn verified(
    log: &str,
    job: &str,
    binding: &Value,
) -> std::result::Result<(Value, Option<Vec<u8>>), String> {
    let state = binding.get("state").and_then(Value::as_str).unwrap_or("");
    let bound_job = binding.get("job").and_then(Value::as_str).unwrap_or("");
    if bound_job != job {
        return Err(format!(
            "the receipt's log_retention.gate_log is bound to another Job, {}",
            quoted(bound_job)
        ));
    }
    if binding.get("stream").and_then(Value::as_str) != Some(GATE_LOG_STREAM) {
        return Err(
            "the receipt's log_retention.gate_log does not name the gate.log stream".into(),
        );
    }
    if state == "unavailable" {
        let why = binding.get("reason").and_then(Value::as_str).unwrap_or("");
        return Ok((
            json!({
                "state": "unavailable",
                "stream": GATE_LOG_STREAM,
                "job": job,
                "reason": format!("the runner framed no gate.log: {}", quoted(why)),
            }),
            None,
        ));
    }
    if state != "retained" && state != "partial" {
        return Err(format!(
            "the receipt's log_retention.gate_log states {}, not retained, partial or \
             unavailable",
            quoted(state)
        ));
    }
    let mut expected = Vec::new();
    for (trailer_key, binding_key) in BOUND_FIELDS {
        let value = word(binding, binding_key).ok_or_else(|| {
            format!("the receipt's log_retention.gate_log has no usable `{binding_key}`")
        })?;
        expected.push((trailer_key, value));
    }
    let number = |key: &str| {
        binding
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("the receipt's log_retention.gate_log `{key}` is not a count"))
    };
    let (kept_bytes, gz_bytes, frames) = (
        number("kept_bytes")?,
        number("gz_bytes")?,
        number("frames")?,
    );
    let original_bytes = number("original_bytes")?;
    let text = |key: &str| word(binding, key).unwrap_or_default();
    if state == "retained"
        && (kept_bytes != original_bytes || text("kept_sha256") != text("original_sha256"))
    {
        return Err(
            "the binding says retained, but its kept bytes are not the whole: a prefix must say \
             partial"
                .into(),
        );
    }
    if kept_bytes > MAX_GATE_LOG_BYTES {
        return Err(format!(
            "the binding claims {kept_bytes} inflated bytes, past the conductor's ceiling of \
             {MAX_GATE_LOG_BYTES}; nothing was inflated"
        ));
    }
    // THE BOUND TRAILER: every line that states exactly the binding. A
    // trailer printed by anything else (a test, a replay) differs in a
    // hash, and is passed over; the first bound one whose frames verify
    // is the record.
    let lines: Vec<&str> = log.lines().collect();
    let mut seen = 0usize;
    let mut first_failure: Option<String> = None;
    for (at, line) in lines.iter().enumerate() {
        let Some(fields) = line.strip_prefix(LOG_TRAILER_PREFIX) else {
            continue;
        };
        seen += 1;
        if !trailer_states(fields, &expected) {
            continue;
        }
        match gzip_before(&lines[..at], frames, gz_bytes, &text("gz_sha256"))
            .and_then(|gz| inflated_matches(&gz, kept_bytes, &text("kept_sha256")).map(|()| gz))
        {
            Ok(gz) => {
                let mut account = json!({
                    "state": if state == "retained" { "verified" } else { "partial" },
                    "stream": GATE_LOG_STREAM,
                    "job": job,
                    "original_bytes": original_bytes,
                    "original_sha256": text("original_sha256"),
                    "kept_bytes": kept_bytes,
                    "kept_sha256": text("kept_sha256"),
                    "gz_bytes": gz_bytes,
                    "gz_sha256": text("gz_sha256"),
                    "frames": frames,
                    "checked": "each frame's sha256, then the gzip's bytes and sha256, then the \
                                inflated bytes and sha256, against the receipt's \
                                log_retention.gate_log",
                    "stored": format!(
                        "after the verdict: the conductor attaches this gzip to the verdict \
                         step and says where under the gate-run's `{GATE_LOG_RETENTION_KEY}`"
                    ),
                });
                if state == "partial" {
                    account["reason"] = json!(format!(
                        "the runner kept a prefix: {}",
                        quoted(binding.get("reason").and_then(Value::as_str).unwrap_or(""))
                    ));
                }
                return Ok((account, Some(gz)));
            }
            Err(why) => {
                first_failure.get_or_insert(why);
            }
        }
    }
    Err(first_failure.unwrap_or_else(|| {
        format!(
            "no gate-runner: log-end v1 line in the pod log matches the receipt's \
             log_retention.gate_log for Job {job} ({seen} log-end line(s) seen)"
        )
    }))
}

/// Does this trailer state exactly the binding's fields? A field stated
/// twice, or missing, is not a match; `stream` must be gate.log.
fn trailer_states(fields: &str, expected: &[(&str, String)]) -> bool {
    let head = fields.split_once(" reason=").map_or(fields, |(h, _)| h);
    let mut stated: HashMap<&str, &str> = HashMap::new();
    for field in head.split(' ') {
        let Some((key, value)) = field.split_once('=') else {
            return false;
        };
        if stated.insert(key, value).is_some() {
            return false;
        }
    }
    stated.get("stream") == Some(&GATE_LOG_STREAM)
        && expected
            .iter()
            .all(|(key, value)| stated.get(key) == Some(&value.as_str()))
}

/// The gzip the `frames` lines directly before a trailer carry, checked
/// frame by frame and then whole.
fn gzip_before(
    before: &[&str],
    frames: u64,
    gz_bytes: u64,
    gz_sha256: &str,
) -> std::result::Result<Vec<u8>, String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let n = usize::try_from(frames).map_err(|_| "a frame count past this platform".to_string())?;
    if n > before.len() {
        return Err(format!(
            "the trailer names {n} frame(s) and only {} line(s) precede it",
            before.len()
        ));
    }
    let mut gz = Vec::new();
    for (i, line) in before[before.len() - n..].iter().enumerate() {
        let Some(body) = line.strip_prefix(LOG_FRAME_PREFIX) else {
            return Err(format!(
                "line {i} of the {n} before the trailer is not a frame: {}",
                quoted(line)
            ));
        };
        let Ok(Value::Object(frame)) = serde_json::from_str::<Value>(body) else {
            return Err(format!("frame {i} is not a JSON object"));
        };
        let mut keys: Vec<&str> = frame.keys().map(String::as_str).collect();
        keys.sort_unstable();
        if keys != ["b64", "i", "of", "sha256", "stream"] {
            return Err(format!(
                "frame {i} does not carry exactly stream, i, of, b64, sha256"
            ));
        }
        if frame["stream"] != GATE_LOG_STREAM
            || frame["i"].as_u64() != Some(i as u64)
            || frame["of"].as_u64() != Some(frames)
        {
            return Err(format!(
                "frame {i} of {n} says it is frame {} of {} of {}: the frames are out of order \
                 or cut",
                quoted(&frame["i"].to_string()),
                quoted(&frame["of"].to_string()),
                quoted(&frame["stream"].to_string())
            ));
        }
        let chunk = frame["b64"]
            .as_str()
            .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
            .ok_or_else(|| format!("frame {i}'s b64 is not base64"))?;
        if Some(hex::encode(Sha256::digest(&chunk)).as_str()) != frame["sha256"].as_str() {
            return Err(format!("frame {i}'s bytes are not its own sha256"));
        }
        gz.extend_from_slice(&chunk);
    }
    if gz.len() as u64 != gz_bytes || hex::encode(Sha256::digest(&gz)) != gz_sha256 {
        return Err(format!(
            "the frames hold {} gzip bytes with sha256 {}, not the binding's {gz_bytes} with \
             {gz_sha256}",
            gz.len(),
            hex::encode(Sha256::digest(&gz))
        ));
    }
    Ok(gz)
}

/// Do these gzip bytes inflate to exactly `kept_bytes` with
/// `kept_sha256`? Read in blocks, and refused the moment they pass the
/// stated size.
fn inflated_matches(
    gz: &[u8],
    kept_bytes: u64,
    kept_sha256: &str,
) -> std::result::Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut inflater = flate2::read::GzDecoder::new(gz);
    let mut hasher = Sha256::new();
    let mut block = vec![0u8; INFLATE_BLOCK];
    let mut total: u64 = 0;
    loop {
        let n = inflater
            .read(&mut block)
            .map_err(|e| format!("the gzip does not inflate: {e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > kept_bytes {
            return Err(format!(
                "the gzip inflates past the {kept_bytes} bytes the binding states; refused there"
            ));
        }
        hasher.update(&block[..n]);
    }
    let digest = hex::encode(hasher.finalize());
    if total != kept_bytes || digest != kept_sha256 {
        return Err(format!(
            "the gzip inflates to {total} bytes with sha256 {digest}, not the binding's \
             {kept_bytes} with {kept_sha256}"
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    pub(in crate::train) const JOB: &str = "gate-fix-x-abc12";

    pub(in crate::train) fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// A gate.log whose one cause sits in the MIDDLE, before a long
    /// passing tail — the shape the 300-line replay cannot reach.
    pub(in crate::train) fn middle_failure() -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..6000 {
            writeln!(out, "test a::passes_{i} ... ok").unwrap();
        }
        out.extend_from_slice(b"thread 'a::the_cause' panicked at src/x.rs:9: the one cause\r\n");
        out.extend_from_slice(&[0xff, 0xfe, b'\n']);
        for i in 0..13000 {
            writeln!(out, "test b::tail_{i} ... ok").unwrap();
        }
        out
    }

    /// What an honest collector leaves for `kept` of a whole `original`:
    /// the frame lines, the trailer and the receipt's binding.
    pub(in crate::train) struct Framed {
        pub(in crate::train) frames: Vec<String>,
        pub(in crate::train) trailer: String,
        pub(in crate::train) binding: Value,
    }

    pub(in crate::train) fn framed_as(
        job: &str,
        original: &[u8],
        kept: &[u8],
        state: &str,
    ) -> Framed {
        let packed = gz(kept);
        let chunks: Vec<&[u8]> = packed.chunks(3072).collect();
        let frames = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{LOG_FRAME_PREFIX}{}",
                    json!({"stream": "gate.log", "i": i, "of": chunks.len(),
                           "b64": base64::Engine::encode(
                               &base64::engine::general_purpose::STANDARD, c),
                           "sha256": sha(c)})
                )
            })
            .collect();
        let binding = json!({
            "version": 1, "stream": "gate.log", "state": state, "reason": null,
            "original_bytes": original.len(), "original_sha256": sha(original),
            "kept_bytes": kept.len(), "kept_sha256": sha(kept),
            "encoding": "gzip", "gz_bytes": packed.len(), "gz_sha256": sha(&packed),
            "frames": chunks.len(), "job": job,
        });
        Framed {
            frames,
            trailer: trailer_of(&binding),
            binding,
        }
    }

    /// The trailer an honest collector prints for `binding`.
    fn trailer_of(b: &Value) -> String {
        let w = |k: &str| match &b[k] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        format!(
            "{LOG_TRAILER_PREFIX}job={} stream=gate.log bytes={} sha256={} kept_bytes={} \
             kept_sha256={} gz_bytes={} gz_sha256={} frames={} state={}",
            w("job"),
            w("original_bytes"),
            w("original_sha256"),
            w("kept_bytes"),
            w("kept_sha256"),
            w("gz_bytes"),
            w("gz_sha256"),
            w("frames"),
            w("state")
        )
    }

    pub(in crate::train) fn framed(original: &[u8]) -> Framed {
        framed_as(JOB, original, original, "retained")
    }

    pub(in crate::train) fn pod_log(frames: &[String], trailer: &str) -> String {
        let mut log = String::from("gate-runner: building 6-wide\n");
        for f in frames {
            log.push_str(f);
            log.push('\n');
        }
        log.push_str(trailer);
        log.push_str("\ngate-runner: receipt {}\n");
        log
    }

    pub(in crate::train) fn receipt_of(binding: &Value) -> Map<String, Value> {
        json!({"verdict": "failed", "log_retention": {"gate_log": binding}})
            .as_object()
            .unwrap()
            .clone()
    }

    fn verify(f: &Framed) -> Value {
        verify_gate_log(
            &pod_log(&f.frames, &f.trailer),
            JOB,
            &receipt_of(&f.binding),
        )
    }

    fn refused_naming(got: &Value, words: &str) {
        assert_eq!(got["state"], "refused", "{got}");
        let why = got["reason"].as_str().unwrap_or_default();
        assert!(
            why.contains(words),
            "the reason should name {words:?}: {got}"
        );
    }

    #[test]
    fn whole_frames_for_this_job_are_verified_byte_for_byte() {
        let data = middle_failure();
        let f = framed(&data);
        let got = verify(&f);
        assert_eq!(got["state"], "verified", "{got}");
        assert_eq!(got["original_bytes"], data.len());
        assert_eq!(got["original_sha256"], sha(&data));
        assert_eq!(got["gz_sha256"], f.binding["gz_sha256"]);
        assert_eq!(got["frames"], f.frames.len());
        assert_eq!(got["job"], JOB);
    }

    /// THE FORMAT LIVES TWICE — the collector writes it in Python, this
    /// reads it in Rust (CLAUDE.md §9a) — so the pin runs the REAL
    /// collector on a log whose only cause is in the middle and hands its
    /// stdout and receipt to the verifier.
    #[test]
    fn the_collectors_own_frames_and_receipt_are_verified() {
        let dir = boss_testing::scratch_dir("gate-log-verify");
        let (log_path, receipt_path) = (dir.join("gate.log"), dir.join("receipt.json"));
        let data = middle_failure();
        std::fs::write(&log_path, &data).unwrap();
        std::fs::write(&receipt_path, r#"{"verdict":"failed"}"#).unwrap();
        let out = std::process::Command::new("python3")
            .arg(boss_testing::repo_root().join("infra/gate-runner/runtime-evidence.py"))
            .arg("retain-log")
            .arg(&log_path)
            .arg(&receipt_path)
            .env("NATIVE_JOB_NAME", JOB)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let receipt: Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        let mut log = String::from("gate-runner: building 6-wide\n");
        log.push_str(std::str::from_utf8(&out.stdout).unwrap());
        log.push_str("=== gate-runner: replaying failed checks from gate.log ===\n");
        let got = verify_gate_log(&log, JOB, receipt.as_object().unwrap());
        assert_eq!(got["state"], "verified", "{got}");
        assert_eq!(got["original_sha256"], sha(&data));
        assert_eq!(got["original_bytes"], data.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The recorder writes its account into `recorded_by.gate_log`,
    /// beside the runner's binding and never in place of the verdict.
    #[test]
    fn the_recorded_verdict_carries_the_conductors_account_of_its_gate_log() {
        let f = framed(&middle_failure());
        let payload = json!({"verdict": "failed", "head": "a".repeat(40),
                             "log_retention": {"gate_log": f.binding}})
        .to_string();
        let end = format!(
            "gate-runner: receipt-end v1 job={JOB} verdict=failed bytes={} sha256={}",
            payload.len(),
            sha(payload.as_bytes())
        );
        let mut log = pod_log(&f.frames, &f.trailer);
        log.push_str(&format!("gate-runner: receipt {payload}\n{end}\n"));
        let gate = GateJob {
            name: JOB.into(),
            uid: "uid-1".into(),
            packet: "run-1".into(),
            created: "2026-10-06T19:00:00Z".into(),
            carrier: true,
            judged_init: false,
            finished: None,
        };
        let run = json!({"id": "run-1", "kind": "gate-run", "status": "open",
                         "metadata": {"sha": "a".repeat(40)}, "steps": []});
        let Carried::Record { verdict, receipt } = judge(
            &run,
            &gate,
            &fixtures::ended_on(&end, 1, "Error"),
            &log,
            "boss-dev",
            Utc::now(),
        ) else {
            panic!("a complete frame is recorded");
        };
        assert_eq!(verdict, "failed");
        let got: Value = serde_json::from_str(&receipt).unwrap();
        assert_eq!(got["recorded_by"]["gate_log"]["state"], "verified", "{got}");
        assert_eq!(
            got["log_retention"]["gate_log"], f.binding,
            "the runner's binding is kept"
        );
    }

    #[test]
    fn a_frame_whose_bytes_changed_is_refused() {
        let mut f = framed(&middle_failure());
        let mut frame: Value =
            serde_json::from_str(f.frames[3].strip_prefix(LOG_FRAME_PREFIX).unwrap()).unwrap();
        let b64 = frame["b64"].as_str().unwrap().to_string();
        let flipped = if b64.starts_with('A') { "B" } else { "A" };
        frame["b64"] = json!(format!("{flipped}{}", &b64[1..]));
        f.frames[3] = format!("{LOG_FRAME_PREFIX}{frame}");
        refused_naming(&verify(&f), "frame 3");
    }

    #[test]
    fn frames_out_of_order_are_refused() {
        let mut f = framed(&middle_failure());
        f.frames.swap(1, 2);
        refused_naming(&verify(&f), "frame 1");
    }

    #[test]
    fn a_missing_frame_is_refused() {
        let mut f = framed(&middle_failure());
        f.frames.remove(5);
        refused_naming(&verify(&f), "frame");
    }

    #[test]
    fn a_line_between_the_frames_is_refused() {
        let mut f = framed(&middle_failure());
        f.frames
            .insert(4, "gate-runner: an interleaved line".into());
        // The N lines before the trailer now start one frame late.
        refused_naming(&verify(&f), "out of order or cut");
    }

    #[test]
    fn frames_bound_to_another_job_are_refused() {
        // The binding names another Job: refused before the log is read.
        let f = framed_as("gate-someone-else", b"x\n", b"x\n", "retained");
        refused_naming(&verify(&f), "another Job");
        // The binding is this Job's, the only trailer another's: no
        // trailer in the log is the bound one.
        let mine = framed(b"x\n");
        let theirs = framed_as("gate-someone-else", b"x\n", b"x\n", "retained");
        let got = verify_gate_log(
            &pod_log(&mine.frames, &theirs.trailer),
            JOB,
            &receipt_of(&mine.binding),
        );
        refused_naming(&got, "matches the receipt");
    }

    /// A trailer and frames printed earlier — by a test, a replay, a
    /// forger — that do not state the bound hashes are not the record.
    #[test]
    fn earlier_frames_that_do_not_match_the_binding_do_not_displace_it() {
        let forged = framed(b"all green, nothing to see\n");
        let honest = framed(&middle_failure());
        let mut log = pod_log(&forged.frames, &forged.trailer);
        log.push_str(&pod_log(&honest.frames, &honest.trailer));
        let got = verify_gate_log(&log, JOB, &receipt_of(&honest.binding));
        assert_eq!(got["state"], "verified", "{got}");
        assert_eq!(got["original_sha256"], honest.binding["original_sha256"]);
    }

    #[test]
    fn a_receipt_without_a_binding_is_absent_and_an_unavailable_one_says_why() {
        let absent = verify_gate_log(
            "",
            JOB,
            &json!({"verdict": "green"}).as_object().unwrap().clone(),
        );
        assert_eq!(absent["state"], "absent", "{absent}");
        let unavailable = verify_gate_log(
            "",
            JOB,
            &receipt_of(
                &json!({"state": "unavailable", "job": JOB, "stream": "gate.log",
                                "reason": "gate.log unavailable: no such file"}),
            ),
        );
        assert_eq!(unavailable["state"], "unavailable", "{unavailable}");
        assert!(
            unavailable["reason"]
                .as_str()
                .unwrap()
                .contains("no\\u{20}such\\u{20}file"),
            "the runner's own reason is quoted: {unavailable}"
        );
    }

    #[test]
    fn a_partial_is_a_verified_prefix_of_a_stated_whole() {
        let whole = middle_failure();
        let prefix = &whole[..64 * 1024];
        let f = framed_as(JOB, &whole, prefix, "partial");
        let got = verify(&f);
        assert_eq!(got["state"], "partial", "{got}");
        assert_eq!(got["kept_bytes"], prefix.len());
        assert_eq!(got["kept_sha256"], sha(prefix));
        assert_eq!(got["original_bytes"], whole.len());
    }

    #[test]
    fn a_retained_log_whose_kept_bytes_are_not_the_whole_is_refused() {
        let whole = middle_failure();
        let f = framed_as(JOB, &whole, &whole[..1000], "retained");
        refused_naming(&verify(&f), "not the whole");
    }

    /// Inflation stops at the stated size: a gzip that claims ten bytes
    /// and inflates to a megabyte is refused there, not hashed to the end.
    #[test]
    fn inflating_past_the_stated_size_is_refused() {
        let big = vec![b'a'; 1 << 20];
        let mut f = framed(&big);
        for key in ["kept_bytes", "original_bytes"] {
            f.binding[key] = json!(10);
        }
        f.trailer = trailer_of(&f.binding);
        refused_naming(&verify(&f), "past the");
    }

    #[test]
    fn a_claim_past_the_ceiling_is_refused_before_inflating() {
        let mut f = framed(b"x\n");
        let over = MAX_GATE_LOG_BYTES + 1;
        for key in ["kept_bytes", "original_bytes"] {
            f.binding[key] = json!(over);
        }
        f.trailer = trailer_of(&f.binding);
        refused_naming(&verify(&f), "ceiling");
    }
}
