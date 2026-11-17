//! The ops verb `prove-gate-volume` (`infra/forge/prove-gate-volume.sh`,
//! backlog ae59464b) and the Job it applies,
//! `infra/gate-runner/gate-volume-proof.yaml`.
//!
//! WHY. `infra/gate-runner/run.sh`'s gate-disk guard refuses every gate
//! whose runs directory carries no `/gate-runs/.gate-volume` — the marker
//! a proof Job writes once it has shown the claim `gate` is the gate disk
//! and not the install disk's EPHEMERAL. The only proof ever run (backlog
//! 52ea56ac, 2026-09-30, Job gate-volume-proof-4dss4 on w-1) was made by
//! hand from a run and is not in the tree, so a fresh gate volume — the
//! move of w-1's gate volume onto a Micron 7400 PRO David ordered on
//! 2026-11-07 — would stop every gate with no door to start them again.
//! This is that door, as data plus one script:
//!
//! - the Job is a FILE, and these tests hold it to what run.sh reads: the
//!   claim and its two mounts, the identity, the image, and the three
//!   paths the guard compares (the runs directory, the seed, the file on
//!   EPHEMERAL) — each read out of run.sh and gate-runner.yaml, never
//!   typed here, so the proof cannot prove a layout the gate does not use;
//! - the Job's own script refuses, writing no marker, when the seed and
//!   the runs directory are two filesystems or the runs directory is
//!   EPHEMERAL's — run here for real against scratch directories;
//! - the verb's script, against a stub cluster behind ops_kubectl, bounds
//!   the node the way every node verb does, refuses an unbound claim, a
//!   gate in flight and a proof still running, removes a FINISHED previous
//!   proof only after printing its whole log and only by its uid, prints
//!   the new Job's whole log, and prints its effect line only when the Job
//!   completed AND its log reads the marker back naming the node and the
//!   gate disk. (The removal exists because every Job here keeps its
//!   finished history an hour — every_job_bounds_its_finished_history.rs
//!   — and a retry must not wait that long with every gate stopped.)
//!
//! Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/prove-gate-volume.sh";
const MANIFEST: &str = "infra/gate-runner/gate-volume-proof.yaml";
const RUNNER: &str = "infra/gate-runner/gate-runner.yaml";
const RUN_SH: &str = "infra/gate-runner/run.sh";
const VERB: &str = "prove-gate-volume";

// ------------------------------------------------------------ the manifest

fn documents(rel: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    yaml_serde::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).unwrap_or_else(|e| panic!("{rel} parses as YAML: {e}")))
        .collect()
}

fn the_job() -> Value {
    let docs = documents(MANIFEST);
    assert_eq!(docs.len(), 1, "{MANIFEST} declares exactly one object");
    docs.into_iter().next().expect("one document")
}

/// The gate runner's `gate` container (an init container since the
/// judged-init layout, a container before it) and its pod spec.
fn the_gate() -> (Value, Value) {
    let job = documents(RUNNER)
        .into_iter()
        .find(|d| d["kind"] == "Job")
        .unwrap_or_else(|| panic!("{RUNNER} declares the gate Job"));
    let pod = job["spec"]["template"]["spec"].clone();
    let gate = ["initContainers", "containers"]
        .iter()
        .flat_map(|k| pod[*k].as_array().cloned().unwrap_or_default())
        .find(|c| c["name"] == "gate")
        .unwrap_or_else(|| panic!("{RUNNER} has a container named gate"));
    (pod, gate)
}

/// The claim a pod spec mounts under the volume `name`.
fn claim_of(pod: &Value, name: &str) -> Value {
    pod["volumes"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["name"] == name))
        .map(|v| v["persistentVolumeClaim"]["claimName"].clone())
        .unwrap_or(Value::Null)
}

/// The mount of `path` in a container, as (volume, subPath).
fn mount_of(container: &Value, path: &str) -> Option<(String, String)> {
    container["volumeMounts"].as_array().and_then(|ms| {
        ms.iter().find(|m| m["mountPath"] == path).map(|m| {
            (
                m["name"].as_str().unwrap_or_default().to_string(),
                m["subPath"].as_str().unwrap_or_default().to_string(),
            )
        })
    })
}

/// run.sh's own call of the guard: the words after `gate_disk_guard`
/// on the line that runs it — `<GATE_DISK> <runs dir> <seed> <workspace>
/// <a file on EPHEMERAL> <POD_NAME>`.
fn the_guard_call() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join(RUN_SH)).expect("run.sh");
    let line = text
        .lines()
        .find(|l| l.contains("GATE_DISK_REFUSAL=$(gate_disk_guard "))
        .expect("run.sh calls gate_disk_guard into GATE_DISK_REFUSAL");
    let args = line
        .split("gate_disk_guard ")
        .nth(1)
        .and_then(|r| r.split(')').next())
        .expect("the call's arguments");
    args.split_whitespace().map(str::to_string).collect()
}

fn the_proof_container() -> Value {
    let job = the_job();
    let cs = job["spec"]["template"]["spec"]["containers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(cs.len(), 1, "the proof Job runs one container: {cs:?}");
    assert!(
        job["spec"]["template"]["spec"]["initContainers"].is_null(),
        "and no init container"
    );
    cs[0].clone()
}

/// The proof's own script, as the pod runs it: `bash -c <args[0]>`.
fn the_proof_script() -> String {
    let c = the_proof_container();
    assert_eq!(c["command"], json!(["/bin/bash", "-c"]), "{c}");
    let args = c["args"].as_array().cloned().unwrap_or_default();
    assert_eq!(args.len(), 1, "one script argument");
    args[0].as_str().expect("the script is text").to_string()
}

#[test]
fn the_proof_job_is_one_named_job_that_cleans_up_after_itself() {
    let job = the_job();
    assert_eq!(job["apiVersion"], "batch/v1");
    assert_eq!(job["kind"], "Job");
    // A FIXED name, so the API server itself refuses a second proof while
    // one stands (AlreadyExists) — the verb's in-flight check is not the
    // only thing between two proofs writing one marker.
    assert_eq!(job["metadata"]["name"], "gate-volume-proof");
    assert!(job["metadata"]["generateName"].is_null());
    assert_eq!(job["metadata"]["namespace"], "boss-dev");
    assert_eq!(job["spec"]["backoffLimit"], 0, "one attempt, never retried");
    assert!(
        job["spec"]["activeDeadlineSeconds"]
            .as_u64()
            .is_some_and(|s| s > 0 && s <= 600),
        "a bounded deadline: {}",
        job["spec"]["activeDeadlineSeconds"]
    );
    // A finished proof removes itself, an hour on (the tree's floor for a
    // Job's finished history); the verb removes it sooner, after capture.
    assert!(
        job["spec"]["ttlSecondsAfterFinished"]
            .as_u64()
            .is_some_and(|s| s == 3600),
        "a finished proof removes itself in an hour: {}",
        job["spec"]["ttlSecondsAfterFinished"]
    );
    let pod = &job["spec"]["template"]["spec"];
    assert_eq!(pod["restartPolicy"], "Never");
    assert_eq!(pod["automountServiceAccountToken"], false);
}

#[test]
fn the_proof_mounts_the_gate_claim_the_way_the_gate_does() {
    let (gate_pod, gate) = the_gate();
    let job = the_job();
    let pod = &job["spec"]["template"]["spec"];
    let proof = the_proof_container();
    let call = the_guard_call();
    let (runs, seed) = (call[1].clone(), call[2].clone());
    assert_eq!(runs, "/gate-runs", "run.sh's guard reads {call:?}");
    for path in [&seed, &runs] {
        let want =
            mount_of(&gate, path).unwrap_or_else(|| panic!("the gate container mounts {path}"));
        let have = mount_of(&proof, path)
            .unwrap_or_else(|| panic!("the proof mounts {path}, which run.sh's guard reads"));
        assert_eq!(have, want, "{path}: the proof mounts what the gate mounts");
        assert_eq!(
            claim_of(pod, &have.0),
            claim_of(&gate_pod, &want.0),
            "{path}: the same claim"
        );
    }
    assert_eq!(claim_of(pod, "gate-disk"), json!("gate"));
    // ONLY those two: no workspace, no other volume to write.
    let mounts = proof["volumeMounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(mounts.len(), 2, "{mounts:?}");
    assert_eq!(pod["volumes"].as_array().map(Vec::len), Some(1), "{pod}");
}

#[test]
fn the_proof_runs_as_the_gate_runs() {
    let (gate_pod, gate) = the_gate();
    let job = the_job();
    let pod = &job["spec"]["template"]["spec"];
    let proof = the_proof_container();
    for key in ["runAsUser", "runAsGroup"] {
        assert_eq!(
            proof["securityContext"][key], gate["securityContext"][key],
            "{key}: the proof writes as the gate writes"
        );
    }
    assert_eq!(proof["securityContext"]["runAsUser"], 65534);
    assert_eq!(proof["securityContext"]["runAsGroup"], 1500);
    assert_eq!(
        pod["securityContext"]["fsGroup"],
        gate_pod["securityContext"]["fsGroup"]
    );
    assert_eq!(pod["securityContext"]["fsGroup"], 1500);
    // The restricted profile, met as the gate meets it.
    assert_eq!(pod["securityContext"]["runAsNonRoot"], true);
    assert_eq!(
        pod["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(proof["securityContext"]["allowPrivilegeEscalation"], false);
    assert_eq!(
        proof["securityContext"]["capabilities"]["drop"],
        json!(["ALL"])
    );
    // The gate's own image: resident on the build node, GNU cp with
    // --reflink, and it moves when the gate's does (CLAUDE.md §9a).
    assert_eq!(proof["image"], gate["image"]);
    assert_eq!(pod["imagePullSecrets"], gate_pod["imagePullSecrets"]);
}

#[test]
fn the_proof_is_pinned_to_the_node_the_verb_names_and_never_a_control_plane() {
    let job = the_job();
    let terms = job["spec"]["template"]["spec"]["affinity"]["nodeAffinity"]
        ["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(terms.len(), 1, "one term, so both expressions must hold");
    let exprs = terms[0]["matchExpressions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        exprs.contains(&json!({"key": "kubernetes.io/hostname", "operator": "In", "values": ["$GATE_PROOF_NODE"]})),
        "the node is the verb's, substituted at apply: {exprs:?}"
    );
    assert!(
        exprs.contains(
            &json!({"key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist"})
        ),
        "never a control plane: {exprs:?}"
    );
    let text = std::fs::read_to_string(repo_root().join(MANIFEST)).expect("manifest");
    let code: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert_eq!(
        code.iter()
            .filter(|l| l.contains("$GATE_PROOF_NODE"))
            .count(),
        1,
        "the node placeholder is the one substitution"
    );
}

#[test]
fn the_proof_writes_the_marker_run_sh_reads_and_compares_what_it_compares() {
    let call = the_guard_call();
    let runsh = std::fs::read_to_string(repo_root().join(RUN_SH)).expect("run.sh");
    assert!(
        runsh.contains("[ ! -f \"$2/.gate-volume\" ]"),
        "run.sh's guard reads the marker as <runs dir>/.gate-volume"
    );
    let script = the_proof_script();
    for (var, want) in [
        ("SEED", &call[2]),
        ("RUNS", &call[1]),
        ("EPHEMERAL", &call[4]),
    ] {
        let line = format!("{var}=\"${{PROOF_{var}:-{want}}}\"");
        assert!(
            script.lines().any(|l| l.trim() == line),
            "the proof's {var} defaults to run.sh's {want}: wanted the line {line:?}"
        );
    }
    assert!(
        script
            .lines()
            .any(|l| l.trim() == "MARKER=\"$RUNS/.gate-volume\""),
        "the marker is <runs dir>/.gate-volume, as run.sh reads it"
    );
    // The order is the proof: devices, then the write, the reflink and the
    // compare, and the marker only after all three.
    let at = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("the proof script carries {needle:?}"))
    };
    let devices = at("stat -c %d \"$EPHEMERAL\"");
    let reflink = at("cp --reflink=always");
    let compare = at("cmp ");
    let marker = at("mv -f \"$MARKER.$POD\" \"$MARKER\"");
    assert!(devices < reflink && reflink < compare && compare < marker);
    assert!(script.contains("65534:1500"), "the identity is checked");
    assert!(script.contains("df "), "the df is printed");
}

/// Run the proof's own script with its three paths moved to `seed`,
/// `runs` and `eph` (its PROOF_* seams). Returns the output.
fn run_proof(seed: &Path, runs: &Path, eph: &Path) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(the_proof_script())
        .env("PROOF_SEED", seed)
        .env("PROOF_RUNS", runs)
        .env("PROOF_EPHEMERAL", eph)
        .env("NODE_NAME", "w-1")
        .env("POD_NAME", "gate-volume-proof-x7k2p")
        .output()
        .expect("bash runs")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn the_proof_refuses_the_install_disk_and_writes_no_marker() {
    // Seed, runs and the "EPHEMERAL" file all on one filesystem: what a
    // node without its gate disk looks like, the empty mountpoint on
    // EPHEMERAL bound in by the local PV.
    let dir = scratch_dir("proof-on-ephemeral");
    let (seed, runs) = (dir.join("seed"), dir.join("runs"));
    std::fs::create_dir_all(&seed).expect("seed");
    std::fs::create_dir_all(&runs).expect("runs");
    write_file(&dir.join("hosts"), "127.0.0.1 localhost\n");
    let o = run_proof(&seed, &runs, &dir.join("hosts"));
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("gate-volume-proof: REFUSED"), "{out}");
    assert!(out.contains("EPHEMERAL"), "{out}");
    assert!(!out.contains("PROOF-OK"), "{out}");
    assert!(!runs.join(".gate-volume").exists(), "no marker: {out}");
    assert_eq!(
        std::fs::read_dir(&seed).expect("seed").count()
            + std::fs::read_dir(&runs).expect("runs").count(),
        0,
        "nothing written before the devices were compared: {out}"
    );
}

#[test]
fn the_proof_refuses_a_seed_and_runs_on_two_filesystems() {
    // /proc is another device than any scratch directory.
    let dir = scratch_dir("proof-two-filesystems");
    let runs = dir.join("runs");
    std::fs::create_dir_all(&runs).expect("runs");
    let o = run_proof(Path::new("/proc/self"), &runs, Path::new("/proc/version"));
    let out = text(&o);
    assert!(!o.status.success(), "{out}");
    assert!(out.contains("gate-volume-proof: REFUSED"), "{out}");
    assert!(out.contains("different filesystems"), "{out}");
    assert!(!runs.join(".gate-volume").exists(), "no marker: {out}");
}

// ------------------------------------------------------------- the verb

/// The stand-in behind ops_kubectl's `sudo docker run … kubectl
/// --kubeconfig=/kc`. State under $STUB_STATE: `ip-<node>` its
/// InternalIP, `cp-<node>` a control-plane label, `unsched-<node>`
/// cordoned, `pvc.json` the claim (absent answers nothing, as
/// --ignore-not-found does), `pv.json` its volume, `pods.json` the
/// namespace's pods, `gatejobs.json` its gate-runner Jobs, `job.json` the
/// proof Job (made by the create, or planted), `sent.yaml` what the create
/// was handed on stdin, `polls` how often the made Job was read.
/// STUB_SCENARIO is the proof's life: ok | refused | deadline | othernode |
/// ephemeral | nomarker. STUB_RAN_ON: the node its pod ran on (w-1).
/// STUB_DELETE is the delete of a planted previous proof (uid ...aa, whose
/// one pod is gate-volume-proof-old11): ok | refuse (a precondition
/// failure) | noop (answers success, removes nothing); any delete whose
/// body's preconditions.uid is not the standing Job's is refused too.
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
log_ok() {
    dev_runs=66310; node=w-1
    [ "$scenario" = ephemeral ] && dev_runs=66308
    [ "$scenario" = othernode ] && node=w-2
    printf 'gate-volume-proof: running as uid 65534 gid 1500 on node w-1, pod gate-volume-proof-x7k2p\n'
    printf 'gate-volume-proof: devices gate-seed=%s gate-runs=%s etc-hosts(EPHEMERAL)=66308\n' "$dev_runs" "$dev_runs"
    printf 'gate-volume-proof: filesystem xfs on /dev/nvme2n1\n'
    printf 'gate-volume-proof: df: Filesystem      Size  Used Avail Use%% Mounted on\n'
    printf 'gate-volume-proof: df: /dev/nvme2n1    3.5T   25G  3.5T   1%% /gate-seed\n'
    printf 'gate-volume-proof: df: /dev/nvme2n1    3.5T   25G  3.5T   1%% /gate-runs\n'
    printf 'gate-volume-proof: PROOF-OK: wrote as 65534:1500, cp --reflink=always seed to runs succeeded, cmp equal, marker written\n'
    [ "$scenario" = nomarker ] && return 0
    printf 'gate-volume-proof: marker /gate-runs/.gate-volume reads: proven 2026-11-07T03:00:00Z node=%s device=%s fstype=xfs source=/dev/nvme2n1 pod=gate-volume-proof-x7k2p\n' "$node" "$dev_runs"
}
log_refused() {
    printf 'gate-volume-proof: running as uid 65534 gid 1500 on node w-1, pod gate-volume-proof-x7k2p\n'
    printf 'gate-volume-proof: devices gate-seed=66308 gate-runs=66308 etc-hosts(EPHEMERAL)=66308\n'
    printf 'gate-volume-proof: REFUSED — /gate-runs is on the same filesystem as /etc/hosts (device 66308)\n'
    printf 'gate-volume-proof: no marker was written\n'
}
node_json() {
    n="$1"
    [ -f "$S/ip-$n" ] || { echo "Error from server (NotFound): nodes \"$n\" not found" >&2; exit 1; }
    labels='"kubernetes.io/hostname":"'"$n"'"'
    [ -f "$S/cp-$n" ] && labels="$labels"',"node-role.kubernetes.io/control-plane":""'
    spec='{}'
    [ -f "$S/unsched-$n" ] && spec='{"unschedulable":true}'
    printf '{"kind":"Node","metadata":{"name":"%s","labels":{%s}},"spec":%s,"status":{"addresses":[{"type":"InternalIP","address":"%s"}]}}\n' "$n" "$labels" "$spec" "$(cat "$S/ip-$n")"
}
finished() { [ "$polls" -gt 1 ]; }
case "$1" in
get)
    case "$2" in
    node) node_json "$3"; exit 0 ;;
    pvc)
        [ "$3" = gate ] || { echo "stub: get pvc $3" >&2; exit 2; }
        [ -f "$S/pvc.json" ] && cat "$S/pvc.json"; exit 0 ;;
    pv)
        [ -f "$S/pv.json" ] && [ "$(jq -r .metadata.name "$S/pv.json")" = "$3" ] && cat "$S/pv.json"; exit 0 ;;
    jobs)
        case "$*" in *"-l app=gate-runner"*) ;; *) echo "stub: jobs by another selector: $*" >&2; exit 2 ;; esac
        printf '{"kind":"List","items":%s}\n' "$(cat "$S/gatejobs.json" 2>/dev/null || echo '[]')"; exit 0 ;;
    job)
        [ "$3" = gate-volume-proof ] || { echo "stub: get job $3" >&2; exit 2; }
        [ -f "$S/job.json" ] || exit 0
        if [ -f "$S/made" ]; then
            polls=$((polls + 1)); echo "$polls" > "$S/polls"
            cond='[]'
            if finished; then
                case "$scenario" in
                refused) cond='[{"type":"Failed","status":"True","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit"}]' ;;
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
        *"-l batch.kubernetes.io/controller-uid=6f1e9b99-0000-4000-8000-0000000000aa "*)
            # The planted previous proof's one pod, finished.
            printf '{"kind":"List","items":[{"metadata":{"name":"gate-volume-proof-old11","creationTimestamp":"2026-11-07T02:40:00Z"},"spec":{"nodeName":"w-1"},"status":{"phase":"Failed","containerStatuses":[{"name":"proof","state":{"terminated":{"exitCode":1}}}]}}]}\n'
            exit 0 ;;
        *"-l batch.kubernetes.io/controller-uid=6f1e9b99-0000-4000-8000-0000000000ae "*)
            on="${STUB_RAN_ON:-w-1}"
            if [ "$scenario" = deadline ]; then items='[]'
            elif ! finished; then items='[{"metadata":{"name":"gate-volume-proof-x7k2p"},"spec":{"nodeName":"'"$on"'"},"status":{"phase":"Running"}}]'
            else
                ec=0; ph=Succeeded
                [ "$scenario" = refused ] && { ec=1; ph=Failed; }
                items='[{"metadata":{"name":"gate-volume-proof-x7k2p","creationTimestamp":"2026-11-07T03:00:00Z"},"spec":{"nodeName":"'"$on"'"},"status":{"phase":"'"$ph"'","containerStatuses":[{"name":"proof","state":{"terminated":{"exitCode":'"$ec"'}}}]}}]'
            fi
            printf '{"kind":"List","items":%s}\n' "$items"; exit 0 ;;
        *" -l "*) echo "stub: pods by another selector: $*" >&2; exit 2 ;;
        *) printf '{"kind":"List","items":%s}\n' "$(cat "$S/pods.json" 2>/dev/null || echo '[]')"; exit 0 ;;
        esac ;;
    esac ;;
create)
    case "$*" in
    "create -f - -n boss-dev -o json "*) ;;
    *) echo "stub: a create that is not the manifest on stdin into boss-dev: $*" >&2; exit 2 ;;
    esac
    cat > "$S/sent.yaml"
    if [ -f "$S/job.json" ]; then
        echo 'Error from server (AlreadyExists): error when creating "STDIN": jobs.batch "gate-volume-proof" already exists' >&2
        exit 1
    fi
    printf '{"kind":"Job","metadata":{"name":"gate-volume-proof","namespace":"boss-dev","uid":"6f1e9b99-0000-4000-8000-0000000000ae"},"status":{}}\n' > "$S/job.json"
    touch "$S/made"
    cat "$S/job.json"; exit 0 ;;
logs)
    if [ "$2" = gate-volume-proof-old11 ]; then
        printf 'gate-volume-proof: REFUSED — an earlier attempt, before the volume was provisioned\n'
        exit 0
    fi
    [ "$2" = gate-volume-proof-x7k2p ] || { echo "stub: logs of $2" >&2; exit 2; }
    case "$scenario" in
    refused) log_refused ;;
    *) log_ok ;;
    esac
    exit 0 ;;
delete)
    # Only the one Job, by its API path, with the uid precondition read
    # from the body on stdin. STUB_DELETE: ok | refuse | noop.
    case "$*" in
    "delete --raw /apis/batch/v1/namespaces/boss-dev/jobs/gate-volume-proof -f - "*) ;;
    *) echo "stub: a delete that is not the proof Job by its path: $*" >&2; exit 2 ;;
    esac
    cat > "$S/delete-body.json"
    want=$(jq -r .metadata.uid "$S/job.json")
    have=$(jq -r .preconditions.uid "$S/delete-body.json")
    if [ "${STUB_DELETE:-ok}" = refuse ] || [ "$have" != "$want" ]; then
        echo "Error from server (Conflict): Operation cannot be fulfilled on jobs.batch \"gate-volume-proof\": Precondition failed: UID in precondition: $have, UID in object meta: $want" >&2
        exit 1
    fi
    [ "${STUB_DELETE:-ok}" = noop ] || rm -f "$S/job.json"
    echo '{"kind":"Status","status":"Success"}'
    exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

struct Cluster {
    dir: PathBuf,
}

fn needs_tools() {
    for tool in ["jq", "curl"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "prove_gate_volume_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

impl Cluster {
    /// The estate of 2026-11-07: two control planes and two workers; the
    /// claim `gate` Bound to the local PV gate-w-1 on w-1; nothing
    /// running in boss-dev but the dev pod.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        // The script's wait sleeps between polls; here it returns at once.
        write_exec(&dir.join("bin/sleep"), "#!/bin/sh\nexit 0\n");
        let c = Cluster { dir };
        write_file(
            &c.dir.join("nodes.json"),
            &json!({"data": [
                node("forge", "10.20.0.15", "forge"),
                node("cp-1", "10.20.0.11", "talos-control-plane"),
                node("w-1", "10.20.0.14", "talos-worker"),
                node("w-2", "10.20.0.16", "talos-worker"),
            ]})
            .to_string(),
        );
        for (n, ip) in [
            ("cp-1", "10.20.0.11"),
            ("w-1", "10.20.0.14"),
            ("w-2", "10.20.0.16"),
        ] {
            c.state(&format!("ip-{n}"), ip);
        }
        c.state("cp-cp-1", "");
        c.state(
            "pvc.json",
            &json!({"kind": "PersistentVolumeClaim",
                "metadata": {"name": "gate", "namespace": "boss-dev"},
                "spec": {"volumeName": "gate-w-1", "storageClassName": "gate-local"},
                "status": {"phase": "Bound"}})
            .to_string(),
        );
        c.state("pv.json", &pv_on("w-1").to_string());
        c.state(
            "pods.json",
            &json!([pod("boss-dev-858b849c4c-mz6gq", "Running", "work")]).to_string(),
        );
        c
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn read(&self, file: &str) -> String {
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

    fn created(&self) -> bool {
        self.calls().iter().any(|c| c.starts_with("create "))
    }
}

fn node(id: &str, address: &str, role: &str) -> Value {
    json!({
        "id": id, "label": id, "address": address, "role": role, "roles": [],
        "cpu": 4, "memory_gb": 15, "disk_gb": 100, "notes": null, "retired": false
    })
}

fn pv_on(node: &str) -> Value {
    json!({"kind": "PersistentVolume",
        "metadata": {"name": "gate-w-1"},
        "spec": {"local": {"path": "/var/mnt/gate"},
            "claimRef": {"namespace": "boss-dev", "name": "gate"},
            "nodeAffinity": {"required": {"nodeSelectorTerms": [{"matchExpressions": [
                {"key": "kubernetes.io/hostname", "operator": "In", "values": [node]}]}]}}},
        "status": {"phase": "Bound"}})
}

/// A pod of boss-dev in `phase`, mounting the claim `claim`.
fn pod(name: &str, phase: &str, claim: &str) -> Value {
    json!({"metadata": {"name": name, "namespace": "boss-dev"},
        "spec": {"nodeName": "w-1", "volumes": [{"name": "v", "persistentVolumeClaim": {"claimName": claim}}]},
        "status": {"phase": phase}})
}

fn verb() -> Value {
    let p = repo_root().join(format!("infra/ops/verbs/{VERB}.json"));
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The lines of `out` the verb's declared `effect` matches, judged by the
/// runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    let re = verb()["effect"]
        .as_str()
        .expect("prove-gate-volume declares an `effect`")
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

#[test]
fn prove_gate_volume_applies_the_tree_s_job_and_prints_its_log_and_the_effect() {
    let c = Cluster::new("prove-ok");
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    // What the create was handed is the tree's manifest, the node filled in.
    let sent = c.read("sent.yaml");
    let want = std::fs::read_to_string(repo_root().join(MANIFEST))
        .expect("manifest")
        .replace("$GATE_PROOF_NODE", "w-1");
    assert_eq!(
        sent, want,
        "the tree's manifest, with only the node substituted"
    );
    // The whole log, every line.
    for line in [
        "gate-volume-proof: devices gate-seed=66310 gate-runs=66310 etc-hosts(EPHEMERAL)=66308",
        "gate-volume-proof: df: /dev/nvme2n1    3.5T   25G  3.5T   1% /gate-runs",
        "gate-volume-proof: PROOF-OK: wrote as 65534:1500, cp --reflink=always seed to runs succeeded, cmp equal, marker written",
    ] {
        assert!(
            out.contains(line),
            "the whole log is printed — missing {line:?}:\n{out}"
        );
    }
    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "exactly one effect line:\n{out}");
    assert!(
        hits[0].starts_with(
            "prove-gate-volume: PROVEN w-1 device 66310 marker /gate-runs/.gate-volume"
        ),
        "{hits:?}"
    );
    // One create, and no delete: a finished proof removes itself.
    assert_eq!(
        c.calls()
            .iter()
            .filter(|l| l.starts_with("create "))
            .count(),
        1
    );
    assert!(
        !c.calls().iter().any(|l| l.starts_with("delete ")),
        "{:?}",
        c.calls()
    );
}

#[test]
fn prove_gate_volume_refuses_a_claim_that_is_not_bound() {
    let c = Cluster::new("prove-unbound");
    c.state(
        "pvc.json",
        &json!({"kind": "PersistentVolumeClaim",
            "metadata": {"name": "gate", "namespace": "boss-dev"},
            "spec": {"storageClassName": "gate-local"},
            "status": {"phase": "Pending"}})
        .to_string(),
    );
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("REFUSED"), "{out}");
    assert!(out.contains("Pending"), "it names the phase: {out}");
    assert!(!c.created(), "nothing was created: {:?}", c.calls());
    assert!(effect_lines(&out).is_empty(), "{out}");

    let c = Cluster::new("prove-no-claim");
    std::fs::remove_file(c.dir.join("state/pvc.json")).expect("rm pvc");
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(!c.created(), "{:?}", c.calls());
}

#[test]
fn prove_gate_volume_refuses_a_claim_bound_to_another_node() {
    let c = Cluster::new("prove-other-node");
    c.state("pv.json", &pv_on("w-2").to_string());
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("w-2"), "it names where the volume is: {out}");
    assert!(!c.created(), "{:?}", c.calls());
}

#[test]
fn prove_gate_volume_refuses_while_a_gate_runs() {
    // A running pod mounting the claim: a gate, or its seed refresh.
    let c = Cluster::new("prove-gate-running");
    c.state(
        "pods.json",
        &json!([
            pod("boss-dev-858b849c4c-mz6gq", "Running", "work"),
            pod("gate-fix-a-thing-q7x2c-abcde", "Running", "gate"),
            pod("gate-earlier-z9z9z-fghij", "Succeeded", "gate"),
        ])
        .to_string(),
    );
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("gate-fix-a-thing-q7x2c-abcde"),
        "it names the gate: {out}"
    );
    assert!(
        !out.contains("gate-earlier-z9z9z-fghij"),
        "a finished pod is not in flight: {out}"
    );
    assert!(!c.created(), "{:?}", c.calls());

    // A gate Job admitted but not yet given a pod is in flight too.
    let c = Cluster::new("prove-gate-job-unfinished");
    c.state(
        "gatejobs.json",
        &json!([{"metadata": {"name": "gate-fix-a-thing-q7x2c", "labels": {"app": "gate-runner"}},
            "status": {"active": 0}}])
        .to_string(),
    );
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("gate-fix-a-thing-q7x2c"), "{out}");
    assert!(!c.created(), "{:?}", c.calls());
}

/// A previous proof Job standing in boss-dev, uid ...aa, with these
/// conditions.
fn plant_previous_proof(c: &Cluster, conditions: Value) {
    c.state(
        "job.json",
        &json!({"kind": "Job", "metadata": {"name": "gate-volume-proof", "namespace": "boss-dev",
            "uid": "6f1e9b99-0000-4000-8000-0000000000aa", "creationTimestamp": "2026-11-07T02:40:00Z"},
            "status": {"conditions": conditions}})
        .to_string(),
    );
}

#[test]
fn prove_gate_volume_never_touches_a_proof_still_running() {
    let c = Cluster::new("prove-job-running");
    plant_previous_proof(&c, json!([]));
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("6f1e9b99-0000-4000-8000-0000000000aa"),
        "it names the Job: {out}"
    );
    assert!(out.contains("NOT finished"), "{out}");
    assert!(!c.created(), "{:?}", c.calls());
    assert!(!c.calls().iter().any(|l| l.starts_with("delete ")));
}

#[test]
fn a_finished_previous_proof_is_captured_then_removed_by_uid_before_the_new_one() {
    let c = Cluster::new("prove-job-finished");
    plant_previous_proof(
        &c,
        json!([{"type": "Failed", "status": "True", "reason": "BackoffLimitExceeded"}]),
    );
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    // Its whole log is on this run's record before the delete.
    let captured = out
        .find("gate-volume-proof: REFUSED — an earlier attempt")
        .unwrap_or_else(|| panic!("the previous proof's log is printed: {out}"));
    let removed = out
        .find("removed the previous proof Job")
        .unwrap_or_else(|| panic!("the removal is read back and said: {out}"));
    assert!(captured < removed, "capture before delete: {out}");
    let calls = c.calls();
    let del = calls.iter().position(|l| l.starts_with("delete --raw "));
    let create = calls.iter().position(|l| l.starts_with("create "));
    assert!(
        matches!((del, create), (Some(d), Some(n)) if d < n),
        "one delete, then the create: {calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|l| l.starts_with("delete ")).count(),
        1,
        "{calls:?}"
    );
    let body: Value =
        serde_json::from_str(&c.read("delete-body.json")).expect("the delete carried a body");
    assert_eq!(
        body["preconditions"]["uid"],
        "6f1e9b99-0000-4000-8000-0000000000aa"
    );
    assert_eq!(effect_lines(&out).len(), 1, "{out}");
}

#[test]
fn a_previous_proof_that_will_not_go_stops_the_run_before_any_create() {
    for (mode, why) in [
        ("refuse", "Precondition failed"),
        ("noop", "not read back gone"),
    ] {
        let c = Cluster::new("prove-job-stays");
        plant_previous_proof(&c, json!([{"type": "Complete", "status": "True"}]));
        let o = c.run(&["w-1"], &[("STUB_DELETE", mode)]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{mode}: {out}");
        assert!(out.contains(why), "{mode}: {out}");
        assert!(!c.created(), "{mode}: {:?}", c.calls());
        assert!(effect_lines(&out).is_empty(), "{mode}: {out}");
    }
}

#[test]
fn prove_gate_volume_bounds_the_node_like_every_node_verb() {
    // A control plane is refused by the registry before any door opens.
    let c = Cluster::new("prove-cp");
    let o = c.run(&["cp-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(c.calls().is_empty(), "no door was opened: {:?}", c.calls());
    // A cordoned worker cannot schedule the proof's pod.
    let c = Cluster::new("prove-cordoned");
    c.state("unsched-w-1", "");
    let o = c.run(&["w-1"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(out.contains("cordoned"), "{out}");
    assert!(!c.created(), "{:?}", c.calls());
    // A malformed node, or more than one argument, before anything is read.
    for args in [&["W-1"][..], &["w-1", "extra"][..], &[][..]] {
        let c = Cluster::new("prove-malformed");
        let o = c.run(args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
        assert!(c.calls().is_empty(), "{args:?}: {:?}", c.calls());
    }
}

#[test]
fn a_proof_that_refused_is_failed_with_its_whole_log() {
    let c = Cluster::new("prove-refused");
    let o = c.run(&["w-1"], &[("STUB_SCENARIO", "refused")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("prove-gate-volume: FAILED"), "{out}");
    assert!(
        out.contains(
            "gate-volume-proof: REFUSED — /gate-runs is on the same filesystem as /etc/hosts"
        ),
        "the Job's own words are printed: {out}"
    );
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_proof_that_never_ran_is_failed_with_the_cluster_s_reason() {
    let c = Cluster::new("prove-deadline");
    let o = c.run(&["w-1"], &[("STUB_SCENARIO", "deadline")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("DeadlineExceeded"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}

#[test]
fn a_completed_proof_whose_log_does_not_vouch_is_not_an_effect() {
    // The Job says Complete, and its log is not the proof the verb asked
    // for: another node's marker, the runs directory on EPHEMERAL's device,
    // no marker line at all, or a pod that ran somewhere else.
    for (scenario, ran_on) in [
        ("othernode", "w-1"),
        ("ephemeral", "w-1"),
        ("nomarker", "w-1"),
        ("ok", "w-2"),
    ] {
        let c = Cluster::new("prove-not-vouched");
        let o = c.run(
            &["w-1"],
            &[("STUB_SCENARIO", scenario), ("STUB_RAN_ON", ran_on)],
        );
        let out = text(&o);
        assert_eq!(o.status.code(), Some(1), "{scenario}/{ran_on}: {out}");
        assert!(
            out.contains("prove-gate-volume: FAILED"),
            "{scenario}/{ran_on}: {out}"
        );
        assert!(effect_lines(&out).is_empty(), "{scenario}/{ran_on}: {out}");
    }
}

#[test]
fn the_verb_file_bounds_what_a_packet_can_ask_and_says_why_it_takes_no_passkey() {
    let v = verb();
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(v["argv"], json!([SCRIPT, "{1}"]), "the node, nothing else");
    let ps = v["params"].as_array().cloned().unwrap_or_default();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0]["name"], "node");
    assert!(ps[0].get("default").is_none(), "a node verb names its node");
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("MUTATING"), "{about}");
    assert!(about.contains("David"), "it names who authorised it");
    assert!(
        about.contains("NO PASSKEY"),
        "the passkey decision is in its about"
    );
    assert!(v["requires_approval"].is_null(), "decided: no passkey");
    assert!(v["plan_verb"].is_null());
    assert!(v["effect"].is_string());
    assert!(v["effect_unread"].is_null());
    let timeout = v["timeout"].as_u64().expect("a declared timeout");
    let deadline = the_job()["spec"]["activeDeadlineSeconds"]
        .as_u64()
        .expect("deadline");
    assert!(
        timeout > deadline + 120,
        "the runner's timeout ({timeout}s) outlasts the Job's deadline ({deadline}s), the wait's margin and the door's first pull"
    );
}
