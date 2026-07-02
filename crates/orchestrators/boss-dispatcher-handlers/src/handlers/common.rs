//! Common helpers shared across step-completion handlers.
//!
//! All step-completion handlers follow the same shape: read the
//! triggering `step.done.<kind>` event payload, extract step
//! metadata + subject + day, build an HTTP body, POST it. These
//! helpers cut the boilerplate to ~5 lines per handler.

use boss_dispatcher::rules::handler::HandlerError;
use serde::Deserialize;
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

/// One production-overhead driver absorbed into WIP at a production
/// step: `amount_cents` capitalized DR 1310 / CR `credit_account`.
/// Stamped by the step author in the `overhead_absorbed` metadata array
/// — one entry per granular driver (direct labor, process utilities,
/// production depreciation, …), so the books model burden as separable
/// drivers rather than a single $/bbl plug.
#[derive(Debug, Deserialize)]
pub(crate) struct OverheadAbsorbed {
    pub(crate) amount_cents: i64,
    pub(crate) credit_account: String,
    #[serde(default)]
    pub(crate) memo: Option<String>,
}

/// Parse the `overhead_absorbed` array from a step's metadata — the ONE
/// parser both halves of the absorption contract share. The absorb side
/// (`inventory.parts.consume`) posts exactly these rows; the drain side
/// (`products.produce` drain-actual-wip) reconstructs fact ids from
/// exactly these rows via [`overhead_source_id`]. Deriving both from one
/// parse means what gets capitalized and what gets drained can never
/// disagree.
///
/// Tolerances, each deliberate:
/// - missing array → empty: absorption is optional; steps (or tenants)
///   that don't model burden simply skip it;
/// - `amount_cents == 0` → row silently skipped: the authoring idiom
///   for disabling a driver without deleting its row;
/// - a row that fails to deserialize, or a negative amount → row
///   skipped WITH a warn. A present-but-broken row is an authoring
///   error, not a policy choice — silence here would quietly leave a
///   driver's cost in period OpEx with nothing flagging the gap;
/// - rows sharing a `credit_account` are aggregated (amounts summed,
///   memos joined). The fact store keys one row per (step, account) —
///   `(kind, source_table, source_id)` unique — so un-aggregated
///   duplicates would collide there and the later row's cents would
///   silently vanish from the books.
pub(crate) fn overhead_absorbed(
    meta: &serde_json::Map<String, Value>,
    ctx: &str,
) -> Vec<OverheadAbsorbed> {
    let Some(rows) = meta.get("overhead_absorbed").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<OverheadAbsorbed> = Vec::new();
    for r in rows {
        let row = match serde_json::from_value::<OverheadAbsorbed>(r.clone()) {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!("overhead_absorbed row dropped ({ctx}): {e} — row: {r}");
                continue;
            }
        };
        if row.amount_cents < 0 {
            tracing::warn!(
                "overhead_absorbed row dropped ({ctx}): negative amount_cents {}",
                row.amount_cents
            );
            continue;
        }
        if row.amount_cents == 0 {
            continue;
        }
        match out
            .iter_mut()
            .find(|o| o.credit_account == row.credit_account)
        {
            Some(existing) => {
                existing.amount_cents = existing.amount_cents.saturating_add(row.amount_cents);
                existing.memo = match (existing.memo.take(), row.memo) {
                    (Some(a), Some(b)) => Some(format!("{a}; {b}")),
                    (a, b) => a.or(b),
                };
            }
            None => out.push(row),
        }
    }
    out
}

/// The absorption fact's `source_id` for one driver:
/// `overhead-absorbed@{step_id}:{credit_account}`. Mirrors the id the
/// inventory absorption endpoint mints (`overhead_absorbed_handler`,
/// boss-inventory `http/items.rs`) — these two `format!`s are the
/// write/reconstruct halves of one contract; change them together.
pub(crate) fn overhead_source_id(step_id: &str, credit_account: &str) -> String {
    format!("overhead-absorbed@{step_id}:{credit_account}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn overhead_absorbed_reads_each_driver() {
        let m = meta(json!({
            "overhead_absorbed": [
                { "amount_cents": 578_280, "credit_account": "6100", "memo": "Direct labor" },
                { "amount_cents": 88_480,  "credit_account": "6300" },
                { "amount_cents": 135_880, "credit_account": "6900" }
            ]
        }));
        let abs = overhead_absorbed(&m, "test");
        assert_eq!(abs.len(), 3);
        assert_eq!(abs[0].credit_account, "6100");
        assert_eq!(abs[0].memo.as_deref(), Some("Direct labor"));
        assert_eq!(abs[1].amount_cents, 88_480);
        assert!(abs[2].memo.is_none());
    }

    #[test]
    fn overhead_absorbed_drops_nonpositive_missing_and_malformed() {
        // No array → nothing.
        assert!(overhead_absorbed(&meta(json!({})), "test").is_empty());
        // Zero (disabled driver) and negative (warned) amounts are
        // dropped; a malformed row (string amount) is dropped with a
        // warn instead of poisoning the whole array.
        let m = meta(json!({
            "overhead_absorbed": [
                { "amount_cents": 0,        "credit_account": "6100" },
                { "amount_cents": -5,       "credit_account": "6300" },
                { "amount_cents": "578280", "credit_account": "6100" },
                { "amount_cents": 1_000,    "credit_account": "6900" }
            ]
        }));
        let abs = overhead_absorbed(&m, "test");
        assert_eq!(abs.len(), 1);
        assert_eq!(abs[0].credit_account, "6900");
    }

    #[test]
    fn overhead_absorbed_aggregates_same_account_rows() {
        // Two drivers crediting the same account must merge: the fact
        // store keys one row per (step, account), so posting them
        // separately would silently drop the second amount on the
        // unique-key conflict.
        let m = meta(json!({
            "overhead_absorbed": [
                { "amount_cents": 578_280, "credit_account": "6100", "memo": "Brewhouse labor" },
                { "amount_cents": 100_000, "credit_account": "6100", "memo": "Cellar labor" },
                { "amount_cents": 88_480,  "credit_account": "6300" }
            ]
        }));
        let abs = overhead_absorbed(&m, "test");
        assert_eq!(abs.len(), 2);
        assert_eq!(abs[0].credit_account, "6100");
        assert_eq!(abs[0].amount_cents, 678_280);
        assert_eq!(
            abs[0].memo.as_deref(),
            Some("Brewhouse labor; Cellar labor")
        );
        assert_eq!(abs[1].credit_account, "6300");
    }

    #[test]
    fn overhead_source_id_matches_endpoint_contract() {
        assert_eq!(
            overhead_source_id("step-1", "6100"),
            "overhead-absorbed@step-1:6100"
        );
    }
}
