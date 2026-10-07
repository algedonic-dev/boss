//! The fixed discovery adapter runs its real pure sanitizer against native hostile fixtures.
//! No protected cluster read occurs here. Source delivery cannot prove retained history.
//! tree-wide pin — the request declaration reads a cluster manifest, so
//! a future manifest-only car must visit its equality test too.
use boss_testing::rbac::{Node, Object, Value as Yaml, read_stream};
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn the_operator_sanitizer_conserves_sources_and_refuses_unknown() {
    let output = Command::new("python3")
        .arg(repo_root().join("infra/forge/tests/admission-source-test.py"))
        .output()
        .expect("python3 is required; an unavailable boundary test is not a pass");
    assert!(
        output.status.success(),
        "actual sanitizer failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json_node(node: &Node) -> Value {
    match &node.value {
        Yaml::Str(value) => json!(value),
        Yaml::Seq(values) => values.iter().map(json_node).collect(),
        Yaml::Map(values) => Value::Object(
            values
                .iter()
                .map(|(key, node)| (key.clone(), json_node(node)))
                .collect(),
        ),
        Yaml::Null => Value::Null,
        Yaml::Text => panic!("the declaration pin cannot interpret block scalar text"),
    }
}

#[test]
fn the_declared_targets_equal_the_policy_binding_and_actor_grant() {
    let root = repo_root();
    let manifest = "infra/cluster/manifests/boss-break-glass-operator.yaml";
    let text = std::fs::read_to_string(root.join(manifest)).unwrap();
    let objects = read_stream(manifest, &text).unwrap();
    assert_target_parity(&root, &objects);
}

fn assert_target_parity(root: &std::path::Path, objects: &[Object]) {
    let object = |kind: &str| {
        let found: Vec<_> = objects.iter().filter(|o| o.kind == kind).collect();
        assert_eq!(found.len(), 1, "one reviewed declaration of {kind}");
        found[0]
    };
    let target: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("infra/ops/admission-discovery-target.json")).unwrap(),
    )
    .unwrap();
    let policy = object("ValidatingAdmissionPolicy");
    let binding = object("ValidatingAdmissionPolicyBinding");
    let account = object("ServiceAccount");
    let role = object("Role");
    let actor_binding = object("RoleBinding");
    assert_eq!(target["policy"], policy.name);
    assert_eq!(target["binding"], binding.name);
    assert_eq!(
        binding
            .root
            .path(&["spec", "policyName"])
            .unwrap()
            .str()
            .unwrap(),
        policy.name
    );
    assert!(
        binding.root.path(&["spec", "matchResources"]).is_none(),
        "a narrowed binding changes the coverage declaration"
    );
    let namespace = account.namespace.as_ref().unwrap();
    assert_eq!(target["namespace"], namespace.as_str());
    assert_eq!(role.namespace.as_ref().unwrap(), namespace);
    assert_eq!(actor_binding.namespace.as_ref().unwrap(), namespace);
    assert_eq!(
        json_node(actor_binding.root.get("roleRef").unwrap()),
        json!({"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":role.name})
    );
    assert_eq!(
        json_node(actor_binding.root.get("subjects").unwrap()),
        json!([{"kind":"ServiceAccount","name":account.name,"namespace":namespace}])
    );
    let user = format!("system:serviceaccount:{namespace}:{}", account.name);
    assert_eq!(target["user"], user);
    assert_eq!(
        target["groups"],
        json!([
            "system:serviceaccounts",
            format!("system:serviceaccounts:{namespace}"),
            "system:authenticated"
        ])
    );
    let conditions = json_node(policy.root.path(&["spec", "matchConditions"]).unwrap());
    assert_eq!(conditions.as_array().unwrap().len(), 1);
    assert_eq!(
        conditions[0]["expression"],
        format!("request.userInfo.username == '{user}'")
    );
    let constraints = policy.root.path(&["spec", "matchConstraints"]).unwrap();
    assert_eq!(
        json_node(constraints.get("namespaceSelector").unwrap()),
        json!({"matchLabels":{"kubernetes.io/metadata.name":namespace}})
    );
    let rules = json_node(constraints.get("resourceRules").unwrap());
    assert_eq!(rules.as_array().unwrap().len(), 1);
    assert_eq!(rules[0]["operations"], json!(["UPDATE"]));
    assert_eq!(rules[0]["apiVersions"], json!(["*"]));
    let grant = json_node(role.root.get("rules").unwrap());
    assert_eq!(grant.as_array().unwrap().len(), 1);
    assert_eq!(grant[0]["apiGroups"], rules[0]["apiGroups"]);
    assert_eq!(grant[0]["resources"], rules[0]["resources"]);
    let mut requests = Vec::new();
    for group in grant[0]["apiGroups"].as_array().unwrap() {
        for resource in grant[0]["resources"].as_array().unwrap() {
            for verb in grant[0]["verbs"].as_array().unwrap() {
                requests.push(json!({"group":group,"resource":resource,"verb":verb}));
            }
        }
    }
    assert_eq!(target["requests"], json!(requests));
    assert_eq!(target["stages"], json!(["ResponseComplete"]));
}

#[test]
fn a_role_without_its_actor_binding_cannot_define_the_target() {
    let root = repo_root();
    let manifest = "infra/cluster/manifests/boss-break-glass-operator.yaml";
    let text = std::fs::read_to_string(root.join(manifest)).unwrap();
    for key in ["roleRef", "subjects"] {
        let mut objects = read_stream(manifest, &text).unwrap();
        let binding = objects
            .iter_mut()
            .find(|o| o.kind == "RoleBinding")
            .unwrap();
        let Yaml::Map(entries) = &mut binding.root.value else {
            panic!("mapping")
        };
        entries.retain(|(name, _)| name != key);
        assert!(
            std::panic::catch_unwind(|| assert_target_parity(&root, &objects)).is_err(),
            "missing RoleBinding {key} passed the attribution pin"
        );
    }
}

#[test]
fn the_host_boundary_suppresses_native_errors_and_rejects_nested_export() {
    let root = repo_root();
    let scratch = scratch_dir("admission-discovery-boundary");
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_file(
        &scratch.join("estate.json"),
        r#"{"data":[{"id":"cp-1","role":"talos-control-plane","address":"192.0.2.11","retired":false}]}"#,
    );
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/sh
case "$*" in
 '-n docker image inspect '*|'-n docker create '*) exit 0 ;;
 '-n docker container inspect '*) exit 1 ;;
esac
printf '%s\n' "$*" > "$CALLS"
cat > "$INPUT"
echo 'DO-NOT-EXPORT credential raw diagnostic' >&2
cat "$ANSWER"
exit "${DOOR_RC:-0}"
"#,
    );
    let run = |answer: &str, code: &str| {
        write_file(&scratch.join("answer.json"), answer);
        Command::new("bash")
            .arg(root.join("infra/forge/discover-admission-source.sh"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("BOSS_SOR_ENV", scratch.join("absent.env"))
            .env(
                "BOSS_ESTATE_NODES_URL",
                format!("file://{}", scratch.join("estate.json").display()),
            )
            .env("BOSS_FORGE_REGISTRY_HOST", "registry.invalid")
            .env("CALLS", scratch.join("calls"))
            .env("INPUT", scratch.join("input"))
            .env("ANSWER", scratch.join("answer.json"))
            .env("DOOR_RC", code)
            .output()
            .unwrap()
    };
    let clean = r#"{"schema":"boss.admission-source-discovery.v1","scope":"configuration_discovery_only","history_verdict":"unavailable","roster":{"state":"unavailable","reason":"native_read_failed"},"nodes":[]}"#;
    let output = run(clean, "0");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        serde_json::from_str::<Value>(clean).unwrap()
    );
    let unavailable_output = run(clean, "4");
    assert_eq!(unavailable_output.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable_output.stdout).unwrap(),
        serde_json::from_str::<Value>(clean).unwrap(),
        "an unavailable source retains its complete sanitized diagnosis"
    );
    let calls = std::fs::read_to_string(scratch.join("calls")).unwrap();
    for flag in [
        "--read-only",
        "--cap-drop ALL",
        "--entrypoint timeout",
        "-k 5 850 python3 -B /discovery.py",
        "dst=/tc,readonly",
        "dst=/kc,readonly",
        "dst=/talosctl,readonly",
        "dst=/target.json,readonly",
    ] {
        assert!(
            calls.contains(flag),
            "missing fixed boundary {flag}: {calls}"
        );
    }
    for (answer, code) in [
        (clean, "1"),
        ("DO-NOT-EXPORT", "0"),
        (
            &clean.replace(
                "\"reason\":\"native_read_failed\"",
                "\"reason\":\"native_read_failed\",\"raw\":\"DO-NOT-EXPORT\"",
            ),
            "0",
        ),
    ] {
        let output = run(answer, code);
        assert_eq!(
            output.status.code(),
            Some(4),
            "answer={answer} code={code} stdout={}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("DO-NOT-EXPORT"));
        assert!(output.stderr.is_empty());
    }
}
