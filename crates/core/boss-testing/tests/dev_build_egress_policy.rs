//! Source render and admission controls; deployed Cilium effects remain
//! a separate fake-only runtime obligation on b3071bf4 (a68 D4).
use boss_testing::{repo_root, scratch_dir, write_file};
use serde::Deserialize;
use serde_json::Value;
use std::process::Command;

#[test]
fn published_control_image_tags_equal_the_managed_dev_pod_images() {
    let inputs: Value = serde_json::from_str(
        &std::fs::read_to_string(repo_root().join("infra/dev/control-images.json")).unwrap(),
    )
    .unwrap();
    let text =
        std::fs::read_to_string(repo_root().join("infra/cluster/manifests/boss-dev.yaml")).unwrap();
    let manifest = yaml_serde::Deserializer::from_str(&text)
        .map(|document| Value::deserialize(document).unwrap())
        .find(|document| document["kind"] == "Deployment")
        .unwrap();
    let source = manifest["spec"]["template"]["spec"]["containers"]
        .as_array()
        .unwrap();
    for (field, name) in [("worker_image", "dev"), ("postgres_image", "postgres")] {
        let container = source.iter().find(|item| item["name"] == name).unwrap();
        let image = container["image"]
            .as_str()
            .unwrap()
            .split_once('/')
            .unwrap()
            .1;
        assert_eq!(
            image,
            format!(
                "{}:{}",
                inputs[field]["repository"].as_str().unwrap(),
                inputs[field]["tag"].as_str().unwrap()
            ),
            "published {field} drifted from managed {name}"
        );
    }
}

#[test]
fn a_forge_exception_that_is_the_system_of_record_refuses_without_replacing_policy() {
    let root = scratch_dir("dev-build-policy-refusal");
    let estate = root.join("estate.toml");
    write_file(
        &estate,
        "forge_host = \"192.168.50.7\"\nforge_url = \"http://192.168.50.7:3000\"\nsor_url = \"http://192.168.50.7:7900\"\n",
    );
    let output = root.join("policy.yaml");
    write_file(&output, "existing original policy");
    let result = Command::new("python3")
        .arg(repo_root().join("infra/cluster/render-dev-build-policy.py"))
        .arg(repo_root().join("infra/cluster/manifests/boss-dev-build-egress.yaml"))
        .arg(&estate)
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot be the system of record"));
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "existing original policy"
    );
}

#[test]
fn the_converge_renders_worker_egress_from_estate_before_any_apply() {
    let root = scratch_dir("dev-build-egress-render");
    let estate = root.join("estate.toml");
    write_file(
        &estate,
        "forge_host = \"192.168.50.7\"\nforge_url = \"http://192.168.50.7:3000\"\nsor_url = \"http://198.51.100.44:7900\"\n",
    );
    let output = root.join("policy.yaml");
    let result = Command::new("python3")
        .arg(repo_root().join("infra/cluster/render-dev-build-policy.py"))
        .arg(repo_root().join("infra/cluster/manifests/boss-dev-build-egress.yaml"))
        .arg(&estate)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "render refusal: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let policy: Value = yaml_serde::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(policy["kind"], "CiliumNetworkPolicy");
    assert_eq!(policy["metadata"]["namespace"], "boss-dev");
    assert_eq!(
        policy["spec"]["endpointSelector"]["matchLabels"]["boss.dev/purpose"],
        "isolated-build"
    );
    let deny = &policy["spec"]["egressDeny"];
    let wire = deny.to_string();
    assert!(wire.contains("kube-apiserver") && wire.contains("remote-node"));
    assert!(
        wire.contains("198.51.100.44/32"),
        "even a public SoR alias must be denied"
    );
    for cidr in [
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "169.254.0.0/16",
        "100.64.0.0/10",
        "fc00::/7",
        "fe80::/10",
        "::ffff:0:0/96",
    ] {
        assert!(
            wire.contains(cidr),
            "private/special network missing: {cidr}"
        );
    }
    assert!(
        wire.contains("NotIn") && wire.contains("kube-system"),
        "non-DNS endpoints in every namespace, including future BOSS instances, must be denied"
    );
    let allows = &policy["spec"]["egress"];
    assert!(allows.to_string().contains("192.168.50.7/32"));
    assert!(allows.to_string().contains("3000"));
    assert!(allows.to_string().contains("kube-dns") && allows.to_string().contains("443"));
    assert!(
        !allows.to_string().contains("\"all\""),
        "no broad allow competes with the fence"
    );
    let source =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let render = source
        .find("render-dev-build-policy.py")
        .expect("normal converge owns the policy render");
    assert!(
        render < source.find("apply_instance \"$SOURCE_NS\"").unwrap(),
        "policy must be rendered before the source apply"
    );
}
