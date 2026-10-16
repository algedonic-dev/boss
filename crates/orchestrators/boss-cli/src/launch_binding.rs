//! Observation binding for the launched-agent source controller (3869107c).
//! The isolated-build controller binds a Job UID, its owned Pod and the
//! container incarnation before execution (`infra/dev/dev-build.py`). This
//! is that same needed boundary for the agent Job, whose manifest contract
//! lives in [`crate::launch_job`]. A name or selector is not an identity.
//!
//! Native transport originals are retained before these pure checks run.
//! Versioned normalization accepts only named default values; unknown
//! additions survive it and fail complete intent equality. Dispatch's
//! Cluster route calls this code only after authoritative inputs resolve.
//! The shipped selection remains Process. Image/login publication,
//! activation and kernel credential READ/USE evidence remain held.

use crate::launch_job::{self, Spec};
use serde_json::{Value, json};

/// The controller's immutable identity and declared intent. Only a judged
/// creation response can construct it; callers cannot replace its UID.
#[derive(Debug, Clone)]
pub(crate) struct JobBinding {
    name: String,
    uid: String,
    intent: Value,
}

fn text<'a>(value: &'a Value, what: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && !s.chars().any(|c| c.is_control() || c.is_whitespace()))
        .ok_or_else(|| format!("{what} is absent or malformed"))
}

fn not_deleting(object: &Value, what: &str) -> Result<(), String> {
    if !object["metadata"]["deletionTimestamp"].is_null() {
        return Err(format!("{what} is being deleted"));
    }
    Ok(())
}

/// Version 1: each removable admission field has one value and one site.
/// Unknown keys survive normalization and therefore fail complete equality.
/// Callers retain the original object before calling this pure function.
fn default_field(object: &mut Value, key: &str, expected: Value) -> Result<(), String> {
    let fields = object
        .as_object_mut()
        .ok_or("default site is not an object")?;
    if let Some(actual) = fields.get(key) {
        if actual != &expected {
            return Err(format!("normalization v1 refuses {key}: {actual}"));
        }
        fields.remove(key);
    }
    Ok(())
}

fn pod_spec_v1(raw: &Value, placed: bool) -> Result<Value, String> {
    let mut pod = raw.clone();
    // Kubernetes omits these three non-pointer false fields (observed in the
    // first bounded isolation pilot). Restore only their known default;
    // null, true and non-boolean values are never normalized into authority.
    for field in ["hostNetwork", "hostPID", "hostIPC"] {
        match pod.get(field) {
            None => {
                pod.as_object_mut()
                    .ok_or("Pod spec is not an object")?
                    .insert(field.to_owned(), json!(false));
            }
            Some(Value::Bool(false)) => {}
            Some(_) => return Err(format!("normalization v1 refuses {field}")),
        }
    }
    for (key, value) in [
        ("dnsPolicy", json!("ClusterFirst")),
        ("schedulerName", json!("default-scheduler")),
        ("terminationGracePeriodSeconds", json!(30)),
        ("preemptionPolicy", json!("PreemptLowerPriority")),
        ("priority", json!(0)),
    ] {
        default_field(&mut pod, key, value)?;
    }
    // A declared dedicated account is intent, not an invalid default. The
    // deprecated native alias may disappear only when it names that account.
    let account = pod
        .get("serviceAccountName")
        .cloned()
        .unwrap_or(json!("default"));
    if account == "default" {
        default_field(&mut pod, "serviceAccountName", json!("default"))?;
    }
    default_field(&mut pod, "serviceAccount", account)?;
    if placed && let Some(node) = pod.get("nodeName") {
        text(node, "observed scheduled node")?;
        pod.as_object_mut()
            .ok_or("Pod spec is not an object")?
            .remove("nodeName");
    }
    if placed {
        default_field(
            &mut pod,
            "tolerations",
            json!([
                {"key":"node.kubernetes.io/not-ready","operator":"Exists","effect":"NoExecute","tolerationSeconds":300},
                {"key":"node.kubernetes.io/unreachable","operator":"Exists","effect":"NoExecute","tolerationSeconds":300},
            ]),
        )?;
    }
    for container in pod["containers"]
        .as_array_mut()
        .ok_or("no container list")?
    {
        default_field(
            container,
            "terminationMessagePath",
            json!("/dev/termination-log"),
        )?;
        default_field(container, "terminationMessagePolicy", json!("File"))?;
    }
    Ok(pod)
}

fn normalized_job_v1(raw: &Value) -> Result<Value, String> {
    let mut job = raw.clone();
    let uid = text(&job["metadata"]["uid"], "Job UID")?.to_string();
    text(&job["metadata"]["resourceVersion"], "Job resourceVersion")?;
    let name = text(&job["metadata"]["name"], "Job name")?.to_string();
    for (key, value) in [
        ("parallelism", json!(1)),
        ("completions", json!(1)),
        ("completionMode", json!("NonIndexed")),
        ("suspend", json!(false)),
        ("manualSelector", json!(false)),
        ("podReplacementPolicy", json!("TerminatingOrFailed")),
        (
            "selector",
            json!({"matchLabels":{"batch.kubernetes.io/controller-uid":uid}}),
        ),
    ] {
        default_field(&mut job["spec"], key, value)?;
    }
    for (key, value) in [
        ("batch.kubernetes.io/controller-uid", json!(uid)),
        ("controller-uid", json!(uid)),
        ("batch.kubernetes.io/job-name", json!(name)),
        ("job-name", json!(name)),
    ] {
        default_field(
            &mut job["spec"]["template"]["metadata"]["labels"],
            key,
            value,
        )?;
    }
    if job["spec"]["template"]["metadata"]
        .get("creationTimestamp")
        .is_some()
    {
        default_field(
            &mut job["spec"]["template"]["metadata"],
            "creationTimestamp",
            Value::Null,
        )?;
    }
    job["spec"]["template"]["spec"] = pod_spec_v1(&job["spec"]["template"]["spec"], false)?;
    Ok(job)
}

impl JobBinding {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn uid(&self) -> &str {
        &self.uid
    }
    pub(crate) fn from_created(job: &Value, spec: &Spec) -> Result<Self, String> {
        let intent = launch_job::manifest(spec)?;
        Self::from_intent(job, intent, |job| launch_job::authority_error(job, spec))
    }

    pub(crate) fn from_recovery_created(job: &Value, spec: &Spec) -> Result<Self, String> {
        let intent = launch_job::recovery_manifest(spec)?;
        Self::from_intent(job, intent, |job| {
            launch_job::recovery_authority_error(job, spec)
        })
    }

    pub(crate) fn from_collector_created(
        job: &Value,
        spec: &crate::launch_controller::managed_cache::collector::Spec<'_>,
    ) -> Result<Self, String> {
        let intent = crate::launch_controller::managed_cache::collector::manifest(spec)
            .map_err(|_| "private collector intent unavailable".to_owned())?;
        Self::from_intent(job, intent, |_| None)
    }

    fn from_intent(
        job: &Value,
        intent: Value,
        judge: impl Fn(&Value) -> Option<String>,
    ) -> Result<Self, String> {
        if job["metadata"]["labels"] != intent["metadata"]["labels"] {
            return Err("created Job run labels differ from its prepared intent".into());
        }
        if let Some(annotations) = intent["metadata"].get("annotations")
            && job["metadata"].get("annotations") != Some(annotations)
        {
            return Err("created Job differs from its declared generation annotations".into());
        }
        let judged = normalized_job_v1(job)?;
        if let Some(why) = judge(&judged) {
            return Err(format!("created Job breaks its declared contract: {why}"));
        }
        if judged["spec"] != intent["spec"] {
            return Err(
                "created Job differs from the controller's complete declared intent".into(),
            );
        }
        not_deleting(job, "created Job")?;
        Ok(Self {
            name: text(&intent["metadata"]["name"], "judged Job name")?.into(),
            uid: text(&job["metadata"]["uid"], "created Job UID")?.into(),
            intent,
        })
    }

    fn recheck_job(&self, job: &Value) -> Result<(), String> {
        if job["apiVersion"] != self.intent["apiVersion"]
            || job["kind"] != self.intent["kind"]
            || job["metadata"]["namespace"] != launch_job::NAMESPACE
            || job["metadata"]["name"] != self.name
            || job["metadata"]["uid"] != self.uid
        {
            return Err("observed Job identity differs from the creation binding".into());
        }
        not_deleting(job, "observed Job")?;
        if job["metadata"]["labels"] != self.intent["metadata"]["labels"] {
            return Err("observed Job ownership labels changed".into());
        }
        if let Some(annotations) = self.intent["metadata"].get("annotations")
            && job["metadata"].get("annotations") != Some(annotations)
        {
            return Err("observed Job generation annotations changed".into());
        }
        if normalized_job_v1(job)?["spec"] != self.intent["spec"] {
            return Err("observed Job no longer has its judged intent".into());
        }
        Ok(())
    }

    /// A complete selector result must contain exactly one owned Pod.
    /// Unknown pagination and a second candidate are refusals, not passes.
    fn observed_pod<'a>(&self, job: &Value, pods: &'a Value) -> Result<Option<&'a Value>, String> {
        self.recheck_job(job)?;
        if pods["apiVersion"] != "v1" || pods["kind"] != "PodList" {
            return Err("observation is not a v1 PodList".into());
        }
        let metadata = pods["metadata"]
            .as_object()
            .ok_or("PodList has no metadata")?;
        text(
            &pods["metadata"]["resourceVersion"],
            "PodList observed revision",
        )?;
        if metadata
            .get("continue")
            .is_some_and(|v| v.as_str() != Some(""))
            || metadata
                .get("remainingItemCount")
                .is_some_and(|v| v.as_u64() != Some(0))
        {
            return Err("PodList is incomplete or its pagination is unknown".into());
        }
        let items = pods["items"]
            .as_array()
            .ok_or("PodList has no items array")?;
        let pod = match items.as_slice() {
            [] => return Ok(None),
            [pod] => pod,
            _ => {
                return Err(format!(
                    "PodList has {} candidates, not exactly one",
                    items.len()
                ));
            }
        };
        if pod["apiVersion"] != "v1"
            || pod["kind"] != "Pod"
            || pod["metadata"]["namespace"] != launch_job::NAMESPACE
        {
            return Err("observed Pod kind or namespace differs".into());
        }
        not_deleting(pod, "observed Pod")?;
        text(&pod["metadata"]["uid"], "observed Pod UID")?;
        text(&pod["metadata"]["name"], "observed Pod name")?;
        text(
            &pod["metadata"]["resourceVersion"],
            "Pod observed resourceVersion",
        )?;
        let mut labels = pod["metadata"]["labels"].clone();
        for (key, value) in [
            ("batch.kubernetes.io/controller-uid", json!(self.uid)),
            ("controller-uid", json!(self.uid)),
            ("batch.kubernetes.io/job-name", json!(self.name)),
            ("job-name", json!(self.name)),
        ] {
            default_field(&mut labels, key, value)?;
        }
        if labels != self.intent["spec"]["template"]["metadata"]["labels"] {
            return Err("Pod ownership labels differ from the bound run".into());
        }
        let owners = pod["metadata"]["ownerReferences"]
            .as_array()
            .ok_or("Pod has no owners")?;
        let [owner] = owners.as_slice() else {
            return Err("Pod does not have exactly one observed owner".into());
        };
        if owner["apiVersion"] != "batch/v1"
            || owner["kind"] != "Job"
            || owner["name"] != self.name
            || owner["uid"] != self.uid
            || owner["controller"] != true
        {
            return Err("Pod is not controlled by the bound Job UID".into());
        }
        if pod_spec_v1(&pod["spec"], true)? != self.intent["spec"]["template"]["spec"] {
            return Err("Pod no longer has the judged private worker posture".into());
        }
        Ok(Some(pod))
    }

    pub(crate) fn scheduling(&self, job: &Value, pods: &Value) -> Result<bool, String> {
        Ok(self
            .observed_pod(job, pods)?
            .is_none_or(|p| p["status"]["phase"] == "Pending"))
    }

    pub(crate) fn observe_running(
        &self,
        job: &Value,
        pods: &Value,
    ) -> Result<RunningBinding, String> {
        let pod = self
            .observed_pod(job, pods)?
            .ok_or("complete PodList contains no running Pod yet")?;
        let statuses = pod["status"]["containerStatuses"]
            .as_array()
            .ok_or("Pod has no container observations")?;
        let [agent] = statuses.as_slice() else {
            return Err("Pod does not have exactly one observed container".into());
        };
        if pod["status"]["phase"] != "Running"
            || agent["name"] != self.intent["spec"]["template"]["spec"]["containers"][0]["name"]
            || agent["restartCount"].as_u64() != Some(0)
            || agent["ready"] != true
            || !agent["state"]["running"].is_object()
            || agent["state"]
                .as_object()
                .is_none_or(|state| state.len() != 1)
        {
            return Err(
                "bound container is not observed running, ready and never restarted".into(),
            );
        }
        Ok(RunningBinding {
            job: self.clone(),
            pod_name: text(&pod["metadata"]["name"], "Pod name")?.into(),
            pod_uid: text(&pod["metadata"]["uid"], "Pod UID")?.into(),
            container_id: text(&agent["containerID"], "container incarnation")?.into(),
        })
    }

    /// The same UID precondition and foreground collection used by the
    /// isolated-build delete door. This is request data, never evidence
    /// that deletion happened or that escaped processes have stopped.
    pub(crate) fn delete_options(&self) -> Value {
        json!({"apiVersion":"v1", "kind":"DeleteOptions", "preconditions":{"uid":self.uid}, "propagationPolicy":"Foreground"})
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RunningBinding {
    job: JobBinding,
    pod_name: String,
    pod_uid: String,
    container_id: String,
}

impl RunningBinding {
    pub(crate) fn matches_run(&self, run: &str) -> bool {
        self.job.intent["metadata"]["labels"]["boss.dev/agent-run"] == run
    }
    pub(crate) fn pod_name(&self) -> &str {
        &self.pod_name
    }
    pub(crate) fn identity(&self) -> Value {
        json!({"job_uid":self.job.uid,"pod":self.pod_name,"pod_uid":self.pod_uid,"container_id":self.container_id})
    }
    /// A later observation must conserve the Job, Pod and container
    /// incarnation. There is no fallback to the new Pod bearing its name.
    pub(crate) fn recheck(&self, job: &Value, pods: &Value) -> Result<(), String> {
        let observed = self.job.observe_running(job, pods)?;
        if observed.pod_name != self.pod_name
            || observed.pod_uid != self.pod_uid
            || observed.container_id != self.container_id
        {
            return Err(
                "observed Pod or container incarnation replaced the running binding".into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_job::{self, Spec};
    use serde_json::{Value, json};
    use std::time::Duration;

    #[test]
    fn collector_binding_matches_the_fixed_factory() {
        use crate::launch_controller::managed_cache::collector;
        let witness = json!({"image":format!("registry.invalid/agent@sha256:{}","a".repeat(64)),"generation":"b".repeat(40),"image_platform":"linux/amd64",
            "collector_helper_sha256":crate::launch_controller::hash(include_bytes!("../../../../infra/agent/private-collector.py")),
            "collector_contract_sha256":crate::launch_controller::hash(include_bytes!("../../../../infra/agent/private-collector-contract.json"))});
        let spec = collector::Spec {
            run: uuid::Uuid::from_u128(1),
            nonce: uuid::Uuid::from_u128(2),
            workspace_uid: "original",
            image_witness: &witness,
            trust_generation: uuid::Uuid::from_u128(3),
        };
        let mut original = collector::manifest(&spec).unwrap();
        original["metadata"]["uid"] = json!("collector-job");
        original["metadata"]["resourceVersion"] = json!("1");
        // The actual generic binding normalization must handle the fixed
        // collector SA, rather than treating every SA as the default one.
        assert!(normalized_job_v1(&original).is_ok());
        let binding = JobBinding::from_collector_created(&original, &spec).unwrap();
        let mut aliased = original.clone();
        aliased["spec"]["template"]["spec"]["serviceAccount"] =
            json!("boss-managed-cache-collector");
        assert!(JobBinding::from_collector_created(&aliased, &spec).is_ok());
        for path in ["serviceAccountName", "serviceAccount"] {
            let mut changed = original.clone();
            changed["spec"]["template"]["spec"][path] = json!("wrong-account");
            assert!(JobBinding::from_collector_created(&changed, &spec).is_err());
        }
        let mut altered_trust = original.clone();
        altered_trust["metadata"]["annotations"]["boss.dev/trust-generation"] =
            json!(uuid::Uuid::new_v4());
        assert!(JobBinding::from_collector_created(&altered_trust, &spec).is_err());
        let mut native_defaults = original.clone();
        for flag in ["hostNetwork", "hostPID", "hostIPC"] {
            native_defaults["spec"]["template"]["spec"]
                .as_object_mut()
                .unwrap()
                .remove(flag);
        }
        assert!(JobBinding::from_collector_created(&native_defaults, &spec).is_ok());
        for flag in ["hostNetwork", "hostPID", "hostIPC"] {
            for forbidden in [json!(true), Value::Null, json!(0), json!("false")] {
                let mut changed = native_defaults.clone();
                changed["spec"]["template"]["spec"][flag] = forbidden;
                assert!(
                    JobBinding::from_collector_created(&changed, &spec).is_err(),
                    "{flag}"
                );
            }
        }
        let mut observed = pod(&original);
        observed["status"]["containerStatuses"][0]["name"] = json!("collector");
        let running = binding
            .observe_running(&original, &listed(observed.clone()))
            .unwrap();
        running
            .recheck(&original, &listed(observed.clone()))
            .unwrap();
        assert!(
            running
                .recheck(&altered_trust, &listed(observed.clone()))
                .is_err()
        );
        observed["spec"]["containers"][0]["volumeMounts"][0]["readOnly"] = json!(false);
        assert!(running.recheck(&original, &listed(observed)).is_err());
    }

    fn created() -> Value {
        let mut job = launch_job::manifest(&Spec {
            run_id: "61be2c4a-68ee-4156-9709-7dc1ebe89b16",
            image: "registry/agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            login_secret: "harness-fixture-login", bound: Duration::from_secs(60), grace: Duration::from_secs(5),
        }).unwrap();
        job["metadata"]["uid"] = json!("observed-job-uid");
        job["metadata"]["resourceVersion"] = json!("1");
        job
    }

    fn pod(job: &Value) -> Value {
        json!({"apiVersion":"v1", "kind":"Pod",
            "metadata": {"name":"observed-pod", "namespace":"boss-dev", "uid":"observed-pod-uid","resourceVersion":"1","labels":job["spec"]["template"]["metadata"]["labels"],
                "ownerReferences":[{"apiVersion":"batch/v1", "kind":"Job", "name":job["metadata"]["name"], "uid":job["metadata"]["uid"], "controller":true}]},
            "spec":job["spec"]["template"]["spec"],
            "status":{"phase":"Running", "containerStatuses":[{"name":"agent", "containerID":"containerd://observed", "restartCount":0, "ready":true, "state":{"running":{"startedAt":"2026-10-11T07:00:00Z"}}}]}})
    }

    fn spec() -> Spec<'static> {
        Spec {
            run_id: "61be2c4a-68ee-4156-9709-7dc1ebe89b16",
            image: "registry/agent@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            login_secret: "harness-fixture-login",
            bound: Duration::from_secs(60),
            grace: Duration::from_secs(5),
        }
    }

    fn listed(pod: Value) -> Value {
        json!({"apiVersion":"v1", "kind":"PodList", "metadata":{"resourceVersion":"observed"}, "items":[pod]})
    }

    #[test]
    fn a_created_job_binds_its_uid_and_conditions_retirement_on_it() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        assert_eq!(
            binding.delete_options(),
            json!({"apiVersion":"v1", "kind":"DeleteOptions", "preconditions":{"uid":"observed-job-uid"}, "propagationPolicy":"Foreground"})
        );
        assert!(binding.observe_running(&job, &listed(pod(&job))).is_ok());
    }

    #[test]
    fn a_missing_or_changed_job_identity_is_not_a_running_binding() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        for key in ["uid", "name", "namespace"] {
            let mut changed = job.clone();
            changed["metadata"][key] = json!("other");
            assert!(
                binding
                    .observe_running(&changed, &listed(pod(&job)))
                    .is_err(),
                "{key}"
            );
            let mut missing = job.clone();
            missing["metadata"].as_object_mut().unwrap().remove(key);
            assert!(
                JobBinding::from_created(&missing, &spec()).is_err(),
                "missing {key}"
            );
        }
        let mut changed = job.clone();
        changed["metadata"]["deletionTimestamp"] = json!("2026-10-11T07:00:00Z");
        assert!(
            binding
                .observe_running(&changed, &listed(pod(&job)))
                .is_err()
        );
    }

    #[test]
    fn pod_owner_posture_and_running_incarnation_are_observed() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        let original = pod(&job);
        let mutations = [
            ("/metadata/namespace", json!("other")),
            ("/metadata/uid", json!("")),
            ("/metadata/ownerReferences/0/uid", json!("other")),
            ("/metadata/ownerReferences/0/name", json!("other")),
            ("/metadata/ownerReferences/0/controller", json!(false)),
            ("/metadata/ownerReferences/0/kind", json!("ReplicaSet")),
            ("/spec/automountServiceAccountToken", json!(true)),
            ("/status/phase", json!("Pending")),
            ("/status/containerStatuses/0/restartCount", json!(1)),
            ("/status/containerStatuses/0/ready", json!(false)),
            ("/status/containerStatuses/0/containerID", json!("")),
            (
                "/status/containerStatuses/0/state",
                json!({"terminated":{"exitCode":0}}),
            ),
        ];
        for (pointer, value) in mutations {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                binding.observe_running(&job, &listed(changed)).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn a_replaced_pod_or_container_cannot_reuse_a_running_binding() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        let original = listed(pod(&job));
        let running = binding.observe_running(&job, &original).unwrap();
        assert!(running.recheck(&job, &original).is_ok());
        for pointer in [
            "/items/0/metadata/uid",
            "/items/0/metadata/name",
            "/items/0/status/containerStatuses/0/containerID",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("replacement");
            assert!(running.recheck(&job, &changed).is_err(), "{pointer}");
        }
    }

    #[test]
    fn incomplete_empty_ambiguous_or_untyped_pod_lists_are_not_evidence() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        let original = listed(pod(&job));
        for value in [
            Value::Null,
            json!({}),
            json!({"items":[pod(&job)]}),
            json!({"apiVersion":"v1", "kind":"PodList", "metadata":{}, "items":[]}),
        ] {
            assert!(binding.observe_running(&job, &value).is_err());
        }
        for (pointer, value) in [
            (
                "/metadata",
                json!({"resourceVersion":"observed", "continue":"next-page"}),
            ),
            (
                "/metadata",
                json!({"resourceVersion":"observed", "remainingItemCount":1}),
            ),
            (
                "/metadata",
                json!({"resourceVersion":"observed", "continue":null}),
            ),
            ("/items", json!([pod(&job), pod(&job)])),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                binding.observe_running(&job, &changed).is_err(),
                "{pointer}"
            );
        }
        let mut full = original.clone();
        full["metadata"] =
            json!({"resourceVersion":"observed", "continue":"", "remainingItemCount":0});
        assert!(binding.observe_running(&job, &full).is_ok());
    }

    #[test]
    fn creation_and_every_recheck_conserve_the_judged_posture() {
        let original = created();
        let binding = JobBinding::from_created(&original, &spec()).unwrap();
        for (pointer, value) in [
            ("/kind", json!("Pod")),
            ("/metadata/uid", json!("")),
            ("/spec/activeDeadlineSeconds", json!(999)),
            (
                "/spec/template/spec/automountServiceAccountToken",
                json!(true),
            ),
            (
                "/spec/template/spec/containers/0/image",
                json!("unbound:latest"),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                JobBinding::from_created(&changed, &spec()).is_err(),
                "creation {pointer}"
            );
            assert!(
                binding
                    .observe_running(&changed, &listed(pod(&original)))
                    .is_err(),
                "recheck {pointer}"
            );
        }
        let mut defaulted = original.clone();
        defaulted["spec"]["template"]["spec"]["dnsPolicy"] = json!("ClusterFirst");
        assert!(JobBinding::from_created(&defaulted, &spec()).is_ok());
        assert!(
            binding
                .observe_running(&defaulted, &listed(pod(&original)))
                .is_ok()
        );
    }

    #[test]
    fn unknown_list_revision_and_blank_identity_cannot_establish_a_binding() {
        let job = created();
        let binding = JobBinding::from_created(&job, &spec()).unwrap();
        let mut no_revision = listed(pod(&job));
        no_revision["metadata"] = json!({});
        assert!(binding.observe_running(&job, &no_revision).is_err());
        let mut blank_uid = job.clone();
        blank_uid["metadata"]["uid"] = json!(" ");
        assert!(JobBinding::from_created(&blank_uid, &spec()).is_err());
    }

    #[test]
    fn creation_response_cannot_add_an_unjudged_start_command() {
        let mut job = created();
        job["spec"]["template"]["spec"]["containers"][0]["command"] =
            json!(["/bin/sh", "-c", "echo unexpected"]);
        assert!(JobBinding::from_created(&job, &spec()).is_err());
    }

    #[test]
    fn controller_v1_accepts_named_kubernetes_defaults_but_refuses_authority_twins() {
        let mut job = created();
        job["metadata"]["resourceVersion"] = json!("1");
        job["spec"]["parallelism"] = json!(1);
        job["spec"]["completions"] = json!(1);
        job["spec"]["completionMode"] = json!("NonIndexed");
        job["spec"]["suspend"] = json!(false);
        let uid = job["metadata"]["uid"].clone();
        job["spec"]["selector"] = json!({"matchLabels":{"batch.kubernetes.io/controller-uid":uid}});
        job["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/controller-uid"] = uid;
        job["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/job-name"] =
            job["metadata"]["name"].clone();
        job["spec"]["template"]["spec"]["dnsPolicy"] = json!("ClusterFirst");
        job["spec"]["template"]["spec"]["schedulerName"] = json!("default-scheduler");
        job["spec"]["template"]["spec"]["terminationGracePeriodSeconds"] = json!(30);
        job["spec"]["template"]["spec"]["containers"][0]["terminationMessagePath"] =
            json!("/dev/termination-log");
        job["spec"]["template"]["spec"]["containers"][0]["terminationMessagePolicy"] =
            json!("File");
        assert!(JobBinding::from_created(&job, &spec()).is_ok());
        for (pointer, value) in [
            ("/spec/parallelism", json!(2)),
            ("/spec/template/spec/dnsPolicy", json!("Default")),
            (
                "/spec/template/spec/containers/0/terminationMessagePath",
                json!("/harness/login/auth.json"),
            ),
            (
                "/spec/template/spec/automountServiceAccountToken",
                json!(true),
            ),
        ] {
            let mut injected = job.clone();
            *injected.pointer_mut(pointer).unwrap() = value;
            assert!(
                JobBinding::from_created(&injected, &spec()).is_err(),
                "{pointer}"
            );
        }
    }
}
