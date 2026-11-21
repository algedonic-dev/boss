//! `read-service-address`, the READ-ONLY forge verb that answers which
//! address the cluster assigned a LoadBalancer Service (backlog 7f05ef2f).
//!
//! WHY. Item 47601829 (the standby dev pod's ssh door) must pin the
//! address the pool gave `boss-dev/boss-dev-standby-ssh`, and nothing could
//! read it: the dev session's credential cannot `get services` in boss-dev,
//! no verb under infra/ops/verbs read a Service, and the estate API holds
//! no load-balancer address. The manifest's own comment told its reader to
//! run a kubectl no session holding it could run. So the read is a verb,
//! through the forge's ops_kubectl door, bounded to a Service the tree
//! declares, and an address it could not read is CANNOT ANSWER — never an
//! empty success.
//!
//! Every test here runs the real script against a stub cluster behind
//! ops_kubectl and a fixture tree read by the one derivation of what the
//! tree declares (infra/cluster/undeclared-objects.sh --declared). Nothing
//! here reaches a cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/read-service-address.sh";

/// The stand-in behind ops_kubectl's `sudo -n docker run … kubectl
/// --kubeconfig=/kc`. Every kubectl call is appended to $STUB_ARGV.
/// State under $STUB_STATE: `svc-<ns>-<name>.json` the Service the
/// cluster holds (absent: NotFound), `forbid` makes the read refused,
/// `hang` makes it never answer.
const CLUSTER: &str = r#"#!/bin/sh
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
case "$1 $2" in
'get svc')
    [ -f "$S/hang" ] && exec sleep 30
    if [ -f "$S/forbid" ]; then
        echo "Error from server (Forbidden): services \"$3\" is forbidden: User \"x\" cannot get resource \"services\"" >&2
        exit 1
    fi
    f="$S/svc-$5-$3.json"
    [ -f "$f" ] || { echo "Error from server (NotFound): services \"$3\" not found" >&2; exit 1; }
    cat "$f"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

/// The kubectl undeclared-objects.sh parses manifests with
/// (`BOSS_KUBECTL`). The fixture's manifests are JSON, which is YAML, and
/// `kubectl create --dry-run=client -o json -f` echoes them, so `create`
/// is a cat. Anything else is not a parse and fails loudly.
/// `STUB_BADPARSE` names a manifest that will not parse.
const PARSER: &str = r#"#!/bin/sh
[ "$1" = create ] || { echo "parser stub: not a parse: $*" >&2; exit 2; }
f=""
while [ $# -gt 0 ]; do [ "$1" = "-f" ] && { f="$2"; break; }; shift; done
[ -n "$f" ] || { echo "parser stub: no -f" >&2; exit 1; }
if [ -n "${STUB_BADPARSE:-}" ] && [ "$(basename "$f")" = "$STUB_BADPARSE" ]; then
    echo "error: error parsing $f: broken fixture" >&2
    exit 1
fi
cat "$f"
"#;

fn needs_tools() {
    for tool in ["jq", "python3", "timeout"] {
        let ok = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(
            ok,
            "read_service_address_sh: no {tool} on this box — the gate image has it, and a \
             test that cannot run must fail, never pass by returning early"
        );
    }
}

fn object(kind: &str, ns: &str, name: &str) -> Value {
    json!({"apiVersion": "v1", "kind": kind, "metadata": {"name": name, "namespace": ns}})
}

/// A live Service as `kubectl get svc -o json` prints it.
fn service(ns: &str, name: &str, annotations: Value, spec: Value, status: Value) -> String {
    let mut s = json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": name, "namespace": ns, "annotations": annotations},
        "spec": {"type": "LoadBalancer", "clusterIP": "10.96.4.20",
                 "ports": [{"name": "ssh", "port": 22, "protocol": "TCP", "targetPort": 22}]},
        "status": status
    });
    if let (Some(base), Some(extra)) = (s["spec"].as_object_mut(), spec.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }
    s.to_string()
}

struct Cluster {
    dir: PathBuf,
}

impl Cluster {
    /// A tree whose manifests declare `boss-dev/boss-dev-standby-ssh`
    /// and twelve Services in `boss` (the derivation's "the scrape broke"
    /// floors — ten files, twenty objects — are cleared by the fixture,
    /// not relaxed for it), plus one Service declared OUTSIDE
    /// infra/cluster/manifests, in the gate's template directory.
    fn new(name: &str) -> Self {
        needs_tools();
        let dir = scratch_dir(&format!("read-service-address-{name}"));
        let manifests = dir.join("tree/infra/cluster/manifests");
        std::fs::create_dir_all(&manifests).expect("manifests");
        std::fs::create_dir_all(dir.join("tree/infra/gate-runner")).expect("gate-runner");
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        std::fs::create_dir_all(dir.join("state")).expect("state");
        write_file(
            &manifests.join("namespaces.yaml"),
            &format!(
                "{}{}",
                json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "boss"}}),
                json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "boss-dev"}})
            ),
        );
        write_file(
            &manifests.join("boss-dev-standby.yaml"),
            &format!(
                "{}{}",
                object("Deployment", "boss-dev", "boss-dev-standby"),
                object("Service", "boss-dev", "boss-dev-standby-ssh")
            ),
        );
        for n in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"] {
            write_file(
                &manifests.join(format!("svc-{n}.yaml")),
                &format!(
                    "{}{}",
                    object("Service", "boss", &format!("svc-{n}")),
                    object("ConfigMap", "boss", &format!("svc-{n}"))
                ),
            );
        }
        write_file(
            &dir.join("tree/infra/gate-runner/gate-runner.yaml"),
            &object("Service", "boss-dev", "gate-only").to_string(),
        );
        write_exec(&dir.join("bin/sudo"), CLUSTER);
        write_exec(&dir.join("parser"), PARSER);
        Cluster { dir }
    }

    fn state(&self, file: &str, body: &str) {
        write_file(&self.dir.join("state").join(file), body);
    }

    fn holds(&self, ns: &str, name: &str, body: String) {
        self.state(&format!("svc-{ns}-{name}.json"), &body);
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
            .env_remove("KUBECONFIG")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_CLUSTER_TREE", self.dir.join("tree"))
            .env("BOSS_KUBECTL", self.dir.join("parser"))
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
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn stdout_lines(o: &Output) -> Vec<String> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The one read the verb makes, and nothing else: a get of the named
/// Service, bounded on the server side.
fn only_the_get(c: &Cluster) {
    assert_eq!(
        c.calls(),
        vec!["get svc boss-dev-standby-ssh -n boss-dev -o json --request-timeout=20s".to_string()],
        "READ-ONLY, one get"
    );
}

fn verb_file(dir: &Path) -> Value {
    let p = dir.join("infra/ops/verbs/read-service-address.json");
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn the_verb_is_a_read_only_forge_door_with_a_bounded_namespace_and_service() {
    let v = verb_file(&repo_root());
    assert_eq!(v["hosts"], json!(["forge"]));
    assert_eq!(v["argv"], json!([SCRIPT, "{1}", "{2}"]));
    let about = v["about"].as_str().expect("about");
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
    let names: Vec<&str> = v["params"]
        .as_array()
        .expect("params")
        .iter()
        .map(|p| p["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, vec!["namespace", "service"]);
    assert!(
        std::fs::metadata(repo_root().join(SCRIPT))
            .map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0)
            .unwrap_or(false),
        "{SCRIPT} must be executable — the runner execs it"
    );
}

#[test]
fn an_assigned_address_is_printed_with_its_ports_pins_and_allocator() {
    let c = Cluster::new("metallb");
    c.holds(
        "boss-dev",
        "boss-dev-standby-ssh",
        service(
            "boss-dev",
            "boss-dev-standby-ssh",
            json!({
                "metallb.io/ip-allocated-from-pool": "lan",
                // Neither of these is a field the verb names, so neither
                // may reach its output: it prints what it reads, by name.
                "example.test/token": "s3cr3t-never-printed",
                "kubectl.kubernetes.io/last-applied-configuration": "{\"also\":\"never\"}"
            }),
            json!({}),
            json!({"loadBalancer": {"ingress": [{"ip": "10.20.0.36", "ipMode": "VIP"}]}}),
        ),
    );
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let lines: Vec<String> = stdout_lines(&o)
        .into_iter()
        .filter(|l| !l.starts_with("read-service-address:"))
        .collect();
    assert_eq!(
        lines,
        vec![
            "service boss-dev/boss-dev-standby-ssh type LoadBalancer class none",
            "ingress 10.20.0.36",
            "port ssh 22/TCP",
            "pin io.cilium/lb-ipam-ips=none",
            "pin metallb.universe.tf/loadBalancerIPs=none",
            "pin spec.loadBalancerIP=none",
            "allocator metallb pool lan (metallb.io/ip-allocated-from-pool)",
        ],
        "{out}"
    );
    assert!(
        out.contains("declared in infra/cluster/manifests/boss-dev-standby.yaml"),
        "{out}"
    );
    assert!(
        out.contains("read-service-address: boss-dev/boss-dev-standby-ssh — address 10.20.0.36"),
        "{out}"
    );
    assert!(!out.contains("s3cr3t"), "{out}");
    assert!(!out.contains("never"), "{out}");
    only_the_get(&c);
}

#[test]
fn a_cilium_lb_ipam_allocation_and_its_pins_are_named() {
    let c = Cluster::new("cilium");
    c.holds(
        "boss-dev",
        "boss-dev-standby-ssh",
        service(
            "boss-dev",
            "boss-dev-standby-ssh",
            json!({"io.cilium/lb-ipam-ips": "10.20.0.36"}),
            json!({"loadBalancerClass": "io.cilium/l2-announcer", "loadBalancerIP": "10.20.0.36"}),
            json!({
                "loadBalancer": {"ingress": [{"ip": "10.20.0.36"}]},
                "conditions": [{"type": "cilium.io/IPAMRequestSatisfied", "status": "True",
                                "reason": "satisfied", "message": ""}]
            }),
        ),
    );
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert!(o.status.success(), "{out}");
    let lines = stdout_lines(&o);
    for want in [
        "service boss-dev/boss-dev-standby-ssh type LoadBalancer class io.cilium/l2-announcer",
        "ingress 10.20.0.36",
        "pin io.cilium/lb-ipam-ips=10.20.0.36",
        "pin metallb.universe.tf/loadBalancerIPs=none",
        "pin spec.loadBalancerIP=10.20.0.36",
        "allocator cilium-lb-ipam cilium.io/IPAMRequestSatisfied=True satisfied",
    ] {
        assert!(lines.iter().any(|l| l == want), "{want}: {out}");
    }
    only_the_get(&c);
}

#[test]
fn a_service_the_tree_does_not_declare_is_refused_before_any_cluster_read() {
    let c = Cluster::new("undeclared");
    for (args, want) in [
        // Declared nowhere.
        (
            vec!["boss-dev", "boss-dev-ssh-extra"],
            "no manifest under infra/cluster/manifests declares Service boss-dev/boss-dev-ssh-extra",
        ),
        // Declared, but in another namespace.
        (
            vec!["boss-dev", "svc-a"],
            "no manifest under infra/cluster/manifests declares Service boss-dev/svc-a",
        ),
        // Declared by the tree, but not under infra/cluster/manifests.
        (
            vec!["boss-dev", "gate-only"],
            "no manifest under infra/cluster/manifests declares Service boss-dev/gate-only",
        ),
        // A name of the right shape in the wrong kind's place.
        (
            vec!["boss-dev", "boss-dev-standby"],
            "no manifest under infra/cluster/manifests declares Service boss-dev/boss-dev-standby",
        ),
        (vec!["Boss-dev", "x"], "one lowercase DNS label"),
        (vec!["boss-dev", "-x"], "one lowercase DNS label"),
        (vec!["boss-dev"], "usage"),
        (vec!["boss-dev", "a", "b"], "usage"),
    ] {
        let o = c.run(&args, &[]);
        assert_eq!(o.status.code(), Some(78), "{args:?}: {}", text(&o));
        assert!(text(&o).contains(want), "{args:?}: {}", text(&o));
        assert!(
            !text(&o).contains("CANNOT ANSWER"),
            "{args:?}: {}",
            text(&o)
        );
    }
    assert!(
        c.calls().is_empty(),
        "a refusal reads nothing: {:?}",
        c.calls()
    );
}

#[test]
fn a_declared_set_that_cannot_be_derived_is_cannot_answer_not_a_refusal() {
    let c = Cluster::new("noderive");
    let o = c.run(
        &["boss-dev", "boss-dev-standby-ssh"],
        &[("STUB_BADPARSE", "svc-c.yaml")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(c.calls().is_empty(), "{:?}", c.calls());
}

#[test]
fn no_assigned_address_is_cannot_answer_never_an_empty_success() {
    let c = Cluster::new("pending");
    c.holds(
        "boss-dev",
        "boss-dev-standby-ssh",
        service(
            "boss-dev",
            "boss-dev-standby-ssh",
            json!({}),
            json!({}),
            json!({"loadBalancer": {}}),
        ),
    );
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(
            "CANNOT ANSWER — Service boss-dev/boss-dev-standby-ssh has no assigned address"
        ),
        "{out}"
    );
    // What it did read is still printed: a pending allocation is diagnosed
    // by the pins and the allocator line beside it.
    assert!(
        out.contains("service boss-dev/boss-dev-standby-ssh type LoadBalancer class none"),
        "{out}"
    );
    assert!(
        out.contains("allocator none named — the Service carries neither"),
        "{out}"
    );
    assert!(!out.lines().any(|l| l.starts_with("ingress ")), "{out}");
    only_the_get(&c);
}

#[test]
fn a_read_that_failed_or_hung_or_found_nothing_is_cannot_answer() {
    // The cluster does not hold it.
    let c = Cluster::new("absent");
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("not found"), "{out}");

    // The read was refused.
    let c = Cluster::new("forbidden");
    c.state("forbid", "");
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("Forbidden"), "{out}");

    // The read never answered: the bound ends it, and says so.
    let c = Cluster::new("hang");
    c.state("hang", "");
    let o = c.run(
        &["boss-dev", "boss-dev-standby-ssh"],
        &[("BOSS_READ_SERVICE_TIMEOUT_S", "1")],
    );
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(out.contains("timed out after 1s"), "{out}");

    // An answer about another object is not an answer about this one.
    let c = Cluster::new("other");
    c.holds(
        "boss-dev",
        "boss-dev-standby-ssh",
        service(
            "boss",
            "svc-a",
            json!({}),
            json!({}),
            json!({"loadBalancer": {"ingress": [{"ip": "10.20.0.30"}]}}),
        ),
    );
    let o = c.run(&["boss-dev", "boss-dev-standby-ssh"], &[]);
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(out.contains("CANNOT ANSWER"), "{out}");
    assert!(!out.contains("ingress 10.20.0.30"), "{out}");
}
