//! The two ops verbs that put one replica of a Longhorn volume on a NAMED
//! disk of the node that already holds it (backlog b9789705): the
//! read-only `plan-a-volume-replica-disk-move` (`infra/forge/
//! move-volume-replica.sh --plan-to-disk`) and the passkey-approved
//! `move-volume-replica-to-disk` (`--to-disk`).
//!
//! WHY. On 2026-11-16 the system of record's volume, boss/pgdata-postgres-0
//! (pvc-93e11a6e-…), had three healthy replicas — cp-2, cp-3 and w-3 — and
//! the w-3 one (r-b407252b) sat on w-3's DEFAULT disk (diskID 38052ee5-…,
//! /var/lib/longhorn), not on u-postgres (/var/mnt/postgres, tagged
//! durable,postgres) where design 3c475a19 puts the database
//! (read-volume-replica-history 1f0c9fad). `move-volume-replica` only ever
//! puts the new replica on a node holding none, so no door could move it
//! one disk over. Longhorn places by tag, not by disk name, so the move
//! holds the volume's diskSelector at the disk's tags and its node
//! anti-affinity soft for the move only, and puts both back.
//!
//! THE ESTATE in these tests is that one: the three replicas, w-3 with its
//! default disk and its two Microns (u-postgres tagged durable,postgres,
//! u-data durable), and w-1 with an untagged disk and room.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`. The stub applies a JSON patch as the API server
//! does (every op in order, all or nothing, a failed `test` refusing the
//! lot), and places the replica a raised count asks for as Longhorn would
//! here: on w-3 (STUB_NEW_NODE) only while the volume's
//! replicaSoftAntiAffinity reads `enabled` — with it hard, Longhorn puts
//! no second replica on a node holding one — on the first disk of that
//! node whose tags hold the volume's diskSelector and that holds no
//! replica of the volume (or STUB_NEW_DISK). Every door call lands in
//! `argv`. Nothing here reaches a cluster.

use boss_testing::signals::{DEFAULT_SIGNALS_ARGV, host_ignoring};
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/move-volume-replica.sh";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
/// The w-3 replica the triage read, on w-3's default disk.
const REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-b407252b";
const GIB: u64 = 1024 * 1024 * 1024;
const SIZE: u64 = 30 * GIB;
const W3_DEFAULT: &str = "38052ee5-2be4-4901-be23-5fa7be230e5a";
const W3_POSTGRES: &str = "0b0b0b0b-1111-4222-8333-444455556666";
const W3_DATA: &str = "0c0c0c0c-1111-4222-8333-444455556666";
const W1_DISK: &str = "5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10";
const CP2_DISK: &str = "976a5246-7a68-452c-bb25-f9e3ecb2258e";
const CP3_DISK: &str = "9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1";
/// The new replica the stub builds.
const NEW: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-e0e0e0e1";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `vol-<name>.json`, `setting-<name>.json`, and the `replicas.json` and
/// `lhnodes.json` lists (arrays). See the module doc for placement.
/// STUB_REBUILD=never builds the new replica unhealthy; STUB_PATCH_FAIL=1
/// refuses every patch, STUB_PATCH_FAIL_AT=<n> only the n-th.
/// STUB_UNWELL=<replica> fails that replica when the count is raised.
/// STUB_VOL_GET_FAIL_AT=<n> refuses the n-th read of the volume.
/// STUB_TERM_AFTER_RAISE=1 sends the script SIGTERM, once, at the first
/// read of the volume after a raise (the script is the stub's parent:
/// the door is called directly, never in a subshell).
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
case "$1 $2" in
"get volumes.longhorn.io")
    g=$(( $(cat "$S/volgets.n" 2>/dev/null || echo 0) + 1 )); echo "$g" > "$S/volgets.n"
    [ "${STUB_VOL_GET_FAIL_AT:-0}" != "$g" ] \
        || { echo "Unable to connect to the server: dial tcp 10.20.0.1:6443: i/o timeout" >&2; exit 1; }
    if [ -n "${STUB_TERM_AFTER_RAISE:-}" ] && [ -f "$S/raised" ] && [ ! -f "$S/termed" ]; then
        : > "$S/termed"; kill -TERM "$PPID"
    fi
    f="$S/vol-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): volumes.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get settings.longhorn.io")
    f="$S/setting-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): settings.longhorn.io \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
"get replicas.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"; exit 0 ;;
"get nodes.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/lhnodes.json")"; exit 0 ;;
"patch volumes.longhorn.io")
    v="$3"; f="$S/vol-$v.json"; p=""
    while [ $# -gt 0 ]; do [ "$1" = -p ] && p="$2"; shift; done
    k=$(( $(cat "$S/patches.n" 2>/dev/null || echo 0) + 1 )); echo "$k" > "$S/patches.n"
    [ -z "${STUB_PATCH_FAIL:-}" ] && [ "${STUB_PATCH_FAIL_AT:-0}" != "$k" ] \
        || { echo "Error from server: admission webhook \"validator.longhorn.io\" denied the request" >&2; exit 1; }
    cur=$(jq -c '.spec.numberOfReplicas' "$f")
    # RFC 6902 as the API server applies it: in order, all or nothing.
    jq --argjson p "$p" 'reduce $p[] as $o (.;
        if . == null then null else
        ($o.path | ltrimstr("/") | split("/")) as $k
        | ($k[:-1]) as $parent | ($k[-1]) as $leaf
        | (getpath($parent) | type == "object" and has($leaf)) as $there
        | if $o.op == "test" then (if $there and getpath($k) == $o.value then . else null end)
          elif $o.op == "replace" then (if $there then setpath($k; $o.value) else null end)
          elif $o.op == "add" then setpath($k; $o.value)
          elif $o.op == "remove" then (if $there then delpaths([$k]) else null end)
          else null end end)' "$f" > "$f.new"
    if [ "$(cat "$f.new")" = null ]; then
        rm -f "$f.new"
        echo "The request is invalid: the test operation failed" >&2; exit 1
    fi
    mv "$f.new" "$f"
    new=$(jq -c '.spec.numberOfReplicas' "$f")
    if [ "$new" -gt "$cur" ]; then
        : > "$S/raised"
        if [ -n "${STUB_UNWELL:-}" ]; then
            jq --arg r "$STUB_UNWELL" 'map(if .metadata.name == $r then .spec.healthyAt = "" | .spec.failedAt = "2026-11-16T19:31:00Z" | .status.currentState = "stopped" else . end)' \
                "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
        fi
    fi
    if [ "$new" -gt "$cur" ] && [ "$(jq -r '.spec.replicaSoftAntiAffinity // "ignored"' "$f")" = enabled ]; then
        node="${STUB_NEW_NODE:-w-3}"
        disk="${STUB_NEW_DISK:-$(jq -r --arg n "$node" --arg v "$v" --slurpfile vol "$f" --slurpfile reps "$S/replicas.json" '
            ($vol[0].spec.diskSelector // []) as $sel
            | [$reps[0][] | select(.spec.volumeName == $v) | .spec.diskID] as $used
            | [.[] | select(.metadata.name == $n) | .spec.disks as $sd | .status.diskStatus | to_entries[]
               | select(($sel - ($sd[.key].tags // [])) == [] and (.value.diskUUID as $u | $used | index($u) | not))
               | .value.diskUUID] | .[0] // ""' "$S/lhnodes.json")}"
        if [ -n "$disk" ]; then
            h="2026-11-16T19:30:00Z"
            [ "${STUB_REBUILD:-}" = never ] && h=""
            jq --arg v "$v" --arg r "$v-r-e0e0e0e1" --arg n "$node" --arg d "$disk" --arg h "$h" \
                '. + [{"apiVersion":"longhorn.io/v1beta2","kind":"Replica","metadata":{"name":$r,"namespace":"longhorn-system"},"spec":{"volumeName":$v,"nodeID":$n,"diskID":$d,"healthyAt":$h,"failedAt":"","active":true},"status":{"currentState":"running"}}]' \
                "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
        fi
    fi
    echo "volume.longhorn.io/$v patched"; exit 0 ;;
"delete replicas.longhorn.io")
    r="$3"
    [ -n "$(jq -c --arg r "$r" '.[] | select(.metadata.name == $r)' "$S/replicas.json")" ] \
        || { echo "Error from server (NotFound): replicas.longhorn.io \"$r\" not found" >&2; exit 1; }
    jq --arg r "$r" 'map(select(.metadata.name != $r))' "$S/replicas.json" > "$S/replicas.new" && mv "$S/replicas.new" "$S/replicas.json"
    echo "replica.longhorn.io \"$r\" deleted"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

fn needs_tools() {
    for tool in ["jq", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "move_volume_replica_to_disk_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

/// The volume as Longhorn's mutator leaves it: every key present.
fn volume() -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
        "metadata": {"name": VOL, "namespace": "longhorn-system"},
        "spec": {"numberOfReplicas": 3, "size": SIZE.to_string(),
                 "dataLocality": "disabled", "replicaSoftAntiAffinity": "ignored",
                 "replicaZoneSoftAntiAffinity": "ignored",
                 "replicaAutoBalance": "ignored", "dataEngine": "v1", "nodeID": "w-3",
                 "diskSelector": [], "nodeSelector": []},
        "status": {"state": "attached", "robustness": "healthy", "currentNodeID": "w-3",
                   "actualSize": 20 * GIB,
                   "kubernetesStatus": {"namespace": "boss", "pvcName": "pgdata-postgres-0",
                                        "pvName": VOL}}
    })
}

fn replica(name: &str, node: &str, disk: &str) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"volumeName": VOL, "nodeID": node, "diskID": disk,
                 "healthyAt": "2026-11-11T20:28:56Z", "failedAt": "", "active": true},
        "status": {"currentState": "running"}
    })
}

/// One Longhorn disk: name, UUID, path, tags, and a ledger with room.
struct Disk {
    name: &'static str,
    uuid: &'static str,
    path: &'static str,
    tags: &'static [&'static str],
    max: u64,
}

fn lh_node(name: &str, disks: &[Disk]) -> Value {
    let mut spec = serde_json::Map::new();
    let mut status = serde_json::Map::new();
    for d in disks {
        spec.insert(
            d.name.to_string(),
            json!({"allowScheduling": true, "evictionRequested": false,
                   "path": d.path, "diskType": "filesystem",
                   "storageReserved": d.max / 10, "tags": d.tags}),
        );
        status.insert(
            d.name.to_string(),
            json!({"diskUUID": d.uuid, "storageMaximum": d.max,
                   "storageScheduled": 100 * GIB, "storageAvailable": d.max / 2,
                   "conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}]}),
        );
    }
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Node",
        "metadata": {"name": name, "namespace": "longhorn-system"},
        "spec": {"allowScheduling": true, "evictionRequested": false, "tags": [],
                 "disks": spec},
        "status": {"conditions": [{"type": "Ready", "status": "True"},
                                  {"type": "Schedulable", "status": "True"}],
                   "diskStatus": status}
    })
}

fn disk(
    name: &'static str,
    uuid: &'static str,
    path: &'static str,
    tags: &'static [&'static str],
) -> Disk {
    Disk {
        name,
        uuid,
        path,
        tags,
        max: 900 * GIB,
    }
}

/// The estate's nodes, with w-1's disk tagged as given.
fn nodes(w1_tags: &'static [&'static str]) -> Value {
    json!([
        lh_node(
            "cp-2",
            &[disk("default-disk-cp2", CP2_DISK, "/var/lib/longhorn", &[])]
        ),
        lh_node(
            "cp-3",
            &[disk("default-disk-cp3", CP3_DISK, "/var/lib/longhorn", &[])]
        ),
        lh_node(
            "w-1",
            &[disk(
                "default-disk-1f1f",
                W1_DISK,
                "/var/lib/longhorn",
                w1_tags
            )]
        ),
        lh_node(
            "w-3",
            &[
                disk(
                    "default-disk-38052ee5",
                    W3_DEFAULT,
                    "/var/lib/longhorn",
                    &[]
                ),
                disk(
                    "u-postgres",
                    W3_POSTGRES,
                    "/var/mnt/postgres",
                    &["durable", "postgres"]
                ),
                disk("u-data", W3_DATA, "/var/mnt/data", &["durable"]),
            ]
        ),
    ])
}

struct Longhorn {
    dir: PathBuf,
}

impl Longhorn {
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        let l = Longhorn { dir };
        l.volume(volume());
        l.state(
            "replicas.json",
            &json!([
                replica(
                    "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-a2e85dc4",
                    "cp-2",
                    CP2_DISK
                ),
                replica(
                    "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-2bd9cc5a",
                    "cp-3",
                    CP3_DISK
                ),
                replica(REP, "w-3", W3_DEFAULT),
            ])
            .to_string(),
        );
        l.state("lhnodes.json", &nodes(&[]).to_string());
        l.setting("storage-over-provisioning-percentage", "100");
        l.setting("storage-minimal-available-percentage", "25");
        l.setting("replica-soft-anti-affinity", "false");
        l.setting("replica-auto-balance", "disabled");
        l.setting("replica-zone-soft-anti-affinity", "true");
        l.setting("allow-empty-disk-selector-volume", "true");
        l
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn volume(&self, v: Value) {
        self.state(&format!("vol-{VOL}.json"), &v.to_string());
    }

    fn setting(&self, name: &str, value: &str) {
        self.state(
            &format!("setting-{name}.json"),
            &json!({"apiVersion": "longhorn.io/v1beta2", "kind": "Setting",
                    "metadata": {"name": name, "namespace": "longhorn-system"},
                    "value": value})
            .to_string(),
        );
    }

    fn volume_now(&self) -> Value {
        let p = self.dir.join("state").join(format!("vol-{VOL}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("json")
    }

    /// This volume's replicas now, `<name> on <node> (<disk>)`, sorted.
    fn replicas_now(&self) -> Vec<String> {
        let p = self.dir.join("state").join("replicas.json");
        let all: Value =
            serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("json");
        let mut v: Vec<String> = all
            .as_array()
            .expect("an array")
            .iter()
            .map(|r| {
                format!(
                    "{} on {} ({})",
                    r["metadata"]["name"].as_str().unwrap_or_default(),
                    r["spec"]["nodeID"].as_str().unwrap_or_default(),
                    r["spec"]["diskID"].as_str().unwrap_or_default()
                )
            })
            .collect();
        v.sort();
        v
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run_from(&[], args, env)
    }

    /// `run`, started by `parent` — an argv that sets up the signal
    /// state of the host the script is run from and execs the rest.
    /// Whatever that state, the script itself starts through
    /// `boss_testing::signals`, so its TERM trap is real (backlog
    /// 5f6a2cde).
    fn run_from(&self, parent: &[&str], args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut argv: Vec<&OsStr> = parent.iter().map(OsStr::new).collect();
        argv.extend(DEFAULT_SIGNALS_ARGV.map(OsStr::new));
        argv.push(OsStr::new("bash"));
        let mut c = Command::new(argv[0]);
        c.args(&argv[1..])
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_MOVE_REBUILD_S", "3")
            .env("BOSS_MOVE_SETTLE_S", "1")
            .env("BOSS_MOVE_READBACK_S", "3")
            .env("BOSS_MOVE_POLL_S", "1")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// The mutating door calls, in order, with the door's prefix cut off.
    fn writes(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                l.split_once("--kubeconfig=/kc ")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| l.to_string())
            })
            .filter(|c| c.starts_with("patch ") || c.starts_with("delete "))
            .collect()
    }

    fn plan_of(&self, disk: &str) -> (String, String) {
        let o = self.run(&["--plan-to-disk", VOL, REP, disk], &[]);
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

    fn plan(&self) -> (String, String) {
        self.plan_of("u-postgres")
    }

    /// A plan that must refuse: exit 78, no plan, no write.
    fn refused(&self, disk: &str) -> String {
        let o = self.run(&["--plan-to-disk", VOL, REP, disk], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{disk} is refused: {out}");
        assert!(
            !out.lines().any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        assert!(self.writes().is_empty(), "{:?}", self.writes());
        out
    }
}

/// The JSON patch a `patch` door call sent: the text after ` -p `.
fn patch_body(call: &str) -> &str {
    call.split_once(" -p ")
        .map(|(_, p)| p.trim())
        .unwrap_or_else(|| panic!("a patch call carries -p: {call}"))
}

/// What every exit between an accepted raise and an accepted lower owes
/// (review cf45a112, finding 1): the LAST write is the put-back — a
/// compare-and-set on the move's two fields that puts back what the plan
/// read and leaves the count alone — the volume reads both fields as
/// before with the count still 4 (more copies, never fewer), the replica
/// is not deleted, and the failure line says what the volume reads.
fn assert_put_back(l: &Longhorn, plan: &str, out: &str, writes: usize) {
    let w = l.writes();
    assert_eq!(w.len(), writes, "{w:?}");
    let last = w.last().expect("a write");
    assert!(
        last.starts_with(&format!("patch volumes.longhorn.io {VOL} ")),
        "{w:?}"
    );
    let p = patch_body(last);
    assert_eq!(
        p,
        r#"[{"op":"test","path":"/spec/diskSelector","value":["durable","postgres"]},{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"enabled"},{"op":"replace","path":"/spec/diskSelector","value":[]},{"op":"replace","path":"/spec/replicaSoftAntiAffinity","value":"ignored"}]"#,
        "the put-back tests the move's fields, puts back the plan's, and leaves the count alone"
    );
    assert!(
        plan.contains(p),
        "the signed plan carries the put-back verbatim:\n{plan}"
    );
    assert!(w.iter().all(|c| !c.starts_with("delete ")), "{w:?}");
    let v = l.volume_now();
    assert_eq!(v["spec"]["numberOfReplicas"], 4, "{v}");
    assert_eq!(v["spec"]["diskSelector"], json!([]), "{v}");
    assert_eq!(v["spec"]["replicaSoftAntiAffinity"], "ignored", "{v}");
    assert!(l.replicas_now().iter().any(|r| r.starts_with(REP)));
    let failed: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("move-volume-replica-to-disk: FAILED"))
        .collect();
    assert_eq!(failed.len(), 1, "one failure line: {out}");
    assert!(
        failed[0].contains("PUT BACK as the plan read them")
            && failed[0].contains(
                "reads back spec.numberOfReplicas 4, spec.diskSelector [], \
                 spec.replicaSoftAntiAffinity ignored"
            )
            && failed[0].contains("NOT deleted")
            && !failed[0].contains("still reads the move's"),
        "{out}"
    );
    assert!(effect_lines(out).is_empty(), "{out}");
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

/// The lines of `out` the write's declared `effect` matches, judged by
/// the runner's own engine (jq's `test`).
fn effect_lines(out: &str) -> Vec<String> {
    effect_lines_of("move-volume-replica-to-disk", out)
}

fn effect_lines_of(name: &str, out: &str) -> Vec<String> {
    let re = verb(name)["effect"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} declares an `effect`"))
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

// ------------------------------------------------------------------- plan

#[test]
fn the_plan_names_the_disk_the_held_selector_and_what_goes_back() {
    let l = Longhorn::new("mvrd-plan");
    let (plan, hash) = l.plan();
    for want in [
        "plan: move-volume-replica-to-disk".to_string(),
        format!("volume: {VOL}"),
        "claim: boss/pgdata-postgres-0".to_string(),
        format!(
            "move: replica {REP} on w-3 (disk {W3_DEFAULT}) to u-postgres on the SAME node, w-3: \
             diskUUID {W3_POSTGRES}, path /var/mnt/postgres, tags [\"durable\",\"postgres\"]"
        ),
        "for the move: spec.diskSelector [] -> [\"durable\",\"postgres\"]".to_string(),
        "spec.replicaSoftAntiAffinity ignored -> enabled".to_string(),
        "both go back to [] and ignored when the count is lowered".to_string(),
        "after: spec.diskSelector [] admits u-postgres, so the new replica is not stranded"
            .to_string(),
        "allow-empty-disk-selector-volume true".to_string(),
        "numberOfReplicas: 3 -> 4 -> 3".to_string(),
        format!(
            "target: u-postgres (diskUUID {W3_POSTGRES}) on w-3 is the one disk Longhorn could place"
        ),
        "zone anti-affinity soft (the volume says ignored, the setting \
         replica-zone-soft-anti-affinity says true)"
            .to_string(),
        "workload: this run makes no call on a pod, an engine or an attachment".to_string(),
        format!("kubectl delete replicas.longhorn.io {REP} -n longhorn-system"),
        format!("disk {W3_POSTGRES} (u-postgres on w-3): TARGET"),
        format!("disk {W3_DATA} (u-data on w-3): not a target"),
    ] {
        assert!(plan.contains(&want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(l.writes().is_empty(), "the plan writes nothing");
    assert!(effect_lines(&plan).is_empty(), "{plan}");
    // Deterministic.
    assert_eq!(l.plan(), (plan, hash));
}

#[test]
fn the_plan_refuses_a_disk_it_cannot_single_out_and_writes_nothing() {
    // Not a disk of the replica's node (w-1's), and no such disk.
    let l = Longhorn::new("mvrd-other-node");
    let out = l.refused("default-disk-1f1f");
    assert!(
        out.contains("this moves a replica only to a disk of the node that holds it")
            && out.contains("u-postgres"),
        "names the node's disks: {out}"
    );
    l.refused("u-nonesuch");

    // The disk the replica already sits on.
    let l = Longhorn::new("mvrd-same-disk");
    let out = l.refused("default-disk-38052ee5");
    assert!(out.contains("already holds"), "{out}");

    // An untagged disk: Longhorn places by tag, never by name.
    let l = Longhorn::new("mvrd-untagged");
    let mut n = nodes(&[]);
    n[3]["spec"]["disks"]["u-postgres"]["tags"] = json!([]);
    l.state("lhnodes.json", &n.to_string());
    let out = l.refused("u-postgres");
    assert!(out.contains("carries no tags"), "{out}");

    // Another disk Longhorn could place on carries the same tags (w-1's
    // 980 PRO, tagged durable,postgres by design 3c475a19).
    let l = Longhorn::new("mvrd-two-postgres");
    l.state("lhnodes.json", &nodes(&["durable", "postgres"]).to_string());
    let out = l.refused("u-postgres");
    assert!(
        out.contains("admits other disks") && out.contains(W1_DISK),
        "names the other disk: {out}"
    );

    // Zone anti-affinity hard: a second replica in one zone is not placed.
    let l = Longhorn::new("mvrd-zone");
    l.setting("replica-zone-soft-anti-affinity", "false");
    let out = l.refused("u-postgres");
    assert!(out.contains("zone anti-affinity is hard"), "{out}");

    // The selector put back would strand the new replica: an empty one,
    // with Longhorn closing tagged disks to selector-less volumes.
    let l = Longhorn::new("mvrd-strand");
    l.setting("allow-empty-disk-selector-volume", "false");
    let out = l.refused("u-postgres");
    assert!(out.contains("would be stranded"), "{out}");

    // A volume that may already hold two replicas on one node.
    let l = Longhorn::new("mvrd-soft");
    let mut v = volume();
    v["spec"]["replicaSoftAntiAffinity"] = json!("enabled");
    l.volume(v);
    l.refused("u-postgres");

    // Not healthy.
    let l = Longhorn::new("mvrd-degraded");
    let mut v = volume();
    v["status"]["robustness"] = json!("degraded");
    l.volume(v);
    l.refused("u-postgres");
}

#[test]
fn the_argv_bound_is_the_scripts_and_opens_no_door() {
    let l = Longhorn::new("mvrd-argv");
    for args in [
        vec!["--plan-to-disk", VOL, REP],
        vec!["--plan-to-disk", VOL, REP, "u-postgres", "extra"],
        vec!["--plan-to-disk", VOL, REP, "/var/mnt/postgres"],
        vec!["--plan-to-disk", VOL, REP, "U-postgres"],
        vec!["--to-disk", VOL, REP, "u-postgres"],
        vec!["--to-disk", VOL, REP, "u-postgres", "not-a-hash"],
    ] {
        let o = l.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(
        !l.dir.join("argv").exists(),
        "no door was opened: {:?}",
        std::fs::read_to_string(l.dir.join("argv"))
    );
}

// ------------------------------------------------------------------ write

#[test]
fn the_write_holds_the_selector_rebuilds_on_the_disk_puts_it_back_and_reads_it() {
    let l = Longhorn::new("mvrd-write");
    let (plan, hash) = l.plan();
    let o = l.run(&["--to-disk", VOL, REP, "u-postgres", &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );

    let w = l.writes();
    assert_eq!(w.len(), 3, "two compare-and-sets and one delete: {w:?}");
    for want in [
        r#"{"op":"test","path":"/spec/numberOfReplicas","value":3}"#,
        r#"{"op":"test","path":"/spec/diskSelector","value":[]}"#,
        r#"{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"ignored"}"#,
        r#"{"op":"replace","path":"/spec/numberOfReplicas","value":4}"#,
        r#"{"op":"replace","path":"/spec/diskSelector","value":["durable","postgres"]}"#,
        r#"{"op":"replace","path":"/spec/replicaSoftAntiAffinity","value":"enabled"}"#,
    ] {
        assert!(w[0].contains(want), "the raise carries {want}: {w:?}");
    }
    for want in [
        r#"{"op":"test","path":"/spec/numberOfReplicas","value":4}"#,
        r#"{"op":"test","path":"/spec/diskSelector","value":["durable","postgres"]}"#,
        r#"{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"enabled"}"#,
        r#"{"op":"replace","path":"/spec/numberOfReplicas","value":3}"#,
        r#"{"op":"replace","path":"/spec/diskSelector","value":[]}"#,
        r#"{"op":"replace","path":"/spec/replicaSoftAntiAffinity","value":"ignored"}"#,
    ] {
        assert!(w[1].contains(want), "the lower carries {want}: {w:?}");
    }
    // The signature binds every op the act sends, its kind included
    // (review cf45a112, finding 4): each patch is in the plan verbatim.
    for (i, call) in w[..2].iter().enumerate() {
        assert!(
            plan.contains(patch_body(call)),
            "patch {i} is in the signed plan verbatim: {call}\n{plan}"
        );
    }
    assert!(
        w[2].starts_with(&format!(
            "delete replicas.longhorn.io {REP} -n longhorn-system"
        )),
        "{w:?}"
    );

    let v = l.volume_now();
    assert_eq!(v["spec"]["numberOfReplicas"], 3);
    assert_eq!(v["spec"]["diskSelector"], json!([]));
    assert_eq!(v["spec"]["replicaSoftAntiAffinity"], "ignored");
    let now = l.replicas_now();
    assert!(!now.iter().any(|r| r.starts_with(REP)), "{now:?}");
    assert!(
        now.contains(&format!("{NEW} on w-3 ({W3_POSTGRES})")),
        "{now:?}"
    );
    assert_eq!(now.len(), 3, "{now:?}");

    let hits = effect_lines(&out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("(cp-2, cp-3, w-3)")
            && hits[0].contains(&format!(
                "is healthy on w-3 disk u-postgres (diskUUID {W3_POSTGRES}, /var/mnt/postgres)"
            ))
            && hits[0].contains("spec.diskSelector [], spec.replicaSoftAntiAffinity ignored"),
        "{hits:?}"
    );

    // Applied once, the approval is spent: the replica is gone.
    let o = l.run(&["--to-disk", VOL, REP, "u-postgres", &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(l.writes().len(), 3, "no second move: {:?}", l.writes());
}

/// A volume that carried neither key gets them added for the move and
/// removed after, not left set to a default. It reads exactly as one
/// carrying `[]` and `ignored` (the defaults), so only the op kinds tell
/// the two apart — and the signed plan must (review cf45a112, finding 4):
/// its hash differs, it carries the add and remove ops verbatim, and its
/// step 1 claims no test of a key the patch cannot test.
#[test]
fn keys_the_volume_did_not_carry_are_added_then_removed() {
    let present = Longhorn::new("mvrd-present-keys");
    let (_, present_hash) = present.plan();
    let l = Longhorn::new("mvrd-absent-keys");
    let mut v = volume();
    let spec = v["spec"].as_object_mut().expect("spec");
    spec.remove("diskSelector");
    spec.remove("replicaSoftAntiAffinity");
    l.volume(v);
    let (plan, hash) = l.plan();
    assert_ne!(
        hash, present_hash,
        "the op kinds are in the signed bytes:\n{plan}"
    );
    let step1 = plan
        .lines()
        .find(|s| s.starts_with("step 1: "))
        .unwrap_or_else(|| panic!("a step 1: {plan}"));
    assert!(
        !step1.contains(r#""op":"test","path":"/spec/diskSelector""#)
            && !step1.contains(r#""op":"test","path":"/spec/replicaSoftAntiAffinity""#)
            && step1.contains("spec.diskSelector absent, no test: add")
            && step1.contains("spec.replicaSoftAntiAffinity absent, no test: add"),
        "{step1}"
    );
    let o = l.run(&["--to-disk", VOL, REP, "u-postgres", &hash], &[]);
    assert!(o.status.success(), "{}", text(&o));
    let w = l.writes();
    assert!(
        w[0].contains(r#"{"op":"add","path":"/spec/diskSelector","value":["durable","postgres"]}"#)
            && w[1].contains(r#"{"op":"remove","path":"/spec/diskSelector"}"#)
            && w[1].contains(r#"{"op":"remove","path":"/spec/replicaSoftAntiAffinity"}"#),
        "{w:?}"
    );
    for call in &w[..2] {
        assert!(plan.contains(patch_body(call)), "{call}\n{plan}");
    }
    let v = l.volume_now();
    assert!(v["spec"].get("diskSelector").is_none(), "{v}");
    assert!(v["spec"].get("replicaSoftAntiAffinity").is_none(), "{v}");
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan() {
    let l = Longhorn::new("mvrd-drift");
    let (_, hash) = l.plan();
    // The volume's selector changed since the signature.
    let mut v = volume();
    v["spec"]["diskSelector"] = json!(["durable"]);
    l.volume(v);
    let o = l.run(&["--to-disk", VOL, REP, "u-postgres", &hash], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(l.writes().is_empty(), "{:?}", l.writes());
}

// Every exit after the raise and before the lower (review cf45a112,
// finding 1). Left as the raise wrote them, diskSelector = u-postgres's
// tags lets Longhorn place ANY replica of the volume only on u-postgres —
// a replacement for a failed control-plane replica included — and the
// plan refuses that state, so no verb could take the pin off. Each exit
// below puts the two fields back and leaves the count at 4.

#[test]
fn a_stalled_rebuild_deletes_nothing_and_puts_the_fields_back() {
    let l = Longhorn::new("mvrd-stall");
    let (plan, hash) = l.plan();
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_REBUILD", "never")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("STALLED")
            && out.contains("STAYS 4")
            && out.contains("plan-a-volume-replica-disk-move"),
        "{out}"
    );
    assert_put_back(&l, &plan, &out, 2);
}

#[test]
fn a_new_replica_that_lands_off_the_named_disk_deletes_nothing() {
    let l = Longhorn::new("mvrd-off-disk");
    let (plan, hash) = l.plan();
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_NEW_DISK", W3_DATA)],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("did not name as a target"), "{out}");
    assert_put_back(&l, &plan, &out, 2);
}

#[test]
fn an_unread_volume_before_the_lower_puts_the_fields_back() {
    let l = Longhorn::new("mvrd-unread-before-lower");
    let (plan, hash) = l.plan();
    // Reads of the volume, counted from the plan's: 1 the plan, 2 the
    // write's render, 3 the wait (the stub builds the new replica healthy
    // at the raise), 4 the read before the lower, 5 the put-back's.
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_VOL_GET_FAIL_AT", "4")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("the read before lowering the count could not look"),
        "{out}"
    );
    assert_put_back(&l, &plan, &out, 2);
}

#[test]
fn an_original_replica_failing_during_the_move_puts_the_fields_back() {
    let l = Longhorn::new("mvrd-unwell");
    let (plan, hash) = l.plan();
    let cp2 = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-a2e85dc4";
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_UNWELL", cp2)],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!("not healthy: {cp2}")),
        "names the replica that failed: {out}"
    );
    assert_put_back(&l, &plan, &out, 2);
}

#[test]
fn a_refused_lower_puts_the_fields_back_and_keeps_the_count() {
    let l = Longhorn::new("mvrd-lower-refused");
    let (plan, hash) = l.plan();
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_PATCH_FAIL_AT", "2")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("(4 -> 3) did not succeed"), "{out}");
    assert_put_back(&l, &plan, &out, 3);
}

/// THE HOST IS PART OF THE FIXTURE (backlog 5f6a2cde, after b43f68bb):
/// bash can neither trap nor reset a TERM ignored when it starts, so the
/// case is run from a parent that leaves TERM alone AND from one that
/// ignores it, and `run` starts the script with every signal at its
/// default, so both read the same.
#[test]
fn a_signal_after_the_raise_puts_the_fields_back() {
    for host in ["default", "ignored"] {
        let l = Longhorn::new(&format!("mvrd-signal-{host}"));
        let (plan, hash) = l.plan();
        let parent: Vec<&str> = match host {
            "default" => vec![],
            _ => host_ignoring("TERM").to_vec(),
        };
        let o = l.run_from(
            &parent,
            &["--to-disk", VOL, REP, "u-postgres", &hash],
            &[("STUB_REBUILD", "never"), ("STUB_TERM_AFTER_RAISE", "1")],
        );
        let out = format!("TERM from a parent where it is {host}:\n{}", text(&o));
        assert_eq!(o.status.code(), Some(1), "{out}");
        assert!(out.contains("stopped by a signal"), "{out}");
        assert_put_back(&l, &plan, &out, 2);
    }
}

/// The put-back itself refused: nothing else can be done from here, so
/// the line says so loudly, with the fields the volume is left holding.
#[test]
fn a_refused_put_back_names_the_fields_left_standing() {
    let l = Longhorn::new("mvrd-put-back-refused");
    let (_, hash) = l.plan();
    let o = l.run(
        &["--to-disk", VOL, REP, "u-postgres", &hash],
        &[("STUB_REBUILD", "never"), ("STUB_PATCH_FAIL_AT", "2")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert_eq!(
        l.writes().len(),
        2,
        "the raise and the put-back: {:?}",
        l.writes()
    );
    let v = l.volume_now();
    assert_eq!(v["spec"]["numberOfReplicas"], 4, "{v}");
    assert_eq!(
        v["spec"]["diskSelector"],
        json!(["durable", "postgres"]),
        "{v}"
    );
    assert!(
        out.contains("PUT-BACK REFUSED")
            && out.contains("denied the request")
            && out.contains(
                "the volume reads spec.numberOfReplicas 4, spec.diskSelector \
                 [\"durable\",\"postgres\"], spec.replicaSoftAntiAffinity enabled"
            )
            && out.contains("only on a disk carrying"),
        "{out}"
    );
    assert!(l.replicas_now().iter().any(|r| r.starts_with(REP)));
}

// ------------------------------------------------------------------ verbs

#[test]
fn the_verbs_are_a_passkey_pair_over_one_script() {
    let w = verb("move-volume-replica-to-disk");
    let p = verb("plan-a-volume-replica-disk-move");
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-volume-replica-disk-move");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_ne!(p["requires_approval"], true, "the plan is read-only");
    assert!(
        p["about"]
            .as_str()
            .is_some_and(|a| a.starts_with("READ-ONLY"))
    );
    assert!(
        w["about"]
            .as_str()
            .is_some_and(|a| a.starts_with("MUTATING"))
    );
    assert_eq!(
        w["argv"],
        json!([SCRIPT, "--to-disk", "{1}", "{2}", "{3}", "{4}"])
    );
    assert_eq!(
        p["argv"],
        json!([SCRIPT, "--plan-to-disk", "{1}", "{2}", "{3}"])
    );
    let wp = w["params"].as_array().expect("params");
    assert_eq!(wp.last().expect("a hash")["name"], "plan_sha256");
    assert_eq!(&wp[..wp.len() - 1], p["params"].as_array().expect("params"));
    assert_eq!(w["hosts"], p["hosts"]);
}

// ----------------------------------------------------------------- finish
//
// WHY (backlog 6d0b3c96). On 2026-11-17 the signed move 02035ff0 raised
// boss/pgdata-postgres-0 to 4, and its new replica on u-postgres was still
// rebuilding when the 1200 s wait ran out (the volume's actualSize was
// ~95 GiB). The run failed as designed: the count stayed 4, the old
// replica was not deleted, and the put-back restored diskSelector [] and
// replicaSoftAntiAffinity ignored. That leaves 4 healthy replicas on 3
// nodes, w-3 holding two, and no verb could take it from there: the move's
// plan refuses 4 replicas on 3 nodes, and retire-volume-replica refuses two
// on one node. The finish is that door: `plan-a-volume-replica-move-finish`
// (`--plan-finish`) and `finish-volume-replica-move` (`--finish`).

const FINISH: &str = "finish-volume-replica-move";

impl Longhorn {
    /// The state move 02035ff0 left: count 4, four healthy replicas, w-3
    /// holding the old one on its default disk and the new one on
    /// u-postgres. `held` is a run that died before its put-back (SIGKILL,
    /// OOM, a refused put-back): the move's selector and anti-affinity
    /// still on the volume.
    fn stalled(name: &str, held: bool) -> Self {
        let l = Longhorn::new(name);
        let mut v = volume();
        v["spec"]["numberOfReplicas"] = json!(4);
        if held {
            v["spec"]["diskSelector"] = json!(["durable", "postgres"]);
            v["spec"]["replicaSoftAntiAffinity"] = json!("enabled");
        }
        l.volume(v);
        l.state(
            "replicas.json",
            &json!([
                replica(
                    "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-a2e85dc4",
                    "cp-2",
                    CP2_DISK
                ),
                replica(
                    "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-2bd9cc5a",
                    "cp-3",
                    CP3_DISK
                ),
                replica(REP, "w-3", W3_DEFAULT),
                replica(NEW, "w-3", W3_POSTGRES),
            ])
            .to_string(),
        );
        l
    }

    fn finish_plan(&self, rep: &str) -> (String, String) {
        let o = self.run(&["--plan-finish", VOL, rep], &[]);
        assert!(o.status.success(), "the finish plan renders: {}", text(&o));
        let stderr = String::from_utf8_lossy(&o.stderr).to_string();
        let hash = stderr
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("plan-sha256 on stderr: {stderr}"))
            .trim()
            .to_string();
        (String::from_utf8_lossy(&o.stdout).to_string(), hash)
    }

    fn finish_refused(&self, rep: &str) -> String {
        let o = self.run(&["--plan-finish", VOL, rep], &[]);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{rep} is refused: {out}");
        assert!(
            !out.lines().any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        assert!(self.writes().is_empty(), "{:?}", self.writes());
        out
    }
}

/// The finish's one compare-and-set from the state 02035ff0 left: the
/// count 4 -> 3, the two fields tested as read and left alone.
const FINISH_PATCH: &str = r#"[{"op":"test","path":"/spec/numberOfReplicas","value":4},{"op":"test","path":"/spec/diskSelector","value":[]},{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"ignored"},{"op":"replace","path":"/spec/numberOfReplicas","value":3}]"#;

#[test]
fn the_finish_plan_names_the_pair_and_the_patch_it_sends() {
    let l = Longhorn::stalled("mvrf-plan", false);
    let (plan, hash) = l.finish_plan(REP);
    for want in [
        "plan: finish-volume-replica-move".to_string(),
        format!("volume: {VOL}"),
        "claim: boss/pgdata-postgres-0".to_string(),
        "shape: 4 healthy replicas on 3 distinct nodes — w-3 holds two".to_string(),
        format!(
            "retire: replica {REP} on w-3 (disk {W3_DEFAULT}, default-disk-38052ee5); \
             {NEW} stays on w-3 (disk {W3_POSTGRES}, u-postgres)"
        ),
        "numberOfReplicas: 4 -> 3".to_string(),
        "spec.diskSelector [] and spec.replicaSoftAntiAffinity ignored are not the move's held \
         values"
            .to_string(),
        "after: spec.diskSelector [] admits every remaining replica's disk \
         (allow-empty-disk-selector-volume true)"
            .to_string(),
        format!("patch: {FINISH_PATCH}"),
        format!("kubectl delete replicas.longhorn.io {REP} -n longhorn-system"),
        format!("replica {NEW} on w-3 (disk {W3_POSTGRES}): healthy"),
    ] {
        assert!(plan.contains(&want), "the plan says {want:?}:\n{plan}");
    }
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(l.writes().is_empty(), "the plan writes nothing");
    assert!(effect_lines_of(FINISH, &plan).is_empty(), "{plan}");
    assert_eq!(l.finish_plan(REP), (plan, hash), "deterministic");
}

#[test]
fn the_finish_lowers_deletes_the_named_replica_and_reads_it_back() {
    let l = Longhorn::stalled("mvrf-write", false);
    let (plan, hash) = l.finish_plan(REP);
    let o = l.run(&["--finish", VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert!(
        String::from_utf8_lossy(&o.stdout).starts_with(&plan),
        "the approved plan is printed first: {out}"
    );
    let w = l.writes();
    assert_eq!(w.len(), 2, "one compare-and-set and one delete: {w:?}");
    assert_eq!(patch_body(&w[0]), FINISH_PATCH, "{w:?}");
    assert!(
        w[1].starts_with(&format!(
            "delete replicas.longhorn.io {REP} -n longhorn-system"
        )),
        "{w:?}"
    );
    let v = l.volume_now();
    assert_eq!(v["spec"]["numberOfReplicas"], 3, "{v}");
    assert_eq!(v["spec"]["diskSelector"], json!([]), "{v}");
    assert_eq!(v["spec"]["replicaSoftAntiAffinity"], "ignored", "{v}");
    let now = l.replicas_now();
    assert_eq!(now.len(), 3, "{now:?}");
    assert!(!now.iter().any(|r| r.starts_with(REP)), "{now:?}");
    assert!(
        now.contains(&format!("{NEW} on w-3 ({W3_POSTGRES})")),
        "{now:?}"
    );
    let hits = effect_lines_of(FINISH, &out);
    assert_eq!(hits.len(), 1, "one effect line: {out}");
    assert!(
        hits[0].contains("(cp-2, cp-3, w-3)")
            && hits[0].contains(&format!(
                "{NEW} stays healthy on w-3 disk u-postgres (diskUUID {W3_POSTGRES})"
            ))
            && hits[0].contains("spec.diskSelector [], spec.replicaSoftAntiAffinity ignored"),
        "{hits:?}"
    );
    assert!(
        effect_lines(&out).is_empty(),
        "the move's effect is not claimed: {out}"
    );

    // Applied once, the approval is spent: the shape is gone.
    let o = l.run(&["--finish", VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(l.writes().len(), 2, "no second finish: {:?}", l.writes());
}

/// A run that died before its put-back (review 1c413d8d finding 3:
/// SIGKILL, OOM, host loss, a refused put-back) left the move's selector
/// and anti-affinity on the volume. The finish clears both in the same
/// compare-and-set that lowers the count.
#[test]
fn a_held_selector_is_cleared_in_the_same_compare_and_set() {
    let l = Longhorn::stalled("mvrf-held", true);
    let (plan, hash) = l.finish_plan(REP);
    assert!(
        plan.contains(
            "spec.diskSelector [\"durable\",\"postgres\"] and spec.replicaSoftAntiAffinity \
             enabled are the move's own"
        ),
        "{plan}"
    );
    let o = l.run(&["--finish", VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let w = l.writes();
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(
        patch_body(&w[0]),
        r#"[{"op":"test","path":"/spec/numberOfReplicas","value":4},{"op":"test","path":"/spec/diskSelector","value":["durable","postgres"]},{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"enabled"},{"op":"replace","path":"/spec/numberOfReplicas","value":3},{"op":"replace","path":"/spec/diskSelector","value":[]},{"op":"replace","path":"/spec/replicaSoftAntiAffinity","value":"ignored"}]"#,
        "{w:?}"
    );
    assert!(plan.contains(patch_body(&w[0])), "signed verbatim:\n{plan}");
    let v = l.volume_now();
    assert_eq!(v["spec"]["numberOfReplicas"], 3, "{v}");
    assert_eq!(v["spec"]["diskSelector"], json!([]), "{v}");
    assert_eq!(v["spec"]["replicaSoftAntiAffinity"], "ignored", "{v}");
    assert_eq!(effect_lines_of(FINISH, &out).len(), 1, "{out}");
}

/// Restore is the same door the other way round: name the NEW replica,
/// and the old one stays where it was.
#[test]
fn the_finish_restores_by_retiring_the_new_replica() {
    let l = Longhorn::stalled("mvrf-restore", false);
    let (_, hash) = l.finish_plan(NEW);
    let o = l.run(&["--finish", VOL, NEW, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let now = l.replicas_now();
    assert!(!now.iter().any(|r| r.starts_with(NEW)), "{now:?}");
    assert!(
        now.contains(&format!("{REP} on w-3 ({W3_DEFAULT})")),
        "{now:?}"
    );
    let hits = effect_lines_of(FINISH, &out);
    assert_eq!(hits.len(), 1, "{out}");
    assert!(
        hits[0].contains(&format!(
            "{REP} stays healthy on w-3 disk default-disk-38052ee5"
        )),
        "{hits:?}"
    );
}

/// A finish killed after its lower: the count is already 3 against four
/// replicas. A NEW plan signs that state; its compare-and-set only tests.
#[test]
fn a_stopped_finish_resumes_at_the_lowered_count() {
    let l = Longhorn::stalled("mvrf-resume", false);
    let mut v = l.volume_now();
    v["spec"]["numberOfReplicas"] = json!(3);
    l.volume(v);
    let (plan, hash) = l.finish_plan(REP);
    assert!(
        plan.contains("numberOfReplicas: 3, already lowered"),
        "{plan}"
    );
    let o = l.run(&["--finish", VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let w = l.writes();
    assert_eq!(
        patch_body(&w[0]),
        r#"[{"op":"test","path":"/spec/numberOfReplicas","value":3},{"op":"test","path":"/spec/diskSelector","value":[]},{"op":"test","path":"/spec/replicaSoftAntiAffinity","value":"ignored"}]"#,
        "test-only: {w:?}"
    );
    assert_eq!(l.replicas_now().len(), 3);
    assert_eq!(effect_lines_of(FINISH, &out).len(), 1, "{out}");
}

#[test]
fn the_finish_refuses_every_other_shape_and_writes_nothing() {
    // The named replica is alone on its node: retiring it costs cp-2 its
    // only copy.
    let l = Longhorn::stalled("mvrf-alone", false);
    let out = l.finish_refused("pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-a2e85dc4");
    assert!(out.contains("exactly one other"), "{out}");

    // A plain three-on-three volume: nothing to finish.
    let l = Longhorn::new("mvrf-plain");
    let out = l.finish_refused(REP);
    assert!(out.contains("exactly one other"), "{out}");

    // The new replica still rebuilding.
    let l = Longhorn::stalled("mvrf-rebuilding", false);
    let mut r: Value = serde_json::from_str(
        &std::fs::read_to_string(l.dir.join("state/replicas.json")).expect("state"),
    )
    .expect("json");
    r[3]["spec"]["healthyAt"] = json!("");
    l.state("replicas.json", &r.to_string());
    let out = l.finish_refused(REP);
    assert!(out.contains("every copy is healthy"), "{out}");

    // A count no stopped move leaves.
    let l = Longhorn::stalled("mvrf-count", false);
    let mut v = l.volume_now();
    v["spec"]["numberOfReplicas"] = json!(5);
    l.volume(v);
    let out = l.finish_refused(REP);
    assert!(out.contains("not a stopped move's"), "{out}");

    // Anti-affinity soft under a selector that is no disk's tags here:
    // not what a move leaves, so nothing says what to put back.
    let l = Longhorn::stalled("mvrf-foreign-selector", false);
    let mut v = l.volume_now();
    v["spec"]["diskSelector"] = json!(["durable"]);
    v["spec"]["replicaSoftAntiAffinity"] = json!("enabled");
    l.volume(v);
    let out = l.finish_refused(REP);
    assert!(out.contains("not the state"), "{out}");

    // Held, but the cleared selector would strand the replica that stays.
    let l = Longhorn::stalled("mvrf-strand", true);
    l.setting("allow-empty-disk-selector-volume", "false");
    let out = l.finish_refused(REP);
    assert!(out.contains("does not admit"), "{out}");

    // Longhorn could trim a replica of its own choosing on the lowered count.
    let l = Longhorn::stalled("mvrf-autobalance", false);
    l.setting("replica-auto-balance", "best-effort");
    let out = l.finish_refused(REP);
    assert!(out.contains("auto-balance"), "{out}");
}

#[test]
fn the_finish_refuses_a_hash_that_is_not_todays_plan() {
    let l = Longhorn::stalled("mvrf-drift", false);
    let (_, hash) = l.finish_plan(REP);
    let mut v = l.volume_now();
    v["spec"]["diskSelector"] = json!(["durable"]);
    l.volume(v);
    let o = l.run(&["--finish", VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(l.writes().is_empty(), "{:?}", l.writes());
}

#[test]
fn a_refused_finish_lower_deletes_nothing() {
    let l = Longhorn::stalled("mvrf-lower-refused", false);
    let (_, hash) = l.finish_plan(REP);
    let o = l.run(
        &["--finish", VOL, REP, &hash],
        &[("STUB_PATCH_FAIL_AT", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert_eq!(l.writes().len(), 1, "{:?}", l.writes());
    assert!(
        out.contains("did not succeed") && out.contains("NOT deleted"),
        "{out}"
    );
    assert_eq!(l.volume_now()["spec"]["numberOfReplicas"], 4);
    assert_eq!(l.replicas_now().len(), 4);
    assert!(effect_lines_of(FINISH, &out).is_empty(), "{out}");
}

#[test]
fn the_finish_argv_bound_is_the_scripts_and_opens_no_door() {
    let l = Longhorn::stalled("mvrf-argv", false);
    for args in [
        vec!["--plan-finish", VOL],
        vec!["--plan-finish", VOL, REP, "u-postgres"],
        vec![
            "--plan-finish",
            VOL,
            "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-zz",
        ],
        vec!["--finish", VOL, REP],
        vec!["--finish", VOL, REP, "not-a-hash"],
    ] {
        let o = l.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
    }
    assert!(!l.dir.join("argv").exists(), "no door was opened");
}

#[test]
fn the_finish_verbs_are_a_passkey_pair_over_the_same_script() {
    let w = verb(FINISH);
    let p = verb("plan-a-volume-replica-move-finish");
    assert_eq!(w["requires_approval"], true);
    assert_eq!(w["plan_verb"], "plan-a-volume-replica-move-finish");
    assert_eq!(w["approvers"], json!(["emp-david"]));
    assert_ne!(p["requires_approval"], true, "the plan is read-only");
    assert!(
        p["about"]
            .as_str()
            .is_some_and(|a| a.starts_with("READ-ONLY"))
    );
    assert!(
        w["about"]
            .as_str()
            .is_some_and(|a| a.starts_with("MUTATING"))
    );
    assert_eq!(w["argv"], json!([SCRIPT, "--finish", "{1}", "{2}", "{3}"]));
    assert_eq!(p["argv"], json!([SCRIPT, "--plan-finish", "{1}", "{2}"]));
    let wp = w["params"].as_array().expect("params");
    assert_eq!(wp.last().expect("a hash")["name"], "plan_sha256");
    assert_eq!(&wp[..wp.len() - 1], p["params"].as_array().expect("params"));
    assert_eq!(w["hosts"], p["hosts"]);
}
