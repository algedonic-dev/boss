//! Run the converge's READ adapter and renderer with private, nonsecret
//! Kubernetes fixture ports. No credential or real cluster is touched.
use boss_testing::{repo_root, scratch_dir, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-deposit.yaml";
const PIN: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

struct Fixture {
    dir: PathBuf,
    cm: Value,
    cron: Value,
}

impl Fixture {
    fn legacy(case: &str, host: &str, account: &str, server: &str) -> Self {
        let dir = scratch_dir(&format!("deposit-config-{case}"));
        let cm = json!({"apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":"break-glass-deposit-known-hosts","namespace":"boss",
                "uid":"cm-identity","resourceVersion":"17"},
            "data":{"known_hosts":format!("{host} {PIN}\n")}});
        let cron = json!({"apiVersion":"batch/v1","kind":"CronJob",
        "metadata":{"name":"boss-break-glass-deposit","namespace":"boss",
            "uid":"cron-identity","resourceVersion":"23"},
        "spec":{"jobTemplate":{"spec":{"template":{"spec":{"containers":[{
            "name":"chore","env":[
                {"name":"BOSS_DEPOSIT_API_SERVER","value":server},
                {"name":"BOSS_DEPOSIT_TARGET","value":format!("{account}@{host}")}
            ]}]}}}}}});
        Self { dir, cm, cron }
    }

    fn stable(&mut self) {
        let env = &mut self.cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"]
            [0]["env"];
        for (index, key) in [(0, "api_server"), (1, "target")] {
            self.cm["data"][key] = env[index]["value"].take();
            env[index] = json!({"name":env[index]["name"],"valueFrom":{"configMapKeyRef":{
                "name":"break-glass-deposit-known-hosts","key":key}}});
        }
    }

    fn run(
        &self,
        cm_raw: Option<&str>,
        cron_raw: Option<&str>,
        read_failure: bool,
    ) -> (i32, String) {
        write_file(
            &self.dir.join("cm.json"),
            cm_raw.unwrap_or(&self.cm.to_string()),
        );
        write_file(
            &self.dir.join("cron.json"),
            cron_raw.unwrap_or(&self.cron.to_string()),
        );
        write_file(
            &self.dir.join("reader"),
            &format!(
                r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> '{d}/reads'
[ '{fail}' = false ] || exit 8
case "$*" in
  'get configmap break-glass-deposit-known-hosts -n boss -o json') cat '{d}/cm.json' ;;
  'get cronjob boss-break-glass-deposit -n boss -o json') cat '{d}/cron.json' ;;
  *) echo 'unexpected read' >&2; exit 9 ;;
esac
"#,
                d = self.dir.display(),
                fail = read_failure
            ),
        );
        write_file(
            &self.dir.join("run.sh"),
            &format!(
                r#"#!/bin/bash
set -euo pipefail
. '{root}/infra/forge/cluster-deploy-lib.sh'
deposit_config_render 'bash {d}/reader' boss '{root}/{manifest}' '{d}/rendered.yaml' '{root}/infra/cluster/render-deposit-config.sh' '{d}/receipt.json'
"#,
                root = repo_root().display(),
                d = self.dir.display(),
                manifest = MANIFEST
            ),
        );
        let output = Command::new("bash")
            .arg(self.dir.join("run.sh"))
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn rendered(&self) -> String {
        std::fs::read_to_string(self.dir.join("rendered.yaml")).unwrap()
    }

    fn assert_refused(&self, raw_cm: Option<&str>, raw_cron: Option<&str>, read_failure: bool) {
        let (status, error) = self.run(raw_cm, raw_cron, read_failure);
        assert_ne!(status, 0, "accepted unread or invalid deployment inputs");
        assert!(error.contains("deposit-config"), "{error}");
        assert!(
            !self.dir.join("rendered.yaml").exists(),
            "partial render escaped refusal"
        );
        assert!(
            !self.dir.join("receipt.json").exists(),
            "positive receipt escaped refusal"
        );
    }
}

#[test]
fn legacy_inputs_migrate_once_and_stable_replay_preserves_the_exact_data() {
    let mut f = Fixture::legacy(
        "legacy",
        "receiver.example",
        "operator",
        "https://api.example:6443",
    );
    let (status, error) = f.run(None, None, false);
    assert_eq!(status, 0, "{error}");
    let before = f.rendered();
    let cm: Value = serde_json::from_str(
        before
            .lines()
            .find_map(|line| line.strip_prefix("data: {").map(|rest| format!("{{{rest}")))
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cm["known_hosts"], f.cm["data"]["known_hosts"]);
    assert_eq!(cm["target"], "operator@receiver.example");
    assert_eq!(cm["api_server"], "https://api.example:6443");
    let receipt: Value =
        serde_json::from_str(&std::fs::read_to_string(f.dir.join("receipt.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["mode"], "legacy-migration");
    assert_eq!(receipt["config_map"]["uid"], "cm-identity");
    assert_eq!(receipt["cronjob"]["resource_version"], "23");
    assert_eq!(
        receipt["data"], cm,
        "the nonsecret deployment intent is retained, not just its hash"
    );
    assert!(before.contains("  uid: \"cm-identity\"\n  resourceVersion: \"17\""));
    assert!(before.contains("  uid: \"cron-identity\"\n  resourceVersion: \"23\""));
    f.stable();
    let (status, error) = f.run(None, None, false);
    assert_eq!(status, 0, "{error}");
    assert_eq!(
        before,
        f.rendered(),
        "rereading the deployment changed its data"
    );
    let receipt: Value =
        serde_json::from_str(&std::fs::read_to_string(f.dir.join("receipt.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["mode"], "config-map");
}

#[test]
fn another_deployment_uses_only_its_own_endpoint_account_and_pin() {
    let f = Fixture::legacy(
        "second",
        "other.example",
        "recovery",
        "https://other-api.example:7443",
    );
    let (status, error) = f.run(None, None, false);
    assert_eq!(status, 0, "{error}");
    let text = std::fs::read_to_string(f.dir.join("rendered.yaml")).unwrap();
    assert!(
        text.contains("recovery@other.example") && text.contains("https://other-api.example:7443")
    );
    assert!(!text.contains("receiver.example") && !text.contains("api.example:6443"));
    assert!(
        !text.contains("david@")
            && !text.contains("34.45.110.40")
            && !text.contains("https://10.20.0.10:6443")
    );
}

#[test]
fn unread_silent_malformed_and_multiple_envelopes_leave_nothing_to_apply() {
    for (i, raw) in ["", "  \n", "garbage", "null", "{}\n{}"].iter().enumerate() {
        let f = Fixture::legacy(
            &format!("bad-envelope-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        f.assert_refused(Some(raw), None, false);
        f.assert_refused(None, Some(raw), false);
    }
    let f = Fixture::legacy(
        "read-failed",
        "receiver.example",
        "operator",
        "https://api.example:6443",
    );
    f.assert_refused(None, None, true);
}

#[test]
fn wrong_missing_or_deleting_object_identity_refuses() {
    for (i, (field, value)) in [
        ("name", json!("foreign")),
        ("namespace", json!("elsewhere")),
        ("uid", Value::Null),
        ("uid", json!(" ")),
        ("resourceVersion", json!("")),
        ("resourceVersion", json!(" ")),
        ("deletionTimestamp", json!("2026-10-03T00:00:00Z")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut f = Fixture::legacy(
            &format!("cm-identity-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        f.cm["metadata"][field] = value.clone();
        f.assert_refused(None, None, false);
        let mut f = Fixture::legacy(
            &format!("cron-identity-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        f.cron["metadata"][field] = value;
        f.assert_refused(None, None, false);
    }
}

#[test]
fn malformed_values_and_pin_mismatches_refuse_without_shell_interpretation() {
    for (i, value) in [
        json!(null),
        json!(false),
        json!(""),
        json!("http://api.example:6443"),
        json!("https://user@api.example:6443"),
        json!("https://api.example:0"),
        json!("https://api.example:99999"),
        json!("https://api.example:6443/foreign"),
        json!("https://$(id).example:6443"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut f = Fixture::legacy(
            &format!("server-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        f.cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"] =
            value;
        f.assert_refused(None, None, false);
    }
    for (i, value) in [
        "foreign.example ssh-ed25519 AAAA\n",
        "receiver.example ssh-rsa AAAA\n",
        "receiver.example ssh-ed25519 invalid\n",
        &format!("receiver.example {PIN}\nreceiver.example {PIN}\n"),
        &format!("receiver.example {PIN}\n#extra"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut f = Fixture::legacy(
            &format!("pin-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        f.cm["data"]["known_hosts"] = json!(value);
        f.assert_refused(None, None, false);
    }
}

#[test]
fn partial_or_ambiguous_env_sources_and_containers_refuse() {
    for i in 0..6 {
        let mut f = Fixture::legacy(
            &format!("ambiguous-{i}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        let containers =
            &mut f.cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"];
        match i {
            0 => {
                let duplicate = containers[0].clone();
                containers.as_array_mut().unwrap().push(duplicate);
            }
            1 => {
                let duplicate = containers[0]["env"][0].clone();
                containers[0]["env"].as_array_mut().unwrap().push(duplicate);
            }
            2 => {
                containers[0]["env"][0]["valueFrom"] =
                    json!({"secretKeyRef":{"name":"not-authorized","key":"server"}})
            }
            3 => {
                containers[0]["env"].as_array_mut().unwrap().remove(0);
            }
            4 => {
                f.cm["data"]["target"] = json!("operator@receiver.example");
            }
            _ => {
                f.stable();
                f.cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"]
                    [0]["valueFrom"]["configMapKeyRef"]["name"] = json!("foreign");
            }
        }
        f.assert_refused(None, None, false);
    }
}

#[test]
fn the_generic_template_preserves_security_and_is_migrated_before_any_apply() {
    let m = std::fs::read_to_string(repo_root().join(MANIFEST)).unwrap();
    assert!(
        m.contains("configMapKeyRef: {name: break-glass-deposit-known-hosts, key: api_server}")
    );
    assert!(m.contains("configMapKeyRef: {name: break-glass-deposit-known-hosts, key: target}"));
    assert!(
        !m.contains("david@")
            && !m.contains("34.45.110.40")
            && !m.contains("https://10.20.0.10:6443")
    );
    for security in [
        "resourceNames: [break-glass-operator-token]",
        "resourceNames: [break-glass-deposit-key]",
        "runAsNonRoot: true",
        "runAsUser: 1500",
        "fsGroup: 1500",
        "emptyDir: {medium: Memory",
        "secretName: break-glass-deposit-key, defaultMode: 0o440",
    ] {
        assert!(m.contains(security), "lost {security}");
    }
    let runner =
        std::fs::read_to_string(repo_root().join("infra/forge/cluster-deploy-runner.sh")).unwrap();
    let migration = runner
        .find("deposit_config_render ")
        .expect("production read adapter mounted");
    let stage = runner.find("manifests_with_image \"$RENDER_DIR").unwrap();
    assert!(
        migration < stage,
        "generic template reached image/apply staging before required migration"
    );
    assert!(
        runner.contains("run_summary_json deposit_config"),
        "migration evidence lost before durable record"
    );
}

#[test]
fn interrupted_migration_retries_only_when_both_authoritative_inputs_still_agree() {
    let mut f = Fixture::legacy(
        "resume",
        "receiver.example",
        "operator",
        "https://api.example:6443",
    );
    f.cm["data"]["target"] = json!("operator@receiver.example");
    f.cm["data"]["api_server"] = json!("https://api.example:6443");
    let (status, error) = f.run(None, None, false);
    assert_eq!(
        status, 0,
        "the safe interrupted migration must resume: {error}"
    );
    let mut f = Fixture::legacy(
        "resume-disagreement",
        "receiver.example",
        "operator",
        "https://api.example:6443",
    );
    f.cm["data"]["target"] = json!("another@receiver.example");
    f.cm["data"]["api_server"] = json!("https://api.example:6443");
    f.assert_refused(None, None, false);
}

#[test]
fn actual_write_port_receives_conditional_patches_and_keeps_both_objects_out_of_plain_apply() {
    for failed in [
        "none",
        "stable",
        "configmap",
        "cronjob",
        "cm-silent",
        "cm-duplicate",
        "cron-silent",
        "cron-duplicate",
        "cron-command",
        "cron-security",
        "cron-labels",
        "cron-deleting",
        "cron-invalid-version",
        "cm-deleting",
        "cm-invalid-version",
        "defaults",
        "cron-foreign-default",
    ] {
        let mut f = Fixture::legacy(
            &format!("conditional-{failed}"),
            "receiver.example",
            "operator",
            "https://api.example:6443",
        );
        if failed == "stable" {
            f.stable();
        }
        assert_eq!(f.run(None, None, false).0, 0);
        let mut desired = f.cron.clone();
        desired["spec"]["schedule"] = json!("20 17 * * *");
        desired["metadata"]["labels"] = json!({"boss-chore":"true"});
        desired["spec"]["jobTemplate"]["spec"]["template"]["spec"]["securityContext"] =
            json!({"runAsNonRoot":true,"runAsUser":1500});
        desired["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["command"] =
            json!(["/bin/bash", "-c"]);
        desired["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["image"] =
            json!("registry.example/boss:tested");
        desired["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"] = json!([
            {"name":"BOSS_DEPOSIT_API_SERVER","valueFrom":{"configMapKeyRef":{"name":"break-glass-deposit-known-hosts","key":"api_server"}}},
            {"name":"BOSS_DEPOSIT_TARGET","valueFrom":{"configMapKeyRef":{"name":"break-glass-deposit-known-hosts","key":"target"}}}
        ]);
        write_file(
            &f.dir.join("template.json"),
            &json!({"apiVersion":"v1","kind":"List","items":[desired]}).to_string(),
        );
        write_file(
            &f.dir.join("writer"),
            &format!(
                r#"#!/bin/bash
set -euo pipefail
[ "$1" = patch ] && [ "$4 $5 $6 $7 $8 $9" = '-n boss --type=json --patch-file=/dev/stdin -o json' ] || exit 99
cat > '{d}/patch-'"$2"'.json'
[ '{failed}' != "$2" ] || {{ echo 'private fixture: conditional conflict' >&2; exit 1; }}
if [ "$2" = configmap ]; then
  [ '{failed}' != cm-silent ] || exit 0
else
  [ '{failed}' != cron-silent ] || exit 0
fi
if [ "$2" = configmap ]; then
  jq --arg failed '{failed}' --slurpfile patch '{d}/patch-configmap.json' '.data = ($patch[0][] | select(.path == "/data") | .value) | .metadata.resourceVersion = "18"
    | if $failed == "cm-deleting" then .metadata.deletionTimestamp = "2026-10-03T00:00:00Z" elif $failed == "cm-invalid-version" then .metadata.resourceVersion = "bad" else . end' '{d}/cm.json'
  [ '{failed}' != cm-duplicate ] || cat '{d}/cm.json'
else
  jq --arg failed '{failed}' '.items[0] | .metadata.resourceVersion = "24"
    | if $failed == "cron-command" then .spec.jobTemplate.spec.template.spec.containers[0].command = ["/bin/false"]
      elif $failed == "cron-security" then del(.spec.jobTemplate.spec.template.spec.securityContext)
      elif $failed == "cron-labels" then del(.metadata.labels)
      elif $failed == "cron-deleting" then .metadata.deletionTimestamp = "2026-10-03T00:00:00Z"
      elif $failed == "cron-invalid-version" then .metadata.resourceVersion = "bad"
      elif $failed == "cron-foreign-default" then .spec.jobTemplate.spec.template.spec.dnsPolicy = "None"
      elif $failed == "defaults" then .spec.suspend = false | .spec.jobTemplate.spec.parallelism = 1
        | .spec.jobTemplate.spec.template.metadata = {{creationTimestamp:null}}
        | .spec.jobTemplate.spec.template.spec.dnsPolicy = "ClusterFirst"
        | .spec.jobTemplate.spec.template.spec.serviceAccountName = "default"
        | .spec.jobTemplate.spec.template.spec.serviceAccount = "default"
        | .spec.jobTemplate.spec.template.spec.containers[0].terminationMessagePolicy = "File"
        | .spec.jobTemplate.spec.template.spec.containers[0].terminationMessagePath = "/dev/termination-log"
        | .spec.jobTemplate.spec.template.spec.containers[0].imagePullPolicy = "IfNotPresent"
      else . end' '{d}/template.json'
  [ '{failed}' != cron-duplicate ] || jq '.items[0]' '{d}/template.json'
fi
"#,
                d = f.dir.display()
            ),
        );
        write_file(
            &f.dir.join("apply.sh"),
            &format!(
                r#"#!/bin/bash
set -euo pipefail
. '{root}/infra/forge/cluster-deploy-lib.sh'
deposit_config_apply 'bash {d}/writer' boss '{d}/rendered.yaml' '{d}/template.json' '{d}/receipt.json'
"#,
                root = repo_root().display(),
                d = f.dir.display()
            ),
        );
        let out = Command::new("bash")
            .arg(f.dir.join("apply.sh"))
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            failed == "none" || failed == "stable" || failed == "defaults",
            "{failed}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if failed == "stable" {
            assert!(
                !f.dir.join("patch-configmap.json").exists(),
                "stable config must never be rewritten"
            );
        } else {
            let cm_patch: Value = serde_json::from_str(
                &std::fs::read_to_string(f.dir.join("patch-configmap.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                cm_patch[0],
                json!({"op":"test","path":"/metadata/uid","value":"cm-identity"})
            );
            assert_eq!(
                cm_patch[1],
                json!({"op":"test","path":"/metadata/resourceVersion","value":"17"})
            );
        }
        if failed != "configmap" && !failed.starts_with("cm-") {
            let cron_patch: Value = serde_json::from_str(
                &std::fs::read_to_string(f.dir.join("patch-cronjob.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                cron_patch[0],
                json!({"op":"test","path":"/metadata/uid","value":"cron-identity"})
            );
            assert_eq!(
                cron_patch[1],
                json!({"op":"test","path":"/metadata/resourceVersion","value":"23"})
            );
        } else {
            assert!(!f.dir.join("patch-cronjob.json").exists());
        }
        let receipt: Value =
            serde_json::from_str(&std::fs::read_to_string(f.dir.join("receipt.json")).unwrap())
                .unwrap();
        if failed == "none" || failed == "stable" || failed == "defaults" {
            let rendered = f.rendered();
            assert!(
                !rendered.contains("\nkind: ConfigMap\n")
                    && !rendered.contains("\nkind: CronJob\n")
            );
            assert!(rendered.contains("\nkind: Secret\n") && rendered.contains("\nkind: Role\n"));
            assert_eq!(receipt["phase"], "applied-conditionally");
        } else {
            assert_ne!(receipt["phase"], "applied-conditionally");
            assert!(
                f.rendered().contains("\nkind: CronJob\n"),
                "failure must stop before plain-apply staging"
            );
        }
    }
}

#[test]
fn client_conversion_counts_the_document_sequence_instead_of_ignoring_its_tail() {
    let f = Fixture::legacy(
        "typed-producer",
        "receiver.example",
        "operator",
        "https://api.example:6443",
    );
    write_file(
        &f.dir.join("template.yaml"),
        "kind: ConfigMap\n---\nkind: CronJob\n",
    );
    let valid = format!("{}\n{}\n", f.cm, f.cron);
    for (i, raw) in [
        &valid,
        "",
        "{}",
        "{}\n{}",
        "null\nnull",
        &format!("{valid}{}", f.cm),
    ]
    .into_iter()
    .enumerate()
    {
        write_file(&f.dir.join("documents.json"), raw);
        let output = f.dir.join(format!("source-{i}.json"));
        write_file(
            &f.dir.join("convert.sh"),
            &format!(
                r#"#!/bin/bash
set -euo pipefail
. '{root}/infra/forge/cluster-deploy-lib.sh'
deposit_config_source '{d}/template.yaml' '{d}/documents.json' '{output}'
"#,
                root = repo_root().display(),
                d = f.dir.display(),
                output = output.display()
            ),
        );
        let out = Command::new("bash")
            .arg(f.dir.join("convert.sh"))
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            i == 0,
            "typed producer case{i}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if i == 0 {
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
            assert_eq!(value["kind"], "List");
            assert_eq!(value["items"], json!([f.cm, f.cron]));
        } else {
            assert!(!output.exists(), "partial conversion escaped");
        }
    }
}
