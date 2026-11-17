//! `infra/forge/retire-access-object.sh` — a ServiceAccount, Role or
//! RoleBinding the tree no longer declares is retired through a plan David
//! signs with his passkey, never through an admin kubeconfig typed at a
//! workstation (backlog 526bedf2).
//!
//! WHY. `delete-orphan-object` withholds these three kinds from its kind
//! floor on purpose: a ServiceAccount's deletion invalidates the tokens
//! issued from it, and a Role or RoleBinding's deletion "can remove a live
//! capability nobody wrote down". So until this verb they stayed a named
//! human step, run by hand with the admin kubeconfig — and David,
//! 2026-10-18: manually running any ops command is a gap in BOSS's
//! coverage. retire-volume-claim (f36eb7ce) answered the same gap for
//! claims; this is its sibling for privileges. The human act is the
//! approval of a plan that names every consumer, not the command.
//!
//! STRICT ABOUT WHAT CAN BE DELETED. Nothing becomes deletable WITHOUT a
//! passkey: `delete-orphan-object`'s floor is pinned unchanged here, and
//! this verb's own floor is exactly the three kinds, held equal between
//! the script and the allowlist entry, and disjoint from that floor. Every
//! retirement still takes its authority from the one derivation
//! (`undeclared-objects.sh --check`), so a DECLARED object is never
//! eligible, however it is approved.
//!
//! WHAT EACH CASE PINS, in the order the script applies its bounds: the
//! argument shape and the kind floor; the protected names; a committed
//! manifest tree; the derivation's verdict; the object's own facts (not
//! being deleted, no owner); its consumers, per kind — for a
//! ServiceAccount the Pods that run as it and the workloads that would
//! (the holders of its issued tokens), a long-lived token Secret, and any
//! RoleBinding or ClusterRoleBinding that grants it; for a Role the
//! RoleBindings that grant it; for a RoleBinding the Pods and workloads
//! running as a subject it grants, a long-lived token Secret issued from
//! one (review 64b37b0c B1), and a User or Group subject, whose holders no
//! read can name. A read that cannot look is a failure, never
//! an empty consumer list. Then the write: the signed hash, the capture
//! landing on the request BEFORE the delete, a uid-preconditioned delete,
//! and a read-back that fails closed — only the server's NotFound, or the
//! name held by another uid, is gone.
//!
//! THE FIXTURE is retire_volume_claim_sh.rs's: a manifest tree that is a
//! real git repository, a stub `kubectl` on `BOSS_KUBECTL` whose cluster
//! is a directory of JSON objects, and a stub `boss` on `BOSS_BIN` that
//! records each attach. The stub refuses a delete without a uid
//! precondition. Nothing here touches a cluster or the system of record.

use boss_testing::{repo_root, scratch_dir, write_exec};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const NS: &str = "boss-dev";
const NAME: &str = "old-runner";
const SA_UID: &str = "6f1e9b99-0000-4000-8000-0000000000a1";
const ROLE_UID: &str = "6f1e9b99-0000-4000-8000-0000000000a2";
const RB_UID: &str = "6f1e9b99-0000-4000-8000-0000000000a3";
const REQUEST: &str = "0e0e0e0e-0000-4000-8000-0000000000aa";

fn script() -> PathBuf {
    repo_root().join("infra/forge/retire-access-object.sh")
}

/// The stand-in cluster. `objects/<resource>/<ns or _>/<name>.json` is
/// one object, the resource spelled as the script and the derivation ask
/// for it; `-A` lists every namespace directory. `deny-<resource>` makes
/// that resource Forbidden; `sticks` makes an accepted delete leave the
/// object in place; `answer-<resource>` is what a successful get of a
/// missing object of that resource prints, verbatim. `calls` logs every
/// invocation, `deleted` every delete the precondition admitted.
const KUBECTL: &str = r#"#!/usr/bin/env bash
S="${KSTATE:?}"
echo "kubectl $*" >> "$S/calls"
ns=""; raw=""; file=""; fmt=""; sel=""; all=0; words=()
while [ $# -gt 0 ]; do
    case "$1" in
        -n) ns="$2"; shift 2 ;;
        -A|--all-namespaces) all=1; shift ;;
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
        if [ "$all" -eq 1 ]; then dirs=("$O/$r"/*); else dirs=("$(dir_of "$r" "$ns")"); fi
        for d in "${dirs[@]}"; do
            [ -d "$d" ] || continue
            for f in "$d"/*.json; do
                [ -f "$f" ] || continue
                items=$(jq -c --slurpfile o "$f" '. + $o' <<< "$items")
            done
        done
    done
    if [ -n "$sel" ]; then
        k="${sel%%=*}"; v="${sel#*=}"
        items=$(jq -c --arg k "$k" --arg v "$v" 'map(select(.metadata.labels[$k] == $v))' <<< "$items")
    fi
    case "$fmt" in
        json) jq '{apiVersion: "v1", kind: "List", items: .}' <<< "$items" ;;
        *service-account*) jq -r '.[] | "\(.metadata.name)\t\(.type // "")\t\(.metadata.annotations["kubernetes.io/service-account.name"] // "")"' <<< "$items" ;;
        *ownerReferences*) jq -r '.[] | "\(.metadata.name)\t\(.metadata.ownerReferences[0].kind // "")"' <<< "$items" ;;
        jsonpath=*) jq -r '.[] | .metadata.name' <<< "$items" ;;
        *) echo "stub: unmodelled list format $fmt" >&2; exit 64 ;;
    esac
    exit 0 ;;
delete)
    [ -n "$raw" ] || { echo "stub: only a --raw delete is modelled: $*" >&2; exit 64; }
    if [[ "$raw" =~ ^/api/v1/namespaces/([^/]+)/serviceaccounts/([^/]+)$ ]]; then dres=ServiceAccount
    elif [[ "$raw" =~ ^/apis/rbac.authorization.k8s.io/v1/namespaces/([^/]+)/roles/([^/]+)$ ]]; then dres=Role
    elif [[ "$raw" =~ ^/apis/rbac.authorization.k8s.io/v1/namespaces/([^/]+)/rolebindings/([^/]+)$ ]]; then dres=RoleBinding
    else echo "stub: a delete of something this verb never deletes: $raw" >&2; exit 64; fi
    dns="${BASH_REMATCH[1]}"; dname="${BASH_REMATCH[2]}"
    f="$(dir_of "$dres" "$dns")/$dname.json"
    body=$(cat)
    want=$(jq -r '.preconditions.uid // empty' <<< "$body" 2>/dev/null)
    [ -n "$want" ] || { echo "stub: a delete without a uid precondition: $body" >&2; exit 1; }
    [ -f "$f" ] || { echo "Error from server (NotFound): $dres \"$dname\" not found" >&2; exit 1; }
    have=$(jq -r '.metadata.uid' "$f")
    if [ "$have" != "$want" ]; then
        echo "Error from server (Conflict): Operation cannot be fulfilled on $dres \"$dname\": Precondition failed: UID in precondition: $want, UID in object meta: $have" >&2
        exit 1
    fi
    echo "$dres $dns/$dname $want" >> "$S/deleted"
    [ -e "$S/sticks" ] || rm -f "$f"
    jq -n --arg k "$dres" --arg n "$dname" '{kind: $k, metadata: {name: $n}}'
    exit 0 ;;
esac
echo "stub: unmodelled call: ${words[*]}" >&2
exit 64
"#;

/// The stand-in `boss`, as retire_volume_claim_sh.rs models it: only
/// `attach`, printing the real verb's two receipt lines over the bytes.
/// `attach-fails` makes the file store refuse.
const BOSS: &str = r#"#!/usr/bin/env bash
S="${KSTATE:?}"
echo "boss $*" >> "$S/calls"
[ "${1:-}" = attach ] || { echo "stub boss: only attach is modelled: $*" >&2; exit 64; }
[ -e "$S/attach-fails" ] && { echo "boss attach: POST /api/files answered HTTP 503" >&2; exit 1; }
f="$3"
cp "$f" "$S/attached"
sha=$(sha256sum "$f" | cut -c1-64); n=$(wc -c < "$f" | tr -d ' ')
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

/// The tree owns `boss` and `boss-dev` and declares the dev session's
/// ServiceAccount, Role and RoleBinding in boss-dev (so the three kinds
/// are in the derivation's scope there), and enough ConfigMaps to clear
/// the derivation's "the scrape broke" floors.
fn fixture(case: &str) -> Fixture {
    let root = scratch_dir(&format!("retire-access-object-{case}"));
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
        &dir.join("dev-session.yaml"),
        &format!(
            "{}\n{}\n{}\n{}\n",
            doc("ServiceAccount", "boss-dev", "dev-session"),
            doc("Role", "boss-dev", "dev-session"),
            doc("RoleBinding", "boss-dev", "dev-session"),
            doc("ServiceAccount", "boss", "boss"),
        ),
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

fn remove(f: &Fixture, res: &str, ns: &str, name: &str) {
    let ns = if ns.is_empty() { "_" } else { ns };
    std::fs::remove_file(
        f.state
            .join("objects")
            .join(res)
            .join(ns)
            .join(format!("{name}.json")),
    )
    .unwrap();
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

fn meta(name: &str, ns: &str, uid: &str) -> Value {
    json!({
        "name": name, "namespace": ns, "uid": uid,
        "creationTimestamp": "2026-09-20T10:00:00Z",
        "labels": {"app": "old-runner"},
        "resourceVersion": "4711"
    })
}

fn service_account(name: &str, uid: &str) -> Value {
    json!({
        "apiVersion": "v1", "kind": "ServiceAccount",
        "metadata": meta(name, NS, uid),
        "secrets": [{"name": "old-runner-pull"}]
    })
}

fn role(name: &str, uid: &str) -> Value {
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role",
        "metadata": meta(name, NS, uid),
        "rules": [{"apiGroups": ["batch"], "resources": ["jobs"], "verbs": ["create", "delete"]}]
    })
}

fn binding(name: &str, uid: &str, role_kind: &str, role: &str, subjects: Value) -> Value {
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding",
        "metadata": meta(name, NS, uid),
        "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": role_kind, "name": role},
        "subjects": subjects
    })
}

fn sa_subject(name: &str, ns: &str) -> Value {
    json!({"kind": "ServiceAccount", "name": name, "namespace": ns})
}

fn pod_running_as(name: &str, sa: &str, phase: &str) -> Value {
    json!({
        "kind": "Pod",
        "metadata": {"name": name, "namespace": NS, "uid": format!("u-{name}")},
        "spec": {"serviceAccountName": sa, "containers": []},
        "status": {"phase": phase}
    })
}

/// The ordinary cluster: the three undeclared objects to retire —
/// ServiceAccount, Role and the RoleBinding that grants that Role to that
/// ServiceAccount — beside the declared dev session, whose pod runs as its
/// own ServiceAccount; a Job that runs as a DIFFERENT ServiceAccount, and a
/// token Secret issued to a different one, so every consumer scan has
/// something to read past.
fn ordinary(f: &Fixture) {
    put(
        f,
        "ServiceAccount",
        NS,
        NAME,
        &service_account(NAME, SA_UID),
    );
    put(f, "Role", NS, NAME, &role(NAME, ROLE_UID));
    put(
        f,
        "RoleBinding",
        NS,
        NAME,
        &binding(NAME, RB_UID, "Role", NAME, json!([sa_subject(NAME, NS)])),
    );
    put(
        f,
        "ServiceAccount",
        NS,
        "dev-session",
        &service_account("dev-session", "u-dev-sa"),
    );
    put(
        f,
        "Role",
        NS,
        "dev-session",
        &role("dev-session", "u-dev-role"),
    );
    put(
        f,
        "RoleBinding",
        NS,
        "dev-session",
        &binding(
            "dev-session",
            "u-dev-rb",
            "Role",
            "dev-session",
            json!([sa_subject("dev-session", NS)]),
        ),
    );
    put(
        f,
        "pods",
        NS,
        "boss-dev-0",
        &pod_running_as("boss-dev-0", "dev-session", "Running"),
    );
    put(
        f,
        "jobs",
        NS,
        "gate-run-a",
        &json!({
            "kind": "Job",
            "metadata": {"name": "gate-run-a", "namespace": NS},
            "spec": {"template": {"spec": {"serviceAccountName": "gate-runner"}}}
        }),
    );
    put(
        f,
        "Secret",
        NS,
        "gate-runner-token",
        &json!({
            "kind": "Secret", "type": "kubernetes.io/service-account-token",
            "metadata": {"name": "gate-runner-token", "namespace": NS,
                         "annotations": {"kubernetes.io/service-account.name": "gate-runner"}}
        }),
    );
}

/// The ServiceAccount and Role are retirable only once nothing grants
/// them, so their cases begin from a cluster whose binding has gone.
fn without_the_binding(f: &Fixture) {
    remove(f, "RoleBinding", NS, NAME);
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
        .env("BOSS_RETIRE_ACCESS_WAIT_S", "2")
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

fn plan(f: &Fixture, kind: &str) -> (String, String) {
    let (code, out, err) = run(f, &["--plan", kind, NS, NAME]);
    assert_eq!(code, 0, "the {kind} plan renders:\n{err}\n{out}");
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
    let re = verb("retire-access-object")["effect"]
        .as_str()
        .expect("retire-access-object declares an effect")
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

fn failed_without_a_plan(f: &Fixture, code: i32, out: &str, err: &str) {
    assert_eq!(
        code, 1,
        "could-not-look is a failure, never an empty consumer list:\n{err}\n{out}"
    );
    assert!(err.contains("FAILED"), "{err}");
    assert!(!out.contains("consumers: none"), "{out}");
    assert_eq!(deleted(f), "");
}

// --- the plans -----------------------------------------------------------------

#[test]
fn the_binding_plan_names_what_it_grants_and_to_whom_and_deletes_nothing() {
    let f = fixture("plan-binding");
    let (out, said) = plan(&f, "RoleBinding");
    assert_eq!(
        said,
        sha256(&out, &f),
        "plan-sha256 is the hash of the bytes on stdout"
    );
    for want in [
        "plan: retire-access-object",
        &format!("object: RoleBinding {NS}/{NAME}"),
        &format!("  uid: {RB_UID}"),
        "  created: 2026-09-20T10:00:00Z",
        r#"  labels: {"app":"old-runner"}"#,
        "  owner references: none",
        &format!("  grants: Role {NS}/{NAME}"),
        &format!("    - ServiceAccount {NS}/{NAME}"),
        "consumers: none",
        &format!(
            "DELETE /apis/rbac.authorization.k8s.io/v1/namespaces/{NS}/rolebindings/{NAME} with preconditions.uid={RB_UID}"
        ),
    ] {
        assert!(out.contains(want), "the plan carries {want:?}:\n{out}");
    }
    assert!(
        !out.contains("4711"),
        "no resourceVersion in the signed bytes:\n{out}"
    );
    let (again, _) = plan(&f, "RoleBinding");
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
fn the_role_plan_names_the_privilege_it_removes() {
    let f = fixture("plan-role");
    without_the_binding(&f);
    let (out, _) = plan(&f, "Role");
    for want in [
        &format!("object: Role {NS}/{NAME}"),
        &format!("  uid: {ROLE_UID}"),
        r#"    - {"apiGroups":["batch"],"resources":["jobs"],"verbs":["create","delete"]}"#,
        "consumers: none",
        &format!(
            "DELETE /apis/rbac.authorization.k8s.io/v1/namespaces/{NS}/roles/{NAME} with preconditions.uid={ROLE_UID}"
        ),
    ] {
        assert!(out.contains(want), "the plan carries {want:?}:\n{out}");
    }
}

#[test]
fn the_service_account_plan_names_its_tokens_holders_and_grants_as_none() {
    let f = fixture("plan-sa");
    without_the_binding(&f);
    let (out, _) = plan(&f, "ServiceAccount");
    for want in [
        &format!("object: ServiceAccount {NS}/{NAME}"),
        &format!("  uid: {SA_UID}"),
        "  secrets it names (kept): old-runner-pull",
        "consumers: none",
        "no Pod (any phase)",
        "token",
        "ClusterRoleBinding",
        &format!(
            "DELETE /api/v1/namespaces/{NS}/serviceaccounts/{NAME} with preconditions.uid={SA_UID}"
        ),
    ] {
        assert!(out.contains(want), "the plan carries {want:?}:\n{out}");
    }
}

/// A token minted through the TokenRequest API (`kubectl create token`)
/// and carried off the cluster leaves no object any read can find, so a
/// plan that says no token "is held now or will be again" states what it
/// could not have read. The account's and the binding's plans each say
/// plainly which holder they cannot see, as their own signed line
/// (review 64b37b0c, finding N1).
#[test]
fn the_plans_name_the_holder_no_read_can_see() {
    for kind in ["ServiceAccount", "RoleBinding"] {
        let f = fixture(&format!("plan-unseen-{kind}"));
        if kind == "ServiceAccount" {
            without_the_binding(&f);
        }
        let (out, _) = plan(&f, kind);
        assert!(
            out.lines()
                .any(|l| l.starts_with("cannot read: ") && l.contains("TokenRequest")),
            "the {kind} plan names the holder it cannot see:\n{out}"
        );
        assert!(
            !out.contains("will be again"),
            "the {kind} plan claims no more than it read:\n{out}"
        );
    }
}

// --- what is never deletable ----------------------------------------------------

#[test]
fn a_kind_outside_the_floor_is_refused_before_anything_is_read() {
    let f = fixture("kind-floor");
    for kind in [
        "PersistentVolumeClaim",
        "Secret",
        "Service",
        "ConfigMap",
        "ClusterRole",
        "ClusterRoleBinding",
        "StatefulSet",
        "serviceaccount",
    ] {
        let (code, _, err) = run(&f, &["--plan", kind, NS, NAME]);
        refused_without_a_write(&f, code, &err, "ServiceAccount, Role, RoleBinding");
    }
    assert_eq!(calls(&f), "", "a kind outside the floor reads nothing");
}

#[test]
fn a_protected_name_is_refused_even_where_nothing_declares_it() {
    let f = fixture("protected");
    for (kind, name) in [
        ("ServiceAccount", "default"),
        ("ServiceAccount", "break-glass-operator"),
        ("Role", "break-glass-operator-rollback"),
        ("RoleBinding", "break-glass-deposit"),
    ] {
        let (code, _, err) = run(&f, &["--plan", kind, NS, name]);
        refused_without_a_write(&f, code, &err, "protected");
    }
    assert_eq!(calls(&f), "", "a protected name reads nothing");
}

#[test]
fn an_object_a_manifest_declares_is_refused() {
    let f = fixture("declared");
    for kind in ["ServiceAccount", "Role", "RoleBinding"] {
        let (code, _, err) = run(&f, &["--plan", kind, NS, "dev-session"]);
        refused_without_a_write(&f, code, &err, "DECLARES");
    }
}

#[test]
fn a_namespace_the_tree_does_not_own_is_refused() {
    let f = fixture("namespace");
    let (code, _, err) = run(&f, &["--plan", "Role", "kube-system", NAME]);
    refused_without_a_write(&f, code, &err, "does not own namespace");
}

#[test]
fn an_object_a_controller_owns_is_refused() {
    let f = fixture("owned");
    let mut b = binding(NAME, RB_UID, "Role", NAME, json!([]));
    b["metadata"]["ownerReferences"] =
        json!([{"apiVersion": "v1", "kind": "ConfigMap", "name": "x", "uid": "u-x"}]);
    put(&f, "RoleBinding", NS, NAME, &b);
    let (code, _, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
    refused_without_a_write(&f, code, &err, "controller owns it");
}

#[test]
fn an_object_already_being_deleted_is_refused() {
    let f = fixture("terminating");
    let mut b = binding(NAME, RB_UID, "Role", NAME, json!([]));
    b["metadata"]["deletionTimestamp"] = json!("2026-10-18T11:00:00Z");
    put(&f, "RoleBinding", NS, NAME, &b);
    let (code, _, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
    refused_without_a_write(&f, code, &err, "already being deleted");
}

#[test]
fn arguments_out_of_shape_are_refused() {
    let f = fixture("shape");
    for args in [
        vec!["--plan", "Role", NS],
        vec!["--plan", "Role", "Boss-Dev", NAME],
        vec!["--plan", "Role", NS, "a b"],
        vec!["--plan", "Role", NS, "-rf"],
        vec!["--plan", "Role", NS, "x/y"],
        vec!["Role", NS, NAME],
        vec!["Role", NS, NAME, "not-a-hash"],
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
        &f.tree.join("infra/cluster/manifests/dev-session.yaml"),
        "{\"kind\":\"ConfigMap\",\"metadata\":{\"name\":\"x\",\"namespace\":\"boss-dev\"}}\n",
    );
    let (code, _, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
    refused_without_a_write(&f, code, &err, "uncommitted");
}

// --- consumers: a ServiceAccount ------------------------------------------------

#[test]
fn a_service_account_a_binding_grants_is_refused() {
    // The ordinary cluster: the RoleBinding old-runner grants it.
    let f = fixture("sa-bound");
    let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
    refused_without_a_write(&f, code, &err, &format!("RoleBinding {NS}/{NAME}"));
}

#[test]
fn a_service_account_a_binding_in_another_namespace_grants_is_refused() {
    let f = fixture("sa-bound-elsewhere");
    without_the_binding(&f);
    let mut b = binding(
        "reads-boss",
        "u-rb-boss",
        "Role",
        "reader",
        json!([sa_subject(NAME, NS)]),
    );
    b["metadata"]["namespace"] = json!("boss");
    put(&f, "RoleBinding", "boss", "reads-boss", &b);
    let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
    refused_without_a_write(&f, code, &err, "RoleBinding boss/reads-boss");
}

#[test]
fn a_service_account_a_cluster_role_binding_grants_is_refused() {
    let f = fixture("sa-cluster-bound");
    without_the_binding(&f);
    put(
        &f,
        "ClusterRoleBinding",
        "",
        "old-runner-nodes",
        &json!({
            "kind": "ClusterRoleBinding",
            "metadata": {"name": "old-runner-nodes", "uid": "u-crb"},
            "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": "view"},
            "subjects": [sa_subject(NAME, NS)]
        }),
    );
    let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
    refused_without_a_write(&f, code, &err, "ClusterRoleBinding old-runner-nodes");
}

#[test]
fn a_service_account_a_pod_runs_as_is_refused_whatever_its_phase() {
    for phase in ["Running", "Succeeded"] {
        let f = fixture(&format!("sa-pod-{phase}"));
        without_the_binding(&f);
        put(
            &f,
            "pods",
            NS,
            "worker-0",
            &pod_running_as("worker-0", NAME, phase),
        );
        let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
        refused_without_a_write(&f, code, &err, "Pod boss-dev/worker-0");
    }
}

#[test]
fn a_service_account_a_workload_would_run_as_is_refused() {
    let f = fixture("sa-cronjob");
    without_the_binding(&f);
    put(
        &f,
        "cronjobs",
        NS,
        "nightly",
        &json!({
            "kind": "CronJob",
            "metadata": {"name": "nightly", "namespace": NS},
            "spec": {"jobTemplate": {"spec": {"template": {"spec": {"serviceAccountName": NAME}}}}}
        }),
    );
    let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
    refused_without_a_write(&f, code, &err, "CronJob boss-dev/nightly");
}

#[test]
fn a_service_account_with_a_long_lived_token_is_refused() {
    let f = fixture("sa-token");
    without_the_binding(&f);
    put(
        &f,
        "Secret",
        NS,
        "old-runner-token",
        &json!({
            "kind": "Secret", "type": "kubernetes.io/service-account-token",
            "metadata": {"name": "old-runner-token", "namespace": NS,
                         "annotations": {"kubernetes.io/service-account.name": NAME}}
        }),
    );
    let (code, _, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
    refused_without_a_write(&f, code, &err, "Secret boss-dev/old-runner-token");
}

#[test]
fn unreadable_service_account_consumers_are_a_failure() {
    for res in ["pods", "RoleBinding", "ClusterRoleBinding", "Secret"] {
        let f = fixture(&format!("sa-deny-{res}"));
        without_the_binding(&f);
        touch(&f, &format!("deny-{res}"));
        let (code, out, err) = run(&f, &["--plan", "ServiceAccount", NS, NAME]);
        failed_without_a_plan(&f, code, &out, &err);
    }
}

// --- consumers: a Role and a RoleBinding ----------------------------------------

#[test]
fn a_role_a_binding_grants_is_refused() {
    let f = fixture("role-bound");
    let (code, _, err) = run(&f, &["--plan", "Role", NS, NAME]);
    refused_without_a_write(&f, code, &err, &format!("RoleBinding {NS}/{NAME}"));
}

#[test]
fn unreadable_role_bindings_are_a_failure() {
    let f = fixture("role-deny");
    without_the_binding(&f);
    touch(&f, "deny-RoleBinding");
    let (code, out, err) = run(&f, &["--plan", "Role", NS, NAME]);
    failed_without_a_plan(&f, code, &out, &err);
}

#[test]
fn a_binding_whose_subject_a_pod_runs_as_is_refused() {
    let f = fixture("rb-live");
    put(
        &f,
        "pods",
        NS,
        "worker-0",
        &pod_running_as("worker-0", NAME, "Running"),
    );
    let (code, _, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
    refused_without_a_write(&f, code, &err, "Pod boss-dev/worker-0");
}

#[test]
fn a_binding_to_a_user_or_group_is_refused_because_no_read_names_its_holders() {
    for kind in ["User", "Group"] {
        let f = fixture(&format!("rb-{kind}"));
        put(
            &f,
            "RoleBinding",
            NS,
            NAME,
            &binding(
                NAME,
                RB_UID,
                "ClusterRole",
                "edit",
                json!([{"apiGroup": "rbac.authorization.k8s.io", "kind": kind, "name": "someone"}]),
            ),
        );
        let (code, _, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
        refused_without_a_write(&f, code, &err, &format!("{kind} someone"));
    }
}

/// A long-lived token Secret is a holder outside any Pod: whoever has its
/// bytes (a kubeconfig on a workstation, the break-glass operator's token)
/// loses the grant when the binding goes. The ServiceAccount branch always
/// counted one; the RoleBinding branch did not, and planned "consumers:
/// none" over it (review 64b37b0c, finding B1, reproduced at e579b497).
/// The Secret is read in the SUBJECT's namespace, which is not always the
/// binding's.
#[test]
fn a_binding_whose_subject_holds_a_long_lived_token_is_refused() {
    for (case, sns) in [("rb-token", NS), ("rb-token-elsewhere", "boss")] {
        let f = fixture(case);
        put(
            &f,
            "RoleBinding",
            NS,
            NAME,
            &binding(NAME, RB_UID, "Role", NAME, json!([sa_subject(NAME, sns)])),
        );
        put(
            &f,
            "Secret",
            sns,
            "old-runner-token",
            &json!({
                "kind": "Secret", "type": "kubernetes.io/service-account-token",
                "metadata": {"name": "old-runner-token", "namespace": sns,
                             "annotations": {"kubernetes.io/service-account.name": NAME}}
            }),
        );
        let (code, out, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
        assert!(!out.contains("consumers: none"), "{out}");
        refused_without_a_write(&f, code, &err, &format!("Secret {sns}/old-runner-token"));
    }
}

#[test]
fn unreadable_binding_holders_are_a_failure() {
    for res in ["pods", "Secret"] {
        let f = fixture(&format!("rb-deny-{res}"));
        touch(&f, &format!("deny-{res}"));
        let (code, out, err) = run(&f, &["--plan", "RoleBinding", NS, NAME]);
        failed_without_a_plan(&f, code, &out, &err);
    }
}

// --- the write --------------------------------------------------------------------

#[test]
fn the_write_refuses_a_plan_that_moved_since_it_was_signed() {
    let f = fixture("moved");
    let (_, signed) = plan(&f, "RoleBinding");
    put(
        &f,
        "RoleBinding",
        NS,
        NAME,
        &binding(
            NAME,
            RB_UID,
            "Role",
            NAME,
            json!([sa_subject(NAME, NS), sa_subject("another", NS)]),
        ),
    );
    let (code, _, err) = run(&f, &["RoleBinding", NS, NAME, &signed]);
    refused_without_a_write(&f, code, &err, &signed);
}

#[test]
fn a_plan_signed_for_one_kind_does_not_delete_another() {
    let f = fixture("other-kind");
    without_the_binding(&f);
    let (_, signed) = plan(&f, "Role");
    let (code, _, err) = run(&f, &["ServiceAccount", NS, NAME, &signed]);
    refused_without_a_write(&f, code, &err, &signed);
}

#[test]
fn the_write_captures_onto_the_request_before_it_deletes_and_reads_the_object_back_gone() {
    for (kind, uid) in [
        ("RoleBinding", RB_UID),
        ("Role", ROLE_UID),
        ("ServiceAccount", SA_UID),
    ] {
        let f = fixture(&format!("write-{kind}"));
        if kind != "RoleBinding" {
            without_the_binding(&f);
        }
        let (planned, signed) = plan(&f, kind);
        let (code, out, err) = run(&f, &[kind, NS, NAME, &signed]);
        assert_eq!(code, 0, "the signed {kind} plan is applied:\n{err}\n{out}");
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
            .unwrap_or_else(|| panic!("the {kind} is deleted:\n{log}"));
        assert!(
            attach < delete,
            "the capture lands on the request BEFORE the delete:\n{log}"
        );
        assert_eq!(
            deleted(&f),
            format!("{kind} {NS}/{NAME} {uid}\n"),
            "exactly the planned object, by uid"
        );
        let captured = std::fs::read_to_string(f.state.join("attached")).unwrap();
        assert!(
            captured.contains(&format!("{kind} {NS}/{NAME}")) && captured.contains(uid),
            "the capture holds the object:\n{captured}"
        );
        assert!(object(&f, kind, NS, NAME).is_none());
        let effects = effect_lines(&out);
        assert_eq!(effects.len(), 1, "exactly one effect line:\n{out}");
        assert!(
            effects[0].contains(&format!(
                "{kind} {NS}/{NAME} uid {uid} is gone (read back: NotFound)"
            )),
            "the effect names the object and the read: {}",
            effects[0]
        );
        assert_eq!(effect_lines(&planned).len(), 0, "the plan is not an effect");
        assert_eq!(
            effect_lines(&err).len(),
            0,
            "nothing on stderr is an effect:\n{err}"
        );
    }
}

#[test]
fn a_second_run_of_an_applied_plan_is_refused() {
    let f = fixture("twice");
    let (_, signed) = plan(&f, "RoleBinding");
    assert_eq!(run(&f, &["RoleBinding", NS, NAME, &signed]).0, 0);
    let before = deleted(&f);
    let (code, _, err) = run(&f, &["RoleBinding", NS, NAME, &signed]);
    assert_eq!(
        code, 78,
        "the binding is gone, so the plan no longer holds:\n{err}"
    );
    assert_eq!(deleted(&f), before, "no second delete");
}

#[test]
fn a_capture_that_does_not_land_deletes_nothing() {
    let f = fixture("attach-fails");
    let (_, signed) = plan(&f, "RoleBinding");
    touch(&f, "attach-fails");
    let (code, out, err) = run(&f, &["RoleBinding", NS, NAME, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains("FAILED"), "{err}");
    assert_eq!(deleted(&f), "", "no capture, no delete");
    assert!(object(&f, "RoleBinding", NS, NAME).is_some());
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn a_write_with_no_request_to_carry_the_capture_deletes_nothing() {
    let f = fixture("no-request");
    let (_, signed) = plan(&f, "RoleBinding");
    let (code, _, err) = run_with(
        &f,
        &["RoleBinding", NS, NAME, &signed],
        &[("OPS_REQUEST_ID", None)],
    );
    refused_without_a_write(&f, code, &err, "OPS_REQUEST_ID");
}

#[test]
fn an_object_still_there_after_the_delete_is_not_reported_gone() {
    let f = fixture("sticks");
    let (_, signed) = plan(&f, "RoleBinding");
    touch(&f, "sticks");
    let (code, out, err) = run(&f, &["RoleBinding", NS, NAME, &signed]);
    assert_eq!(code, 1, "{err}\n{out}");
    assert!(err.contains("FAILED"), "{err}");
    assert!(err.contains("still there"), "the survivor is named:\n{err}");
    assert!(effect_lines(&out).is_empty(), "no effect line:\n{out}");
}

/// The read-back fails closed (the item's second acceptance line): a get
/// that exits 0 with no document, or with one carrying no uid, read
/// nothing, and a get that fails other than NotFound read nothing either.
/// None of them is gone.
#[test]
fn a_read_back_that_reads_nothing_is_not_reported_gone() {
    for (shape, answer) in [
        ("empty", Some("")),
        ("blank", Some("  \n")),
        (
            "no-uid",
            Some(r#"{"kind":"RoleBinding","metadata":{"name":"x"}}"#),
        ),
        ("forbidden", None),
    ] {
        let f = fixture(&format!("silent-{shape}"));
        let (_, signed) = plan(&f, "RoleBinding");
        match answer {
            Some(a) => write_file(&f.state.join("answer-RoleBinding"), a),
            // Forbidden only after the delete: the stub's deny applies to
            // every get, so plant it once the delete has been admitted.
            None => write_file(&f.state.join("deny-after-delete"), ""),
        }
        if answer.is_none() {
            // Model a credential that loses its read between the delete and
            // the read-back: the object goes, and every later get answers
            // Forbidden.
            let k = std::fs::read_to_string(&f.kubectl).unwrap();
            let k = k.replacen(
                "    [ -e \"$S/sticks\" ] || rm -f \"$f\"\n",
                "    [ -e \"$S/sticks\" ] || rm -f \"$f\"\n    [ -e \"$S/deny-after-delete\" ] && touch \"$S/deny-RoleBinding\"\n",
                1,
            );
            write_exec(&f.kubectl, &k);
        }
        let (code, out, err) = run(&f, &["RoleBinding", NS, NAME, &signed]);
        assert_eq!(
            code, 1,
            "a read-back answering {shape} is not proven gone:\n{err}\n{out}"
        );
        assert!(err.contains("FAILED"), "{err}");
        assert!(
            err.contains("not proven gone"),
            "the failure says it is unproven:\n{err}"
        );
        assert!(
            effect_lines(&out).is_empty(),
            "{shape}: no effect line:\n{out}"
        );
    }
}

// --- the verb files, and the floors held equal ------------------------------------

/// The kinds a script lists in its one `NAME=(...)` array line.
fn array_in(script: &str, name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join(script)).unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with(&format!("{name}=(")))
        .collect();
    assert_eq!(lines.len(), 1, "{script} carries one {name}=(...) line");
    lines[0]
        .trim_start_matches(&format!("{name}=("))
        .trim_end_matches(')')
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_verb_files_bind_the_script_and_its_plan() {
    let w = verb("retire-access-object");
    let p = verb("plan-an-access-object-retirement");
    assert_eq!(
        w["argv"],
        json!([
            "infra/forge/retire-access-object.sh",
            "{1}",
            "{2}",
            "{3}",
            "{4}"
        ])
    );
    assert_eq!(
        p["argv"],
        json!([
            "infra/forge/retire-access-object.sh",
            "--plan",
            "{1}",
            "{2}",
            "{3}"
        ])
    );
    assert_eq!(w["requires_approval"], json!(true));
    assert_eq!(w["plan_verb"], json!("plan-an-access-object-retirement"));
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert!(w["about"].as_str().unwrap().contains("MUTATING"));
    assert!(!p["about"].as_str().unwrap().contains("MUTATING"));
    assert_ne!(p["requires_approval"], json!(true));
    let pat = |v: &Value, i: usize| {
        regex::Regex::new(v["params"][i]["pattern"].as_str().unwrap()).unwrap()
    };
    for (i, bad) in [
        (1, "Boss-Dev"),
        (1, "-n"),
        (2, "a b"),
        (2, "-rf"),
        (2, "x/y"),
    ] {
        assert!(!pat(&w, i).is_match(bad), "param {i} admits {bad:?}");
        assert!(!pat(&p, i).is_match(bad), "plan param {i} admits {bad:?}");
    }
}

/// STRICT ABOUT WHAT CAN BE DELETED. Three floors, held to each other:
/// - this verb deletes exactly ServiceAccount, Role and RoleBinding, and
///   the allowlist's `one_of` (the words a packet may select) is the
///   script's own floor, word for word, in both verb files;
/// - delete-orphan-object — the verb an agent runs with NO approval —
///   keeps the floor it had, and shares no kind with this one. Widening
///   either is a reviewed change to these lines, and this test names it.
#[test]
fn the_floors_are_exact_and_nothing_new_is_deletable_without_a_passkey() {
    let ours = array_in("infra/forge/retire-access-object.sh", "RETIRABLE_KINDS");
    assert_eq!(
        ours,
        ["ServiceAccount", "Role", "RoleBinding"],
        "retire-access-object retires these three kinds and no other"
    );
    for v in ["retire-access-object", "plan-an-access-object-retirement"] {
        let one_of: Vec<String> = verb(v)["params"][0]["one_of"]
            .as_array()
            .unwrap_or_else(|| panic!("{v}'s first param is a one_of"))
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect();
        assert_eq!(one_of, ours, "{v}'s kinds are the script's floor");
        assert_eq!(verb(v)["params"][0]["name"], json!("kind"));
    }
    let unapproved = array_in("infra/forge/delete-orphan-object.sh", "DELETABLE_KINDS");
    assert_eq!(
        unapproved,
        ["Service", "ConfigMap", "Deployment", "CronJob", "Job"],
        "delete-orphan-object's floor, which needs no approval, is unchanged"
    );
    for k in &ours {
        assert!(
            !unapproved.contains(k),
            "{k} is deletable without a passkey through delete-orphan-object"
        );
    }
}
