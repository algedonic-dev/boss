//! Placement is evaluated as scheduler data; the reader proves no live sizing.
use boss_testing::{repo_root, scratch_dir};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::process::Command;
use yaml_serde::Value;

fn eligible(spec: &Value, labels: &BTreeMap<&str, &str>) -> bool {
    if spec["nodeSelector"].as_mapping().is_some_and(|selector| {
        !selector
            .iter()
            .all(|(key, value)| labels.get(key.as_str().unwrap()).copied() == value.as_str())
    }) {
        return false;
    }
    let terms = &spec["affinity"]["nodeAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
        ["nodeSelectorTerms"];
    terms.as_sequence().is_none_or(|terms| {
        terms.iter().any(|term| {
            term["matchExpressions"]
                .as_sequence()
                .unwrap()
                .iter()
                .all(|expression| {
                    let key = expression["key"].as_str().unwrap();
                    let actual = labels.get(key);
                    let included = expression["values"].as_sequence().is_some_and(|values| {
                        values
                            .iter()
                            .any(|v| Some(v.as_str().unwrap()) == actual.copied())
                    });
                    match expression["operator"].as_str().unwrap() {
                        "In" => actual.is_some() && included,
                        "NotIn" => !included,
                        "Exists" => actual.is_some(),
                        "DoesNotExist" => actual.is_none(),
                        other => panic!("unsupported placement operator {other}"),
                    }
                })
        })
    })
}

#[test]
fn rendered_instances_have_distinct_required_nodes_and_no_cp1_fallback() {
    let out = scratch_dir("stack-placement-render");
    let output = Command::new("bash")
        .arg(repo_root().join("infra/cluster/render-instance.sh"))
        .arg("--all")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (namespace, target, control_plane) in
        [("boss", "cp-3", true), ("boss-playground", "w-1", false)]
    {
        let text = std::fs::read_to_string(out.join(namespace).join("boss.yaml")).unwrap();
        let deployment: Vec<Value> = yaml_serde::Deserializer::from_str(&text)
            .map(|d| Value::deserialize(d).unwrap())
            .filter(|d| {
                d["kind"].as_str() == Some("Deployment")
                    && d["metadata"]["name"].as_str() == Some("boss")
            })
            .collect();
        assert_eq!(deployment.len(), 1);
        let spec = &deployment[0]["spec"]["template"]["spec"];
        if !control_plane {
            assert!(spec["nodeSelector"]["node-role.kubernetes.io/control-plane"].is_null());
        }
        for node in ["cp-1", "cp-2", "cp-3", "w-1", "w-2"] {
            let mut labels = BTreeMap::from([("kubernetes.io/hostname", node)]);
            if node.starts_with("cp-") {
                labels.insert("node-role.kubernetes.io/control-plane", "");
            }
            assert_eq!(
                eligible(spec, &labels),
                node == target,
                "{namespace}: {node}"
            );
        }
        assert!(
            !eligible(spec, &BTreeMap::new()),
            "missing required node cannot fall back"
        );
        let boss: Vec<_> = spec["containers"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter(|c| c["name"].as_str() == Some("boss"))
            .collect();
        assert_eq!(boss.len(), 1);
        assert_eq!(
            boss[0]["resources"]["limits"]["memory"].as_str(),
            Some("12Gi")
        );
    }
}

#[test]
fn placement_follows_instance_data_and_missing_placement_refuses_before_rendering() {
    let tree = scratch_dir("stack-placement-data").join("tree");
    let cluster = tree.join("infra/cluster");
    std::fs::create_dir_all(cluster.join("manifests")).unwrap();
    for entry in std::fs::read_dir(repo_root().join("infra/cluster/manifests")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "yaml") {
            std::fs::copy(
                &path,
                cluster.join("manifests").join(path.file_name().unwrap()),
            )
            .unwrap();
        }
    }
    std::fs::copy(
        repo_root().join("infra/cluster/instance-manifests.txt"),
        cluster.join("instance-manifests.txt"),
    )
    .unwrap();
    let tenant = tree.join("examples/brewery/seeds");
    std::fs::create_dir_all(&tenant).unwrap();
    std::fs::copy(
        repo_root().join("examples/brewery/seeds/tenant.toml"),
        tenant.join("tenant.toml"),
    )
    .unwrap();
    let original =
        std::fs::read_to_string(repo_root().join("infra/cluster/instances.toml")).unwrap();
    let changed = original.replace("stack_node = \"w-1\"", "stack_node = \"w-2\"");
    assert_ne!(original, changed);
    std::fs::write(cluster.join("instances.toml"), &changed).unwrap();
    let out = tree.join("rendered");
    let render = || {
        Command::new("bash")
            .arg(repo_root().join("infra/cluster/render-instance.sh"))
            .env("BOSS_CLUSTER_TREE", &tree)
            .arg("--all")
            .arg(&out)
            .output()
            .unwrap()
    };
    let output = render();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = std::fs::read_to_string(out.join("boss-playground/boss.yaml")).unwrap();
    let deployment = yaml_serde::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).unwrap())
        .find(|d| {
            d["kind"].as_str() == Some("Deployment")
                && d["metadata"]["name"].as_str() == Some("boss")
        })
        .unwrap();
    let spec = &deployment["spec"]["template"]["spec"];
    assert!(eligible(
        spec,
        &BTreeMap::from([("kubernetes.io/hostname", "w-2")])
    ));
    assert!(!eligible(
        spec,
        &BTreeMap::from([("kubernetes.io/hostname", "w-1")])
    ));
    std::fs::remove_dir_all(&out).unwrap();
    std::fs::write(
        cluster.join("instances.toml"),
        changed.replace("stack_node = \"w-2\"", ""),
    )
    .unwrap();
    let refused = render();
    assert_eq!(
        refused.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !out.exists(),
        "missing registered placement must not leave a partial render"
    );
}

#[test]
fn unregistered_positional_render_preserves_the_admitted_control_plane_contract() {
    // Admitted origin/main26574: control-plane selector plus cp-2 NotIn;
    // no required cp-3. This legacy render form does not create a row.
    let output = Command::new("bash")
        .arg(repo_root().join("infra/cluster/render-instance.sh"))
        .args(["boss-other", "tenant", "true", "other.example", "false"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let deployment = yaml_serde::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).unwrap())
        .find(|d| {
            d["kind"].as_str() == Some("Deployment")
                && d["metadata"]["name"].as_str() == Some("boss")
        })
        .unwrap();
    let spec = &deployment["spec"]["template"]["spec"];
    for (node, expected) in [
        ("cp-1", true),
        ("cp-2", false),
        ("cp-3", true),
        ("w-1", false),
    ] {
        let mut labels = BTreeMap::from([("kubernetes.io/hostname", node)]);
        if node.starts_with("cp-") {
            labels.insert("node-role.kubernetes.io/control-plane", "");
        }
        assert_eq!(
            eligible(spec, &labels),
            expected,
            "legacy positional {node}"
        );
    }
}

#[test]
fn the_actual_boss_deployment_requires_cp3_and_keeps_its_unmeasured_limit() {
    let text =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss.yaml")).unwrap();
    let deployments: Vec<Value> = yaml_serde::Deserializer::from_str(&text)
        .map(|document| Value::deserialize(document).unwrap())
        .filter(|document| {
            document["kind"].as_str() == Some("Deployment")
                && document["metadata"]["name"].as_str() == Some("boss")
        })
        .collect();
    assert_eq!(deployments.len(), 1);
    let spec = &deployments[0]["spec"]["template"]["spec"];
    for (hostname, control_plane, expected) in [
        (Some("cp-1"), true, false),
        (Some("cp-2"), true, false),
        (Some("cp-3"), true, true),
        (Some("cp-3"), false, false),
        (None, true, false),
    ] {
        let mut labels = BTreeMap::new();
        if let Some(hostname) = hostname {
            labels.insert("kubernetes.io/hostname", hostname);
        }
        if control_plane {
            labels.insert("node-role.kubernetes.io/control-plane", "");
        }
        assert_eq!(
            eligible(spec, &labels),
            expected,
            "placement for {labels:?}"
        );
    }
    let containers = spec["containers"].as_sequence().unwrap();
    let boss: Vec<_> = containers
        .iter()
        .filter(|c| c["name"].as_str() == Some("boss"))
        .collect();
    assert_eq!(boss.len(), 1);
    assert_eq!(
        boss[0]["resources"]["limits"]["memory"].as_str(),
        Some("12Gi")
    );
    assert_eq!(
        boss[0]["resources"]["requests"]["memory"].as_str(),
        Some("4Gi")
    );
}

#[test]
fn memory_observation_preserves_native_identity_and_unknowns() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(repo_root().join("infra/forge/tests/stack-memory-test.py"))
        .output()
        .expect("python3 must answer");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
