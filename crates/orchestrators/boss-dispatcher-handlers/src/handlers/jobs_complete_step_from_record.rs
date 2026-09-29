//! `jobs.complete_step_from_record` — a step going ready is completed
//! from what its own packet already holds.
//!
//! The gap this closes (backlog b951c00a, 2026-09-28): a builder runs
//! `boss dispatch <run> --report` at handback, which is usually BEFORE
//! its gate reaches a verdict — the harness backgrounds `boss gate
//! --wait` at ten minutes. The report is written onto the run packet at
//! once (`metadata.report`, with `spend_usd` and `tokens` beside it),
//! but the run's `reported` step opens only when `building` closes
//! `gated` or `delivered`, so the step the run lands on was never
//! completed by anything. Measured that day: six runs re-reported by
//! hand, each 32 to 113 minutes after its green, while the shop floor
//! read TROUBLED at "7 runs finished and not reported — each holds a
//! slot" and dispatch refused at the cap — and run e737a54f, whose
//! handback sat on the packet the whole time, was aged out by the clock
//! as `handback = absent`, a false record.
//!
//! So when the step goes ready, this reads the packet and, if the
//! record the rule names is there, completes the step from it. The
//! words are the agent's own, copied — not a summary the machine
//! invents, so "no rule writes the report" still means what it said:
//! no rule AUTHORS one.
//!
//! ## Why this is a handler and not agent-run code
//!
//! Every noun is a rule arg, the same shape `jobs.age_out_step` and
//! `jobs.complete_linked_step` keep:
//!
//! - `kind`, `step` — the packet kind and the step's `spec_slug`. The
//!   rule's `when` already names both off the `step.ready.<kind>`
//!   payload; the handler checks them again on the packet it reads,
//!   because the shared `step.ready.task` topic carries every task step
//!   on the board and a rule authored without the `when` must still
//!   touch nothing else.
//! - `requires` — the packet metadata key whose value IS the record.
//!   Absent, null or empty, there is nothing to land and nothing is
//!   written: the step waits for a hand, as it did before this rule.
//! - `record` — a JSON object, step field -> packet metadata key. Each
//!   present scalar is copied as TEXT (the step's fields are strings;
//!   the packet's `spend_usd` and `tokens` are numbers, and `boss
//!   dispatch --report` writes them onto the step the same way).
//! - `done_metadata` — fixed keys, as every completing handler takes
//!   them; `handback = recorded` for a run.
//! - `evidence_key` — where the provenance lands (default
//!   `from_record`): the rule, the triggering event, the keys copied.
//!
//! A value already on the step is its writer's record: every key fills
//! ABSENT ones only, and rides the step merge door (e39a9d2a).
//!
//! ## Idempotence
//!
//! JetStream is at-least-once. A completed step is not open, so a
//! redelivery reads the packet, finds the step closed, and writes
//! nothing. A builder whose own `--report` completes the step first
//! wins the same way.

use super::common::{api_client, complete_step, get_json};
use super::jobs_complete_linked_step::{is_open, is_unset, step_by_slug, template_arg};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use serde_json::json;
use std::sync::Arc;

/// Default metadata key the provenance lands under on the completed
/// step. Overridable per rule via the `evidence_key` arg.
const DEFAULT_EVIDENCE_KEY: &str = "from_record";

pub struct JobsCompleteStepFromRecord {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsCompleteStepFromRecord {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    /// Construct with a custom reqwest client (tests point it at a
    /// local stand-in for jobs-api).
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// Does the packet hold a record under `key` — a value that is neither
/// absent, null nor an empty string? One definition, shared with
/// `jobs.age_out_step`'s `unless_job_holds`, so the clock spares
/// exactly the packets this handler can land (CLAUDE.md §9a).
pub(crate) fn job_holds(job: &serde_json::Value, key: &str) -> bool {
    match job.get("metadata").and_then(|m| m.get(key)) {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

/// A scalar as the text a string field holds; `None` for null and for
/// an object or a list, which no string field can carry honestly.
fn as_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// What the rule is, read off its args once.
#[derive(Debug, Clone)]
pub(crate) struct Landing<'a> {
    pub kind: &'a str,
    pub step: &'a str,
    pub requires: &'a str,
    pub record: serde_json::Map<String, serde_json::Value>,
    pub done: serde_json::Map<String, serde_json::Value>,
    pub evidence_key: &'a str,
}

/// PURE: the fields that complete step `step_id` on `job`, or `None`
/// when this firing is not the rule's to act on — another kind, another
/// step, a step no longer open, or a packet holding no record.
pub(crate) fn landing_fields(
    job: &serde_json::Value,
    step_id: &str,
    l: &Landing<'_>,
    provenance: serde_json::Value,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if job.get("kind").and_then(|v| v.as_str()) != Some(l.kind) {
        return None;
    }
    let step = step_by_slug(job, l.step)?;
    if step.get("id").and_then(|v| v.as_str()) != Some(step_id) || !is_open(step) {
        return None;
    }
    if !job_holds(job, l.requires) {
        return None;
    }
    let md = job.get("metadata")?;
    let existing = step.get("metadata").and_then(|m| m.as_object());
    let unset = |k: &str| is_unset(existing.and_then(|m| m.get(k)));
    let mut copied = Vec::new();
    let mut fields = serde_json::Map::new();
    for (field, source) in &l.record {
        let Some(source) = source.as_str() else {
            continue;
        };
        if !unset(field) {
            continue;
        }
        if let Some(text) = md.get(source).and_then(as_text) {
            fields.insert(field.clone(), json!(text));
            copied.push(source.to_string());
        }
    }
    for (k, v) in &l.done {
        if unset(k) && !fields.contains_key(k) {
            fields.insert(k.clone(), v.clone());
        }
    }
    let mut evidence = provenance;
    evidence["copied"] = json!(copied);
    fields.insert(l.evidence_key.to_string(), evidence);
    Some(fields)
}

#[async_trait]
impl Handler for JobsCompleteStepFromRecord {
    fn name(&self) -> &'static str {
        "jobs.complete_step_from_record"
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let step = arg_string(args, "step")?;
        let requires = arg_string(args, "requires")?;
        // A record that is not an object is rule authoring — no
        // redelivery repairs it.
        let record = template_arg(args, "record", &ctx.rule_name).ok_or_else(|| {
            HandlerError::Permanent(
                "record must be a JSON object of step field -> packet metadata key".into(),
            )
        })?;
        let done = template_arg(args, "done_metadata", &ctx.rule_name).unwrap_or_default();
        let evidence_key = match arg(args, "evidence_key") {
            Some(Value::String(s)) if !s.is_empty() => s.as_str(),
            _ => DEFAULT_EVIDENCE_KEY,
        };
        let (Some(job_id), Some(step_id)) = (
            ctx.event_payload.get("job_id").and_then(|v| v.as_str()),
            ctx.event_payload.get("step_id").and_then(|v| v.as_str()),
        ) else {
            return Ok(());
        };
        let job = get_json(
            &self.client,
            &format!("{}/api/jobs/{job_id}", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let landing = Landing {
            kind,
            step,
            requires,
            record,
            done,
            evidence_key,
        };
        let provenance = json!({
            "rule": ctx.rule_name,
            "event": ctx.triggering_event_id,
            "from": requires,
        });
        let Some(fields) = landing_fields(&job, step_id, &landing, provenance) else {
            return Ok(());
        };
        complete_step(
            &self.client,
            self.base(),
            job_id,
            step_id,
            fields,
            &ctx.rule_name,
        )
        .await?;
        tracing::info!(
            rule = %ctx.rule_name,
            packet = %job_id,
            "`{step}` completed from the packet's own `{requires}`"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, routing::get};
    use std::sync::Mutex;

    const RUN: &str = "44444444-4444-4444-4444-444444444444";

    fn ctx(payload: serde_json::Value) -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "agent-run-lands-a-report-sent-before-green".into(),
            triggering_event_id: "ev-1".into(),
            triggering_topic: "step.ready.task".into(),
            event_payload: payload,
        }
    }

    /// The rule as `infra/dispatcher/rules/` authors it.
    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".into(), Value::String("agent-run".into())),
            ("step".into(), Value::String("reported".into())),
            ("requires".into(), Value::String("report".into())),
            (
                "record".into(),
                Value::String(
                    r#"{"summary": "report", "spend_usd": "spend_usd", "tokens": "tokens"}"#.into(),
                ),
            ),
            (
                "done_metadata".into(),
                Value::String(r#"{"handback": "recorded"}"#.into()),
            ),
        ]
    }

    /// A run whose gate just went green: `building` closed `gated`,
    /// `reported` is ready, and the builder's pre-green `--report` is
    /// on the packet.
    fn run(report: Option<&str>) -> serde_json::Value {
        let mut metadata = json!({ "packet": "p", "step": "build", "tokens": 761000,
                                   "spend_usd": 4.2 });
        if let Some(r) = report {
            metadata["report"] = json!(r);
        }
        json!({
            "id": RUN, "kind": "agent-run", "status": "open", "metadata": metadata,
            "steps": [
                { "id": "run-building", "spec_slug": "building", "status": "completed",
                  "metadata": { "result": "gated" } },
                { "id": "run-reported", "spec_slug": "reported", "status": "ready",
                  "metadata": { "authority_role": "platform-admin" } },
            ],
        })
    }

    fn landing(a: &[(String, Value)]) -> Landing<'_> {
        let s = |k: &str| match arg(a, k) {
            Some(Value::String(v)) => v.as_str(),
            _ => "",
        };
        Landing {
            kind: s("kind"),
            step: s("step"),
            requires: s("requires"),
            record: template_arg(a, "record", "t").unwrap(),
            done: template_arg(a, "done_metadata", "t").unwrap(),
            evidence_key: DEFAULT_EVIDENCE_KEY,
        }
    }

    /// THE PIN (b951c00a): a report sent before the green lands on the
    /// green — the agent's own words as the summary, the cost copied as
    /// the step's text fields, `handback = recorded`, and where it came
    /// from beside it.
    #[test]
    fn a_report_on_the_packet_completes_the_step_in_the_agents_words() {
        let a = args();
        let f = landing_fields(
            &run(Some("packet x, branch y, sha z")),
            "run-reported",
            &landing(&a),
            json!({ "rule": "r" }),
        )
        .expect("a run with a report on the packet lands");
        assert_eq!(f["summary"], "packet x, branch y, sha z");
        assert_eq!(f["spend_usd"], "4.2");
        assert_eq!(f["tokens"], "761000");
        assert_eq!(f["handback"], "recorded");
        let mut copied: Vec<&str> = f["from_record"]["copied"]
            .as_array()
            .expect("the copied keys are listed")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        copied.sort();
        assert_eq!(copied, ["report", "spend_usd", "tokens"]);
        assert!(
            f.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them"
        );
    }

    /// No report on the packet, nothing written: the step waits for a
    /// hand, and the clock's `unreported` ending stays true of it. An
    /// empty or whitespace report is not a report.
    #[test]
    fn a_packet_holding_no_report_is_left_alone() {
        let a = args();
        let l = landing(&a);
        for r in [None, Some(""), Some("   ")] {
            assert!(
                landing_fields(&run(r), "run-reported", &l, json!({})).is_none(),
                "{r:?}"
            );
        }
        let mut null = run(None);
        null["metadata"]["report"] = serde_json::Value::Null;
        assert!(landing_fields(&null, "run-reported", &l, json!({})).is_none());
    }

    /// Another kind, another step, or a step no longer open is not this
    /// firing's to act on — the shared topic carries every task step.
    #[test]
    fn only_the_named_open_step_of_the_named_kind_is_touched() {
        let a = args();
        let l = landing(&a);
        let mut other = run(Some("r"));
        other["kind"] = json!("backlog-item");
        assert!(landing_fields(&other, "run-reported", &l, json!({})).is_none());
        assert!(landing_fields(&run(Some("r")), "run-building", &l, json!({})).is_none());
        let mut done = run(Some("r"));
        done["steps"][1]["status"] = json!("completed");
        assert!(landing_fields(&done, "run-reported", &l, json!({})).is_none());
    }

    /// A value already on the step is its writer's record.
    #[test]
    fn a_value_on_the_step_is_never_overwritten() {
        let a = args();
        let mut r = run(Some("the packet's copy"));
        r["steps"][1]["metadata"]["summary"] = json!("what the operator wrote");
        let f = landing_fields(&r, "run-reported", &landing(&a), json!({})).unwrap();
        assert!(f.get("summary").is_none(), "{f:?}");
        assert_eq!(f["handback"], "recorded");
    }

    #[test]
    fn job_holds_reads_only_a_real_value() {
        assert!(job_holds(
            &json!({ "metadata": { "report": "x" } }),
            "report"
        ));
        assert!(job_holds(&json!({ "metadata": { "n": 0 } }), "n"));
        assert!(!job_holds(
            &json!({ "metadata": { "report": " " } }),
            "report"
        ));
        assert!(!job_holds(
            &json!({ "metadata": { "report": null } }),
            "report"
        ));
        assert!(!job_holds(&json!({ "metadata": {} }), "report"));
        assert!(!job_holds(&json!({}), "report"));
    }

    type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// A jobs API holding one run: GET it, the step merge door, the
    /// status PUT — which flips the stored step, so a redelivery reads
    /// it closed.
    async fn mock(job: serde_json::Value) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let stored = Arc::new(Mutex::new(job));
        let (g, u) = (stored.clone(), stored);
        let (pw, uw) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move || {
                    let g = g.clone();
                    async move { Json(g.lock().unwrap().clone()) }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                axum::routing::patch(
                    move |Path((id, sid)): Path<(String, String)>,
                          Json(b): Json<serde_json::Value>| {
                        let pw = pw.clone();
                        async move {
                            pw.lock().unwrap().push((id, format!("{sid}/metadata"), b));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                axum::routing::put(
                    move |Path((id, sid)): Path<(String, String)>,
                          Json(b): Json<serde_json::Value>| {
                        let (u, uw) = (u.clone(), uw.clone());
                        async move {
                            if let Some(steps) = u.lock().unwrap()["steps"].as_array_mut() {
                                for s in steps.iter_mut().filter(|s| s["id"] == json!(sid)) {
                                    s["status"] = b["status"].clone();
                                }
                            }
                            uw.lock().unwrap().push((id, sid, b));
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    fn ready(step_id: &str) -> serde_json::Value {
        json!({ "job_id": RUN, "step_id": step_id, "kind": "task",
                "workflow_kind": "agent-run", "spec_slug": "reported" })
    }

    /// The firing end to end: one merge and one flip, and a redelivery
    /// of the same event writes nothing more.
    #[tokio::test]
    async fn the_green_lands_the_report_once() {
        let (base, writes) = mock(run(Some("packet x, branch y"))).await;
        let h =
            JobsCompleteStepFromRecord::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the firing runs");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "the merge, then the flip: {w:?}");
        assert_eq!(w[0].1, "run-reported/metadata");
        assert_eq!(w[0].2["summary"], "packet x, branch y");
        assert_eq!(w[0].2["from_record"]["event"], "ev-1");
        assert_eq!(w[1].2, json!({ "status": "completed" }));

        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the redelivery runs");
        assert_eq!(writes.lock().unwrap().len(), 2, "nothing is written twice");
    }

    /// A record that is not an object is rule authoring, and permanent.
    #[tokio::test]
    async fn a_record_that_is_not_an_object_is_permanent() {
        let mut a = args();
        a[3].1 = Value::String("report".into());
        let h = JobsCompleteStepFromRecord::with_client(
            crate::handlers::common::api_client(),
            "http://unused",
        );
        let err = h
            .invoke(&a, &ctx(ready("run-reported")))
            .await
            .expect_err("no record, no landing");
        assert!(matches!(err, HandlerError::Permanent(_)), "{err}");
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsCompleteStepFromRecord::with_client(
            crate::handlers::common::api_client(),
            "http://unused",
        );
        assert_eq!(h.name(), "jobs.complete_step_from_record");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.complete_step_from_record")
                .cloned(),
            Some(vec!["jobs.step.completed"]),
            "the cascade table knows what this handler emits"
        );
    }
}
