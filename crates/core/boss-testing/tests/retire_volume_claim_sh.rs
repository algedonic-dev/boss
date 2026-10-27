//! `infra/forge/retire-volume-claim.sh` — a PersistentVolumeClaim is
//! retired through a plan David signs with his passkey, never through an
//! admin kubeconfig typed at a workstation (backlog f36eb7ce, design
//! b4b1c99d).
//!
//! WHY. Retiring two superseded claims (gate-runner-disk, gate-seed) on
//! 2026-10-10 needed David to run kubectl by hand: the dev session cannot
//! delete a claim, and `delete-orphan-object` withholds
//! PersistentVolumeClaim from its kind floor on purpose ("bytes on
//! Longhorn ... stay a named human step"). David, 2026-10-18 (the
//! design-review answer on f36eb7ce): that step must NOT stay a human
//! command line — a claim is retired by a passkey-approved ops verb, plan
//! on the approve step, re-checked at execute, PVC and PV captured before
//! the delete, one claim per request, referenced and protected claims
//! refused.
//!
//! WHAT EACH CASE PINS, in the order the script applies its bounds: the
//! argument shape; the protected names; a committed manifest tree; the
//! derivation's verdict (`undeclared-objects.sh --check`, the one
//! definition of "a manifest declares it", which also refuses a
//! namespace the tree does not own and a controller-owned claim); the
//! claim's own facts (not being deleted, no owner); its volume (bound to
//! THIS claim, a reclaim this verb can prove); Longhorn not attached; no
//! consumer in the namespace; and for the write, the signed hash, the
//! capture landing on the request BEFORE the delete, a uid-preconditioned
//! delete, and the read-back of claim, volume and bytes.
//!
//! THE FIXTURE. A manifest tree that is a real git repository (JSON in
//! .yaml files, which `kubectl create --dry-run -o json -f` echoes back,
//! the trick `delete_orphan_object_sh.rs` uses), a stub `kubectl` on
//! `BOSS_KUBECTL` whose cluster is a directory of JSON objects, and a
//! stub `boss` on `BOSS_BIN` that records each attach. The stub refuses a
//! delete without a uid precondition, so a delete that lost its body
//! fails here rather than passing as a bare delete. Nothing here touches
//! a cluster or the system of record.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const NS: &str = "boss-dev";
const CLAIM: &str = "gate-runner-disk";
const CLAIM_UID: &str = "6f1e9b99-0000-4000-8000-0000000000c1";
const PV: &str = "pvc-11111111-1111-4111-8111-111111111111";
const PV_UID: &str = "6f1e9b99-0000-4000-8000-0000000000d1";
const LH_UID: &str = "6f1e9b99-0000-4000-8000-0000000000f1";
const REQUEST: &str = "0e0e0e0e-0000-4000-8000-0000000000aa";

fn script() -> PathBuf {
    repo_root().join("infra/forge/retire-volume-claim.sh")
}

/// The stand-in cluster. `objects/<resource>/<ns or _>/<name>.json` is
/// one object; `deny-<resource>` makes that resource Forbidden;
/// `terminating` makes a claim delete leave the claim held (the
/// pvc-protection finalizer waiting on a consumer); `pv-sticks` and
/// `lh-sticks` keep the volume or the Longhorn volume after the claim
/// goes; `pv-replaced` (holding a uid) leaves the volume's NAME held by
/// that uid instead; `answer-<resource>` is what a successful get of a
/// missing object of that resource prints, verbatim (an empty file is a
/// get that exits 0 with no output). `calls` logs every invocation,
/// `deleted` every delete the precondition admitted.
const KUBECTL: &str = r#"#!/usr/bin/env bash
S="${KSTATE:?}"
echo "kubectl $*" >> "$S/calls"
ns=""; raw=""; file=""; fmt=""; sel=""; words=()
while [ $# -gt 0 ]; do
    case "$1" in
        -n) ns="$2"; shift 2 ;;
        --raw) raw="$2"; shift 2 ;;
        -f) file="$2"; shift 2 ;;
        -o) fmt="$2"; shift 2 ;;
        -l) sel="$2"; shift 2 ;;
        --*) shift ;;
        *) words+=("$1"); shift ;;
    esac
done
O="$S/objects"
dir_of() { printf '%s/%s/%s' "$O" "$1" "${2:-_}"; }
case "${words[0]:-}" in
version) echo '{}'; exit 0 ;;
create) cat "$file"; exit 0 ;;
get)
    res="${words[1]}"; name="${words[2]:-}"
    for r in ${res//,/ }; do
        if [ -e "$S/deny-$r" ]; then
            echo "Error from server (Forbidden): $r is forbidden: User \"ops\" cannot list resource \"$r\" in the namespace \"$ns\"" >&2
            exit 1
        fi
    done
    if [ -n "$name" ]; then
        f="$(dir_of "$res" "$ns")/$name.json"
        if [ ! -f "$f" ] && [ -e "$S/answer-$res" ]; then cat "$S/answer-$res"; exit 0; fi
        [ -f "$f" ] || { echo "Error from server (NotFound): $res \"$name\" not found" >&2; exit 1; }
        case "$fmt" in
            json) cat "$f" ;;
            yaml) printf '# %s %s/%s, as yaml\n' "$res" "${ns:-_}" "$name"; cat "$f" ;;
            *labels*) jq -j '.metadata.labels["app.kubernetes.io/part-of"] // ""' "$f" ;;
            *ownerReferences*) jq -j '.metadata.ownerReferences[0].kind // ""' "$f" ;;
            *) echo "stub: unmodelled get format $fmt" >&2; exit 64 ;;
        esac
        exit 0
    fi
    items='[]'
    for r in ${res//,/ }; do
        d="$(dir_of "$r" "$ns")"
        [ -d "$d" ] || continue
        for f in "$d"/*.json; do
            [ -f "$f" ] || continue
            items=$(jq -c --slurpfile o "$f" '. + $o' <<< "$items")
        done
    done
    if [ -n "$sel" ]; then
        k="${sel%%=*}"; v="${sel#*=}"
        items=$(jq -c --arg k "$k" --arg v "$v" 'map(select(.metadata.labels[$k] == $v))' <<< "$items")
    fi
    case "$fmt" in
        json) jq '{apiVersion: "v1", kind: "List", items: .}' <<< "$items" ;;
        *ownerReferences*) jq -r '.[] | "\(.metadata.name)\t\(.metadata.ownerReferences[0].kind // "")"' <<< "$items" ;;
        jsonpath=*) jq -r '.[] | .metadata.name' <<< "$items" ;;
        *) echo "stub: unmodelled list format $fmt" >&2; exit 64 ;;
    esac
    exit 0 ;;
delete)
    [ -n "$raw" ] || { echo "stub: only a --raw delete is modelled: $*" >&2; exit 64; }
    dns=$(cut -d/ -f5 <<< "$raw"); dres=$(cut -d/ -f6 <<< "$raw"); dname=$(cut -d/ -f7 <<< "$raw")
    [ "$dres" = persistentvolumeclaims ] || { echo "stub: a delete of something other than a claim: $raw" >&2; exit 64; }
    f="$(dir_of PersistentVolumeClaim "$dns")/$dname.json"
    body=$(cat)
    want=$(jq -r '.preconditions.uid // empty' <<< "$body" 2>/dev/null)
    [ -n "$want" ] || { echo "stub: a delete without a uid precondition: $body" >&2; exit 1; }
    [ -f "$f" ] || { echo "Error from server (NotFound): persistentvolumeclaims \"$dname\" not found" >&2; exit 1; }
    have=$(jq -r '.metadata.uid' "$f")
    if [ "$have" != "$want" ]; then
        echo "Error from server (Conflict): Operation cannot be fulfilled on PersistentVolumeClaim \"$dname\": Precondition failed: UID in precondition: $want, UID in object meta: $have" >&2
        exit 1
    fi
    echo "$dns/$dname $want" >> "$S/deleted"
    if [ -e "$S/terminating" ]; then
        jq '.metadata.deletionTimestamp = "2026-10-18T12:00:00Z"' "$f" > "$f.new" && mv "$f.new" "$f"
    else
        vol=$(jq -r '.spec.volumeName // ""' "$f")
        rm -f "$f"
        pvf="$(dir_of PersistentVolume)/$vol.json"
        if [ -n "$vol" ] && [ -f "$pvf" ]; then
            if [ "$(jq -r '.spec.persistentVolumeReclaimPolicy' "$pvf")" = Delete ]; then
                h=$(jq -r '.spec.csi.volumeHandle // ""' "$pvf")
                if [ -e "$S/pv-replaced" ]; then
                    jq --arg u "$(cat "$S/pv-replaced")" '.metadata.uid = $u' "$pvf" > "$pvf.new" && mv "$pvf.new" "$pvf"
                else
                    [ -e "$S/pv-sticks" ] || rm -f "$pvf"
                fi
                [ -e "$S/lh-sticks" ] || rm -f "$(dir_of volumes.longhorn.io longhorn-system)/$h.json"
            else
                jq '.status.phase = "Released"' "$pvf" > "$pvf.new" && mv "$pvf.new" "$pvf"
            fi
        fi
    fi
    jq -n --arg n "$dname" '{kind: "PersistentVolumeClaim", metadata: {name: $n}}'
    exit 0 ;;
esac
echo "stub: unmodelled call: ${words[*]}" >&2
exit 64
"#;

/// The stand-in `boss`: only `attach` is modelled, and it prints the
/// real verb's two receipt lines (attach.rs) over the bytes it was given.
/// `attach-fails` makes the file store refuse; `attach-unconfirmed` exits
/// 0 with the first line only, no byte-identical read-back.
const BOSS: &str = r#"#!/usr/bin/env bash
S="${KSTATE:?}"
echo "boss $*" >> "$S/calls"
[ "${1:-}" = attach ] || { echo "stub boss: only attach is modelled: $*" >&2; exit 64; }
[ -e "$S/attach-fails" ] && { echo "boss attach: POST /api/files answered HTTP 503" >&2; exit 1; }
f="$3"
cp "$f" "$S/attached"
sha=$(sha256sum "$f" | cut -c1-64); n=$(wc -c < "$f" | tr -d ' ')
if [ -e "$S/attach-unconfirmed" ]; then
    printf 'boss attach: %s (%s bytes, sha256 %s) attached to packet %s as file 0f0f0f0f-0000-4000-8000-00000000000a, uploaded by the ops runner\n' \
        "$(basename "$f")" "$n" "$sha" "${2:0:8}"
    exit 0
fi
printf 'boss attach: %s (%s bytes, sha256 %s) attached to packet %s as file 0f0f0f0f-0000-4000-8000-00000000000a, uploaded by the ops runner\nconfirmed: read back from http://content/api/files/0f0f0f0f-0000-4000-8000-00000000000a byte-identical\n' \
    "$(basename "$f")" "$n" "$sha" "${2:0:8}"
"#;

struct Fixture {
    tree: PathBuf,
    state: PathBuf,
    kubectl: PathBuf,
    boss: PathBuf,
}

fn write_file(path: &Path, body: &str) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
    }
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn doc(kind: &str, ns: &str, name: &str) -> String {
    if ns.is_empty() {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}"}}}}"#)
    } else {
        format!(r#"{{"kind":"{kind}","metadata":{{"name":"{name}","namespace":"{ns}"}}}}"#)
    }
}

/// The tree owns `boss` and `boss-dev`, declares the dev work claim and
/// the database's StatefulSet (whose volumeClaimTemplate declares
/// pgdata-postgres-0), and carries enough ConfigMaps to clear the
/// derivation's "the scrape broke" floors (10 files, 20 objects) rather
/// than relaxing them for a fixture.
fn fixture(case: &str) -> Fixture {
    let root = scratch_dir(&format!("retire-volume-claim-{case}"));
    let tree = root.join("tree");
    let dir = tree.join("infra/cluster/manifests");
    write_file(
        &dir.join("namespaces.yaml"),
        &format!(
            "{}\n{}\n",
            doc("Namespace", "", "boss"),
            doc("Namespace", "", "boss-dev")
        ),
    );
    write_file(
        &dir.join("dev-work.yaml"),
        &format!(
            "{}\n{}\n",
            doc("PersistentVolumeClaim", "boss-dev", "boss-dev-work"),
            doc("PersistentVolumeClaim", "boss-dev", "build-cache")
        ),
    );
    write_file(
        &dir.join("postgres.yaml"),
        r#"{"kind":"StatefulSet","metadata":{"name":"postgres","namespace":"boss"},
 "spec":{"volumeClaimTemplates":[{"metadata":{"name":"pgdata"}}]}}
"#,
    );
    for i in 0..10 {
        write_file(
            &dir.join(format!("cm-{i}.yaml")),
            &format!(
                "{}\n{}\n",
                doc("ConfigMap", "boss", &format!("cm-{i}-a")),
                doc("ConfigMap", "boss-dev", &format!("cm-{i}-b"))
            ),
        );
    }
    git(&tree, &["init", "-q", "-b", "main"]);
    git(&tree, &["add", "-A"]);
    git(
        &tree,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"],
    );
    let state = root.join("cluster");
    std::fs::create_dir_all(state.join("objects")).unwrap();
    let kubectl = root.join("kubectl-stub");
    write_exec(&kubectl, KUBECTL);
    let boss = root.join("boss-stub");
    write_exec(&boss, BOSS);
    let f = Fixture {
        tree,
        state,
        kubectl,
        boss,
    };
    ordinary(&f);
    f
}

fn put(f: &Fixture, res: &str, ns: &str, name: &str, v: &Value) {
    let ns = if ns.is_empty() { "_" } else { ns };
    write_file(
        &f.state
            .join("objects")
            .join(res)
            .join(ns)
            .join(format!("{name}.json")),
        &serde_json::to_string(v).unwrap(),
    );
}

fn object(f: &Fixture, res: &str, ns: &str, name: &str) -> Option<Value> {
    let ns = if ns.is_empty() { "_" } else { ns };
    let p = f
        .state
        .join("objects")
        .join(res)
        .join(ns)
        .join(format!("{name}.json"));
    std::fs::read_to_string(p)
        .ok()
        .map(|s| serde_json::from_str(&s).unwrap())
}

fn claim(name: &str, uid: &str, volume: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolumeClaim",
        "metadata": {
            "name": name, "namespace": NS, "uid": uid,
            "creationTimestamp": "2026-09-20T10:00:00Z",
            "labels": {"app": "gate-runner"},
            "finalizers": ["kubernetes.io/pvc-protection"],
            "resourceVersion": "4711"
        },
        "spec": {
            "accessModes": ["ReadWriteOnce"],
            "storageClassName": "longhorn",
            "resources": {"requests": {"storage": "150Gi"}},
            "volumeName": volume
        },
        "status": {"phase": "Bound", "capacity": {"storage": "150Gi"}}
    })
}

fn volume(policy: &str) -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "PersistentVolume",
        "metadata": {"name": PV, "uid": PV_UID, "creationTimestamp": "2026-09-20T10:00:05Z"},
        "spec": {
            "capacity": {"storage": "150Gi"},
            "storageClassName": "longhorn",
            "persistentVolumeReclaimPolicy": policy,
            "claimRef": {"namespace": NS, "name": CLAIM, "uid": CLAIM_UID},
            "csi": {"driver": "driver.longhorn.io", "volumeHandle": PV}
        },
        "status": {"phase": "Bound"}
    })
}

fn pod_mounting(name: &str, claim: &str, phase: &str) -> Value {
    json!({
        "kind": "Pod",
        "metadata": {"name": name, "namespace": NS, "uid": format!("u-{name}")},
        "spec": {"volumes": [{"name": "work", "persistentVolumeClaim": {"claimName": claim}}]},
        "status": {"phase": phase}
    })
}

/// The ordinary cluster: the claim to retire, bound to a Longhorn volume
/// with reclaim Delete and detached; the declared dev work claim, which a
/// running pod mounts; and a Job whose template mounts a DIFFERENT claim
/// — so the consumer scan has something to read past.
fn ordinary(f: &Fixture) {
    put(
        f,
        "PersistentVolumeClaim",
        NS,
        CLAIM,
        &claim(CLAIM, CLAIM_UID, PV),
    );
    put(f, "PersistentVolume", "", PV, &volume("Delete"));
    put(
        f,
        "volumes.longhorn.io",
        "longhorn-system",
        PV,
        &json!({
            "kind": "Volume",
            "metadata": {"name": PV, "namespace": "longhorn-system", "uid": LH_UID},
            "spec": {"size": "161061273600"},
            "status": {"state": "detached", "currentNodeID": ""}
        }),
    );
    let mut dev_work = claim(
        "boss-dev-work",
        "u-dev-work",
        "pvc-22222222-2222-4222-8222-222222222222",
    );
    dev_work["metadata"]["labels"] = json!({"app.kubernetes.io/part-of": "boss"});
    put(f, "PersistentVolumeClaim", NS, "boss-dev-work", &dev_work);
    put(
        f,
        "pods",
        NS,
        "boss-dev-0",
        &pod_mounting("boss-dev-0", "boss-dev-work", "Running"),
    );
    put(
        f,
        "jobs",
        NS,
        "gate-run-a",
        &json!({
            "kind": "Job",
            "metadata": {"name": "gate-run-a", "namespace": NS},
            "spec": {"template": {"spec": {"volumes": [
                {"name": "seed", "persistentVolumeClaim": {"claimName": "gate-seed"}}
            ]}}}
        }),
    );
}

fn touch(f: &Fixture, name: &str) {
    write_file(&f.state.join(name), "");
}

/// Run the script. `env` overrides: `Some` sets, `None` removes.
fn run_with(f: &Fixture, args: &[&str], env: &[(&str, Option<&str>)]) -> (i32, String, String) {
    let mut c = Command::new("bash");
    c.arg(script())
        .args(args)
        .env("BOSS_CLUSTER_TREE", &f.tree)
        .env("BOSS_KUBECTL", &f.kubectl)
        .env("BOSS_BIN", &f.boss)
        .env("KSTATE", &f.state)
        .env("OPS_REQUEST_ID", REQUEST)
        .env("BOSS_RETIRE_CLAIM_WAIT_S", "2")
        .env_remove("BOSS_OPS_CHECKOUT")
        .env_remove("KUBECONFIG");
    for (k, v) in env {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    let out = c.output().expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run(f: &Fixture, args: &[&str]) -> (i32, String, String) {
    run_with(f, args, &[])
}

fn plan(f: &Fixture) -> (String, String) {
    let (code, out, err) = run(f, &["--plan", NS, CLAIM]);
    assert_eq!(code, 0, "the plan renders:\n{err}\n{out}");
    let said = err
        .lines()
        .find_map(|l| l.strip_prefix("plan-sha256: "))
        .unwrap_or_else(|| panic!("the plan names its hash on stderr:\n{err}"))
        .to_string();
    (out, said)
}

fn sha256(bytes: &str, f: &Fixture) -> String {
    let p = f.state.join("hash-me");
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("sha256sum").arg(&p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

fn calls(f: &Fixture) -> String {
    std::fs::read_to_string(f.state.join("calls")).unwrap_or_default()
}

fn deleted(f: &Fixture) -> String {
    std::fs::read_to_string(f.state.join("deleted")).unwrap_or_default()
}

fn verb(name: &str) -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

/// The lines of `out` the shipped verb's declared `effect` matches — how
/// the ops runner judges a run (ops-runner.sh, `effect`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("retire-volume-claim")["effect"]
        .as_str()
        .expect("retire-volume-claim declares an effect")
        .to_string();
    let re = regex::Regex::new(&re).expect("the effect compiles");
    out.lines()
        .filter(|l| re.is_match(l))
        .map(str::to_string)
        .collect()
}

fn refused_without_a_write(f: &Fixture, code: i32, err: &str, words: &str) {
    assert_eq!(code, 78, "a refusal exits 78:\n{err}");
    assert!(err.contains("REFUSED"), "the refusal says so:\n{err}");
    assert!(err.contains(words), "the refusal names {words:?}:\n{err}");
    assert_eq!(deleted(f), "", "a refusal deletes nothing");
    assert!(
        !calls(f).contains("boss attach"),
        "a refusal attaches nothing"
    );
}

#[test]
fn the_plan_names_the_claim_its_volume_and_no_consumer_and_deletes_nothing() {
    let f = fixture("plan");
    let (out, said) = plan(&f);
    assert_eq!(
        said,
        sha256(&out, &f),
        "plan-sha256 is the hash of the bytes on stdout"
    );
    for want in [
        "plan: retire-volume-claim",
        &format!("claim: {NS}/{CLAIM}"),
        &format!("  uid: {CLAIM_UID}"),
        "  created: 2026-09-20T10:00:00Z",
        "  requested: 150Gi",
        "  storage class: longhorn",
        r#"  labels: {"app":"gate-runner"}"#,
        "  owner references: none",
        &format!("volume: {PV}"),
        "  reclaim policy: Delete",
        &format!("  source: csi driver.longhorn.io {PV}"),
        &format!("longhorn volume: longhorn-system/{PV}"),
        "  state: detached",
        "consumers: none",
        &format!(
            "DELETE /api/v1/namespaces/{NS}/persistentvolumeclaims/{CLAIM} with preconditions.uid={CLAIM_UID}"
        ),
    ] {
        assert!(out.contains(want), "the plan carries {want:?}:\n{out}");
    }
    assert!(
        !out.contains("4711"),
        "no resourceVersion in the signed bytes:\n{out}"
    );
    let (again, _) = plan(&f);
    assert_eq!(out, again, "two renders of one state are byte-identical");
    assert_eq!(deleted(&f), "", "--plan deletes nothing");
    assert!(
        !calls(&f).contains(" delete "),
        "--plan never reaches a delete:\n{}",
        calls(&f)
    );
    assert!(
        !calls(&f).contains("boss attach"),
        "--plan attaches nothing"
    );
}

#[test]
fn a_claim_a_manifest_declares_is_refused() {
    let f = fixture("declared");
    put(
        &f,
        "PersistentVolumeClaim",
        NS,
        "build-cache",
        &claim("build-cache", "u-cache", ""),
    );
    let (code, _, err) = run(&f, &["--plan", NS, "build-cache"]);
    refused_without_a_write(&f, code, &err, "DECLARES");
}

#[test]
fn a_protected_claim_is_refused_even_where_nothing_declares_it() {
    let f = fixture("protected");
    // pgdata-postgres-0 live in boss-dev, where no StatefulSet declares
    // it: the name alone refuses, before the derivation is asked.
    put(
        &f,
        "PersistentVolumeClaim",
        NS,
        "pgdata-postgres-0",
        &claim("pgdata-postgres-0", "u-pg", ""),
    );
    let (code, _, err) = run(&f, &["--plan", NS, "pgdata-postgres-0"]);
    refused_without_a_write(&f, code, &err, "protected");
}

#[test]
fn a_namespace_the_tree_does_not_own_is_refused() {
    let f = fixture("namespace");
    let (code, _, err) = run(&f, &["--plan", "longhorn-system", CLAIM]);
    refused_without_a_write(&f, code, &err, "does not own namespace");
}

#[test]
fn a_claim_a_controller_owns_is_refused() {
    // The dev-build worker claims carry an ownerReference to their Job
    // (infra/dev/dev-build.py); the Job's deletion releases them, so the
    // claim is not this verb's to retire.
    let f = fixture("owned");
    let mut c = claim(CLAIM, CLAIM_UID, PV);
    c["metadata"]["ownerReferences"] =
        json!([{"apiVersion": "batch/v1", "kind": "Job", "name": "dev-build-x", "uid": "u-job"}]);
    put(&f, "PersistentVolumeClaim", NS, CLAIM, &c);
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "controller owns it");
}

#[test]
fn a_claim_a_pod_mounts_is_refused_whatever_its_phase() {
    for phase in ["Running", "Failed"] {
        let f = fixture(&format!("pod-{phase}"));
        put(
            &f,
            "pods",
            NS,
            "worker-0",
            &pod_mounting("worker-0", CLAIM, phase),
        );
        let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
        refused_without_a_write(&f, code, &err, "Pod boss-dev/worker-0");
    }
}

#[test]
fn a_claim_a_workload_template_mounts_is_refused() {
    let f = fixture("job-template");
    put(
        &f,
        "cronjobs",
        NS,
        "nightly",
        &json!({
            "kind": "CronJob",
            "metadata": {"name": "nightly", "namespace": NS},
            "spec": {"jobTemplate": {"spec": {"template": {"spec": {"volumes": [
                {"name": "w", "persistentVolumeClaim": {"claimName": CLAIM}}
            ]}}}}}
        }),
    );
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "CronJob boss-dev/nightly");
}

#[test]
fn a_claim_a_live_statefulset_would_recreate_is_refused() {
    let f = fixture("sts");
    put(
        &f,
        "PersistentVolumeClaim",
        NS,
        "data-cache-0",
        &claim("data-cache-0", "u-cache", ""),
    );
    put(
        &f,
        "statefulsets",
        NS,
        "cache",
        &json!({
            "kind": "StatefulSet",
            "metadata": {"name": "cache", "namespace": NS},
            "spec": {"template": {"spec": {}}, "volumeClaimTemplates": [{"metadata": {"name": "data"}}]}
        }),
    );
    let (code, _, err) = run(&f, &["--plan", NS, "data-cache-0"]);
    refused_without_a_write(&f, code, &err, "StatefulSet boss-dev/cache");
}

#[test]
fn an_attached_longhorn_volume_is_refused() {
    let f = fixture("attached");
    put(
        &f,
        "volumes.longhorn.io",
        "longhorn-system",
        PV,
        &json!({
            "kind": "Volume",
            "metadata": {"name": PV, "namespace": "longhorn-system"},
            "spec": {"size": "161061273600"},
            "status": {"state": "attached", "currentNodeID": "w-1"}
        }),
    );
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "attached");
}

#[test]
fn a_volume_bound_to_another_claim_is_refused() {
    let f = fixture("claimref");
    let mut v = volume("Delete");
    v["spec"]["claimRef"]["uid"] = json!("someone-else");
    put(&f, "PersistentVolume", "", PV, &v);
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "claimRef");
}

#[test]
fn a_namespace_that_cannot_be_read_is_not_one_without_consumers() {
    let f = fixture("unreadable");
    touch(&f, "deny-pods");
    let (code, out, err) = run(&f, &["--plan", NS, CLAIM]);
    assert_eq!(
        code, 1,
        "could-not-look is a failure, never an empty consumer list:\n{err}\n{out}"
    );
    assert!(err.contains("FAILED"), "{err}");
    assert!(!out.contains("consumers: none"), "{out}");
}

#[test]
fn arguments_out_of_shape_are_refused() {
    let f = fixture("shape");
    for args in [
        vec!["--plan", NS],
        vec!["--plan", "Boss-Dev", CLAIM],
        vec!["--plan", NS, "a b"],
        vec!["--plan", NS, "-rf"],
        vec![NS, CLAIM],
        vec![NS, CLAIM, "not-a-hash"],
    ] {
        let (code, _, err) = run(&f, &args);
        assert_eq!(code, 78, "{args:?} is refused:\n{err}");
    }
    assert_eq!(deleted(&f), "");
}

#[test]
fn an_uncommitted_manifest_tree_is_refused() {
    let f = fixture("dirty");
    write_file(
        &f.tree.join("infra/cluster/manifests/dev-work.yaml"),
        "{\"kind\":\"ConfigMap\",\"metadata\":{\"name\":\"x\",\"namespace\":\"boss-dev\"}}\n",
    );
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "uncommitted");
}

#[test]
fn the_write_refuses_a_plan_that_moved_since_it_was_signed() {
    let f = fixture("moved");
    let (_, signed) = plan(&f);
    let mut c = claim(CLAIM, CLAIM_UID, PV);
    c["metadata"]["labels"] = json!({"app": "gate-runner", "moved": "yes"});
    put(&f, "PersistentVolumeClaim", NS, CLAIM, &c);
    let (code, _, err) = run(&f, &[NS, CLAIM, &signed]);
    refused_without_a_write(&f, code, &err, &signed);
}

#[test]
fn the_write_captures_onto_the_request_before_it_deletes_and_reads_back_claim_volume_and_bytes_gone()
 {
    let f = fixture("write");
    let (planned, signed) = plan(&f);
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 0, "the signed plan is applied:\n{err}\n{out}");
    assert!(
        out.starts_with(&planned),
        "the approved plan is printed first:\n{out}"
    );
    let log = calls(&f);
    let attach = log
        .find(&format!("boss attach {REQUEST} "))
        .unwrap_or_else(|| panic!("the capture is attached to the request:\n{log}"));
    let delete = log
        .find(" delete --raw ")
        .unwrap_or_else(|| panic!("the claim is deleted:\n{log}"));
    assert!(
        attach < delete,
        "the capture lands on the request BEFORE the delete:\n{log}"
    );
    assert_eq!(
        deleted(&f),
        format!("{NS}/{CLAIM} {CLAIM_UID}\n"),
        "exactly the planned claim, by uid"
    );
    let captured = std::fs::read_to_string(f.state.join("attached")).unwrap();
    for want in [
        &format!("PersistentVolumeClaim {NS}/{CLAIM}"),
        &format!("PersistentVolume _/{PV}"),
        &format!("volumes.longhorn.io longhorn-system/{PV}"),
        CLAIM_UID,
        PV_UID,
    ] {
        assert!(
            captured.contains(want),
            "the capture holds {want:?}:\n{captured}"
        );
    }
    assert!(
        out.contains("0f0f0f0f-0000-4000-8000-00000000000a"),
        "the attachment's file id is on the output:\n{out}"
    );
    assert!(object(&f, "PersistentVolumeClaim", NS, CLAIM).is_none());
    assert!(object(&f, "PersistentVolume", "", PV).is_none());
    let effects = effect_lines(&out);
    assert_eq!(effects.len(), 1, "exactly one effect line:\n{out}");
    for want in [CLAIM_UID, PV, "Longhorn volume", "NotFound"] {
        assert!(
            effects[0].contains(want),
            "the effect names {want:?}: {}",
            effects[0]
        );
    }
    // The verb's other output must not read as the effect.
    assert_eq!(effect_lines(&planned).len(), 0, "the plan is not an effect");
    assert_eq!(
        effect_lines(&err).len(),
        0,
        "nothing on stderr is an effect:\n{err}"
    );
}

#[test]
fn a_second_run_of_an_applied_plan_is_refused() {
    let f = fixture("twice");
    let (_, signed) = plan(&f);
    assert_eq!(run(&f, &[NS, CLAIM, &signed]).0, 0);
    let before = deleted(&f);
    let (code, _, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(
        code, 78,
        "the claim is gone, so the plan no longer holds:\n{err}"
    );
    assert_eq!(deleted(&f), before, "no second delete");
}

#[test]
fn a_capture_that_does_not_land_deletes_nothing() {
    let f = fixture("attach-fails");
    let (_, signed) = plan(&f);
    touch(&f, "attach-fails");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains("FAILED"), "{err}");
    assert_eq!(deleted(&f), "", "no capture, no delete");
    assert!(object(&f, "PersistentVolumeClaim", NS, CLAIM).is_some());
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn a_write_with_no_request_to_carry_the_capture_deletes_nothing() {
    let f = fixture("no-request");
    let (_, signed) = plan(&f);
    let (code, _, err) = run_with(&f, &[NS, CLAIM, &signed], &[("OPS_REQUEST_ID", None)]);
    refused_without_a_write(&f, code, &err, "OPS_REQUEST_ID");
}

#[test]
fn a_claim_still_held_after_the_delete_is_not_reported_gone() {
    let f = fixture("terminating");
    let (_, signed) = plan(&f);
    touch(&f, "terminating");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains("FAILED"), "{err}");
    assert!(err.contains("being deleted"), "the hold is named:\n{err}");
    assert!(effect_lines(&out).is_empty(), "no effect line:\n{out}");
}

#[test]
fn a_volume_that_outlives_its_delete_reclaim_is_not_reported_released() {
    let f = fixture("pv-sticks");
    let (_, signed) = plan(&f);
    touch(&f, "pv-sticks");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains(PV), "{err}");
    assert!(effect_lines(&out).is_empty(), "no effect line:\n{out}");
}

/// Backlog 37338a58 (review run 7cab3a42 of car ed4b5418, finding 1): a
/// get that exits 0 with NO document, or with a document carrying no
/// uid, was read as "a different object holds the name", so gone — and
/// the effect line then printed "(read back: NotFound)", which no read
/// produced. jq-1.6 over an empty file exits 0 with no output, so that
/// is the shape planted here: the answer a successful get prints for a
/// missing PV or Longhorn volume, empty and uid-less in turn.
#[test]
fn a_read_back_that_holds_no_object_is_not_reported_gone() {
    for res in ["PersistentVolume", "volumes.longhorn.io"] {
        for (shape, answer) in [
            ("empty", ""),
            ("blank", "  \n"),
            (
                "no-uid",
                r#"{"kind":"PersistentVolume","metadata":{"name":"x"}}"#,
            ),
        ] {
            let f = fixture(&format!("silent-{shape}-{}", res.replace('.', "-")));
            let (_, signed) = plan(&f);
            write_file(&f.state.join(format!("answer-{res}")), answer);
            let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
            assert_eq!(
                code, 1,
                "{res} answering {shape} is not proven gone:\n{err}\n{out}"
            );
            assert!(err.contains("FAILED"), "{err}");
            assert!(
                effect_lines(&out).is_empty(),
                "{res} answering {shape}: no effect line:\n{out}"
            );
        }
    }
}

/// The same finding's other half: the effect line quotes what was read.
/// A PV name taken again by another uid IS gone — the deleted volume —
/// and the line says how it was read, not a NotFound nobody saw.
#[test]
fn the_effect_line_quotes_the_read_back_it_made() {
    let f = fixture("pv-replaced");
    let (_, signed) = plan(&f);
    write_file(
        &f.state.join("pv-replaced"),
        "6f1e9b99-0000-4000-8000-0000000000e1",
    );
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 0, "{err}\n{out}");
    let effects = effect_lines(&out);
    assert_eq!(effects.len(), 1, "{out}");
    assert!(
        effects[0].contains(&format!(
            "PersistentVolume {PV} (reclaim policy Delete) is gone (read back: its name is now held by uid 6f1e9b99-0000-4000-8000-0000000000e1"
        )),
        "{}",
        effects[0]
    );
    assert!(
        effects[0].contains(&format!(
            "Longhorn volume {PV} is gone (read back: NotFound)"
        )),
        "{}",
        effects[0]
    );
}

/// Three refusals the same review found untested — removing each left
/// the suite green.
#[test]
fn a_delete_reclaim_off_longhorn_is_refused() {
    let f = fixture("not-longhorn");
    let mut v = volume("Delete");
    v["spec"]["csi"]["driver"] = json!("rbd.csi.ceph.com");
    put(&f, "PersistentVolume", "", PV, &v);
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "only for Longhorn");
}

#[test]
fn a_claim_already_being_deleted_is_refused() {
    let f = fixture("already-terminating");
    let mut c = claim(CLAIM, CLAIM_UID, PV);
    c["metadata"]["deletionTimestamp"] = json!("2026-10-18T11:00:00Z");
    put(&f, "PersistentVolumeClaim", NS, CLAIM, &c);
    let (code, _, err) = run(&f, &["--plan", NS, CLAIM]);
    refused_without_a_write(&f, code, &err, "already being deleted");
}

#[test]
fn an_attach_receipt_without_a_read_back_deletes_nothing() {
    let f = fixture("attach-unconfirmed");
    let (_, signed) = plan(&f);
    touch(&f, "attach-unconfirmed");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains("byte-identical read-back"), "{err}");
    assert_eq!(deleted(&f), "", "an unconfirmed capture deletes nothing");
    assert!(object(&f, "PersistentVolumeClaim", NS, CLAIM).is_some());
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn a_retained_volume_is_kept_and_said_so() {
    let f = fixture("retain");
    put(&f, "PersistentVolume", "", PV, &volume("Retain"));
    let (planned, signed) = plan(&f);
    assert!(planned.contains("  reclaim policy: Retain"), "{planned}");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 0, "{err}\n{out}");
    let effects = effect_lines(&out);
    assert_eq!(effects.len(), 1, "{out}");
    assert!(
        effects[0].contains("is kept, phase Released"),
        "{}",
        effects[0]
    );
    assert!(
        object(&f, "PersistentVolume", "", PV).is_some(),
        "a retained volume is not deleted"
    );
}

#[test]
fn an_unbound_claim_is_retired_alone() {
    let f = fixture("unbound");
    let mut c = claim(CLAIM, CLAIM_UID, "");
    c["status"] = json!({"phase": "Pending"});
    put(&f, "PersistentVolumeClaim", NS, CLAIM, &c);
    let (planned, signed) = plan(&f);
    assert!(planned.contains("volume: none"), "{planned}");
    let (code, out, err) = run(&f, &[NS, CLAIM, &signed]);
    assert_eq!(code, 0, "{err}\n{out}");
    let effects = effect_lines(&out);
    assert_eq!(effects.len(), 1, "{out}");
    assert!(
        effects[0].contains("bound to no PersistentVolume"),
        "{}",
        effects[0]
    );
}

#[test]
fn the_verb_files_bind_the_script_and_its_plan() {
    let w = verb("retire-volume-claim");
    let p = verb("plan-a-volume-claim-retirement");
    assert_eq!(
        w["argv"],
        json!(["infra/forge/retire-volume-claim.sh", "{1}", "{2}", "{3}"])
    );
    assert_eq!(
        p["argv"],
        json!(["infra/forge/retire-volume-claim.sh", "--plan", "{1}", "{2}"])
    );
    assert_eq!(w["requires_approval"], json!(true));
    assert_eq!(w["plan_verb"], json!("plan-a-volume-claim-retirement"));
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert!(w["about"].as_str().unwrap().contains("MUTATING"));
    assert!(!p["about"].as_str().unwrap().contains("MUTATING"));
    // The patterns the allowlist applies refuse what the script refuses.
    let pat = |v: &Value, i: usize| {
        regex::Regex::new(v["params"][i]["pattern"].as_str().unwrap()).unwrap()
    };
    for (i, bad) in [
        (0, "Boss-Dev"),
        (0, "-n"),
        (1, "a b"),
        (1, "-rf"),
        (1, "x/y"),
    ] {
        assert!(!pat(&w, i).is_match(bad), "param {i} admits {bad:?}");
        assert!(!pat(&p, i).is_match(bad), "plan param {i} admits {bad:?}");
    }
    assert!(pat(&w, 0).is_match(NS) && pat(&w, 1).is_match(CLAIM));
}
