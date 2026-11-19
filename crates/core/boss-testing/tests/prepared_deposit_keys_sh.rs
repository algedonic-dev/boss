//! `infra/gcp/prepared-deposit-keys.sh` and the boss-gcp converge's use
//! of it are RUN, not read — against fixture records and authorized_keys
//! files in a scratch directory, with keys generated per case.
//!
//! WHY (backlog 77920d6a; review 728c6f76 of car 3cd92f7a). The
//! deposit-key audit recognised a root-door receiver line by the text of
//! its forced command, and sshd hands that command to the account's
//! shell, which reads `lib'exec'`, `bo\ss`, `$L` and `libexe?` as the
//! root door while the text match read none: alone in the file with no
//! `restrict`, such a line read clean while sshd granted a pty and
//! forwarding, and the deposit key placed bare with no confined twin read
//! as one of the owner's own logins. Car 5cc2731a taught the audit
//! `--prepared-key`; nothing passed it, so on boss-gcp every one of those
//! lines still read clean. These cases pin the half that was owed: the
//! converge reads the public key each prepare packet recorded and hands
//! it to the audit BEFORE the files (backlog 26173f40: an option after a
//! file is read as an absent file, and the set reads clean), and a
//! record it cannot read is UNVERIFIED, never a text-only clean.
//!
//! THE BREAK-GLASS KEY (backlog 403d9d7e). The break-glass kubeconfig
//! deposit key has no prepare packet — its CronJob mints it — so the
//! cases above left it judged by its command text alone. Its record is
//! the line its own Job prints on its `maintenance-break-glass-deposit`
//! packets (the authorize line on the run that minted it, and one line
//! on every run that holds it since), and the reader hands that line
//! whole, so the key is judged by fingerprint AND bound to the
//! kubeconfig receiver.
//!
//! No key here is anyone's: every one is generated into the case's
//! scratch directory, and the records are fixture files read through
//! `file://`, so nothing reaches the system of record.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER: &str = "infra/gcp/prepared-deposit-keys.sh";
const CONVERGE: &str = "infra/gcp/boss-gcp-converge.sh";

/// The `<kind> <subject>` pairs of the three deposit keys the broker
/// prepares for boss-gcp today, as infra/dispatcher/rules declares them.
const PAIRS: [(&str, &str); 3] = [
    ("prepare-a-deposit-key", "runner-deposit-key"),
    (
        "prepare-the-machine-token-deposit-key",
        "machine-token-deposit-key",
    ),
    ("prepare-the-root-tree-deposit-key", "root-tree-deposit-key"),
];

/// A fresh ed25519 pair under `dir`; its public half `<type> <blob>`.
fn keygen(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"])
        .arg(&path)
        .output()
        .expect("ssh-keygen runs (the gate image carries it)");
    assert!(
        out.status.success(),
        "ssh-keygen: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    public
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One prepare packet as the jobs list answers it with `full=true`.
fn packet(id: &str, kind: &str, subject: &str, steps: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": id, "kind": kind, "status": "closed",
        "subject": {"kind": "custom", "id": subject},
        "steps": steps,
    })
}

/// The issue and install steps the broker completes, each carrying `key`.
fn prepared_steps(key: &str) -> serde_json::Value {
    serde_json::json!([
        {"spec_slug": "scope", "status": "completed", "metadata": {}},
        {"spec_slug": "issue", "status": "completed", "metadata": {"public_key": key}},
        {"spec_slug": "install", "status": "completed", "metadata": {"public_key": key}},
        {"spec_slug": "enroll", "status": "completed", "metadata": {"receiver_host": "boss-gcp"}},
    ])
}

/// A jobs-list answer of `rows`, its `.total` the row count.
fn record(dir: &Path, name: &str, rows: Vec<serde_json::Value>) -> PathBuf {
    let total = rows.len();
    record_with_total(dir, name, rows, total)
}

fn record_with_total(
    dir: &Path,
    name: &str,
    rows: Vec<serde_json::Value>,
    total: usize,
) -> PathBuf {
    let path = dir.join(name);
    write_file(
        &path,
        &serde_json::json!({"total": total, "data": rows}).to_string(),
    );
    path
}

/// The break-glass deposit's Job, the packet kind its chore files and the
/// subject boss-maintenance-wrap.sh gives every maintenance packet
/// (`infra/<kind>`).
const DEPOSIT_JOB: &str = "infra/cluster/break-glass-deposit.sh";
const DEPOSIT_MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-deposit.yaml";
const BREAK_GLASS_KIND: &str = "maintenance-break-glass-deposit";
const BREAK_GLASS_SUBJECT: &str = "infra/maintenance-break-glass-deposit";

/// `FORCED='…'` as `script` declares it — the options of the one line
/// the break-glass deposit key may sit on.
fn forced_in(script: &str, decl: &str) -> String {
    let text = std::fs::read_to_string(repo_root().join(script)).unwrap();
    let line = text
        .lines()
        .find(|l| l.starts_with(&format!("{decl}='")))
        .unwrap_or_else(|| panic!("{script} declares {decl}='…'"));
    line.trim_start_matches(&format!("{decl}='"))
        .trim_end_matches('\'')
        .to_string()
}

/// The options the Job prints in front of its key.
fn break_glass_forced() -> String {
    forced_in(DEPOSIT_JOB, "FORCED")
}

/// The value the reader hands the audit for break-glass key `key`: the
/// whole line, so the key is bound to the kubeconfig receiver.
fn break_glass_line(key: &str) -> String {
    format!("{} {key}", break_glass_forced())
}

/// One break-glass deposit packet whose run step recorded `output`.
fn break_glass_packet(id: &str, result: &str, output: &str) -> serde_json::Value {
    packet(
        id,
        BREAK_GLASS_KIND,
        BREAK_GLASS_SUBJECT,
        serde_json::json!([
            {"spec_slug": "scheduled", "status": "completed", "metadata": {}},
            {"spec_slug": "run", "status": "completed",
             "metadata": {"result": result, "output": output}},
        ]),
    )
}

/// The run that MINTED `key`: its RED authorize line, as the Job prints it.
fn minted_output(key: &str) -> String {
    format!(
        "break-glass-deposit: minted a deposit key into Secret boss/break-glass-deposit-key; the private half never left this pod\nRED break-glass-deposit/authorize-key authorize: on boss-gcp itself, run ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub and compare it with the host key this Job pins, SHA256:HOSTKEY; only if they match, place this line in ~david/.ssh/authorized_keys there: {} break-glass-deposit@boss\n",
        break_glass_line(key)
    )
}

/// A run that HELD `key` and deposited with it.
fn deposited_output(key: &str) -> String {
    format!(
        "break-glass-deposit: the deposit key, as boss-gcp holds it: {} break-glass-deposit@boss\nbreak-glass-deposit: deposited: david@boss-gcp received the 2437 bytes sent\n",
        break_glass_line(key)
    )
}

/// The three prepare packets of today, each recording one of `keys`, and
/// one break-glass deposit packet whose run held `break_glass`.
fn todays_packets(keys: [&str; 3], break_glass: &str) -> Vec<serde_json::Value> {
    PAIRS
        .iter()
        .zip(keys)
        .enumerate()
        .map(|(i, ((kind, subject), key))| {
            packet(&format!("p{i}"), kind, subject, prepared_steps(key))
        })
        .chain([break_glass_packet(
            "bg-today",
            "ok",
            &deposited_output(break_glass),
        )])
        .collect()
}

/// An address file naming no jobs API, so sor.sh reads nothing of the
/// host's own (the dev pod may carry /etc/boss/sor.env).
fn empty_sor_env(dir: &Path) -> PathBuf {
    let path = dir.join("sor.env");
    write_file(&path, "# no address: these cases read fixtures only\n");
    path
}

fn helper(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> (i32, String, String) {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(HELPER))
        .args(args)
        .env_remove("BOSS_JOBS_URL")
        .env_remove("BOSS_GCP_PREPARED_KEYS_URL")
        .env_remove("BOSS_GCP_PREPARED_KEY_RULES")
        .env("BOSS_SOR_ENV", empty_sor_env(dir))
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-machine-token"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn url(path: &Path) -> PathBuf {
    PathBuf::from(format!("file://{}", path.display()))
}

/// The pairs are read off the dispatcher rules, not restated: every
/// rule whose handler prepares an ssh deposit key for boss-gcp names one,
/// so a new deposit key for this host is looked for with no edit here.
#[test]
fn the_deposit_keys_read_are_the_ones_the_rules_prepare_for_boss_gcp() {
    let dir = scratch_dir("prepared-deposit-keys-kinds");
    let (rc, out, err) = helper(&dir, &["--kinds"], &[]);
    assert_eq!(rc, 0, "{out}{err}");
    let want: Vec<String> = PAIRS.iter().map(|(k, s)| format!("{k} {s}")).collect();
    assert_eq!(out.lines().collect::<Vec<_>>(), want, "{err}");
}

/// Every key the record holds on an issue or install step, once each,
/// whatever the packet's status; a row of another kind or subject (a
/// file:// record answers every query alike) gives none.
#[test]
fn every_recorded_public_key_is_named_once() {
    let dir = scratch_dir("prepared-deposit-keys-named");
    let keys = [
        keygen(&dir, "runner-deposit-key"),
        keygen(&dir, "machine-token-deposit-key"),
        keygen(&dir, "root-tree-deposit-key"),
    ];
    let older = keygen(&dir, "older-machine-token-deposit-key");
    let stranger = keygen(&dir, "someone-elses-key");
    let break_glass = keygen(&dir, "break-glass-deposit-key");
    let mut rows = todays_packets([&keys[0], &keys[1], &keys[2]], &break_glass);
    // A superseded preparation of the same key: still a deposit key.
    rows.push(packet(
        "p-older",
        PAIRS[1].0,
        PAIRS[1].1,
        prepared_steps(&older),
    ));
    rows.push(packet(
        "p-other",
        "prepare-something-else",
        "elsewhere",
        prepared_steps(&stranger),
    ));
    let rec = record(&dir, "record.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 0, "{out}{err}");
    let mut want = vec![
        keys[0].clone(),
        keys[1].clone(),
        keys[2].clone(),
        older.clone(),
        break_glass_line(&break_glass),
    ];
    want.sort();
    assert_eq!(out.lines().collect::<Vec<_>>(), want, "{err}");
    assert!(
        !out.contains(&stranger),
        "a row of another kind gives no key:\n{out}"
    );
}

#[test]
fn deposit_record_corrections_cannot_introduce_a_public_identity() {
    let dir = scratch_dir("deposit-record-original-authority");
    let key = keygen(&dir, "broker");
    let next = keygen(&dir, "replacement");
    let writer = "rule:broker-prepares-the-runner-deposit-key";
    let stored = serde_json::json!({"written_by":writer, "public_key":key});
    // A job-update right records corrections; it does not mint deposit
    // credentials. The immutable broker record supplies key authority,
    // independently of aliases or serialized actor spellings.
    for actor in [
        "codex@algedonic.dev",
        "agent-intruder",
        "automation:rule:broker-prepares-the-runner-deposit-key",
        writer,
        "emp-david",
    ] {
        let mut rows = todays_packets([&key, &key, &key], &key);
        rows[0]["steps"][1]["id"] = serde_json::json!("reserved-issue");
        rows[0]["steps"][1]["metadata"] = stored.clone();
        rows[0]["metadata"] = serde_json::json!({"corrections":[{
            "step":"reserved-issue", "field":"public_key", "reads":key,
            "should_read":next, "by":actor, "why":"fixture replacement",
            "at":"2026-11-13T00:00:00Z"
        }]});
        let rec = record(&dir, "reserved.json", rows);
        let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        assert_eq!(rc, 2, "signer {actor:?}: {out}{err}");
        assert!(
            out.contains("p0") && out.contains("introduce"),
            "{out}{err}"
        );
        assert!(!out.contains(&next), "replacement escaped: {out}{err}");
        let mut rows: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap();
        rows["data"][0]["metadata"]["corrections"]
            .as_array_mut()
            .unwrap()
            .push(
                serde_json::json!({"step":"reserved-issue", "field":"public_key",
                "withdraws":0, "by":"emp-david", "why":"withdraw the replacement"}),
            );
        write_file(&rec, &rows.to_string());
        let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        assert_eq!(rc, 0, "{out}{err}");
        assert!(!out.contains(&next), "{out}{err}");
        // Correcting a comment retains the same SSH public identity.
        rows["data"][0]["metadata"]["corrections"]
            .as_array_mut()
            .unwrap()
            .push(
                serde_json::json!({"step":"reserved-issue", "field":"public_key",
                "reads":key, "should_read":format!("{key} repaired-comment"),
                "by":actor, "why":"repair commentary without changing the key"}),
            );
        write_file(&rec, &rows.to_string());
        let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        assert_eq!(rc, 0, "{out}{err}");
        assert!(out.contains("repaired-comment"), "{out}{err}");
    }
}

#[test]
fn deposit_record_output_corrections_cannot_introduce_a_key() {
    let dir = scratch_dir("deposit-output-original-authority");
    let key = keygen(&dir, "recorded");
    let next = keygen(&dir, "replacement");
    let mut rows = todays_packets([&key, &key, &key], &key);
    let mut changed = break_glass_packet("bg-replaced", "ok", &deposited_output(&key));
    changed["steps"][1]["id"] = serde_json::json!("changed-run");
    changed["metadata"] = serde_json::json!({"corrections":[{
        "step":"changed-run", "field":"output", "reads":key,
        "should_read":next, "by":"codex@algedonic.dev", "why":"replacement"
    }]});
    rows.push(changed);
    let rec = record(&dir, "introduced.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains("bg-replaced") && out.contains("introduce"),
        "{out}{err}"
    );
    assert!(!out.contains(&next), "{out}{err}");
}

#[test]
fn deposit_record_an_authorised_empty_correction_retracts_the_bad_value() {
    let dir = scratch_dir("deposit-record-empty-correction");
    let key = keygen(&dir, "prepared");
    let bad = "ssh-ed25519 AAAAQQ==";
    let mut rows = todays_packets([&key, &key, &key], &key);
    rows[0]["steps"][1]["id"] = serde_json::json!("bad-issue");
    rows[0]["steps"][1]["metadata"] = serde_json::json!({
        "written_by":"rule:broker", "public_key":bad
    });
    rows[0]["metadata"] = serde_json::json!({"corrections":[{
        "step":"bad-issue", "field":"public_key", "reads":bad,
        "should_read":"", "by":"emp-david", "why":"retract a bad recorded value",
        "at":"2026-11-13T00:00:00Z"
    }]});
    let rec = record(&dir, "retracted.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(!out.contains(bad), "{out}{err}");
    assert!(
        out.contains(&key),
        "other broker records must remain: {out}{err}"
    );
}

#[test]
fn deposit_record_overlapping_corrections_name_withdrawal_before_replacement() {
    let dir = scratch_dir("deposit-record-overlap-recovery");
    let key = keygen(&dir, "prepared");
    let next = keygen(&dir, "replacement");
    let mut rows = todays_packets([&key, &key, &key], &key);
    rows[0]["steps"][1]["id"] = serde_json::json!("overlap-issue");
    let blob = key.split_whitespace().nth(1).unwrap();
    rows[0]["metadata"] = serde_json::json!({"corrections":[
        {"step":"overlap-issue", "field":"public_key", "reads":key,
         "should_read":next, "by":"emp-david"},
        {"step":"overlap-issue", "field":"public_key", "reads":blob,
         "should_read":"other", "by":"emp-david"}
    ]});
    let rec = record(&dir, "overlap.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains("--withdraws") && out.contains("--step issue"),
        "{out}{err}"
    );
}

#[test]
fn deposit_record_repairs_name_the_packet_and_honour_corrections() {
    let dir = scratch_dir("deposit-record-repairs");
    let key = keygen(&dir, "prepared");
    let bad = "ssh-ed25519 AAAAQQ==";
    let mut rows = todays_packets([&key, &key, &key], &key);
    let mut damaged = break_glass_packet("bg-bad", "failed", &deposited_output(bad));
    damaged["steps"][1]["id"] = serde_json::json!("bad-run");
    rows.push(damaged.clone());
    let rec = record(&dir, "bad.json", rows.clone());
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains("bg-bad") && out.contains(bad) && out.contains("boss correct"),
        "{out}{err}"
    );

    damaged["metadata"] = serde_json::json!({"corrections": [{
        "step": "bad-run", "field": "output", "reads": bad,
        "should_read": "", "why": "retract the malformed public key", "by": "fixture", "at": "2026-10-21T00:00:00Z"
    }]});
    *rows.last_mut().unwrap() = damaged.clone();
    let rec = record(&dir, "corrected.json", rows.clone());
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(!out.contains(bad), "{out}");
    damaged["metadata"]["corrections"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"step":"bad-run", "field":"output", "withdraws":0}));
    *rows.last_mut().unwrap() = damaged;
    let rec = record(&dir, "withdrawn.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(out.contains("bg-bad"), "{out}{err}");
}

#[test]
fn deposit_record_uses_a_completed_outputs_correction() {
    let dir = scratch_dir("deposit-record-correction");
    let key = keygen(&dir, "prepared");
    let bad = "ssh-ed25519 AAAAQQ==";
    let mut rows = todays_packets([&key, &key, &key], &key);
    let mut damaged = break_glass_packet("bg-corrected", "failed", &deposited_output(bad));
    damaged["steps"][1]["id"] = serde_json::json!("bad-run");
    damaged["metadata"] = serde_json::json!({"corrections": [{
        "step": "bad-run", "field": "output", "reads": bad,
        "should_read": "", "why": "retract the malformed key", "by": "fixture", "at": "2026-10-21T00:00:00Z"
    }]});
    rows.push(damaged);
    let rec = record(&dir, "corrected.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(
        !out.contains(bad),
        "the correction must remove the malformed record: {out}"
    );
}

#[test]
fn deposit_record_rejects_a_confined_receiver_key_absent_from_the_record() {
    let c = Converge::new("deposit-record-unregistered");
    let prepared = keygen(&c.dir, "prepared");
    let stranger = keygen(&c.dir, "unregistered");
    let rec = c.record(&prepared);
    let ak = c.keys_file(
        "authorized_keys",
        &[format!("{} {stranger}", break_glass_forced())],
    );
    let (verdict, _, text) = c.run(&ak, &rec);
    assert_eq!(verdict, "unconfined", "{text}");
    assert!(
        text.contains("no record") && text.contains(&format!("{}:1", ak.display())),
        "{text}"
    );
}

#[test]
fn deposit_record_warm_reads_use_a_frontier_and_reread_old_corrections() {
    let dir = scratch_dir("deposit-record-frontier");
    let key = keygen(&dir, "prepared");
    let next = keygen(&dir, "next");
    let open_key = keygen(&dir, "open-key");
    let mut rows = todays_packets([&key, &key, &key], &key);
    for i in 0..120 {
        rows.push(break_glass_packet(
            &format!("old-{i}"),
            "ok",
            &deposited_output(&key),
        ));
    }
    for row in &mut rows {
        let id = row["id"].as_str().unwrap().to_string();
        for (i, step) in row["steps"].as_array_mut().unwrap().iter_mut().enumerate() {
            step["id"] = serde_json::json!(format!("{id}-{i}"));
            step["completed_at"] = serde_json::json!(if id.starts_with("old-") {
                "2026-10-20T00:00:00Z"
            } else {
                "2026-10-21T00:00:00Z"
            });
        }
    }
    let rec = record(&dir, "record.json", rows.clone());
    let log = dir.join("queries.jsonl");
    rows[5]["status"] = serde_json::json!("open");
    rows[5]["steps"][1]["status"] = serde_json::json!("active");
    rows[5]["steps"][1]["completed_at"] = serde_json::Value::Null;
    record(&dir, "record.json", rows.clone());
    let ignored = dir.join("ignore-frontier");
    let cache = dir.join("cache.json");
    let bin = dir.join("bin");
    std::fs::create_dir(&bin).unwrap();
    // A transport fixture for the existing list API, including real
    // page offsets and cursor predicates. It opens no socket.
    write_exec(&bin.join("curl"), &r#"#!/usr/bin/env python3
import json, pathlib, sys, urllib.parse
q = urllib.parse.parse_qs(urllib.parse.urlsplit(sys.argv[-1]).query)
rows = json.loads(pathlib.Path(@RECORD@).read_text())["data"]
if not q:
    if pathlib.Path(@IGNORED@ + "-empty-witness").exists(): sys.exit(0)
    wanted = urllib.parse.unquote(urllib.parse.urlsplit(sys.argv[-1]).path.rsplit("/", 1)[-1])
    matches = [r for r in rows if r["id"] == wanted]
    if not matches: sys.exit(22)
    print(json.dumps(matches[0])); sys.exit(0)
rows = [r for r in rows if r["kind"] == q["kind"][0] and r["subject"]["id"] == q["subject_id"][0]]
if "metadata_has" in q:
    rows = [r for r in rows if q["metadata_has"][0] in r.get("metadata", {})]
if "status" in q:
    rows = [r for r in rows if r["status"] == q["status"][0]]
if "step_completed_since" in q and not pathlib.Path(@IGNORED@).exists():
    rows = [r for r in rows if any((s.get("completed_at") or "") >= q["step_completed_since"][0] for s in r["steps"])]
total = len(rows)
offset = int(q.get("offset", [0])[0]); limit = int(q.get("limit", [50])[0])
rows = rows[offset:offset+limit]
with open(@LOG@, "a") as out:
    out.write(json.dumps({"query":q, "ids":[r["id"] for r in rows]}) + "\n")
print(json.dumps({"data":rows, "total":total}))
"#.replace("@RECORD@", &serde_json::to_string(&rec.to_str().unwrap()).unwrap())
        .replace("@IGNORED@", &serde_json::to_string(&ignored.to_str().unwrap()).unwrap())
        .replace("@LOG@", &serde_json::to_string(&log.to_str().unwrap()).unwrap()));
    let path = PathBuf::from(format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap()
    ));
    let fixture_url = url(&rec);
    let env = [
        ("PATH", path.as_path()),
        ("BOSS_GCP_PREPARED_KEYS_URL", fixture_url.as_path()),
        ("BOSS_GCP_PREPARED_KEYS_CACHE", cache.as_path()),
    ];
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 0, "{out}{err}");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert!(
        saved["rows"].as_array().unwrap().len() <= 5,
        "repeated daily packets must not accumulate in the local projection"
    );
    // A warm read must still refuse a replacement from a job updater's
    // alias and leave its last good projection untouched.
    let clean_cache = std::fs::read(&cache).unwrap();
    rows[2]["metadata"] = serde_json::json!({"corrections":[{
        "step":"p2-1", "field":"public_key", "reads":key, "should_read":next,
        "by":"codex@algedonic.dev", "why":"untrusted replacement", "at":"2026-11-13T00:00:00Z"
    }]});
    record(&dir, "record.json", rows.clone());
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains("p2") && out.contains("introduce"),
        "{out}{err}"
    );
    assert!(
        !out.contains(&next),
        "a warm read admitted the replacement: {out}{err}"
    );
    assert_eq!(
        std::fs::read(&cache).unwrap(),
        clean_cache,
        "a refused correction must not advance or replace the cache"
    );
    rows[2]["metadata"] = serde_json::json!({});
    record(&dir, "record.json", rows.clone());
    std::fs::write(&cache, clean_cache).unwrap();
    write_file(&log, "");
    let mut newer = break_glass_packet("new", "ok", &deposited_output(&next));
    for step in newer["steps"].as_array_mut().unwrap() {
        step["completed_at"] = serde_json::json!("2026-10-22T00:00:00Z");
    }
    rows.push(newer);
    rows[4]["metadata"] = serde_json::json!({"corrections":[{
        "step":"old-0-1", "field":"output", "reads":key, "should_read":"",
        "why":"late retraction", "by":"fixture", "at":"2026-10-22T00:00:00Z"
    }]});
    rows[5]["steps"][1]["metadata"]["output"] = serde_json::json!(deposited_output(&open_key));
    record(&dir, "record.json", rows);
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(
        out.contains(&break_glass_line(&key)) && out.contains(&break_glass_line(&next)),
        "{out}"
    );
    assert!(
        out.contains(&break_glass_line(&open_key)),
        "an open packet's mutable output was not refreshed: {out}"
    );
    for line in std::fs::read_to_string(&log).unwrap().lines() {
        let read: serde_json::Value = serde_json::from_str(line).unwrap();
        let q = &read["query"];
        assert!(
            q.get("step_completed_since").is_some()
                || q.get("metadata_has").is_some()
                || q.get("status").is_some(),
            "a warm tick reread unbounded history: {line}"
        );
        if q.get("metadata_has").is_none() && q.get("status").is_none() {
            assert!(
                !read["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id.as_str().unwrap().starts_with("old-")),
                "a warm tick read old uncorrected history: {line}"
            );
        }
    }
    // Correct the retained old-key witness. Other historical copies
    // still vouch for that key, so a rebuild must conserve it.
    let mut source: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap();
    source["data"][3]["metadata"] = serde_json::json!({"corrections":[{
        "step":"bg-today-1", "field":"output", "reads":key, "should_read":"",
        "why":"retract the retained witness", "by":"fixture", "at":"2026-10-22T00:00:00Z"
    }]});
    std::fs::write(&rec, source.to_string()).unwrap();
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 0, "{out}{err}");
    assert!(
        out.contains(&break_glass_line(&key)),
        "a witness correction forgot other historical copies: {out}"
    );
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&cache).unwrap()).unwrap();
    assert!(saved["rows"].as_array().unwrap().len() <= 8);
    let before = std::fs::read(&cache).expect("a successful audit persists its projection");
    let empty_witness = PathBuf::from(format!("{}-empty-witness", ignored.display()));
    write_file(&empty_witness, "empty successful point response\n");
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 2, "a silent point response must refuse: {out}{err}");
    assert!(
        out.contains("cached packet") && out.contains("no document"),
        "{out}{err}"
    );
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    std::fs::remove_file(&empty_witness).unwrap();
    write_file(&ignored, "ignore the query parameter\n");
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(
        rc, 2,
        "an API ignoring the cursor cannot support a cache: {out}{err}"
    );
    assert_eq!(
        std::fs::read(&cache).unwrap(),
        before,
        "a refused read must not advance the cache"
    );
    std::fs::remove_file(&ignored).unwrap();
    let source_before = std::fs::read(&rec).unwrap();
    record(&dir, "record.json", vec![]);
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(
        rc, 2,
        "an empty wrong deployment must not pass from cached keys: {out}{err}"
    );
    assert!(out.contains("cached packet"), "{out}{err}");
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    std::fs::write(&rec, source_before).unwrap();
    std::fs::write(&cache, "{truncated").unwrap();
    let (rc, out, err) = helper(&dir, &[], &env);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains(&cache.display().to_string()) && out.contains("unreadable"),
        "{out}{err}"
    );
    assert_eq!(std::fs::read_to_string(&cache).unwrap(), "{truncated");
}

/// NO EVIDENCE IS NOT A PASS. Each of these leaves a deposit key that
/// cannot be looked for, and answers ONE UNVERIFIED line, exit 2, never
/// an empty or partial list.
#[test]
fn a_record_that_cannot_name_the_keys_is_unverified() {
    let dir = scratch_dir("prepared-deposit-keys-unverified");
    let key = keygen(&dir, "machine-token-deposit-key");
    let bg = keygen(&dir, "break-glass-deposit-key");
    let all = todays_packets([&key, &key, &key], &bg);
    let mut keyless = todays_packets([&key, &key, &key], &bg);
    keyless[2]["steps"][2]["metadata"] = serde_json::json!({});
    let mut broken = todays_packets([&key, &key, &key], &bg);
    broken[0]["steps"][1]["metadata"]["public_key"] = serde_json::json!(format!("{key}\n{key}"));
    let none: Vec<serde_json::Value> = PAIRS
        .iter()
        .enumerate()
        .map(|(i, (k, s))| {
            packet(
                &format!("p{i}"),
                k,
                s,
                serde_json::json!([{"spec_slug": "issue", "status": "ready", "metadata": {}}]),
            )
        })
        .collect();
    let cases: Vec<(&str, PathBuf)> = vec![
        ("absent", dir.join("no-such-record.json")),
        (
            "partial",
            record_with_total(&dir, "partial.json", all.clone(), 7),
        ),
        ("not-json", {
            let p = dir.join("not-json.json");
            write_file(&p, "<html>502</html>\n");
            p
        }),
        (
            "completed-without-a-key",
            record(&dir, "keyless.json", keyless),
        ),
        ("line-break", record(&dir, "broken.json", broken)),
        ("none-at-all", record(&dir, "none.json", none)),
        ("empty", record(&dir, "empty.json", vec![])),
    ];
    for (name, rec) in cases {
        let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        assert_eq!(rc, 2, "{name}: {out}{err}");
        assert_eq!(out.lines().count(), 1, "{name}: one line:\n{out}");
        assert!(
            out.starts_with("deposit-key-audit: UNVERIFIED — "),
            "{name}: {out}{err}"
        );
        assert!(
            !out.contains("AAAA"),
            "{name}: no key is printed on a refusal:\n{out}"
        );
    }
    // No address and no record named: refused, not read from a default.
    let (rc, out, err) = helper(&dir, &[], &[]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.contains("UNVERIFIED") && out.contains("BOSS_JOBS_URL"),
        "{out}{err}"
    );
}

/// A rule that prepares a boss-gcp deposit key in a shape this cannot
/// read, or rules that prepare none, leave the keys unnamed: UNVERIFIED.
#[test]
fn rules_that_cannot_name_the_deposit_keys_are_unverified() {
    let dir = scratch_dir("prepared-deposit-keys-rules");
    let rule = |when: &str, host: &str| {
        format!(
            "[[rule]]\nname = \"r\"\nwhy = \"w\"\non_event = \"step.done.credential-rotation\"\nwhen = '{when}'\n[[rule.do]]\nhandler = \"credential.prepare.ssh-deposit\"\nargs = {{ protocol_kind = \"\\\"prepare-x\\\"\", receiver_host = \"\\\"{host}\\\"\" }}\n"
        )
    };
    for (name, body) in [
        ("unread-when", rule("subject_id != \"x\"", "boss-gcp")),
        ("other-host", rule("subject_id = \"x\"", "the-forge")),
        ("not-toml", "[[rule]\n".to_string()),
    ] {
        let rules = dir.join(name);
        std::fs::create_dir_all(&rules).unwrap();
        write_file(&rules.join("r.toml"), &body);
        let (rc, out, err) = helper(
            &dir,
            &["--kinds"],
            &[("BOSS_GCP_PREPARED_KEY_RULES", &rules)],
        );
        assert_eq!(rc, 2, "{name}: {out}{err}");
        assert!(
            out.starts_with("deposit-key-audit: UNVERIFIED — "),
            "{name}: {out}"
        );
    }
}

/// The converge's deposit-key block, run as written: from the bound to
/// the log's removal, with the summary writes printed instead of filed.
struct Converge {
    dir: PathBuf,
    runner: PathBuf,
}

impl Converge {
    fn new(name: &str) -> Self {
        let dir = scratch_dir(name);
        let source = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
        let start = source
            .find("audit_bound=\"${BOSS_GCP_DEPOSIT_AUDIT_BOUND_S")
            .expect("the converge bounds its deposit-key audit");
        let end = source[start..]
            .find("rm -f \"$alog\"")
            .expect("the converge removes the audit's log")
            + start;
        let runner = dir.join("deposit-block.sh");
        write_exec(
            &runner,
            &format!(
                "#!/bin/bash\nset -uo pipefail\nrun_summary_field() {{ printf 'field %s=%s\\n' \"$1\" \"$2\"; }}\nrun_summary_note() {{ printf 'note %s\\n' \"$1\"; }}\nOWNER=$(id -un)\n{}\n",
                &source[start..end]
            ),
        );
        Converge { dir, runner }
    }

    /// The block over `ak`, with the record at `rec`: (verdict word,
    /// deposit_key_audit, whole output).
    fn run(&self, ak: &Path, rec: &Path) -> (String, String, String) {
        let out = Command::new("bash")
            .arg(&self.runner)
            .env("BOSS_GCP_CONVERGE_AUTHORIZED_KEYS", ak)
            .env("BOSS_GCP_CONVERGE_INFRA", repo_root().join("infra"))
            .env("BOSS_GCP_PREPARED_KEYS_URL", url(rec))
            .env_remove("BOSS_GCP_DEPOSIT_USER")
            .env_remove("BOSS_GCP_PREPARED_KEY_RULES")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", empty_sor_env(&self.dir))
            .env("BOSS_MACHINE_TOKEN_DIR", self.dir.join("no-machine-token"))
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("field {name}=")))
                .unwrap_or("")
                .to_string()
        };
        (
            field("deposit_key_verdict"),
            field("deposit_key_audit"),
            text,
        )
    }

    /// The record of today's packets, the machine token's key `mt`.
    fn record(&self, mt: &str) -> PathBuf {
        let bg = keygen(&self.dir, "break-glass-deposit-key");
        self.record_with(mt, &bg)
    }

    /// Today's packets, the machine token's key `mt` and the break-glass
    /// deposit key `bg`.
    fn record_with(&self, mt: &str, bg: &str) -> PathBuf {
        let runner = keygen(&self.dir, "runner-deposit-key");
        let root = keygen(&self.dir, "root-tree-deposit-key");
        record(
            &self.dir,
            "record.json",
            todays_packets([&runner, mt, &root], bg),
        )
    }

    fn keys_file(&self, name: &str, lines: &[String]) -> PathBuf {
        let path = self.dir.join(name);
        write_file(&path, &format!("{}\n", lines.join("\n")));
        path
    }
}

/// THE ITEM'S SHAPES, ON THE CONVERGE (backlog 77920d6a). Each line
/// forces the machine token's root door spelled through shell syntax —
/// quoted, escaped, through a variable, through a glob — alone in the
/// file and WITHOUT restrict, so sshd grants the key a pty, forwarding
/// and ~/.ssh/rc while the account's shell runs the door. Before this
/// car the converge passed no prepared key and every one read clean on
/// the packet; now the key the prepare packet recorded is judged by its
/// fingerprint, and the packet reads unconfined.
#[test]
fn the_converge_judges_a_shell_spelled_door_by_its_prepared_key() {
    let mut wrong = Vec::new();
    for (name, cmd) in [
        (
            "quoted",
            "exec sudo -n /usr/local/lib'exec'/boss/ops-credential-recv machine-token",
        ),
        (
            "escaped",
            "exec sudo -n /usr/local/libexec/bo\\ss/ops-credential-recv machine-token",
        ),
        (
            "variable",
            "L=libexec; exec sudo -n /usr/local/$L/boss/ops-credential-recv machine-token",
        ),
        (
            "glob",
            "exec sudo -n /usr/local/libexe?/boss/ops-credential-recv machine-token",
        ),
    ] {
        let c = Converge::new(&format!("prepared-deposit-keys-converge-{name}"));
        let mt = keygen(&c.dir, "machine-token-deposit-key");
        let rec = c.record(&mt);
        let ak = c.keys_file(
            "authorized_keys",
            &[format!("command=\"{cmd}\" {mt} machine-token-deposit-key")],
        );
        let (verdict, audit, text) = c.run(&ak, &rec);
        // Every spelling is judged before any is asserted, so a red names
        // each one that read wrong rather than the first.
        if verdict != "unconfined"
            || !audit.starts_with("UNCONFINED:")
            || !text.contains(&format!("UNCONFINED {}:1", ak.display()))
        {
            wrong.push(format!("{name}: deposit_key_verdict={verdict}\n{text}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "a prepared deposit key forced through a shell spelling of its door, with no restrict, is a finding on the packet naming the line:\n{}",
        wrong.join("\n")
    );
}

/// THE EXCLUDED CASE: the prepared key on a bare line with no confined
/// twin, beside the owner's own login. There is no receiver line to tie
/// it to, so a text-only audit read the account's shell for whoever
/// holds a deposit key as the owner's business. It is only a finding if
/// the keys reach the audit BEFORE the files (backlog 26173f40): placed
/// after them they are read as absent files, and the set reads clean.
#[test]
fn the_converge_finds_a_prepared_key_on_a_bare_line_alone() {
    let c = Converge::new("prepared-deposit-keys-converge-bare");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let login = keygen(&c.dir, "owner-laptop");
    let rec = c.record(&mt);
    let ak = c.keys_file(
        "authorized_keys",
        &[
            format!("{login} owner@laptop"),
            format!("{mt} machine-token-deposit-key"),
        ],
    );
    let (verdict, _, text) = c.run(&ak, &rec);
    assert_eq!(verdict, "unconfined", "{text}");
    assert!(
        text.contains(&format!("UNCONFINED {}:2", ak.display())) && text.contains("shell"),
        "the finding names the bare line and what it opens:\n{text}"
    );
    assert!(
        !text.contains(&format!("UNCONFINED {}:1", ak.display())),
        "the owner's own login key is not a finding:\n{text}"
    );
    let source = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
    for line in source.lines().filter(|l| l.contains("bash -s --")) {
        assert!(
            line.contains("bash -s -- \"${prepared_args[@]}\" \"${ak_files[@]}\""),
            "every audit call hands the prepared keys before the files: {line}"
        );
    }
}

/// The lines the installers print, over their prepared keys, stay clean,
/// and the packet says how many prepared keys the verdict was judged by.
#[test]
fn the_converge_reads_a_confined_prepared_key_clean() {
    let c = Converge::new("prepared-deposit-keys-converge-clean");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let rec = c.record(&mt);
    let ak = c.keys_file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {mt} machine-token-deposit-key"
        )],
    );
    let (verdict, audit, text) = c.run(&ak, &rec);
    assert_eq!(verdict, "clean", "{text}");
    assert!(
        audit.starts_with("deposit-key-audit: clean — 1 line(s)")
            && audit.contains("4 prepared deposit key(s) judged by fingerprint"),
        "the clean verdict counts the prepared keys it judged by:\n{text}"
    );
}

#[test]
fn the_converge_records_missing_key_files_clean() {
    let c = Converge::new("prepared-deposit-keys-converge-absent");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let rec = c.record(&mt);
    let missing = c.dir.join("uncreated/.ssh/authorized_keys");
    let (verdict, audit, text) = c.run(&missing, &rec);
    assert_eq!(
        verdict, "clean",
        "an empty account is no unverified streak:\n{text}"
    );
    assert!(
        audit.starts_with("deposit-key-audit: clean — none of the files named is present"),
        "the packet records why no receiver line was read:\n{text}"
    );
}

/// UNKNOWN REFUSES. A record that cannot name the prepared keys leaves a
/// bare or shell-spelled deposit key unjudged, so a file the text-only
/// audit reads clean is UNVERIFIED on the packet — never clean, and
/// never a finding either (the chore judge files nothing from it). A
/// finding the text-only audit does reach still stands.
#[test]
fn the_converge_never_reads_clean_without_the_prepared_keys() {
    let c = Converge::new("prepared-deposit-keys-converge-unknown");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let none = record(&c.dir, "none.json", vec![]);
    let confined = c.keys_file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {mt} machine-token-deposit-key"
        )],
    );
    for (name, rec) in [
        ("unreadable", c.dir.join("no-such-record.json")),
        ("no-key-recorded", none.clone()),
    ] {
        let (verdict, audit, text) = c.run(&confined, &rec);
        assert_eq!(verdict, "unverified", "{name}: {text}");
        assert!(
            audit.starts_with("UNVERIFIED:") && audit.contains("prepared"),
            "{name}: the packet says it was the prepared keys that could not be read:\n{text}"
        );
        assert!(
            !text.contains("RED "),
            "{name}: an unverified audit files nothing:\n{text}"
        );
    }
    let unrestricted = c.keys_file(
        "authorized_keys.unrestricted",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\" {mt} machine-token-deposit-key"
        )],
    );
    let (verdict, _, text) = c.run(&unrestricted, &none);
    assert_eq!(
        verdict, "unconfined",
        "a finding the text-only audit reaches still stands:\n{text}"
    );
}

#[test]
fn an_unparseable_prepared_key_preserves_text_only_findings() {
    for source in ["broker", "break-glass"] {
        let c = Converge::new(&format!("prepared-deposit-keys-unparseable-{source}"));
        let mt = keygen(&c.dir, "machine-token-deposit-key");
        let bg = keygen(&c.dir, "break-glass-deposit-key");
        // A single-line value accepted by the old reader, but no SSH key.
        // The break-glass shape also passes its text extraction regex.
        let bad = "ssh-ed25519 AAAA";
        let rec = c.record_with(
            if source == "broker" { bad } else { &mt },
            if source == "break-glass" { bad } else { &bg },
        );
        let (rc, out, err) = helper(&c.dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        let door =
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\"";
        let unrestricted = c.keys_file("authorized_keys.unrestricted", &[format!("{door} {mt}")]);
        let (verdict, audit, text) = c.run(&unrestricted, &rec);
        assert_eq!(verdict, "unconfined", "{source}: {text}");
        assert!(audit.starts_with("UNCONFINED:"), "{audit}: {text}");
        assert!(
            text.contains(&format!("UNCONFINED {}:1", unrestricted.display())),
            "{text}"
        );
        assert_eq!(
            rc, 2,
            "{source}: reader must refuse the bad key: {out}{err}"
        );
        assert!(
            out.starts_with("deposit-key-audit: UNVERIFIED"),
            "{out}{err}"
        );
        assert_eq!(out.lines().count(), 1, "no partial key list: {out}{err}");

        let confined = c.keys_file(
            "authorized_keys.confined",
            &[format!("{door},restrict {mt}")],
        );
        let (verdict, audit, text) = c.run(&confined, &rec);
        assert_eq!(
            verdict, "unverified",
            "{source}: text-only clean cannot certify the keys: {text}"
        );
        assert!(audit.contains("prepared"), "{audit}: {text}");
        assert!(
            !text.contains("RED boss-gcp-converge/deposit-key-unconfined"),
            "{text}"
        );
    }
}

#[test]
fn the_prepared_key_reader_uses_its_selected_key_parser() {
    let c = Converge::new("prepared-deposit-keys-selected-parser");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let rec = c.record(&mt);
    let parser = c.dir.join("rejecting-key-parser");
    write_exec(&parser, "#!/bin/sh\nexit 1\n");
    let (rc, out, err) = helper(
        &c.dir,
        &[],
        &[
            ("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec)),
            ("BOSS_DEPOSIT_AUDIT_SSH_KEYGEN", &parser),
        ],
    );
    assert_eq!(rc, 2, "the selected parser refused every key: {out}{err}");
    assert!(
        out.starts_with("deposit-key-audit: UNVERIFIED"),
        "{out}{err}"
    );
    assert_eq!(out.lines().count(), 1, "no partial key list: {out}{err}");
}

#[test]
fn the_prepared_key_reader_refuses_a_missing_key_parser() {
    let c = Converge::new("prepared-deposit-keys-missing-parser");
    let mt = keygen(&c.dir, "machine-token-deposit-key");
    let rec = c.record(&mt);
    let missing = c.dir.join("no-such-key-parser");
    let (rc, out, err) = helper(
        &c.dir,
        &[],
        &[
            ("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec)),
            ("BOSS_DEPOSIT_AUDIT_SSH_KEYGEN", &missing),
        ],
    );
    assert_eq!(rc, 2, "{out}{err}");
    assert!(
        out.starts_with("deposit-key-audit: UNVERIFIED") && out.contains("key parser"),
        "{out}{err}"
    );
    assert_eq!(
        out.lines().count(),
        1,
        "no keys before the parser answers: {out}{err}"
    );
}

/// THE BREAK-GLASS KEY'S RECORD (backlog 403d9d7e). Every key a
/// break-glass deposit packet's run recorded on the Job's line — the
/// authorize line of the run that minted it, the line of every run that
/// held it — is named once, WHOLE (`command="…kubeconfig",restrict
/// <key>`), so the audit binds it to the kubeconfig receiver. A re-minted
/// key leaves the old one named, as a superseded preparation does. A
/// sentence naming the line's shape with no key in it (the Job's own
/// remedy text), and a row of another kind or subject, name nothing.
#[test]
fn the_break_glass_key_is_named_whole_from_its_jobs_own_packets() {
    let dir = scratch_dir("prepared-deposit-keys-break-glass");
    let keys = [
        keygen(&dir, "runner-deposit-key"),
        keygen(&dir, "machine-token-deposit-key"),
        keygen(&dir, "root-tree-deposit-key"),
    ];
    let first = keygen(&dir, "break-glass-deposit-key");
    let reminted = keygen(&dir, "break-glass-deposit-key-2");
    let stranger = keygen(&dir, "someone-elses-key");
    let forced = break_glass_forced();
    let mut rows: Vec<serde_json::Value> = PAIRS
        .iter()
        .zip(&keys)
        .enumerate()
        .map(|(i, ((kind, subject), key))| {
            packet(&format!("p{i}"), kind, subject, prepared_steps(key))
        })
        .collect();
    rows.extend([
        break_glass_packet("bg-mint", "failed", &minted_output(&first)),
        break_glass_packet("bg-held", "ok", &deposited_output(&first)),
        break_glass_packet("bg-remint", "failed", &minted_output(&reminted)),
        break_glass_packet(
            "bg-remedy",
            "failed",
            &format!(
                "RED break-glass-deposit/key-unconfined refused: this key reaches a shell. REMOVE THAT LINE NOW, then place it whole: {forced} <the deposit key's public half>. No kubeconfig byte was sent.\n"
            ),
        ),
        packet(
            "other-kind",
            "maintenance-something-else",
            "infra/maintenance-something-else",
            serde_json::json!([{"spec_slug": "run", "status": "completed",
                "metadata": {"result": "ok", "output": deposited_output(&stranger)}}]),
        ),
        packet(
            "other-subject",
            BREAK_GLASS_KIND,
            "elsewhere",
            serde_json::json!([{"spec_slug": "run", "status": "completed",
                "metadata": {"result": "ok", "output": deposited_output(&stranger)}}]),
        ),
    ]);
    let rec = record(&dir, "record.json", rows);
    let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
    assert_eq!(rc, 0, "{out}{err}");
    let mut want = vec![
        keys[0].clone(),
        keys[1].clone(),
        keys[2].clone(),
        break_glass_line(&first),
        break_glass_line(&reminted),
    ];
    want.sort();
    assert_eq!(out.lines().collect::<Vec<_>>(), want, "{err}");
    assert!(
        !out.contains(&stranger),
        "a row of another kind or subject names no key:\n{out}"
    );
}

/// NO EVIDENCE IS NOT A PASS, FOR THE BREAK-GLASS KEY TOO. The deposit
/// runs daily and every run that holds a key names it, so a record with
/// no such packet, or packets naming no key, is a dark or wrong read — a
/// break-glass key boss-gcp may hold that nothing can look for. One
/// UNVERIFIED line, exit 2, naming the break-glass deposit; never the
/// prepare packets' keys alone.
#[test]
fn a_break_glass_record_that_names_no_key_is_unverified() {
    let dir = scratch_dir("prepared-deposit-keys-break-glass-unverified");
    let key = keygen(&dir, "machine-token-deposit-key");
    let prepared: Vec<serde_json::Value> = PAIRS
        .iter()
        .enumerate()
        .map(|(i, (kind, subject))| packet(&format!("p{i}"), kind, subject, prepared_steps(&key)))
        .collect();
    let with = |extra: Vec<serde_json::Value>| {
        let mut rows = prepared.clone();
        rows.extend(extra);
        rows
    };
    let forced = break_glass_forced();
    let cases: Vec<(&str, Vec<serde_json::Value>)> = vec![
        ("no-break-glass-packet", with(vec![])),
        (
            "no-key-on-the-line",
            with(vec![break_glass_packet(
                "bg",
                "failed",
                &format!("then place it whole: {forced} <the deposit key's public half>\n"),
            )]),
        ),
        (
            "no-output",
            with(vec![packet(
                "bg",
                BREAK_GLASS_KIND,
                BREAK_GLASS_SUBJECT,
                serde_json::json!([{"spec_slug": "run", "status": "completed", "metadata": {"result": "ok"}}]),
            )]),
        ),
    ];
    for (name, rows) in cases {
        let rec = record(&dir, &format!("{name}.json"), rows);
        let (rc, out, err) = helper(&dir, &[], &[("BOSS_GCP_PREPARED_KEYS_URL", &url(&rec))]);
        assert_eq!(rc, 2, "{name}: {out}{err}");
        assert_eq!(out.lines().count(), 1, "{name}: one line:\n{out}");
        assert!(
            out.starts_with("deposit-key-audit: UNVERIFIED — ") && out.contains("break-glass"),
            "{name}: the refusal names the break-glass deposit's record: {out}{err}"
        );
        assert!(
            !out.contains("AAAA"),
            "{name}: no key is printed on a refusal:\n{out}"
        );
    }
}

/// ONE LINE, SPELLED ONCE PER READER AND HELD EQUAL (CLAUDE.md 9a). The
/// reader looks for the options the Job prints in front of its key, on
/// the packets of the kind the Job's chore files; break_glass_deposit_sh.rs
/// already holds the Job's options equal to the receiver installer's.
#[test]
fn the_reader_looks_for_the_line_and_the_packets_the_job_writes() {
    assert_eq!(
        forced_in(HELPER, "BREAK_GLASS_FORCED"),
        break_glass_forced(),
        "the reader and the break-glass Job spell the deposit key's options differently"
    );
    let reader = std::fs::read_to_string(repo_root().join(HELPER)).unwrap();
    assert!(
        reader
            .lines()
            .any(|l| l == format!("BREAK_GLASS_KIND={BREAK_GLASS_KIND}")),
        "the reader reads the {BREAK_GLASS_KIND} packets"
    );
    let manifest = std::fs::read_to_string(repo_root().join(DEPOSIT_MANIFEST)).unwrap();
    assert!(
        manifest.contains(&format!("boss-chore.sh {BREAK_GLASS_KIND} ")),
        "the break-glass deposit's chore files {BREAK_GLASS_KIND} packets"
    );
}

/// THE ITEM, ON THE CONVERGE (backlog 403d9d7e). The break-glass deposit
/// key on a bare line beside the owner's login, forced through a shell
/// spelling of its door with no restrict, or confined and restricted but
/// forced to the OTHER purpose of the same receiver: a text-only audit
/// read the first as the owner's own key and the last two clean. Judged
/// by the line its Job recorded, each is a finding naming the line, and
/// the line the installer prints stays clean.
#[test]
fn the_converge_judges_the_break_glass_key_by_its_recorded_line() {
    let forced = break_glass_forced();
    let mut wrong = Vec::new();
    for (name, line, want) in [
        ("bare", "{bg} break-glass-deposit@boss".to_string(), "unconfined"),
        (
            "shell-spelled",
            "command=\"exec sudo -n /usr/local/lib'exec'/boss/ops-credential-recv kubeconfig\" {bg} break-glass-deposit@boss".to_string(),
            "unconfined",
        ),
        (
            "other-purpose",
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {bg} break-glass-deposit@boss".to_string(),
            "unconfined",
        ),
        ("installed", format!("{forced} {{bg}} break-glass-deposit@boss"), "clean"),
    ] {
        let c = Converge::new(&format!("prepared-deposit-keys-converge-bg-{name}"));
        let mt = keygen(&c.dir, "machine-token-deposit-key");
        let bg = keygen(&c.dir, "break-glass-deposit-key");
        let login = keygen(&c.dir, "owner-laptop");
        let rec = c.record_with(&mt, &bg);
        let ak = c.keys_file(
            "authorized_keys",
            &[
                format!("{login} owner@laptop"),
                format!(
                    "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {mt} machine-token-deposit-key"
                ),
                line.replace("{bg}", &bg),
            ],
        );
        let (verdict, audit, text) = c.run(&ak, &rec);
        let finding = text.contains(&format!("UNCONFINED {}:3", ak.display()));
        let ok = match want {
            "clean" => {
                verdict == "clean"
                    && audit.contains("4 prepared deposit key(s) judged by fingerprint")
            }
            _ => {
                verdict == "unconfined"
                    && finding
                    && !text.contains(&format!("UNCONFINED {}:1", ak.display()))
                    && !text.contains(&format!("UNCONFINED {}:2", ak.display()))
            }
        };
        if !ok {
            wrong.push(format!(
                "{name}: wanted {want}, deposit_key_verdict={verdict}\n{text}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "the break-glass deposit key is judged by the line its Job recorded:\n{}",
        wrong.join("\n")
    );
}
