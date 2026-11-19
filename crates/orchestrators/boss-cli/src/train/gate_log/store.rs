//! The conductor STORES the gate.log it verified (backlog 7c49bd38;
//! design 5a274e7c car 2, second piece, decided 2026-11-13).
//!
//! WHY. The first piece of car 2 (`gate_log.rs`) checks the runner's
//! frames and records its account in the verdict's `recorded_by.gate_log`
//! — and the bytes it checked still died with the pod log, 24 h after the
//! Job ended. All 33 W42 gate losses (retro 9612e4fe) were Job-log
//! expiries, and the one cause of a red sits in the MIDDLE of gate.log,
//! where the receipt's excerpts and the 300-line replay do not reach. So
//! once the verdict is recorded, the conductor attaches the verified gzip
//! to the gate-run's verdict step in the file store — file_refs is on,
//! content-addressed and backed up — and says where under the gate-run's
//! [`GATE_LOG_RETENTION_KEY`]. The runner holds no token (designs
//! c395e62c, bdc60b65), so the conductor, which already reads the log for
//! the verdict, is the writer.
//!
//! THE ORDER IS THE DESIGN'S `Failure` LINE. The verdict is recorded
//! first and never waits on the store. Storage runs on a LATER pass, from
//! the branch of the recorder that sees a verdict already written — the
//! seed-housekeeping record's shape — so a store that is down costs the
//! verdict nothing. A failed upload is recorded `unavailable` with its
//! reason, and is tried again on every pass while the cluster still lists
//! the Job, whose log is the only copy; the same failure twice is written
//! once (the merge door records an event per write).
//!
//! WHAT IS STORED: the gzip `verified_gate_log` checked, byte for byte —
//! never re-read, re-framed or re-compressed — cut into parts the store's
//! body limit takes ([`crate::attach::largest_file`]), each attached
//! through `attach::attach_at`, which reads it back by id and refuses
//! unless the bytes sent, the row's sha256 and the bytes read back are
//! one digest. A retry is idempotent: the store answers a part the step
//! already holds with that attachment (backlog e5490f2e), so an
//! interrupted upload resumes and never duplicates.
//!
//! NOT YET (car 3 of the design): the reader verb, file ids on the
//! receipt's excerpts, the outer pod log, and the expiry rule (30 days
//! green, 180 otherwise).

use super::*;

/// The gate-run metadata key the storage record is written under. On the
/// PACKET, not the verdict step: the step is completed by the verdict,
/// and the jobs API refuses metadata on a completed step.
pub(crate) const GATE_LOG_RETENTION_KEY: &str = "gate_log_retention";

/// How many gate-runs one recorder pass stores. The pass runs at the top
/// of every reconcile — the verb that merges trains — and each store is a
/// pod-log read (5 MB of text measured on gate-run 803b0d69, 2026-11-18)
/// plus an upload, under the verdict lock. Steady state is about 110
/// gates a day, one per pass at most; the bound matters on the first
/// pass after this lands, when a day of recorded runs is owed at once,
/// and on a pass after the store was down. The rest wait one pass; a
/// Job's log outlives a pass by 24 h.
pub(crate) const GATE_LOGS_PER_PASS: usize = 8;

/// The verdict step the parts attach to.
const VERDICT_STEP: (&str, &str) = ("record-verdict", "Record the gate verdict");

/// What a gate-run owes the file store: the Job whose log holds the
/// frames, the verdict step the parts attach to, and the recorded
/// receipt whose `log_retention.gate_log` binds them.
#[derive(Debug, Clone)]
pub(crate) struct RetentionOwed<'a> {
    pub(crate) job: &'a GateJob,
    pub(crate) step_id: String,
    pub(crate) receipt: Map<String, Value>,
}

/// PURE: does this gate-run owe its gate.log to the file store? Only
/// once its verdict is recorded; only when that verdict's receipt says
/// the conductor VERIFIED the frames (`verified` or `partial` — a refused
/// or absent account has nothing to store, and says why already); only
/// while the cluster still lists the Job that account names, because its
/// log is where the bytes are; and only while the packet carries no
/// record, or carries an `unavailable` one, which is retried.
pub(crate) fn retention_owed<'a>(run: &Value, jobs: &'a [GateJob]) -> Option<RetentionOwed<'a>> {
    if run.get("kind").and_then(Value::as_str) != Some("gate-run") || verdict_owed(run).is_ok() {
        return None;
    }
    if let Some(record) = metadata_map(run).get(GATE_LOG_RETENTION_KEY)
        && record.get("state").and_then(Value::as_str) != Some("unavailable")
    {
        return None;
    }
    let step = find_step(run, VERDICT_STEP.0, VERDICT_STEP.1)?;
    let step_id = step.get("id").and_then(Value::as_str)?.to_string();
    let Ok(Value::Object(receipt)) =
        serde_json::from_str::<Value>(step.pointer("/metadata/receipt").and_then(Value::as_str)?)
    else {
        return None;
    };
    let account = receipt.get("recorded_by")?.get("gate_log")?;
    if !matches!(
        account.get("state").and_then(Value::as_str),
        Some("verified" | "partial")
    ) {
        return None;
    }
    let name = account.get("job").and_then(Value::as_str)?;
    let job = jobs.iter().find(|j| j.name == name)?;
    Some(RetentionOwed {
        job,
        step_id,
        receipt,
    })
}

/// PURE: the record of a store that could not take the log this pass —
/// the reason, and the condition it is retried under.
pub(crate) fn unavailable_record(job: &str, reason: &str, at: DateTime<Utc>) -> Value {
    json!({
        "state": "unavailable",
        "stream": "gate.log",
        "job": job,
        "reason": reason,
        "attempted_at": crate::sor_client::stamp(at),
        "retried": "on every conductor pass while the cluster lists the gate Job, whose pod \
                    log (kept 24 h after it ends) is the only copy",
    })
}

/// PURE: is `record` the same failure `previous` already says? Then it is
/// not written again: one event per change, not one per pass.
pub(crate) fn same_failure(previous: Option<&Value>, record: &Value) -> bool {
    let state = |v: &Value| v.get("state").and_then(Value::as_str).map(str::to_string);
    let reason = |v: &Value| v.get("reason").and_then(Value::as_str).map(str::to_string);
    previous.is_some_and(|p| {
        state(p).as_deref() == Some("unavailable")
            && state(record).as_deref() == Some("unavailable")
            && reason(p) == reason(record)
    })
}

/// The part names, in order: `<job>.gate.log.gz` whole, or
/// `<job>.gate.log.gz.<i>-of-<n>` (from 1) when the store needs several.
fn part_name(job: &str, index: usize, of: usize) -> String {
    if of == 1 {
        format!("{job}.gate.log.gz")
    } else {
        format!("{job}.gate.log.gz.{}-of-{of}", index + 1)
    }
}

/// Attach one part, from a file of its own name in a private temporary
/// directory: `attach_at` streams a file from disk, names the attachment
/// after it, and reads it back by id.
async fn attach_part(
    http: &crate::machine_http::MachineClient,
    content: &str,
    target: &crate::attach::Target,
    name: &str,
    bytes: &[u8],
    signer: &str,
) -> Result<crate::attach::Attached> {
    let dir = tempfile::Builder::new()
        .prefix("boss-gate-log-")
        .tempdir()
        .context("creating the part's temporary directory")?;
    let path = dir.path().join(name);
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    crate::attach::attach_at(
        http,
        content,
        target,
        &path,
        crate::identity::Signature::As(signer.to_string()),
    )
    .await
}

/// Store the gate.log `owed` names, from the pod `log` it was read from,
/// in the file store at `content`, signed as `signer`. Never an error:
/// the answer is always the record to write — `retained` or `partial`
/// with every part's file id, `refused` when the log no longer verifies
/// (written once, not retried: the frames will not change), or
/// `unavailable` with the reason.
pub(crate) async fn retain_gate_log(
    http: &crate::machine_http::MachineClient,
    content: &str,
    owed: &RetentionOwed<'_>,
    log: &str,
    signer: &str,
    now: DateTime<Utc>,
) -> Value {
    let job = owed.job.name.as_str();
    let VerifiedGateLog { account, gzip } = verified_gate_log(log, job, &owed.receipt);
    let Some(gzip) = gzip else {
        // The recorded account said verified; this read does not. Said,
        // and final: the frames are the log's, and a log is never
        // rewritten, only cut or gone.
        return json!({
            "state": "refused",
            "stream": "gate.log",
            "job": job,
            "reason": format!(
                "the pod log read after the verdict no longer verifies ({}): {}",
                account["state"].as_str().unwrap_or("unknown"),
                account["reason"].as_str().unwrap_or("")
            ),
            "attempted_at": crate::sor_client::stamp(now),
        });
    };
    let target = crate::attach::Target {
        kind: "step",
        id: owed.step_id.clone(),
        label: format!("the verdict step of Job {job}'s gate-run"),
    };
    let size = usize::try_from(crate::attach::largest_file()).unwrap_or(usize::MAX);
    let chunks: Vec<&[u8]> = gzip.chunks(size).collect();
    let mut parts = Vec::with_capacity(chunks.len());
    let mut offset = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        let name = part_name(job, index, chunks.len());
        match attach_part(http, content, &target, &name, chunk, signer).await {
            Ok(a) => parts.push(json!({
                "index": index,
                "offset": offset,
                "file_id": a.file_id,
                "filename": a.filename,
                "sha256": a.sha256,
                "size_bytes": a.size_bytes,
            })),
            Err(e) => {
                return unavailable_record(
                    job,
                    &format!(
                        "part {} of {} ({name}, {} bytes) was not stored: {e:#}",
                        index + 1,
                        chunks.len(),
                        chunk.len()
                    ),
                    now,
                );
            }
        }
        offset += chunk.len();
    }
    let mut record = json!({
        "state": if account["state"] == "partial" { "partial" } else { "retained" },
        "stream": "gate.log",
        "job": job,
        "encoding": "gzip",
        "attached_to": {"kind": "step", "id": owed.step_id, "slug": VERDICT_STEP.0},
        "parts": parts,
        "read_back": "each part read back by its file id byte-identical to the bytes sent; the \
                      parts in order are the gzip the conductor verified, whose bytes and sha256 \
                      are gz_bytes and gz_sha256",
        "stored_by": signer,
        "stored_at": crate::sor_client::stamp(now),
    });
    for key in [
        "original_bytes",
        "original_sha256",
        "kept_bytes",
        "kept_sha256",
        "gz_bytes",
        "gz_sha256",
        "reason",
    ] {
        if let Some(v) = account.get(key) {
            record[key] = v.clone();
        }
    }
    record
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        Framed, JOB, framed, framed_as, middle_failure, pod_log, receipt_of, sha,
    };
    use super::*;

    const STEP: &str = "9ec2df60-9177-4377-bee7-6c1d0739bb15";

    fn job(name: &str) -> GateJob {
        GateJob {
            name: name.into(),
            uid: "uid-1".into(),
            packet: "run-1".into(),
            created: "2026-11-18T19:00:00Z".into(),
            carrier: true,
            judged_init: false,
            finished: None,
        }
    }

    /// The receipt the recorder wrote for `f`: the runner's binding plus
    /// the conductor's account of it, as `judge_by` records it.
    fn recorded_receipt(f: &Framed) -> Map<String, Value> {
        let mut r = receipt_of(&f.binding);
        let account = verify_gate_log(&pod_log(&f.frames, &f.trailer), JOB, &r);
        r.insert("recorded_by".into(), json!({"gate_log": account}));
        r
    }

    /// A gate-run whose verdict is recorded with `receipt`, carrying
    /// `retention` under the key when given.
    fn recorded_run(receipt: &Map<String, Value>, retention: Option<Value>) -> Value {
        let mut run = json!({
            "id": "run-1", "kind": "gate-run", "status": "closed", "metadata": {},
            "steps": [{"id": STEP, "spec_slug": "record-verdict",
                       "title": "Record the gate verdict", "status": "completed",
                       "metadata": {"verdict": "failed",
                                    "receipt": Value::Object(receipt.clone()).to_string()}}]
        });
        if let Some(r) = retention {
            run["metadata"][GATE_LOG_RETENTION_KEY] = r;
        }
        run
    }

    fn client() -> crate::machine_http::MachineClient {
        crate::sor_client::machine_client().unwrap()
    }

    /// Every part's bytes, read back from the store by its file id, in
    /// order.
    async fn read_parts(base: &str, record: &Value) -> Vec<u8> {
        let mut whole = Vec::new();
        for part in record["parts"].as_array().expect("parts") {
            let id = part["file_id"].as_str().unwrap();
            let bytes = client()
                .get(format!("{base}/api/files/{id}"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(part["offset"], whole.len(), "{record:#}");
            whole.extend_from_slice(&bytes);
        }
        whole
    }

    fn inflate(gz: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    /// THE ACCEPTANCE SHAPE (7c49bd38): the one cause in the MIDDLE of a
    /// long passing tail is stored, retrieved independently of the pod by
    /// file id, and inflates to exactly the bytes and sha256 the runner
    /// bound — attached to the verdict step, not the packet.
    #[tokio::test]
    async fn a_verified_gate_log_is_stored_on_the_verdict_step_and_reads_back_whole() {
        let data = middle_failure();
        let f = framed(&data);
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        let run = recorded_run(&receipt, None);
        let owed = retention_owed(&run, &jobs).expect("owed");
        assert_eq!(owed.step_id, STEP);
        let base = crate::attach::tests::store().await;
        let log = pod_log(&f.frames, &f.trailer);
        let record = retain_gate_log(
            &client(),
            &base,
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(record["state"], "retained", "{record:#}");
        assert_eq!(record["attached_to"]["kind"], "step");
        assert_eq!(record["attached_to"]["id"], STEP);
        assert_eq!(record["original_sha256"], sha(&data));
        assert_eq!(record["parts"][0]["filename"], format!("{JOB}.gate.log.gz"));
        let gz = read_parts(&base, &record).await;
        assert_eq!(sha(&gz), f.binding["gz_sha256"].as_str().unwrap());
        let whole = inflate(&gz);
        assert_eq!(whole, data, "the original, byte for byte");
        assert!(
            String::from_utf8_lossy(&whole).contains("the one cause"),
            "the middle survives"
        );
        // Listed on the verdict step it was attached to.
        let listed: Value = client()
            .get(format!(
                "{base}/api/files?target_kind=step&target_id={STEP}"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.as_array().map(Vec::len), Some(1), "{listed:#}");
    }

    /// Past the store's body limit the gzip goes in ordered parts, each
    /// within it, which concatenate to the verified gzip.
    #[tokio::test]
    async fn a_gzip_past_the_body_limit_is_stored_in_ordered_parts() {
        // Incompressible bytes, so the gzip is as large as the log.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let data: Vec<u8> = (0..(5 * 1024 * 1024 / 2))
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x & 0xff) as u8
            })
            .collect();
        let f = framed(&data);
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        let run = recorded_run(&receipt, None);
        let owed = retention_owed(&run, &jobs).expect("owed");
        let base = crate::attach::tests::store().await;
        let log = pod_log(&f.frames, &f.trailer);
        let record = retain_gate_log(
            &client(),
            &base,
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(record["state"], "retained", "{record:#}");
        let parts = record["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "{record:#}");
        assert_eq!(parts[1]["filename"], format!("{JOB}.gate.log.gz.2-of-2"));
        for p in parts {
            assert!(p["size_bytes"].as_u64().unwrap() <= crate::attach::largest_file());
        }
        let gz = read_parts(&base, &record).await;
        assert_eq!(sha(&gz), f.binding["gz_sha256"].as_str().unwrap());
        assert_eq!(inflate(&gz), data);
    }

    /// An upload that fails is `unavailable` with its reason, and owed
    /// again; the retry stores it, and a third pass reuses the same file,
    /// never a second copy.
    #[tokio::test]
    async fn a_failed_upload_is_unavailable_and_its_retry_stores_once() {
        let f = framed(&middle_failure());
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        let log = pod_log(&f.frames, &f.trailer);
        let run = recorded_run(&receipt, None);
        let owed = retention_owed(&run, &jobs).expect("owed");
        let down = retain_gate_log(
            &client(),
            "http://127.0.0.1:9",
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(down["state"], "unavailable", "{down:#}");
        assert!(
            down["reason"].as_str().unwrap().contains("part 1 of 1"),
            "{down:#}"
        );

        let run = recorded_run(&receipt, Some(down));
        let owed = retention_owed(&run, &jobs).expect("an unavailable record is owed again");
        let base = crate::attach::tests::store().await;
        let first = retain_gate_log(
            &client(),
            &base,
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(first["state"], "retained", "{first:#}");
        let again = retain_gate_log(
            &client(),
            &base,
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(again["parts"][0]["file_id"], first["parts"][0]["file_id"]);

        let run = recorded_run(&receipt, Some(first));
        assert!(retention_owed(&run, &jobs).is_none(), "stored is final");
    }

    /// A runner that kept only a prefix is stored as `partial`, with the
    /// stated whole beside the kept bytes — never as the whole.
    #[tokio::test]
    async fn a_partial_is_stored_as_partial_with_the_stated_whole() {
        let whole = middle_failure();
        let prefix = &whole[..64 * 1024];
        let f = framed_as(JOB, &whole, prefix, "partial");
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        let run = recorded_run(&receipt, None);
        let owed = retention_owed(&run, &jobs).expect("owed");
        let base = crate::attach::tests::store().await;
        let log = pod_log(&f.frames, &f.trailer);
        let record = retain_gate_log(
            &client(),
            &base,
            &owed,
            &log,
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(record["state"], "partial", "{record:#}");
        assert_eq!(record["original_bytes"], whole.len());
        assert_eq!(record["kept_bytes"], prefix.len());
        assert_eq!(inflate(&read_parts(&base, &record).await), prefix);
    }

    /// A log that no longer verifies is refused by name, nothing is
    /// uploaded, and the refusal is final.
    #[tokio::test]
    async fn a_log_that_no_longer_verifies_is_refused_and_nothing_is_stored() {
        let f = framed(&middle_failure());
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        let run = recorded_run(&receipt, None);
        let owed = retention_owed(&run, &jobs).expect("owed");
        let mut cut = f.frames.clone();
        cut.remove(2);
        let record = retain_gate_log(
            &client(),
            "http://127.0.0.1:9",
            &owed,
            &pod_log(&cut, &f.trailer),
            "automation:train-conductor",
            Utc::now(),
        )
        .await;
        assert_eq!(record["state"], "refused", "{record:#}");
        assert!(record.get("parts").is_none());
        let run = recorded_run(&receipt, Some(record));
        assert!(retention_owed(&run, &jobs).is_none());
    }

    /// Owed only after the verdict, only for a verified account, and only
    /// while the cluster still lists the Job that account names.
    #[test]
    fn retention_is_owed_only_for_a_recorded_verified_log_whose_job_is_listed() {
        let f = framed(b"x\n");
        let receipt = recorded_receipt(&f);
        let jobs = [job(JOB)];
        assert!(retention_owed(&recorded_run(&receipt, None), &jobs).is_some());
        // The Job is gone: its log with it.
        assert!(retention_owed(&recorded_run(&receipt, None), &[job("gate-other")]).is_none());
        // The verdict is still owed.
        let mut open = recorded_run(&receipt, None);
        open["status"] = json!("open");
        open["steps"][0]["status"] = json!("ready");
        assert!(retention_owed(&open, &jobs).is_none());
        // A receipt whose account is not verified, or not a receipt.
        let mut refused = receipt.clone();
        refused["recorded_by"]["gate_log"] = json!({"state": "refused", "reason": "x"});
        assert!(retention_owed(&recorded_run(&refused, None), &jobs).is_none());
        let mut lost = recorded_run(&receipt, None);
        lost["steps"][0]["metadata"]["receipt"] = json!("NO VERDICT WAS PRODUCED.");
        assert!(retention_owed(&lost, &jobs).is_none());
    }

    #[test]
    fn the_same_failure_is_written_once() {
        let at = Utc::now();
        let a = unavailable_record(JOB, "the store is down", at);
        let b = unavailable_record(JOB, "the store is down", at + chrono::Duration::minutes(2));
        let c = unavailable_record(JOB, "the store refused 403", at);
        assert!(same_failure(Some(&a), &b));
        assert!(!same_failure(Some(&a), &c));
        assert!(!same_failure(None, &b));
    }
}
