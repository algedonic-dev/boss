//! THE STANDBY SHELL STAYS SMALL AND STAYS OFF THE DEV POD'S NODE
//! (design 0491591b, backlog 47601829; both questions decided by David
//! 2026-11-07).
//!
//! On 2026-11-07 the dev pod sat Pending for about 80 minutes because w-2
//! could not hold its requests, and it was the only interactive shell
//! inside the LAN. `infra/cluster/manifests/boss-dev-standby.yaml` is the
//! second one. It is only useful while three properties hold, and each
//! is easy to lose in a later edit:
//!
//!   - It is on a different node from the dev pod. A required pod
//!     anti-affinity against the dev pod's label, per hostname, and off
//!     the control planes as every workload here is.
//!   - It is never on the node the dev pod PREFERS. The scheduler holds a
//!     required anti-affinity in both directions, so a standby placed on
//!     the build node while the dev pod was elsewhere would bar the dev
//!     pod from it until something moved the standby, and nothing does
//!     (review run 75a760c9, finding 1, 2026-11-15). So the standby is
//!     REQUIRED off every node whose purpose the dev pod prefers, and
//!     that purpose is read from boss-dev.yaml, not typed here.
//!   - It stays small (requests under 1 CPU and 1Gi), because the day it
//!     is needed is a day the node it is on has little room.
//!   - It is not a second dev pod: no /work claim, and not the dev pod's
//!     label, which would join it to the dev pod's Deployment and ssh
//!     Service. Nothing in its spec names the forge WRITE token, by any
//!     of the three routes a pod spec can take a Secret (Q1: it reads and
//!     files, it never pushes). That is a statement about this pod's
//!     spec, not an authority: the dev-session Role it runs as can create
//!     a Job that mounts the token (review 75a760c9, finding 2).
//!   - A standby whose door never opened, or whose sshd died, is
//!     restarted by the kubelet: a liveness probe on the ssh port.
//!
//! Read, not run: the manifest is text, and what the scheduler did with it
//! is what the standby's own banner prints.

use boss_testing::repo_root;
use serde::Deserialize;
use serde_json::Value;

const STANDBY: &str = "infra/cluster/manifests/boss-dev-standby.yaml";
const DEV: &str = "infra/cluster/manifests/boss-dev.yaml";
/// The node label a node's role in the estate is read from
/// (infra/estate/estate.toml: build, utility, database).
const PURPOSE: &str = "boss.dev/purpose";
/// The Secret holding the forge WRITE token, which the dev pod mounts.
const WRITE_TOKEN: &str = "boss-dev-forge-token";

/// The purposes a Deployment PREFERS its pod's node to have.
fn preferred_purposes(deploy: &Value) -> Vec<String> {
    deploy["spec"]["template"]["spec"]["affinity"]["nodeAffinity"]
        ["preferredDuringSchedulingIgnoredDuringExecution"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| {
            p["preference"]["matchExpressions"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .filter(|e| e["key"] == PURPOSE && e["operator"] == "In")
        .flat_map(|e| e["values"].as_array().into_iter().flatten())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// Every Secret a pod spec names, by each route a spec can take one: a
/// `secret` volume, a `projected` volume's secret source, a container's
/// env `secretKeyRef`, and a container's `envFrom` `secretRef` (review
/// 75a760c9, finding 6: this read only the first). Each is (route, name).
fn secrets_named(pod: &Value, containers: &[&Value]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |route: String, name: &Value| {
        if let Some(n) = name.as_str() {
            out.push((route, n.to_string()));
        }
    };
    for v in pod["volumes"].as_array().into_iter().flatten() {
        let vol = v["name"].as_str().unwrap_or("?");
        push(format!("volume `{vol}`"), &v["secret"]["secretName"]);
        for s in v["projected"]["sources"].as_array().into_iter().flatten() {
            push(format!("projected volume `{vol}`"), &s["secret"]["name"]);
        }
    }
    for c in containers {
        let cname = c["name"].as_str().unwrap_or("?");
        for e in c["env"].as_array().into_iter().flatten() {
            let var = e["name"].as_str().unwrap_or("?");
            push(
                format!("container `{cname}` env `{var}`"),
                &e["valueFrom"]["secretKeyRef"]["name"],
            );
        }
        for e in c["envFrom"].as_array().into_iter().flatten() {
            push(
                format!("container `{cname}` envFrom"),
                &e["secretRef"]["name"],
            );
        }
    }
    out
}

fn documents_of(text: &str) -> Vec<Value> {
    yaml_serde::Deserializer::from_str(text)
        .map(|document| Value::deserialize(document).unwrap())
        .filter(|d| !d.is_null())
        .collect()
}

fn file(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn object<'a>(docs: &'a [Value], kind: &str, name: &str) -> &'a Value {
    docs.iter()
        .find(|d| d["kind"] == kind && d["metadata"]["name"] == name)
        .unwrap_or_else(|| panic!("a {kind} named {name}"))
}

/// Millicores from a Kubernetes CPU quantity (`250m`, `2`, `0.5`).
fn millicores(q: &str) -> u64 {
    match q.strip_suffix('m') {
        Some(m) => m.parse().unwrap_or_else(|_| panic!("cpu quantity {q}")),
        None => {
            (q.parse::<f64>()
                .unwrap_or_else(|_| panic!("cpu quantity {q}"))
                * 1000.0) as u64
        }
    }
}

/// Mebibytes from a memory quantity (`512Mi`, `1Gi`, `1G`).
fn mebibytes(q: &str) -> u64 {
    let (n, unit) = q
        .find(|c: char| c.is_ascii_alphabetic())
        .map(|i| q.split_at(i))
        .unwrap_or((q, ""));
    let n: f64 = n.parse().unwrap_or_else(|_| panic!("memory quantity {q}"));
    let mib = match unit {
        "Mi" => n,
        "Gi" => n * 1024.0,
        "M" => n * 1_000_000.0 / 1_048_576.0,
        "G" => n * 1_000_000_000.0 / 1_048_576.0,
        "Ki" => n / 1024.0,
        "" => n / 1_048_576.0,
        other => panic!("memory unit {other} in {q}"),
    };
    mib.ceil() as u64
}

/// Every reason the standby manifest's text breaks one of the properties,
/// each naming what to put back. Empty when they all hold.
fn findings(standby_text: &str) -> Vec<String> {
    let docs = documents_of(standby_text);
    let dev_docs = documents_of(&file(DEV));
    let dev_labels =
        &object(&dev_docs, "Deployment", "boss-dev")["spec"]["template"]["metadata"]["labels"];
    let dev_app = dev_labels["app"]
        .as_str()
        .expect("the dev pod carries an `app` label");
    let dev_prefers = preferred_purposes(object(&dev_docs, "Deployment", "boss-dev"));
    assert!(
        !dev_prefers.is_empty(),
        "{DEV} no longer prefers a {PURPOSE}, so the standby's bar from it reads nothing"
    );
    let mut out = Vec::new();

    let deploy = object(&docs, "Deployment", "boss-dev-standby");
    let pod = &deploy["spec"]["template"]["spec"];
    let labels = &deploy["spec"]["template"]["metadata"]["labels"];

    if labels["app"].as_str() == Some(dev_app) {
        out.push(format!(
            "the standby carries app={dev_app}, the dev pod's label: the dev pod's Deployment and \
             its ssh Service would select it"
        ));
    }

    let anti = pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let off_the_dev_node = anti.iter().any(|t| {
        t["topologyKey"] == "kubernetes.io/hostname"
            && t["labelSelector"]["matchLabels"]["app"].as_str() == Some(dev_app)
    });
    if !off_the_dev_node {
        out.push(format!(
            "no REQUIRED podAntiAffinity term against app={dev_app} with topologyKey \
             kubernetes.io/hostname: the standby may land on the dev pod's node and go down with it"
        ));
    }

    let off_control_planes = pod["affinity"]["nodeAffinity"]
        ["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"]
        .as_array()
        .is_some_and(|terms| {
            terms.iter().all(|t| {
                t["matchExpressions"].as_array().is_some_and(|es| {
                    es.iter().any(|e| {
                        e["key"] == "node-role.kubernetes.io/control-plane"
                            && e["operator"] == "DoesNotExist"
                    })
                })
            })
        });
    if !off_control_planes {
        out.push(
            "no REQUIRED nodeAffinity keeping it off the control planes \
             (node-role.kubernetes.io/control-plane DoesNotExist)"
                .to_string(),
        );
    }

    // Every required term must bar the dev pod's preferred purposes: the
    // terms are ORed, so one term without the bar lets the standby onto
    // that node.
    let off_the_dev_pods_preferred_node = pod["affinity"]["nodeAffinity"]
        ["requiredDuringSchedulingIgnoredDuringExecution"]["nodeSelectorTerms"]
        .as_array()
        .is_some_and(|terms| {
            terms.iter().all(|t| {
                t["matchExpressions"].as_array().is_some_and(|es| {
                    es.iter().any(|e| {
                        e["key"] == PURPOSE
                            && e["operator"] == "NotIn"
                            && dev_prefers.iter().all(|p| {
                                e["values"]
                                    .as_array()
                                    .is_some_and(|vs| vs.iter().any(|v| v == p.as_str()))
                            })
                    })
                })
            })
        });
    if !off_the_dev_pods_preferred_node {
        out.push(format!(
            "no REQUIRED nodeAffinity term {{key: {PURPOSE}, operator: NotIn, values: {dev_prefers:?}}}: \
             the standby may land on the node the dev pod prefers, and its anti-affinity then bars \
             the dev pod from that node with nothing to move it back (review 75a760c9, finding 1)"
        ));
    }

    let containers: Vec<&Value> = ["initContainers", "containers"]
        .iter()
        .flat_map(|k| pod[*k].as_array().into_iter().flatten())
        .collect();
    if containers.is_empty() {
        out.push("the standby pod declares no containers".to_string());
    }
    let mut cpu = 0;
    let mut mem = 0;
    for c in &containers {
        let name = c["name"].as_str().unwrap_or("?");
        match (
            c["resources"]["requests"]["cpu"].as_str(),
            c["resources"]["requests"]["memory"].as_str(),
        ) {
            (Some(cpu_q), Some(mem_q)) => {
                cpu += millicores(cpu_q);
                mem += mebibytes(mem_q);
            }
            _ => out.push(format!(
                "container `{name}` requests no cpu or no memory: an unbounded request is not \
                 a small one"
            )),
        }
    }
    if cpu >= 1000 {
        out.push(format!(
            "the standby requests {cpu}m CPU; it must stay under 1 CPU to fit a node that is short"
        ));
    }
    if mem >= 1024 {
        out.push(format!(
            "the standby requests {mem}Mi memory; it must stay under 1Gi to fit a node that is short"
        ));
    }

    for v in pod["volumes"].as_array().into_iter().flatten() {
        let name = v["name"].as_str().unwrap_or("?");
        if v.get("persistentVolumeClaim").is_some() {
            out.push(format!(
                "volume `{name}` is a PersistentVolumeClaim: the standby owns no volume, so \
                 nothing it holds can keep it from scheduling or be lost in a roll"
            ));
        }
    }
    for (route, secret) in secrets_named(pod, &containers) {
        if secret == WRITE_TOKEN {
            out.push(format!(
                "{route} names the forge WRITE token ({WRITE_TOKEN}): the standby reads and \
                 files, it never pushes (Q1, David 2026-11-07)"
            ));
        }
    }

    let restarted = containers.iter().any(|c| {
        let port = &c["livenessProbe"]["tcpSocket"]["port"];
        port == 22 || port == "ssh"
    });
    if !restarted {
        out.push(
            "no container declares a livenessProbe on tcpSocket 22: a standby whose sshd never \
             installed or later died stays doorless with nothing to restart it (review 75a760c9, \
             finding 4)"
                .to_string(),
        );
    }

    if !standby_text
        .lines()
        .any(|l| l.trim() == "AllowTcpForwarding no")
    {
        out.push(
            "the standby's sshd_config does not say `AllowTcpForwarding no`: OpenSSH's default \
             forwards into the cluster network from this pod (review 75a760c9, finding 3)"
                .to_string(),
        );
    }

    let service = object(&docs, "Service", "boss-dev-standby-ssh");
    if service["spec"]["selector"]["app"] != labels["app"] {
        out.push(format!(
            "Service boss-dev-standby-ssh selects {} but the standby pod is {}",
            service["spec"]["selector"]["app"], labels["app"]
        ));
    }
    out
}

#[test]
fn the_standby_is_small_off_the_dev_pods_node_and_holds_no_build_state() {
    let found = findings(&file(STANDBY));
    assert!(found.is_empty(), "{STANDBY}:\n  {}", found.join("\n  "));
}

fn mutant(from: &str, to: &str) -> String {
    let text = file(STANDBY);
    assert!(
        text.contains(from),
        "{STANDBY} no longer holds `{from}`, so this mutant would test nothing"
    );
    text.replacen(from, to, 1)
}

fn refused(text: &str, needle: &str) {
    let found = findings(text);
    assert!(
        found.iter().any(|f| f.contains(needle)),
        "expected a finding holding `{needle}`, got: {found:#?}"
    );
}

/// Each property refuses its own break, so the pin cannot pass by reading
/// nothing.
#[test]
fn each_broken_property_is_refused_by_name() {
    refused(
        &mutant(
            "topologyKey: kubernetes.io/hostname",
            "topologyKey: topology.kubernetes.io/zone",
        ),
        "no REQUIRED podAntiAffinity",
    );
    refused(
        &mutant(
            "podAntiAffinity:\n          requiredDuringSchedulingIgnoredDuringExecution:",
            "podAntiAffinity:\n          preferredDuringSchedulingIgnoredDuringExecution:",
        ),
        "no REQUIRED podAntiAffinity",
    );
    refused(
        &mutant(
            "{key: node-role.kubernetes.io/control-plane, operator: DoesNotExist}",
            "{key: node-role.kubernetes.io/control-plane, operator: Exists}",
        ),
        "off the control planes",
    );
    refused(
        &mutant(
            "requests: {cpu: 250m, memory: 512Mi",
            "requests: {cpu: \"1\", memory: 512Mi",
        ),
        "must stay under 1 CPU",
    );
    refused(
        &mutant(
            "requests: {cpu: 250m, memory: 512Mi",
            "requests: {cpu: 250m, memory: 1Gi",
        ),
        "must stay under 1Gi",
    );
    refused(
        &mutant(
            "      labels: {app: boss-dev-standby}\n    spec:",
            "      labels: {app: boss-dev}\n    spec:",
        ),
        "the dev pod's label",
    );
    refused(
        &mutant(
            "        - name: standby\n          emptyDir: {}",
            "        - name: standby\n          persistentVolumeClaim: {claimName: boss-dev-work}",
        ),
        "is a PersistentVolumeClaim",
    );
    refused(
        &mutant(
            "secret: {secretName: forge-read,",
            "secret: {secretName: boss-dev-forge-token,",
        ),
        "forge WRITE token",
    );
    // Finding 1: the bar from the dev pod's preferred node, dropped, and
    // aimed at a purpose the dev pod does not prefer.
    let bar = "\n                  - {key: boss.dev/purpose, operator: NotIn, values: [\"build\"]}";
    refused(&mutant(bar, ""), "no REQUIRED nodeAffinity term");
    refused(
        &mutant(
            bar,
            "\n                  - {key: boss.dev/purpose, operator: NotIn, values: [\"utility\"]}",
        ),
        "no REQUIRED nodeAffinity term",
    );
    // Finding 6: the write token by each route a spec can take a Secret.
    let hosts = "- {name: BOSS_MACHINE_TOKEN_HOSTS, value: \".boss.svc.cluster.local\"}";
    refused(
        &mutant(
            hosts,
            &format!(
                "{hosts}\n            - {{name: FORGE_PUSH, valueFrom: {{secretKeyRef: {{name: \
                 boss-dev-forge-token, key: token}}}}}}"
            ),
        ),
        "env `FORGE_PUSH` names the forge WRITE token",
    );
    let env = "          env:\n            - {name: FORGE_URL";
    refused(
        &mutant(
            env,
            &format!("          envFrom: [{{secretRef: {{name: boss-dev-forge-token}}}}]\n{env}"),
        ),
        "envFrom names the forge WRITE token",
    );
    refused(
        &mutant(
            "- name: forge-token\n          secret: {secretName: forge-read, optional: true, defaultMode: 0400}",
            "- name: forge-token\n          projected: {sources: [{secret: {name: boss-dev-forge-token}}]}",
        ),
        "projected volume `forge-token` names the forge WRITE token",
    );
    // Finding 4: no liveness probe, so nothing restarts a doorless standby.
    refused(
        &mutant("livenessProbe:", "livenessProbeGone:"),
        "no container declares a livenessProbe",
    );
    // Finding 3: sshd forwarding back at OpenSSH's default.
    refused(
        &mutant("AllowTcpForwarding no", "AllowTcpForwarding yes"),
        "AllowTcpForwarding no",
    );
}

#[test]
fn quantities_read_as_kubernetes_writes_them() {
    assert_eq!(millicores("250m"), 250);
    assert_eq!(millicores("2"), 2000);
    assert_eq!(millicores("0.5"), 500);
    assert_eq!(mebibytes("512Mi"), 512);
    assert_eq!(mebibytes("1Gi"), 1024);
    assert_eq!(mebibytes("1G"), 954);
}
