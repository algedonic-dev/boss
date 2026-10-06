//! An admission policy or binding that EXISTS but says something other
//! than the tree is DRIFT, not present. `infra/cluster/check-manifests-
//! applied.sh` is RUN against a fixture tree and a stub `kubectl` whose
//! live and declared objects are JSON files the test writes, so every
//! verdict below is one the script actually reached.
//!
//! WHY (backlog e4a9a9b3; reviews 89c716e0 and 87fc0e0c of the break-glass
//! image-only policy, N3). The check compared RBAC objects by content and
//! every other kind by existence alone, so the policy that narrows the
//! break-glass operator's patch, and the binding that decides whether it
//! reports or refuses, were green as long as both NAMES existed. A binding
//! hand-flipped from Warn/Audit to Deny — or back from Deny to Warn once
//! the Deny car lands — a `repositories` map edited to admit a foreign
//! image, or a binding-level matchResources narrowing what the policy
//! judges out of sight of its own matchConstraints: each is a change to
//! what the break-glass road may do, made outside the tree, and each read
//! green. Now both kinds are compared on their `spec`, with the fields the
//! API server DEFAULTS stripped from both sides at their default value, so
//! a round-trip through the API server is not drift and an edit is — named
//! down to the field.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::process::Command;

const CHECK: &str = "infra/cluster/check-manifests-applied.sh";

/// A fixture tree the renderer accepts (source prod + a playground, two
/// instance manifests, two pipeline manifests — one of them the admission
/// pair), and a stub kubectl that answers the existence read from a
/// `Kind/name/namespace` list, the JSON reads from files the test writes.
struct Case {
    root: PathBuf,
    tree: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("admission-drift-{name}"));
        let tree = root.join("tree");
        let dir = tree.join("infra/cluster/manifests");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(tree.join("examples/fixture/seeds")).unwrap();
        write_file(&tree.join("examples/fixture/seeds/tenant.toml"), "");
        write_file(
            &dir.join("boss.yaml"),
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: boss\n---\n\
             apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss\n  namespace: boss\n\
             spec:\n  template:\n    spec:\n      containers:\n      - name: boss\n        env:\n\
             \x20       - {name: BOSS_SIM_ENABLED, value: \"false\"}\n\
             \x20       - {name: BOSS_GUEST_ACCESS, value: \"0\"}\n\
             \x20       - {name: BOSS_TENANT_DIR, value: /opt/boss/examples/fixture}\n\
             \x20       - {name: BOSS_PUBLIC_URL, value: \"https://boss.algedonic.dev\"}\n\
             ---\napiVersion: v1\nkind: Service\nmetadata:\n  name: boss-gateway\n  namespace: boss\n",
        );
        write_file(
            &dir.join("boss-jobs-internal.yaml"),
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: boss-jobs-internal\n  namespace: boss\n",
        );
        write_file(
            &dir.join("boss-conductor.yaml"),
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: boss-conductor\n  namespace: boss-dev\n",
        );
        // The stub reads kind and name off this file; what the tree
        // DECLARES for each object is the JSON beside it (want_admission).
        write_file(
            &dir.join("admission.yaml"),
            "apiVersion: admissionregistration.k8s.io/v1\nkind: ValidatingAdmissionPolicy\n\
             metadata:\n  name: image-only\n---\n\
             apiVersion: admissionregistration.k8s.io/v1\nkind: ValidatingAdmissionPolicyBinding\n\
             metadata:\n  name: image-only\n",
        );
        // And one RBAC object, whose content read goes through the same
        // helper and had the same false clean (review d133e704, M1).
        write_file(
            &dir.join("rbac.yaml"),
            "apiVersion: rbac.authorization.k8s.io/v1\nkind: Role\n\
             metadata:\n  name: reader\n  namespace: boss\n",
        );
        write_file(
            &tree.join("infra/cluster/instance-manifests.txt"),
            "boss.yaml instance\nboss-jobs-internal.yaml instance\nboss-conductor.yaml pipeline\n\
             admission.yaml pipeline\nrbac.yaml pipeline\n",
        );
        write_file(
            &tree.join("infra/cluster/instances.toml"),
            "source = \"prod\"\n\n[prod]\nnamespace = \"boss\"\ntenant_dir = \"examples/fixture\"\n\
             sim = false\nhostname = \"boss.algedonic.dev\"\nguest = false\n\n[playground]\nnamespace = \"boss-playground\"\n\
             tenant_dir = \"examples/fixture\"\nsim = true\nhostname = \"playground.algedonic.dev\"\nguest = \"audit\"\n",
        );
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        write_exec(
            &bin.join("kubectl"),
            r#"#!/usr/bin/env bash
case "$1" in
  version) exit 0 ;;
  create)
    f="${@: -1}"
    case " $* " in
      *" -o json "*)
        cat "$STUB_WANT/${f##*/}.json" 2>/dev/null || echo '{}'
        exit 0 ;;
    esac
    awk 'function flush() { if (kind != "") printf "%s\t%s\t%s\n", kind, name, ns; kind = ""; name = ""; ns = ""; meta = 0 }
         /^---/ { flush(); next }
         /^kind: / { kind = $2 }
         /^metadata:$/ { meta = 1; next }
         /^[^ ]/ { meta = 0 }
         meta && /^  name: / { name = $2 }
         meta && /^  namespace: / { ns = $2 }
         END { flush() }' "$f"
    exit 0 ;;
  get)
    kind="$2"; name="$3"; ns=""; json=0
    shift 3
    while [ $# -gt 0 ]; do
      case "$1" in -n) ns="$2" ;; -o) [ "$2" = json ] && json=1 ;; esac
      shift
    done
    if ! grep -qx -- "$kind/$name/$ns" "$STUB_PRESENT"; then
      echo "Error from server (NotFound): $kind \"$name\" not found" >&2; exit 1
    fi
    [ "$json" = 1 ] || { echo "NAME"; exit 0; }
    # The CONTENT read fails the way kubectl's does (review d133e704):
    # a timeout marker answers an error and exit 1, and a live file
    # that is missing is cat's own exit 1 — never a quiet exit 0.
    if [ -e "$STUB_LIVE/$kind-$name.timeout" ]; then
      echo "Unable to connect to the server: net/http: request canceled (Client.Timeout exceeded while awaiting headers)" >&2
      exit 1
    fi
    exec cat "$STUB_LIVE/$kind-$name.json" ;;
esac
exit 0
"#,
        );
        std::fs::create_dir_all(root.join("want")).unwrap();
        std::fs::create_dir_all(root.join("live")).unwrap();
        let mut present = vec!["Deployment/boss-conductor/boss-dev".to_string()];
        for ns in ["boss", "boss-playground"] {
            present.push(format!("Namespace/{ns}/"));
            present.push(format!("Deployment/boss/{ns}"));
            present.push(format!("Service/boss-gateway/{ns}"));
            present.push(format!("Service/boss-jobs-internal/{ns}"));
        }
        present.push("ValidatingAdmissionPolicy/image-only/".to_string());
        present.push("ValidatingAdmissionPolicyBinding/image-only/".to_string());
        present.push("Role/reader/boss".to_string());
        write_file(&root.join("present"), &format!("{}\n", present.join("\n")));
        let c = Self { root, tree };
        // What the tree declares: kubectl's client dry run prints the
        // file's documents concatenated, with nothing defaulted.
        let want = format!("{}\n{}\n", declared_policy(), declared_binding());
        write_file(&c.root.join("want/admission.yaml.json"), &want);
        write_file(
            &c.root.join("want/rbac.yaml.json"),
            &declared_role().to_string(),
        );
        // The Role is served as declared unless a test says otherwise.
        write_file(
            &c.root.join("live/Role-reader.json"),
            &served(declared_role()).to_string(),
        );
        c
    }

    /// Make one object's CONTENT read time out, while its existence read
    /// still succeeds — the shape review d133e704 reproduced.
    fn content_read_times_out(&self, kind: &str, name: &str) {
        write_file(&self.root.join(format!("live/{kind}-{name}.timeout")), "");
    }

    /// The live pair, as the API server hands it back.
    fn live(&self, policy: &Json, binding: &Json) {
        write_file(
            &self
                .root
                .join("live/ValidatingAdmissionPolicy-image-only.json"),
            &policy.to_string(),
        );
        write_file(
            &self
                .root
                .join("live/ValidatingAdmissionPolicyBinding-image-only.json"),
            &binding.to_string(),
        );
    }

    fn run(&self) -> (i32, String, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(CHECK))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("BOSS_CLUSTER_TREE", &self.tree)
            .env("STUB_PRESENT", self.root.join("present"))
            .env("STUB_WANT", self.root.join("want"))
            .env("STUB_LIVE", self.root.join("live"))
            .env_remove("BOSS_INSTANCES_SKIPPED")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

fn declared_policy() -> Json {
    json!({
        "apiVersion": "admissionregistration.k8s.io/v1",
        "kind": "ValidatingAdmissionPolicy",
        "metadata": {"name": "image-only"},
        "spec": {
            "failurePolicy": "Fail",
            "matchConstraints": {"resourceRules": [{
                "apiGroups": ["apps"], "apiVersions": ["*"],
                "operations": ["UPDATE"], "resources": ["deployments", "statefulsets"]
            }]},
            "matchConditions": [{"name": "only-the-operator", "expression": "true"}],
            "variables": [{"name": "repositories",
                           "expression": "{'Deployment/boss': 'forge/david/boss'}\n"}],
            "validations": [{"expression": "variables.repository != ''\n", "message": "m"}]
        }
    })
}

fn declared_binding() -> Json {
    json!({
        "apiVersion": "admissionregistration.k8s.io/v1",
        "kind": "ValidatingAdmissionPolicyBinding",
        "metadata": {"name": "image-only"},
        "spec": {"policyName": "image-only", "validationActions": ["Warn", "Audit"]}
    })
}

fn declared_role() -> Json {
    json!({
        "apiVersion": "rbac.authorization.k8s.io/v1",
        "kind": "Role",
        "metadata": {"name": "reader", "namespace": "boss"},
        "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["get"]}]
    })
}

/// The declared object after a round-trip through the API server: the
/// server's own metadata and status, and the defaults it fills in —
/// matchPolicy, both selectors, each rule's scope. None of it is drift.
fn served(mut obj: Json) -> Json {
    obj["metadata"]["uid"] = json!("00000000-0000-0000-0000-000000000001");
    obj["metadata"]["resourceVersion"] = json!("4242");
    obj["metadata"]["generation"] = json!(1);
    if obj["kind"] == "ValidatingAdmissionPolicy" {
        let m = &mut obj["spec"]["matchConstraints"];
        m["matchPolicy"] = json!("Equivalent");
        m["namespaceSelector"] = json!({});
        m["objectSelector"] = json!({});
        m["resourceRules"][0]["scope"] = json!("*");
        obj["status"] = json!({"observedGeneration": 1, "typeChecking": {}});
    }
    obj
}

fn summary(out: &str) -> String {
    out.lines()
        .rfind(|l| l.starts_with("check-manifests-applied: "))
        .unwrap_or_else(|| panic!("the check prints its one-line summary: {out}"))
        .to_string()
}

#[test]
fn the_pair_as_the_api_server_serves_it_is_clean() {
    let c = Case::new("clean");
    c.live(&served(declared_policy()), &served(declared_binding()));
    let (rc, out, err) = c.run();
    assert_eq!(
        rc, 0,
        "server metadata, status and defaults are not drift: {out}\n{err}"
    );
    assert_eq!(
        summary(&out),
        "check-manifests-applied: 12 present, 0 missing, 0 skipped, 0 drifted, 0 unreadable (of 12)"
    );
}

#[test]
fn a_binding_flipped_to_deny_by_hand_is_drift_and_names_the_field() {
    let c = Case::new("flipped");
    let mut binding = served(declared_binding());
    binding["spec"]["validationActions"] = json!(["Deny", "Audit"]);
    c.live(&served(declared_policy()), &binding);
    let (rc, out, err) = c.run();
    assert_eq!(
        rc, 1,
        "a binding that says something else is drift: {out}\n{err}"
    );
    assert!(
        err.contains(
            "DRIFT   ValidatingAdmissionPolicyBinding/image-only — spec.validationActions differs"
        ),
        "the drift names the object AND the field: {err}"
    );
    assert!(summary(&out).contains(" 1 drifted,"), "{out}");
}

#[test]
fn a_repositories_map_edited_live_is_drift() {
    let c = Case::new("repositories");
    let mut policy = served(declared_policy());
    policy["spec"]["variables"][0]["expression"] =
        json!("{'Deployment/boss': 'attacker.example/boss'}\n");
    c.live(&policy, &served(declared_binding()));
    let (rc, out, err) = c.run();
    assert_eq!(rc, 1, "{out}\n{err}");
    assert!(
        err.contains("DRIFT   ValidatingAdmissionPolicy/image-only — spec.variables differs"),
        "{err}"
    );
}

#[test]
fn a_narrowing_the_tree_never_declared_is_drift() {
    let c = Case::new("narrowed");
    let mut binding = served(declared_binding());
    binding["spec"]["matchResources"] = json!({
        "matchPolicy": "Equivalent",
        "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "nowhere"}},
        "objectSelector": {}
    });
    let mut policy = served(declared_policy());
    policy["spec"]["failurePolicy"] = json!("Ignore");
    c.live(&policy, &binding);
    let (rc, out, err) = c.run();
    assert_eq!(rc, 1, "{out}\n{err}");
    assert!(
        err.contains("ValidatingAdmissionPolicyBinding/image-only — spec.matchResources differs"),
        "a binding-level narrowing is named: {err}"
    );
    assert!(
        err.contains("ValidatingAdmissionPolicy/image-only — spec.failurePolicy differs"),
        "a policy moved to fail OPEN is named, though Fail is also the default: {err}"
    );
    assert!(summary(&out).contains(" 2 drifted,"), "{out}");
}

// --- review d133e704, M1: a content read that fails is UNREADABLE --------
//
// The existence read has already succeeded when the content read runs, so
// nothing else will count a failure there. Until this fold the helper
// exited 0 on an unreadable live or declared object, died with a traceback
// on a body that was not JSON, and the caller ignored its status — each
// counted `present` with no drift, the run exited 0, and the converge
// recorded a verified-clean line for a check that read nothing.

/// A weaker live binding (Audit dropped) whose content read fails must
/// not read clean: exit 2, counted unreadable and named.
fn assert_unreadable(rc: i32, out: &str, err: &str, object: &str) {
    assert_eq!(
        rc, 2,
        "a content read that failed is 'unknown', never clean: {out}\n{err}"
    );
    assert!(
        summary(out).contains(" 0 drifted, 1 unreadable "),
        "it is counted unreadable, not present: {out}"
    );
    let all = format!("{out}\n{err}");
    assert!(
        all.contains(&format!(
            "{object} — present, but its content could not be read"
        )),
        "the object and the reason are named: {all}"
    );
}

#[test]
fn a_content_read_that_times_out_is_unreadable_not_clean() {
    let c = Case::new("timeout");
    let mut binding = served(declared_binding());
    binding["spec"]["validationActions"] = json!(["Warn"]);
    c.live(&served(declared_policy()), &binding);
    c.content_read_times_out("ValidatingAdmissionPolicyBinding", "image-only");
    let (rc, out, err) = c.run();
    assert_unreadable(
        rc,
        &out,
        &err,
        "ValidatingAdmissionPolicyBinding/image-only",
    );
    assert!(
        format!("{out}\n{err}").contains("Client.Timeout exceeded"),
        "kubectl's own words ride the line: {out}\n{err}"
    );
}

#[test]
fn a_live_object_the_content_read_cannot_find_is_unreadable() {
    let c = Case::new("no-live-file");
    // Only the binding is served; the policy's content read gets nothing.
    write_file(
        &c.root
            .join("live/ValidatingAdmissionPolicyBinding-image-only.json"),
        &served(declared_binding()).to_string(),
    );
    let (rc, out, err) = c.run();
    assert_unreadable(rc, &out, &err, "ValidatingAdmissionPolicy/image-only");
}

#[test]
fn an_empty_content_body_is_unreadable() {
    let c = Case::new("empty-body");
    c.live(&served(declared_policy()), &json!(null));
    write_file(
        &c.root
            .join("live/ValidatingAdmissionPolicyBinding-image-only.json"),
        "",
    );
    let (rc, out, err) = c.run();
    assert_unreadable(
        rc,
        &out,
        &err,
        "ValidatingAdmissionPolicyBinding/image-only",
    );
}

#[test]
fn an_rbac_content_read_that_times_out_is_unreadable() {
    let c = Case::new("rbac-timeout");
    c.live(&served(declared_policy()), &served(declared_binding()));
    c.content_read_times_out("Role", "reader");
    let (rc, out, err) = c.run();
    assert_unreadable(rc, &out, &err, "Role/reader (ns boss)");
}

#[test]
fn a_tree_object_the_dry_run_cannot_render_is_unreadable() {
    let c = Case::new("no-want");
    c.live(&served(declared_policy()), &served(declared_binding()));
    // The stub's dry run answers `{}` for a file it has no JSON for: no
    // document is the Role, so what the tree declares is unknown.
    std::fs::remove_file(c.root.join("want/rbac.yaml.json")).unwrap();
    let (rc, out, err) = c.run();
    assert_unreadable(rc, &out, &err, "Role/reader (ns boss)");
}
