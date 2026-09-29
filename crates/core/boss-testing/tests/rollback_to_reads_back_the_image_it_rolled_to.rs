//! `rollback-to <sha>` (`infra/forge/rollback-to.sh`, the ops verb of the
//! same name) proves its EFFECT, not only its exit: after `kubectl rollout
//! status` it reads deploy/boss's template image back and requires it to
//! be REGISTRY:sha, then reads the pods of the deployment's current
//! ReplicaSet and requires at least one Ready pod and EVERY Ready pod of
//! deploy/boss to run that image. Only then does it print the line its
//! verb file declares as `effect` (backlog 1058e686, car A).
//!
//! WHY. Until this car the verb's only proof was `rollout status`, which
//! reads the rollout's PROGRESS, never the image: a patch that changed
//! nothing (a wrong sha that is somehow already the template, a patch
//! that answered success and wrote nothing) passes rollout status at
//! once, and the verb printed "deploy/boss is Ready on REGISTRY:sha"
//! without ever reading REGISTRY:sha back. It was one of the six MUTATING
//! verbs the effect-proof car (fdbb447e part 1) admitted with
//! `effect_unread`, and the triage put it FIRST because it is a DR verb.
//!
//! THE DR RULE. The read-back only REPORTS. The patch and the rollout
//! wait run exactly as before — same order, same arguments, same refusal
//! when the cluster cannot be read, same "never went Ready" exit — and
//! the read-back runs only after both succeeded, so no answer it gets can
//! stop or undo a rollback. A read-back that disagrees or cannot be read
//! exits nonzero AFTER the rollback was applied, and says that it was.
//!
//! HOW THIS IS MEASURED. The script runs for real with `sudo` stubbed on
//! PATH as a small stateful cluster: the patch writes the image, the
//! revision advances, and the ReplicaSet/Pod listing is generated from
//! that state — with switches for a patch that writes nothing, a read
//! that fails, a current ReplicaSet with no Ready pod, a Ready pod on the
//! wrong image, and an old ReplicaSet's pod still Ready. Nothing here
//! reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/rollback-to.sh";
const VERB: &str = "infra/ops/verbs/rollback-to.json";
const REG: &str = "registry.example/boss";

/// A stateful `sudo docker run … kubectl --kubeconfig=/kc <args>`. Every
/// call's argv lands in $STUB_ARGV, one line each, in order.
const STUB: &str = r#"#!/bin/sh
# ops_image_ready's housekeeping (the image present, its pin made) is
# answered before the argv log, which records only kubectl's calls.
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
while [ $# -gt 0 ] && [ "$1" != "--kubeconfig=/kc" ]; do shift; done
shift
S="$STUB_STATE"
rev=$(cat "$S/rev")
img=$(cat "$S/image")
case "$1 $2" in
"get deploy")
    case "$*" in
    *"-o jsonpath"*)
        printf '%s' "$img" ;;
    *"-o json"*)
        if [ "${STUB_READBACK:-ok}" = fail ]; then
            echo 'The connection to the server 10.0.0.1:6443 was refused' >&2
            exit 1
        fi
        [ "${STUB_READBACK:-ok}" = empty ] && exit 0
        printf '{"kind":"Deployment","metadata":{"name":"boss","annotations":{"deployment.kubernetes.io/revision":"%s"}},"spec":{"template":{"spec":{"containers":[{"name":"boss","image":"%s"}]}}}}\n' "$rev" "$img" ;;
    *) echo "stub: unexpected get: $*" >&2; exit 2 ;;
    esac ;;
"patch deploy")
    new=$(printf '%s' "$*" | sed 's/.*"value":"\([^"]*\)".*/\1/')
    if [ "${STUB_PATCH:-apply}" = apply ] && [ "$new" != "$img" ]; then
        printf '%s\n' "$new" > "$S/image"
        expr "$rev" + 1 > "$S/rev"
    fi
    echo 'deployment.apps/boss patched' ;;
"rollout status")
    exit "${STUB_ROLLOUT:-0}" ;;
"get rs,pods")
    if [ "${STUB_PODS_READ:-ok}" = fail ]; then
        echo 'Unable to connect to the server: net/http: request canceled' >&2
        exit 1
    fi
    [ "${STUB_PODS_READ:-ok}" = empty ] && exit 0
    ready="${STUB_READY:-True}"
    podimg="${STUB_POD_IMAGE:-$img}"
    old=''
    if [ "${STUB_OLD_POD:-0}" = 1 ]; then
        old=',{"kind":"ReplicaSet","metadata":{"name":"boss-r0","annotations":{"deployment.kubernetes.io/revision":"0"},"ownerReferences":[{"kind":"Deployment","name":"boss"}]}},{"kind":"Pod","metadata":{"name":"boss-r0-old","ownerReferences":[{"kind":"ReplicaSet","name":"boss-r0"}]},"spec":{"containers":[{"name":"boss","image":"registry.example/boss:0ld0000"}]},"status":{"conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"boss","imageID":"sha256:0ld"}]}}'
    fi
    printf '{"kind":"List","items":[{"kind":"ReplicaSet","metadata":{"name":"boss-r%s","annotations":{"deployment.kubernetes.io/revision":"%s"},"ownerReferences":[{"kind":"Deployment","name":"boss"}]}},{"kind":"Pod","metadata":{"name":"boss-r%s-a","ownerReferences":[{"kind":"ReplicaSet","name":"boss-r%s"}]},"spec":{"containers":[{"name":"boss","image":"%s"}]},"status":{"conditions":[{"type":"Ready","status":"%s"}],"containerStatuses":[{"name":"boss","imageID":"sha256:feed"}]}},{"kind":"Pod","metadata":{"name":"chore-x","ownerReferences":[{"kind":"Job","name":"chore"}]},"spec":{"containers":[{"name":"c","image":"elsewhere/chore:1"}]},"status":{"conditions":[{"type":"Ready","status":"True"}]}}%s]}\n' "$rev" "$rev" "$rev" "$rev" "$podimg" "$ready" "$old" ;;
*)
    echo "stub: unexpected kubectl call: $*" >&2
    exit 2 ;;
esac
"#;

struct Cluster {
    dir: PathBuf,
}

impl Cluster {
    /// A cluster serving REG:`serving`, at revision 4.
    fn new(name: &str, serving: &str) -> Self {
        let dir = scratch_dir(name);
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&bin.join("sudo"), STUB);
        write_file(&dir.join("state/image"), &format!("{REG}:{serving}\n"));
        write_file(&dir.join("state/rev"), "4\n");
        Cluster { dir }
    }

    fn run(&self, sha: &str, env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .arg(sha)
            .env("PATH", path)
            .env("HOME", &self.dir)
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("REGISTRY", REG)
            // The mirrored kubectl image is spelled from the registry
            // host /etc/boss/sor.env carries (backlog cf321ffd).
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("rollback-to runs")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                l.split_once("--kubeconfig=/kc ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .collect()
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn verb() -> serde_json::Value {
    let body = std::fs::read_to_string(repo_root().join(VERB)).expect("verb file");
    serde_json::from_str(&body).expect("verb json")
}

/// The lines of `out` the verb's declared `effect` matches, judged by the
/// runner's own engine (jq's `test`). `None` when jq is absent.
fn effect_lines(out: &str) -> Option<Vec<String>> {
    Command::new("jq").arg("--version").output().ok()?;
    let spec = verb();
    let re = spec["effect"]
        .as_str()
        .expect("rollback-to declares an `effect`")
        .to_string();
    let hits = out
        .lines()
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
        .collect();
    Some(hits)
}

fn patched(c: &Cluster) -> bool {
    c.calls()
        .iter()
        .any(|l| l.starts_with("patch deploy boss "))
}

#[test]
fn a_rollback_prints_its_effect_only_after_reading_the_image_back() {
    let c = Cluster::new("rollback-reads-back", "bad0000");
    let o = c.run("abc1234", &[]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(
        t.contains(&format!(
            "rollback-to: rolled to {REG}:abc1234 — read back: the template and 1 Ready pod(s) of replicaset boss-r5 run it (was {REG}:bad0000)"
        )),
        "{t}"
    );
    // The order is the whole point: act, wait, THEN read back — the
    // template first, the pods second, and nothing else.
    let calls = c.calls();
    let shape: Vec<&str> = calls
        .iter()
        .map(|l| {
            let mut w = l.split_whitespace();
            match (w.next(), w.next()) {
                (Some("get"), Some("deploy")) if l.contains("-o jsonpath") => "read-before",
                (Some("patch"), _) => "patch",
                (Some("rollout"), _) => "rollout-status",
                (Some("get"), Some("deploy")) => "read-template",
                (Some("get"), Some("rs,pods")) => "read-pods",
                _ => "other",
            }
        })
        .collect();
    assert_eq!(
        shape,
        [
            "read-before",
            "patch",
            "rollout-status",
            "read-template",
            "read-pods"
        ],
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|l| l.starts_with("get rs,pods -n boss -l app=boss -o json")),
        "{calls:?}"
    );
    if let Some(hits) = effect_lines(&t) {
        assert_eq!(
            hits.len(),
            1,
            "exactly the read-back line is the effect: {t}"
        );
        assert!(hits[0].starts_with("rollback-to: rolled to "), "{hits:?}");
    }
}

#[test]
fn a_rollback_onto_the_image_already_served_still_rolls_and_says_already_on() {
    let c = Cluster::new("rollback-already-on", "abc1234");
    let o = c.run("abc1234", &[]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    // Rolls exactly as before: the patch and the wait still run.
    assert!(patched(&c), "{:?}", c.calls());
    assert!(
        c.calls().iter().any(|l| l.starts_with("rollout status ")),
        "{:?}",
        c.calls()
    );
    assert!(
        t.contains(&format!(
            "rollback-to: already on {REG}:abc1234 — read back: the template and 1 Ready pod(s) of replicaset boss-r4 run it"
        )),
        "{t}"
    );
    assert!(!t.contains("rollback-to: rolled to "), "{t}");
    if let Some(hits) = effect_lines(&t) {
        assert_eq!(hits.len(), 1, "{t}");
        assert!(hits[0].starts_with("rollback-to: already on "), "{hits:?}");
    }
}

/// Each case: the rollback WAS applied (patch and rollout status ran), the
/// read-back refused to vouch for it, the run exits 1 and saying why, and
/// no line of its output is the declared effect.
fn applied_but_not_proven(name: &str, env: &[(&str, &str)], says: &[&str]) {
    let c = Cluster::new(name, "bad0000");
    let o = c.run("abc1234", env);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{name}: {t}");
    assert!(
        patched(&c),
        "{name}: the rollback is applied first: {:?}",
        c.calls()
    );
    assert!(
        t.contains(&format!("the rollback to {REG}:abc1234 was applied")),
        "{name}: a failed read-back says the rollback happened: {t}"
    );
    for s in says {
        assert!(t.contains(s), "{name}: expected {s:?} in {t}");
    }
    if let Some(hits) = effect_lines(&t) {
        assert!(
            hits.is_empty(),
            "{name}: no effect was proven: {hits:?}\n{t}"
        );
    }
}

#[test]
fn a_patch_that_changed_nothing_passes_rollout_status_and_fails_the_read_back() {
    applied_but_not_proven(
        "rollback-patch-noop",
        &[("STUB_PATCH", "noop")],
        &[
            "rollback-to: FAILED — ",
            &format!("deploy/boss's template reads back {REG}:bad0000"),
        ],
    );
}

#[test]
fn a_template_that_cannot_be_read_back_is_not_proof() {
    applied_but_not_proven(
        "rollback-template-unread",
        &[("STUB_READBACK", "fail")],
        &["rollback-to: CANNOT ANSWER — ", "was refused"],
    );
}

#[test]
fn an_empty_answer_is_not_a_read_back() {
    // jq-1.6 exits 0 over no document at all (backlog d96e38ab): a get
    // that answers success with nothing on stdout must not read as a
    // template, nor as a list with no pods to disagree.
    applied_but_not_proven(
        "rollback-template-empty",
        &[("STUB_READBACK", "empty")],
        &["rollback-to: CANNOT ANSWER — "],
    );
    applied_but_not_proven(
        "rollback-pods-empty",
        &[("STUB_PODS_READ", "empty")],
        &[
            "rollback-to: CANNOT ANSWER — ",
            "the pods of deploy/boss could not be listed",
        ],
    );
}

#[test]
fn pods_that_cannot_be_listed_are_not_proof() {
    applied_but_not_proven(
        "rollback-pods-unread",
        &[("STUB_PODS_READ", "fail")],
        &["rollback-to: CANNOT ANSWER — ", "request canceled"],
    );
}

#[test]
fn a_current_replicaset_with_no_ready_pod_is_not_proof() {
    // "every Ready pod runs it" is vacuously true of none.
    applied_but_not_proven(
        "rollback-no-ready-pod",
        &[("STUB_READY", "False")],
        &["rollback-to: FAILED — ", "no Ready pod"],
    );
}

#[test]
fn a_ready_pod_on_another_image_fails_the_read_back() {
    applied_but_not_proven(
        "rollback-pod-wrong-image",
        &[("STUB_POD_IMAGE", "registry.example/boss:e1se000")],
        &[
            "rollback-to: FAILED — ",
            "boss-r5-a (registry.example/boss:e1se000)",
        ],
    );
}

#[test]
fn an_old_replicaset_still_serving_fails_the_read_back() {
    applied_but_not_proven(
        "rollback-old-pod-ready",
        &[("STUB_OLD_POD", "1")],
        &[
            "rollback-to: FAILED — ",
            "boss-r0-old (registry.example/boss:0ld0000)",
        ],
    );
}

#[test]
fn a_rollout_that_never_goes_ready_fails_as_before_and_reads_nothing_back() {
    let c = Cluster::new("rollback-never-ready", "bad0000");
    let o = c.run("abc1234", &[("STUB_ROLLOUT", "1")]);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains(&format!(
            "rollback-to: {REG}:abc1234 never went Ready — deploy/boss is NOT restored; hands needed"
        )),
        "{t}"
    );
    let calls = c.calls();
    assert!(
        !calls
            .iter()
            .any(|l| l.contains(" -o json ") || l.starts_with("get rs,pods")),
        "{calls:?}"
    );
    if let Some(hits) = effect_lines(&t) {
        assert!(hits.is_empty(), "{hits:?}");
    }
}

#[test]
fn the_verb_outlasts_its_own_rollout_wait() {
    // rollout status waits up to 420 s, and the runner kills a verb that
    // declares no `timeout` at 30 s: through the ops door the effect line
    // could never print for a rollout slower than that. The declared
    // timeout must cover the wait, the patch and three reads.
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("script");
    assert!(script.contains("--timeout=420s"), "the rollout wait moved");
    let timeout = verb()["timeout"].as_u64().unwrap_or(0);
    assert!(
        timeout >= 420 + 120,
        "rollback-to declares timeout {timeout}, which does not outlast its 420 s rollout wait plus its reads"
    );
}
