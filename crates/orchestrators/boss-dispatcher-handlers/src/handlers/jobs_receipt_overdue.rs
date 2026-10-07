//! Observe a registry-declared receipt deadline without ending its work.
//! Receipt absence is an overdue obligation, never proof of worker death
//! (da708aaf). Legacy packets declare no obligation and remain untouched.

use super::common::{
    RECOVERED_AT, Retraction, api_client, dispatcher_actor_header, get_json, owner_for_filing,
    post_json, recovery_note, retraction, row_or_refuse, rows_or_refuse, sim_origin_value,
    with_lane, withdrawal_fields, write_json,
};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const HANDLER: &str = "jobs.receipt_overdue";
pub const ALARM_KEY: &str = "receipt_watch";

/// A receipt obligation declared by registry data, independent of executor.
#[derive(Debug, Clone)]
pub struct Declared {
    pub kind: String,
    pub step: String,
    pub dispatched_step: String,
    pub expectation_key: String,
    pub receipt_key: String,
}

#[derive(Debug, PartialEq)]
pub struct Overdue {
    pub packet: String,
    pub since: DateTime<Utc>,
    pub bound_minutes: i64,
}

fn declared(args: &[(String, Value)]) -> Result<Declared, HandlerError> {
    let text = |key| -> Result<String, HandlerError> {
        let value = arg_string(args, key)?;
        if value.is_empty() {
            return Err(HandlerError::Permanent(format!("{key} is empty")));
        }
        Ok(value.to_string())
    };
    Ok(Declared {
        kind: text("kind")?,
        step: text("step")?,
        dispatched_step: text("dispatched_step")?,
        expectation_key: text("expectation_key")?,
        receipt_key: text("receipt_key")?,
    })
}

/// A declared receipt missed its dispatch deadline. This observes recorded
/// acknowledgment, never physical execution, and never completes work.
pub fn judge(job: &Json, d: &Declared, now: DateTime<Utc>) -> Result<Option<Overdue>, String> {
    if job.get("kind").and_then(Json::as_str) != Some(d.kind.as_str())
        || job.get("status").and_then(Json::as_str) != Some("open")
    {
        return Ok(None);
    }
    let steps = job
        .get("steps")
        .and_then(Json::as_array)
        .ok_or_else(|| "receipt watch read no step array".to_string())?;
    let find = |slug: &str| {
        steps
            .iter()
            .find(|s| s.get("spec_slug").and_then(Json::as_str) == Some(slug))
    };
    let Some(step) = find(&d.step) else {
        return Ok(None);
    };
    if !matches!(
        step.get("status").and_then(Json::as_str),
        Some("ready" | "active")
    ) {
        return Ok(None);
    }
    let Some(expectation) = step.get("metadata").and_then(|m| m.get(&d.expectation_key)) else {
        // No retrospective expectation on legacy or unmarked workers.
        return Ok(None);
    };
    if expectation.get("schema").and_then(Json::as_u64) != Some(1) {
        return Err("receipt expectation has an unsupported schema".into());
    }
    let minutes = expectation
        .get("minutes")
        .and_then(Json::as_i64)
        .filter(|m| *m > 0)
        .ok_or_else(|| "receipt expectation has no positive minute bound".to_string())?;
    let duration = Duration::try_minutes(minutes)
        .ok_or_else(|| "receipt minute bound exceeds clock range".to_string())?;
    let packet = job
        .get("id")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "receipt watch read no packet id".to_string())?;
    let _dispatch_login = job
        .pointer("/metadata/agent")
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "receipt watch read no execution actor".to_string())?;
    if let Some(receipt) = job.get("metadata").and_then(|m| m.get(&d.receipt_key)) {
        let shape = receipt.as_object().is_some_and(|m| m.len() == 3);
        let assigned = step.get("assignee_id").and_then(Json::as_str);
        if !shape
            || receipt.get("schema").and_then(Json::as_u64) != Some(1)
            || receipt.get("run").and_then(Json::as_str) != Some(packet)
            || assigned.is_none_or(|a| a.is_empty())
            || receipt.get("actor").and_then(Json::as_str) != assigned
        {
            return Err("receipt schema, run or current assigned actor cannot be verified".into());
        }
        return Ok(None);
    }
    let dispatch = find(&d.dispatched_step)
        .filter(|s| s.get("status").and_then(Json::as_str) == Some("completed"))
        .ok_or_else(|| "receipt expectation has no completed dispatch step".to_string())?;
    let since = dispatch
        .get("completed_at")
        .and_then(Json::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| "receipt expectation has no readable native dispatch stamp".to_string())?;
    let due = since
        .checked_add_signed(duration)
        .ok_or_else(|| "receipt deadline exceeds clock range".to_string())?;
    Ok((now > due).then(|| Overdue {
        packet: packet.to_string(),
        since,
        bound_minutes: minutes,
    }))
}

pub struct JobsReceiptOverdue {
    client: boss_core::machine_token::Client,
    base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl JobsReceiptOverdue {
    pub fn new(
        base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            base: base.into(),
            owner,
        })
    }

    // Refuse a short or changing listing BEFORE any filing or recovery.
    // The existing walk permits an empty tail; this obligation cannot
    // call unread rows resolved merely because the far side stopped.
    async fn all(&self, filter: &[(&str, &str)], rule: &str) -> Result<Vec<Json>, HandlerError> {
        let mut out = Vec::new();
        let mut total = None;
        let mut seen = BTreeSet::new();
        loop {
            let mut url =
                reqwest::Url::parse(&format!("{}/api/jobs", self.base.trim_end_matches('/')))
                    .map_err(|e| HandlerError::Permanent(format!("jobs base: {e}")))?;
            {
                let mut query = url.query_pairs_mut();
                for (k, v) in filter {
                    query.append_pair(k, v);
                }
                query
                    .append_pair("full", "true")
                    .append_pair("limit", "500")
                    .append_pair("offset", &out.len().to_string());
            }
            let body = get_json(&self.client, url.as_str(), rule).await?;
            let count = body
                .get("total")
                .and_then(Json::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| {
                    HandlerError::Downstream("receipt watch listing has no readable total".into())
                })?;
            if total.is_some_and(|n| n != count) {
                return Err(HandlerError::Downstream(
                    "receipt watch listing changed during read".into(),
                ));
            }
            total = Some(count);
            let page: Vec<Json> = rows_or_refuse(&body, "receipt watch jobs listing")
                .map_err(HandlerError::Downstream)?;
            if page.is_empty() && out.len() != count {
                return Err(HandlerError::Downstream(
                    "receipt watch listing ended before its total".into(),
                ));
            }
            for row in page {
                let id = row
                    .get("id")
                    .and_then(Json::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        HandlerError::Downstream("receipt watch row has no id".into())
                    })?;
                if !seen.insert(id.to_string()) {
                    return Err(HandlerError::Downstream(
                        "receipt watch listing repeated an id".into(),
                    ));
                }
                out.push(row);
            }
            if out.len() > count {
                return Err(HandlerError::Downstream(
                    "receipt watch listing exceeds total".into(),
                ));
            }
            if out.len() == count {
                return Ok(out);
            }
        }
    }

    async fn resolve(
        &self,
        alarm: &Json,
        now: DateTime<Utc>,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let id = alarm["id"].as_str().unwrap_or_default();
        let evidence = format!(
            "{HANDLER} at {}: the expected receipt is recorded or its work is no longer open; no worker death or delivery is inferred.",
            now.to_rfc3339()
        );
        match retraction(alarm) {
            Some(Retraction::Complete { step_id, .. }) => {
                let step = alarm["steps"]
                    .as_array()
                    .and_then(|steps| {
                        steps
                            .iter()
                            .find(|step| step["id"].as_str() == Some(&step_id))
                    })
                    .ok_or_else(|| {
                        HandlerError::Downstream("receipt recovery step unavailable".into())
                    })?;
                if step["status"].as_str() == Some("active") {
                    return self
                        .annotate(
                            alarm,
                            &evidence,
                            now,
                            rule,
                            "an executor holds the recovery step; its work remains open",
                        )
                        .await;
                }
                if step["status"].as_str() != Some("ready") {
                    return Err(HandlerError::Downstream(
                        "receipt recovery step is not ready".into(),
                    ));
                }
                let holder = step
                    .get("assignee_id")
                    .filter(|holder| {
                        holder.is_null()
                            || holder
                                .as_str()
                                .is_some_and(|actor| !actor.trim().is_empty())
                    })
                    .ok_or_else(|| {
                        HandlerError::Downstream("receipt recovery holder unavailable".into())
                    })?;
                // Evidence and completion share the native row CAS. A claim
                // after this listing must preserve the claimant (review6c24).
                let operation = uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("{HANDLER}/{rule}/{id}/{step_id}/{}", now.to_rfc3339()).as_bytes(),
                );
                let body = json!({"operation_id":operation,
                    "expected":{"status":"ready","assignee_id":holder},
                    "evidence":withdrawal_fields(&evidence, HANDLER, &now.to_rfc3339())});
                let url = format!("{}/api/jobs/{id}/steps/{step_id}/complete-if", self.base);
                let response = self
                    .client
                    .post(&url)
                    .header("x-boss-user", dispatcher_actor_header(rule))
                    .header("x-sim-origin", sim_origin_value())
                    .json(&body)
                    .send()
                    .await
                    .map_err(|error| HandlerError::Downstream(format!("POST {url}: {error}")))?;
                let status = response.status();
                let outcome: Json = response.json().await.map_err(|error| {
                    HandlerError::Downstream(format!(
                        "POST {url} returned {status}, unreadable outcome: {error}"
                    ))
                })?;
                match (status, outcome["outcome"].as_str()) {
                    (reqwest::StatusCode::OK, Some("completed" | "replayed"))
                        if outcome["receipt"]["operation_id"] == json!(operation)
                        && outcome["receipt"]["job_id"].as_str() == Some(id)
                        && outcome["receipt"]["step_id"].as_str() == Some(&step_id)
                        && serde_json::from_value::<boss_jobs::conditional_completion::CompletionReceipt>(
                            outcome["receipt"].clone()).is_ok() => Ok(()),
                    (reqwest::StatusCode::CONFLICT, Some("precondition_failed"))
                        if outcome["step_id"].as_str() == Some(&step_id) =>
                        self.annotate(alarm, &evidence, now, rule,
                            "the recovery step changed after observation; its executor must judge the remaining work").await,
                    _ => Err(HandlerError::Downstream(format!("POST {url} returned {status}: {outcome}"))),
                }
            }
            Some(Retraction::Annotate { why_open })
                if alarm
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_none_or(Json::is_null) =>
            {
                self.annotate(alarm, &evidence, now, rule, &why_open).await
            }
            _ => Ok(()),
        }
    }

    async fn annotate(
        &self,
        alarm: &Json,
        evidence: &str,
        now: DateTime<Utc>,
        rule: &str,
        why_open: &str,
    ) -> Result<(), HandlerError> {
        if alarm
            .pointer(&format!("/metadata/{RECOVERED_AT}"))
            .is_some_and(|v| !v.is_null())
        {
            return Ok(());
        }
        write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!(
                "{}/api/jobs/{}/metadata",
                self.base,
                alarm["id"].as_str().unwrap_or_default()
            ),
            &Json::Object(recovery_note(
                evidence,
                HANDLER,
                &now.to_rfc3339(),
                why_open,
            )),
            rule,
        )
        .await
    }
}

#[async_trait]
impl Handler for JobsReceiptOverdue {
    fn name(&self) -> &'static str {
        HANDLER
    }
    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let d = declared(args)?;
        let rule = &ctx.rule_name;
        let now = ctx
            .event_payload
            .get("_at")
            .and_then(Json::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .ok_or_else(|| {
                HandlerError::Permanent("receipt watch requires a native sub-day clock tick".into())
            })?;
        let packets = self
            .all(&[("kind", &d.kind), ("status", "open")], rule)
            .await?;
        let late = packets
            .iter()
            .map(|p| judge(p, &d, now))
            .collect::<Result<Vec<_>, _>>()
            .map_err(HandlerError::Downstream)?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        // Recovery is a current receipt/work fact, never merely an older
        // replayed tick that makes an already-overdue deadline look young.
        let still_missing: BTreeSet<_> = packets
            .iter()
            .filter(|p| {
                p.get("metadata")
                    .and_then(|m| m.get(&d.receipt_key))
                    .is_none()
                    && p.get("steps")
                        .and_then(Json::as_array)
                        .is_some_and(|steps| {
                            steps.iter().any(|s| {
                                s.get("spec_slug").and_then(Json::as_str) == Some(d.step.as_str())
                                    && matches!(
                                        s.get("status").and_then(Json::as_str),
                                        Some("ready" | "active")
                                    )
                                    && s.get("metadata")
                                        .and_then(|m| m.get(&d.expectation_key))
                                        .is_some()
                            })
                        })
            })
            .filter_map(|p| p.get("id").and_then(Json::as_str))
            .map(|id| format!("{rule}:{id}"))
            .collect();
        let alarms = self
            .all(
                &[("kind", "backlog-item"), ("metadata_has", ALARM_KEY)],
                rule,
            )
            .await?;
        let mut filed = BTreeMap::new();
        for alarm in alarms {
            let key = alarm
                .pointer(&format!("/metadata/{ALARM_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| HandlerError::Downstream("receipt alarm has no watch key".into()))?;
            if filed.insert(key.to_string(), alarm).is_some() {
                return Err(HandlerError::Downstream(
                    "receipt watch read duplicate alarms".into(),
                ));
            }
        }
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;
        let keys: BTreeSet<_> = late
            .iter()
            .map(|o| format!("{rule}:{}", o.packet))
            .collect();
        for o in late {
            let key = format!("{rule}:{}", o.packet);
            if filed.contains_key(&key) {
                continue;
            }
            let id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!("{HANDLER}/{key}").as_bytes(),
            )
            .to_string();
            let body = json!({"id":id,"kind":"backlog-item","status":"open","priority":"urgent",
                "title":format!("Overdue receipt: {} {} ({} minute bound)",d.kind,o.packet,o.bound_minutes),
                "owner_id":owner,"subject":{"subject_kind":"custom","id":o.packet},"tags":[],
                "metadata":with_lane(json!({"area":"platform","scope":"overdue-receipt",ALARM_KEY:key,
                    "watch_rule":rule,"packet":o.packet,"dispatch_at":o.since.to_rfc3339(),
                    "bound_minutes":o.bound_minutes,"measured_at":now.to_rfc3339(),
                    "detail":format!("The declared {} receipt is not recorded after the {} minute bound from native dispatch {}. Host, session and ACTIVE are not receipts. This observation does not mark death, reclaim work or claim delivery.",d.receipt_key,o.bound_minutes,o.since.to_rfc3339())}),InputChannel::Telemetry)});
            if let Err(error) = post_json(
                &self.client,
                &format!("{}/api/jobs", self.base),
                &body,
                rule,
            )
            .await
            {
                // Deterministic create id protects concurrent ticks too.
                // A lost race is accepted only after the native winner is read.
                let winner =
                    get_json(&self.client, &format!("{}/api/jobs/{id}", self.base), rule).await?;
                let winner = row_or_refuse(winner, "receipt watch concurrent alarm winner")
                    .map_err(HandlerError::Downstream)?;
                if winner.get("id").and_then(Json::as_str) != Some(id.as_str())
                    || winner.get("kind").and_then(Json::as_str) != Some("backlog-item")
                    || winner
                        .pointer(&format!("/metadata/{ALARM_KEY}"))
                        .and_then(Json::as_str)
                        != Some(key.as_str())
                    || winner.pointer("/metadata/scope").and_then(Json::as_str)
                        != Some("overdue-receipt")
                {
                    return Err(error);
                }
            }
        }
        for (key, alarm) in filed {
            if key.starts_with(&format!("{rule}:"))
                && !keys.contains(&key)
                && !still_missing.contains(&key)
                && alarm.get("status").and_then(Json::as_str) == Some("open")
            {
                self.resolve(&alarm, now, rule).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn printed_prompt() -> serde_json::Value {
        // The native shape dispatch writes, not a fictitious missing host:
        // session and ACTIVE can both be supplied before worker execution.
        json!({"id":"run-1","kind":"agent-run","status":"open","owner_id":"owner",
            "metadata":{"agent":"agent-codex","host":"live-pod","session":"coordinator-session",
                "ignored_dispatch_metadata":true},
            "steps":[{"id":"brief-1","spec_slug":"briefed","status":"completed",
                "completed_at":"2026-10-03T12:00:00Z"},
                {"id":"build-1","spec_slug":"building","status":"active",
                 "assignee_id":"agent-codex","metadata":{"start_expected":{"schema":1,"minutes":15}}}]})
    }

    fn declaration() -> Declared {
        Declared {
            kind: "agent-run".into(),
            step: "building".into(),
            dispatched_step: "briefed".into(),
            expectation_key: "start_expected".into(),
            receipt_key: "worker_started".into(),
        }
    }

    #[test]
    fn a_printed_prompt_on_a_live_host_is_overdue_without_a_worker_receipt() {
        let job = printed_prompt();
        let late = judge(&job, &declaration(), at("2026-10-03T12:20:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(late.packet, "run-1");
        assert_eq!(late.since, at("2026-10-03T12:00:00Z"));
        assert_eq!(late.bound_minutes, 15);
        assert_eq!(
            job["steps"][1]["status"], "active",
            "observation never reaps work"
        );
    }

    #[test]
    fn a_recorded_receipt_spares_hooked_and_nonhook_workers_with_login_aliases() {
        for actor in ["agent-codex", "agent-claude"] {
            let mut job = printed_prompt();
            job["metadata"]["agent"] = json!(format!("{actor}@example.test"));
            job["steps"][1]["assignee_id"] = json!(actor);
            job["metadata"]["worker_started"] = json!({"schema":1,"run":"run-1","actor":actor});
            assert!(
                judge(&job, &declaration(), at("2026-10-03T12:20:00Z"))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn legacy_runs_and_terminal_runs_are_not_subject_to_the_new_expectation() {
        let mut job = printed_prompt();
        job["steps"][1]["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("start_expected");
        assert!(
            judge(&job, &declaration(), at("2026-10-04T12:00:00Z"))
                .unwrap()
                .is_none()
        );
        job["steps"][1]["metadata"]["start_expected"] = json!({"schema":1,"minutes":15});
        job["status"] = json!("closed");
        assert!(
            judge(&job, &declaration(), at("2026-10-04T12:00:00Z"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unreadable_or_wrong_identity_receipts_are_unavailable_not_clean() {
        for receipt in [
            json!({"schema":1,"run":"other","actor":"agent-codex"}),
            json!({"schema":1,"run":"run-1","actor":"other"}),
            json!({"schema":2,"run":"run-1","actor":"agent-codex"}),
            json!(null),
        ] {
            let mut job = printed_prompt();
            job["metadata"]["worker_started"] = receipt;
            assert!(judge(&job, &declaration(), at("2026-10-03T12:20:00Z")).is_err());
        }
        let mut job = printed_prompt();
        job["steps"][0]["completed_at"] = json!("unknown");
        assert!(judge(&job, &declaration(), at("2026-10-03T12:20:00Z")).is_err());
    }

    #[test]
    fn the_exact_bound_is_not_overdue_and_the_next_second_is() {
        assert!(
            judge(
                &printed_prompt(),
                &declaration(),
                at("2026-10-03T12:15:00Z")
            )
            .unwrap()
            .is_none()
        );
        assert!(
            judge(
                &printed_prompt(),
                &declaration(),
                at("2026-10-03T12:15:01Z")
            )
            .unwrap()
            .is_some()
        );
    }

    fn args() -> Vec<(String, boss_dispatcher::rules::expr::Value)> {
        use boss_dispatcher::rules::expr::Value;
        [
            ("kind", "agent-run"),
            ("step", "building"),
            ("dispatched_step", "briefed"),
            ("expectation_key", "start_expected"),
            ("receipt_key", "worker_started"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), Value::String(v.into())))
        .collect()
    }

    fn ctx() -> boss_dispatcher::rules::handler::InvocationContext {
        boss_dispatcher::rules::handler::InvocationContext {
            event_timestamp: None,
            rule_name: "receipt-watch".into(),
            triggering_event_id: "tick1".into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_at":"2026-10-03T12:20:00Z"}),
        }
    }

    #[tokio::test]
    async fn native_recovery_cas_preserves_a_claim_after_the_listing() {
        use axum::{
            Router,
            body::{Body, Bytes},
            http::{Method, Request, Uri},
            response::IntoResponse,
        };
        use boss_core::{
            actor::ActorId,
            job::{Job, JobStatus, Priority, Step, StepStatus, Subject},
            port::EventBus,
            publisher::{DomainPublisher, EventStamp},
        };
        use boss_jobs::{
            InMemoryJobs, JobsRepository,
            http::{JobsApiState, router},
        };
        use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
        use chrono::NaiveDate;
        use tower::ServiceExt;
        for claim_after_listing in [false, true] {
            let jobs = Arc::new(InMemoryJobs::new());
            let bus = boss_testing::RecordingEventBus::new();
            let app = router(JobsApiState::minimal(
                jobs.clone(),
                bus.clone(),
                DomainPublisher::new(bus as Arc<dyn EventBus>, "jobs"),
                Arc::new(
                    FakePolicyClient::builder()
                        .allow(
                            "platform-admin",
                            Action::Update,
                            Resource::step(),
                            Scope::All,
                        )
                        .allow(
                            "platform-admin",
                            Action::Update,
                            Resource::job(),
                            Scope::All,
                        )
                        .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
                        .build(),
                ),
                Arc::new(boss_clock_client::WallClockClient),
            ));
            let mut alarm = Job::new(
                "test-alarm",
                Subject::new("custom", "watch"),
                "Receipt alarm",
                "owner",
                Priority::Standard,
                NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
            );
            alarm.metadata = json!({"receipt_watch":"receipt-watch:run-1"});
            alarm.status = JobStatus::Open;
            jobs.create_job(&alarm).await.unwrap();
            let mut step = Step::new(alarm.id, "task", "Triage", 0);
            step.spec_slug = Some("triage".into());
            step.status = StepStatus::Ready;
            step.metadata = json!({"retained":"original evidence"});
            jobs.add_step(&step).await.unwrap();
            let mut listed = serde_json::to_value(&alarm).unwrap();
            listed["steps"] = json!([step]);
            let mut run = printed_prompt();
            run["metadata"]["worker_started"] =
                json!({"schema":1,"run":"run-1","actor":"agent-codex"});
            let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured = writes.clone();
            let claim_jobs = jobs.clone();
            let step_id = step.id;
            let proxy = Router::new().fallback(
                move |method: Method, uri: Uri, headers: axum::http::HeaderMap, body: Bytes| {
                    let app = app.clone();
                    let listed = listed.clone();
                    let run = run.clone();
                    let jobs = claim_jobs.clone();
                    let captured = captured.clone();
                    async move {
                        if method == Method::GET && uri.path() == "/api/jobs" {
                            let rows = if uri.query().unwrap_or_default().contains("kind=agent-run")
                            {
                                vec![run]
                            } else {
                                vec![listed]
                            };
                            return axum::Json(json!({"total":rows.len(),"data":rows}))
                                .into_response();
                        }
                        captured
                            .lock()
                            .unwrap()
                            .push(format!("{method} {}", uri.path()));
                        if claim_after_listing
                            && method == Method::POST
                            && uri.path().ends_with("/complete-if")
                        {
                            jobs.claim_step_at(
                                &step_id,
                                "emp-reviewer",
                                &EventStamp::new("jobs", ActorId::Human("emp-reviewer".into())),
                                &[],
                            )
                            .await
                            .unwrap();
                        }
                        let mut request = Request::builder().method(method).uri(uri);
                        *request.headers_mut().unwrap() = headers;
                        let response = app
                            .oneshot(request.body(Body::from(body)).unwrap())
                            .await
                            .unwrap();
                        let status = response.status();
                        let bytes = http_body_util::BodyExt::collect(response.into_body())
                            .await
                            .unwrap()
                            .to_bytes();
                        println!(
                            "native recovery response {status}: {}",
                            String::from_utf8_lossy(&bytes)
                        );
                        (status, bytes).into_response()
                    }
                },
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, proxy).await.unwrap() });
            let handler = JobsReceiptOverdue::new(
                format!("http://{address}"),
                Arc::new(boss_core::platform_owner::Fixed("owner".into())),
            );
            handler.invoke(&args(), &ctx()).await.unwrap();
            let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
            assert_eq!(stored.metadata["retained"], "original evidence");
            if claim_after_listing {
                assert_eq!(stored.status, StepStatus::Active);
                assert_eq!(stored.assignee_id.as_deref(), Some("emp-reviewer"));
                assert_eq!(
                    stored.metadata, step.metadata,
                    "no recovery evidence leaks into held work"
                );
                assert_eq!(
                    writes.lock().unwrap().len(),
                    2,
                    "conditional refusal then packet annotation"
                );
            } else {
                assert_eq!(stored.status, StepStatus::Completed);
                assert_eq!(stored.metadata["disposition"], "stale");
                assert_eq!(
                    writes.lock().unwrap().len(),
                    1,
                    "one atomic evidence/completion request"
                );
                let events = jobs.recorded_events();
                handler.invoke(&args(), &ctx()).await.unwrap();
                assert_eq!(
                    serde_json::to_value(jobs.recorded_events()).unwrap(),
                    serde_json::to_value(events).unwrap(),
                    "equal replay emits no new event"
                );
                assert_eq!(jobs.get_step(&step.id).await.unwrap().unwrap(), stored);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn receipt_recovery_preserves_active_downstream_work_and_deduplicates_notes() {
        for slug in ["measure", "build"] {
            for already_noted in [false, true] {
                let mut run = printed_prompt();
                run["metadata"]["worker_started"] =
                    json!({"schema":1,"run":"run-1","actor":"agent-codex"});
                let mut alarm = json!({"id":"alarm-1","status":"open",
                    "metadata":{"receipt_watch":"receipt-watch:run-1"},
                    "steps":[{"id":"triage-1","spec_slug":"triage","status":"completed",
                        "metadata":{"disposition":"build"}},
                        {"id":"work-1","spec_slug":slug,"status":"active","assignee_id":"emp-reviewer","metadata":{}}]});
                if already_noted {
                    alarm["metadata"][RECOVERED_AT] = json!("2026-10-03T12:19:00Z");
                }
                let stub = super::super::listing_stub::serve(vec![
                    (
                        "/api/jobs?kind=agent-run&status=open",
                        json!({"data":[run],"total":1}),
                    ),
                    (
                        "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                        json!({"data":[alarm],"total":1}),
                    ),
                ])
                .await;
                let handler = JobsReceiptOverdue::new(
                    stub.base.clone(),
                    Arc::new(boss_core::platform_owner::Fixed("owner".into())),
                );
                handler.invoke(&args(), &ctx()).await.unwrap();
                assert_eq!(
                    stub.writes(),
                    if already_noted {
                        vec![]
                    } else {
                        vec!["PATCH /api/jobs/alarm-1/metadata"]
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn receipt_recovery_preserves_another_executors_active_triage() {
        let mut run = printed_prompt();
        run["metadata"]["worker_started"] = json!({"schema":1,"run":"run-1","actor":"agent-codex"});
        let alarm = json!({"id":"alarm-1","status":"open",
            "metadata":{"receipt_watch":"receipt-watch:run-1"},
            "steps":[{"id":"triage-1","spec_slug":"triage","status":"active",
                "assignee_id":"emp-reviewer","metadata":{}}]});
        let stub = super::super::listing_stub::serve(vec![
            (
                "/api/jobs?kind=agent-run&status=open",
                json!({"data":[run],"total":1}),
            ),
            (
                "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                json!({"data":[alarm],"total":1}),
            ),
        ])
        .await;
        let handler = JobsReceiptOverdue::new(
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("owner".into())),
        );
        handler.invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(
            stub.writes(),
            vec!["PATCH /api/jobs/alarm-1/metadata"],
            "recovery annotates the packet without completing its executor's work"
        );
    }

    #[tokio::test]
    async fn a_missing_receipt_files_a_native_alarm_without_writing_the_run() {
        use boss_dispatcher::rules::handler::Handler;
        let stub = super::super::listing_stub::serve(vec![
            (
                "/api/jobs?kind=agent-run&status=open",
                json!({"data":[printed_prompt()],"total":1}),
            ),
            (
                "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                json!({"data":[],"total":0}),
            ),
        ])
        .await;
        let handler = JobsReceiptOverdue::new(
            stub.base.clone(),
            std::sync::Arc::new(boss_core::platform_owner::Fixed("owner".into())),
        );
        handler.invoke(&args(), &ctx()).await.unwrap();
        assert_eq!(stub.writes(), vec!["POST /api/jobs"]);
        let body = &stub.sent()[0].1;
        assert_eq!(body["metadata"]["receipt_watch"], "receipt-watch:run-1");
        assert_eq!(body["metadata"]["bound_minutes"], 15);
        assert!(
            body["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("receipt")
        );
        assert_eq!(body["priority"], "urgent");
    }

    #[tokio::test]
    async fn partial_listings_and_malformed_receipts_refuse_before_any_write() {
        use boss_dispatcher::rules::handler::Handler;
        for body in [json!({"data":[],"total":1}), json!({"total":1}), {
            let mut j = printed_prompt();
            j["metadata"]["worker_started"] = json!({"schema":2});
            json!({"data":[j],"total":1})
        }] {
            let stub = super::super::listing_stub::serve(vec![
                ("/api/jobs?kind=agent-run&status=open", body),
                (
                    "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                    json!({"data":[],"total":0}),
                ),
            ])
            .await;
            let handler = JobsReceiptOverdue::new(
                stub.base.clone(),
                std::sync::Arc::new(boss_core::platform_owner::Fixed("owner".into())),
            );
            assert!(handler.invoke(&args(), &ctx()).await.is_err());
            assert!(stub.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn duplicate_ticks_keep_one_alarm_without_a_second_filing() {
        use boss_dispatcher::rules::handler::Handler;
        let alarm = json!({"id":"alarm-1","status":"open","metadata":{"receipt_watch":"receipt-watch:run-1"},
            "steps":[{"id":"triage-1","spec_slug":"triage","status":"ready","metadata":{}}]});
        for _ in 0..2 {
            let j = printed_prompt();
            let stub = super::super::listing_stub::serve(vec![
                (
                    "/api/jobs?kind=agent-run&status=open",
                    json!({"data":[j],"total":1}),
                ),
                (
                    "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                    json!({"data":[alarm.clone()],"total":1}),
                ),
            ])
            .await;
            let handler = JobsReceiptOverdue::new(
                stub.base.clone(),
                std::sync::Arc::new(boss_core::platform_owner::Fixed("owner".into())),
            );
            handler.invoke(&args(), &ctx()).await.unwrap();
            assert!(stub.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn an_earlier_clock_tick_cannot_resolve_a_still_missing_receipt() {
        use boss_dispatcher::rules::handler::Handler;
        let alarm = json!({"id":"alarm-1","status":"open",
            "metadata":{"receipt_watch":"receipt-watch:run-1","measured_at":"2026-10-03T12:20:00Z"},
            "steps":[{"id":"triage-1","spec_slug":"triage","status":"ready","metadata":{}}]});
        let stub = super::super::listing_stub::serve(vec![
            (
                "/api/jobs?kind=agent-run&status=open",
                json!({"data":[printed_prompt()],"total":1}),
            ),
            (
                "/api/jobs?kind=backlog-item&metadata_has=receipt_watch",
                json!({"data":[alarm],"total":1}),
            ),
        ])
        .await;
        let handler = JobsReceiptOverdue::new(
            stub.base.clone(),
            std::sync::Arc::new(boss_core::platform_owner::Fixed("owner".into())),
        );
        let mut earlier = ctx();
        earlier.event_payload["_at"] = json!("2026-10-03T12:10:00Z");
        handler.invoke(&args(), &earlier).await.unwrap();
        assert!(
            stub.writes().is_empty(),
            "older clock time is not a recovered receipt"
        );
    }

    #[tokio::test]
    async fn a_create_race_accepts_only_the_expected_bare_native_alarm_row() {
        use axum::{
            Router,
            http::{Method, StatusCode, Uri},
            response::IntoResponse,
        };
        for shape in [
            "bare",
            "envelope",
            "missing-id",
            "wrong-id",
            "wrong-kind",
            "wrong-key",
            "wrong-scope",
        ] {
            let app = Router::new().fallback(move |method: Method, uri: Uri| async move {
                if method == Method::POST {
                    return StatusCode::CONFLICT.into_response();
                }
                if uri.path() == "/api/jobs" {
                    let rows = if uri.query().unwrap_or_default().contains("kind=agent-run") {
                        vec![printed_prompt()]
                    } else {
                        vec![]
                    };
                    return axum::Json(json!({"total":rows.len(),"data":rows})).into_response();
                }
                let id = uri.path().strip_prefix("/api/jobs/").unwrap();
                let mut row = json!({"id":id,"kind":"backlog-item", "metadata":{
                    "receipt_watch":"receipt-watch:run-1", "scope":"overdue-receipt"}});
                match shape {
                    "envelope" => row = json!({"data":row}),
                    "missing-id" => {
                        row.as_object_mut().unwrap().remove("id");
                    }
                    "wrong-id" => row["id"] = json!("other"),
                    "wrong-kind" => row["kind"] = json!("other"),
                    "wrong-key" => row["metadata"]["receipt_watch"] = json!("other"),
                    "wrong-scope" => row["metadata"]["scope"] = json!("other"),
                    _ => {}
                }
                axum::Json(row).into_response()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let handler = JobsReceiptOverdue::new(
                format!("http://{address}"),
                Arc::new(boss_core::platform_owner::Fixed("owner".into())),
            );
            let result = handler.invoke(&args(), &ctx()).await;
            server.abort();
            assert_eq!(
                result.is_ok(),
                shape == "bare",
                "race winner shape {shape}: {result:?}"
            );
        }
    }

    #[test]
    fn the_shipped_registry_rule_judges_the_admitted_protocol_expectation() {
        use boss_dispatcher::rules::{
            expr::{Context, NoHelpers, eval},
            registry::Registry,
        };
        let root = boss_testing::repo_root();
        let src = std::fs::read_to_string(root.join(
            "infra/dispatcher/rules/a-declared-worker-receipt-is-observed-every-five-minutes.toml",
        ))
        .unwrap();
        let registry = Registry::from_toml(&src).unwrap();
        assert_eq!(registry.rules().len(), 1);
        let rule = &registry.rules()[0];
        let action = &rule.do_steps[0];
        assert_eq!(action.handler, HANDLER);
        let payload = json!({"_at":"2026-10-03T12:20:00Z"});
        let values = action
            .args
            .iter()
            .map(|(key, expression)| {
                (
                    key.clone(),
                    eval(
                        expression,
                        &Context {
                            payload: &payload,
                            helpers: &NoHelpers,
                        },
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let d = declared(&values).unwrap();
        let workflow = boss_jobs::registry::seedable_platform_workflows()
            .into_iter()
            .find(|w| w.kind == d.kind)
            .unwrap();
        let building = workflow.steps.iter().find(|s| s.title == d.step).unwrap();
        let expectation = building.metadata_defaults.get(&d.expectation_key).unwrap();
        let mut job = printed_prompt();
        job["steps"][1]["metadata"] = json!({d.expectation_key.clone():expectation});
        assert!(
            judge(&job, &d, at("2026-10-03T12:20:00Z"))
                .unwrap()
                .is_some()
        );
        job["metadata"][&d.receipt_key] = json!({"schema":1,"run":"run-1","actor":"agent-codex"});
        assert!(
            judge(&job, &d, at("2026-10-03T12:20:00Z"))
                .unwrap()
                .is_none()
        );
    }
}
