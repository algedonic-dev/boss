//! Common helpers shared across step-completion handlers.
//!
//! All step-completion handlers follow the same shape: read the
//! triggering `step.done.<kind>` event payload, extract step
//! metadata + subject + day, build an HTTP body, POST it. These
//! helpers cut the boilerplate to ~5 lines per handler.

use crate::rules::handler::HandlerError;
use serde_json::Value;

/// Step-event payload fields the handlers commonly read.
///
/// The `step.done.<kind>` event published by jobs-api carries this
/// shape inside its `payload` envelope. The dispatcher unwraps the
/// envelope; handlers see this inner shape as `ctx.event_payload`.
#[derive(Debug)]
pub struct StepEvent<'a> {
    pub job_id: &'a str,
    pub step_id: &'a str,
    pub kind: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub completed_on: Option<chrono::NaiveDate>,
    pub metadata: &'a serde_json::Map<String, Value>,
}

impl<'a> StepEvent<'a> {
    /// Extract the canonical fields from a step.done payload.
    /// Returns a tightly-typed view that handlers consume; errors
    /// surface as HandlerError::Downstream with a clear shape-mismatch
    /// message for the operator.
    pub fn from_payload(payload: &'a Value) -> Result<Self, HandlerError> {
        let obj = payload
            .as_object()
            .ok_or_else(|| HandlerError::Downstream("step.done payload is not an object".into()))?;

        let job_id = obj
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing job_id".into()))?;
        let step_id = obj
            .get("step_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing step_id".into()))?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing kind".into()))?;
        let subject_kind = obj
            .get("subject_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_id = obj.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
        let completed_on = obj
            .get("completed_on")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        let metadata = obj
            .get("metadata")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                HandlerError::Downstream("step.done payload missing metadata object".into())
            })?;

        Ok(StepEvent {
            job_id,
            step_id,
            kind,
            subject_kind,
            subject_id,
            completed_on,
            metadata,
        })
    }

    /// Convenience: pull a string field from step metadata, with a
    /// fallback closure for the common subject-derived defaults.
    pub fn meta_string_or<F: FnOnce(&Self) -> String>(&self, key: &str, fallback: F) -> String {
        self.metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| fallback(self))
    }
}

/// Parse a `YYYY-MM-DD` string out of an optional JSON value, e.g. a
/// step-metadata field. Returns `None` when the value is absent, not a
/// string, or not a valid date — leaving the fallback to the caller.
pub(crate) fn parse_date(v: Option<&Value>) -> Option<chrono::NaiveDate> {
    v.and_then(|v| v.as_str())
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// Build the `x-boss-user` header value for dispatcher-side
/// API calls. Per the rule-as-actor model in the dispatcher design
/// doc: every dispatcher-fired event names the rule as actor, with
/// `executed_by = automation:dispatcher` distinct from `actor`.
pub fn dispatcher_actor_header(rule_name: &str) -> String {
    serde_json::json!({
        "id": format!("rule:{}", rule_name),
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// POST a JSON body to a downstream service, stamping the dispatcher's
/// rule-as-actor `x-boss-user` header, and map a non-2xx response into a
/// `HandlerError::Downstream`.
///
/// This is the shared epilogue every step-completion handler ends with:
/// build the POST, attach `content-type: application/json` +
/// `x-boss-user: dispatcher_actor_header(rule_name)`, send, and turn a
/// transport failure or non-success status into a `Downstream` error with
/// the URL/status/body baked into the message. Handlers whose epilogue
/// differs (a PUT, a response-body read, a lenient no-fail webhook, or an
/// omitted header) keep their inline call.
pub(crate) async fn post_json(
    client: &reqwest::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(HandlerError::Downstream(format!(
            "POST {url} returned {status}: {body}"
        )));
    }
    Ok(())
}
