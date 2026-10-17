//! `infra/gcp/audit-deposit-keys.sh` is RUN, not read — against
//! authorized_keys files written into a scratch directory — so every
//! verdict below is one the script actually reached.
//!
//! WHY IT EXISTS (backlog dea2236f, N5 of car c1cdb202's review). On the
//! first live enrolment of the machine token deposit key (packet
//! 96a0f7bb, 2026-10-07) the bare public key went into
//! ~david/.ssh/authorized_keys on boss-gcp where the line forcing the
//! receiver belonged, and for ten to fifteen minutes a key minted for one
//! receiver command opened that account's shell. The senders now read the
//! receiver's greeting before any byte (cars c1cdb202, aff9c8c2), but a
//! greeting proves the key reached the receiver ONCE: it cannot see a line
//! that forces the receiver and lacks `restrict` (a pty, forwarding and
//! the user's rc stay open to the key), nor a second, bare line carrying
//! the same key, which sshd honours whenever it is reached first. Only a
//! read of the file on boss-gcp shows either, and boss-gcp's converge runs
//! there every half hour, reading it as the account sshd reads it as.
//!
//! Review 1fa0d093 (B1) stood a real `sshd -i` behind the audit and found
//! bare twins sshd honours that the audit read as clean — stray `\r`,
//! `\v`, `\f` that sshd's base64 decoder skips, and a NUL where sshd's
//! line ends. Review ee6589a9 (B1', B1'') found the class behind them:
//! the audit compared keys as base64 TEXT, and one key has several
//! spellings sshd accepts — an RSA mpint with a leading zero byte, an
//! ed25519 blob whose inner type name is `ED25519`, an outer type of
//! `rsa-sha2-256`. The audit now takes every line's key from ssh-keygen,
//! which is sshd's own key parser, and compares fingerprints; the cases
//! below carry the reviewer's shapes (61, 62, 64, 65, 66, 83), each
//! checked first to be the SAME key to ssh-keygen.
//!
//! The keys are real, generated per case: a stand-in blob is no longer
//! something the audit can read a key from, which is the point.
//!
//! What each case pins: the three lines the tree tells David to place
//! are clean, read from the files that print them; a receiver line
//! without `restrict`, or with a later option that re-opens what
//! `restrict` closed, is a finding naming the line; the 96a0f7bb shape —
//! a confined key also present bare — is a finding naming the BARE line,
//! in the same file or in authorized_keys2, in whatever spelling sshd
//! reads; `restrict` inside the quoted command is not `restrict`; an
//! absent file is clean and says so; a file that cannot be read, or an
//! audit without its key parser, is not a pass; and the converge runs the
//! audit and carries its verdict to the packet.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SCRIPT: &str = "infra/gcp/audit-deposit-keys.sh";

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `<pattern>'<value>'` on its own line of `file`, the value returned.
fn quoted_after(file: &str, pattern: &str) -> String {
    let text = read(&repo_root().join(file));
    let line = text
        .lines()
        .find(|l| l.starts_with(pattern))
        .unwrap_or_else(|| panic!("{file} has no line starting {pattern}"));
    line[pattern.len()..].trim_matches('\'').to_string()
}

/// A public half: its outer type name and its base64 blob.
#[derive(Clone)]
struct Key {
    kind: String,
    blob: String,
}

/// A fresh key pair of `kind` under `dir`, its public half returned.
fn keygen(dir: &Path, name: &str, kind: &str) -> Key {
    let path = dir.join(name);
    let mut args = vec!["-q", "-t", kind, "-N", "", "-C", name];
    if kind == "rsa" {
        args.extend(["-b", "2048"]);
    }
    let out = Command::new("ssh-keygen")
        .args(&args)
        .arg("-f")
        .arg(&path)
        .output()
        .expect("ssh-keygen runs (the gate image carries it)");
    assert!(
        out.status.success(),
        "ssh-keygen -t {kind}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let public = read(&path.with_extension("pub"));
    let mut fields = public.split_whitespace();
    Key {
        kind: fields.next().unwrap().to_string(),
        blob: fields.next().unwrap().to_string(),
    }
}

/// What ssh-keygen — sshd's key parser — reads from one line: the
/// fingerprint, or None for a line it reads no key from.
fn fingerprint(line: &str) -> Option<String> {
    let mut child = Command::new("ssh-keygen")
        .args(["-l", "-E", "sha256", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(&mut child, format!("{line}\n").as_bytes());
    let out = child.wait_with_output().unwrap();
    out.status
        .success()
        .then(|| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .nth(1)
                .map(str::to_string)
        })
        .flatten()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn b64_decode(text: &str) -> Vec<u8> {
    let vals: Vec<u32> = text
        .bytes()
        .filter(|b| *b != b'=')
        .map(|b| B64.iter().position(|c| *c == b).unwrap() as u32)
        .collect();
    vals.chunks(4)
        .flat_map(|c| {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, v)| n | (v << (18 - 6 * i)));
            (0..c.len() - 1).map(move |i| (n >> (16 - 8 * i)) as u8)
        })
        .collect()
}

/// The SSH wire strings a blob is made of.
fn strings(blob: &str) -> Vec<Vec<u8>> {
    let raw = b64_decode(blob);
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= raw.len() {
        let n = u32::from_be_bytes(raw[i..i + 4].try_into().unwrap()) as usize;
        out.push(raw[i + 4..i + 4 + n].to_vec());
        i += 4 + n;
    }
    out
}

fn blob_of(parts: &[Vec<u8>]) -> String {
    let mut raw = Vec::new();
    for p in parts {
        raw.extend((p.len() as u32).to_be_bytes());
        raw.extend(p);
    }
    b64_encode(&raw)
}

struct Keys {
    a: Key,
    b: Key,
    c: Key,
    david: Key,
}

struct Case {
    root: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        Case {
            root: scratch_dir(name),
        }
    }

    fn keys(&self) -> Keys {
        Keys {
            a: keygen(&self.root, "runner-deposit-key", "ed25519"),
            b: keygen(&self.root, "machine-token-deposit-key", "ed25519"),
            c: keygen(&self.root, "break-glass-deposit", "ed25519"),
            david: keygen(&self.root, "david-laptop", "ed25519"),
        }
    }

    fn file(&self, name: &str, lines: &[String]) -> PathBuf {
        let path = self.root.join(name);
        let mut body = lines.join("\n");
        body.push('\n');
        write_file(&path, &body);
        path
    }

    fn run(&self, files: &[&Path]) -> (i32, String) {
        self.run_with(files, &[])
    }

    fn run_with(&self, files: &[&Path], env: &[(&str, &str)]) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(files)
            .envs(env.iter().copied())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

fn bare(key: &Key, comment: &str) -> String {
    format!("{} {} {comment}", key.kind, key.blob)
}

/// The forced-command lines the tree tells David to place, each with a
/// key of its own, read from the files that print them — so a change to
/// any of their shapes reaches this audit's tests on the same car.
fn tree_lines(keys: &Keys) -> Vec<String> {
    let template = read(&repo_root().join("infra/gcp/runner-credential-enrollment.template"));
    let runner = template
        .lines()
        .find(|l| l.starts_with("command="))
        .expect("runner-credential-enrollment.template carries its command= line")
        .replace(
            "@ENROLLED_PUBLIC_KEY@",
            &bare(&keys.a, "runner-deposit-key"),
        );
    let machine = quoted_after("infra/cluster/machine-token-push.sh", "FORCED_COMMAND=");
    let installer = read(&repo_root().join("infra/gcp/install-ops-credential-receiver.sh"));
    assert!(
        installer.contains("command=\\\"exec sudo -n $RECEIVER kubeconfig\\\",restrict"),
        "install-ops-credential-receiver.sh no longer prints the kubeconfig line this test reproduces"
    );
    vec![
        runner,
        format!(
            "command=\"{machine}\",restrict {}",
            bare(&keys.b, "machine-token-deposit-key")
        ),
        format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig\",restrict {}",
            bare(&keys.c, "break-glass-deposit")
        ),
    ]
}

/// The confined receiver line every twin case below hides a twin beside.
fn machine_receiver(key: &Key) -> String {
    format!(
        "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {}",
        bare(key, "machine-token-deposit-key")
    )
}

#[test]
fn the_lines_the_tree_prints_are_clean_beside_an_unrelated_login_key() {
    let case = Case::new("audit-deposit-keys-clean");
    let keys = case.keys();
    let mut lines = vec![
        "# david's keys".to_string(),
        bare(&keys.david, "david@laptop"),
        String::new(),
    ];
    lines.extend(tree_lines(&keys));
    let ak = case.file("authorized_keys", &lines);
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 0,
        "the three lines the tree tells David to place must be clean:\n{out}"
    );
    assert!(
        out.lines()
            .last()
            .unwrap_or("")
            .starts_with("deposit-key-audit: clean — 3 line(s)"),
        "the verdict line counts the receiver lines it judged:\n{out}"
    );
}

#[test]
fn a_receiver_line_without_restrict_is_a_finding_naming_the_line() {
    let case = Case::new("audit-deposit-keys-unrestricted");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.david, "david@laptop"),
            format!(
                "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\" {}",
                bare(&keys.b, "machine-token-deposit-key")
            ),
        ],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "a receiver line without restrict must be a finding:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:2", ak.display())) && out.contains("restrict"),
        "the finding must name the file and line, and what it lacks:\n{out}"
    );
}

#[test]
fn an_option_after_restrict_that_reopens_it_is_a_finding() {
    let case = Case::new("audit-deposit-keys-reopened");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig\",restrict,pty {}",
            bare(&keys.c, "break-glass-deposit")
        )],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "restrict followed by pty gives the key a terminal again:\n{out}"
    );
    assert!(
        out.contains(":1") && out.contains("pty"),
        "the finding names the re-opening option:\n{out}"
    );
}

#[test]
fn restrict_inside_the_quoted_command_is_not_restrict() {
    let case = Case::new("audit-deposit-keys-quoted");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig,restrict\",no-pty {}",
            bare(&keys.c, "break-glass-deposit")
        )],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "a word inside the quoted command is not an option:\n{out}"
    );
}

#[test]
fn option_names_are_read_as_sshd_reads_them_without_case() {
    let case = Case::new("audit-deposit-keys-case");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[format!(
            "Command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig\",RESTRICT {}",
            bare(&keys.c, "break-glass-deposit")
        )],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 0,
        "sshd matches option names without case, and so does the audit:\n{out}"
    );
}

#[test]
fn the_first_live_enrolment_shape_names_the_bare_line() {
    // Packet 96a0f7bb: the key placed bare. Here it sits beside its
    // confined line, which is the shape a greeted push cannot see — sshd
    // takes whichever of the two it reaches first.
    let case = Case::new("audit-deposit-keys-bare-twin");
    let keys = case.keys();
    let mut lines = tree_lines(&keys);
    lines.insert(0, bare(&keys.b, "machine-token-deposit-key"));
    let ak = case.file("authorized_keys", &lines);
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "a confined key that is also on a bare line opens a shell:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains("shell"),
        "the finding names the BARE line and what it opens:\n{out}"
    );
    assert!(
        out.contains(&format!("{}:3", ak.display())),
        "the finding names the confined line the key belongs to:\n{out}"
    );
}

#[test]
fn a_twin_in_authorized_keys2_is_found_too() {
    let case = Case::new("audit-deposit-keys-two-files");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let ak2 = case.file("authorized_keys2", &[bare(&keys.a, "runner-deposit-key")]);
    let (rc, out) = case.run(&[&ak, &ak2]);
    assert_eq!(
        rc, 1,
        "sshd reads authorized_keys2 by default, so the audit does:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak2.display())),
        "{out}"
    );
}

#[test]
fn an_absent_file_is_clean_and_says_so() {
    let case = Case::new("audit-deposit-keys-absent");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let missing = case.root.join("authorized_keys2");
    let (rc, out) = case.run(&[&ak, &missing]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("absent"),
        "an absent file is named, not skipped in silence:\n{out}"
    );
}

#[test]
fn a_file_that_cannot_be_read_is_not_a_pass() {
    let case = Case::new("audit-deposit-keys-unreadable");
    let dir = case.root.join("authorized_keys");
    boss_testing::create_dir(&dir);
    let (rc, out) = case.run(&[&dir]);
    assert_eq!(
        rc, 2,
        "a path the audit could not read is UNVERIFIED, never clean:\n{out}"
    );
}

/// WHAT sshd TAKES AS ONE KEY, THE AUDIT TAKES AS ONE KEY (review
/// 1fa0d093, B1, measured against a real `sshd -i`, OpenSSH 9.2p1).
/// sshd splits a key line on space and tab only, and the base64 decoder
/// it hands the blob to skips every other whitespace byte, so each of
/// these bare twins opened the account's shell while the audit read the
/// file as clean: a CRLF line with no comment, a vertical tab or form
/// feed after the blob, a carriage return inside it.
#[test]
fn a_bare_twin_sshd_reads_through_stray_whitespace_is_still_a_twin() {
    let root = Case::new("audit-deposit-keys-ws");
    let key = keygen(&root.root, "machine-token-deposit-key", "ed25519");
    let (t, b) = (&key.kind, &key.blob);
    let half = b.len() / 2;
    for (name, twin) in [
        ("crlf", format!("{t} {b}\r")),
        ("vt", format!("{t} {b}\u{0b}")),
        ("ff-comment", format!("{t} {b}\u{0c} dep")),
        (
            "cr-inside",
            format!("{t} {}\r{} dep", &b[..half], &b[half..]),
        ),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-ws-{name}"));
        let ak = case.file("authorized_keys", &[twin, machine_receiver(&key)]);
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: sshd reads this bare line as the deposit key, so it is a shell:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())),
            "{name}: the finding names the bare line:\n{out}"
        );
    }
}

/// A RUN OF BLANKS AFTER THE OPTIONS IS STILL ONE GAP TO sshd (review
/// dd8f18e0, B3, measured against a real `sshd -i`, OpenSSH 9.2p1).
/// sshd steps past the options and then over EVERY space and tab before
/// it reads the key (`skip_space`); `ssh-keygen -l` steps over exactly
/// one. So each of these lines — the only line in its file — forces a
/// root door and is honoured by sshd with a pty, forwarding and the
/// account's ~/.ssh/rc, while ssh-keygen read no key from the whole line
/// and the audit, judging nothing it could not read, called the file
/// clean. The reviewer's case numbers are kept.
#[test]
fn a_receiver_line_with_a_blank_run_before_its_key_is_still_judged() {
    let keys = Case::new("audit-deposit-keys-blank-run");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    let cmd = "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\"";
    let dep = bare(&key, "machine-token-deposit-key");
    for (name, line, why) in [
        ("90-two-spaces", format!("{cmd}  {dep}"), "restrict"),
        ("91-tab-space", format!("{cmd}\t {dep}"), "restrict"),
        (
            "92-restrict-pty",
            format!("{cmd},restrict,pty  {dep}"),
            "pty",
        ),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-blank-run-{name}"));
        let ak = case.file("authorized_keys", &[line]);
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: sshd reads this line's key past the blank run, so it is judged:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains(why),
            "{name}: the finding names the line and what it grants ({why}):\n{out}"
        );
    }
}

/// A ROOT DOOR IN ANOTHER SPELLING IS STILL A ROOT DOOR (review dd8f18e0,
/// N6). A receiver line was recognised by the text of ROOT_DOORS, so
/// `/usr/local//libexec/boss/…` — the same file to the kernel, and to the
/// sudo that runs it — forced the receiver with NO restrict and read as
/// no receiver line at all, so the file was clean. Each absolute word of
/// a forced command is read as a path before it is matched.
#[test]
fn a_root_door_spelled_another_way_is_still_a_root_door() {
    let keys = Case::new("audit-deposit-keys-door-spelling");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    for (name, door) in [
        (
            "95-double-slash",
            "/usr/local//libexec/boss/ops-credential-recv",
        ),
        ("96-dot", "/usr/local/./libexec/boss/ops-credential-recv"),
        (
            "dotdot-into",
            "/usr/local/libexec/x/../boss/ops-credential-recv",
        ),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-door-spelling-{name}"));
        let ak = case.file(
            "authorized_keys",
            &[format!(
                "command=\"exec sudo -n {door} machine-token\" {}",
                bare(&key, "machine-token-deposit-key")
            )],
        );
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: a forced command running a root door is judged however the door is spelled:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())),
            "{name}: {out}"
        );
    }
}

/// ONE KEY, SEVERAL SPELLINGS (review ee6589a9, B1', measured against a
/// real `sshd -i`, OpenSSH 9.2p1). sshd decodes a blob and compares the
/// KEY, so each of these bare twins — the same key to sshd, spelled as
/// different text — opened the account's shell ahead of a correctly
/// confined receiver line while the audit, comparing base64 text, read
/// the file as clean. The reviewer's case numbers are kept. Each twin is
/// first checked to be the same key to ssh-keygen (sshd's own parser),
/// so the case cannot pass on a twin sshd would not honour.
#[test]
fn a_bare_twin_in_another_spelling_of_the_same_key_is_still_a_twin() {
    let keys = Case::new("audit-deposit-keys-spellings");
    let ed = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    let rsa = keygen(&keys.root, "machine-token-deposit-key-rsa", "rsa");
    let ed_parts = strings(&ed.blob);
    let rsa_parts = strings(&rsa.blob);
    assert_eq!(rsa_parts[0], b"ssh-rsa", "an RSA blob is ssh-rsa, e, n");
    let zero_led = |p: &[u8]| [&[0u8][..], p].concat();
    let cases: Vec<(&str, &Key, String)> = vec![
        (
            "83-ed25519-inner-type-ED25519",
            &ed,
            format!(
                "{} {} dep",
                ed.kind,
                blob_of(&[b"ED25519".to_vec(), ed_parts[1].clone()])
            ),
        ),
        (
            "61-rsa-e-leading-zero",
            &rsa,
            format!(
                "{} {} dep",
                rsa.kind,
                blob_of(&[
                    rsa_parts[0].clone(),
                    zero_led(&rsa_parts[1]),
                    rsa_parts[2].clone()
                ])
            ),
        ),
        (
            "62-rsa-n-extra-leading-zero",
            &rsa,
            format!(
                "{} {} dep",
                rsa.kind,
                blob_of(&[
                    rsa_parts[0].clone(),
                    rsa_parts[1].clone(),
                    zero_led(&rsa_parts[2])
                ])
            ),
        ),
        (
            "64-rsa-outer-type-rsa-sha2-512",
            &rsa,
            format!("rsa-sha2-512 {}", rsa.blob),
        ),
        (
            "66-rsa-outer-type-rsa-sha2-256",
            &rsa,
            format!("rsa-sha2-256 {}", rsa.blob),
        ),
    ];
    let mut missed = Vec::new();
    for (name, key, twin) in cases {
        let canonical = fingerprint(&bare(key, "dep"));
        assert!(canonical.is_some(), "{name}: ssh-keygen reads the key");
        assert_eq!(
            fingerprint(&twin),
            canonical,
            "{name}: the twin must be the same key to ssh-keygen, or the case proves nothing"
        );
        assert_ne!(
            twin.split_whitespace().take(2).collect::<Vec<_>>(),
            [key.kind.as_str(), key.blob.as_str()],
            "{name}: the twin must be spelled differently, or the case is the canonical one"
        );
        let case = Case::new(&format!("audit-deposit-keys-spelling-{name}"));
        let ak = case.file("authorized_keys", &[twin, machine_receiver(key)]);
        let (rc, out) = case.run(&[&ak]);
        if rc != 1
            || !(out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains("shell"))
        {
            missed.push(format!("{name}: exit {rc}\n{out}"));
        }
    }
    assert!(
        missed.is_empty(),
        "sshd reads each of these bare lines as the deposit key, so each is a shell, and the finding must name line 1:\n{}",
        missed.join("\n")
    );
}

/// Review ee6589a9, B1'' (case 65): the receiver line ITSELF in a
/// spelling the audit's own type check refused. sshd took
/// `rsa-sha2-256` as an RSA key and gave this unrestricted line every
/// forwarding, while the audit read no key from it and reported clean.
#[test]
fn a_receiver_line_typed_rsa_sha2_256_without_restrict_is_judged() {
    let case = Case::new("audit-deposit-keys-rsa-sha2-receiver");
    let rsa = keygen(&case.root, "machine-token-deposit-key", "rsa");
    let ak = case.file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\" rsa-sha2-256 {} dep",
            rsa.blob
        )],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "sshd honours this unrestricted receiver line, so it is a finding:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains("restrict"),
        "{out}"
    );
}

#[test]
fn a_receiver_line_without_restrict_and_ending_crlf_is_still_judged() {
    let case = Case::new("audit-deposit-keys-crlf-unrestricted");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[format!(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\" {} {}\r",
            keys.b.kind, keys.b.blob
        )],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 1,
        "sshd honours this line, so the audit judges it:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak.display())),
        "{out}"
    );
}

/// sshd stops reading a line at a NUL (its line is a C string); bash's
/// `read` drops the byte and joins what follows, so the audit would judge
/// a key sshd never sees and miss the one it does. It says so instead.
#[test]
fn a_file_carrying_a_nul_byte_is_unverified_never_clean() {
    let case = Case::new("audit-deposit-keys-nul");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            format!("{} {}\0AAAA dep", keys.b.kind, keys.b.blob),
            machine_receiver(&keys.b),
        ],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(rc, 2, "a NUL-bearing file cannot be judged:\n{out}");
    assert!(out.contains("UNVERIFIED") && out.contains("NUL"), "{out}");
}

/// A line ssh-keygen reads no key from, in a file set that grants no
/// root door, is named by file:line and judged no further: it is not a
/// deposit line, and the account's other lines are its owner's business.
#[test]
fn a_line_read_as_no_key_beside_no_receiver_is_named_not_judged() {
    let case = Case::new("audit-deposit-keys-unparsed-alone");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.david, "david@laptop"),
            "from=\"10.0.0.1\" not-a-key-type AAAAnotakey".to_string(),
        ],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains(&format!("{}:2", ak.display())),
        "the unread line is named:\n{out}"
    );
}

/// Review ee6589a9, N2: in a file set that grants a root door, a line
/// the audit read no key from is not left behind a clean verdict. The
/// key parser is sshd's, so such a line should grant nothing — but a
/// line nobody can say anything about, beside a root door, is not
/// evidence of a clean file.
#[test]
fn a_line_read_as_no_key_beside_a_receiver_line_is_not_clean() {
    let case = Case::new("audit-deposit-keys-unparsed");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            machine_receiver(&keys.b),
            "from=\"10.0.0.1\" not-a-key-type AAAAnotakey".to_string(),
        ],
    );
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!("UNCONFINED {}:2", ak.display())),
        "the unread line is named as the one to act on:\n{out}"
    );
}

/// The audit's option split must reach the key ssh-keygen reads from the
/// line; where it does not, the line is a finding, never clean (review
/// ee6589a9, B1: "a line ssh-keygen reads a key from but the parser does
/// not must not read clean"). With the real ssh-keygen the two cannot
/// differ — the split follows its loop — so a stand-in parser that reads
/// a key from the WHOLE line only stands for whatever sshd release next
/// reads one the split does not reach.
#[test]
fn a_line_whose_key_the_option_split_cannot_reach_is_a_finding() {
    let case = Case::new("audit-deposit-keys-split-disagrees");
    let keys = case.keys();
    let odd = format!(
        "frobnicate=\"x\" {}",
        bare(&keys.david, "a-line-a-newer-sshd-reads")
    );
    let stub = case.root.join("ssh-keygen");
    write_exec(
        &stub,
        "#!/usr/bin/env bash\n\
         # Reads a key from exactly one line, the whole of ODD_LINE.\n\
         line=$(cat)\n\
         [ \"$line\" = \"$ODD_LINE\" ] || { echo '(stdin) is not a public key file.'; exit 255; }\n\
         echo '256 SHA256:stub-fingerprint a-line-a-newer-sshd-reads (ED25519)'\n",
    );
    let ak = case.file("authorized_keys", std::slice::from_ref(&odd));
    let (rc, out) = case.run_with(
        &[&ak],
        &[
            ("BOSS_DEPOSIT_AUDIT_SSH_KEYGEN", stub.to_str().unwrap()),
            ("ODD_LINE", odd.as_str()),
        ],
    );
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak.display())),
        "{out}"
    );
}

/// The key parser is the audit's evidence: without it nothing about any
/// line can be said, and that is UNVERIFIED, never clean.
#[test]
fn an_audit_without_its_key_parser_is_unverified() {
    let case = Case::new("audit-deposit-keys-no-keygen");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let absent = case.root.join("no-such-ssh-keygen");
    let (rc, out) = case.run_with(
        &[&ak],
        &[("BOSS_DEPOSIT_AUDIT_SSH_KEYGEN", absent.to_str().unwrap())],
    );
    assert_eq!(rc, 2, "{out}");
    assert!(out.contains("UNVERIFIED"), "{out}");
}

/// N1 of review 1fa0d093: a forced command that merely MENTIONS the
/// root-door directory read as a confined receiver. sshd hands a forced
/// command to the account's shell, so `echo …; : /usr/local/libexec/…`
/// runs whatever precedes the path. A line naming a root door must run
/// that door and nothing else.
#[test]
fn a_forced_command_naming_a_root_door_must_be_that_door_alone() {
    let keys = Case::new("audit-deposit-keys-not-alone");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    for (name, cmd) in [
        ("shell", "echo SHELL-OK; : /usr/local/libexec/boss/x"),
        (
            "trailing",
            "exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token; bash",
        ),
        (
            "dotdot",
            "exec sudo -n /usr/local/libexec/boss/../../bin/bash",
        ),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-not-alone-{name}"));
        let ak = case.file(
            "authorized_keys",
            &[format!(
                "command=\"{cmd}\",restrict {}",
                bare(&key, "machine-token-deposit-key")
            )],
        );
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: a forced command around a root door is a finding:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())),
            "{name}: {out}"
        );
    }
}

#[test]
fn the_audit_and_both_receiver_installers_name_one_root_door_directory() {
    // The audit recognises a receiver line by the directory its forced
    // command runs from; each installer puts its receiver there. A
    // directory moved in one place and not the other would leave every
    // receiver line unjudged — clean by default (CLAUDE.md §9a).
    let audit = read(&repo_root().join(SCRIPT));
    assert!(
        audit.contains("ROOT_DOORS='/usr/local/libexec/boss/'"),
        "{SCRIPT} must declare ROOT_DOORS='/usr/local/libexec/boss/'"
    );
    for installer in [
        "infra/gcp/install-ops-credential-receiver.sh",
        "infra/gcp/install-runner-credential-receiver.sh",
    ] {
        assert!(
            read(&repo_root().join(installer))
                .contains("LIBEXEC=\"${INSTALL_RECV_LIBEXEC:-/usr/local/libexec/boss}\""),
            "{installer} must install its receiver under /usr/local/libexec/boss, the directory the audit reads"
        );
    }
}

#[test]
fn the_converge_runs_the_audit_and_carries_its_verdict() {
    let converge = read(&repo_root().join("infra/gcp/boss-gcp-converge.sh"));
    assert!(
        converge.contains("gcp/audit-deposit-keys.sh"),
        "boss-gcp-converge.sh must run {SCRIPT} on every tick"
    );
    assert!(
        converge.contains("run_summary_field deposit_key_audit"),
        "the converge must put the audit's verdict on its packet as deposit_key_audit"
    );
    // The bound runs INSIDE runuser (review dd8f18e0, N8): runuser
    // starts its command in a session of its own, so a `timeout` outside
    // it kills runuser and leaves the audit's children running. Pinned
    // by text in the unprivileged gate; gcp_deposit_audit_transport also
    // exercises the command boundary and a real uid drop when run as root.
    let runuser = converge
        .lines()
        .position(|l| l.contains("runuser -u \"$DEPOSIT_ACCOUNT\" --") && l.contains("audit_bound"))
        .expect("the converge runs the audit through runuser as the deposit account");
    assert!(
        converge.contains("DEPOSIT_ACCOUNT=\"${BOSS_GCP_DEPOSIT_USER:-$OWNER}\""),
        "root generation ownership must not replace the bound deposit account; legacy ticks use the checkout owner"
    );
    assert!(
        converge.lines().nth(runuser + 1).is_some_and(|l| l
            .trim_start()
            .starts_with("timeout -k 5 \"$audit_bound\" bash -s --")),
        "the audit's own bound must run inside runuser, so it kills the audit's process group"
    );
    // The one word the alarm rule's `when` compares (review 1fa0d093,
    // B2): a finding rides an ok tick and is filed from the packet.
    let rule =
        read(&repo_root().join(
            "infra/dispatcher/rules/file-backlog-items-on-boss-gcp-deposit-key-unconfined.toml",
        ));
    assert!(
        converge.contains("run_summary_field deposit_key_verdict unconfined")
            && rule.contains("metadata.deposit_key_verdict = \"unconfined\""),
        "the converge's verdict word and the rule that alarms on it must agree"
    );
}
