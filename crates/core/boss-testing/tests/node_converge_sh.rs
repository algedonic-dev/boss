//! `node-converge` and its plan `plan-a-node-converge`
//! (`infra/forge/node-converge.sh`, backlog 9d56c616): the ONE bounded
//! Talos act design 1bc4b4ed decided — a node's declared patch
//! (`infra/cluster/talos/patches/<node>.yaml`, read out of git at a
//! named commit) applied through `talosctl patch machineconfig
//! --mode=no-reboot`, after a `--dry-run` whose diff and sha256 a
//! passkey signed.
//!
//! WHY. On 2026-09-30 David ran `talosctl patch` by hand for w-1's second
//! NVMe (disk car 52ea56ac) and asked why the agent could not: there was
//! no door. This is the door, and every refusal below is a property the
//! trust boundary depends on — a free path or patch is impossible (the
//! file comes from git at a commit on the converged history, and must be
//! the declaration HEAD carries), a reboot is refused, a declaration the
//! read-back cannot read is refused, a patch that would append a second
//! copy of a live entry is refused, the act refuses a plan that moved,
//! and a credential in Talos's diff is never rendered.
//!
//! THE EFFECT IS READ BACK: after the apply, the live machine config is
//! piped through `infra/cluster/talos/check-declared.sh` until every
//! declared entry reads MATCH, and only then is the verb's `effect` line
//! printed — judged here with the runner's own engine (jq `test`).
//!
//! HOW THIS IS MEASURED. The script runs for real against a git fixture
//! (`BOSS_NODE_CONVERGE_TREE`), with `sudo` stubbed on PATH as a small
//! stateful node behind both doors and the estate registry served from a
//! file. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/node-converge.sh";

/// The stand-in behind both doors. State under $STUB_STATE: `ip-<node>`
/// its InternalIP, `cp-<node>` a control-plane label, `mc-<ip>.yaml` the
/// live machine config `get machineconfig` answers, `dry1-<ip>` /
/// `dry2-<ip>` what a dry run with one / two `--patch` flags prints
/// (two falls back to one: an idempotent patch), `after-<ip>.yaml` the
/// config an apply makes live, `dry-after-<ip>` the dry run once applied.
/// Every `--patch=` value lands in `patch-arg`; an apply touches
/// `applied-<ip>`. `ready-<node>` sets the Node's Ready status (default
/// True), `renew-<node>` freezes its kubelet lease's renewTime (default:
/// now). Switches: STUB_NEEDS_REBOOT=1 (Talos refuses the change in
/// no-reboot mode), STUB_APPLY_NOOP=1 (the apply answers and changes
/// nothing), STUB_APPLY_FAIL=<words> (the apply, not a dry run, fails
/// saying them).
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
tool=""
while [ $# -gt 0 ]; do
    case "$1" in
    --kubeconfig=/kc) tool=kubectl; shift; break ;;
    --talosconfig=/tc) tool=talosctl; shift; break ;;
    esac
    shift
done
if [ "$tool" = kubectl ]; then
    if [ "$1 $2" = "get node" ]; then
        n="$3"
        [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
        labels='"kubernetes.io/hostname":"'"$n"'"'
        [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
        ready=True
        [ -f "$S/ready-$n" ] && ready=$(cat "$S/ready-$n")
        # STUB_READY_AFTER_APPLY_READS=<k>: after an apply, the Node reads
        # Ready for its first k reads and NotReady after (a runtime that
        # goes bad a status update after the kubelet came back).
        if [ -n "${STUB_READY_AFTER_APPLY_READS:-}" ] && [ -f "$S/applied-$(cat "$S/ip-$n")" ]; then
            c=$(cat "$S/postreads-$n" 2>/dev/null || echo 0)
            c=$((c + 1))
            echo "$c" > "$S/postreads-$n"
            [ "$c" -gt "$STUB_READY_AFTER_APPLY_READS" ] && ready=False
        fi
        printf '{"kind":"Node","metadata":{"name":"%s","uid":"uid-%s","labels":{%s}},"spec":{},"status":{"addresses":[{"type":"InternalIP","address":"%s"}],"conditions":[{"type":"Ready","status":"%s"}]}}\n' "$n" "$n" "$labels" "$(cat "$S/ip-$n")" "$ready"
        exit 0
    fi
    if [ "$1 $2" = "get lease" ]; then
        # The kubelet's lease: renewed now, unless the test froze it.
        renew=$(date -u +%Y-%m-%dT%H:%M:%S.000000Z)
        [ -f "$S/renew-$3" ] && renew=$(cat "$S/renew-$3")
        printf '{"kind":"Lease","metadata":{"name":"%s","namespace":"kube-node-lease"},"spec":{"holderIdentity":"%s","renewTime":"%s"}}\n' "$3" "$3" "$renew"
        exit 0
    fi
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2
fi
if [ "$tool" = talosctl ]; then
    [ "$1" = -n ] || { echo "stub: talosctl without -n: $*" >&2; exit 2; }
    ip="$2"; shift 2
    case "$1 $2" in
    "get machineconfig")
        [ "$3 $4" = "-o yaml" ] || { echo "stub: get machineconfig without -o yaml" >&2; exit 2; }
        cat "$S/mc-$ip.yaml"
        exit 0 ;;
    "patch machineconfig")
        shift 2
        mode=""; dry=""; n=0
        for a in "$@"; do
            case "$a" in
            --mode=*) mode="${a#--mode=}" ;;
            --dry-run) dry=1 ;;
            --patch=*) n=$((n + 1)); printf '%s' "${a#--patch=}" > "$S/patch-arg" ;;
            *) echo "stub: unexpected patch argument: $a" >&2; exit 2 ;;
            esac
        done
        [ "$mode" = no-reboot ] || { echo "stub: a patch without --mode=no-reboot" >&2; exit 2; }
        if [ -n "${STUB_NEEDS_REBOOT:-}" ]; then
            echo "error applying new configuration: rpc error: code = InvalidArgument desc = apply configuration without a reboot is not possible: this change requires a reboot" >&2
            exit 1
        fi
        if [ -n "$dry" ]; then
            f="$S/dry$n-$ip"
            [ -f "$f" ] || f="$S/dry1-$ip"
            cat "$f" >&2
            exit 0
        fi
        if [ -n "${STUB_APPLY_FAIL:-}" ]; then
            echo "$STUB_APPLY_FAIL" >&2
            exit 1
        fi
        touch "$S/applied-$ip"
        if [ -z "${STUB_APPLY_NOOP:-}" ]; then
            cp "$S/after-$ip.yaml" "$S/mc-$ip.yaml"
            [ -f "$S/dry-after-$ip" ] && cp "$S/dry-after-$ip" "$S/dry1-$ip" && rm -f "$S/dry2-$ip"
        fi
        printf 'patched MachineConfigs.config.talos.dev/v1alpha1 at the node %s\n' "$ip"
        echo "Applied configuration without a reboot" >&2
        exit 0 ;;
    esac
fi
echo "stub: unexpected call: $*" >&2
exit 2
"#;

/// w-1's test declaration: two kubelet image-GC thresholds. A mapping of
/// scalars, so applying it twice changes nothing more than once.
const DECL: &str = "# w-1 (test) — the declared image-GC pair\n\
machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n      imageGCLowThresholdPercent: 40\n";

/// A live machine config as `talosctl get machineconfig -o yaml` prints
/// it, carrying a token and a CA key that must never be rendered.
fn live_config(host: &str, extra: &str) -> String {
    format!(
        "node: 10.20.0.14\nmetadata:\n    namespace: config\n    type: MachineConfigs.config.talos.dev\n    id: v1alpha1\nspec: |\n    version: v1alpha1\n    machine:\n        type: worker\n        token: SECRETTOKENaaaa.bbbbCCCC1234\n        ca:\n            crt: LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCk1JSUJQekNC\n            key: \"\"\n        kubelet:\n            image: ghcr.io/siderolabs/kubelet:v1.33.0\n{extra}            defaultRuntimeSeccompProfileEnabled: true\n        network:\n            hostname: {host}\n"
    )
}

const GC_LIVE: &str = "            extraConfig:\n                imageGCHighThresholdPercent: 50\n                imageGCLowThresholdPercent: 40\n";

/// What Talos prints for a dry run of DECL on a node without it — on
/// stderr, as talosctl's PrintApplyResults does — context lines carrying
/// the node's token.
fn dry_run(summary: &str, diff: &str) -> String {
    format!("Dry run summary:\n{summary}\n\nConfig diff:\n\n{diff}")
}

const NO_REBOOT: &str = "Applied configuration without a reboot (skipped in dry-run).";

const GC_DIFF: &str = "--- a\n+++ b\n@@ -2,6 +2,9 @@\n     type: worker\n     token: SECRETTOKENaaaa.bbbbCCCC1234\n     kubelet:\n         image: ghcr.io/siderolabs/kubelet:v1.33.0\n+        extraConfig:\n+            imageGCHighThresholdPercent: 50\n+            imageGCLowThresholdPercent: 40\n         defaultRuntimeSeccompProfileEnabled: true\n";

struct Node {
    dir: PathBuf,
    tree: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "curl", "sha256sum", "git", "python3"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "node_converge_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn git(tree: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(o.status.success(), "git {args:?}: {}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

impl Node {
    /// w-1, a worker the registry and the cluster agree on, with DECL
    /// committed in the fixture tree and absent live.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("node-converge-{name}"));
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let tree = dir.join("tree");
        std::fs::create_dir_all(tree.join("infra/cluster/talos/patches")).expect("tree");
        git(&tree, &["init", "-q", "-b", "main"]);
        let n = Node { dir, tree };
        n.registry(json!([
            node("forge", "10.20.0.15", "forge"),
            node("cp-1", "10.20.0.11", "talos-control-plane"),
            node("w-1", "10.20.0.14", "talos-worker"),
            node("w-2", "10.20.0.16", "talos-worker"),
        ]));
        n.state("ip-cp-1", "10.20.0.11");
        n.state("cp-cp-1", "");
        n.state("ip-w-1", "10.20.0.14");
        n.state("ip-w-2", "10.20.0.16");
        n.declare("w-1", DECL);
        n.state("mc-10.20.0.14.yaml", &live_config("w-1", ""));
        n.state("after-10.20.0.14.yaml", &live_config("w-1", GC_LIVE));
        n.state("dry1-10.20.0.14", &dry_run(NO_REBOOT, GC_DIFF));
        n.state("dry-after-10.20.0.14", &dry_run(NO_REBOOT, ""));
        n
    }

    /// Commit `body` as <node>'s declaration; returns the commit sha.
    fn declare(&self, node: &str, body: &str) -> String {
        write_file(
            &self
                .tree
                .join(format!("infra/cluster/talos/patches/{node}.yaml")),
            body,
        );
        self.commit(&format!("declare {node}"))
    }

    fn commit(&self, msg: &str) -> String {
        git(&self.tree, &["add", "-A"]);
        git(&self.tree, &["commit", "-q", "--allow-empty", "-m", msg]);
        git(&self.tree, &["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        git(&self.tree, &["rev-parse", "HEAD"])
    }

    fn registry(&self, nodes: Value) {
        write_file(
            &self.dir.join("nodes.json"),
            &json!({ "data": nodes }).to_string(),
        );
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn has(&self, file: &str) -> bool {
        self.dir.join("state").join(file).exists()
    }

    fn read_state(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join("state").join(file)).unwrap_or_default()
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_NODE_CONVERGE_TREE", &self.tree)
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_TALOSCTL", self.dir.join("talosctl"))
            .env("BOSS_NODE_CONVERGE_WAIT_S", "2")
            .env("BOSS_NODE_CONVERGE_POLL_S", "1")
            .env("BOSS_NODE_CONVERGE_SETTLE_S", "0")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    fn argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default()
    }

    /// The plan for <node> at <sha>: (stdout, the hash on stderr).
    fn plan(&self, node: &str, sha: &str) -> (String, String) {
        let o = self.run(&["--plan", node, sha], &[]);
        assert!(o.status.success(), "the plan renders: {}", text(&o));
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        let hash = stderr
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("plan-sha256 on stderr: {stderr}"))
            .trim()
            .to_string();
        (String::from_utf8_lossy(&o.stdout).to_string(), hash)
    }
}

fn node(id: &str, address: &str, role: &str) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": false
    })
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn verb(name: &str) -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The lines of `out` the verb's declared `effect` matches, judged by the
/// runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("node-converge")["effect"]
        .as_str()
        .expect("node-converge declares an `effect`")
        .to_string();
    out.lines()
        .filter(|line| {
            let o = Command::new("jq")
                .args([
                    "-n",
                    "--arg",
                    "re",
                    &re,
                    "--arg",
                    "l",
                    line,
                    "$l | test($re)",
                ])
                .output()
                .expect("jq runs");
            assert!(o.status.success(), "jq judges {re:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim() == "true"
        })
        .map(str::to_string)
        .collect()
}

/// No byte of the node's credentials reaches anything the verb prints.
fn no_credential(out: &str) {
    for secret in ["SECRETTOKEN", "LS0tLS1CRUdJTi", "bbbbCCCC1234"] {
        assert!(
            !out.contains(secret),
            "a credential of the live config was printed ({secret}):\n{out}"
        );
    }
}

// ------------------------------------------------------------------ plan

#[test]
fn the_plan_names_the_node_the_commit_the_declaration_and_talos_own_diff() {
    let n = Node::new("plan");
    let sha = n.head();
    let (plan, hash) = n.plan("w-1", &sha);
    for want in [
        "plan: node-converge",
        "node: w-1",
        "address: 10.20.0.14",
        "estate role: talos-worker",
        &format!("at commit {sha}"),
        "patch machineconfig --mode=no-reboot",
        "imageGCHighThresholdPercent: 50",
        "ABSENT",
        "Applied configuration without a reboot (skipped in dry-run).",
        "+        extraConfig:",
        "+            imageGCLowThresholdPercent: 40",
        "@@ -2,6 +2,9 @@",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    // A context line shows its key, never its value: the token's line is
    // in Talos's diff and its value is not in the plan.
    assert!(plan.contains("     token: …"), "{plan}");
    no_credential(&plan);
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(!n.has("applied-10.20.0.14"), "the plan applies nothing");
    // Deterministic: a second render of one true state is byte-identical.
    assert_eq!(n.plan("w-1", &sha), (plan, hash));
    assert!(
        !n.argv().lines().any(|l| l.contains("patch machineconfig")
            && !l.contains("--dry-run")
            && l.contains("--talosconfig")),
        "only dry runs: {}",
        n.argv()
    );
}

#[test]
fn the_declaration_comes_from_git_at_a_commit_the_checkout_converged_to() {
    let n = Node::new("tree-sha");
    let first = n.head();
    // A later commit that does not touch the declaration: the named
    // commit still carries the declaration HEAD carries.
    write_file(&n.tree.join("README"), "unrelated\n");
    n.commit("unrelated");
    let (plan, _) = n.plan("w-1", &first);
    assert!(plan.contains(&first), "{plan}");
    // Every refusal below is judged on the door calls it makes alone.
    std::fs::remove_file(n.dir.join("argv")).expect("the plan above called the doors");

    // A short sha, or anything that is not 40 hex, is refused unread.
    for bad in [&first[..12], "HEAD", "not-a-sha"] {
        let o = n.run(&["--plan", "w-1", bad], &[]);
        assert_eq!(o.status.code(), Some(78), "{bad}: {}", text(&o));
    }
    // A commit this checkout does not hold.
    let o = n.run(&["--plan", "w-1", &"a".repeat(40)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));

    // A commit off the converged history: a branch nobody merged.
    git(&n.tree, &["checkout", "-q", "-b", "side"]);
    let side = n.declare("w-1", &DECL.replace("50", "90"));
    git(&n.tree, &["checkout", "-q", "main"]);
    let o = n.run(&["--plan", "w-1", &side], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("not on the history"), "{out}");

    // The declaration changed after the named commit: the older file is
    // not what HEAD declares, and applying it would regress the node.
    let newer = n.declare("w-1", &DECL.replace("40", "35"));
    let o = n.run(&["--plan", "w-1", &first], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&newer[..12]) || out.contains("HEAD"), "{out}");

    // A node with no declaration at that commit.
    let o = n.run(&["--plan", "w-2", &newer], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("patches/w-2.yaml"), "{out}");
    assert!(
        !n.argv().contains("patch machineconfig"),
        "a refused declaration reaches no dry run: {}",
        n.argv()
    );
}

#[test]
fn a_declaration_the_read_back_cannot_read_is_refused_before_any_door() {
    let n = Node::new("unread-class");
    let sha = n.declare(
        "w-1",
        "machine:\n  install:\n    disk: /dev/nvme1n1\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50\n",
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("machine.install"), "names the entry: {out}");
    assert!(n.argv().is_empty(), "no door opened for it: {}", n.argv());
}

#[test]
fn a_patch_that_would_reboot_the_node_is_refused() {
    let n = Node::new("reboot");
    let sha = n.head();
    // Talos, asked in no-reboot mode, refuses the change itself.
    let o = n.run(&["--plan", "w-1", &sha], &[("STUB_NEEDS_REBOOT", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("REBOOT"), "{out}");
    // And a dry run that says anything but "without a reboot" is refused.
    n.state(
        "dry1-10.20.0.14",
        &dry_run(
            "Applied configuration with a reboot (skipped in dry-run).",
            GC_DIFF,
        ),
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("with a reboot"), "{out}");
    // No evidence is not a pass: a dry run with no summary at all.
    n.state("dry1-10.20.0.14", "something else entirely\n");
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(!n.has("applied-10.20.0.14"));
}

#[test]
fn a_changed_credential_in_the_diff_is_refused_and_never_printed() {
    let n = Node::new("secret");
    let sha = n.head();
    n.state(
        "dry1-10.20.0.14",
        &dry_run(
            NO_REBOOT,
            "--- a\n+++ b\n@@ -2,3 +2,3 @@\n     type: worker\n-    token: SECRETTOKENaaaa.bbbbCCCC1234\n+    token: NEWSECRETvalue.zzzz\n",
        ),
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("token"), "names the key: {out}");
    assert!(!out.contains("NEWSECRET"), "{out}");
    no_credential(&out);

    // A credential-shaped value under an innocent key: a long base64 run.
    n.state(
        "dry1-10.20.0.14",
        &dry_run(
            NO_REBOOT,
            "--- a\n+++ b\n@@ -2,3 +2,3 @@\n+    note: QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9w\n",
        ),
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(!out.contains("QUJDREVGR0hJSktMTU5PUFFS"), "{out}");
}

#[test]
fn a_node_that_already_carries_the_declaration_has_nothing_to_converge() {
    let n = Node::new("noop");
    let sha = n.head();
    n.state("dry1-10.20.0.14", &dry_run(NO_REBOOT, ""));
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("already carries"), "{out}");
}

#[test]
fn a_patch_that_would_append_a_second_copy_of_a_live_entry_is_refused() {
    // README: Talos's merge appends list entries it cannot key — how
    // w-1's seed file entry came to be doubled. Two --patch flags show it:
    // applying twice changes more than applying once.
    let n = Node::new("append");
    let decl = "machine:\n  files:\n    - content: |\n        x\n      permissions: 0o644\n      path: /var/local/gate-seed/.declared-by-talos\n      op: create\n";
    let sha = n.declare("w-1", decl);
    let one = "--- a\n+++ b\n@@ -9,3 +9,8 @@\n         hostname: w-1\n+    files:\n+        - content: |\n+            x\n+          permissions: 0o644\n+          path: /var/local/gate-seed/.declared-by-talos\n+          op: create\n";
    let two = format!(
        "{one}+        - content: |\n+            x\n+          permissions: 0o644\n+          path: /var/local/gate-seed/.declared-by-talos\n+          op: create\n"
    );
    n.state("dry1-10.20.0.14", &dry_run(NO_REBOOT, one));
    n.state("dry2-10.20.0.14", &dry_run(NO_REBOOT, &two));
    // Absent live: the one application adds it once, and the plan says
    // the patch is not idempotent.
    let (plan, _) = n.plan("w-1", &sha);
    assert!(plan.contains("idempotent: no"), "{plan}");
    // Present live: a second copy is refused, naming the entry.
    let live = live_config("w-1", "").replace(
        "        network:\n",
        "        files:\n            - content: |\n                x\n              permissions: 0o644\n              path: /var/local/gate-seed/.declared-by-talos\n              op: create\n        network:\n",
    );
    n.state("mc-10.20.0.14.yaml", &live);
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("machine.files[/var/local/gate-seed/.declared-by-talos]"),
        "{out}"
    );
    assert!(out.contains("second copy"), "{out}");
}

#[test]
fn the_plan_refuses_a_node_the_registry_or_the_cluster_does_not_vouch_for() {
    let n = Node::new("bounds");
    let sha = n.head();
    // Not a Talos node: refused before any door opens.
    let o = n.run(&["--plan", "forge", &sha], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(n.argv().is_empty(), "{}", n.argv());
    // Not in the registry at all.
    let o = n.run(&["--plan", "w-9", &sha], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    // The cluster calls the worker a control plane.
    n.state("cp-w-1", "");
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("control plane"), "{out}");
    std::fs::remove_file(n.dir.join("state/cp-w-1")).expect("unlabel");
    // The registry and the cluster disagree on the address.
    n.state("ip-w-1", "10.20.0.99");
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    n.state("ip-w-1", "10.20.0.14");
    // The live config names another host: a wrong target answers.
    n.state("mc-10.20.0.14.yaml", &live_config("w-2", ""));
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("hostname"), "{out}");
    no_credential(&out);
    assert!(!n.argv().contains("--dry-run"), "{}", n.argv());
}

#[test]
fn a_control_plane_is_converged_only_when_the_cluster_agrees_it_is_one() {
    // Design 1bc4b4ed: every node with a declaration is converged; the
    // bound is no reboot and a signed diff, not the node's kind.
    let n = Node::new("control-plane");
    let sha = n.declare("cp-1", DECL);
    n.state(
        "mc-10.20.0.11.yaml",
        &live_config("cp-1", "").replace("type: worker", "type: controlplane"),
    );
    n.state("dry1-10.20.0.11", &dry_run(NO_REBOOT, GC_DIFF));
    let (plan, _) = n.plan("cp-1", &sha);
    assert!(plan.contains("estate role: talos-control-plane"), "{plan}");
    // A registry row that says control plane for a node the cluster does
    // not label as one acts on nothing.
    std::fs::remove_file(n.dir.join("state/cp-cp-1")).expect("unlabel");
    let o = n.run(&["--plan", "cp-1", &sha], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
}

// ------------------------------------------------------------------- act

#[test]
fn node_converge_applies_only_the_signed_plan_and_reads_the_declaration_back() {
    let n = Node::new("act");
    let sha = n.head();
    let (plan, hash) = n.plan("w-1", &sha);
    let o = n.run(&["w-1", &sha, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(n.has("applied-10.20.0.14"), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first, the capture before the act: {out}"
    );
    assert_eq!(
        n.read_state("patch-arg").trim_end(),
        DECL.trim_end(),
        "the patch applied is the declaration at the named commit, byte for byte"
    );
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("w-1") && hits[0].contains(&sha),
        "{hits:?}"
    );
    no_credential(&out);

    // At most once: the applied plan re-renders to a refusal.
    let o = n.run(&["w-1", &sha, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn node_converge_refuses_a_plan_that_moved_since_it_was_signed() {
    let n = Node::new("drift");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    // The node changed under the signature: Talos's diff is not the one
    // the passkey saw.
    n.state(
        "dry1-10.20.0.14",
        &dry_run(NO_REBOOT, &GC_DIFF.replace("50", "55")),
    );
    let o = n.run(&["w-1", &sha, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains(&hash), "names the approved hash: {out}");
    assert!(!n.has("applied-10.20.0.14"), "{out}");

    for bad in ["0".repeat(64), "not-a-hash".to_string()] {
        let o = n.run(&["w-1", &sha, &bad], &[]);
        assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    }
    // The write takes exactly the node, the commit and the hash.
    let o = n.run(&["w-1", &sha], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(!n.has("applied-10.20.0.14"));
}

#[test]
fn an_apply_the_node_never_took_is_not_an_effect() {
    let n = Node::new("noop-apply");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    let o = n.run(&["w-1", &sha, &hash], &[("STUB_APPLY_NOOP", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("NOT proven"), "{out}");
    assert!(
        out.contains("ABSENT"),
        "says what the read-back found: {out}"
    );
    no_credential(&out);
}

// ------------------------------------------- review 9cb9af67 (CHANGES)

/// A declaration of the image-GC pair plus one files entry, and a patch
/// Talos's two-application dry run shows appending.
fn appending_declaration(n: &Node) -> String {
    let sha = n.declare(
        "w-1",
        &format!(
            "{DECL}  files:\n    - content: |\n        x\n      permissions: 0o644\n      path: /var/local/gate-seed/.declared-by-talos\n      op: create\n"
        ),
    );
    let one = "--- a\n+++ b\n@@ -9,3 +9,5 @@\n         hostname: w-1\n+    files:\n+        - path: /var/local/gate-seed/.declared-by-talos\n";
    let two = format!("{one}+        - path: /var/local/gate-seed/.declared-by-talos\n");
    n.state("dry1-10.20.0.14", &dry_run(NO_REBOOT, one));
    n.state("dry2-10.20.0.14", &dry_run(NO_REBOOT, &two));
    sha
}

#[test]
fn review_1_an_appending_patch_is_refused_when_any_declared_entry_of_any_class_is_live() {
    // The image-GC pair is live (MATCH); only the files entry is ABSENT.
    // The patch appends, so the plan may not say every entry is ABSENT.
    let n = Node::new("rv1-any-class");
    let sha = appending_declaration(&n);
    n.state("mc-10.20.0.14.yaml", &live_config("w-1", GC_LIVE));
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("machine.kubelet.extraConfig.imageGCHighThresholdPercent"),
        "names the live entry of the extraConfig class: {out}"
    );
    assert!(!out.contains("ABSENT live, so this run adds"), "{out}");
    // With nothing live, the plan renders and says so truthfully.
    n.state("mc-10.20.0.14.yaml", &live_config("w-1", ""));
    let (plan, _) = n.plan("w-1", &sha);
    assert!(
        plan.contains("idempotent: no")
            && plan.contains("every entry it declares reads ABSENT live"),
        "{plan}"
    );
}

#[test]
fn review_2_the_refusal_never_tells_anyone_to_remove_a_working_live_entry() {
    let n = Node::new("rv2-remedy");
    let sha = appending_declaration(&n);
    n.state("mc-10.20.0.14.yaml", &live_config("w-1", GC_LIVE));
    let out = text(&n.run(&["--plan", "w-1", &sha], &[]));
    assert!(out.contains("narrow"), "names the remedy: {out}");
    for bad in ["emove the live", "edit machineconfig", "emove a"] {
        assert!(
            !out.contains(bad),
            "prescribes removing live config ({bad}): {out}"
        );
    }
    let readme =
        std::fs::read_to_string(repo_root().join("infra/cluster/talos/README.md")).expect("README");
    let door = readme
        .split("## The door: `node-converge`")
        .nth(1)
        .expect("the README's node-converge section");
    assert!(
        door.contains("Never remove a working live entry"),
        "the README says it plainly:\n{door}"
    );
    assert!(!door.contains("removed live by hand"), "{door}");
}

#[test]
fn review_3_the_effect_is_the_node_ready_not_only_its_config() {
    // The config reads MATCH and the Node goes NotReady: not an effect.
    let n = Node::new("rv3-not-ready");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    n.state("ready-w-1", "False");
    let o = n.run(&["w-1", &sha, &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("NOT proven") && out.contains("Ready"), "{out}");

    // Ready, but the kubelet's lease has not renewed since the apply: a
    // stale Ready is not a kubelet that came back.
    let n = Node::new("rv3-stale-lease");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    n.state("renew-w-1", "2026-01-01T00:00:00.000000Z");
    let o = n.run(
        &["w-1", &sha, &hash],
        &[("BOSS_NODE_CONVERGE_SETTLE_S", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(out.contains("lease"), "{out}");

    // Ready with a fresh lease: the effect names Ready.
    let n = Node::new("rv3-ready");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    let out = text(&n.run(&["w-1", &sha, &hash], &[]));
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "{out}");
    assert!(hits[0].contains("Ready=True"), "{hits:?}");
}

#[test]
fn review_4_the_plan_embeds_verdicts_and_keys_never_live_values() {
    let n = Node::new("rv4-findings");
    let sha = n.head();
    // A live file's content shaped like a credential: base64 of the
    // alphabet, a fixture, never a secret.
    let planted = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2";
    let live = live_config("w-1", "").replace(
        "        network:\n",
        &format!(
            "        files:\n            - content: {planted}\n              permissions: 0o600\n              path: /var/local/creds/registry-token\n              op: create\n        network:\n"
        ),
    );
    n.state("mc-10.20.0.14.yaml", &live);
    let (plan, _) = n.plan("w-1", &sha);
    assert!(
        plan.contains("UNDECLARED")
            && plan.contains("machine.files[/var/local/creds/registry-token]"),
        "the finding and its key are in the plan: {plan}"
    );
    assert!(
        !plan.contains(planted),
        "a live value reached the signed plan:\n{plan}"
    );
    assert!(!plan.contains("declared, not live: 50"), "{plan}");
}

#[test]
fn review_5_credential_shapes_on_a_changed_line_refuse_the_plan_unprinted() {
    let n = Node::new("rv5-shapes");
    let sha = n.head();
    for (line, secret) in [
        ("+    - abcdef.0123456789abcdef", "0123456789abcdef"),
        (
            "+    x: 0123456789abcdef0123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdef",
        ),
        (
            "+    x: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.c2ln-abc_def",
            "eyJhbGciOiJIUzI1NiJ9",
        ),
        ("+    presharedKey: short", "short"),
    ] {
        n.state(
            "dry1-10.20.0.14",
            &dry_run(
                NO_REBOOT,
                &format!("--- a\n+++ b\n@@ -2,3 +2,3 @@\n{line}\n"),
            ),
        );
        let o = n.run(&["--plan", "w-1", &sha], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{line}: {out}");
        assert!(!out.contains(secret), "{line} was printed: {out}");
    }
}

#[test]
fn review_6_a_summary_prefixed_with_the_node_reads_and_anything_else_fails_closed() {
    let n = Node::new("rv6-prefix");
    let sha = n.head();
    for prefix in ["10.20.0.14: ", "w-1: "] {
        n.state(
            "dry1-10.20.0.14",
            &format!("{prefix}{}", dry_run(NO_REBOOT, GC_DIFF)),
        );
        let (plan, _) = n.plan("w-1", &sha);
        assert!(plan.contains("+        extraConfig:"), "{prefix}: {plan}");
    }
    n.state(
        "dry1-10.20.0.14",
        &format!("10.20.0.99: {}", dry_run(NO_REBOOT, GC_DIFF)),
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
}

#[test]
fn review_7_nothing_was_applied_is_said_only_of_talos_own_no_reboot_refusal() {
    let n = Node::new("rv7-apply-fail");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    let o = n.run(
        &["w-1", &sha, &hash],
        &[(
            "STUB_APPLY_FAIL",
            "error: rpc error: code = Unavailable desc = connection closed while the node was rebooting",
        )],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(!out.contains("nothing was applied"), "{out}");
    assert!(out.contains("cannot say"), "{out}");

    let o = n.run(
        &["w-1", &sha, &hash],
        &[(
            "STUB_APPLY_FAIL",
            "error applying new configuration: rpc error: code = InvalidArgument desc = apply configuration without a reboot is not possible: kubelet image changed",
        )],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("nothing was applied"), "{out}");
}

#[test]
fn review_8_extra_config_admits_only_the_kubelet_keys_declared_today() {
    let n = Node::new("rv8-extra-config");
    for (key, value) in [
        (
            "authentication",
            "\n        anonymous:\n          enabled: true",
        ),
        ("authorization", "\n        mode: AlwaysAllow"),
        ("allowedUnsafeSysctls", "\n        - net.core.somaxconn"),
    ] {
        let sha = n.declare(
            "w-1",
            &format!("machine:\n  kubelet:\n    extraConfig:\n      {key}:{value}\n"),
        );
        let o = n.run(&["--plan", "w-1", &sha], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{key}: {out}");
        assert!(out.contains(key), "{key}: {out}");
    }
    assert!(!n.argv().contains("--dry-run"), "{}", n.argv());
}

#[test]
fn a_user_volume_is_applied_only_with_a_selector_that_excludes_the_system_disk() {
    // Disk car 1 (52ea56ac) made check-declared read UserVolumeConfig
    // documents back by name. node-converge may apply one only when its
    // diskSelector provably excludes the system disk; a volume Talos
    // provisions on the install disk would take w-1's EPHEMERAL with it.
    let n = Node::new("user-volume");
    let volume = |selector: &str| {
        format!(
            "{DECL}---\napiVersion: v1alpha1\nkind: UserVolumeConfig\nname: gate\nprovisioning:\n  diskSelector:\n    match: '{selector}'\n  minSize: 500GB\nfilesystem:\n  type: xfs\n"
        )
    };
    let sha = n.declare("w-1", &volume("!system_disk && disk.transport == \"nvme\""));
    let (plan, _) = n.plan("w-1", &sha);
    assert!(plan.contains("kind: UserVolumeConfig"), "{plan}");
    assert!(
        plan.contains("ABSENT") && plan.contains("UserVolumeConfig[gate]"),
        "the volume is read back by name: {plan}"
    );

    std::fs::remove_file(n.dir.join("argv")).expect("the plan above called the doors");
    let sha = n.declare("w-1", &volume("disk.transport == \"nvme\""));
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("UserVolumeConfig[gate]") && out.contains("system disk"),
        "{out}"
    );
    assert!(n.argv().is_empty(), "no door opened for it: {}", n.argv());
}

#[test]
fn review_be5ba8f7_a_live_config_the_check_cannot_decode_renders_no_plan() {
    // Finding 2: a YAML-only escape crashed check-declared with exit 1,
    // which render read as "findings" — and, with an appending patch,
    // signed "every entry it declares reads ABSENT live" over a config
    // nobody read. The check now says UNREADABLE (exit 2), and render
    // trusts exit 0 or 1 only beside the check's own summary line.
    let n = Node::new("rvb-undecodable");
    let sha = appending_declaration(&n);
    n.state(
        "mc-10.20.0.14.yaml",
        &live_config("w-1", &format!("{GC_LIVE}            note: \"\\x41\"\n")),
    );
    let o = n.run(&["--plan", "w-1", &sha], &[]);
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(o.stdout.is_empty(), "no plan is rendered: {out}");
    assert!(!out.contains("reads ABSENT live"), "{out}");
    assert!(out.contains("UNREADABLE"), "{out}");
}

#[test]
fn review_be5ba8f7_ready_is_read_again_at_the_end_of_the_bound() {
    // Finding 5: a change that restarts containerd but not the kubelet
    // can report NotReady a status update later than the first fresh
    // lease. The effect is printed only on a read at the end of the bound.
    let n = Node::new("rvb-ready-end");
    let sha = n.head();
    let (_, hash) = n.plan("w-1", &sha);
    let o = n.run(
        &["w-1", &sha, &hash],
        &[("STUB_READY_AFTER_APPLY_READS", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
    assert!(
        out.contains("NOT proven") && out.contains("Ready=False"),
        "{out}"
    );
}

#[test]
fn review_5ed37182_a_unicode_line_break_cannot_hide_a_document_or_a_key() {
    // Talos's yaml.v3 breaks lines at NEL, LS and PS; the check read
    // them as part of a comment, so a UserVolumeConfig or a `cluster`
    // key hid behind one and --only-read-classes passed. The check now
    // refuses them before any parse, so the verb refuses before any door.
    let n = Node::new("rv5ed-breaks");
    for sep in ['\u{0085}', '\u{2028}', '\u{2029}'] {
        for (shape, body) in [
            (
                "doc",
                format!(
                    "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50 # note{sep}---{sep}apiVersion: v1alpha1{sep}kind: UserVolumeConfig{sep}name: evil{sep}provisioning:{sep}  diskSelector:{sep}    match: \"true\"\n"
                ),
            ),
            (
                "key",
                format!(
                    "machine:\n  kubelet:\n    extraConfig:\n      imageGCHighThresholdPercent: 50 # note{sep}cluster:{sep}  apiServer:{sep}    extraArgs:{sep}      anonymous-auth: \"true\"\n"
                ),
            ),
        ] {
            let sha = n.declare("w-1", &body);
            let o = n.run(&["--plan", "w-1", &sha], &[]);
            let out = text(&o);
            let cp = format!("U+{:04X}", sep as u32);
            assert_eq!(o.status.code(), Some(78), "{cp} hiding a {shape}: {out}");
            assert!(out.contains(&cp), "{cp} {shape}: {out}");
        }
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

#[test]
fn review_a7fa61ec_a_declaration_not_in_canonical_form_is_refused_before_any_door() {
    // A `--- {…}` flow document was dropped by the reader and read whole
    // by Talos. Declarations must now equal the reader's own canonical
    // re-emission, so every such shape is refused before any door opens.
    let n = Node::new("rva7-canonical");
    for body in [
        format!(
            "{DECL}--- {{apiVersion: v1alpha1, kind: UserVolumeConfig, name: evil, provisioning: {{diskSelector: {{match: \"true\"}}}}}}\n"
        ),
        format!(
            "{DECL}--- {{cluster: {{apiServer: {{extraArgs: {{anonymous-auth: \"true\"}}}}}}}}\n"
        ),
        DECL.replace(
            "imageGCHighThresholdPercent: 50",
            "imageGCHighThresholdPercent: \"50\"",
        ),
    ] {
        let sha = n.declare("w-1", &body);
        let o = n.run(&["--plan", "w-1", &sha], &[]);
        assert_eq!(o.status.code(), Some(78), "{body:?}: {}", text(&o));
    }
    assert!(n.argv().is_empty(), "no door opened: {}", n.argv());
}

#[test]
fn review_9_an_annotated_tag_is_not_a_commit() {
    let n = Node::new("rv9-tag");
    git(&n.tree, &["tag", "-a", "v1", "-m", "a tag"]);
    let tag = git(&n.tree, &["rev-parse", "v1"]);
    assert_ne!(tag, n.head());
    let o = n.run(&["--plan", "w-1", &tag], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
}

// ----------------------------------------------------------------- verbs

#[test]
fn the_verb_files_bound_what_a_packet_can_ask() {
    let w = verb("node-converge");
    let p = verb("plan-a-node-converge");
    for (name, v) in [("node-converge", &w), ("plan-a-node-converge", &p)] {
        assert_eq!(v["hosts"], json!(["forge"]), "{name} serves the forge only");
        assert_eq!(v["argv"][0], SCRIPT, "{name}");
        assert!(
            v["timeout"].as_u64().is_some(),
            "{name} declares its timeout"
        );
        assert_eq!(v["params"][0]["name"], "node", "{name}");
        assert_eq!(v["params"][1]["name"], "tree_sha", "{name}");
        assert_eq!(
            v["params"][1]["pattern"], "^[0-9a-f]{40}$",
            "{name}: a full commit, never an abbreviation"
        );
    }
    assert!(w["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert!(w["effect"].is_string(), "node-converge declares its effect");
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-node-converge");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_eq!(w["argv"], json!([SCRIPT, "{1}", "{2}", "{3}"]));
    assert!(!p["about"].as_str().unwrap_or_default().contains("MUTATING"));
    assert_eq!(p["argv"], json!([SCRIPT, "--plan", "{1}", "{2}"]));
}
