//! tree-wide pin — the full-output opt-in roster reads every chore manifest,
//! so scoped gates must judge changes outside this crate.
//!
//! The break-glass kubeconfig reaches a configured receiver from a cluster CronJob,
//! through a forced-command receiver, and only a credential that can do
//! exactly what it is declared to do is ever deposited (backlog
//! 7336cb5f; transport and verify-the-negative decided on design
//! 835c0c9c, the non-root uid on design b08725c2 Q3).
//!
//! Three scripts and the files that must agree with them:
//! * `infra/cluster/break-glass-deposit.sh` — the Job. Driven here with
//!   `kubectl`, `ssh` and `ssh-keygen` stubs on PATH; the `ssh` stub
//!   execs the REAL receiver, so the pair is exercised end to end over
//!   the one wire they share (stdin).
//! * `infra/gcp/ops-credential-recv.sh` — the receiver: refuses an
//!   empty, foreign, tokenless or command-carrying deposit and never
//!   touches the file it would have replaced.
//! * `infra/gcp/install-ops-credential-receiver.sh` — the door on
//!   boss-gcp: a copy outside the checkout and one sudoers rule that
//!   names the argument.
//!
//! What no test here can reach is OpenSSH's own key-mode check against a
//! kubelet-projected Secret, because the gate has no second uid and no
//! CAP_CHOWN to build one. That was rehearsed with a real sshd on the
//! dev pod before this car was gated, and the rehearsal is in the car's
//! report: a root-owned 0440 key read through a supplementary group was
//! accepted; the same key owned by the client was refused as
//! "UNPROTECTED PRIVATE KEY FILE".

use boss_testing::rbac::{Node, Value as Yaml, read_stream};
use boss_testing::{create_dir, feed_stdin, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const JOB: &str = "infra/cluster/break-glass-deposit.sh";
const RECEIVER: &str = "infra/gcp/ops-credential-recv.sh";
const INSTALLER: &str = "infra/gcp/install-ops-credential-receiver.sh";
const MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-deposit.yaml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const POLICY_MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-operator.yaml";

fn yaml_json(node: &Node) -> serde_json::Value {
    match &node.value {
        Yaml::Str(value) => serde_json::Value::String(value.clone()),
        Yaml::Seq(values) => values.iter().map(yaml_json).collect(),
        Yaml::Map(values) => values
            .iter()
            .map(|(key, value)| (key.clone(), yaml_json(value)))
            .collect(),
        Yaml::Text => serde_json::Value::String("<block scalar>".into()),
        Yaml::Null => serde_json::Value::Null,
    }
}

fn manifest_object(path: &str, kind: &str, name: &str) -> serde_json::Value {
    let objects = read_stream(path, &tree(path)).expect("manifest parses");
    let object = objects
        .iter()
        .find(|object| object.kind == kind && object.name == name)
        .expect("manifest object");
    yaml_json(&object.root)
}

const TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.TOKENSENTINELqwertyuiop.sigSENTINEL";
const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nCA-SENTINEL-BYTES\n-----END CERTIFICATE-----\n";
const MOUNTED_PRIVATE: &str = "PRIVATE-KEY-SENTINEL-MOUNTED\n";
const MOUNTED_PUBLIC: &str = "ssh-ed25519 AAAAMOUNTEDPUBLIC break-glass-deposit@boss\n";
const MINTED_PRIVATE: &str = "PRIVATE-KEY-SENTINEL-MINTED\n";
const SERVER: &str = "https://api.example.invalid:6443";
const TARGET: &str = "recovery@receiver.example.invalid";

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn tree(rel: &str) -> String {
    read(&repo_root().join(rel))
}

fn b64(s: &str) -> String {
    let mut child = Command::new("base64")
        .arg("-w0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn base64");
    feed_stdin(&mut child, s.as_bytes());
    let out = child.wait_with_output().expect("base64 output");
    assert!(out.status.success(), "base64 failed");
    String::from_utf8(out.stdout).expect("ascii")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .unwrap_or_else(|e| panic!("stat {}: {e}", p.display()))
        .permissions()
        .mode()
        & 0o777
}

/// A kubeconfig in exactly the shape the Job writes.
fn kubeconfig(token: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Config\nclusters:\n  - name: break-glass\n    cluster:\n      server: {SERVER}\n      certificate-authority-data: {ca}\nusers:\n  - name: break-glass-operator\n    user:\n      token: {token}\ncontexts:\n  - name: break-glass\n    context:\n      cluster: break-glass\n      user: break-glass-operator\n      namespace: boss\ncurrent-context: break-glass\n",
        ca = b64(CA_PEM)
    )
}

// ---------------------------------------------------------------------------
// The receiver
// ---------------------------------------------------------------------------

fn receive(ops: &Path, args: &[&str], input: &[u8]) -> Output {
    receive_in_locale(ops, args, input, "C")
}

/// The receiver under a caller-chosen locale: sudo keeps `LC_*` and
/// Debian's sshd accepts them from the client (review b6d2a716, N6), so
/// the locale the receiver judges in is the depositor's unless it pins
/// its own.
fn receive_in_locale(ops: &Path, args: &[&str], input: &[u8], locale: &str) -> Output {
    let mut child = Command::new("bash")
        .arg(repo_root().join(RECEIVER))
        .args(args)
        .env("BOSS_OPS_DIR", ops)
        .env("BOSS_RECV_OWNER", "")
        .env("LC_ALL", locale)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the receiver");
    feed_stdin(&mut child, input);
    child.wait_with_output().expect("receiver output")
}

#[test]
fn a_kubeconfig_deposit_is_installed_0600_and_reported_by_bytes_never_by_value() {
    let dir = scratch_dir("bg-recv-ok");
    let ops = dir.join("ops");
    let kc = kubeconfig(TOKEN);
    let out = receive(&ops, &["kubeconfig"], kc.as_bytes());
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    let dest = ops.join("kubeconfig");
    assert_eq!(read(&dest), kc, "the file is not the bytes deposited");
    assert_eq!(mode(&dest), 0o600, "{all}");
    assert_eq!(
        mode(&ops),
        0o700,
        "a directory the receiver creates is root's alone"
    );
    assert!(
        all.contains(&format!("received {} bytes; installed at", kc.len())),
        "the receipt must carry the byte count the Job reads back: {all}"
    );
    assert!(
        !all.contains(TOKEN) && !all.contains(&b64(CA_PEM)),
        "a value leaked: {all}"
    );
    let staged: Vec<_> = std::fs::read_dir(&ops)
        .expect("read ops")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        staged.len(),
        1,
        "a staged file was left beside the credential: {staged:?}"
    );

    let again = receive(&ops, &["kubeconfig"], kc.as_bytes());
    let all = text(&again);
    assert!(again.status.success(), "{all}");
    assert!(
        all.contains("unchanged"),
        "an identical deposit is reported unchanged: {all}"
    );
}

#[test]
fn a_deposit_that_is_empty_foreign_tokenless_or_carries_a_command_never_replaces_the_file() {
    let good = kubeconfig(TOKEN);
    let exec_plugin = good.replace(
        &format!("      token: {TOKEN}\n"),
        &format!("      token: {TOKEN}\n      exec:\n        command: sh\n"),
    );
    let auth_provider = good.replace(
        "    user:\n",
        "    user:\n      auth-provider:\n        name: gcp\n",
    );
    let token_file = good.replace(
        &format!("      token: {TOKEN}\n"),
        &format!("      token: {TOKEN}\n      tokenFile: /etc/shadow\n"),
    );
    let two_tokens = good.replace(
        &format!("      token: {TOKEN}\n"),
        &format!("      token: {TOKEN}\n      token: {TOKEN}\n"),
    );
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("empty", Vec::new(), "empty deposit"),
        (
            "not yaml",
            b"hello, this is not a kubeconfig\n".to_vec(),
            "not one key",
        ),
        (
            "tokenless",
            good.replace(&format!("      token: {TOKEN}\n"), "")
                .into_bytes(),
            "TOKENLESS",
        ),
        (
            "short token",
            good.replace(TOKEN, "abc").into_bytes(),
            "too short",
        ),
        ("exec plugin", exec_plugin.into_bytes(), "'exec'"),
        (
            "auth provider",
            auth_provider.into_bytes(),
            "'auth-provider'",
        ),
        ("token file", token_file.into_bytes(), "'tokenFile'"),
        ("two tokens", two_tokens.into_bytes(), "2 tokens"),
        (
            "plain http",
            good.replace("https://", "http://").into_bytes(),
            "not an https",
        ),
        (
            "flow style",
            good.replace("kind: Config", "kind: {Config: 1}")
                .into_bytes(),
            "not one plain scalar",
        ),
        (
            "carriage returns",
            good.replace('\n', "\r\n").into_bytes(),
            "NUL or CR",
        ),
        ("oversize", vec![b'a'; 70_000], "over 65536"),
        (
            "no kind",
            good.replace("kind: Config\n", "").into_bytes(),
            "'kind: Config'",
        ),
    ];
    for (name, input, why) in cases {
        let dir = scratch_dir("bg-recv-refused");
        let ops = dir.join("ops");
        create_dir(&ops);
        let dest = ops.join("kubeconfig");
        write_file(&dest, "THE WORKING CREDENTIAL\n");
        let out = receive(&ops, &["kubeconfig"], &input);
        let all = text(&out);
        assert_eq!(out.status.code(), Some(65), "{name}: {all}");
        assert!(
            all.contains(why),
            "{name}: the refusal must say why ({why}): {all}"
        );
        assert!(all.contains("left as it was"), "{name}: {all}");
        assert_eq!(
            read(&dest),
            "THE WORKING CREDENTIAL\n",
            "{name}: the file was touched"
        );
        assert!(!all.contains(TOKEN), "{name}: a value leaked: {all}");
        assert_eq!(
            std::fs::read_dir(&ops).expect("read ops").count(),
            1,
            "{name}: a staged file was left behind"
        );
    }
}

#[test]
fn a_blank_line_is_spaces_alone_in_any_locale_the_depositor_sends() {
    // Review b6d2a716 of 7336cb5f, N6 (measured there): with
    // LC_ALL=C.UTF-8 a line of only U+2028 counted as blank, and lines of
    // only a tab, \f or \v were blank in any locale — each accepted,
    // though YAML reads none of them as nothing. The Job never writes a
    // blank line at all, so blank is spaces and nothing else.
    let good = kubeconfig(TOKEN);
    for (name, odd) in [
        ("a tab", "\t"),
        ("a form feed", "\x0c"),
        ("a vertical tab", "\x0b"),
        ("U+2028", "\u{2028}"),
    ] {
        for locale in ["C", "C.UTF-8"] {
            let dir = scratch_dir("bg-recv-blank");
            let ops = dir.join("ops");
            create_dir(&ops);
            let input = good.replacen("users:\n", &format!("{odd}\nusers:\n"), 1);
            let out = receive_in_locale(&ops, &["kubeconfig"], input.as_bytes(), locale);
            let all = text(&out);
            assert_eq!(
                out.status.code(),
                Some(65),
                "a line of only {name} under LC_ALL={locale} must be refused: {all}"
            );
            assert!(
                !ops.join("kubeconfig").exists(),
                "{name}/{locale}: installed anyway"
            );
        }
    }
    // A line of spaces alone is still blank, and the good shape still
    // lands under a UTF-8 locale.
    let dir = scratch_dir("bg-recv-spaces");
    let ops = dir.join("ops");
    create_dir(&ops);
    let input = good.replacen("users:\n", "   \nusers:\n", 1);
    let out = receive_in_locale(&ops, &["kubeconfig"], input.as_bytes(), "C.UTF-8");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let receiver = tree(RECEIVER);
    assert!(
        receiver.lines().any(|l| l.trim() == "export LC_ALL=C"),
        "the receiver pins its own locale rather than judge in the depositor's"
    );
}

#[test]
fn the_receiver_takes_the_kubeconfig_and_no_other_name() {
    for args in [vec![], vec!["talosconfig"], vec!["kubeconfig", "extra"]] {
        let dir = scratch_dir("bg-recv-usage");
        let ops = dir.join("ops");
        let out = receive(&ops, &args, kubeconfig(TOKEN).as_bytes());
        assert_eq!(out.status.code(), Some(78), "{args:?}: {}", text(&out));
        assert!(
            !ops.join("kubeconfig").exists(),
            "{args:?}: something was written"
        );
    }
}

// ---------------------------------------------------------------------------
// The door on boss-gcp
// ---------------------------------------------------------------------------

fn install(dir: &Path, visudo_ok: bool, user: &str) -> Output {
    let bin = dir.join("bin");
    create_dir(&bin);
    create_dir(&dir.join("sudoers.d"));
    write_exec(
        &bin.join("visudo"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >>{d}/visudo-calls\ncp \"$2\" {d}/checked\nexit {rc}\n",
            d = dir.display(),
            rc = if visudo_ok { 0 } else { 1 }
        ),
    );
    Command::new("bash")
        .arg(repo_root().join(INSTALLER))
        .env("INSTALL_RECV_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_RECV_SUDOERS_DIR", dir.join("sudoers.d"))
        .env("INSTALL_VISUDO", bin.join("visudo"))
        .env("INSTALL_RECV_USER", user)
        .env("INSTALL_RECV_OWNER", "")
        .output()
        .expect("run the installer")
}

/// The FORCED line the Job prints, read from the Job script itself.
fn job_forced_line() -> String {
    let job = tree(JOB);
    let line = job
        .lines()
        .find(|l| l.starts_with("FORCED='"))
        .expect("the Job declares FORCED='…'");
    line.trim_start_matches("FORCED='")
        .trim_end_matches('\'')
        .to_string()
}

#[test]
fn the_installer_places_a_copy_and_one_checked_rule_that_names_the_argument() {
    let dir = scratch_dir("bg-recv-install");
    let out = install(&dir, true, "david");
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{}", text(&out));

    let copy = dir.join("libexec/ops-credential-recv");
    assert_eq!(
        read(&copy),
        tree(RECEIVER),
        "the installed receiver is not the tree's"
    );
    assert_eq!(mode(&copy), 0o755);

    let rule = dir.join("sudoers.d/boss-ops-credential-recv");
    let body = read(&rule);
    let lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
    let r = "/usr/local/libexec/boss/ops-credential-recv";
    assert_eq!(
        lines,
        vec![
            format!("Defaults!{r} env_reset").as_str(),
            format!(
                "Defaults!{r} secure_path=\"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\""
            )
            .as_str(),
            format!("david ALL=(root) NOPASSWD: {r} kubeconfig").as_str(),
        ],
        "the sudoers rule grants something other than the receiver for the kubeconfig:\n{body}"
    );
    assert_eq!(mode(&rule), 0o440);
    assert!(
        read(&dir.join("visudo-calls")).starts_with("-cf "),
        "not checked with visudo -cf"
    );
    assert_eq!(
        read(&dir.join("checked")),
        body,
        "visudo checked something other than what was placed"
    );

    // ONE forced-command line, printed by two scripts (CLAUDE.md §9a):
    // the installer's for the placer, the Job's with the key filled in.
    let printed = so
        .lines()
        .find(|l| l.trim_start().starts_with("command="))
        .unwrap_or_else(|| panic!("the installer prints no forced-command line:\n{so}"))
        .trim()
        .trim_end_matches(" <the deposit key's public half>")
        .to_string();
    assert_eq!(
        printed,
        job_forced_line(),
        "the installer and the Job print different authorized_keys lines"
    );
}

#[test]
fn a_rule_visudo_refuses_is_never_placed_and_root_is_never_the_grantee() {
    let dir = scratch_dir("bg-recv-install-refused");
    let out = install(&dir, false, "david");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!dir.join("sudoers.d/boss-ops-credential-recv").exists());
    assert_eq!(
        std::fs::read_dir(dir.join("sudoers.d"))
            .expect("dir")
            .count(),
        0,
        "a staged rule was left"
    );

    let dir = scratch_dir("bg-recv-install-root");
    let out = install(&dir, true, "root");
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(!dir.join("sudoers.d/boss-ops-credential-recv").exists());
}

// ---------------------------------------------------------------------------
// The Job
// ---------------------------------------------------------------------------

const KUBECTL_STUB: &str = r#"#!/usr/bin/env bash
D=@D@
{ printf 'kubectl'; printf ' %q' "$@"; echo; } >> "$D/calls"
args=" $* "
for a in "$@"; do case "$a" in --kubeconfig=*) cp "${a#--kubeconfig=}" "$D/verified-with" ;; esac; done
case "$args" in
  *" auth can-i "*)
    rest="${args#* can-i }"
    set -- $rest
    f="$D/cani-$1-$2"
    if [ -f "$f" ]; then cat "$f"; [ "$(cat "$f")" = yes ]; exit; fi
    echo "Unable to connect to the server: tls: failed to verify certificate" >&2
    exit 1 ;;
  *" patch deployment "*)
    case "$args" in
      *'/initContainers/0/image"'*) probe=roll ;;
      *'/containers/0/command"'*) probe=command ;;
      *'/containers/0/image"'*) probe=image ;;
      *'/metadata/ownerReferences"'*) probe=owner ;;
      *) echo "stub kubectl: an unknown probe patch: $*" >&2; exit 2 ;;
    esac
    { echo "$probe"; } >> "$D/probes"
    if [ "$probe" = roll ]; then
      prev=""
      for a in "$@"; do [ "$prev" = -p ] && printf '%s' "$a" > "$D/roll.patch"; prev="$a"; done
    fi
    case "$*" in *--dry-run=server*) ;; *) echo "stub kubectl: a probe patch WITHOUT --dry-run=server" >&2; exit 2 ;; esac
    case "$(cat "$D/patch-$probe" 2>/dev/null || cat "$D/patch-probe")" in
      accept) echo 'deployment.apps/boss patched (server dry run)'; exit 0 ;;
      warn)
        echo "Warning: Validation failed for ValidatingAdmissionPolicy 'break-glass-operator-image-only' with binding 'break-glass-operator-image-only': break-glass-operator may change only an image" >&2
        echo 'deployment.apps/boss patched (server dry run)'; exit 0 ;;
      forbidden) echo 'Error from server (Forbidden): deployments.apps "boss" is forbidden: User "system:serviceaccount:boss:break-glass-operator" cannot patch resource "deployments"' >&2; exit 1 ;;
      denied) echo "The deployments \"boss\" is invalid: : ValidatingAdmissionPolicy 'break-glass-operator-image-only' with binding 'break-glass-operator-image-only' denied request: $(cat "$D/message-$probe" 2>/dev/null || echo 'failed to evaluate validation: no such key')" >&2; exit 1 ;;
      *) echo 'Error from server (NotFound): deployments.apps "boss" not found' >&2; exit 1 ;;
    esac ;;
  *" get deployment "*)
    cat "$D/deployment.json"; exit 0 ;;
  *" get validatingadmissionpolicybinding "*)
    case "$args" in *--kubeconfig=*) echo "stub kubectl: the policy's phase read through the credential under test" >&2; exit 2 ;; esac
    if [ -f "$D/binding.json" ]; then cat "$D/binding.json"; exit 0; fi
    if [ -f "$D/binding-absent" ]; then
      echo 'Error from server (NotFound): validatingadmissionpolicybindings.admissionregistration.k8s.io "break-glass-operator-image-only" not found' >&2; exit 1
    fi
    echo 'Error from server (Forbidden): validatingadmissionpolicybindings.admissionregistration.k8s.io "break-glass-operator-image-only" is forbidden: User "system:serviceaccount:boss:break-glass-deposit" cannot get resource' >&2
    exit 1 ;;
  *" get secret "*)
    rest="${args#* get secret }"
    name="${rest%% *}"
    if [ -f "$D/secret-$name.json" ]; then cat "$D/secret-$name.json"; exit 0; fi
    echo "Error from server (NotFound): secrets \"$name\" not found" >&2
    exit 1 ;;
  *" patch secret "*)
    prev=""; pf=""
    for a in "$@"; do [ "$prev" = --patch-file ] && pf="$a"; prev="$a"; done
    if [ -f "$D/patch-fails" ]; then echo 'Error from server (Conflict): the object has been modified' >&2; exit 1; fi
    cp "$pf" "$D/patched"
    echo "secret/break-glass-deposit-key patched"
    exit 0 ;;
esac
echo "stub kubectl: unexpected: $*" >&2
exit 2
"#;

const SSH_STUB: &str = r#"#!/usr/bin/env bash
D=@D@
{ printf 'ssh'; printf ' %q' "$@"; echo; } >> "$D/calls"
case "$(cat "$D/ssh-mode" 2>/dev/null || echo real)" in
  denied) cat >/dev/null; echo 'recovery@receiver.example.invalid: Permission denied (publickey).' >&2; exit 255 ;;
  short) cat >/dev/null; echo 'ops-credential-recv: kubeconfig: received 3 bytes; installed at /etc/boss-ops/kubeconfig, root:root 0600 (was absent)'; exit 0 ;;
esac
BOSS_OPS_DIR="$D/ops" BOSS_RECV_OWNER= exec bash @REPO@/infra/gcp/ops-credential-recv.sh kubeconfig
"#;

const KEYGEN_STUB: &str = r#"#!/usr/bin/env bash
D=@D@
{ printf 'ssh-keygen'; printf ' %q' "$@"; echo; } >> "$D/calls"
if [ "$1" = -l ]; then echo '256 SHA256:HOSTKEYFINGERPRINTFIXTURE receiver.example.invalid (ED25519)'; exit 0; fi
prev=""; f=""
for a in "$@"; do [ "$prev" = -f ] && f="$a"; prev="$a"; done
printf 'PRIVATE-KEY-SENTINEL-MINTED\n' > "$f"
printf 'ssh-ed25519 AAAAMINTEDPUBLIC break-glass-deposit@boss\n' > "$f.pub"
"#;

struct Job {
    dir: PathBuf,
}

impl Job {
    /// A case whose every answer is the declared one: a mounted key, a
    /// filled token Secret, can-i yes/no/no/no, and a command patch the
    /// admission policy DENIES.
    fn new(name: &str) -> Job {
        let dir = scratch_dir(&format!("bg-deposit-{name}"));
        let bin = dir.join("bin");
        create_dir(&bin);
        let d = dir.display().to_string();
        let repo = repo_root().display().to_string();
        for (tool, body) in [
            ("kubectl", KUBECTL_STUB),
            ("ssh", SSH_STUB),
            ("ssh-keygen", KEYGEN_STUB),
        ] {
            write_exec(
                &bin.join(tool),
                &body.replace("@D@", &d).replace("@REPO@", &repo),
            );
        }
        create_dir(&dir.join("keys"));
        write_file(&dir.join("keys/id_ed25519"), MOUNTED_PRIVATE);
        write_file(&dir.join("keys/id_ed25519.pub"), MOUNTED_PUBLIC);
        write_file(
            &dir.join("known_hosts"),
            "receiver.example.invalid ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHOSTKEYFIXTURE\n",
        );
        create_dir(&dir.join("tmp"));
        let job = Job { dir };
        job.token_secret(TOKEN);
        job.set("cani-get-nodes", "yes");
        job.set("cani-get-secrets", "no");
        job.set("cani-delete-pods", "no");
        job.set("cani-create-deployments", "no");
        job.set("patch-probe", "denied");
        job.set("patch-roll", "accept");
        job.set(
            "deployment.json",
            &manifest_object("infra/cluster/manifests/boss.yaml", "Deployment", "boss").to_string(),
        );
        let policy = manifest_object(
            POLICY_MANIFEST,
            "ValidatingAdmissionPolicy",
            "break-glass-operator-image-only",
        );
        for (probe, fragment) in [
            ("command", "command, args, env"),
            ("image", "foreign image"),
            ("owner", "ownerReferences"),
        ] {
            let message = policy["spec"]["validations"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|rule| rule["message"].as_str())
                .find(|message| message.contains(fragment))
                .unwrap();
            job.set(&format!("message-{probe}"), message);
        }
        job
    }

    fn set(&self, file: &str, body: &str) {
        write_file(&self.dir.join(file), body);
    }

    /// The live ValidatingAdmissionPolicyBinding, as `kubectl get -o
    /// json` returns it, with these validationActions.
    fn binding(&self, actions: &[&str]) {
        self.binding_for("break-glass-operator-image-only", actions);
    }

    /// The same binding, naming this policy.
    fn binding_for(&self, policy: &str, actions: &[&str]) {
        self.set(
            "binding.json",
            &serde_json::json!({
                "apiVersion": "admissionregistration.k8s.io/v1",
                "kind": "ValidatingAdmissionPolicyBinding",
                "metadata": {"name": "break-glass-operator-image-only"},
                "spec": {
                    "policyName": policy,
                    "validationActions": actions,
                },
            })
            .to_string(),
        );
    }

    /// The Secret with no key yet, and a pod that mounted it empty: the
    /// first run's shape.
    fn first_run(&self) {
        self.unmount_key();
        self.set(
            "secret-break-glass-deposit-key.json",
            "{\"metadata\":{\"name\":\"break-glass-deposit-key\",\"resourceVersion\":\"42\"}}",
        );
    }

    /// Every `RED <route> <kind>: <text>` line, as
    /// maintenance.chore.file_reds parses them: (route, kind, text).
    fn reds(out: &str) -> Vec<(String, String, String)> {
        out.lines()
            .filter_map(|l| {
                let rest = l.trim().strip_prefix("RED ")?;
                let (route, rest) = rest.split_once(' ')?;
                let (kind, text) = rest.split_once(':')?;
                (!kind.contains(char::is_whitespace))
                    .then(|| (route.to_string(), kind.to_string(), text.trim().to_string()))
            })
            .collect()
    }

    fn token_secret(&self, token: &str) {
        self.set(
            "secret-break-glass-operator-token.json",
            &format!(
                "{{\"metadata\":{{\"resourceVersion\":\"7\"}},\"data\":{{\"token\":\"{}\",\"ca.crt\":\"{}\",\"namespace\":\"{}\"}}}}",
                b64(token),
                b64(CA_PEM),
                b64("boss")
            ),
        );
    }

    fn unmount_key(&self) {
        std::fs::remove_file(self.dir.join("keys/id_ed25519")).expect("rm key");
        std::fs::remove_file(self.dir.join("keys/id_ed25519.pub")).expect("rm pub");
    }

    fn run_with(&self, extra: &[(&str, &str)]) -> (i32, String) {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(JOB))
            .env("PATH", path)
            .env("TMPDIR", self.dir.join("tmp"))
            .env("BOSS_DEPOSIT_KEY_DIR", self.dir.join("keys"))
            .env("BOSS_DEPOSIT_KNOWN_HOSTS", self.dir.join("known_hosts"))
            .env("BOSS_DEPOSIT_API_SERVER", SERVER)
            .env("BOSS_DEPOSIT_TARGET", TARGET);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run the Job script");
        (out.status.code().unwrap_or(-1), text(&out))
    }

    fn run(&self) -> (i32, String) {
        self.run_with(&[])
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }

    fn called_ssh(&self) -> bool {
        self.calls().lines().any(|l| l.starts_with("ssh "))
    }

    fn deposited(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("ops/kubeconfig")).ok()
    }

    /// No value reached the record or any child's argv, and the Job's
    /// scratch is gone.
    fn assert_nothing_leaked(&self, out: &str) {
        for v in [
            TOKEN.to_string(),
            b64(CA_PEM),
            MOUNTED_PRIVATE.trim().to_string(),
            MINTED_PRIVATE.trim().to_string(),
        ] {
            assert!(!out.contains(&v), "a value reached the output: {out}");
            assert!(
                !self.calls().contains(&v),
                "a value reached an argv:\n{}",
                self.calls()
            );
        }
        assert_eq!(
            std::fs::read_dir(self.dir.join("tmp"))
                .expect("tmp")
                .count(),
            0,
            "the Job left its scratch (a kubeconfig with a token) behind"
        );
    }
}

#[test]
fn a_credential_that_can_roll_only_images_is_deposited_and_the_receipt_is_read_back() {
    let j = Job::new("ok");
    let (rc, out) = j.run();
    assert_eq!(rc, 0, "{out}");
    let got = j
        .deposited()
        .unwrap_or_else(|| panic!("nothing reached the receiver:\n{out}"));
    assert_eq!(
        got,
        kubeconfig(TOKEN),
        "the receiver holds something other than the assembled kubeconfig"
    );
    assert_eq!(
        read(&j.dir.join("verified-with")),
        got,
        "the file verified by effect must be the file deposited"
    );
    assert!(
        out.contains(&format!(
            "deposited: {TARGET} received the {} bytes sent",
            got.len()
        )),
        "{out}"
    );
    assert!(out.contains("can-i get nodes: yes (wanted yes)"), "{out}");
    assert!(out.contains("refused (wanted refused)"), "{out}");
    let calls = j.calls();
    let ssh = calls
        .lines()
        .find(|l| l.starts_with("ssh "))
        .expect("an ssh call");
    for opt in [
        "-T",
        "StrictHostKeyChecking=yes",
        &format!("UserKnownHostsFile={}", j.dir.join("known_hosts").display()),
        "GlobalKnownHostsFile=/dev/null",
        "IdentitiesOnly=yes",
        "BatchMode=yes",
        &format!("-i {}", j.dir.join("keys/id_ed25519").display()),
    ] {
        assert!(ssh.contains(opt), "ssh without {opt}: {ssh}");
    }
    assert!(ssh.ends_with(TARGET), "{ssh}");
    // The stub refuses any probe patch sent without --dry-run=server, so
    // reaching the deposit proves all three were dry runs.
    assert_eq!(
        read(&j.dir.join("probes")).lines().collect::<Vec<_>>(),
        vec!["roll", "command", "image", "owner"],
        "the allowed roll and three must-not writes are each probed"
    );
    assert!(
        !calls.contains("ssh-keygen"),
        "a mounted key must never be re-minted:\n{calls}"
    );
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_policy_bound_to_deny_that_still_accepts_every_patch_is_never_deposited() {
    // The binding says Deny and the patches land anyway: the policy does
    // not match what it was written for, which is a refusal and not a
    // wait — nothing about it changes on its own.
    let j = Job::new("patch-accepted");
    j.set("patch-probe", "accept");
    j.binding(&["Deny"]);
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("validationActions: Deny"),
        "the refusal names the phase it read: {out}"
    );
    for named in [
        "a pod-template command patch was accepted",
        "a foreign image patch was accepted",
        "a metadata.ownerReferences patch was accepted",
    ] {
        assert!(
            out.contains(named),
            "the refusal must name each accepted write ({named}): {out}"
        );
    }
    assert!(
        out.contains("4e1c33b4") && out.contains("NOT deposited"),
        "{out}"
    );
    assert!(
        !j.called_ssh(),
        "a refused credential reached ssh:\n{}",
        j.calls()
    );
    assert!(j.deposited().is_none());
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_policy_that_denies_the_command_but_allows_a_foreign_image_or_an_owner_is_not_enough() {
    // Review 89c716e0 of 4e1c33b4: B1 (any image is itself a credential
    // read) and N1 (an owner the garbage collector deletes for).
    for (probe, named) in [
        ("patch-image", "a foreign image patch was accepted"),
        (
            "patch-owner",
            "a metadata.ownerReferences patch was accepted",
        ),
    ] {
        let j = Job::new("one-accepted");
        j.set(probe, "accept");
        j.binding(&["Deny"]);
        let (rc, out) = j.run();
        assert_eq!(rc, 1, "{probe}: {out}");
        assert!(out.contains(named), "{probe}: {out}");
        assert!(
            !out.contains("a pod-template command patch was accepted"),
            "{probe}: {out}"
        );
        assert!(
            !j.called_ssh() && j.deposited().is_none(),
            "{probe}: deposited anyway"
        );
        j.assert_nothing_leaked(&out);
    }
}

// ---------------------------------------------------------------------------
// After Deny: an accepted patch is a refusal, whatever the binding says
// (backlog e4a9a9b3, the Deny car; review 0d3019f0 F1 and F2). While
// row B's binding reported, [Warn, Audit], an accepted patch read `not
// yet` and exited 75 (backlog 17a7bd18). Once it denies, a binding
// DELETED — the car's own named misfire rollback — or moved back to
// Warn or Audit lets the same patches land while boss-gcp may already
// hold a kubeconfig that can make them, and a quiet not-yet there is
// silence about a widened credential.
// ---------------------------------------------------------------------------

/// The one RED line a run printed, which must be the deposit's own
/// refusal; its text.
fn the_refusal(out: &str) -> String {
    let reds = Job::reds(out);
    assert_eq!(reds.len(), 1, "one RED line: {out}");
    assert_eq!(
        (reds[0].0.as_str(), reds[0].1.as_str()),
        ("break-glass-deposit", "refused"),
        "{out}"
    );
    reds[0].2.clone()
}

#[test]
fn a_binding_moved_off_deny_is_red_and_names_the_binding_and_its_actions() {
    for (actions, answer) in [
        (&["Warn", "Audit"][..], "warn"),
        (&["Audit"][..], "accept"),
        (&["Warn"][..], "warn"),
    ] {
        let j = Job::new("off-deny");
        j.set("patch-probe", answer);
        j.binding(actions);
        let (rc, out) = j.run();
        assert_eq!(rc, 1, "{actions:?}: moved off Deny is a refusal: {out}");
        let red = the_refusal(&out);
        assert!(
            red.contains(&format!(
                "binding break-glass-operator-image-only does not deny (validationActions: {})",
                actions.join(",")
            )),
            "{actions:?}: the RED line names the binding and what it reads: {red}"
        );
        assert!(
            red.contains("a pod-template command patch was accepted")
                && red.contains("boss-gcp already holds"),
            "{actions:?}: {red}"
        );
        assert!(!out.contains("not yet"), "{actions:?}: {out}");
        assert!(
            !j.called_ssh() && j.deposited().is_none(),
            "{actions:?}: a credential the policy does not refuse for was shipped"
        );
        // Read with the Job's own account: the stub refuses a read
        // through the credential under test.
        assert!(
            j.calls()
                .contains("get validatingadmissionpolicybinding break-glass-operator-image-only"),
            "{}",
            j.calls()
        );
        j.assert_nothing_leaked(&out);
    }
}

#[test]
fn a_deleted_binding_is_red_and_says_it_was_deleted() {
    let j = Job::new("deleted");
    j.set("patch-probe", "accept");
    j.set("binding-absent", "");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "a deleted binding is a refusal, never a wait: {out}");
    let red = the_refusal(&out);
    assert!(
        red.contains("binding break-glass-operator-image-only is absent")
            && red.contains("misfire rollback"),
        "{red}"
    );
    assert!(
        !out.contains("not bound yet") && !out.contains("not yet"),
        "the stale 4e1c33b4 wait is gone: {out}"
    );
    assert!(!j.called_ssh() && j.deposited().is_none());
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_binding_that_binds_another_policy_is_named_in_the_refusal() {
    let j = Job::new("other-policy");
    j.set("patch-probe", "accept");
    j.binding_for("some-other-policy", &["Deny", "Audit"]);
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    let red = the_refusal(&out);
    assert!(
        red.contains("it binds policy some-other-policy, not break-glass-operator-image-only"),
        "{red}"
    );
}

#[test]
fn a_binding_that_cannot_be_read_is_named_in_the_refusal() {
    let j = Job::new("binding-unread");
    j.set("patch-probe", "warn");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    let red = the_refusal(&out);
    assert!(
        red.contains("binding break-glass-operator-image-only could not be read"),
        "{red}"
    );
    assert!(!j.called_ssh());
}

#[test]
fn an_accepted_patch_and_another_wrong_answer_ride_one_red_line() {
    for (file, answer, named) in [
        (
            "cani-get-secrets",
            "yes",
            "can-i get secrets -n boss answered yes",
        ),
        (
            "patch-image",
            "notfound",
            "the foreign image patch probe unmeasured",
        ),
    ] {
        let j = Job::new("accepted-plus");
        j.set("patch-probe", "warn");
        j.binding(&["Warn", "Audit"]);
        j.set(file, answer);
        let (rc, out) = j.run();
        assert_eq!(rc, 1, "{file}: {out}");
        let red = the_refusal(&out);
        assert!(
            red.contains(named) && red.contains("a pod-template command patch was accepted"),
            "{file}: {red}"
        );
        assert!(red.contains("does not deny"), "{file}: {red}");
    }
}

#[test]
fn the_first_run_hands_david_the_line_even_when_the_credential_is_refused() {
    // The first run mints the key before it verifies, so the one act
    // David owes rides a RED line of its own, which the chore's judge
    // files as an open item (review b6d2a716, N1), with the pinned host
    // key's fingerprint to check first (N8) — beside the refusal, not
    // instead of it.
    let j = Job::new("mint-refused");
    j.first_run();
    j.set("patch-probe", "warn");
    j.binding(&["Warn", "Audit"]);
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    let reds = Job::reds(&out);
    assert_eq!(reds.len(), 2, "the act, then the refusal: {out}");
    let (route, kind, line) = &reds[0];
    assert_eq!(
        (route.as_str(), kind.as_str()),
        ("break-glass-deposit/authorize-key", "authorize")
    );
    assert!(
        line.ends_with(&format!(
            "{} ssh-ed25519 AAAAMINTEDPUBLIC break-glass-deposit@boss",
            job_forced_line()
        )),
        "the RED line ends with the whole authorized_keys line: {line}"
    );
    assert!(
        line.contains("SHA256:HOSTKEYFINGERPRINTFIXTURE")
            && line.contains("ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub"),
        "the host key check comes with the line: {line}"
    );
    assert_eq!(
        (reds[1].0.as_str(), reds[1].1.as_str()),
        ("break-glass-deposit", "refused"),
        "{out}"
    );
    assert!(!out.contains("not yet"), "{out}");
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_first_run_the_policy_refuses_for_stops_to_be_authorized_not_to_wait() {
    let j = Job::new("mint-denied");
    j.first_run();
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    let reds = Job::reds(&out);
    assert_eq!(reds.len(), 1, "the act alone — nothing was refused: {out}");
    assert_eq!(reds[0].0, "break-glass-deposit/authorize-key");
    assert!(out.contains("minted on this run"), "{out}");
    assert!(!out.contains("not yet"), "{out}");
    assert!(!j.called_ssh());
}

#[test]
fn the_deposit_has_no_not_yet_and_its_manifest_opts_into_none() {
    // The Job measures nothing that waiting changes, so it never says
    // EX_TEMPFAIL, and boss-chore.sh maps 75 to not-yet only for a chore
    // whose manifest opts in (review 0d3019f0, the chore note) — this
    // one does not, so a stray 75 closes `failed` and reaches a reader.
    let job = tree(JOB);
    assert!(
        !job.lines().any(|l| {
            let code = l.split('#').next().unwrap_or("");
            code.contains("exit 75")
        }),
        "the deposit exits 75 again"
    );
    assert!(
        !tree(MANIFEST).contains("name: BOSS_CHORE_NOT_YET_ON_75"),
        "the deposit's manifest opts into a not-yet its Job cannot say"
    );
}

#[test]
fn every_refusal_rides_one_red_line_the_chore_judge_can_file() {
    let j = Job::new("red-refusal");
    j.set("cani-get-nodes", "no");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    let reds = Job::reds(&out);
    assert_eq!(reds.len(), 1, "{out}");
    assert_eq!(reds[0].0, "break-glass-deposit");
    assert_eq!(reds[0].1, "refused");
    assert!(reds[0].2.contains("NOT deposited"), "{out}");
    // A manifest that sets no target is a misconfiguration, and it too
    // reaches a reader.
    let (rc, out) = j.run_with(&[("BOSS_DEPOSIT_TARGET", "")]);
    assert_eq!(rc, 78, "{out}");
    assert_eq!(
        Job::reds(&out)
            .iter()
            .map(|r| (r.0.as_str(), r.1.as_str()))
            .collect::<Vec<_>>(),
        vec![("break-glass-deposit", "misconfigured")],
        "{out}"
    );
}

#[test]
fn the_binding_the_job_reads_is_the_one_its_role_names_and_the_policy_declares() {
    let job = tree(JOB);
    let name = job
        .lines()
        .find_map(|l| {
            l.strip_prefix("POLICY_BINDING=\"${BOSS_DEPOSIT_POLICY_BINDING:-")
                .and_then(|r| r.strip_suffix("}\""))
        })
        .expect("the Job names the binding it reads");
    let m = tree(MANIFEST);
    let rule = m
        .split("\n---\n")
        .find(|doc| doc.lines().any(|l| l == "kind: ClusterRole"))
        .expect("a ClusterRole for the phase read");
    for decided in [
        "apiGroups: [admissionregistration.k8s.io]",
        "resources: [validatingadmissionpolicybindings]",
        &format!("resourceNames: [{name}]"),
        "verbs: [get]",
    ] {
        assert!(
            rule.contains(decided),
            "the phase read is get-by-name and nothing more ({decided}):\n{rule}"
        );
    }
    // Row B's binding is the one read, and the policy the Job holds it to
    // is the one that binding binds and the manifest declares (CLAUDE.md
    // §9a: one name in three files; review 0d3019f0 F2, "pin the policy
    // name"). 4e1c33b4 landed, so the binding must be there.
    let policy = job
        .lines()
        .find_map(|l| l.strip_prefix("POLICY="))
        .expect("the Job names the policy the binding must bind");
    let op = tree("infra/cluster/manifests/boss-break-glass-operator.yaml");
    let doc = op
        .split("\n---\n")
        .find(|d| d.contains("kind: ValidatingAdmissionPolicyBinding"))
        .expect("the operator manifest declares row B's binding");
    assert!(
        doc.lines().any(|l| l.trim() == format!("name: {name}")),
        "the Job reads a binding the operator manifest does not declare:\n{doc}"
    );
    assert!(
        doc.lines()
            .any(|l| l.trim() == format!("policyName: {policy}")),
        "the Job holds the binding to a policy it does not bind:\n{doc}"
    );
    assert!(
        op.split("\n---\n")
            .any(|d| d.contains("kind: ValidatingAdmissionPolicy\n")
                && d.lines().any(|l| l.trim() == format!("name: {policy}"))),
        "the Job holds the binding to a policy the manifest does not declare"
    );
}

#[test]
fn rbac_refusing_the_patch_does_not_prove_the_admission_rule() {
    let j = Job::new("patch-forbidden");
    j.set("patch-probe", "forbidden");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(j.deposited().is_none(), "{out}");
}

#[test]
fn the_allowed_roll_changes_exactly_the_main_and_init_images_without_persisting() {
    let j = Job::new("allowed-roll");
    let live: serde_json::Value =
        serde_json::from_str(&read(&j.dir.join("deployment.json"))).unwrap();
    let (rc, out) = j.run();
    assert_eq!(rc, 0, "{out}");
    let patch: serde_json::Value = serde_json::from_str(&read(&j.dir.join("roll.patch"))).unwrap();
    let changes = patch.as_array().unwrap();
    assert_eq!(changes.len(), 2);
    let current = live["spec"]["template"]["spec"]["containers"][0]["image"]
        .as_str()
        .unwrap();
    let repository = current.rsplit_once(':').unwrap().0;
    for (change, path, image) in [
        (
            &changes[0],
            "/spec/template/spec/containers/0/image",
            current,
        ),
        (
            &changes[1],
            "/spec/template/spec/initContainers/0/image",
            live["spec"]["template"]["spec"]["initContainers"][0]["image"]
                .as_str()
                .unwrap(),
        ),
    ] {
        assert_eq!(change["op"], "replace");
        assert_eq!(change["path"], path);
        let candidate = change["value"].as_str().unwrap();
        assert_ne!(candidate, image);
        assert_eq!(candidate.rsplit_once(':').unwrap().0, repository);
    }
    assert_eq!(changes[0]["value"], changes[1]["value"]);
    assert!(
        out.contains("allowed image roll (server dry-run): accepted"),
        "{out}"
    );
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_policy_that_denies_everything_cannot_deposit_a_credential() {
    let j = Job::new("deny-everything");
    j.set("patch-roll", "denied");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("allowed image roll"), "{out}");
    assert!(!j.called_ssh());
    assert_eq!(Job::reds(&out).len(), 1, "{out}");
}

#[test]
fn an_allowed_probe_never_reuses_either_observed_tag() {
    let j = Job::new("probe-tag-collision");
    let mut live: serde_json::Value =
        serde_json::from_str(&read(&j.dir.join("deployment.json"))).unwrap();
    let current = live["spec"]["template"]["spec"]["containers"][0]["image"]
        .as_str()
        .unwrap();
    let repository = current.rsplit_once(':').unwrap().0.to_string();
    let main = format!("{repository}:break-glass-deposit-probe");
    let init = format!("{main}-alternate");
    live["spec"]["template"]["spec"]["containers"][0]["image"] = serde_json::json!(main);
    live["spec"]["template"]["spec"]["initContainers"][0]["image"] = serde_json::json!(init);
    j.set("deployment.json", &live.to_string());
    let (rc, out) = j.run();
    assert_eq!(rc, 0, "{out}");
    let patch: serde_json::Value = serde_json::from_str(&read(&j.dir.join("roll.patch"))).unwrap();
    for change in patch.as_array().unwrap() {
        assert_ne!(change["value"], main);
        assert_ne!(change["value"], init);
    }
}

#[test]
fn allowed_image_observations_include_tags_digests_and_tagged_digests() {
    for suffix in [
        ":deployed",
        "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ":deployed@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        let j = Job::new("image-reference-forms");
        let mut live: serde_json::Value =
            serde_json::from_str(&read(&j.dir.join("deployment.json"))).unwrap();
        let current = live["spec"]["template"]["spec"]["containers"][0]["image"]
            .as_str()
            .unwrap();
        let image = format!("{}{suffix}", current.rsplit_once(':').unwrap().0);
        live["spec"]["template"]["spec"]["containers"][0]["image"] = serde_json::json!(image);
        live["spec"]["template"]["spec"]["initContainers"][0]["image"] = serde_json::json!(image);
        j.set("deployment.json", &live.to_string());
        let (rc, out) = j.run();
        assert_eq!(rc, 0, "{suffix}: {out}");
    }
}

#[test]
fn another_rule_or_a_cel_error_cannot_prove_any_negative_probe() {
    for probe in ["command", "image", "owner"] {
        for message in [
            "failed to evaluate validation: no such key",
            "only the image may change",
        ] {
            let j = Job::new(&format!("wrong-message-{probe}"));
            j.set(&format!("message-{probe}"), message);
            let (rc, out) = j.run();
            assert_eq!(rc, 1, "{probe}: {out}");
            assert!(!j.called_ssh());
            assert_eq!(Job::reds(&out).len(), 1, "{out}");
        }
    }
}

#[test]
fn an_unobserved_or_malformed_image_pair_cannot_prove_an_allowed_roll() {
    for bad in [
        serde_json::Value::Null,
        serde_json::json!("boss"),
        serde_json::json!("registry.invalid/foreign:tag"),
        serde_json::json!("repository:bad/tag"),
    ] {
        let j = Job::new("bad-image-pair");
        let mut live: serde_json::Value =
            serde_json::from_str(&read(&j.dir.join("deployment.json"))).unwrap();
        live["spec"]["template"]["spec"]["initContainers"][0]["image"] = bad;
        j.set("deployment.json", &live.to_string());
        let (rc, out) = j.run();
        assert_eq!(rc, 1, "{out}");
        assert!(!j.called_ssh());
        assert_eq!(Job::reds(&out).len(), 1, "{out}");
    }
}

#[test]
fn the_deposit_cronjob_is_in_the_converges_chore_image_pin() {
    let cron = manifest_object(MANIFEST, "CronJob", "boss-break-glass-deposit");
    assert_eq!(cron["metadata"]["labels"]["boss-chore"], "true");
    assert_eq!(
        cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["name"],
        "chore"
    );
    let converge = tree("infra/forge/cluster-deploy-runner.sh");
    assert!(converge.contains("cronjobs -l boss-chore=true \"chore=$REGISTRY:$HEAD\""));
}

#[test]
fn each_probe_message_is_pinned_to_the_canonical_policy_rule() {
    let j = Job::new("message-pin");
    let script = tree(JOB);
    for probe in ["command", "image", "owner"] {
        let message = read(&j.dir.join(format!("message-{probe}")));
        assert!(
            script.contains(&message),
            "{probe}: expected canonical message {message}"
        );
    }
}

#[test]
fn the_deposit_prints_the_full_observed_refusal_including_its_tail() {
    let j = Job::new("complete-refusal-receipt");
    let owner = read(&j.dir.join("message-owner"));
    let observed = format!("{owner}; machine-captured refusal tail sentinel");
    j.set("message-owner", &observed);
    let (rc, out) = j.run();
    assert_eq!(rc, 0, "{out}");
    for probe in ["command", "image", "owner"] {
        let message = read(&j.dir.join(format!("message-{probe}")));
        assert!(
            out.contains(&message),
            "the {probe} receipt lost observed stderr: {out}"
        );
    }
    j.assert_nothing_leaked(&out);
}

fn stored_chore(j: &Job, full_output: bool) -> (Output, PathBuf, PathBuf) {
    let helpers = j.dir.join("helpers");
    create_dir(&helpers.join("lib"));
    for script in ["boss-chore.sh", "boss-step.sh"] {
        write_exec(
            &helpers.join(script),
            &read(&repo_root().join("infra").join(script)),
        );
    }
    write_file(
        &helpers.join("lib/secret-header.sh"),
        &read(&repo_root().join("infra/lib/secret-header.sh")),
    );
    write_exec(
        &helpers.join("boss-maintenance-wrap.sh"),
        "#!/bin/bash\nexit 0\n",
    );
    // The chore and step writer are real. Packet opening and the API
    // adapter are planted; the adapter stores the JSON the writer sends.
    write_exec(
        &helpers.join("boss-api-curl.sh"),
        r#"#!/bin/bash
set -euo pipefail
method=GET body=''
while [ $# -gt 0 ]; do
    case "$1" in
        -X) method=$2; shift ;;
        -d|--data-binary) body=$2; shift ;;
    esac
    shift
done
case "$method" in
    GET) echo '{"data":[{"id":"packet","status":"open","metadata":{},"steps":[{"id":"run-step","spec_slug":"run","status":"ready","metadata":{}}]}],"total":1}' ;;
    PATCH)
        case "$body" in @*) cat "${body#@}" ;; *) printf '%s' "$body" ;; esac > "$STORED_FIELDS"
        ;;
    PUT) test -s "$STORED_FIELDS"; printf completed > "$STORED_STATUS" ;;
    *) exit 2 ;;
esac
"#,
    );
    let stored = j.dir.join("stored-metadata.json");
    let status = j.dir.join("stored-status");
    let path = format!(
        "{}:{}",
        j.dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(helpers.join("boss-chore.sh"))
        .args([
            "maintenance-break-glass-deposit",
            "Break-glass deposit",
            "--",
            "bash",
        ])
        .arg(repo_root().join(JOB))
        .env("PATH", path)
        .env("TMPDIR", j.dir.join("tmp"))
        .env("BOSS_DEPOSIT_KEY_DIR", j.dir.join("keys"))
        .env("BOSS_DEPOSIT_KNOWN_HOSTS", j.dir.join("known_hosts"))
        .env("BOSS_DEPOSIT_API_SERVER", SERVER)
        .env("BOSS_DEPOSIT_TARGET", TARGET)
        .env("BOSS_JOBS_URL", "http://fixture.invalid")
        .env("KUBERNETES_SERVICE_HOST", "fixture")
        .env("BOSS_MACHINE_TOKEN_DIR", j.dir.join("no-machine-token"))
        .env(
            "BOSS_CHORE_FULL_OUTPUT",
            if full_output { "1" } else { "0" },
        )
        .env("STORED_FIELDS", &stored)
        .env("STORED_STATUS", &status)
        .env_remove("BOSS_MACHINE_TOKEN")
        .env_remove("BOSS_RUN_SUMMARY_FILE")
        .env_remove("BOSS_STEP_OUTPUT_FILE")
        .env_remove("BOSS_STEP_DRY_RUN");
    (cmd.output().unwrap(), stored, status)
}

fn long_refusal(j: &Job, repetitions: usize) -> String {
    let owner = read(&j.dir.join("message-owner"));
    let observed = format!(
        "{owner}; {}END-OF-OBSERVED-REFUSAL",
        "long-admission-detail ".repeat(repetitions)
    );
    j.set("message-owner", &observed);
    observed
}

#[test]
fn the_chore_stores_the_complete_refusal_through_the_real_step_writer() {
    let j = Job::new("stored-refusal-file");
    let observed = long_refusal(&j, 7000);
    assert!(
        observed.len() > 128 * 1024,
        "cross Linux's per-argument limit"
    );
    let (out, stored, status) = stored_chore(&j, true);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    let fields: serde_json::Value = serde_json::from_str(&read(&stored)).unwrap();
    assert_eq!(fields["result"], "ok");
    assert_eq!(fields["exit_status"], "0");
    assert_eq!(read(&status), "completed");
    let recorded = fields["output"].as_str().unwrap();
    assert!(
        recorded.contains(&observed),
        "stored field lost the full observed refusal ({} bytes)",
        recorded.len()
    );
    let owner_line = |text: &str| {
        text.lines()
            .find(|line| line.contains("metadata.ownerReferences (server dry-run)"))
            .unwrap()
            .to_string()
    };
    assert_eq!(
        owner_line(recorded),
        owner_line(&log),
        "the durable field copies the observed line byte for byte"
    );
    j.assert_nothing_leaked(recorded);
}

#[test]
fn a_failed_deposit_stores_its_complete_evidence_and_failed_verdict() {
    let j = Job::new("stored-refusal-failed");
    let observed = long_refusal(&j, 7000);
    j.set("patch-roll", "forbidden");
    let (out, stored, status) = stored_chore(&j, true);
    let log = text(&out);
    assert_eq!(out.status.code(), Some(1), "{log}");
    let fields: serde_json::Value = serde_json::from_str(&read(&stored)).unwrap();
    assert_eq!(fields["result"], "failed");
    assert_eq!(fields["exit_status"], "1");
    assert_eq!(read(&status), "completed");
    let recorded = fields["output"].as_str().unwrap();
    assert!(
        recorded.contains(&observed),
        "failed run lost observed refusal"
    );
    assert!(recorded.contains("the allowed image roll was not proven"));
    assert!(!j.called_ssh(), "a refused deposit ships nothing");
    j.assert_nothing_leaked(recorded);
}

#[test]
fn a_chore_without_the_opt_in_keeps_its_existing_excerpt_bound() {
    let j = Job::new("stored-refusal-default");
    let observed = long_refusal(&j, 7000);
    let (out, stored, _) = stored_chore(&j, false);
    assert!(out.status.success(), "{}", text(&out));
    let fields: serde_json::Value = serde_json::from_str(&read(&stored)).unwrap();
    let recorded = fields["output"].as_str().unwrap();
    assert!(
        !recorded.contains(&observed),
        "default chore began posting whole logs"
    );
    assert!(recorded.len() < 128 * 1024);
    assert!(
        text(&out).contains(&observed),
        "the default still streams whole logs"
    );
}

#[test]
fn output_beyond_the_api_body_limit_is_refused_without_trimming_or_completing() {
    let j = Job::new("stored-refusal-oversize");
    let observed = long_refusal(&j, 110000);
    let (out, stored, status) = stored_chore(&j, true);
    let log = text(&out);
    assert!(out.status.success(), "recording remains best-effort: {log}");
    assert!(!stored.exists(), "no shortened metadata was stored");
    assert!(!status.exists(), "a lost record never completes the step");
    assert!(log.contains("beyond the jobs API's 2097152-byte body limit"));
    assert!(log.contains("did not record result=ok"));
    assert!(
        log.contains(&observed),
        "raw failure evidence stays in the chore log"
    );
    j.assert_nothing_leaked(&log);
}

#[test]
fn an_unreadable_output_file_never_posts_a_partial_record_or_completes() {
    let j = Job::new("stored-refusal-unreadable");
    let (_, stored, status) = stored_chore(&j, true);
    std::fs::remove_file(&stored).unwrap();
    std::fs::remove_file(&status).unwrap();
    let out = Command::new("bash")
        .arg(j.dir.join("helpers/boss-step.sh"))
        .args(["maintenance-break-glass-deposit", "run", "result=ok"])
        .env("TMPDIR", j.dir.join("tmp"))
        .env("BOSS_STEP_OUTPUT_FILE", j.dir.join("missing-output"))
        .env("BOSS_JOBS_URL", "http://fixture.invalid")
        .env("KUBERNETES_SERVICE_HOST", "fixture")
        .env("BOSS_MACHINE_TOKEN_DIR", j.dir.join("no-machine-token"))
        .env("STORED_FIELDS", &stored)
        .env("STORED_STATUS", &status)
        .env_remove("BOSS_MACHINE_TOKEN")
        .env_remove("BOSS_RUN_SUMMARY_FILE")
        .env_remove("BOSS_STEP_DRY_RUN")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("output file is not readable"));
    assert!(!stored.exists());
    assert!(!status.exists());
    j.assert_nothing_leaked(&text(&out));
}

#[test]
fn only_the_deposit_manifest_requests_full_chore_output() {
    let manifests = repo_root().join("infra/cluster/manifests");
    let opted: Vec<_> = std::fs::read_dir(manifests)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .filter(|path| read(path).contains("name: BOSS_CHORE_FULL_OUTPUT"))
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(opted, ["boss-break-glass-deposit.yaml"]);
    assert!(
        read(&repo_root().join(MANIFEST))
            .contains("name: BOSS_CHORE_FULL_OUTPUT\n                  value: \"1\"")
    );
}

#[test]
fn every_wrong_or_unmeasured_answer_refuses_the_deposit() {
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        (
            "cani-get-nodes",
            "no",
            "can-i get nodes answered no",
            "cannot read",
        ),
        (
            "cani-get-secrets",
            "yes",
            "can-i get secrets -n boss answered yes",
            "reads secrets",
        ),
        (
            "cani-delete-pods",
            "yes",
            "can-i delete pods -n boss answered yes",
            "deletes pods",
        ),
        (
            "cani-create-deployments",
            "yes",
            "can-i create deployments -n boss answered yes",
            "creates",
        ),
        (
            "patch-probe",
            "notfound",
            "the pod-template command patch probe unmeasured",
            "probe NotFound",
        ),
        (
            "patch-owner",
            "notfound",
            "the metadata.ownerReferences patch probe unmeasured",
            "owner probe NotFound",
        ),
    ];
    for (file, answer, named, what) in cases {
        let j = Job::new("wrong");
        j.set(file, answer);
        let (rc, out) = j.run();
        assert_eq!(rc, 1, "{what}: {out}");
        assert!(
            out.contains(named),
            "{what}: the refusal must name the answer: {out}"
        );
        assert!(j.deposited().is_none(), "{what}: deposited anyway");
        assert!(!j.called_ssh(), "{what}: reached ssh");
        j.assert_nothing_leaked(&out);
    }
    // An answer that is neither yes nor no — a TLS failure — is not a no.
    let j = Job::new("unmeasured");
    std::fs::remove_file(j.dir.join("cani-get-secrets")).expect("rm");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("can-i get secrets -n boss: UNMEASURED"),
        "{out}"
    );
    assert!(j.deposited().is_none());
}

#[test]
fn a_token_secret_with_no_token_is_refused_before_anything_is_verified() {
    let j = Job::new("no-token");
    j.token_secret("");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("holds no ServiceAccount token"), "{out}");
    assert!(!j.calls().contains("can-i"), "{}", j.calls());
}

#[test]
fn an_empty_key_secret_is_minted_into_and_the_line_to_authorize_is_printed() {
    let j = Job::new("mint");
    j.unmount_key();
    j.set(
        "secret-break-glass-deposit-key.json",
        "{\"metadata\":{\"name\":\"break-glass-deposit-key\",\"resourceVersion\":\"42\"}}",
    );
    let (rc, out) = j.run();
    assert_eq!(
        rc, 1,
        "a freshly minted key cannot be authorized yet: {out}"
    );
    assert!(out.contains("minted on this run"), "{out}");
    let patch: serde_json::Value =
        serde_json::from_str(&read(&j.dir.join("patched"))).expect("the patch is JSON");
    assert_eq!(patch[0]["op"], "test");
    assert_eq!(patch[0]["path"], "/metadata/resourceVersion");
    assert_eq!(
        patch[0]["value"], "42",
        "the mint must test the resourceVersion it read"
    );
    assert_eq!(patch[1]["op"], "add");
    assert_eq!(patch[1]["value"]["id_ed25519"], b64(MINTED_PRIVATE));
    assert_eq!(
        patch[1]["value"]["id_ed25519.pub"],
        b64("ssh-ed25519 AAAAMINTEDPUBLIC break-glass-deposit@boss\n")
    );
    assert!(
        out.contains(&format!(
            "{} ssh-ed25519 AAAAMINTEDPUBLIC break-glass-deposit@boss",
            job_forced_line()
        )),
        "the authorized_keys line for David is not printed whole: {out}"
    );
    assert!(
        out.contains("can-i get nodes: yes"),
        "the pass still verifies after minting: {out}"
    );
    assert!(
        !j.called_ssh(),
        "a key minted seconds ago was used:\n{}",
        j.calls()
    );
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_key_already_minted_but_not_yet_mounted_is_never_overwritten() {
    let j = Job::new("held");
    j.unmount_key();
    j.set(
        "secret-break-glass-deposit-key.json",
        &format!(
            "{{\"metadata\":{{\"resourceVersion\":\"43\"}},\"data\":{{\"id_ed25519\":\"{}\",\"id_ed25519.pub\":\"{}\"}}}}",
            b64(MOUNTED_PRIVATE),
            b64(MOUNTED_PUBLIC)
        ),
    );
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        !j.dir.join("patched").exists(),
        "a held key was overwritten"
    );
    assert!(
        !j.calls()
            .lines()
            .any(|l| l.starts_with("ssh-keygen") && l.contains(" -t ")),
        "a held key was re-minted:\n{}",
        j.calls()
    );
    assert!(
        out.contains("AAAAMOUNTEDPUBLIC"),
        "the held key's line is printed: {out}"
    );
    j.assert_nothing_leaked(&out);
}

#[test]
fn a_receipt_that_does_not_match_the_bytes_sent_is_not_a_deposit() {
    let j = Job::new("short");
    j.set("ssh-mode", "short");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("received 3 bytes") && out.contains("not proven"),
        "{out}"
    );
}

#[test]
fn a_key_boss_gcp_does_not_accept_yet_names_the_line_to_place() {
    let j = Job::new("denied");
    j.set("ssh-mode", "denied");
    let (rc, out) = j.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains(&format!("{} {}", job_forced_line(), MOUNTED_PUBLIC.trim())),
        "{out}"
    );
    j.assert_nothing_leaked(&out);
}

#[test]
fn the_job_refuses_to_run_without_the_address_and_the_target_it_is_given() {
    let j = Job::new("usage");
    let (rc, out) = j.run_with(&[("BOSS_DEPOSIT_API_SERVER", "")]);
    assert_eq!(rc, 78, "{out}");
    let (rc, out) = j.run_with(&[("BOSS_DEPOSIT_TARGET", "receiver.example.invalid")]);
    assert_eq!(rc, 78, "{out}");
    assert!(j.calls().is_empty(), "{}", j.calls());
}

// ---------------------------------------------------------------------------
// The files that must agree with the scripts (CLAUDE.md §9a)
// ---------------------------------------------------------------------------

#[test]
fn the_image_carries_the_script_under_the_name_the_manifest_runs_and_an_ssh_client() {
    let d = tree(DOCKERFILE);
    let m = tree(MANIFEST);
    let copy = d
        .lines()
        .find(|l| l.starts_with(&format!("COPY {JOB} ")))
        .unwrap_or_else(|| panic!("the image does not carry {JOB}"));
    let dest = copy.rsplit(' ').next().expect("a destination");
    assert!(
        m.lines().any(|l| l.trim()
            == format!(
                "/usr/local/bin/boss-chore.sh maintenance-break-glass-deposit \"Break-glass kubeconfig deposit to the configured receiver\" -- {dest}"
            )),
        "the manifest runs something other than {dest}, which the image carries, through boss-chore.sh"
    );
    assert!(
        d.lines().any(|l| l.trim() == "openssh-client \\"),
        "the image must carry the ssh client at BUILD time (b08725c2 Q3: no install at run time)"
    );
    let live: Vec<&str> = m
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        !live
            .iter()
            .any(|l| l.contains("apk add") || l.contains("apt-get")),
        "no package install at run time"
    );
}

#[test]
fn the_ssh_client_adds_no_setuid_binary_to_the_shared_image() {
    // Review b6d2a716, N5: openssh-client installs ssh-keysign setuid
    // root (4755, measured on the dev pod), into the image deploy/boss
    // runs too. Cleared in the SAME layer that installs it, so no layer
    // of the image ever carries the bit.
    let d = tree(DOCKERFILE);
    let layer = d
        .split("\nRUN ")
        .find(|l| l.contains("openssh-client \\"))
        .expect("the layer that installs openssh-client");
    assert!(
        layer.contains("chmod u-s /usr/lib/openssh/ssh-keysign"),
        "the layer installing openssh-client must clear ssh-keysign's setuid bit:\n{layer}"
    );
}

#[test]
fn the_workflow_names_its_trigger_and_closes_a_wait_as_not_yet() {
    let w = tree("infra/platform/workflows/maintenance-break-glass-deposit.toml");
    // Review b6d2a716, N7: a CronJob, not a systemd timer.
    assert!(
        w.contains("trigger_name = \"cluster-cronjob\"") && !w.contains("systemd-timer"),
        "the trigger is the CronJob"
    );
    // boss-chore.sh records exit 75 as result=not-yet; the protocol must
    // close that as a wait, and `failed` must not ALSO be reached by it.
    assert!(
        w.contains(
            "ready_when = \"steps.run.done AND steps.run.metadata.result = \\\"not-yet\\\"\""
        ),
        "a not-yet outcome off result=not-yet:\n{w}"
    );
    assert!(w.contains("terminal = { outcome = \"not-yet\" }"), "{w}");
    assert!(
        w.contains("ready_when = \"steps.run.done AND steps.run.metadata.result != \\\"ok\\\" AND steps.run.metadata.result != \\\"not-yet\\\"\""),
        "failed excludes not-yet, so the two terminals never race:\n{w}"
    );
}

#[test]
fn a_red_deposit_reaches_a_reader_and_a_silent_one_is_noticed() {
    // Review b6d2a716, N1. The chore judge files the RED lines of a
    // failed run as backlog-items (maintenance.chore.file_reds) …
    let rule = tree("infra/dispatcher/rules/file-backlog-items-on-break-glass-deposit-red.toml");
    assert!(
        rule.contains(
            "when = 'kind = \"maintenance-break-glass-deposit\" AND outcome = \"failed\"'"
        ) && rule.contains("handler = \"maintenance.chore.file_reds\""),
        "{rule}"
    );
    // … and a CronJob that stops running is a declared cadence the
    // silence sweep reads.
    let cadence = tree("infra/dispatcher/rules/cadence-silence-sweep-daily.toml");
    assert!(
        cadence.contains("\"interval_minutes.maintenance-break-glass-deposit\" = \"1440\""),
        "declare the daily deposit on the silence sweep"
    );
}

#[test]
fn the_pod_runs_as_the_images_uid_with_the_key_group_readable_through_fs_group() {
    let m = tree(MANIFEST);
    for decided in [
        "runAsNonRoot: true",
        "runAsUser: 1500",
        "runAsGroup: 1500",
        "fsGroup: 1500",
        "secretName: break-glass-deposit-key, defaultMode: 0o440",
        "serviceAccountName: break-glass-deposit",
        "emptyDir: {medium: Memory",
    ] {
        assert!(
            m.contains(decided),
            "the manifest no longer says `{decided}` (design b08725c2 Q3)"
        );
    }
    assert!(
        !m.contains("runAsUser: 0"),
        "root is not the decided answer"
    );
    let d = tree(DOCKERFILE);
    assert!(
        d.contains("useradd --uid 1500") && d.contains("\nUSER boss\n"),
        "the image no longer declares uid 1500 as its user"
    );
}

#[test]
fn the_deposit_requires_the_deployments_endpoint_and_target_from_its_pinned_config_map() {
    let m = tree(MANIFEST);
    for key in ["api_server", "target"] {
        assert!(m.contains(&format!(
            "configMapKeyRef: {{name: break-glass-deposit-known-hosts, key: {key}}}"
        )));
    }
    assert!(m.contains("configMap: {name: break-glass-deposit-known-hosts, defaultMode: 0o444}"));
    // The actual read adapter's behavioral test covers HTTPS, account,
    // exact host/pin equality and refusal BEFORE apply for both the
    // legacy migration and stable deployment-owned config.
}
