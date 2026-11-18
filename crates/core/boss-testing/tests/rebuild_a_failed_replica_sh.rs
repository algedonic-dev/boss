//! The two ops verbs that rebuild one stuck replica of a Longhorn volume
//! from the forge (backlog 776bcb59): the read-only
//! `plan-a-failed-replica-rebuild` (`infra/forge/rebuild-a-failed-replica.sh
//! --plan`) and the passkey-approved `rebuild-a-failed-replica`.
//!
//! WHY. The system of record's database volume (pvc-93e11a6e…, 68 GiB,
//! three replicas) read robustness degraded from 2026-10-30: its cp-2
//! replica r-1269eb6e sat in mode WO with the engine's rebuild at 100
//! percent, complete and not rebuilding, healthyAt empty, while the other
//! two served RW. Every volume door refused that shape (the retirement by
//! its floor of 3, purge and move on robustness), so the fix — delete the
//! stuck copy and let Longhorn build a fresh one at the same count — had
//! no bounded door. These tests hold that door to its bound.
//!
//! THE ESTATE in these tests is that one: three replicas on cp-1, cp-2
//! and cp-3, the cp-2 one WO and stalled since the engine restart of
//! 2026-11-04T06:11:35Z, read at 15:00Z.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small Longhorn behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc` (retire_volume_replica_sh's stand-in, with an engine
//! added). A raw delete must carry the uid the plan signed; the deleted
//! replica stays LISTED, terminating, for one read, then goes, and if
//! fewer replicas then stand than the count Longhorn replenishes one onto
//! w-1 in mode WO, which turns RW and healthy on the next read (unless
//! STUB_REBUILD_STUCK). The clock is a file only `sleep` moves. Every door
//! call lands in `argv`, so a refusal's lack of writes is read from the
//! record. Nothing here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/rebuild-a-failed-replica.sh";
const VOL: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56";
const REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-1269eb6e";
const REP_UID: &str = "0b1c2d3e-0000-4000-8000-00000000cp02";
const CP1_REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-2bd9cc5a";
const CP3_REP: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-90eaac42";
const NEW: &str = "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56-r-feedface";
const CP1_DISK: &str = "844915d9-6a5b-4da9-9619-8f3d188d632b";
const CP2_DISK: &str = "976a5246-7a68-452c-bb25-f9e3ecb2258e";
const CP3_DISK: &str = "9430cd68-91a7-41e9-9bfb-7bcb98bc8cb1";
/// 2026-11-04T15:00:00Z, the reading's clock.
const NOW: &str = "1793804400";
/// The engine restart every replica transitioned at.
const RESTART: &str = "2026-11-04T06:11:35Z";

const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" >> "$STUB_ARGV"
S="$STUB_STATE"
stdin_enabled=no
while [ $# -gt 0 ]; do
    [ "$1" = -i ] && stdin_enabled=yes
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
if [ -n "${STUB_FORBIDDEN:-}" ]; then
    echo "Error from server (Forbidden): $2 is forbidden" >&2
    exit 1
fi
reps() { jq "$1" "$S/replicas.json" > "$S/r.new" && mv "$S/r.new" "$S/replicas.json"; }
eng() { jq "$1" "$S/engine.json" > "$S/e.new" && mv "$S/e.new" "$S/engine.json"; }
robust() {
    f="$S/vol.json"
    r=$(jq -r --slurpfile e "$S/engine.json" --argjson c "$(jq '.spec.numberOfReplicas' "$f")" \
        '[.[] | select((.spec.healthyAt // "") != "" and (.spec.failedAt // "") == "" and .spec.active == true and (.metadata.deletionTimestamp // "") == "" and ($e[0].status.replicaModeMap[.metadata.name] // "") == "RW")] | if length >= $c then "healthy" else "degraded" end' \
        "$S/replicas.json")
    jq --arg r "$r" '.status.robustness = $r' "$f" > "$f.new" && mv "$f.new" "$f"
}
# After a list: the rebuilding replacement finishes (unless stuck), then
# every replica already shown terminating goes, and Longhorn replenishes.
progress() {
    if [ -z "${STUB_REBUILD_STUCK:-}" ] && [ -n "$(jq -r '.[] | select(.metadata.name == "'"$NEW"'") | .metadata.name' "$S/replicas.json")" ]; then
        reps 'map(if .metadata.name == "'"$NEW"'" then .spec.healthyAt = "2026-11-04T15:20:00Z" else . end)'
        eng '.status.replicaModeMap["'"$NEW"'"] = "RW" | .status.rebuildStatus = {}'
    fi
    for r in $(jq -r '.[] | select((.metadata.deletionTimestamp // "") != "") | .metadata.name' "$S/replicas.json"); do
        reps "map(select(.metadata.name != \"$r\"))"
        eng "del(.status.replicaModeMap[\"$r\"]) | del(.status.replicaTransitionTimeMap[\"$r\"])"
        if [ -z "${STUB_NO_REPLENISH:-}" ] && [ "$(jq length "$S/replicas.json")" -lt "$(jq '.spec.numberOfReplicas' "$S/vol.json")" ]; then
            reps '. + [{"apiVersion":"longhorn.io/v1beta2","kind":"Replica","metadata":{"name":"'"$NEW"'","namespace":"longhorn-system","uid":"new-uid"},"spec":{"volumeName":"'"$VOL"'","nodeID":"w-1","diskID":"5d1c0a2e-7b44-4f0e-9a51-0c6e2b1d9f10","healthyAt":"","failedAt":"","active":true},"status":{"currentState":"running"}}]'
            eng '.status.replicaModeMap["'"$NEW"'"] = "WO" | .status.rebuildStatus = {"tcp://10.244.4.4:10000": {"isRebuilding": true, "progress": 1, "state": "in_progress"}}'
        fi
    done
    robust
}
case "$1 $2" in
"get volumes.longhorn.io")
    [ "$3" = "$VOL" ] || { echo "Error from server (NotFound): volumes.longhorn.io \"$3\" not found" >&2; exit 1; }
    # The volume is the first of each read's three answers, so a drift
    # applied here is seen by the whole read: volume, replicas and engine.
    n=$(cat "$S/vreads" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "$S/vreads"
    if [ -n "${STUB_DRIFT_AT:-}" ] && [ "$n" = "$STUB_DRIFT_AT" ]; then
        jq "$STUB_DRIFT" "$S/$STUB_DRIFT_FILE" > "$S/d.new" && mv "$S/d.new" "$S/$STUB_DRIFT_FILE"
    fi
    cat "$S/vol.json"; exit 0 ;;
"get replicas.longhorn.io")
    n=$(cat "$S/reads" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" > "$S/reads"
    if [ -n "${STUB_SWAP_AT:-}" ] && [ "$n" = "$STUB_SWAP_AT" ]; then
        reps 'map(if .metadata.name == "'"$STUB_SWAP_NAME"'" then .metadata.uid = "swapped-uid" else . end)'
    fi
    printf '{"apiVersion":"v1","kind":"List","items":%s}\n' "$(cat "$S/replicas.json")"
    progress; exit 0 ;;
"get engines.longhorn.io")
    printf '{"apiVersion":"v1","kind":"List","items":[%s]}\n' "$(cat "$S/engine.json")"; exit 0 ;;
"delete --raw")
    [ "$stdin_enabled" = yes ] || { echo 'a raw delete needs docker -i' >&2; exit 1; }
    path="$3"; r=${path##*/}
    cat > "$S/delete-options.json"
    [ -z "${STUB_DELETE_FAIL:-}" ] || { echo "Error from server: deletion refused" >&2; exit 1; }
    uid=$(jq -r '.preconditions.uid' "$S/delete-options.json")
    actual=$(jq -r --arg r "$r" '.[] | select(.metadata.name == $r) | .metadata.uid' "$S/replicas.json")
    [ -n "$actual" ] && [ "$uid" = "$actual" ] || { echo 'Operation cannot be fulfilled: Precondition failed: UID in precondition' >&2; exit 1; }
    reps "map(if .metadata.name == \"$r\" then .metadata.deletionTimestamp = \"2026-11-04T15:00:05Z\" else . end)"
    echo '{"kind":"Status","status":"Success"}'; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

const CLOCK_DATE: &str = r#"#!/bin/sh
[ "$*" = "-u +%s" ] || { echo "stub date: unexpected call: $*" >&2; exit 2; }
cat "$STUB_STATE/clock"
"#;
const CLOCK_SLEEP: &str = r#"#!/bin/sh
case "${1:-}" in ''|*[!0-9]*) echo "stub sleep: unexpected call: $*" >&2; exit 2 ;; esac
now=$(cat "$STUB_STATE/clock") || exit 2
echo $((now + $1)) > "$STUB_STATE/clock"
"#;

fn needs_tools() {
    for tool in ["jq", "sha256sum"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "rebuild_a_failed_replica_sh: no {tool} on this box — the gate image has it, and a \
             trust-boundary test that cannot run must fail, never pass by returning early"
        );
    }
}

fn replica(name: &str, uid: &str, node: &str, disk: &str, healthy: bool) -> Value {
    json!({
        "apiVersion": "longhorn.io/v1beta2", "kind": "Replica",
        "metadata": {"name": name, "namespace": "longhorn-system", "uid": uid},
        "spec": {"volumeName": VOL, "nodeID": node, "diskID": disk, "active": true,
                 "healthyAt": if healthy { "2026-11-04T06:11:40Z" } else { "" },
                 "lastHealthyAt": if healthy { "2026-11-04T06:11:40Z" } else { "2026-10-30T20:00:40Z" },
                 "failedAt": "",
                 "lastFailedAt": if healthy { "" } else { "2026-10-30T20:34:45Z" }},
        "status": {"currentState": "running"}
    })
}

struct Longhorn {
    dir: PathBuf,
}

impl Longhorn {
    /// The estate of 2026-11-04T15:00Z: cp-1 and cp-3 RW and healthy,
    /// cp-2 WO, rebuild complete, not rebuilding, healthyAt empty.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(name);
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_exec(&dir.join("bin/sudo"), STUB);
        write_exec(&dir.join("bin/date"), CLOCK_DATE);
        write_exec(&dir.join("bin/sleep"), CLOCK_SLEEP);
        let l = Longhorn { dir };
        l.state("clock", &format!("{NOW}\n"));
        l.state(
            "vol.json",
            &json!({
                "apiVersion": "longhorn.io/v1beta2", "kind": "Volume",
                "metadata": {"name": VOL, "namespace": "longhorn-system",
                             "uid": "93e11a6e-0000-4000-8000-0000000000aa"},
                "spec": {"numberOfReplicas": 3, "size": "73014444032", "nodeID": "cp-1"},
                "status": {"state": "attached", "robustness": "degraded", "currentNodeID": "cp-1",
                           "kubernetesStatus": {"namespace": "boss", "pvcName": "pgdata-postgres-0"}}
            })
            .to_string(),
        );
        l.replicas(json!([
            replica(
                CP1_REP,
                "0b1c2d3e-0000-4000-8000-00000000cp01",
                "cp-1",
                CP1_DISK,
                true
            ),
            replica(REP, REP_UID, "cp-2", CP2_DISK, false),
            replica(
                CP3_REP,
                "0b1c2d3e-0000-4000-8000-00000000cp03",
                "cp-3",
                CP3_DISK,
                true
            ),
        ]));
        l.engine(|_| {});
        l
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn read(&self, file: &str) -> Value {
        let p = self.dir.join("state").join(file);
        serde_json::from_str(&std::fs::read_to_string(p).expect("state")).expect("json")
    }

    fn replicas(&self, r: Value) {
        self.state("replicas.json", &r.to_string());
    }

    fn edit_replicas(&self, f: impl FnOnce(&mut Vec<Value>)) {
        let mut r = self.read("replicas.json");
        f(r.as_array_mut().expect("an array"));
        self.replicas(r);
    }

    fn edit_volume(&self, f: impl FnOnce(&mut Value)) {
        let mut v = self.read("vol.json");
        f(&mut v);
        self.state("vol.json", &v.to_string());
    }

    /// The one engine, with the modes and the rebuild status of the
    /// reading, then `f` to change it.
    fn engine(&self, f: impl FnOnce(&mut Value)) {
        let mut e = json!({
            "apiVersion": "longhorn.io/v1beta2", "kind": "Engine",
            "metadata": {"name": format!("{VOL}-e-0"), "namespace": "longhorn-system",
                         "uid": "e0e0e0e0-0000-4000-8000-0000000000e0"},
            "spec": {"volumeName": VOL, "nodeID": "cp-1"},
            "status": {"currentState": "running",
                       "replicaModeMap": {CP1_REP: "RW", REP: "WO", CP3_REP: "RW"},
                       "replicaTransitionTimeMap": {CP1_REP: RESTART, REP: RESTART, CP3_REP: RESTART},
                       "rebuildStatus": {"tcp://10.244.2.9:10000": {
                           "isRebuilding": false, "progress": 100, "state": "complete",
                           "appliedRebuildingMBps": 0}}}
        });
        f(&mut e);
        self.state("engine.json", &e.to_string());
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
            .env_remove("BOSS_REBUILD_STALL_S")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("BOSS_REBUILD_START_S", "3")
            .env("BOSS_REBUILD_READBACK_S", "3")
            .env("BOSS_REBUILD_POLL_S", "1")
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"))
            .env("VOL", VOL)
            .env("NEW", NEW);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().expect("the script runs")
    }

    /// Every door call, with the door's prefix cut off.
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

    fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| {
                c.starts_with("patch ")
                    || c.starts_with("delete ")
                    || c.starts_with("create ")
                    || c.starts_with("apply ")
            })
            .collect()
    }

    fn plan(&self) -> (String, String) {
        let o = self.run(&["--plan", VOL, REP], &[]);
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

    /// A plan that must refuse: exit 78, no plan, no write; the words.
    fn refused(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let o = self.run(args, env);
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{args:?} is refused: {out}");
        assert!(
            !out.lines().any(|l| l.starts_with("plan: ")),
            "a refusal renders no plan: {out}"
        );
        assert!(self.writes().is_empty(), "{:?}", self.writes());
        out
    }

    fn refused_plan(&self) -> String {
        self.refused(&["--plan", VOL, REP], &[])
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
    serde_json::from_str(&std::fs::read_to_string(&p).expect("verb file")).expect("verb json")
}

/// Does `line` match `re` in the runner's own engine (jq's `test`)?
fn jq_test(re: &str, line: &str) -> bool {
    let out = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "re",
            re,
            "--arg",
            "l",
            line,
            "$l | test($re)",
        ])
        .output()
        .expect("jq runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim() == "true"
}

fn effect_lines(out: &str) -> Vec<String> {
    let re = verb("rebuild-a-failed-replica")["effect"]
        .as_str()
        .expect("the verb declares its effect")
        .to_string();
    out.lines()
        .filter(|l| jq_test(&re, l))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_verb_pair_is_a_read_only_plan_and_a_passkey_write_of_its_bytes() {
    let plan = verb("plan-a-failed-replica-rebuild");
    let write = verb("rebuild-a-failed-replica");
    assert_eq!(write["requires_approval"], true);
    assert_eq!(write["plan_verb"], "plan-a-failed-replica-rebuild");
    assert_eq!(write["approvers"], json!(["emp-david"]));
    assert_ne!(plan["requires_approval"], true, "the plan is read-only");
    assert!(
        !plan["about"]
            .as_str()
            .unwrap_or_default()
            .contains("MUTATING"),
        "the plan verb must not say MUTATING"
    );
    assert!(
        write["about"]
            .as_str()
            .unwrap_or_default()
            .starts_with("MUTATING")
    );
    assert_eq!(plan["argv"], json!([SCRIPT, "--plan", "{1}", "{2}"]));
    assert_eq!(write["argv"], json!([SCRIPT, "{1}", "{2}", "{3}"]));
    let mut wp = write["params"].as_array().expect("params").clone();
    assert_eq!(wp.pop().expect("a hash")["name"], "plan_sha256");
    assert_eq!(&wp, plan["params"].as_array().expect("params"));
    for p in &wp {
        let re = p["pattern"].as_str().expect("a pattern");
        assert!(
            jq_test(re, VOL) || jq_test(re, REP),
            "{re} admits the estate's names"
        );
        assert!(
            !jq_test(re, "-x") && !jq_test(re, "a b"),
            "{re} admits no dash or space"
        );
    }
}

#[test]
fn the_plan_names_the_stuck_copy_its_uid_and_an_unchanged_count_and_writes_nothing() {
    let l = Longhorn::new("rebuild-plan");
    let (plan, hash) = l.plan();
    assert_eq!(hash.len(), 64);
    for want in [
        "plan: rebuild-a-failed-replica",
        &format!("volume: {VOL} (uid 93e11a6e-0000-4000-8000-0000000000aa)"),
        "claim: boss/pgdata-postgres-0",
        "robustness: degraded",
        "numberOfReplicas: 3 -> 3 — unchanged",
        &format!("rebuild: replica {REP} (uid {REP_UID}) on cp-2 (disk {CP2_DISK}): mode WO"),
        "lastFailedAt '2026-10-30T20:34:45Z'",
        &format!("stalled: not RW since {RESTART} at the latest, past the stall bound of 3600s"),
        "remain: 2 RW healthy replicas on 2 distinct nodes (cp-1, cp-3)",
        &format!("preconditions.uid {REP_UID}"),
        // The engine the write holds itself to at step 2 is in the
        // signed bytes (review run 9b9e2275, R1).
        &format!(
            "the one engine still {VOL}-e-0 (uid e0e0e0e0-0000-4000-8000-0000000000e0) and running"
        ),
        // And so is the history it holds there (review run ffd7de14, R2).
        "with its history marks as signed above and still past the stall bound of 3600s",
    ] {
        assert!(plan.contains(want), "the plan says {want:?}:\n{plan}");
    }
    assert!(
        l.writes().is_empty(),
        "a plan writes nothing: {:?}",
        l.writes()
    );
    // Two renders of one state hash alike, though the clock moved: no
    // clock reading is in the signed bytes.
    l.state("clock", "1793808000\n");
    let (again, hash2) = l.plan();
    assert_eq!((again, hash2), (plan, hash));
}

#[test]
fn the_plan_refuses_every_shape_but_one_stalled_non_rw_copy_beside_rw_ones() {
    // The named copy serves RW.
    let l = Longhorn::new("rebuild-refuse-rw");
    l.engine(|e| e["status"]["replicaModeMap"][REP] = json!("RW"));
    assert!(l.refused_plan().contains("reads RW in the engine"));

    // Longhorn counts the named copy healthy.
    let l = Longhorn::new("rebuild-refuse-healthy");
    l.edit_replicas(|r| r[1]["spec"]["healthyAt"] = json!("2026-11-04T06:11:40Z"));
    assert!(l.refused_plan().contains("counts"));

    // A second copy is not RW: deleting this one is not the whole story.
    let l = Longhorn::new("rebuild-refuse-two");
    l.engine(|e| e["status"]["replicaModeMap"][CP3_REP] = json!("WO"));
    let out = l.refused_plan();
    assert!(out.contains("1 of 2 do"), "{out}");
    assert!(out.contains(CP3_REP), "{out}");

    // Two retained copies on one node.
    let l = Longhorn::new("rebuild-refuse-shared-node");
    l.edit_replicas(|r| r[2]["spec"]["nodeID"] = json!("cp-1"));
    assert!(l.refused_plan().contains("two share a node"));

    // A rebuild is in progress.
    let l = Longhorn::new("rebuild-refuse-rebuilding");
    l.engine(|e| {
        e["status"]["rebuildStatus"]["tcp://10.244.2.9:10000"]["isRebuilding"] = json!(true)
    });
    assert!(l.refused_plan().contains("rebuild in progress"));

    // Not stalled yet: its last transition was half an hour ago.
    let l = Longhorn::new("rebuild-refuse-fresh");
    l.engine(|e| e["status"]["replicaTransitionTimeMap"][REP] = json!("2026-11-04T14:30:00Z"));
    assert!(l.refused_plan().contains("under the stall bound"));

    // No history at all to judge a stall by.
    let l = Longhorn::new("rebuild-refuse-no-history");
    l.engine(|e| {
        e["status"]["replicaTransitionTimeMap"]
            .as_object_mut()
            .expect("a map")
            .remove(REP);
    });
    l.edit_replicas(|r| {
        r[1]["spec"]["lastHealthyAt"] = json!("");
        r[1]["spec"]["lastFailedAt"] = json!("");
    });
    assert!(l.refused_plan().contains("no history"));

    // Two declared copies: one RW would be left.
    let l = Longhorn::new("rebuild-refuse-two-count");
    l.edit_volume(|v| v["spec"]["numberOfReplicas"] = json!(2));
    l.edit_replicas(|r| {
        r.remove(2);
    });
    let out = l.refused_plan();
    assert!(out.contains("at least 3"), "{out}");

    // A count its replicas do not match.
    let l = Longhorn::new("rebuild-refuse-count");
    l.edit_volume(|v| v["spec"]["numberOfReplicas"] = json!(4));
    assert!(l.refused_plan().contains("do not match"));

    // Detached; robustness healthy; one replica already being deleted.
    let l = Longhorn::new("rebuild-refuse-detached");
    l.edit_volume(|v| v["status"]["state"] = json!("detached"));
    assert!(l.refused_plan().contains("not attached"));
    let l = Longhorn::new("rebuild-refuse-robust");
    l.edit_volume(|v| v["status"]["robustness"] = json!("healthy"));
    assert!(l.refused_plan().contains("robustness healthy"));
    let l = Longhorn::new("rebuild-refuse-deleting");
    l.edit_replicas(|r| r[1]["metadata"]["deletionTimestamp"] = json!("2026-11-04T14:59:00Z"));
    assert!(l.refused_plan().contains("being deleted already"));

    // Two engines (a live migration).
    let l = Longhorn::new("rebuild-refuse-engines");
    let e = l.read("engine.json");
    let mut e2 = e.clone();
    e2["metadata"]["name"] = json!(format!("{VOL}-e-1"));
    l.state("engine.json", &format!("{e},{e2}"));
    assert!(l.refused_plan().contains("2 engines"));

    // Words the argv bound does not admit, and a replica of another volume.
    let l = Longhorn::new("rebuild-refuse-args");
    l.refused(&["--plan", VOL], &[]);
    l.refused(&["--plan", VOL, "r-1269eb6e"], &[]);
    l.refused(
        &[
            "--plan",
            VOL,
            "pvc-11111111-2222-3333-4444-555555555555-r-1269eb6e",
        ],
        &[],
    );
    l.refused(&["--plan", VOL, &format!("{VOL}-r-0000abcd")], &[]);
    l.refused(&[VOL, REP, "not-a-hash"], &[]);
    l.refused(
        &["--plan", "pvc-00000000-0000-0000-0000-000000000000", REP],
        &[],
    );
}

#[test]
fn a_read_that_cannot_look_cannot_answer_and_writes_nothing() {
    let l = Longhorn::new("rebuild-forbidden");
    let o = l.run(&["--plan", VOL, REP], &[("STUB_FORBIDDEN", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("plan: ")), "{out}");
    assert!(l.writes().is_empty());
}

#[test]
fn the_write_deletes_exactly_the_signed_object_and_reads_the_rebuild_back() {
    let l = Longhorn::new("rebuild-write");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    assert_eq!(
        l.writes(),
        vec![format!(
            "delete --raw /apis/longhorn.io/v1beta2/namespaces/longhorn-system/replicas/{REP} -f - --request-timeout=20s"
        )],
        "one delete, by name, and nothing patched: {out}"
    );
    assert_eq!(
        l.read("delete-options.json")["preconditions"]["uid"],
        REP_UID,
        "the delete carries the uid the plan signed"
    );
    assert_eq!(
        l.read("vol.json")["spec"]["numberOfReplicas"],
        3,
        "the count is unchanged"
    );
    let effect = effect_lines(&out);
    assert_eq!(
        effect,
        vec![format!(
            "rebuild-a-failed-replica: {VOL} has 3 healthy RW replicas on 3 distinct nodes (cp-1, cp-3, w-1) — {REP} is gone, rebuilt as {NEW} on w-1 — read back: spec.numberOfReplicas=3 unchanged, robustness healthy (after 0s)"
        )],
        "exactly the read-back line shows the effect: {out}"
    );
}

#[test]
fn the_write_refuses_a_hash_that_is_not_todays_plan_and_a_second_run() {
    let l = Longhorn::new("rebuild-hash");
    let o = l.run(&[VOL, REP, &"0".repeat(64)], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(l.writes().is_empty());

    let (_, hash) = l.plan();
    assert!(l.run(&[VOL, REP, &hash], &[]).status.success());
    let before = l.writes().len();
    // Applied once, the same approval re-renders to a refusal.
    let o = l.run(&[VOL, REP, &hash], &[]);
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert_eq!(l.writes().len(), before, "a second run writes nothing");
}

#[test]
fn a_replica_that_changed_identity_after_the_plan_read_is_not_deleted() {
    // The write's own render reads the replicas once (read 1); the
    // recheck straight before the delete is read 2. A retained copy
    // replaced under its name in between stops the run, nothing deleted.
    let l = Longhorn::new("rebuild-swap");
    let (_, hash) = l.plan();
    std::fs::remove_file(l.dir.join("state/reads")).expect("reads counter");
    let o = l.run(
        &[VOL, REP, &hash],
        &[("STUB_SWAP_AT", "2"), ("STUB_SWAP_NAME", CP3_REP)],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("is NOT deleted"), "{out}");
    assert!(l.writes().is_empty(), "{:?}", l.writes());
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn an_engine_or_volume_that_moved_after_the_plan_read_is_not_deleted_from() {
    // Review run 9b9e2275 (R1, P1): the recheck straight before the
    // delete compared the replicas to the signed plan but not the ENGINE
    // the plan stood on, so a replacement engine, or the signed one
    // stopped with its last replicaModeMap still cached, passed the
    // recheck and the replica was deleted under a basis the plan never
    // signed. The write's own render is the first volume read; the
    // recheck is the second. Each drift lands at the second, and must
    // stop the run with nothing deleted and the moved fact named.
    let engine = format!("{VOL}-e-0");
    let cases: [(&str, &str, String, &str); 5] = [
        (
            "rebuild-drift-engine-uid",
            "engine.json",
            ".metadata.uid = \"replacement-engine-uid\"".to_string(),
            "replacement-engine-uid",
        ),
        (
            "rebuild-drift-engine-stopped",
            "engine.json",
            ".status.currentState = \"stopped\"".to_string(),
            "stopped",
        ),
        (
            "rebuild-drift-engine-name",
            "engine.json",
            format!(".metadata.name = \"{VOL}-e-1\""),
            "-e-1",
        ),
        (
            "rebuild-drift-detached",
            "vol.json",
            ".status.state = \"detached\"".to_string(),
            "detached",
        ),
        (
            "rebuild-drift-mode",
            "engine.json",
            format!(".status.replicaModeMap[\"{REP}\"] = \"ERR\""),
            "mode ERR",
        ),
    ];
    // Every case is run before any is judged, so a failure names each
    // drift the recheck let through, not only the first.
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(name, file, drift, named)| {
            let l = Longhorn::new(name);
            let (_, hash) = l.plan();
            std::fs::remove_file(l.dir.join("state/vreads")).expect("volume reads counter");
            let o = l.run(
                &[VOL, REP, &hash],
                &[
                    ("STUB_DRIFT_AT", "2"),
                    ("STUB_DRIFT_FILE", file),
                    ("STUB_DRIFT", drift),
                ],
            );
            let out = text(&o);
            let held = o.status.code() == Some(1)
                && out.contains("is NOT deleted")
                && out.contains(named)
                && l.writes().is_empty()
                && effect_lines(&out).is_empty();
            (!held).then(|| {
                format!(
                    "{name} (exit {:?}, writes {:?}, must name {named:?}):\n{out}",
                    o.status.code(),
                    l.writes()
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} drift(s) passed the recheck straight before the delete:\n{}",
        wrong.len(),
        wrong.join("\n---\n")
    );
    // The control: the same hook with nothing moved deletes, so each
    // stop above is the drift's, not the hook's.
    let l = Longhorn::new("rebuild-drift-control");
    let (_, hash) = l.plan();
    std::fs::remove_file(l.dir.join("state/vreads")).expect("volume reads counter");
    let o = l.run(
        &[VOL, REP, &hash],
        &[
            ("STUB_DRIFT_AT", "2"),
            ("STUB_DRIFT_FILE", "engine.json"),
            ("STUB_DRIFT", &format!(".metadata.name = \"{engine}\"")),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(l.writes().len(), 1, "{:?}", l.writes());
}

#[test]
fn a_stall_history_that_moved_after_the_plan_read_is_not_deleted() {
    // Review run ffd7de14 (R2, P1): the recheck straight before the
    // delete held identities, modes, health, rebuilding and the engine,
    // but not the named copy's HISTORY — the marks the plan signs and
    // the stall bound it says was met. A fresh lastFailedAt landing
    // between the write's render and that recheck (a rebuild that just
    // restarted) left every checked fact the same, and the copy was
    // deleted with exit 0, while the same mark present before the render
    // refuses 78 "under the stall bound". The pair, as the review ran it.
    let fresh = "2026-11-04T14:30:00Z";
    let at_render = Longhorn::new("rebuild-history-at-render");
    at_render.edit_replicas(|r| r[1]["spec"]["lastFailedAt"] = json!(fresh));
    assert!(
        at_render.refused_plan().contains("under the stall bound"),
        "the control: a fresh mark before the render is refused"
    );

    // Each signed mark, moved only at the second volume read (the
    // recheck): fresh ones break the stall bound, and an OLD one that
    // merely moved is still not the history the passkey signed.
    let rep_mark = |field: &str, value: &str| {
        format!("map(if .metadata.name == \"{REP}\" then .spec.{field} = \"{value}\" else . end)")
    };
    let cases: [(&str, &str, String, &[&str]); 5] = [
        (
            "rebuild-history-last-failed",
            "replicas.json",
            rep_mark("lastFailedAt", fresh),
            &["lastFailedAt", fresh, "stall bound"],
        ),
        (
            "rebuild-history-failed",
            "replicas.json",
            rep_mark("failedAt", fresh),
            &["failedAt", fresh, "stall bound"],
        ),
        (
            "rebuild-history-last-healthy",
            "replicas.json",
            rep_mark("lastHealthyAt", fresh),
            &["lastHealthyAt", fresh, "stall bound"],
        ),
        (
            "rebuild-history-transition",
            "engine.json",
            format!(".status.replicaTransitionTimeMap[\"{REP}\"] = \"{fresh}\""),
            &["engine transition", fresh, "stall bound"],
        ),
        (
            "rebuild-history-moved-old",
            "replicas.json",
            rep_mark("lastFailedAt", "2026-10-31T01:02:03Z"),
            &["lastFailedAt", "2026-10-31T01:02:03Z"],
        ),
    ];
    let wrong: Vec<String> = cases
        .iter()
        .filter_map(|(name, file, drift, named)| {
            let l = Longhorn::new(name);
            let (_, hash) = l.plan();
            std::fs::remove_file(l.dir.join("state/vreads")).expect("volume reads counter");
            let o = l.run(
                &[VOL, REP, &hash],
                &[
                    ("STUB_DRIFT_AT", "2"),
                    ("STUB_DRIFT_FILE", file),
                    ("STUB_DRIFT", drift),
                ],
            );
            let out = text(&o);
            let held = o.status.code() == Some(1)
                && out.contains("is NOT deleted")
                && named.iter().all(|n| out.contains(n))
                && l.writes().is_empty()
                && effect_lines(&out).is_empty();
            (!held).then(|| {
                format!(
                    "{name} (exit {:?}, writes {:?}, must name {named:?}):\n{out}",
                    o.status.code(),
                    l.writes()
                )
            })
        })
        .collect();
    assert!(
        wrong.is_empty(),
        "{} history drift(s) passed the recheck straight before the delete:\n{}",
        wrong.len(),
        wrong.join("\n---\n")
    );

    // The control: the same hook rewriting a mark to the value the plan
    // signed deletes, so each stop above is the history's, not the hook's.
    let l = Longhorn::new("rebuild-history-control");
    let (_, hash) = l.plan();
    std::fs::remove_file(l.dir.join("state/vreads")).expect("volume reads counter");
    let o = l.run(
        &[VOL, REP, &hash],
        &[
            ("STUB_DRIFT_AT", "2"),
            ("STUB_DRIFT_FILE", "replicas.json"),
            (
                "STUB_DRIFT",
                &rep_mark("lastFailedAt", "2026-10-30T20:34:45Z"),
            ),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(l.writes().len(), 1, "{:?}", l.writes());
}

#[test]
fn a_delete_the_server_refuses_is_a_failure_with_its_words() {
    let l = Longhorn::new("rebuild-delete-fail");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_DELETE_FAIL", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("deletion refused"), "{out}");
    assert!(effect_lines(&out).is_empty());
}

#[test]
fn a_rebuild_that_never_starts_or_never_finishes_is_not_the_effect() {
    let l = Longhorn::new("rebuild-no-replenish");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_NO_REPLENISH", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("did NOT start"), "{out}");
    assert!(effect_lines(&out).is_empty());

    let l = Longhorn::new("rebuild-stuck");
    let (_, hash) = l.plan();
    let o = l.run(&[VOL, REP, &hash], &[("STUB_REBUILD_STUCK", "1")]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("Longhorn is rebuilding"), "{out}");
    assert!(out.contains("is NOT proven"), "{out}");
    assert!(effect_lines(&out).is_empty(), "{out}");
}
