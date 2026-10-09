//! `infra/estate/observe-volumes.sh` is RUN, not read — against a stub
//! `sudo` that plays `docker run` of kubectl through the admin
//! kubeconfig, and a stub `curl` that plays the estate door — so every
//! reading below is one the observer actually reached.
//!
//! WHY IT EXISTS (backlog 21ee3b4e, incident d3c0a67c, 2026-10-01). The
//! system of record's Postgres volume filled and writes failed with No
//! space left on device, and nothing was watching any claim. The
//! observer reads every claim in every namespace instances.toml
//! declares, joins it to the kubelet's volume stats, and records the
//! figures — or, for a claim it could not read, `unread` with the
//! reason, never a quiet absence and never a guess.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/estate/observe-volumes.sh";

const GIB: i64 = 1 << 30;

/// The `boss` namespace's claims, as `kubectl get pvc -o json` answers:
/// the database (mounted on w-1), boss-auth (w-1), boss-files (mounted
/// on w-2, whose kubelet will not answer), boss-backups (Pending), and
/// jsdata-nats-0 (Bound, but no running pod mounts it).
const PVC_BOSS: &str = r#"{"items":[
 {"metadata":{"name":"pgdata-postgres-0"},"spec":{"volumeName":"pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56","storageClassName":"longhorn","resources":{"requests":{"storage":"30Gi"}}},"status":{"phase":"Bound"}},
 {"metadata":{"name":"boss-auth"},"spec":{"volumeName":"pvc-auth","storageClassName":"longhorn","resources":{"requests":{"storage":"1Gi"}}},"status":{"phase":"Bound"}},
 {"metadata":{"name":"boss-files"},"spec":{"volumeName":"pvc-files","storageClassName":"longhorn","resources":{"requests":{"storage":"20Gi"}}},"status":{"phase":"Bound"}},
 {"metadata":{"name":"boss-backups"},"spec":{"storageClassName":"longhorn","resources":{"requests":{"storage":"20Gi"}}},"status":{"phase":"Pending"}},
 {"metadata":{"name":"jsdata-nats-0"},"spec":{"volumeName":"pvc-nats","storageClassName":"longhorn","resources":{"requests":{"storage":"10Gi"}}},"status":{"phase":"Bound"}}
]}"#;

const NODES: &str = r#"{"items":[
 {"metadata":{"name":"w-1"}},
 {"metadata":{"name":"w-2"}},
 {"metadata":{"name":"cp-1"}}
]}"#;

fn w1_summary() -> String {
    let cap = 30 * GIB;
    let free = 5 * GIB + GIB / 4;
    serde_json::json!({
        "node": {"nodeName": "w-1"},
        "pods": [
            {"podRef": {"name": "postgres-0", "namespace": "boss"},
             "volume": [
                 {"name": "kube-api-access", "capacityBytes": 1, "availableBytes": 1, "usedBytes": 0},
                 {"name": "pgdata", "pvcRef": {"name": "pgdata-postgres-0", "namespace": "boss"},
                  "capacityBytes": cap, "usedBytes": cap - free, "availableBytes": free}
             ]},
            {"podRef": {"name": "boss-abc", "namespace": "boss"},
             "volume": [
                 {"name": "auth", "pvcRef": {"name": "boss-auth", "namespace": "boss"},
                  "capacityBytes": GIB, "usedBytes": GIB / 10, "availableBytes": GIB - GIB / 10}
             ]},
            // Another namespace's claim of the same name is not boss's.
            {"podRef": {"name": "other", "namespace": "boss-dev"},
             "volume": [
                 {"name": "x", "pvcRef": {"name": "boss-files", "namespace": "boss-dev"},
                  "capacityBytes": GIB, "usedBytes": 0, "availableBytes": GIB}
             ]}
        ]
    })
    .to_string()
}

const CP1_SUMMARY: &str =
    r#"{"node":{"nodeName":"cp-1"},"pods":[{"podRef":{"name":"etcd","namespace":"kube-system"}}]}"#;

struct World {
    dir: PathBuf,
    bin: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let dir = scratch_dir(tag);
        let bin = dir.join("bin");
        create_dir(&bin);
        write_file(&dir.join("pvc-boss.json"), PVC_BOSS);
        write_file(&dir.join("nodes.json"), NODES);
        write_file(&dir.join("summary-w-1.json"), &w1_summary());
        write_file(&dir.join("summary-cp-1.json"), CP1_SUMMARY);
        // sudo: plays `docker run <image> kubectl --kubeconfig=/kc …`,
        // logging every call. boss-playground's claims cannot be listed;
        // w-2's kubelet does not answer.
        write_exec(
            &bin.join("sudo"),
            "#!/usr/bin/env bash\n\
             printf '%s\\n' \"$*\" >> \"$STUB_DIR/sudo.calls\"\n\
             case \" $* \" in\n\
             \x20 *' get pvc -n '*)\n\
             \x20   ns=''; prev=''; for a in \"$@\"; do [ \"$prev\" = -n ] && [ \"$a\" != docker ] && ns=\"$a\"; prev=\"$a\"; done\n\
             \x20   if [ -f \"$STUB_DIR/pvc-$ns.json\" ]; then cat \"$STUB_DIR/pvc-$ns.json\"; exit 0; fi\n\
             \x20   echo \"Error from server (Forbidden): persistentvolumeclaims is forbidden in $ns\" >&2; exit 1 ;;\n\
             \x20 *' get nodes '*) cat \"$STUB_DIR/nodes.json\"; exit 0 ;;\n\
             \x20 *' get --raw /api/v1/nodes/'*)\n\
             \x20   for a in \"$@\"; do case \"$a\" in /api/v1/nodes/*) n=${a#/api/v1/nodes/}; n=${n%%/*} ;; esac; done\n\
             \x20   if [ -f \"$STUB_DIR/summary-$n.json\" ]; then cat \"$STUB_DIR/summary-$n.json\"; exit 0; fi\n\
             \x20   echo 'Error from server: error trying to reach service: dial tcp 10.20.0.22:10250: connect: connection refused' >&2; exit 1 ;;\n\
             esac\n\
             echo \"sudo stub: unexpected: $*\" >&2; exit 99\n",
        );
        World { dir, bin }
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env("STUB_DIR", &self.dir)
            .env("BOSS_OPS_DIR", self.dir.join("ops"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OBSERVE_WORK", self.dir.join("work"))
            .env_remove("JOBS_API")
            .env_remove("BOSS_INSTANCES");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run observe-volumes.sh")
    }

    fn observe(&self) -> (Value, String) {
        let out = self.run(&["--print"], &[]);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "observe-volumes.sh --print failed: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json = stdout
            .lines()
            .find(|l| l.starts_with('{'))
            .unwrap_or_else(|| panic!("no observation printed: {stdout}"));
        (
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}")),
            stdout,
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("sudo.calls")).unwrap_or_default()
    }
}

fn row<'a>(obs: &'a Value, id: &str) -> &'a Value {
    obs["nodes"]
        .as_array()
        .and_then(|a| a.iter().find(|n| n["id"] == id))
        .unwrap_or_else(|| panic!("no volume {id} in {obs}"))
}

/// The instance namespaces as instances.toml declares them — read here
/// the way a person reads the file, so the observer's list is held to it.
fn declared_namespaces() -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join("infra/cluster/instances.toml"))
        .expect("instances.toml");
    let mut ns: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("namespace = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .map(str::to_string)
        .collect();
    ns.sort();
    ns
}

#[test]
fn every_claim_in_every_instance_namespace_is_on_the_reading() {
    let w = World::new("volumes-read");
    let (obs, stdout) = w.observe();
    assert_eq!(obs["scope"], "instance-volumes");
    let namespaces: Vec<String> = obs["namespaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(namespaces, declared_namespaces(), "{obs}");
    assert!(namespaces.contains(&"boss".to_string()), "{obs}");

    let pg = row(&obs, "boss/pgdata-postgres-0");
    assert_eq!(pg["claim"], "pgdata-postgres-0", "{pg}");
    assert_eq!(
        pg["volume"], "pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56",
        "{pg}"
    );
    assert_eq!(pg["capacity_bytes"], 30 * GIB, "{pg}");
    assert_eq!(pg["free_bytes"], 5 * GIB + GIB / 4, "{pg}");
    assert_eq!(pg["used_bytes"], 25 * GIB - GIB / 4, "{pg}");
    assert_eq!(pg["node"], "w-1", "{pg}");
    assert!(pg.get("unread").is_none(), "{pg}");
    assert!(
        stdout.contains(
            "boss/pgdata-postgres-0 (pvc-93e11a6e-6999-41a8-9df3-622f36b7ff56): 30G, free 5.3G"
        ),
        "{stdout}"
    );
    assert_eq!(row(&obs, "boss/boss-auth")["capacity_bytes"], GIB, "{obs}");
}

#[test]
fn a_claim_that_could_not_be_read_is_recorded_unread_with_its_reason() {
    let w = World::new("volumes-unread");
    let (obs, stdout) = w.observe();
    for (id, why) in [
        // Mounted on w-2, whose kubelet did not answer: named.
        ("boss/boss-files", "w-2: Error from server"),
        // A claim of the same name in another namespace is not this one.
        ("boss/boss-files", "no kubelet reports"),
        ("boss/boss-backups", "Pending, not Bound"),
        ("boss/jsdata-nats-0", "no kubelet reports"),
        // A namespace whose claims could not be listed is a row of its own.
        ("boss-playground/*", "could not be listed"),
        ("boss-playground/*", "Forbidden"),
    ] {
        let r = row(&obs, id);
        assert!(
            r["free_bytes"].is_null() && r["capacity_bytes"].is_null(),
            "{id} has figures it could not have read: {r}"
        );
        assert!(
            r["unread"].as_str().is_some_and(|u| u.contains(why)),
            "{id}'s reason must say `{why}`: {r}"
        );
        assert!(
            stdout.contains(&format!("no volume reading for {id}")),
            "{id} not named on stdout — the forge's journal is readable with the record dark: {stdout}"
        );
    }
}

#[test]
fn every_call_is_a_bounded_read_through_the_admin_kubeconfig() {
    let w = World::new("volumes-calls");
    w.observe();
    let calls = w.calls();
    assert!(!calls.is_empty(), "no call reached the stub");
    for l in calls.lines() {
        assert!(
            l.starts_with("-n docker run --rm --network host --mount type=bind,src="),
            "{l}"
        );
        assert!(
            l.contains(" reg.test/david/alpine-k8s:1.33.3 kubectl --kubeconfig=/kc get "),
            "{l}"
        );
        assert!(l.ends_with(" --request-timeout=20s"), "bounded: {l}");
        for write in [
            " patch ",
            " apply ",
            " delete ",
            " edit ",
            " scale ",
            " create ",
            " label ",
            " annotate ",
        ] {
            assert!(!l.contains(write), "the reading is read-only: {l}");
        }
    }
    let summaries = calls
        .lines()
        .filter(|l| l.contains(" get --raw /api/v1/nodes/") && l.contains("/proxy/stats/summary"))
        .count();
    assert_eq!(summaries, 3, "one summary per node: {calls}");
}

#[test]
fn the_reading_is_posted_to_the_estate_door() {
    let w = World::new("volumes-post");
    let posted = w.dir.join("posted.json");
    write_exec(
        &w.bin.join("curl"),
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n%s' '{{\"recorded\":true}}' \"${{STUB_STATUS:-202}}\"\n",
            posted.display()
        ),
    );
    let out = w.run(&[], &[("JOBS_API", "http://stub")]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(&posted).expect("posted")).expect("json");
    assert_eq!(body["scope"], "instance-volumes");
    assert_eq!(
        row(&body, "boss/pgdata-postgres-0")["free_bytes"],
        5 * GIB + GIB / 4
    );

    let out = w.run(&[], &[("JOBS_API", "http://stub"), ("STUB_STATUS", "503")]);
    assert!(
        !out.status.success(),
        "a reading not recorded is a failed run"
    );
}

#[test]
fn no_declared_namespace_posts_nothing_and_says_so() {
    let w = World::new("volumes-nons");
    let empty = w.dir.join("instances.toml");
    write_file(&empty, "source = \"prod\"\n");
    let out = w.run(
        &["--print"],
        &[("BOSS_INSTANCES", empty.to_str().expect("utf-8 path"))],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("declares no instance namespace"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_forge_reads_the_volumes_on_its_fifteen_minute_cadence_best_effort() {
    let unit = std::fs::read_to_string(repo_root().join("infra/forge/estate-observe-host.service"))
        .expect("the forge's host observer unit");
    assert!(
        unit.lines()
            .any(|l| l == "ExecStart=-/var/lib/boss/tree/current/infra/estate/observe-volumes.sh"),
        "the forge's host observer runs the volume reading, best-effort: {unit}"
    );
    let timer = std::fs::read_to_string(repo_root().join("infra/forge/estate-observe-host.timer"))
        .expect("the forge's host observer timer");
    assert!(timer.contains("OnUnitActiveSec=15min"), "{timer}");
}

#[test]
fn an_idle_claim_uses_dated_native_figures_without_retimestamping_the_sample() {
    let w = World::new("volumes-native-idle");
    let date = |offset: &str| -> String {
        let out = Command::new("date")
            .args(["-u", "-d", offset, "+%Y-%m-%dT%H:%M:%SZ"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let at = date("-600 seconds");
    let mut pvc: Value = serde_json::from_str(PVC_BOSS).unwrap();
    let volume = "pvc-98ccf090-32f2-415a-ac79-029cfe41456e";
    pvc["items"][3]["status"]["phase"] = serde_json::json!("Bound");
    pvc["items"][3]["spec"]["volumeName"] = serde_json::json!(volume);
    write_file(&w.dir.join("pvc-boss.json"), &pvc.to_string());
    write_file(&w.dir.join("attempts.json"),&serde_json::json!({"items":[{"metadata":{"uid":"kube-job","namespace":"boss","creationTimestamp":date("-1200 seconds"),"ownerReferences":[{"kind":"CronJob","name":"boss-pg-backup","controller":true}]},"status":{"succeeded":1,"completionTime":date("-400 seconds"),"conditions":[{"type":"Complete","status":"True"}]}}]}).to_string());
    write_file(&w.dir.join("native.json"),&serde_json::json!({"total":1,"data":[{"id":"native-job","kind":"maintenance-backup","partition":"real","simulated":false,"opened_at":date("-1200 seconds"),"steps":[{"id":"native-step","job_id":"native-job","spec_slug":"run","status":"completed","assignee_id":"automation:boss-step","completed_by":"automation:boss-step","completed_at":date("-500 seconds"),"metadata":{"result":"ok","filesystem_sample":{"schema":1,"namespace":"boss","claim":"boss-backups","volume":volume,"mount_source":format!("/dev/longhorn/{volume}"),"mount":"/backup","position":"final","observed_at":at,"kubernetes_job_uid":"kube-job","pod_uid":"pod-gone","pod":"pod-gone","node":"cp-1","capacity_bytes":20*GIB,"used_bytes":18*GIB,"free_bytes":GIB}}}]}]}).to_string());
    let prior = std::fs::read_to_string(w.bin.join("sudo")).unwrap();
    write_exec(&w.bin.join("sudo"),&prior.replace("case \" $* \" in","case \" $* \" in\n  *' get jobs -n boss '*) cat \"$STUB_DIR/attempts.json\"; exit 0 ;;"));
    write_exec(
        &w.bin.join("curl"),
        "#!/bin/sh\ncat \"$STUB_DIR/native.json\"\n",
    );
    let out = w.run(&["--print"], &[("JOBS_API", "http://fixture")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let obs: Value =
        serde_json::from_str(stdout.lines().find(|s| s.starts_with('{')).unwrap()).unwrap();
    let backup = row(&obs, "boss/boss-backups");
    assert_eq!(
        backup["free_bytes"], GIB,
        "low native figures must reach the only floor judge: {backup}"
    );
    assert_eq!(
        backup["sample"]["observed_at"], at,
        "the idle observation is not another mounted sample"
    );
    assert_eq!(backup["native_job_id"], "native-job");
    assert_eq!(
        row(&obs, "boss/pgdata-postgres-0")["free_bytes"],
        5 * GIB + GIB / 4,
        "live kubelet controls remain unchanged"
    );
}
