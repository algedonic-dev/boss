//! Native-shaped, credential-free reads through the existing forge door.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/longhorn-managers.sh";
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
while [ "$#" -gt 0 ]; do
 case "$1" in --kubeconfig=/kc) shift; break ;; esac
 shift
done
printf '%s\n' "$*" >> "$STUB_ARGV"
case "$*" in
'get namespace longhorn-system -o json --request-timeout=20s') f=namespace ;;
'get daemonset longhorn-manager -n longhorn-system -o json --request-timeout=20s') f=daemonset ;;
'get pods -n longhorn-system -l app=longhorn-manager -o json --request-timeout=20s --chunk-size=64') f=pods ;;
'get events -n longhorn-system -o json --request-timeout=20s --chunk-size=64') f=events ;;
*) echo "unexpected or mutating argv: $*" >&2; exit 91 ;;
esac
[ "${STUB_FAIL:-}" != "$f" ] || { echo 'Forbidden: retained native error' >&2; exit 1; }
cat "$STUB_STATE/$f.json"
"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let f = Self(scratch_dir("longhorn-managers"));
        std::fs::create_dir_all(f.0.join("bin")).unwrap();
        std::fs::create_dir_all(f.0.join("state")).unwrap();
        write_exec(&f.0.join("bin/sudo"), STUB);
        f.state("namespace", json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"longhorn-system","uid":"ns-one"}}));
        f.state("daemonset", json!({"apiVersion":"apps/v1","kind":"DaemonSet","metadata":{"name":"longhorn-manager","namespace":"longhorn-system","uid":"ds-one","generation":4},"spec":{"selector":{"matchLabels":{"app":"longhorn-manager"}}},"status":{"observedGeneration":4,"desiredNumberScheduled":3,"numberReady":2,"updatedNumberScheduled":3}}));
        f.state("pods", json!({"apiVersion":"v1","kind":"List","metadata":{},"items":[{"metadata":{"name":"longhorn-manager-one","namespace":"longhorn-system","uid":"pod-one","labels":{"app":"longhorn-manager"},"ownerReferences":[{"kind":"DaemonSet","name":"longhorn-manager","uid":"ds-one","controller":true}]},"spec":{"nodeName":"w-1"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"False","reason":"ContainersNotReady"}],"containerStatuses":[{"name":"longhorn-manager","ready":false,"restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}},"lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"finishedAt":"2026-10-31T18:30:00Z"}}}]}}]}));
        f.state("events", json!({"apiVersion":"v1","kind":"EventList","metadata":{},"items":[{"metadata":{"name":"event-one","namespace":"longhorn-system","uid":"event-one"},"involvedObject":{"kind":"Pod","name":"longhorn-manager-one","uid":"pod-one"},"reason":"BackOff","message":"Back-off restarting failed container","count":3,"lastTimestamp":"2026-10-31T18:31:00Z"}]}));
        f
    }
    fn state(&self, name: &str, value: Value) {
        write_file(
            &self.0.join("state").join(format!("{name}.json")),
            &value.to_string(),
        );
    }
    fn run(&self, args: &[&str], fail: &str) -> Output {
        Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.0.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.0.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.0.join("boss-ops"))
            .env("STUB_STATE", self.0.join("state"))
            .env("STUB_ARGV", self.0.join("argv"))
            .env("STUB_FAIL", fail)
            .output()
            .unwrap()
    }
}

#[test]
fn fixed_reader_reports_restart_and_retained_events_without_claiming_cause() {
    let f = Fixture::new();
    let o = f.run(&[], "");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["state"], "observed");
    assert_eq!(v["cause"], "UNKNOWN");
    assert_eq!(v["pods"][0]["uid"], "pod-one");
    assert_eq!(v["pods"][0]["containers"][0]["restartCount"], 7);
    assert_eq!(
        v["pods"][0]["containers"][0]["lastState"]["terminated"]["reason"],
        "OOMKilled"
    );
    assert_eq!(v["events"][0]["reason"], "BackOff");
    assert_eq!(v["daemonset"]["status"]["numberReady"], 2);
    let calls = std::fs::read_to_string(f.0.join("argv")).unwrap();
    assert_eq!(calls.lines().count(), 4);
    assert!(calls.lines().all(|line| line.starts_with("get ")));
}

#[test]
fn unread_partial_or_wrong_identity_never_becomes_a_healthy_empty_list() {
    for fail in ["namespace", "daemonset", "pods", "events"] {
        let f = Fixture::new();
        let o = f.run(&[], fail);
        assert!(!o.status.success());
        assert!(o.stdout.is_empty());
        assert!(String::from_utf8_lossy(&o.stderr).contains("Forbidden: retained native error"));
    }
    for state in [
        json!({"apiVersion":"v1","kind":"List","metadata":{"continue":"next"},"items":[]}),
        json!({"apiVersion":"v1","kind":"List","items":[{"metadata":{"namespace":"other"}}]}),
    ] {
        let f = Fixture::new();
        f.state("pods", state);
        let o = f.run(&[], "");
        assert!(!o.status.success());
        assert!(o.stdout.is_empty());
    }
}

#[test]
fn malformed_container_status_object_is_refused_instead_of_normalized() {
    let f = Fixture::new();
    let mut pods: Value =
        serde_json::from_str(&std::fs::read_to_string(f.0.join("state/pods.json")).unwrap())
            .unwrap();
    let container = pods["items"][0]["status"]["containerStatuses"][0].clone();
    pods["items"][0]["status"]["containerStatuses"] = json!({"entry": container});
    f.state("pods", pods);
    let o = f.run(&[], "");
    assert!(
        !o.status.success(),
        "malformed object was normalized as a native array"
    );
    assert!(o.stdout.is_empty());
}

#[test]
fn packet_cannot_select_a_namespace_or_command() {
    let f = Fixture::new();
    let o = f.run(&["other"], "");
    assert_eq!(o.status.code(), Some(78));
    assert!(!f.0.join("argv").exists());
}

#[test]
fn registered_reader_has_no_packet_selected_authority() {
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/ops/verbs/read-longhorn-managers.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["argv"], json!([SCRIPT]));
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(v["params"], json!([]));
    assert_eq!(v["timeout"], 240);
    assert!(v["about"].as_str().unwrap().starts_with("READ-ONLY:"));
}
