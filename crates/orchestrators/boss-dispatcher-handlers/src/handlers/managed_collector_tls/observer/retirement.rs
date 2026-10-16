//! Join an authoritative sealed launch record to independently measured absence.
//! This module consumes a protected port; it does not create that producer or
//! infer completeness from Kubernetes labels, empty queries, or a caller flag.
use super::*;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Closure {
    schema: u32,
    installation: uuid::Uuid,
    generation: uuid::Uuid,
    producer_pod_uid: String,
    revision: uuid::Uuid,
    closed_at: DateTime<Utc>,
    unresolved_operations: Vec<Value>,
    collectors: Vec<Collector>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Collector {
    job: Value,
    pod: Value,
    tokens: Vec<Token>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Token {
    issued_at: DateTime<Utc>,
    request: Value,
    response: Value,
}
fn identity<'a>(
    value: &'a Value,
    kind: &str,
    version: &str,
) -> Result<(&'a str, &'a str), Refused> {
    let field = installation::field;
    let name = field(value, "/metadata/name")?;
    let uid = field(value, "/metadata/uid")?;
    if value["kind"] != kind
        || value["apiVersion"] != version
        || value["metadata"]["namespace"] != "boss-dev"
        || !installation::pod_name(name)
        || uid.is_empty()
        || value
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|v| !v.is_null())
    {
        return Err(Refused);
    }
    Ok((name, uid))
}
impl Closure {
    fn validate(
        &self,
        original: &installation::Original,
        generation: uuid::Uuid,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, Refused> {
        let contract = installation::collector_contract()?;
        let token_seconds = contract["token_seconds"]
            .as_i64()
            .filter(|seconds| *seconds > 0 && *seconds <= 600)
            .ok_or(Refused)?;
        if self.schema != 1
            || self.installation != original.installation
            || self.generation != generation
            || self.producer_pod_uid != installation::field(&original.pod, "/metadata/uid")?
            || self.revision.is_nil()
            || self.closed_at > now
            || !self.unresolved_operations.is_empty()
            || self.collectors.len() > 64
        {
            return Err(Refused);
        }
        // A sealed producer forbids further old-generation issuance, including
        // launches without tokens. Its last possible issue is bounded by the
        // fixed token lifetime; an empty sealed census is not an empty query.
        let mut last = self.closed_at + chrono::Duration::seconds(token_seconds);
        let mut jobs = BTreeSet::new();
        let mut pods = BTreeSet::new();
        for collector in &self.collectors {
            let (job, job_uid) = identity(&collector.job, "Job", "batch/v1")?;
            let (pod, pod_uid) = identity(&collector.pod, "Pod", "v1")?;
            if !jobs.insert((job, job_uid))
                || !pods.insert((pod, pod_uid))
                || collector.pod["spec"]["serviceAccountName"] != contract["account"]
                || collector.tokens.is_empty()
                || collector.tokens.len() > 64
            {
                return Err(Refused);
            }
            let owners = collector.pod["metadata"]["ownerReferences"]
                .as_array()
                .ok_or(Refused)?;
            if owners
                .iter()
                .filter(|owner| owner["controller"] == true)
                .count()
                != 1
                || !owners.iter().any(|owner| {
                    owner["controller"] == true
                        && owner["kind"] == "Job"
                        && owner["apiVersion"] == "batch/v1"
                        && owner["name"] == job
                        && owner["uid"] == job_uid
                })
            {
                return Err(Refused);
            }
            for token in &collector.tokens {
                let request = &token.request;
                let response = &token.response;
                if token.issued_at > self.closed_at
                    || request["kind"] != "TokenRequest"
                    || request["apiVersion"] != "authentication.k8s.io/v1"
                    || request["metadata"]["namespace"] != "boss-dev"
                    || request["metadata"]["name"] != contract["account"]
                    || request["spec"]["audiences"] != serde_json::json!([contract["audience"]])
                    || request["spec"]["expirationSeconds"] != token_seconds
                    || request["spec"]["boundObjectRef"]
                        != serde_json::json!({"apiVersion":"v1","kind":"Pod","name":pod,"uid":pod_uid})
                    || response["kind"] != request["kind"]
                    || response["apiVersion"] != request["apiVersion"]
                    || response["metadata"]["namespace"] != request["metadata"]["namespace"]
                    || response["metadata"]["name"] != request["metadata"]["name"]
                    || response["spec"] != request["spec"]
                    || installation::field(response, "/status/token")?.is_empty()
                {
                    return Err(Refused);
                }
                let expiry = DateTime::parse_from_rfc3339(installation::field(
                    response,
                    "/status/expirationTimestamp",
                )?)
                .map_err(|_| Refused)?
                .with_timezone(&Utc);
                if expiry <= token.issued_at
                    || expiry > token.issued_at + chrono::Duration::seconds(token_seconds)
                {
                    return Err(Refused);
                }
                last = last.max(expiry);
            }
        }
        if last > now {
            return Err(Refused);
        }
        Ok(last)
    }
}
fn not_found(reply: &NativeReply, name: &str, resource: &str, group: &str) -> Result<(), Refused> {
    let value: Value = serde_json::from_slice(&reply.body).map_err(|_| Refused)?;
    let group_matches = value["details"]["group"] == group
        || (group.is_empty() && value["details"].get("group").is_none());
    if reply.status != Some(404)
        || reply.error.is_some()
        || value["kind"] != "Status"
        || value["apiVersion"] != "v1"
        || value["status"] != "Failure"
        || value["reason"] != "NotFound"
        || value["code"] != 404
        || value["details"]["name"] != name
        || value["details"]["kind"] != resource
        || !group_matches
    {
        return Err(Refused);
    }
    Ok(())
}
impl Capture<'_> {
    async fn census(&mut self, generation: uuid::Uuid) -> Result<CensusObservation, Refused> {
        let snapshot = self
            .observer
            .census
            .as_ref()
            .ok_or(Refused)?
            .observe(self.invocation, generation)
            .await?;
        self.retain("registered-generation-closure", &snapshot)
            .await?;
        if snapshot.invocation != self.invocation
            || snapshot.observed_at < self.started
            || snapshot.observed_at > self.observer.clock.now()
        {
            return Err(Refused);
        }
        Ok(snapshot)
    }
    pub(super) async fn retirement(
        &mut self,
        original: &installation::Original,
        generation: uuid::Uuid,
    ) -> Result<DateTime<Utc>, Refused> {
        let before = self.census(generation).await?;
        let closure: Closure =
            serde_json::from_value(before.original.clone()).map_err(|_| Refused)?;
        let expiry = closure.validate(original, generation, self.started)?;
        for _ in 0..2 {
            for collector in &closure.collectors {
                let job = installation::field(&collector.job, "/metadata/name")?;
                let pod = installation::field(&collector.pod, "/metadata/name")?;
                not_found(
                    &self.native(Target::CollectorJob(job.into())).await?,
                    job,
                    "jobs",
                    "batch",
                )?;
                not_found(
                    &self.native(Target::CollectorPod(pod.into())).await?,
                    pod,
                    "pods",
                    "",
                )?;
            }
        }
        let after = self.census(generation).await?;
        if after.original != before.original || after.observed_at < before.observed_at {
            return Err(Refused);
        }
        Ok(expiry)
    }
}
