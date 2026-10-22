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

/// The three packets of today, each recording one of `keys`.
fn three_packets(keys: [&str; 3]) -> Vec<serde_json::Value> {
    PAIRS
        .iter()
        .zip(keys)
        .enumerate()
        .map(|(i, ((kind, subject), key))| {
            packet(&format!("p{i}"), kind, subject, prepared_steps(key))
        })
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
    let mut rows = three_packets([&keys[0], &keys[1], &keys[2]]);
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
    ];
    want.sort();
    assert_eq!(out.lines().collect::<Vec<_>>(), want, "{err}");
    assert!(
        !out.contains(&stranger),
        "a row of another kind gives no key:\n{out}"
    );
}

/// NO EVIDENCE IS NOT A PASS. Each of these leaves a deposit key that
/// cannot be looked for, and answers ONE UNVERIFIED line, exit 2, never
/// an empty or partial list.
#[test]
fn a_record_that_cannot_name_the_keys_is_unverified() {
    let dir = scratch_dir("prepared-deposit-keys-unverified");
    let key = keygen(&dir, "machine-token-deposit-key");
    let all = three_packets([&key, &key, &key]);
    let mut keyless = three_packets([&key, &key, &key]);
    keyless[2]["steps"][2]["metadata"] = serde_json::json!({});
    let mut broken = three_packets([&key, &key, &key]);
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

    /// The record of today's three packets, the machine token's key `mt`.
    fn record(&self, mt: &str) -> PathBuf {
        let runner = keygen(&self.dir, "runner-deposit-key");
        let root = keygen(&self.dir, "root-tree-deposit-key");
        record(
            &self.dir,
            "record.json",
            three_packets([&runner, mt, &root]),
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
            && audit.contains("3 prepared deposit key(s) judged by fingerprint"),
        "the clean verdict counts the prepared keys it judged by:\n{text}"
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
