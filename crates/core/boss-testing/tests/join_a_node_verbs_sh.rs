//! The three doors join-a-node's last three verifications read through
//! (backlog 01927489): `node-labels` and `node-pods`, READ-ONLY, and
//! `registry-probe`, which applies the tree's one-shot Job
//! `infra/forge/registry-probe.yaml`, waits, prints its whole log and
//! removes it.
//!
//! WHY. join-a-node aca68557 (w-3, the data node) stopped at
//! `verify-registry` on 2026-11-12: verify-boot had closed through doors,
//! and the next three steps — `image_pulled`, `labels` + `declared_in`,
//! `workload` + `evidence` — had none. w-2's join (829f75a8) closed them
//! with kubectl David ran by hand: `kubectl get node w-2 --show-labels`
//! and a hand-applied Job `w2-registry-probe` (nodeSelector, imagePullPolicy
//! Always). node-status printed conditions but no labels, and nothing
//! listed the pods on a node. A CLI line handed to David is a coverage
//! gap, so each read is a verb, and the probe is a file plus a script.
//!
//! Every test here runs the real script against a stub cluster behind
//! ops_kubectl and a file:// estate registry. Nothing here reaches a
//! cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const LABELS: &str = "infra/forge/node-labels.sh";
const PODS: &str = "infra/forge/node-pods.sh";
const PROBE: &str = "infra/forge/registry-probe.sh";
const MANIFEST: &str = "infra/forge/registry-probe.yaml";

/// The stand-in behind ops_kubectl's `sudo docker run … kubectl
/// --kubeconfig=/kc`. State under $STUB_STATE: `ip-<node>` its
/// InternalIP, `cp-<node>` a control-plane label, `unsched-<node>`
/// cordoned, `labels-<node>` extra labels as a JSON object, `taints-<node>`
/// a JSON array, `alloc-<node>` its status.allocatable as a JSON object
/// (`{}` for a Node that reports none), `pods-<node>` the pods on it (a JSON array; `podsfail`
/// makes the list fail), `job.json` the probe Job (made by the create, or
/// planted), `sent.yaml` what the create was handed, `polls` how often the
/// made Job was read. STUB_SCENARIO is the probe's life: ok | failed |
/// deadline | othernode | otherimage. STUB_DELETE: ok | refuse | noop.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
printf '%s\n' "$*" >> "$STUB_ARGV"
scenario="${STUB_SCENARIO:-ok}"
polls=0
[ -f "$S/polls" ] && polls=$(cat "$S/polls")
finished() { [ "$polls" -gt 1 ]; }
DIGEST=sha256:cdeda0da2cd6896023cc5f96505bc94a699a989dff2bfc3e2008ab92eab1e93a
node_json() {
    n="$1"
    [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
    labels='{"kubernetes.io/hostname":"'"$n"'","kubernetes.io/os":"linux"}'
    [ -f "$S/labels-$n" ] && labels=$(printf '%s' "$labels" | jq -c --argjson x "$(cat "$S/labels-$n")" '. + $x')
    [ -f "$S/cp-$n" ] && labels=$(printf '%s' "$labels" | jq -c '. + {"node-role.kubernetes.io/control-plane":""}')
    spec='{}'
    [ -f "$S/unsched-$n" ] && spec='{"unschedulable":true}'
    [ -f "$S/taints-$n" ] && spec=$(printf '%s' "$spec" | jq -c --argjson t "$(cat "$S/taints-$n")" '.taints = $t')
    alloc='{"cpu":"8","memory":"65240244Ki","pods":"110"}'
    [ -f "$S/alloc-$n" ] && alloc=$(cat "$S/alloc-$n")
    jq -n -c --arg n "$n" --argjson l "$labels" --argjson s "$spec" --arg ip "$(cat "$S/ip-$n")" --argjson a "$alloc" '{kind:"Node",metadata:{name:$n,labels:$l},spec:$s,
        status:{addresses:[{type:"InternalIP",address:$ip}],allocatable:$a,
            conditions:[{type:"Ready",status:"True",reason:"KubeletReady"}],
            nodeInfo:{kubeletVersion:"v1.36.2",osImage:"Talos (v1.13.8)",kernelVersion:"6.18.42-talos",containerRuntimeVersion:"containerd://2.1.4"}}}'
}
case "$1" in
get)
    case "$2" in
    node) node_json "$3"; exit 0 ;;
    job)
        [ "$3" = registry-probe ] || { echo "stub: get job $3" >&2; exit 2; }
        [ -f "$S/job.json" ] || exit 0
        if [ -f "$S/made" ]; then
            polls=$((polls + 1)); echo "$polls" > "$S/polls"
            cond='[]'
            if finished; then
                case "$scenario" in
                failed) cond='[{"type":"Failed","status":"True","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit"}]' ;;
                deadline) cond='[{"type":"Failed","status":"True","reason":"DeadlineExceeded","message":"Job was active longer than specified deadline"}]' ;;
                *) cond='[{"type":"Complete","status":"True"}]' ;;
                esac
            fi
            jq --argjson c "$cond" '.status.conditions = $c' "$S/job.json"
        else
            cat "$S/job.json"
        fi
        exit 0 ;;
    pods)
        case "$*" in
        *"--field-selector spec.nodeName="*)
            [ -f "$S/podsfail" ] && { echo 'Error from server (Forbidden): pods is forbidden' >&2; exit 1; }
            n=$(printf '%s\n' "$*" | sed -n 's/.*spec\.nodeName=\([^ ]*\).*/\1/p')
            printf '{"kind":"List","items":%s}\n' "$(cat "$S/pods-$n" 2>/dev/null || echo '[]')"; exit 0 ;;
        *"-l batch.kubernetes.io/controller-uid=6f1e9b99-0000-4000-8000-0000000000aa "*)
            printf '{"kind":"List","items":[{"metadata":{"name":"registry-probe-old11","creationTimestamp":"2026-11-12T02:40:00Z"},"spec":{"nodeName":"w-2"},"status":{"phase":"Succeeded","containerStatuses":[{"name":"probe","imageID":"x","state":{"terminated":{"exitCode":0}}}]}}]}\n'
            exit 0 ;;
        *"-l batch.kubernetes.io/controller-uid=6f1e9b99-0000-4000-8000-0000000000ae "*)
            on=w-3; [ "$scenario" = othernode ] && on=w-2
            id="reg.test/david/alpine-k8s@$DIGEST"
            [ "$scenario" = otherimage ] && id="reg.test/david/alpine-k8s@sha256:0000000000000000000000000000000000000000000000000000000000000000"
            if [ "$scenario" = deadline ]; then items='[]'
            elif ! finished; then items='[{"metadata":{"name":"registry-probe-x7k2p"},"spec":{"nodeName":"'"$on"'"},"status":{"phase":"Pending"}}]'
            else
                ec=0; ph=Succeeded
                [ "$scenario" = failed ] && { ec=1; ph=Failed; }
                items='[{"metadata":{"name":"registry-probe-x7k2p","creationTimestamp":"2026-11-12T17:00:00Z"},"spec":{"nodeName":"'"$on"'"},"status":{"phase":"'"$ph"'","containerStatuses":[{"name":"probe","imageID":"'"$id"'","state":{"terminated":{"exitCode":'"$ec"'}}}]}}]'
            fi
            printf '{"kind":"List","items":%s}\n' "$items"; exit 0 ;;
        *) echo "stub: pods by another selector: $*" >&2; exit 2 ;;
        esac ;;
    events)
        printf '{"kind":"List","items":[{"reason":"Pulling","message":"Pulling image \\"reg.test/david/alpine-k8s:1.33.3@%s\\"","lastTimestamp":"2026-11-12T17:00:01Z"},{"reason":"Pulled","message":"Successfully pulled image \\"reg.test/david/alpine-k8s:1.33.3@%s\\" in 2.1s (2.1s including waiting). Image size: 61234567 bytes.","lastTimestamp":"2026-11-12T17:00:03Z"}]}\n' "$DIGEST" "$DIGEST"
        exit 0 ;;
    esac ;;
create)
    case "$*" in
    "create -f - -n boss-dev -o json "*) ;;
    *) echo "stub: a create that is not the manifest on stdin into boss-dev: $*" >&2; exit 2 ;;
    esac
    cat > "$S/sent.yaml"
    if [ -f "$S/job.json" ]; then
        echo 'Error from server (AlreadyExists): jobs.batch "registry-probe" already exists' >&2
        exit 1
    fi
    printf '{"kind":"Job","metadata":{"name":"registry-probe","namespace":"boss-dev","uid":"6f1e9b99-0000-4000-8000-0000000000ae"},"status":{}}\n' > "$S/job.json"
    touch "$S/made"
    cat "$S/job.json"; exit 0 ;;
logs)
    case "$2" in
    registry-probe-old11) printf 'registry-probe: ran on node w-2, pod registry-probe-old11, as uid 65534: Alpine Linux v3.22\n' ;;
    registry-probe-x7k2p)
        on=w-3; [ "$scenario" = othernode ] && on=w-2
        printf 'registry-probe: ran on node %s, pod registry-probe-x7k2p, as uid 65534: Alpine Linux v3.22\n' "$on" ;;
    *) echo "stub: logs of $2" >&2; exit 2 ;;
    esac
    exit 0 ;;
delete)
    case "$*" in
    "delete --raw /apis/batch/v1/namespaces/boss-dev/jobs/registry-probe -f - "*) ;;
    *) echo "stub: a delete that is not the probe Job by its path: $*" >&2; exit 2 ;;
    esac
    cat > "$S/delete-body.json"
    want=$(jq -r .metadata.uid "$S/job.json")
    have=$(jq -r .preconditions.uid "$S/delete-body.json")
    if [ "${STUB_DELETE:-ok}" = refuse ] || [ "$have" != "$want" ]; then
        echo "Error from server (Conflict): Precondition failed: UID in precondition: $have, UID in object meta: $want" >&2
        exit 1
    fi
    [ "${STUB_DELETE:-ok}" = noop ] || rm -f "$S/job.json" "$S/made" "$S/polls"
    echo '{"kind":"Status","status":"Success"}'
    exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

fn needs_tools() {
    for tool in ["jq", "curl"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "join_a_node_verbs_sh: no {tool} on this box — the gate image has it, and a \
             test that cannot run must fail, never pass by returning early"
        );
    }
}

struct Cluster {
    dir: PathBuf,
}

fn node(id: &str, address: &str, role: &str, notes: Option<&str>) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 8, "memory_gb": 63, "disk_gb": 447, "notes": notes, "retired": false
    })
}

impl Cluster {
    /// The estate of 2026-11-12: a control plane, w-2, and w-3 — the
    /// data node, whose registry row declares boss.dev/purpose=database.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        write_exec(&dir.join("bin/sleep"), "#!/bin/sh\nexit 0\n");
        let c = Cluster { dir };
        write_file(
            &c.dir.join("nodes.json"),
            &json!({"data": [
                node("forge", "10.20.0.15", "forge", None),
                node("cp-1", "10.20.0.11", "talos-control-plane", None),
                node("w-2", "10.20.0.16", "talos-worker", Some("Utility worker (boss.dev/purpose=utility).")),
                node("w-3", "10.20.0.17", "talos-worker", Some("THE DATA NODE (boss.dev/purpose=database), design 3c475a19.")),
            ]})
            .to_string(),
        );
        for (n, ip) in [
            ("cp-1", "10.20.0.11"),
            ("w-2", "10.20.0.16"),
            ("w-3", "10.20.0.17"),
        ] {
            c.state(&format!("ip-{n}"), ip);
        }
        c.state("cp-cp-1", "");
        c.state("labels-w-3", r#"{"boss.dev/purpose":"database"}"#);
        c.state("labels-w-2", r#"{"boss.dev/purpose":"utility"}"#);
        c
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join("state").join(file)).unwrap_or_default()
    }

    fn has(&self, file: &str) -> bool {
        self.dir.join("state").join(file).exists()
    }

    fn run(&self, script: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(script))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", self.dir.join("nodes.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|l| !l.starts_with("get ") && !l.starts_with("logs "))
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

fn verb(name: &str) -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{name}.json"));
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The lines of `out` the registry-probe verb's `effect` matches, judged
/// by the runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("registry-probe")["effect"]
        .as_str()
        .expect("registry-probe declares an `effect`")
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

// ------------------------------------------------------------ node-labels

#[test]
fn node_labels_prints_every_label_and_compares_the_declared_purpose() {
    let c = Cluster::new("labels-ok");
    c.state(
        "taints-w-3",
        r#"[{"key":"boss.dev/data","value":"postgres","effect":"NoSchedule"}]"#,
    );
    let o = c.run(LABELS, &["w-3"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    for line in [
        "label boss.dev/purpose=database",
        "label kubernetes.io/hostname=w-3",
        "label kubernetes.io/os=linux",
        "taint boss.dev/data=postgres:NoSchedule",
        "declared: boss.dev/purpose=database (the estate registry's row for w-3, its notes)",
        "carried: boss.dev/purpose=database (the Node's label)",
        "node-labels: w-3 — 3 label(s), 1 taint(s); boss.dev/purpose declared database, carried database: AGREES",
    ] {
        assert!(out.contains(line), "missing {line:?}:\n{out}");
    }
    assert!(out.contains("kubelet v1.36.2"), "{out}");
    // Sorted, so two reads of one node print the same text.
    let labels: Vec<&str> = out.lines().filter(|l| l.starts_with("label ")).collect();
    let mut sorted = labels.clone();
    sorted.sort();
    assert_eq!(labels, sorted);
    assert!(c.writes().is_empty(), "READ-ONLY: {:?}", c.calls());
}

#[test]
fn node_labels_says_when_the_node_and_the_registry_disagree() {
    let c = Cluster::new("labels-disagree");
    c.state("labels-w-3", r#"{"boss.dev/purpose":"utility"}"#);
    let out = text(&c.run(LABELS, &["w-3"], &[]));
    assert!(
        out.contains("boss.dev/purpose declared database, carried utility: DISAGREES"),
        "{out}"
    );
    c.state("labels-w-3", "{}");
    let out = text(&c.run(LABELS, &["w-3"], &[]));
    assert!(
        out.contains("boss.dev/purpose declared database, carried none: DISAGREES"),
        "{out}"
    );
    // A control plane may be READ; its registry row declares no purpose.
    let out = text(&c.run(LABELS, &["cp-1"], &[]));
    assert!(
        out.contains("boss.dev/purpose declared none, carried none: UNDECLARED"),
        "{out}"
    );
}

#[test]
fn node_labels_refuses_what_the_registry_does_not_declare_a_talos_node() {
    let c = Cluster::new("labels-bound");
    for (args, want) in [
        (vec!["w-9"], "holds 0 node(s) with id 'w-9'"),
        (vec!["forge"], "not a Talos node"),
        (vec!["W-3"], "one lowercase DNS label"),
        (vec!["w-3", "w-2"], "usage"),
    ] {
        let o = c.run(LABELS, &args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
        assert!(text(&o).contains(want), "{args:?}: {}", text(&o));
    }
}

// -------------------------------------------------------------- node-pods

#[test]
fn node_pods_lists_namespace_name_phase_and_image_of_every_pod_on_the_node() {
    let c = Cluster::new("pods-ok");
    c.state(
        "pods-w-3",
        &json!([
            {"metadata": {"name": "cilium-q2x8z", "namespace": "kube-system",
                "ownerReferences": [{"kind": "DaemonSet", "name": "cilium", "controller": true}]},
             "spec": {"nodeName": "w-3",
                "initContainers": [{"name": "config", "image": "quay.io/cilium/cilium:v1.18.1"}],
                "containers": [{"name": "agent", "image": "quay.io/cilium/cilium:v1.18.1"}]},
             "status": {"phase": "Running"}},
            {"metadata": {"name": "registry-probe-x7k2p", "namespace": "boss-dev",
                "ownerReferences": [{"kind": "Job", "name": "registry-probe", "controller": true}]},
             "spec": {"nodeName": "w-3",
                "containers": [{"name": "probe", "image": "reg.test/david/alpine-k8s:1.33.3"}]},
             "status": {"phase": "Succeeded"}},
            {"metadata": {"name": "engine-image-ei-1", "namespace": "longhorn-system",
                "ownerReferences": [{"kind": "DaemonSet", "name": "engine-image-ei", "controller": true}]},
             "spec": {"nodeName": "w-3",
                "containers": [{"name": "a", "image": "longhornio/longhorn-engine:v1.9.1"},
                               {"name": "b", "image": "longhornio/longhorn-share-manager:v1.9.1"}]},
             "status": {"phase": "Running"}}
        ])
        .to_string(),
    );
    let o = c.run(PODS, &["w-3"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let pods: Vec<&str> = out.lines().filter(|l| l.starts_with("pod ")).collect();
    assert_eq!(
        pods,
        vec![
            "pod boss-dev/registry-probe-x7k2p Succeeded image reg.test/david/alpine-k8s:1.33.3 owner Job registry-probe",
            "pod kube-system/cilium-q2x8z Running image quay.io/cilium/cilium:v1.18.1 init quay.io/cilium/cilium:v1.18.1 owner DaemonSet cilium",
            "pod longhorn-system/engine-image-ei-1 Running image longhornio/longhorn-engine:v1.9.1,longhornio/longhorn-share-manager:v1.9.1 owner DaemonSet engine-image-ei",
        ],
        "{out}"
    );
    assert!(
        out.contains("node-pods: w-3 — 3 pod(s): 2 Running, 1 Succeeded"),
        "{out}"
    );
    // The read is the API server's own filter on the node, never a scan.
    assert!(
        c.calls()
            .iter()
            .any(|l| l.starts_with("get pods -A --field-selector spec.nodeName=w-3 ")),
        "{:?}",
        c.calls()
    );
    assert!(c.writes().is_empty(), "READ-ONLY: {:?}", c.calls());
}

#[test]
fn node_pods_never_answers_none_for_a_node_it_could_not_read() {
    let c = Cluster::new("pods-unread");
    // A node with nothing on it says so ...
    let o = c.run(PODS, &["w-2"], &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("node-pods: w-2 — 0 pod(s): none"),
        "{}",
        text(&o)
    );
    // ... a list that failed is not an empty node ...
    c.state("podsfail", "");
    let o = c.run(PODS, &["w-2"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("CANNOT ANSWER"), "{}", text(&o));
    // ... and a node the cluster does not hold is not an empty one either:
    // a wrong target answers `0 pods` through a field selector.
    let c = Cluster::new("pods-nonode");
    std::fs::remove_file(c.dir.join("state/ip-w-3")).expect("rm");
    let o = c.run(PODS, &["w-3"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("not found"), "{}", text(&o));
}

/// THE ALLOCATED BLOCK (backlog 2b7c78f8). On 2026-11-21 builders were
/// refused `Insufficient cpu` on w-1 while the record said it had room,
/// and nothing the pod could reach printed what held the box: node-pods
/// listed pods with no requests, and `kubectl describe node`'s Allocated
/// resources block is a read of the Node the dev pod is forbidden. The
/// pods fixture is the live shapes of that afternoon: a gate pod (the
/// postgres NATIVE sidecar, restartPolicy Always, beside the gate init
/// container, then the receipt container), a dev-build worker (worker +
/// its database, and a non-sidecar init), Longhorn's instance manager, a
/// DaemonSet pod with no requests, and a Succeeded gate the scheduler no
/// longer counts.
#[test]
fn node_pods_prints_each_pods_effective_request_and_the_nodes_allocated_block() {
    let c = Cluster::new("pods-requests");
    c.state(
        "alloc-w-3",
        r#"{"cpu":"31800m","memory":"65240244Ki","pods":"110"}"#,
    );
    let req = |cpu: &str, mem: &str| json!({"requests": {"cpu": cpu, "memory": mem}});
    c.state(
        "pods-w-3",
        &json!([
            {"metadata": {"name": "gate-aa", "namespace": "boss-dev"},
             "spec": {"nodeName": "w-3",
                "initContainers": [
                    {"name": "postgres", "image": "pg", "restartPolicy": "Always", "resources": req("250m", "3Gi")},
                    {"name": "gate", "image": "ci", "resources": req("4", "8Gi")}],
                "containers": [{"name": "receipt", "image": "ci", "resources": req("100m", "256Mi")}]},
             "status": {"phase": "Pending"}},
            {"metadata": {"name": "dev-build-bb", "namespace": "boss-dev"},
             "spec": {"nodeName": "w-3",
                "initContainers": [{"name": "seed", "image": "ci", "resources": req("1", "512Mi")}],
                "containers": [
                    {"name": "worker", "image": "ci", "resources": req("2", "4Gi")},
                    {"name": "postgres", "image": "pg", "resources": req("500m", "1Gi")}]},
             "status": {"phase": "Running"}},
            {"metadata": {"name": "instance-manager-1", "namespace": "longhorn-system"},
             "spec": {"nodeName": "w-3",
                "containers": [{"name": "im", "image": "lh", "resources": {"requests": {"cpu": "3840m"}}}]},
             "status": {"phase": "Running"}},
            {"metadata": {"name": "cilium-envoy-1", "namespace": "kube-system"},
             "spec": {"nodeName": "w-3", "containers": [{"name": "envoy", "image": "envoy"}]},
             "status": {"phase": "Running"}},
            {"metadata": {"name": "gate-old", "namespace": "boss-dev"},
             "spec": {"nodeName": "w-3",
                "initContainers": [{"name": "gate", "image": "ci", "resources": req("4", "8Gi")}],
                "containers": [{"name": "receipt", "image": "ci"}]},
             "status": {"phase": "Succeeded"}}
        ])
        .to_string(),
    );
    let o = c.run(PODS, &["w-3"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let requests: Vec<&str> = out.lines().filter(|l| l.starts_with("requests ")).collect();
    assert_eq!(
        requests,
        vec![
            "requests boss-dev/gate-aa Pending cpu 4.25 memory 11Gi",
            "requests longhorn-system/instance-manager-1 Running cpu 3.84 memory 0Gi",
            "requests boss-dev/dev-build-bb Running cpu 2.5 memory 5Gi",
            "requests kube-system/cilium-envoy-1 Running cpu 0 memory 0Gi",
        ],
        "largest CPU first; the Succeeded gate holds nothing: {out}"
    );
    let allocated: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("allocated "))
        .collect();
    assert_eq!(
        allocated,
        vec![
            "allocated cpu 10.59 of 31.8 allocatable (33%) on 4 pod(s)",
            "allocated memory 16Gi of 62.22Gi allocatable (25%)",
        ],
        "{out}"
    );
    // The closing count stays the last line, as join-a-node reads it.
    assert!(
        out.contains("node-pods: w-3 — 5 pod(s): 1 Pending, 3 Running, 1 Succeeded"),
        "{out}"
    );
    assert!(c.writes().is_empty(), "READ-ONLY: {:?}", c.calls());
}

/// No evidence is not a pass: a Node that reports no allocatable, or a
/// request the reader cannot parse, is CANNOT ANSWER — never a sum that
/// silently counted it as nothing.
#[test]
fn node_pods_cannot_answer_an_allocation_it_could_not_read() {
    let c = Cluster::new("pods-noalloc");
    c.state("alloc-w-3", "{}");
    let o = c.run(PODS, &["w-3"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("CANNOT ANSWER — the Node w-3 reports no allocatable"),
        "{}",
        text(&o)
    );
    let c = Cluster::new("pods-badqty");
    c.state(
        "pods-w-3",
        &json!([{"metadata": {"name": "odd", "namespace": "x"},
            "spec": {"nodeName": "w-3", "containers": [{"name": "c", "image": "i",
                "resources": {"requests": {"cpu": "two"}}}]},
            "status": {"phase": "Running"}}])
        .to_string(),
    );
    let o = c.run(PODS, &["w-3"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("CANNOT ANSWER"), "{}", text(&o));
    assert!(text(&o).contains("two"), "names the quantity: {}", text(&o));
}

// --------------------------------------------------------- registry-probe

fn documents(rel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    yaml_serde::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).unwrap_or_else(|e| panic!("{rel} parses as YAML: {e}")))
        .collect()
}

#[test]
fn the_probe_is_one_named_bounded_job_pinned_to_one_worker_that_must_pull() {
    let docs = documents(MANIFEST);
    assert_eq!(docs.len(), 1, "{MANIFEST} declares exactly one object");
    let job = &docs[0];
    assert_eq!(job["kind"], "Job");
    // A FIXED name: the API server refuses a second probe while one stands.
    assert_eq!(job["metadata"]["name"], "registry-probe");
    assert_eq!(job["metadata"]["namespace"], "boss-dev");
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert!(
        job["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .is_some_and(|s| s > 0 && s <= 300),
        "{}",
        job["spec"]["activeDeadlineSeconds"]
    );
    assert_eq!(job["spec"]["ttlSecondsAfterFinished"], 3600);
    let pod = &job["spec"]["template"]["spec"];
    assert_eq!(pod["restartPolicy"], "Never");
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert!(pod["volumes"].is_null(), "no volume: {pod}");
    assert!(pod["initContainers"].is_null());
    let exprs = pod["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
        ["nodeSelectorTerms"][0]["matchExpressions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(exprs.contains(&json!({"key": "kubernetes.io/hostname", "operator": "In", "values": ["$REGISTRY_PROBE_NODE"]})), "{exprs:?}");
    assert!(
        exprs.contains(
            &json!({"key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist"})
        ),
        "{exprs:?}"
    );
    let cs = pod["containers"].as_array().cloned().unwrap_or_default();
    assert_eq!(cs.len(), 1);
    let probe = &cs[0];
    // The pull is the point: never served from the node's cache alone, and
    // the image is named by digest, so the imageID the kubelet reports can
    // be held to it.
    assert_eq!(probe["imagePullPolicy"], "Always");
    let image = probe["image"].as_str().expect("image");
    assert!(
        image.starts_with("$BOSS_FORGE_REGISTRY_HOST/") && image.contains("@sha256:"),
        "the forge registry, filled in at apply, by digest: {image}"
    );
    assert_eq!(probe["securityContext"]["allowPrivilegeEscalation"], false);
    assert_eq!(probe["securityContext"]["readOnlyRootFilesystem"], true);
    assert_eq!(
        probe["securityContext"]["capabilities"]["drop"],
        json!(["ALL"])
    );
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
}

#[test]
fn registry_probe_applies_the_tree_s_job_prints_its_log_removes_it_and_states_the_effect() {
    let c = Cluster::new("probe-ok");
    let o = c.run(PROBE, &["w-3"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let want = std::fs::read_to_string(repo_root().join(MANIFEST))
        .expect("manifest")
        .replace("$REGISTRY_PROBE_NODE", "w-3")
        .replace("$BOSS_FORGE_REGISTRY_HOST", "reg.test");
    assert_eq!(
        c.read("sent.yaml"),
        want,
        "the tree's manifest, the node and the registry filled in"
    );
    for line in [
        "    registry-probe: ran on node w-3, pod registry-probe-x7k2p, as uid 65534: Alpine Linux v3.22",
        "event Pulled: Successfully pulled image",
    ] {
        assert!(out.contains(line), "missing {line:?}:\n{out}");
    }
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "exactly one effect line:\n{out}");
    assert!(
        hits[0]
            .starts_with("registry-probe: PULLED reg.test/david/alpine-k8s:1.33.3@sha256:cdeda0da"),
        "{hits:?}"
    );
    // It removes what it made, by the uid it made, and reads it back gone.
    let body: Value = serde_json::from_str(&c.read("delete-body.json")).expect("delete body");
    assert_eq!(
        body["preconditions"]["uid"],
        "6f1e9b99-0000-4000-8000-0000000000ae"
    );
    assert!(!c.has("job.json"), "the Job is gone");
    let writes = c.writes();
    assert_eq!(writes.len(), 2, "one create and one delete: {writes:?}");
}

#[test]
fn a_probe_that_did_not_pull_is_failed_and_still_removed() {
    for (scenario, why) in [
        ("failed", "BackoffLimitExceeded"),
        ("deadline", "DeadlineExceeded"),
        ("othernode", "ran on 'w-2', not w-3"),
        ("otherimage", "not the digest"),
    ] {
        let c = Cluster::new(&format!("probe-{scenario}"));
        let o = c.run(PROBE, &["w-3"], &[("STUB_SCENARIO", scenario)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{scenario}: {out}");
        assert!(out.contains(why), "{scenario}: {why:?}:\n{out}");
        assert!(
            effect_lines(&out).is_empty(),
            "{scenario}: no effect:\n{out}"
        );
        assert!(
            !c.has("job.json"),
            "{scenario}: its own Job is removed anyway"
        );
    }
}

#[test]
fn a_probe_whose_job_will_not_go_is_not_an_effect() {
    let c = Cluster::new("probe-stuck");
    let o = c.run(PROBE, &["w-3"], &[("STUB_DELETE", "noop")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("not read back gone"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn registry_probe_never_touches_a_probe_still_running_and_removes_a_finished_one_first() {
    let c = Cluster::new("probe-running");
    c.state(
        "job.json",
        &json!({"kind": "Job", "metadata": {"name": "registry-probe", "namespace": "boss-dev",
            "uid": "6f1e9b99-0000-4000-8000-0000000000aa", "creationTimestamp": "2026-11-12T02:40:00Z"},
            "status": {"active": 1}})
        .to_string(),
    );
    let o = c.run(PROBE, &["w-3"], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(text(&o).contains("has NOT finished"), "{}", text(&o));
    assert!(c.writes().is_empty(), "{:?}", c.calls());

    let c = Cluster::new("probe-leftover");
    c.state(
        "job.json",
        &json!({"kind": "Job", "metadata": {"name": "registry-probe", "namespace": "boss-dev",
            "uid": "6f1e9b99-0000-4000-8000-0000000000aa", "creationTimestamp": "2026-11-12T02:40:00Z"},
            "status": {"conditions": [{"type": "Complete", "status": "True"}]}})
        .to_string(),
    );
    let o = c.run(PROBE, &["w-3"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    // Capture before delete: the leftover's log is on this run.
    assert!(
        out.contains("registry-probe: ran on node w-2, pod registry-probe-old11"),
        "{out}"
    );
    let writes = c.writes();
    assert_eq!(
        writes.len(),
        3,
        "delete the leftover, create, delete: {writes:?}"
    );
    assert!(writes[0].starts_with("delete "), "{writes:?}");
    assert!(writes[1].starts_with("create "), "{writes:?}");
}

#[test]
fn registry_probe_bounds_the_node_like_every_mutating_node_verb() {
    let c = Cluster::new("probe-bound");
    c.state("unsched-w-2", "");
    for (node, want) in [
        ("cp-1", "not a talos-worker"),
        ("forge", "not a talos-worker"),
        ("w-9", "holds 0 node(s)"),
        ("w-2", "cordoned"),
    ] {
        let o = c.run(PROBE, &[node], &[]);
        assert_eq!(o.status.code(), Some(78), "{node}: {}", text(&o));
        assert!(text(&o).contains(want), "{node}: {want:?}: {}", text(&o));
    }
    // No registry host, no image to name: refused before any door.
    let o = c.run(PROBE, &["w-3"], &[("BOSS_FORGE_REGISTRY_HOST", "")]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(c.writes().is_empty(), "{:?}", c.calls());
}

// ------------------------------------------------------------- the verbs

#[test]
fn the_verb_files_say_what_they_write_and_take_one_node() {
    for name in ["node-labels", "node-pods", "registry-probe"] {
        let v = verb(name);
        assert_eq!(v["hosts"], json!(["forge"]), "{name}");
        assert_eq!(
            v["argv"],
            json!([format!("infra/forge/{name}.sh"), "{1}"]),
            "{name}"
        );
        let params = v["params"].as_array().cloned().unwrap_or_default();
        assert_eq!(params.len(), 1, "{name}: one param, the node");
        assert_eq!(params[0]["name"], "node", "{name}");
        let about = v["about"].as_str().unwrap_or_default();
        if name == "registry-probe" {
            assert!(about.starts_with("MUTATING"), "{name}: {about}");
            assert!(v["effect"].is_string(), "{name} declares its effect");
            // Its Job's deadline, the wait's margin, the door's first pull
            // and the reads all fit inside the runner's timeout.
            let deadline = documents(MANIFEST)[0]["spec"]["activeDeadlineSeconds"]
                .as_u64()
                .expect("deadline");
            assert!(
                v["timeout"]
                    .as_u64()
                    .is_some_and(|t| t >= deadline + 60 + 120 + 120),
                "{name}: timeout {} for a {deadline}s deadline",
                v["timeout"]
            );
        } else {
            assert!(about.starts_with("READ-ONLY"), "{name}: {about}");
            assert!(!about.contains("MUTATING"), "{name}");
        }
    }
}
