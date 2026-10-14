//! THE JOB A LAUNCHED AGENT RUNS IN — its contract, before the verb
//! that creates it (design 21062f95, answered by David 2026-10-10T23:31Z;
//! backlog 3869107c, residue of 167a877c).
//!
//! WHY. `boss dispatch --launch` ([`crate::dispatch_launch`]) spawns the
//! harness as a process of the SAME Unix account as the session that
//! launched it, so every file that account can read — the real home, the
//! machine-token mount, the other harness's login, every worktree — is in
//! reach of any command the model runs, and the time bound is a tokio
//! sleep in a verb that can be suspended or killed. The design's answers:
//! - Q1: the launched agent runs as a Job of its own in boss-dev, on the
//!   isolated-build worker's posture (`infra/dev/dev-build.py`, design
//!   c395e62c): uid 65534 / gid 1500, no privilege, read-only root, not
//!   on a control-plane node.
//! - Q2: it holds none of the dev pod's credentials — no ServiceAccount
//!   token (`automountServiceAccountToken: false`), no machine token, no
//!   forge token — only the harness's login, mounted read-only, and a
//!   workspace of its own. Whatever the run records (its started receipt,
//!   its handback) the launching verb writes from what the process
//!   prints, so the agent needs no system-of-record credential at all.
//! - Q3: the cluster holds the bound — `activeDeadlineSeconds` is the
//!   verb's bound plus its grace, and `ttlSecondsAfterFinished` is short.
//!   A suspended or killed verb no longer extends a launch, and deleting
//!   the Job ends every process in the pod, whatever group it joined.
//!
//! ONE DIFFERENCE FROM THE BUILD WORKER, ON PURPOSE. That worker clones
//! its source in an init container that holds `forge-read`. A launched
//! agent's tree is the launching checkout at a named head, which the
//! verb can hand in as data through the exec that starts the harness, so
//! no forge token enters this pod at all — not even an init container's.
//! The pod's ONE Secret is the harness login.
//!
//! WHAT THIS MODULE IS: [`manifest`] renders the Job and
//! [`authority_error`] judges ANY manifest against the contract by
//! property — the validator does not trust the renderer, so a later edit
//! that adds a mount, a token or a host namespace is refused by name.
//! [`crate::launch_controller`] owns the selected Cluster lifecycle and
//! [`crate::launch_cluster`] its native transport. Selection remains
//! dormant until separately approved activation and authoritative
//! published image/login bindings exist. This contract/source path is
//! not evidence of a live launch or kernel isolation.

use serde_json::{Value, json};
use std::time::Duration;

/// Where the Job runs: the dev pod's namespace, whose Role already lets
/// the session create, watch, exec into and delete Jobs.
pub(crate) const NAMESPACE: &str = "boss-dev";

/// How long a finished Job is kept for a reader. Minutes, not the build
/// worker's hour: the launching verb reads everything it keeps before it
/// lets the Job go.
pub(crate) const TTL_SECONDS: u64 = 300;
// "Short" is minutes, never hours, and never zero (a Job gone at once
// leaves nothing to read): held at compile time.
const _: () = assert!(TTL_SECONDS > 0 && TTL_SECONDS <= 600);

/// Where the harness login is mounted, read-only.
pub(crate) const LOGIN_MOUNT: &str = "/harness/login";

/// Every Secret the dev pod mounts (`infra/cluster/manifests/boss-dev.yaml`,
/// held equal by a test). None of them can be a launched agent's login.
pub(crate) const DEV_POD_SECRETS: [&str; 5] = [
    "boss-dev-access-ssh-ca",
    "boss-dev-forge-token",
    "boss-dev-ssh-authorized-keys",
    "boss-machine-token",
    "forge-read",
];

/// The registry pull credential. The kubelet spends it to pull the
/// image; no container sees it.
const PULL_SECRET: &str = "forgejo-registry";

const LOGIN_VOLUME: &str = "harness-login";
// The existing worker posture, shared by rendering and independent
// judgement so a later renderer edit cannot silently remove its bounds.
const SCRATCH_VOLUMES: [(&str, &str); 1] = [("agent-tmp", "2Gi")];

/// One run, one retained workspace. No ownerReference to compute: a TTL
/// cannot garbage-collect original evidence before a trusted archive ack.
pub(crate) fn workspace_name(run_id: &str) -> String {
    format!("agent-evidence-{run_id}")
}

pub(crate) fn workspace_manifest(spec: &Spec) -> Value {
    json!({"apiVersion":"v1","kind":"PersistentVolumeClaim",
        "metadata":{"name":workspace_name(spec.run_id),"namespace":NAMESPACE,
            "labels":{"boss.dev/agent-run":spec.run_id,"boss.dev/purpose":"agent-evidence"}},
        "spec":{"accessModes":["ReadWriteOnce"],"volumeMode":"Filesystem",
            "storageClassName":"longhorn-dev-disposable","resources":{"requests":{"storage":"20Gi"}}}})
}

pub(crate) fn recovery_name(run: &str) -> String {
    format!("agent-recovery-{run}")
}

pub(crate) fn recovery_manifest(spec: &Spec) -> Result<Value, String> {
    let bounded = Spec {
        bound: Duration::from_secs(30),
        grace: Duration::from_secs(5),
        ..spec.clone()
    };
    let mut job = manifest(&bounded)?;
    job["metadata"]["name"] = json!(recovery_name(spec.run_id));
    job["metadata"]["labels"]["boss.dev/purpose"] = json!("agent-evidence-recovery");
    job["spec"]["template"]["metadata"]["labels"]["boss.dev/purpose"] =
        json!("agent-evidence-recovery");
    job["spec"]["template"]["spec"]["volumes"] = json!([
        {"name":"agent-tmp","emptyDir":{"sizeLimit":"2Gi"}},
        {"name":"workspace","persistentVolumeClaim":{"claimName":workspace_name(spec.run_id),"readOnly":true}},
    ]);
    job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"] = json!([
        {"name":"workspace","mountPath":"/workspace","readOnly":true},
        {"name":"agent-tmp","mountPath":"/tmp"},
    ]);
    if let Some(why) = recovery_authority_error(&job, spec) {
        return Err(why);
    }
    Ok(job)
}

/// Independent recovery judge: no login and no write into retained
/// evidence. Common privilege/resource/namespace rules have one judge.
pub(crate) fn recovery_authority_error(job: &Value, spec: &Spec) -> Option<String> {
    if job["metadata"]["name"] != recovery_name(spec.run_id)
        || job["metadata"]["labels"]["boss.dev/purpose"] != "agent-evidence-recovery"
        || job["spec"]["template"]["metadata"]["labels"]["boss.dev/purpose"]
            != "agent-evidence-recovery"
        || job["spec"]["template"]["spec"]["volumes"]
            != json!([
                {"name":"agent-tmp","emptyDir":{"sizeLimit":"2Gi"}},
                {"name":"workspace","persistentVolumeClaim":{"claimName":workspace_name(spec.run_id),"readOnly":true}},
            ])
        || job["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]
            != json!([
                {"name":"workspace","mountPath":"/workspace","readOnly":true},
                {"name":"agent-tmp","mountPath":"/tmp"},
            ])
    {
        return Some(
            "recovery identity or private read-only/no-login volume contract differs".into(),
        );
    }
    let mut common = job.clone();
    common["metadata"]["name"] = json!(name(spec.run_id));
    common["metadata"]["labels"]["boss.dev/purpose"] = json!("launched-agent");
    common["spec"]["template"]["metadata"]["labels"]["boss.dev/purpose"] = json!("launched-agent");
    let canonical = match manifest(&Spec {
        bound: Duration::from_secs(30),
        grace: Duration::from_secs(5),
        ..spec.clone()
    }) {
        Ok(value) => value,
        Err(why) => return Some(why),
    };
    common["spec"]["template"]["spec"]["volumes"] =
        canonical["spec"]["template"]["spec"]["volumes"].clone();
    common["spec"]["template"]["spec"]["containers"][0]["volumeMounts"] =
        canonical["spec"]["template"]["spec"]["containers"][0]["volumeMounts"].clone();
    authority_error(
        &common,
        &Spec {
            bound: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            ..spec.clone()
        },
    )
}

/// What one launch's Job is made from.
#[derive(Debug, Clone)]
pub(crate) struct Spec<'a> {
    /// The agent-run packet's id; the Job is named after it.
    pub run_id: &'a str,
    /// The harness image, pinned by digest.
    pub image: &'a str,
    /// The Secret holding the harness's login.
    pub login_secret: &'a str,
    /// The verb's own bound ([`crate::dispatch_launch::Request::bound`]).
    pub bound: Duration,
    /// The verb's grace ([`crate::dispatch_launch::Request::grace`]).
    pub grace: Duration,
}

/// The Job's name: the run's, so one is found from the other.
pub(crate) fn name(run_id: &str) -> String {
    format!("agent-run-{run_id}")
}

/// The bound plus the grace, in whole seconds, a part second counted as
/// one more — the cluster's deadline is never earlier than the verb's.
pub(crate) fn deadline_seconds(bound: Duration, grace: Duration) -> Result<u64, String> {
    let seconds = bound
        .checked_add(grace)
        .and_then(|total| {
            total
                .as_secs()
                .checked_add(u64::from(total.subsec_nanos() > 0))
        })
        .filter(|seconds| *seconds > 0 && i64::try_from(*seconds).is_ok());
    seconds
        .ok_or_else(|| "activeDeadlineSeconds is outside the positive signed 64-bit range".into())
}

fn affinity() -> Value {
    json!({"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
        "nodeSelectorTerms": [{"matchExpressions": [{
            "key": "node-role.kubernetes.io/control-plane", "operator": "DoesNotExist"}]}]}}})
}

fn resources() -> Value {
    json!({"requests": {"cpu": "500m", "memory": "1Gi"},
           "limits": {"cpu": "2", "memory": "4Gi"}})
}

/// The agent container's context: the isolated-build worker's.
fn security() -> Value {
    json!({
        "runAsNonRoot": true, "runAsUser": 65534, "runAsGroup": 1500,
        "allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true,
        "capabilities": {"drop": ["ALL"]}, "seccompProfile": {"type": "RuntimeDefault"},
    })
}

fn pod_security() -> Value {
    json!({"fsGroup": 1500, "fsGroupChangePolicy": "OnRootMismatch"})
}

/// Addresses and nothing else. No `BOSS_*` name: the agent signs nothing.
fn environment() -> Value {
    json!([
        {"name": "HOME", "value": "/workspace/home"},
        {"name": "TMPDIR", "value": "/workspace/tmp"},
        {"name": "PATH", "value": "/usr/local/bin:/usr/bin:/bin"},
        {"name":"AGENT_POD_UID","valueFrom":{"fieldRef":{"apiVersion":"v1","fieldPath":"metadata.uid"}}},
    ])
}

pub(crate) fn is_name(text: &str) -> bool {
    (1..=63).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !text.starts_with('-')
        && !text.ends_with('-')
}

pub(crate) fn is_pinned(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(repo, digest)| {
        !repo.is_empty()
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
            && digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The Job for one launch, or why the spec is not exact enough to make
/// one. Rendered and then judged by [`authority_error`], so nothing
/// leaves here that the contract refuses.
pub(crate) fn manifest(spec: &Spec) -> std::result::Result<Value, String> {
    let canonical = uuid::Uuid::parse_str(spec.run_id)
        .ok()
        .map(|u| u.to_string());
    if canonical.as_deref() != Some(spec.run_id) {
        return Err(format!("run id {:?} is not a run's uuid", spec.run_id));
    }
    if !is_pinned(spec.image) {
        return Err(format!(
            "image {:?} is not pinned by digest (<repo>@sha256:<64 hex>)",
            spec.image
        ));
    }
    if !is_name(spec.login_secret) {
        return Err(format!(
            "login Secret {:?} is not a Secret name",
            spec.login_secret
        ));
    }
    if DEV_POD_SECRETS.contains(&spec.login_secret) {
        return Err(format!(
            "login Secret {:?} is one of the dev pod's own credentials — a launched agent holds \
             none of them (design 21062f95 Q2)",
            spec.login_secret
        ));
    }
    if spec.bound.is_zero() {
        return Err("a launch with no bound has no deadline to give the cluster".into());
    }
    let labels = json!({
        "app.kubernetes.io/part-of": "boss",
        "boss.dev/purpose": "launched-agent",
        "boss.dev/agent-run": spec.run_id,
    });
    let agent = json!({
        "name": "agent",
        "image": spec.image,
        "imagePullPolicy": "IfNotPresent",
        // The harness is exec'd into it with the prompt on stdin; the
        // container itself only waits, and the deadline ends the wait.
        "command": ["/bin/sleep", "infinity"],
        "workingDir": "/workspace",
        "securityContext": security(),
        "env": environment(),
        "resources": resources(),
        "volumeMounts": [
            {"name": "workspace", "mountPath": "/workspace"},
            {"name": "agent-tmp", "mountPath": "/tmp"},
            {"name": LOGIN_VOLUME, "mountPath": LOGIN_MOUNT, "readOnly": true},
        ],
    });
    let pod = json!({
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "hostNetwork": false,
        "hostPID": false,
        "hostIPC": false,
        "shareProcessNamespace": false,
        "restartPolicy": "Never",
        "affinity": affinity(),
        "securityContext": pod_security(),
        "containers": [agent],
        "volumes": SCRATCH_VOLUMES.iter()
            .map(|(name, limit)| json!({"name": name, "emptyDir": {"sizeLimit": limit}}))
            .chain(std::iter::once(json!({"name":"workspace","persistentVolumeClaim":{"claimName":workspace_name(spec.run_id)}})))
            .chain(std::iter::once(json!({"name": LOGIN_VOLUME, "secret": {"secretName": spec.login_secret, "defaultMode": 0o440}})))
            .collect::<Vec<_>>(),
        "imagePullSecrets": [{"name": PULL_SECRET}],
    });
    let job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": name(spec.run_id), "namespace": NAMESPACE, "labels": labels},
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": deadline_seconds(spec.bound, spec.grace)?,
            "ttlSecondsAfterFinished": TTL_SECONDS,
            "template": {"metadata": {"labels": labels}, "spec": pod},
        },
    });
    match authority_error(&job, spec) {
        None => Ok(job),
        Some(why) => Err(format!("the rendered Job breaks its own contract: {why}")),
    }
}

/// Why `job` is not a launched agent's Job for `spec`, or `None`. Judged
/// by property over whatever manifest it is handed, never by comparison
/// with [`manifest`]: every key outside the contract is refused, and each
/// key inside it is held to its value.
pub(crate) fn authority_error(job: &Value, spec: &Spec) -> Option<String> {
    judge(job, spec).err()
}

fn only_keys(what: &str, value: &Value, allowed: &[&str]) -> std::result::Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what} is not an object"))?;
    match object.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(extra) => Err(format!(
            "{what} declares {extra}, which a launched agent's Job does not carry"
        )),
        None => Ok(()),
    }
}

fn judge(job: &Value, spec: &Spec) -> std::result::Result<(), String> {
    if job["apiVersion"] != "batch/v1" || job["kind"] != "Job" {
        return Err("not a batch/v1 Job".into());
    }
    if job["metadata"]["namespace"] != NAMESPACE || job["metadata"]["name"] != name(spec.run_id) {
        return Err(format!(
            "not {} in {NAMESPACE}: a launch's Job is named after its run",
            name(spec.run_id)
        ));
    }
    let outer = &job["spec"];
    only_keys(
        "the Job spec",
        outer,
        &[
            "backoffLimit",
            "activeDeadlineSeconds",
            "ttlSecondsAfterFinished",
            "template",
        ],
    )?;
    if outer["backoffLimit"] != 0 {
        return Err("backoffLimit is not 0: a launch is started once".into());
    }
    // Q3: the cluster ends it at the bound plus the grace, not later.
    let deadline = deadline_seconds(spec.bound, spec.grace)?;
    if outer["activeDeadlineSeconds"].as_u64() != Some(deadline) {
        return Err(format!(
            "activeDeadlineSeconds is {}, not the bound plus the grace ({deadline} s) — the \
             cluster holds the bound (design 21062f95 Q3)",
            outer["activeDeadlineSeconds"]
        ));
    }
    if !matches!(
        outer["ttlSecondsAfterFinished"].as_u64(),
        Some(1..=TTL_SECONDS)
    ) {
        return Err(format!(
            "ttlSecondsAfterFinished is {}, not a short one (1..={TTL_SECONDS} s)",
            outer["ttlSecondsAfterFinished"]
        ));
    }
    let pod = &outer["template"]["spec"];
    // A key the contract does not name is refused rather than judged:
    // serviceAccountName, initContainers, ephemeralContainers, hostUsers,
    // nodeName, runtimeClassName — whatever the next one is.
    only_keys(
        "the pod",
        pod,
        &[
            "automountServiceAccountToken",
            "enableServiceLinks",
            "hostNetwork",
            "hostPID",
            "hostIPC",
            "shareProcessNamespace",
            "restartPolicy",
            "affinity",
            "securityContext",
            "containers",
            "volumes",
            "imagePullSecrets",
        ],
    )?;
    for key in [
        "automountServiceAccountToken",
        "enableServiceLinks",
        "hostNetwork",
        "hostPID",
        "hostIPC",
        "shareProcessNamespace",
    ] {
        if pod[key] != false {
            return Err(format!("the pod's {key} is not false"));
        }
    }
    if pod["restartPolicy"] != "Never" {
        return Err("the pod's restartPolicy is not Never".into());
    }
    if pod["securityContext"] != pod_security() {
        return Err("the pod's securityContext is not the isolated worker's".into());
    }
    if pod["affinity"] != affinity() {
        return Err(
            "the pod's affinity does not enforce the worker's control-plane exclusion".into(),
        );
    }
    if pod.get("imagePullSecrets").is_some()
        && pod["imagePullSecrets"] != json!([{ "name": PULL_SECRET }])
    {
        return Err(format!(
            "imagePullSecrets names more than the registry's {PULL_SECRET}"
        ));
    }
    // Q2: every volume is the pod's own scratch, or the login.
    let volumes = pod["volumes"]
        .as_array()
        .ok_or("the pod declares no volumes list")?;
    let mut logins = Vec::new();
    let mut scratch = std::collections::BTreeSet::new();
    let mut workspace = false;
    for volume in volumes {
        let label = volume["name"].as_str().ok_or("a volume has no name")?;
        if volume.get("emptyDir").is_some() {
            only_keys(&format!("volume {label}"), volume, &["name", "emptyDir"])?;
            let limit = SCRATCH_VOLUMES
                .iter()
                .find(|(name, _)| *name == label)
                .map(|(_, limit)| *limit)
                .ok_or_else(|| format!("volume {label} is not private worker scratch"))?;
            if !scratch.insert(label) || volume["emptyDir"] != json!({"sizeLimit": limit}) {
                return Err(format!(
                    "volume {label} does not conserve its private scratch bound ({limit})"
                ));
            }
        } else if volume.get("persistentVolumeClaim").is_some() {
            if workspace
                || volume
                    != &json!({"name":"workspace","persistentVolumeClaim":{"claimName":workspace_name(spec.run_id)}})
            {
                return Err("workspace is not this run's one private durable claim".into());
            }
            workspace = true;
        } else if volume.get("secret").is_some() {
            only_keys(&format!("volume {label}"), volume, &["name", "secret"])?;
            let secret = volume["secret"]["secretName"].as_str().unwrap_or("");
            if DEV_POD_SECRETS.contains(&secret) {
                return Err(format!(
                    "volume {label} mounts {secret}, one of the dev pod's own credentials \
                     (design 21062f95 Q2)"
                ));
            }
            if secret != spec.login_secret {
                return Err(format!(
                    "volume {label} mounts Secret {secret:?}; the login {:?} is the one Secret \
                     a launched agent holds",
                    spec.login_secret
                ));
            }
            logins.push(label);
        } else {
            return Err(format!(
                "volume {label} is neither the pod's own emptyDir nor the login Secret"
            ));
        }
    }
    if scratch.len() != SCRATCH_VOLUMES.len() || !workspace {
        return Err("the pod does not carry its bounded tmp and private retained workspace".into());
    }
    if logins.len() != 1 {
        return Err(format!(
            "{} volumes mount the login; the contract is exactly one",
            logins.len()
        ));
    }
    let containers = pod["containers"]
        .as_array()
        .ok_or("the pod declares no containers list")?;
    let [agent] = containers.as_slice() else {
        return Err(format!(
            "the pod runs {} containers; a launched agent's runs one",
            containers.len()
        ));
    };
    // No envFrom, lifecycle hook, port, device or args: refused as keys.
    only_keys(
        "the agent container",
        agent,
        &[
            "name",
            "image",
            "imagePullPolicy",
            "command",
            "workingDir",
            "securityContext",
            "env",
            "resources",
            "volumeMounts",
        ],
    )?;
    if agent["name"] != "agent" || agent["image"] != spec.image {
        return Err("the container is not `agent` running the spec's image".into());
    }
    if agent["command"] != json!(["/bin/sleep", "infinity"])
        || agent["workingDir"] != "/workspace"
        || agent["imagePullPolicy"] != "IfNotPresent"
    {
        return Err("agent entrypoint, working directory or image pull policy differs from the private supervisor posture".into());
    }
    if agent["env"] != environment() {
        return Err(
            "agent environment differs, including its exact API-owned PodUID fieldRef".into(),
        );
    }
    if agent["volumeMounts"]
        != json!([
            {"name":"workspace","mountPath":"/workspace"},
            {"name":"agent-tmp","mountPath":"/tmp"},
            {"name":LOGIN_VOLUME,"mountPath":LOGIN_MOUNT,"readOnly":true},
        ])
    {
        return Err(
            "agent mounts do not conserve the image-owned helper and private login/workspace paths"
                .into(),
        );
    }
    if agent["securityContext"] != security() {
        return Err("the agent's securityContext is not the isolated worker's".into());
    }
    if agent["resources"] != resources() {
        return Err(
            "the agent's resources do not conserve the worker's CPU and memory bounds".into(),
        );
    }
    for entry in agent["env"]
        .as_array()
        .ok_or("the agent declares no env list")?
    {
        if entry
            == &json!({"name":"AGENT_POD_UID","valueFrom":{"fieldRef":{"apiVersion":"v1","fieldPath":"metadata.uid"}}})
        {
            continue;
        }
        only_keys("an env entry", entry, &["name", "value"])?;
        let variable = entry["name"].as_str().unwrap_or("");
        if !entry["value"].is_string() {
            return Err(format!("env {variable} has no literal value"));
        }
        if let Some(why) = crate::harness::forbidden_env(variable) {
            return Err(format!("env {variable} {why}"));
        }
    }
    for mount in agent["volumeMounts"]
        .as_array()
        .ok_or("the agent declares no volumeMounts list")?
    {
        let volume = mount["name"].as_str().unwrap_or("");
        if !volumes.iter().any(|v| v["name"] == volume) {
            return Err(format!(
                "the agent mounts {volume:?}, which no volume declares"
            ));
        }
        if logins.contains(&volume) && mount["readOnly"] != true {
            return Err(
                "the login is mounted writable; it is read-only (design 21062f95 Q2)".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RUN: &str = "25261f93-1c1a-449d-943c-effdebaafcff";

    fn spec() -> Spec<'static> {
        Spec {
            run_id: RUN,
            image: "registry.invalid/agent@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            login_secret: "harness-login-codex-exec",
            bound: Duration::from_secs(30 * 60),
            grace: Duration::from_secs(5),
        }
    }

    fn rendered() -> Value {
        manifest(&spec()).expect("the spec renders")
    }

    #[test]
    fn a_controller_workspace_outlives_compute_until_trusted_archive_acknowledgment() {
        let job = rendered();
        let volumes = job["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        let workspace = volumes.iter().find(|v| v["name"] == "workspace").unwrap();
        assert_eq!(
            workspace["persistentVolumeClaim"]["claimName"],
            format!("agent-evidence-{RUN}")
        );
        assert!(workspace.get("emptyDir").is_none());
        assert_eq!(job["spec"]["ttlSecondsAfterFinished"], TTL_SECONDS);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 1805);
    }

    #[test]
    fn interrupted_evidence_collector_is_readonly_and_has_no_harness_login() {
        let job = recovery_manifest(&spec()).unwrap();
        assert_eq!(job["metadata"]["name"], format!("agent-recovery-{RUN}"));
        let volumes = job["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .unwrap();
        assert!(!volumes.iter().any(|v| v.get("secret").is_some()));
        let workspace = volumes.iter().find(|v| v["name"] == "workspace").unwrap();
        assert_eq!(workspace["persistentVolumeClaim"]["readOnly"], true);
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 35);
        assert_eq!(job["spec"]["ttlSecondsAfterFinished"], 300);
    }

    fn pod(job: &mut Value) -> &mut Value {
        &mut job["spec"]["template"]["spec"]
    }

    /// Design 21062f95 Q2: none of the dev pod — no ServiceAccount
    /// token, no machine token, no forge token — only the harness login,
    /// read-only, and a workspace of its own.
    #[test]
    fn a_launched_agents_job_holds_no_credential_of_the_dev_pod() {
        let job = rendered();
        assert_eq!(authority_error(&job, &spec()), None);
        assert_eq!(job["kind"], "Job");
        assert_eq!(job["metadata"]["namespace"], NAMESPACE);
        assert_eq!(job["metadata"]["name"], format!("agent-run-{RUN}"));
        assert_eq!(job["metadata"]["labels"]["boss.dev/agent-run"], RUN);
        let pod = &job["spec"]["template"]["spec"];
        for key in [
            "automountServiceAccountToken",
            "enableServiceLinks",
            "hostNetwork",
            "hostPID",
            "hostIPC",
            "shareProcessNamespace",
        ] {
            assert_eq!(pod[key], false, "{key}");
        }
        assert!(pod.get("serviceAccountName").is_none());
        assert!(pod.get("initContainers").is_none());
        assert_eq!(pod["restartPolicy"], "Never");
        let secrets: Vec<&Value> = pod["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v.get("secret").is_some())
            .collect();
        assert_eq!(secrets.len(), 1, "the login is the pod's one Secret");
        assert_eq!(
            secrets[0]["secret"]["secretName"],
            "harness-login-codex-exec"
        );
        let containers = pod["containers"].as_array().unwrap();
        assert_eq!(containers.len(), 1);
        let agent = &containers[0];
        assert_eq!(agent["name"], "agent");
        assert_eq!(agent["securityContext"]["runAsNonRoot"], true);
        assert_eq!(agent["securityContext"]["allowPrivilegeEscalation"], false);
        assert_eq!(
            agent["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
        let login = agent["volumeMounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["mountPath"] == LOGIN_MOUNT)
            .expect("the login is mounted");
        assert_eq!(login["readOnly"], true);
        let names: Vec<&str> = agent["env"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with("BOSS_")), "{names:?}");
    }

    /// Design 21062f95 Q3: the cluster holds the bound — the deadline
    /// is the verb's bound plus its grace, and a finished Job goes soon.
    #[test]
    fn the_cluster_holds_the_bound_plus_the_grace() {
        let job = rendered();
        assert_eq!(job["spec"]["activeDeadlineSeconds"], 30 * 60 + 5);
        assert_eq!(job["spec"]["backoffLimit"], 0);
        assert_eq!(job["spec"]["ttlSecondsAfterFinished"], TTL_SECONDS);
        // A part second is a whole one more, never one fewer: the
        // cluster's deadline is never earlier than the verb's own.
        assert_eq!(
            deadline_seconds(Duration::from_secs(1), Duration::from_millis(500)).unwrap(),
            2
        );
        assert_eq!(
            deadline_seconds(Duration::from_secs(60), Duration::from_secs(5)).unwrap(),
            65
        );
    }

    #[test]
    fn removing_or_reversing_the_worker_posture_is_refused() {
        let mut accepted = Vec::new();
        for what in ["control-plane", "resources", "scratch"] {
            let mut job = rendered();
            match what {
                "control-plane" => {
                    pod(&mut job)["affinity"] = json!({"nodeAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": {
                    "nodeSelectorTerms": [{"matchExpressions": [{"key": "node-role.kubernetes.io/control-plane", "operator": "Exists"}]}]}}})
                }
                "resources" => pod(&mut job)["containers"][0]["resources"] = json!({}),
                _ => pod(&mut job)["volumes"][0]["emptyDir"] = json!({}),
            }
            if authority_error(&job, &spec()).is_none() {
                accepted.push(what);
            }
        }
        assert!(accepted.is_empty(), "accepted {accepted:?}");
    }

    #[test]
    fn out_of_range_cluster_deadlines_are_refused_without_panicking() {
        for bound in [Duration::MAX, Duration::from_secs(u64::MAX)] {
            let huge = Spec { bound, ..spec() };
            assert!(
                manifest(&huge).is_err(),
                "rendered an out-of-range cluster deadline"
            );
        }
    }

    /// The validator judges the manifest, not the renderer: each added
    /// authority is refused on its own, naming what it found.
    #[test]
    fn every_added_authority_is_refused() {
        type Mutation = fn(&mut Value);
        let mutations: [(&str, Mutation); 24] = [
            ("token", |j| {
                pod(j)["automountServiceAccountToken"] = json!(true)
            }),
            ("token absent", |j| {
                pod(j)
                    .as_object_mut()
                    .unwrap()
                    .remove("automountServiceAccountToken");
            }),
            ("service account", |j| {
                pod(j)["serviceAccountName"] = json!("dev-session")
            }),
            ("host network", |j| pod(j)["hostNetwork"] = json!(true)),
            ("host pid", |j| pod(j)["hostPID"] = json!(true)),
            ("shared pids", |j| {
                pod(j)["shareProcessNamespace"] = json!(true)
            }),
            ("service links", |j| {
                pod(j)["enableServiceLinks"] = json!(true)
            }),
            ("restart", |j| pod(j)["restartPolicy"] = json!("OnFailure")),
            ("machine token volume", |j| {
                pod(j)["volumes"].as_array_mut().unwrap().push(json!(
                    {"name": "machine-token", "secret": {"secretName": "boss-machine-token"}}
                ));
            }),
            ("the work volume", |j| {
                pod(j)["volumes"].as_array_mut().unwrap().push(json!(
                    {"name": "work", "persistentVolumeClaim": {"claimName": "boss-dev-work"}}
                ));
            }),
            ("host path", |j| {
                pod(j)["volumes"].as_array_mut().unwrap().push(json!(
                    {"name": "host", "hostPath": {"path": "/"}}
                ));
            }),
            ("projected token", |j| {
                pod(j)["volumes"].as_array_mut().unwrap().push(json!(
                    {"name": "sa", "projected": {"sources": [{"serviceAccountToken": {"path": "t"}}]}}
                ));
            }),
            ("writable login", |j| {
                for m in pod(j)["containers"][0]["volumeMounts"]
                    .as_array_mut()
                    .unwrap()
                {
                    if m["mountPath"] == LOGIN_MOUNT {
                        m["readOnly"] = json!(false);
                    }
                }
            }),
            ("env from", |j| {
                pod(j)["containers"][0]["envFrom"] = json!([{"secretRef": {"name": "forge-read"}}]);
            }),
            ("env value from", |j| {
                pod(j)["containers"][0]["env"].as_array_mut().unwrap().push(json!(
                    {"name": "TOKEN_FILE", "valueFrom": {"secretKeyRef": {"name": "forge-read", "key": "token"}}}
                ));
            }),
            ("machine token env", |j| {
                pod(j)["containers"][0]["env"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(
                        {"name": "BOSS_MACHINE_TOKEN", "value": "x"}
                    ));
            }),
            ("actor env", |j| {
                pod(j)["containers"][0]["env"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(
                        {"name": "BOSS_ACTOR", "value": "agent-claude"}
                    ));
            }),
            ("privilege", |j| {
                pod(j)["containers"][0]["securityContext"]["allowPrivilegeEscalation"] =
                    json!(true);
            }),
            ("privileged", |j| {
                pod(j)["containers"][0]["securityContext"]["privileged"] = json!(true)
            }),
            ("second container", |j| {
                let again = pod(j)["containers"][0].clone();
                pod(j)["containers"].as_array_mut().unwrap().push(again);
            }),
            ("init container", |j| {
                let again = pod(j)["containers"][0].clone();
                pod(j)["initContainers"] = json!([again]);
            }),
            ("no deadline", |j| {
                j["spec"]
                    .as_object_mut()
                    .unwrap()
                    .remove("activeDeadlineSeconds");
            }),
            ("longer deadline", |j| {
                j["spec"]["activeDeadlineSeconds"] = json!(86400)
            }),
            ("long ttl", |j| {
                j["spec"]["ttlSecondsAfterFinished"] = json!(86400)
            }),
        ];
        for (what, mutate) in mutations {
            let mut job = rendered();
            mutate(&mut job);
            assert!(
                authority_error(&job, &spec()).is_some(),
                "{what}: a manifest carrying it passed"
            );
        }
    }

    /// Nothing renders from a spec that is not exact: a run id, a
    /// digest-pinned image, a Secret NAME that is not one of the dev
    /// pod's own, and a bound.
    #[test]
    fn an_inexact_spec_renders_nothing() {
        let cases: [(&str, Spec); 6] = [
            (
                "run id",
                Spec {
                    run_id: "../x",
                    ..spec()
                },
            ),
            (
                "tag image",
                Spec {
                    image: "registry.invalid/agent:latest",
                    ..spec()
                },
            ),
            (
                "login name",
                Spec {
                    login_secret: "Not_A_Name",
                    ..spec()
                },
            ),
            (
                "machine token as login",
                Spec {
                    login_secret: "boss-machine-token",
                    ..spec()
                },
            ),
            (
                "forge token as login",
                Spec {
                    login_secret: "forge-read",
                    ..spec()
                },
            ),
            (
                "no bound",
                Spec {
                    bound: Duration::ZERO,
                    ..spec()
                },
            ),
        ];
        for (what, case) in cases {
            assert!(manifest(&case).is_err(), "{what}: rendered");
        }
    }

    /// CLAUDE.md 9a: the dev pod's Secrets live in its manifest and in
    /// [`DEV_POD_SECRETS`]; this holds the two equal, so a Secret the
    /// pod gains is one a login can never be named as.
    #[test]
    fn the_refused_secrets_are_every_secret_the_dev_pod_mounts() {
        let text = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/cluster/manifests/boss-dev.yaml"),
        )
        .unwrap();
        let mounted: std::collections::BTreeSet<String> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.split("secretName:").nth(1))
            .map(|rest| {
                rest.trim()
                    .split(|c: char| c == ',' || c == '}' || c.is_whitespace())
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let refused: std::collections::BTreeSet<String> =
            DEV_POD_SECRETS.iter().map(|s| s.to_string()).collect();
        assert!(!mounted.is_empty());
        assert_eq!(mounted, refused);
    }
}
