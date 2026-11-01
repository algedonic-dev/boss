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
//! absent file beside a read one is clean and says so; a file that cannot
//! be read, a set with no file read, an option placed after a file, or an
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
        self.run_args(&[], files, env)
    }

    /// The audit told which keys the prepare packets recorded: one
    /// `--prepared-key <public key>` per key, then the files.
    fn run_prepared(&self, prepared: &[&Key], files: &[&Path]) -> (i32, String) {
        let args: Vec<String> = prepared
            .iter()
            .flat_map(|k| ["--prepared-key".to_string(), bare(k, "prepared")])
            .collect();
        self.run_args(&args, files, &[])
    }

    fn run_args(&self, args: &[String], files: &[&Path], env: &[(&str, &str)]) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(args)
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

/// A PREPARED KEY IS JUDGED WHATEVER ITS COMMAND SAYS (backlog 77920d6a;
/// review 728c6f76 of car 3cd92f7a). A receiver line was recognised only
/// by the text of its forced command, and sshd hands that command to the
/// account's shell, which reads quotes, escapes, variables and globs the
/// text match does not. So each of these lines — the reviewer's shapes,
/// alone in the file and WITHOUT restrict — runs the root door with a
/// pty, forwarding and the account's ~/.ssh/rc open, and read clean. Told
/// the key the prepare packet recorded, the audit judges the line by its
/// key, and a command that is not the installers' spelling is a finding.
#[test]
fn a_prepared_key_is_judged_whatever_its_forced_command_says() {
    let keys = Case::new("audit-deposit-keys-prepared-spelling");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
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
        let case = Case::new(&format!("audit-deposit-keys-prepared-spelling-{name}"));
        let ak = case.file(
            "authorized_keys",
            &[format!(
                "command=\"{cmd}\" {}",
                bare(&key, "machine-token-deposit-key")
            )],
        );
        let (rc, out) = case.run_prepared(&[&key], &[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: a line carrying a prepared deposit key is judged by its key:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains("prepared"),
            "{name}: the finding names the line and says the key is a prepared one:\n{out}"
        );
    }
}

/// The bare key ALONE — the case the twin rule could not reach, because
/// there is no confined line to tie it to (backlog 77920d6a). The senders
/// refuse it at the next push (no greeting), but until then whoever
/// holds the key has the account's shell, and the audit read it as one
/// of the owner's own login keys.
#[test]
fn a_prepared_key_on_a_bare_line_alone_is_a_finding() {
    let case = Case::new("audit-deposit-keys-prepared-bare");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.david, "david@laptop"),
            bare(&keys.b, "machine-token-deposit-key"),
        ],
    );
    let (rc, out) = case.run_prepared(&[&keys.b], &[&ak]);
    assert_eq!(rc, 1, "a prepared key on a bare line opens a shell:\n{out}");
    assert!(
        out.contains(&format!("UNCONFINED {}:2", ak.display())) && out.contains("shell"),
        "the finding names the bare line and what it opens:\n{out}"
    );
    assert!(
        !out.contains(&format!("UNCONFINED {}:1", ak.display())),
        "a key no prepare packet recorded is the owner's business, not a finding:\n{out}"
    );
}

/// A bare twin of a prepared key beside its confined line is ONE finding
/// on the bare line, not one from the key and another from the twin rule:
/// the chore judge files one item per finding line (backlog d1b03874).
#[test]
fn a_prepared_bare_twin_is_named_once() {
    let case = Case::new("audit-deposit-keys-prepared-twin");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.b, "machine-token-deposit-key"),
            machine_receiver(&keys.b),
        ],
    );
    let (rc, out) = case.run_prepared(&[&keys.b], &[&ak]);
    assert_eq!(rc, 1, "{out}");
    assert_eq!(
        out.lines()
            .filter(|l| l.starts_with("deposit-key-audit: UNCONFINED "))
            .count(),
        1,
        "one line, one finding:\n{out}"
    );
    assert!(
        out.contains(&format!("UNCONFINED {}:1", ak.display())),
        "{out}"
    );
}

/// The three lines the tree prints, each key prepared, stay clean, and
/// the verdict says how many prepared keys it judged by.
#[test]
fn the_lines_the_tree_prints_stay_clean_when_their_keys_are_prepared() {
    let case = Case::new("audit-deposit-keys-prepared-clean");
    let keys = case.keys();
    let mut lines = vec![bare(&keys.david, "david@laptop")];
    lines.extend(tree_lines(&keys));
    let ak = case.file("authorized_keys", &lines);
    let (rc, out) = case.run_prepared(&[&keys.a, &keys.b, &keys.c, &keys.b], &[&ak]);
    assert_eq!(rc, 0, "{out}");
    let verdict = out.lines().last().unwrap_or("");
    assert!(
        verdict.starts_with("deposit-key-audit: clean — 3 line(s)")
            && verdict.contains("3 prepared deposit key(s)"),
        "the verdict counts the receiver lines and the distinct prepared keys:\n{out}"
    );
}

/// A prepared key the parser reads no key from leaves the audit unable
/// to find the lines it was told to judge: UNVERIFIED, never clean. So is
/// a --prepared-key with nothing after it, an option the audit does not
/// know, and options with no file.
#[test]
fn a_prepared_key_that_cannot_be_read_is_unverified() {
    let case = Case::new("audit-deposit-keys-prepared-unreadable");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let akp = ak.to_str().unwrap().to_string();
    let good = bare(&keys.b, "prepared");
    for (name, args) in [
        (
            "not-a-key",
            vec!["--prepared-key", "ssh-ed25519 AAAAnot-a-key", &akp],
        ),
        (
            "two-keys",
            vec!["--prepared-key", &format!("{good}\n{good}"), &akp],
        ),
        ("no-value", vec!["--prepared-key"]),
        ("unknown-option", vec!["--prepared-keys", &good, &akp]),
        ("no-file", vec!["--prepared-key", &good]),
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, 2, "{name}: {out}");
        assert!(out.contains("UNVERIFIED"), "{name}: {out}");
    }
}

/// AN OPTION AFTER A FILE IS NEVER AN ABSENT FILE (backlog 26173f40;
/// review c403f23e of car 5cc2731a). The options are read up to the first
/// file, and the audit read a `--prepared-key` placed after one as two
/// more paths, both absent — and an absent file is clean: the file below
/// holds the prepared key BARE, a shell to whoever holds it, and the
/// audit exited 0 calling it clean without judging the key. Each word
/// after the first file that is not an absolute path is refused, named,
/// before any file is read. The same key placed BEFORE the file — the
/// shape the converge passes (77920d6a) — is judged, and is the finding.
#[test]
fn an_option_after_a_file_is_refused_never_read_as_an_absent_file() {
    let case = Case::new("audit-deposit-keys-option-after-file");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.david, "david@laptop"),
            bare(&keys.b, "machine-token-deposit-key"),
        ],
    );
    let akp = ak.to_str().unwrap().to_string();
    let good = bare(&keys.b, "prepared");

    let (rc, out) = case.run_prepared(&[&keys.b], &[&ak]);
    assert_eq!(
        rc, 1,
        "the control: a prepared key before the file is judged:\n{out}"
    );

    for (name, args, named) in [
        (
            "prepared-key-after-the-file",
            vec![akp.as_str(), "--prepared-key", &good],
            "--prepared-key",
        ),
        (
            "unknown-option-after-the-file",
            vec![akp.as_str(), "--prepared-keys", &good],
            "--prepared-keys",
        ),
        (
            "short-option-after-the-file",
            vec![akp.as_str(), "-p", &good],
            "-p",
        ),
        (
            "relative-path-after-the-file",
            vec![akp.as_str(), "authorized_keys2"],
            "authorized_keys2",
        ),
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(
            rc, 2,
            "{name}: an argument read as an absent file is not clean:\n{out}"
        );
        let verdict = out.lines().last().unwrap_or("");
        assert!(
            verdict.contains("UNVERIFIED") && verdict.contains(&format!("'{named}'")),
            "{name}: the refusal names the argument it would not read as a file:\n{out}"
        );
        assert!(!out.contains("deposit-key-audit: clean"), "{name}: {out}");
    }
}

/// A SET IN WHICH NO FILE WAS READ IS NOT CLEAN. An absent file beside a
/// read one is named and passed over (authorized_keys2 is absent on most
/// hosts); but when every file named is absent the audit read no line at
/// all, and "clean — 0 line(s) in (none present)" was its verdict, with or
/// without the keys it was told to judge.
#[test]
fn a_set_in_which_no_file_was_read_is_unverified_never_clean() {
    let case = Case::new("audit-deposit-keys-none-read");
    let keys = case.keys();
    let missing = case.root.join("authorized_keys");
    let missing2 = case.root.join("authorized_keys2");
    let (rc, out) = case.run(&[&missing, &missing2]);
    assert_eq!(rc, 2, "no file read is UNVERIFIED, never clean:\n{out}");
    assert!(out.contains("UNVERIFIED"), "{out}");
    assert!(
        out.contains(missing.to_str().unwrap()),
        "the refusal names the files it did not find:\n{out}"
    );
    let (rc, out) = case.run_prepared(&[&keys.b], &[&missing]);
    assert_eq!(rc, 2, "prepared keys with no file read:\n{out}");
    assert!(out.contains("UNVERIFIED"), "{out}");
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

/// THE ONE sudo FLAG IS -n (backlog e85c4dbd; review 1fa0d093, N5).
/// Every installer prints `exec sudo -n <door>`, and the shape check let
/// sudo carry ANY run of flags, leaving sudoers alone to bound what they
/// do: `-E` hands the receiver the caller's environment, `-s` and `-i`
/// run it through a shell. A flag no installer prints is a finding.
#[test]
fn a_receiver_line_whose_sudo_carries_a_flag_but_n_is_a_finding() {
    let keys = Case::new("audit-deposit-keys-sudo-flags");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    for (name, sudo) in [
        ("preserve-env", "sudo -E"),
        ("joined", "sudo -nE"),
        ("after-n", "sudo -n -E"),
        ("shell", "sudo -n -s"),
        ("login", "sudo -i"),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-sudo-{name}"));
        let ak = case.file(
            "authorized_keys",
            &[format!(
                "command=\"exec {sudo} /usr/local/libexec/boss/ops-credential-recv machine-token\",restrict {}",
                bare(&key, "machine-token-deposit-key")
            )],
        );
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: sudo with a flag no installer prints is a finding:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())),
            "{name}: {out}"
        );
    }
}

/// environment= ON A RECEIVER LINE IS A FINDING (backlog e85c4dbd; review
/// 1fa0d093, N4). With PermitUserEnvironment it sets what the account's
/// shell runs the forced command with — BASH_ENV alone has bash source a
/// file of the line author's choosing before the receiver starts — and
/// no installer prints one. A login key's own options stay its owner's.
#[test]
fn a_receiver_line_that_sets_environment_is_a_finding() {
    let keys = Case::new("audit-deposit-keys-environment");
    let key = keygen(&keys.root, "machine-token-deposit-key", "ed25519");
    let cmd = "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token\"";
    for (name, opts) in [
        (
            "after",
            format!("{cmd},restrict,environment=\"BASH_ENV=/srv/x\""),
        ),
        (
            "before",
            format!("environment=\"LD_PRELOAD=/srv/x.so\",{cmd},restrict"),
        ),
        (
            "case",
            format!("{cmd},restrict,Environment=\"BASH_ENV=/srv/x\""),
        ),
    ] {
        let case = Case::new(&format!("audit-deposit-keys-environment-{name}"));
        let ak = case.file(
            "authorized_keys",
            &[format!(
                "{opts} {}",
                bare(&key, "machine-token-deposit-key")
            )],
        );
        let (rc, out) = case.run(&[&ak]);
        assert_eq!(
            rc, 1,
            "{name}: a receiver line that sets environment= is a finding:\n{out}"
        );
        assert!(
            out.contains(&format!("UNCONFINED {}:1", ak.display())) && out.contains("environment"),
            "{name}: the finding names the line and the option:\n{out}"
        );
    }
    // The control: the same option on the owner's login line, beside the
    // three lines the tree prints, is not judged.
    let case = Case::new("audit-deposit-keys-environment-login");
    let keys = case.keys();
    let mut lines = vec![format!(
        "environment=\"EDITOR=vi\" {}",
        bare(&keys.david, "david@laptop")
    )];
    lines.extend(tree_lines(&keys));
    let ak = case.file("authorized_keys", &lines);
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(
        rc, 0,
        "environment= on a login line is its owner's business:\n{out}"
    );
}

/// The forced command a broker rule prepares its deposit key for, read
/// from the rule row — the one definition the broker joins into the
/// enroll step's `authorized_keys_line` (broker_transport_key.rs).
fn rule_forced_command(rule: &str) -> String {
    let path = format!("infra/dispatcher/rules/broker-prepares-the-{rule}-deposit-key.toml");
    let row: toml::Value = toml::from_str(&read(&repo_root().join(&path))).unwrap();
    row["rule"][0]["do"][0]["args"]["forced_command"]
        .as_str()
        .unwrap_or_else(|| panic!("{path} declares its forced command"))
        .trim_matches('"')
        .to_string()
}

/// The line a prepare packet records for `key` under `rule`: its forced
/// command joined to the key, as the broker joins it.
fn prepared_line(rule: &str, key: &Key) -> String {
    format!(
        "command=\"{}\",restrict {}",
        rule_forced_command(rule),
        bare(key, rule)
    )
}

/// The audit told each key AND the receiver it was prepared for: one
/// `--prepared-key <authorized_keys_line>` per key, then the files.
fn run_bound(case: &Case, bound: &[(&str, &Key)], files: &[&Path]) -> (i32, String) {
    let args: Vec<String> = bound
        .iter()
        .flat_map(|(rule, key)| ["--prepared-key".to_string(), prepared_line(rule, key)])
        .collect();
    case.run_args(&args, files, &[])
}

/// A PREPARED KEY IS BOUND TO ITS OWN RECEIVER (backlog c422cf68). The
/// audit judged a line carrying a prepared key by whether its command was
/// SOME root door, so the runner's deposit key forced to the machine-token
/// receiver — or the other way round — read clean, each line confined and
/// restricted, while each key reached a root door it was never prepared
/// or enrolled for. Told the line each key was prepared with, the audit
/// judges the pair: one finding per line, naming both commands; the line
/// that forces the key's own receiver beside it stays clean.
#[test]
fn a_prepared_key_forced_to_another_receiver_is_a_finding() {
    let case = Case::new("audit-deposit-keys-prepared-crossed");
    let keys = case.keys();
    let runner_cmd = rule_forced_command("runner");
    let machine_cmd = rule_forced_command("machine-token");
    let ak = case.file(
        "authorized_keys",
        &[
            bare(&keys.david, "david@laptop"),
            format!(
                "command=\"{machine_cmd}\",restrict {}",
                bare(&keys.a, "runner-deposit-key")
            ),
            format!(
                "command=\"{runner_cmd}\",restrict {}",
                bare(&keys.b, "machine-token-deposit-key")
            ),
            prepared_line("machine-token", &keys.b),
        ],
    );
    let (rc, out) = run_bound(
        &case,
        &[("runner", &keys.a), ("machine-token", &keys.b)],
        &[&ak],
    );
    assert_eq!(
        rc, 1,
        "a deposit key behind another key's receiver reaches a door it was not prepared for:\n{out}"
    );
    for (n, forced, prepared) in [
        (2, &machine_cmd, &runner_cmd),
        (3, &runner_cmd, &machine_cmd),
    ] {
        let finding = out
            .lines()
            .find(|l| {
                l.starts_with(&format!(
                    "deposit-key-audit: UNCONFINED {}:{n} ",
                    ak.display()
                ))
            })
            .unwrap_or_else(|| panic!("line {n} is a finding:\n{out}"));
        assert!(
            finding.contains(&format!("forced to '{forced}'"))
                && finding.contains(&format!("prepared for '{prepared}'")),
            "line {n}'s finding names the receiver it forces and the one it was prepared for:\n{out}"
        );
    }
    assert_eq!(
        out.lines()
            .filter(|l| l.starts_with("deposit-key-audit: UNCONFINED "))
            .count(),
        2,
        "one finding per crossed line, none for the key's own receiver line or the login key:\n{out}"
    );
}

/// Each key the broker rules prepare for boss-gcp, behind the receiver
/// its own rule names, is clean, and the verdict says how many keys were
/// judged against their receivers.
#[test]
fn each_prepared_key_behind_its_own_receiver_stays_clean() {
    let case = Case::new("audit-deposit-keys-prepared-bound");
    let keys = case.keys();
    let bound = [
        ("runner", &keys.a),
        ("machine-token", &keys.b),
        ("root-tree", &keys.c),
    ];
    let mut lines = vec![bare(&keys.david, "david@laptop")];
    lines.extend(bound.iter().map(|(rule, key)| prepared_line(rule, key)));
    let ak = case.file("authorized_keys", &lines);
    let (rc, out) = run_bound(&case, &bound, &[&ak]);
    assert_eq!(rc, 0, "{out}");
    let verdict = out.lines().last().unwrap_or("");
    assert!(
        verdict.starts_with("deposit-key-audit: clean — 3 line(s)")
            && verdict.contains("3 prepared deposit key(s) judged by fingerprint")
            && verdict.contains("3 of them against the receiver each was prepared for"),
        "the verdict counts the prepared keys and how many were bound to a receiver:\n{out}"
    );
}

/// A prepared line that forces no command binds its key to nothing, and
/// one key prepared for two receivers cannot be judged against either:
/// both are UNVERIFIED, never a key judged by fingerprint alone.
#[test]
fn a_prepared_line_that_names_no_single_receiver_is_unverified() {
    let case = Case::new("audit-deposit-keys-prepared-unbound");
    let keys = case.keys();
    let ak = case.file(
        "authorized_keys",
        &[prepared_line("machine-token", &keys.b)],
    );
    let akp = ak.to_str().unwrap().to_string();
    for (name, values) in [
        (
            "no-command",
            vec![format!("restrict {}", bare(&keys.b, "prepared"))],
        ),
        (
            "two-receivers",
            vec![
                prepared_line("machine-token", &keys.b),
                prepared_line("runner", &keys.b),
            ],
        ),
    ] {
        let mut args: Vec<String> = values
            .into_iter()
            .flat_map(|v| ["--prepared-key".to_string(), v])
            .collect();
        args.push(akp.clone());
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, 2, "{name}: {out}");
        assert!(out.contains("UNVERIFIED"), "{name}: {out}");
    }
}

/// What `audit-deposit-keys.sh --keys-files-of <account>` prints, as
/// (exit status, stdout, stderr), with `sshd` standing for sshd.
fn keys_files_of(case: &Case, account: &str, sshd: &Path) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .args(["--keys-files-of", account])
        .env("BOSS_DEPOSIT_AUDIT_SSHD", sshd)
        .env("SSHD_ARGS", case.root.join("sshd.args"))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A stand-in sshd that records its argv and prints `body` as its -T.
fn sshd_stub(case: &Case, name: &str, body: &str, status: i32) -> PathBuf {
    let stub = case.root.join(name);
    write_exec(
        &stub,
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" > \"$SSHD_ARGS\"\nprintf '%s' '{body}'\nexit {status}\n"
        ),
    );
    stub
}

/// root's home, as the passwd database the script reads holds it.
fn root_home() -> String {
    let out = Command::new("getent")
        .args(["passwd", "root"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim_end()
        .split(':')
        .nth(5)
        .expect("getent passwd root names a home")
        .to_string()
}

#[test]
fn both_environment_settings_ride_one_sshd_read_and_are_judged_together() {
    let case = Case::new("audit-deposit-keys-both-settings");
    let keys = case.keys();
    let ak = case.file("deposit_keys", &tree_lines(&keys));
    for (pue, accept, expected) in [
        ("no", "LANG LC_*", 0),
        ("yes", "LANG LC_*", 1),
        ("no", "BASH_ENV", 1),
        ("yes", "BASH_ENV", 1),
        ("no", "", 0),
    ] {
        let calls = case.root.join("sshd.args");
        write_file(&calls, "");
        let stub = case.root.join("sshd-both");
        write_exec(
            &stub,
            &format!(
                "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$SSHD_ARGS\"\nprintf '%s\\n' 'authorizedkeysfile {}' 'permituserenvironment {pue}' 'acceptenv {accept}'\n",
                ak.display()
            ),
        );
        let (rc, list, err) = keys_files_of(&case, "root", &stub);
        assert_eq!(rc, 0, "{list}{err}");
        assert_eq!(
            list,
            format!(
                "--permit-user-environment\n{pue}\n--accept-env\n{accept}\n{}\n",
                ak.display()
            ),
            "both settings must precede files, PUE first, from the same sshd output"
        );
        assert_eq!(read(&calls).lines().count(), 1, "one sshd read");
        let args: Vec<_> = list.lines().map(str::to_string).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, expected, "PUE={pue}, AcceptEnv={accept}: {out}");
        assert!(
            !out.contains("was not read"),
            "both settings were read: {out}"
        );
        assert_eq!(
            out.contains("UNCONFINED sshd:PermitUserEnvironment"),
            pue == "yes",
            "{out}"
        );
        assert_eq!(
            out.contains("UNCONFINED sshd AcceptEnv"),
            accept == "BASH_ENV",
            "{out}"
        );
    }
}

#[test]
fn both_environment_options_after_a_file_are_refused() {
    let case = Case::new("audit-deposit-keys-both-options-first");
    let keys = case.keys();
    let ak = case.file("deposit_keys", &tree_lines(&keys));
    for option in ["--permit-user-environment", "--accept-env"] {
        let args = vec![
            ak.display().to_string(),
            option.to_string(),
            "no".to_string(),
        ];
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(
            rc, 2,
            "an option after a file must not be an absent harmless path: {out}"
        );
        assert!(
            out.contains("UNVERIFIED") && !out.contains("deposit-key-audit: clean"),
            "{out}"
        );
    }
}

/// THE FILES ARE THE ONES sshd READS (backlog e85c4dbd; review dd8f18e0,
/// N7). The converge handed the audit the default pair under the
/// account's home, so a host whose sshd reads another AuthorizedKeysFile
/// was audited on files sshd never opens and read clean. The audit now
/// asks sshd: `sshd -T -C user=<account>` (Match blocks applied), each
/// pattern expanded the way sshd expands it — %h, %u, %U and %%, a
/// relative path under the home.
#[test]
fn the_files_audited_are_the_ones_sshd_reads_for_the_account() {
    let case = Case::new("audit-deposit-keys-files-of");
    let stub = sshd_stub(
        &case,
        "sshd",
        "port 22\nauthorizedkeysfile .ssh/authorized_keys /etc/ssh/keys/%u %h/.ssh/k_%U /srv/100%%\npermituserenvironment no\nauthorizedkeyscommand none\n",
        0,
    );
    let (rc, out, err) = keys_files_of(&case, "root", &stub);
    assert_eq!(rc, 0, "{out}{err}");
    let home = root_home();
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec![
            "--permit-user-environment".to_string(),
            "no".to_string(),
            "--accept-env".to_string(),
            String::new(),
            format!("{home}/.ssh/authorized_keys"),
            "/etc/ssh/keys/root".to_string(),
            format!("{home}/.ssh/k_0"),
            "/srv/100%".to_string(),
        ],
        "PermitUserEnvironment first, AcceptEnv next, then each expanded AuthorizedKeysFile:\n{out}{err}"
    );
    let args = read(&case.root.join("sshd.args"));
    assert!(
        args.split_whitespace().any(|a| a == "-T") && args.contains("-C user=root"),
        "sshd is asked for its configuration AS the account, so Match User applies: {args}"
    );
}

/// No evidence is not a pass: a file list the audit cannot get from sshd
/// is UNVERIFIED, and no path is printed for the converge to audit.
#[test]
fn files_sshd_cannot_name_are_unverified_never_the_default_pair() {
    let case = Case::new("audit-deposit-keys-files-of-unverified");
    let ok = "authorizedkeysfile .ssh/authorized_keys\npermituserenvironment no\n";
    for (name, account, body, status) in [
        (
            "refuses",
            "root",
            "sshd: no hostkeys available -- exiting.\n",
            1,
        ),
        ("no-line", "root", "port 22\n", 0),
        ("none", "root", "authorizedkeysfile none\n", 0),
        (
            "unknown-token",
            "root",
            "authorizedkeysfile %h/.ssh/%x\n",
            0,
        ),
        (
            "trailing-percent",
            "root",
            "authorizedkeysfile /srv/keys%\n",
            0,
        ),
        ("no-account", "no-such-account-e85c4dbd", ok, 0),
        // The files are named and the setting that decides whether sshd
        // also reads ~/.ssh/environment is not (backlog f5ff8170): the
        // audit could not judge that door, so the list is not handed on.
        (
            "no-permituserenvironment",
            "root",
            "authorizedkeysfile .ssh/authorized_keys\n",
            0,
        ),
    ] {
        let stub = sshd_stub(&case, &format!("sshd-{name}"), body, status);
        let (rc, out, err) = keys_files_of(&case, account, &stub);
        assert_eq!(rc, 2, "{name}: {out}{err}");
        assert!(
            out.contains("UNVERIFIED") && !out.contains("authorized_keys\n"),
            "{name}: one UNVERIFIED line on stdout and no file to audit:\n{out}{err}"
        );
    }
    let (rc, out, err) = keys_files_of(&case, "root", &case.root.join("no-such-sshd"));
    assert_eq!(rc, 2, "an absent sshd: {out}{err}");
    assert!(out.contains("UNVERIFIED"), "{out}{err}");
}

/// The same read against a REAL sshd, where this host can run `sshd -T`:
/// a Match User block naming the account moves its AuthorizedKeysFile,
/// and the list follows it.
#[test]
fn a_real_sshd_match_block_moves_the_files_audited() {
    let real = Path::new("/usr/sbin/sshd");
    if !real.exists() {
        eprintln!("NOT RUN: no /usr/sbin/sshd on this host; the stand-in cases above still run");
        return;
    }
    let case = Case::new("audit-deposit-keys-files-of-real-sshd");
    keygen(&case.root, "host-key", "ed25519");
    let cfg = case.root.join("sshd_config");
    write_file(
        &cfg,
        &format!(
            "HostKey {}\nAuthorizedKeysFile .ssh/authorized_keys .ssh/authorized_keys2\nAcceptEnv LANG LC_*\nMatch User root\n    AuthorizedKeysFile /etc/ssh/deposit/%u\n",
            case.root.join("host-key").display()
        ),
    );
    let wrapper = case.root.join("sshd");
    write_exec(
        &wrapper,
        &format!(
            "#!/usr/bin/env bash\nexec {} -f {} \"$@\"\n",
            real.display(),
            cfg.display()
        ),
    );
    let control = Command::new(&wrapper).arg("-T").output().unwrap();
    if !control.status.success() {
        eprintln!(
            "NOT RUN: sshd -T refuses here ({}); the stand-in cases above still run",
            String::from_utf8_lossy(&control.stderr).trim()
        );
        return;
    }
    let (rc, out, err) = keys_files_of(&case, "root", &wrapper);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(
        out, "--permit-user-environment\nno\n--accept-env\nLANG LC_*\n/etc/ssh/deposit/root\n",
        "{err}"
    );
}

/// ACCEPTENV IS READ WITH THE FILES (backlog 54104a05). A client asks
/// sshd to set environment variables, and sshd sets each one its
/// AcceptEnv admits — `restrict` does not close it — before handing a
/// forced command to the account's shell. BASH_ENV admitted that way has
/// bash run a command of the sender's choosing first, a third route to
/// what `environment=` (with PermitUserEnvironment) already reaches. The
/// patterns come from the same `sshd -T -C user=<account>` the files do,
/// and `--keys-files-of` hands them over as the audit's own option,
/// ahead of the files, however sshd spreads them over lines.
#[test]
fn the_accept_env_sshd_reads_rides_ahead_of_the_files() {
    let case = Case::new("audit-deposit-keys-files-of-accept-env");
    let stub = sshd_stub(
        &case,
        "sshd",
        "acceptenv LANG\nacceptenv LC_* BASH_ENV\nauthorizedkeysfile /etc/ssh/keys/%u\npermituserenvironment no\n",
        0,
    );
    let (rc, out, err) = keys_files_of(&case, "root", &stub);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec![
            "--permit-user-environment",
            "no",
            "--accept-env",
            "LANG LC_* BASH_ENV",
            "/etc/ssh/keys/root"
        ],
        "the AcceptEnv patterns, as one option, then the files:\n{out}{err}"
    );
}

/// What the audit does with them. A receiver line beside an AcceptEnv
/// that admits a name the account's shell reads before the receiver
/// starts is a finding — one per admitting pattern, naming it, and
/// naming no key and no count, because the chore judge keys a finding
/// that names no key on its whole text (chore_file_reds.rs,
/// finding_key) and the act is one edit to sshd's config. sshd matches
/// `*` and `?` and nothing else, so `[B]ASH_ENV` is a literal name, not
/// a glob that admits BASH_ENV. The owner's logins alone are not judged.
#[test]
fn an_accept_env_that_admits_a_shell_start_up_name_beside_a_receiver_is_a_finding() {
    let case = Case::new("audit-deposit-keys-accept-env");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let run = |patterns: &str, files: &[&Path]| {
        case.run_args(
            &["--accept-env".to_string(), patterns.to_string()],
            files,
            &[],
        )
    };
    for patterns in ["", "LANG LC_*", "LANG LC_* [B]ASH_ENV", "TERM COLORTERM"] {
        let (rc, out) = run(patterns, &[&ak]);
        assert_eq!(
            rc, 0,
            "AcceptEnv '{patterns}' admits no shell start-up name:\n{out}"
        );
        assert!(
            out.contains("AcceptEnv (") && out.contains("admits no name"),
            "the clean verdict says AcceptEnv was read:\n{out}"
        );
    }
    for (patterns, admitting) in [
        ("LANG LC_* BASH_ENV", "BASH_ENV"),
        ("*", "*"),
        ("LANG BASH_*", "BASH_*"),
        ("B?SH_ENV", "B?SH_ENV"),
        ("ENV", "ENV"),
        ("SHELLOPTS", "SHELLOPTS"),
        ("BASH_FUNC_*", "BASH_FUNC_*"),
        ("PATH", "PATH"),
        ("LD_*", "LD_*"),
    ] {
        let (rc, out) = run(patterns, &[&ak]);
        assert_eq!(
            rc, 1,
            "AcceptEnv '{patterns}' beside a receiver line is a finding:\n{out}"
        );
        let found: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("deposit-key-audit: UNCONFINED "))
            .collect();
        assert_eq!(found.len(), 1, "'{patterns}': exactly one finding:\n{out}");
        let line = found[0];
        assert!(
            line.starts_with(&format!(
                "deposit-key-audit: UNCONFINED sshd AcceptEnv {admitting} — "
            )) && !line.contains("SHA256:"),
            "'{patterns}': the finding names the pattern and no key:\n{out}"
        );
    }
    // Stable text: a second receiver line changes nothing in the finding,
    // so the item it files is not re-filed when the file grows.
    let one = run("BASH_ENV", &[&ak]).1;
    let mut more = tree_lines(&keys);
    more.push(machine_receiver(&keygen(&case.root, "another", "ed25519")));
    let ak2 = case.file("authorized_keys.more", &more);
    let two = run("BASH_ENV", &[&ak2]).1;
    let finding = |out: &str| {
        out.lines()
            .find(|l| l.starts_with("deposit-key-audit: UNCONFINED sshd AcceptEnv "))
            .map(str::to_string)
    };
    assert!(finding(&one).is_some(), "{one}");
    assert_eq!(finding(&one), finding(&two), "{one}\n{two}");
    // Not handed the option, the audit says so rather than imply a read.
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("AcceptEnv was not read"), "{out}");
    // The control: the owner's login keys alone are his business.
    let login = case.file(
        "authorized_keys.login",
        &[bare(&keys.david, "david@laptop")],
    );
    let (rc, out) = run("*", &[&login]);
    assert_eq!(
        rc, 0,
        "AcceptEnv beside no receiver line is not judged:\n{out}"
    );
}

/// The converge audits what sshd names, not the default pair it used to
/// assume: its file block, run with a stand-in sshd, hands the audit the
/// list `--keys-files-of` printed, and an UNVERIFIED read leaves no file
/// and carries its reason to the verdict.
#[test]
fn the_converge_audits_the_files_sshd_names() {
    let converge = read(&repo_root().join("infra/gcp/boss-gcp-converge.sh"));
    assert!(
        !converge.contains("\"$ak_home/.ssh/authorized_keys\" \"$ak_home/.ssh/authorized_keys2\""),
        "the converge must not assume sshd's default pair of files"
    );
    let start = converge
        .find("ak_files=()")
        .expect("the converge builds ak_files");
    let end = converge[start..].find("alog=").unwrap() + start;
    let case = Case::new("audit-deposit-keys-converge-files");
    let good = sshd_stub(
        &case,
        "sshd-good",
        "acceptenv LANG LC_*\nauthorizedkeysfile /etc/ssh/keys/%u .ssh/authorized_keys\npermituserenvironment no\n",
        0,
    );
    let bad = sshd_stub(
        &case,
        "sshd-bad",
        "sshd: no hostkeys available -- exiting.\n",
        1,
    );
    let runner = case.root.join("files.sh");
    write_exec(
        &runner,
        &format!(
            "#!/bin/bash\nset -uo pipefail\nDEPOSIT_ACCOUNT=root\nOWNER=root\naudit_bound=60\n{}\nprintf 'file=%s\\n' \"${{ak_files[@]}}\"\nprintf 'why=%s\\n' \"${{ak_why:-}}\"\n",
            &converge[start..end]
        ),
    );
    for (stub, want_files) in [(&good, true), (&bad, false)] {
        let out = Command::new("bash")
            .arg(&runner)
            .env_remove("BOSS_GCP_CONVERGE_AUTHORIZED_KEYS")
            .env("BOSS_GCP_CONVERGE_INFRA", repo_root().join("infra"))
            .env("BOSS_DEPOSIT_AUDIT_SSHD", stub)
            .env("SSHD_ARGS", case.root.join("sshd.args"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        if want_files {
            assert!(
                text.contains("file=/etc/ssh/keys/root\n")
                    && text.contains(&format!("file={}/.ssh/authorized_keys\n", root_home())),
                "{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
            // The AcceptEnv sshd read reaches the audit as its own option,
            // ahead of the files (backlog 54104a05).
            assert!(
                text.starts_with("file=--permit-user-environment\nfile=no\nfile=--accept-env\nfile=LANG LC_*\nfile=/"),
                "{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        } else {
            assert!(
                !text.contains("file=/") && text.contains("why=deposit-key-audit: UNVERIFIED"),
                "{text}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    assert!(
        converge.contains("echo \"${ak_why:-"),
        "an empty file list writes the read's own UNVERIFIED line as the verdict"
    );
}

/// The audit told sshd's PermitUserEnvironment for the account, then the
/// files.
fn run_pue(case: &Case, value: &str, files: &[&Path]) -> (i32, String) {
    case.run_args(
        &["--permit-user-environment".to_string(), value.to_string()],
        files,
        &[],
    )
}

/// ~/.ssh/environment IS THE SECOND DOOR OF THE SAME SETTING (backlog
/// f5ff8170; review 6b8e7e51 of car d7cfae5f). With PermitUserEnvironment
/// anything but `no` — `yes` or a pattern list — sshd reads the deposit
/// account's ~/.ssh/environment for EVERY session, forced command and
/// restrict included, so a BASH_ENV line there runs a file before every
/// receiver as environment= does, and the three lines the tree prints
/// read clean. The setting sshd will apply is the finding; `no` is clean
/// and says so, and a set with no root door does not judge it.
#[test]
fn sshd_permitting_user_environment_beside_a_root_door_is_a_finding() {
    let case = Case::new("audit-deposit-keys-permituserenvironment");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    for value in ["yes", "LANG,LC_*"] {
        let (rc, out) = run_pue(&case, value, &[&ak]);
        assert_eq!(
            rc, 1,
            "PermitUserEnvironment {value} beside a receiver line is a finding:\n{out}"
        );
        assert!(
            out.contains("UNCONFINED sshd:PermitUserEnvironment — ")
                && out.contains(&format!("PermitUserEnvironment {value}"))
                && out.contains("~/.ssh/environment"),
            "{value}: the finding names the setting, its value and the file it opens:\n{out}"
        );
        assert_eq!(
            out.lines()
                .filter(|l| l.starts_with("deposit-key-audit: UNCONFINED "))
                .count(),
            1,
            "{value}: one finding, for the setting, and none for the lines:\n{out}"
        );
    }
    let (rc, out) = run_pue(&case, "no", &[&ak]);
    assert_eq!(rc, 0, "PermitUserEnvironment no is clean:\n{out}");
    assert!(
        out.contains("deposit-key-audit: clean") && out.contains("PermitUserEnvironment no"),
        "the clean verdict says the setting was read:\n{out}"
    );
    // Not told the setting, the audit judges the lines as before and its
    // verdict says the setting was not read, rather than implying it was.
    let (rc, out) = case.run(&[&ak]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.contains("PermitUserEnvironment was not read"),
        "a verdict without the setting says so:\n{out}"
    );
    // A set with no root door: the owner's login keys are his business,
    // and so is what his own sessions read; named, not judged.
    let login = Case::new("audit-deposit-keys-permituserenvironment-login");
    let lk = login.keys();
    let ak = login.file("authorized_keys", &[bare(&lk.david, "david@laptop")]);
    let (rc, out) = run_pue(&login, "yes", &[&ak]);
    assert_eq!(rc, 0, "no root door, nothing judged:\n{out}");
    assert!(
        out.contains("PermitUserEnvironment yes") && !out.contains("UNCONFINED"),
        "the verdict names the setting it did not judge:\n{out}"
    );
}

/// A --permit-user-environment the audit cannot read one value from is
/// UNVERIFIED, never a setting it guessed.
#[test]
fn a_permit_user_environment_without_one_value_is_unverified() {
    let case = Case::new("audit-deposit-keys-permituserenvironment-bad");
    let keys = case.keys();
    let ak = case.file("authorized_keys", &tree_lines(&keys));
    let akp = ak.to_str().unwrap().to_string();
    for (name, args) in [
        ("no-value", vec!["--permit-user-environment"]),
        ("empty", vec!["--permit-user-environment", "", akp.as_str()]),
        (
            "twice",
            vec![
                "--permit-user-environment",
                "no",
                "--permit-user-environment",
                "yes",
                akp.as_str(),
            ],
        ),
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, 2, "{name}:\n{out}");
        assert!(
            out.contains("UNVERIFIED") && !out.contains("deposit-key-audit: clean"),
            "{name}:\n{out}"
        );
    }
}

/// THE SETTING RIDES TO THE AUDIT WITH THE FILES. The converge hands the
/// audit whatever `--keys-files-of` printed, one argument per line, so
/// sshd's PermitUserEnvironment, read from the same `sshd -T -C
/// user=<account>`, arrives ahead of the files with no second read: an
/// sshd that permits it makes the tree's own lines a finding end to end.
#[test]
fn the_setting_sshd_reads_rides_to_the_audit_ahead_of_the_files() {
    let case = Case::new("audit-deposit-keys-permituserenvironment-e2e");
    let keys = case.keys();
    let ak = case.file("deposit_keys", &tree_lines(&keys));
    for (value, want) in [("yes", 1), ("no", 0)] {
        let stub = sshd_stub(
            &case,
            &format!("sshd-{value}"),
            &format!(
                "authorizedkeysfile {}\npermituserenvironment {value}\n",
                ak.display()
            ),
            0,
        );
        let (rc, list, err) = keys_files_of(&case, "root", &stub);
        assert_eq!(rc, 0, "{list}{err}");
        let args: Vec<String> = list.lines().map(str::to_string).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, want, "PermitUserEnvironment {value}:\n{out}");
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

#[test]
fn usage_refusals_name_all_supported_options() {
    let case = Case::new("audit-deposit-keys-complete-usage");
    for args in [
        vec![],
        vec!["/absent/authorized_keys", "--accept-env", "LANG"],
        vec!["/absent/authorized_keys", "relative_keys"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let (rc, out) = case.run_args(&args, &[], &[]);
        assert_eq!(rc, 2, "{out}");
        assert!(out.contains("UNVERIFIED"), "{out}");
        assert!(
            out.contains("[--prepared-key <public key>]... [--permit-user-environment <value>] [--accept-env <patterns>] <file>..."),
            "complete usage missing: {out}"
        );
    }
}
