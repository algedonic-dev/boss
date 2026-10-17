//! The bounded reader executes through an actual stub of its kubectl port.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn kubectl_stdin_mode_is_explicit_and_unknown_modes_refuse() {
    let helper = repo_root().join("infra/estate/ops-credentials.sh");
    let run = |args: &[&str]| {
        Command::new("sh")
            .arg("-c")
            .arg(". \"$1\"; shift; ops_kubectl \"$@\"")
            .arg("sh")
            .arg(&helper)
            .args(args)
            .env("BOSS_FORGE_REGISTRY_HOST", "fixture.invalid")
            .output()
            .unwrap()
    };
    let ordinary = run(&[]);
    let stdin = run(&["--stdin"]);
    assert!(ordinary.status.success() && stdin.status.success());
    let expected = String::from_utf8(ordinary.stdout).unwrap();
    assert!(!expected.contains(" -i ") && !expected.contains(" -t "));
    assert_eq!(
        String::from_utf8(stdin.stdout).unwrap(),
        expected.replacen("run --rm ", "run --rm -i ", 1)
    );
    for args in [
        vec!["--unknown"],
        vec!["--stdin", "--stdin"],
        vec!["--stdin", "extra"],
    ] {
        let refused = run(&args);
        assert_eq!(String::from_utf8(refused.stdout).unwrap().trim(), "false");
        assert!(String::from_utf8_lossy(&refused.stderr).contains("accepts only"));
    }
}

#[test]
fn installed_fence_requires_exact_spec_identity_and_ready_cilium() {
    let root = scratch_dir("fence-reader");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let source = root.join("source");
    for name in [
        "infra/cluster/render-dev-build-policy.py",
        "infra/cluster/manifests/boss-dev-build-egress.yaml",
        "infra/estate/estate.toml",
    ] {
        std::fs::create_dir_all(source.join(name).parent().unwrap()).unwrap();
        write_file(
            &source.join(name),
            &std::fs::read_to_string(repo_root().join(name)).unwrap(),
        );
    }
    let credential = root.join("fake-ops/kubeconfig");
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    write_file(&credential, "fake only");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    write_exec(
        &bin.join("sudo"),
        r#"#!/bin/sh
case "$*" in
  '-n docker image inspect '* | '-n docker create '*) exit 0;;
  '-n docker container inspect '*) exit 1;;
esac
printf '%s\n' "$*" >> "$STATE/calls"
interactive=no
for argument in "$@"; do
  [ "$argument" != -i ] || interactive=yes
done
# Docker closes container stdin unless the caller explicitly keeps it open.
[ "$interactive" = yes ] || exec < /dev/null
while [ $# -gt 0 ]; do
  [ "$1" != --kubeconfig=/kc ] || { shift; break; }
  shift
done
case "$1 $2" in
  'create --dry-run=client')
    cat > "$STATE/serialized-source.yaml"
    [ -s "$STATE/serialized-source.yaml" ] || { echo 'error: no objects passed to create' >&2; exit 1; }
    cat "$STATE/expected.json";;
  'get ciliumnetworkpolicy.cilium.io')
    [ "$3 $4 $5" = 'boss-dev-build-egress -n boss-dev' ] || exit 90
    [ -f "$STATE/actual.json" ] || { echo 'NotFound actual fence' >&2; exit 1; }
    cat "$STATE/actual.json";;
  'get daemonset')
    [ "$3 $4 $5" = 'cilium -n kube-system' ] || exit 91
    cat "$STATE/cilium.json";;
  *) echo "unexpected command: $*" >&2; exit 92;;
esac
"#,
    );
    let render = root.join("rendered.yaml");
    let rendered = Command::new("python3")
        .arg(repo_root().join("infra/cluster/render-dev-build-policy.py"))
        .arg(repo_root().join("infra/cluster/manifests/boss-dev-build-egress.yaml"))
        .arg(repo_root().join("infra/estate/estate.toml"))
        .arg(&render)
        .output()
        .unwrap();
    assert!(rendered.status.success());
    let expected = Value::deserialize(yaml_serde::Deserializer::from_str(
        &std::fs::read_to_string(render).unwrap(),
    ))
    .unwrap();
    write_file(&root.join("expected.json"), &expected.to_string());
    let mut actual = expected.clone();
    actual["metadata"]["uid"] = json!("native-policy-uid");
    actual["metadata"]["resourceVersion"] = json!("173");
    actual["metadata"]["managedFields"] = json!([{"manager":"server-default"}]);
    let ready = json!({"apiVersion":"apps/v1","kind":"DaemonSet","metadata":{"name":"cilium","namespace":"kube-system","generation":2},"status":{"observedGeneration":2,"desiredNumberScheduled":3,"updatedNumberScheduled":3,"numberReady":3,"numberAvailable":3,"numberUnavailable":0,"numberMisscheduled":0}});
    write_file(&root.join("cilium.json"), &ready.to_string());
    let run = |value: Option<&Value>| {
        let file = root.join("actual.json");
        if let Some(v) = value {
            write_file(&file, &v.to_string());
        } else {
            std::fs::remove_file(&file).unwrap();
        }
        Command::new("bash")
            .arg(repo_root().join("infra/forge/dev-build-fence-status.sh"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("STATE", &root)
            .env("BOSS_OPS_DIR", root.join("fake-ops"))
            .env("BOSS_FENCE_SOURCE", &source)
            .env("BOSS_FORGE_REGISTRY_HOST", "fixture.invalid")
            .output()
            .unwrap()
    };
    let good = run(Some(&actual));
    assert!(
        good.status.success(),
        "{} {}",
        String::from_utf8_lossy(&good.stdout),
        String::from_utf8_lossy(&good.stderr)
    );
    let out = String::from_utf8_lossy(&good.stdout);
    assert!(
        out.contains("native-policy-uid")
            && out.contains("managedFields")
            && out.contains("worker network enforcement remains unproven")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("serialized-source.yaml")).unwrap(),
        std::fs::read_to_string(root.join("rendered.yaml")).unwrap()
    );
    // A terminating object is not an installed/readiness prerequisite (05094098).
    for object in ["policy", "daemonset"] {
        let mut policy = actual.clone();
        let mut daemonset = ready.clone();
        let deleting = if object == "policy" {
            &mut policy
        } else {
            &mut daemonset
        };
        deleting["metadata"]["deletionTimestamp"] = json!("2026-10-14T01:00:00Z");
        write_file(&root.join("cilium.json"), &daemonset.to_string());
        let refused = run(Some(&policy));
        assert!(!refused.status.success(), "accepted terminating {object}");
        assert!(String::from_utf8_lossy(&refused.stdout).contains("deletionTimestamp"));
    }
    let single = json!({"apiVersion":"apps/v1","kind":"DaemonSet","metadata":{"name":"cilium","namespace":"kube-system","generation":1,"deletionTimestamp":null},"status":{"observedGeneration":1,"desiredNumberScheduled":1,"updatedNumberScheduled":1,"numberReady":1,"numberAvailable":1}});
    write_file(&root.join("cilium.json"), &single.to_string());
    let mut null_policy = actual.clone();
    null_policy["metadata"]["deletionTimestamp"] = Value::Null;
    assert!(
        run(Some(&null_policy)).status.success(),
        "null deletion and omitted zero counts are valid"
    );
    for field in [
        "generation",
        "observedGeneration",
        "desiredNumberScheduled",
        "updatedNumberScheduled",
        "numberReady",
        "numberAvailable",
        "numberUnavailable",
        "numberMisscheduled",
    ] {
        for malformed in [
            json!(true),
            json!(false),
            json!(1.0),
            json!("1"),
            Value::Null,
        ] {
            let mut bad = single.clone();
            let section = if field == "generation" {
                "metadata"
            } else {
                "status"
            };
            bad[section][field] = malformed.clone();
            write_file(&root.join("cilium.json"), &bad.to_string());
            assert!(
                !run(Some(&actual)).status.success(),
                "accepted {field}={malformed}"
            );
        }
    }
    write_file(&root.join("cilium.json"), &ready.to_string());
    for path in [
        "namespace",
        "selector",
        "private-deny",
        "dns",
        "forge",
        "extra-allow",
        "uid",
    ] {
        let mut bad = actual.clone();
        match path {
            "namespace" => bad["metadata"]["namespace"] = json!("boss"),
            "selector" => bad["spec"]["endpointSelector"] = json!({}),
            "private-deny" => bad["spec"]["egressDeny"] = json!([]),
            "dns" => bad["spec"]["egress"][0] = json!({}),
            "forge" => bad["spec"]["egress"][1] = json!({}),
            "extra-allow" => bad["spec"]["ingress"] = json!([{}]),
            "uid" => bad["metadata"]["uid"] = Value::Null,
            _ => unreachable!(),
        }
        assert!(!run(Some(&bad)).status.success(), "accepted {path}");
    }
    assert!(!run(None).status.success());
    let mut lagging = ready.clone();
    lagging["status"]["numberReady"] = json!(2);
    write_file(&root.join("cilium.json"), &lagging.to_string());
    assert!(!run(Some(&actual)).status.success());
    write_file(
        &root.join("cilium.json"),
        &json!({"kind":"DaemonSet"}).to_string(),
    );
    assert!(!run(Some(&actual)).status.success());
    std::fs::remove_file(&credential).unwrap();
    std::os::unix::fs::symlink(root.join("fake-credential"), &credential).unwrap();
    let linked = run(Some(&actual));
    assert!(!linked.status.success());
    assert!(String::from_utf8_lossy(&linked.stderr).contains("untrusted"));
    std::fs::remove_file(&credential).unwrap();
    assert!(!run(Some(&actual)).status.success());
    write_file(&credential, "fake only");
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    write_file(&root.join("cilium.json"), &ready.to_string());
    let template = source.join("infra/cluster/manifests/boss-dev-build-egress.yaml");
    std::fs::set_permissions(&template, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(!run(Some(&actual)).status.success());
    std::fs::set_permissions(&template, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::rename(&template, root.join("foreign-template")).unwrap();
    std::os::unix::fs::symlink(root.join("foreign-template"), &template).unwrap();
    assert!(!run(Some(&actual)).status.success());
    let calls = std::fs::read_to_string(root.join("calls")).unwrap();
    assert!(
        !calls.contains(" apply ") && !calls.contains(" patch ") && !calls.contains(" delete ")
    );
    for call in calls
        .lines()
        .filter(|line| line.contains(" create --dry-run"))
    {
        assert!(call.ends_with(
            "create --dry-run=client --validate=false -f - -o json --request-timeout=20s"
        ));
        assert!(
            call.contains("docker run --rm -i "),
            "stdin not carried: {call}"
        );
    }
    assert!(
        calls
            .lines()
            .filter(|call| call.contains(" get "))
            .all(|call| !call.contains(" -i "))
    );
}
