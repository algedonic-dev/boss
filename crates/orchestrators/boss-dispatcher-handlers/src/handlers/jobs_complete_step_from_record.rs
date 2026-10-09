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
//! ## The record the landing also posts (backlog bb32b2a0)
//!
//! Landing the step was half the run's ending. `boss dispatch --report`
//! writes the run's `agent_runs` cost row only once the run has an
//! outcome — the row is insert-once, so an outcome guessed before the
//! terminal would be permanent — and a report sent before the green has
//! none. With this rule landing that report, no second report ever
//! came, so every such run ended with NO cost row and the budget gate
//! (hourly spend, cost per run) under-counted it. So `--report` now
//! stores the finish record on the packet (`finish_record`, everything
//! but what the terminal decides), and a rule naming these args posts
//! it as the step lands:
//!
//! - `post_to` — the API path the record is POSTed to.
//! - `post_record` — the packet metadata key holding the record (an
//!   object). Absent: nothing is posted, and the step's evidence says so.
//! - `post_id_key` — the body key that must name this packet (`run_id`):
//!   the packet's own id is written there, and a record naming another
//!   packet is refused, never posted.
//! - `post_fields` — fixed keys that OVERRIDE the record: the `outcome`
//!   the step going ready implies (`reported` opens only on `gated` or
//!   `delivered`, both `success`).
//! - `post_fill` — keys read off another step of the packet, filled where
//!   the record holds none: `{key: {step, from: [pointer, …]}}`, the
//!   first non-empty string wins — the car's `branch`, which only the
//!   green's evidence on `building` knows. A spec that names no `step`
//!   reads its pointers off the packet itself (`/metadata/model`), which
//!   is how `jobs.age_out_step` builds a record for a run that stored
//!   none (b5a3a174).
//!
//! The POST goes BEFORE the completion: the row is insert-once
//! (`ON CONFLICT (run_id) DO NOTHING`), so a redelivery re-posting is
//! harmless, while a completion made first would close the step and a
//! redelivery would never post. A refusal that no redelivery repairs
//! (the door's 400, or a 422) does not hold the landing hostage: the step completes, and its
//! evidence carries the refusal as `posted`, where the next reader of
//! the step sees it.
//!
//! ## Idempotence
//!
//! JetStream is at-least-once. A completed step is not open, so a
//! redelivery reads the packet, finds the step closed, and writes
//! nothing. A builder whose own `--report` completes the step first
//! wins the same way.

use super::common::{
    api_client, complete_step, dispatcher_actor_header, get_json, sim_origin_value,
};
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

/// The record the landing posts, read off the rule's `post_*` args
/// (bb32b2a0 — see the module doc).
#[derive(Debug, Clone)]
pub(crate) struct Post<'a> {
    pub to: &'a str,
    /// The packet metadata key holding the record. `None` is a post
    /// that starts from nothing and is built from `fields` and `fill`
    /// alone — `jobs.age_out_step`'s, for a run that stored no record
    /// because it never reported (backlog b5a3a174).
    pub record: Option<&'a str>,
    /// The body key that must name THIS packet (`run_id`): the packet's
    /// own id is written there, and a stored value naming another packet
    /// is refused rather than posted (review de8a09bd N1: a record that
    /// rides packet metadata is editable by any `job:update` holder, and
    /// an insert-once row posted for another run would take that run's
    /// place — its own report would then record nothing).
    pub id_key: Option<&'a str>,
    pub fields: serde_json::Map<String, serde_json::Value>,
    pub fill: serde_json::Map<String, serde_json::Value>,
}

/// PURE: the body to post — `None` when the packet holds no record under
/// `p.record` (absent, null, or not an object), `Some(Err)` with the
/// refusal when the record names another packet. `fields` OVERRIDE the
/// record: they are what the step going ready implies (`outcome =
/// success` — `reported` opens on no other ending), and no stored value
/// outranks that. `fill` fills only what the record left unset.
pub(crate) fn post_body(
    job: &serde_json::Value,
    p: &Post<'_>,
) -> Option<Result<serde_json::Value, String>> {
    let mut body = match p.record {
        Some(record) => job
            .get("metadata")
            .and_then(|m| m.get(record))
            .and_then(|r| r.as_object())
            .cloned()?,
        None => serde_json::Map::new(),
    };
    let record = p.record.unwrap_or("the post");
    // A null is unset here, unlike on a step: the record states the
    // keys it could not know as null (`branch` before the green).
    let unset = |b: &serde_json::Map<String, serde_json::Value>, k: &str| {
        matches!(b.get(k), None | Some(serde_json::Value::Null)) || is_unset(b.get(k))
    };
    if let Some(key) = p.id_key {
        let own = job.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if !unset(&body, key) && body.get(key).and_then(|v| v.as_str()) != Some(own) {
            return Some(Err(format!(
                "`{record}.{key}` names {} and the packet is {own}; a record for another packet \
                 is never posted",
                body.get(key).cloned().unwrap_or_default()
            )));
        }
        body.insert(key.to_string(), json!(own));
    }
    for (k, v) in &p.fields {
        body.insert(k.clone(), v.clone());
    }
    for (k, spec) in &p.fill {
        if !unset(&body, k) {
            continue;
        }
        // The pointers read a step when the spec names one, else the
        // packet itself (`/metadata/model`). A step it names and the
        // packet lacks fills nothing.
        let Some(root) = (match spec.get("step").and_then(|s| s.as_str()) {
            Some(slug) => step_by_slug(job, slug),
            None => Some(job),
        }) else {
            continue;
        };
        let found = spec
            .get("from")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
            .filter_map(|ptr| ptr.as_str())
            .find_map(|ptr| {
                root.pointer(ptr)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            });
        if let Some(found) = found {
            body.insert(k.clone(), json!(found));
        }
    }
    Some(Ok(serde_json::Value::Object(body)))
}

/// POST the record: `Ok(Ok)` recorded (or already held — the door is
/// insert-once), `Ok(Err)` a refusal no redelivery repairs, `Err` one
/// worth retrying.
///
/// WHY NOT `post_json` (review de8a09bd B1). That maps only 422 to a
/// final refusal, and the agent-runs door answers its OWN refusals —
/// an empty run_id, finished before started, an actor that is not an
/// agent, no model and no default — with 400 (`agent_runs/http.rs`,
/// `AgentRunError::BadRequest`); 422 is only axum's shape rejection.
/// Retried, a 400 dead-letters after the backoff with `reported` still
/// ready, which is the state b951c00a fixed. So for this post 400 and
/// 422 are final, and everything else (409, 404, 429, 5xx, transport)
/// retries as before.
pub(crate) async fn post_record(
    client: &boss_core::machine_token::Client,
    url: &str,
    body: &serde_json::Value,
    rule_name: &str,
) -> Result<Result<(), String>, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(Ok(()));
    }
    let text = resp.text().await.unwrap_or_default();
    let said = format!("POST {url} returned {status}: {text}");
    match status.as_u16() {
        400 | 422 => Ok(Err(said)),
        _ => Err(HandlerError::Downstream(said)),
    }
}

/// The `post_*` args, or `None` when the rule names no post. One of
/// `post_to` / `post_record` without the other is rule authoring — no
/// redelivery repairs it.
fn post_args<'a>(
    args: &'a [(String, Value)],
    rule: &str,
) -> Result<Option<Post<'a>>, HandlerError> {
    let text = |k: &str| match arg(args, k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    };
    match (text("post_to"), text("post_record")) {
        (None, None) => Ok(None),
        (Some(to), Some(record)) => Ok(Some(Post {
            to,
            record: Some(record),
            id_key: text("post_id_key"),
            fields: template_arg(args, "post_fields", rule).unwrap_or_default(),
            fill: template_arg(args, "post_fill", rule).unwrap_or_default(),
        })),
        _ => Err(HandlerError::Permanent(
            "post_to and post_record name a post together, or not at all".into(),
        )),
    }
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
        let post = post_args(args, &ctx.rule_name)?;
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
        let Some(mut fields) = landing_fields(&job, step_id, &landing, provenance) else {
            return Ok(());
        };
        // THE RECORD, BEFORE THE COMPLETION (bb32b2a0): insert-once, so
        // a redelivery re-posting is harmless; the completion closes
        // the step, after which no redelivery would reach this line.
        if let Some(p) = &post {
            let refused = |e: String| {
                tracing::warn!(rule = %ctx.rule_name, packet = %job_id, "{e}");
                format!("refused: {e}")
            };
            let record = p.record.unwrap_or_default();
            let posted = match post_body(&job, p) {
                None => format!("absent: the packet holds no `{record}`"),
                Some(Err(e)) => refused(e),
                Some(Ok(body)) => {
                    let url = format!("{}{}", self.base(), p.to);
                    // A refusal no redelivery repairs must not hold the
                    // landing hostage — the step's own evidence carries it.
                    match post_record(&self.client, &url, &body, &ctx.rule_name).await? {
                        Ok(()) => format!("recorded: `{record}` posted to {}", p.to),
                        Err(e) => refused(e),
                    }
                }
            };
            if let Some(evidence) = fields.get_mut(evidence_key) {
                evidence["posted"] = json!(posted);
            }
        }
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
            ("post_to".into(), Value::String("/api/agent-runs".into())),
            ("post_record".into(), Value::String("finish_record".into())),
            ("post_id_key".into(), Value::String("run_id".into())),
            (
                "post_fields".into(),
                Value::String(r#"{"outcome": "success"}"#.into()),
            ),
            (
                "post_fill".into(),
                Value::String(
                    r#"{"branch": {"step": "building", "from": ["/metadata/gate_run/branch", "/metadata/car/branch"]}}"#
                        .into(),
                ),
            ),
        ]
    }

    /// THE PIN (CLAUDE.md §9a): [`args`] is the authored rule's own
    /// args, so every test here judges the rule that runs, not a copy.
    #[test]
    fn the_fixture_is_the_authored_rule() {
        let path = boss_testing::repo_root()
            .join("infra/dispatcher/rules/agent-run-lands-a-report-sent-before-green.toml");
        let text = std::fs::read_to_string(&path).expect("the rule is authored");
        let rule: toml::Value = toml::from_str(&text).expect("the rule parses");
        let authored = rule["rule"][0]["do"][0]["args"]
            .as_table()
            .expect("the rule has args")
            .clone();
        let fixture = args();
        assert_eq!(authored.len(), fixture.len(), "{authored:?}");
        for (k, v) in &fixture {
            let Value::String(v) = v else { unreachable!() };
            let src = authored[k]
                .as_str()
                .expect("an arg is an expression string");
            // An arg is an expression: a quoted string literal.
            let lit: String = serde_json::from_str(src).expect("a string literal");
            if let (Ok(a), Ok(b)) = (
                serde_json::from_str::<serde_json::Value>(&lit),
                serde_json::from_str::<serde_json::Value>(v),
            ) {
                assert_eq!(a, b, "{k}");
            } else {
                assert_eq!(&lit, v, "{k}");
            }
        }
    }

    /// A run whose gate just went green: `building` closed `gated`,
    /// `reported` is ready, and the builder's pre-green `--report` is
    /// on the packet — with the finish record it stored beside it.
    fn run(report: Option<&str>) -> serde_json::Value {
        let mut metadata = json!({ "packet": "p", "step": "build", "tokens": 761000,
                                   "spend_usd": 4.2 });
        if let Some(r) = report {
            metadata["report"] = json!(r);
            metadata["finish_record"] = json!({ "run_id": RUN, "actor_id": "agent-claude",
                                                "branch": null, "total_tokens": 761000 });
        }
        json!({
            "id": RUN, "kind": "agent-run", "status": "open", "metadata": metadata,
            "steps": [
                { "id": "run-building", "spec_slug": "building", "status": "completed",
                  "metadata": { "result": "gated",
                                "gate_run": { "id": "g", "branch": "fix/the-car" } } },
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
    /// it closed — and the agent-runs door, answering `post_status`.
    async fn mock(job: serde_json::Value, post_status: u16) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let stored = Arc::new(Mutex::new(job));
        let (g, u) = (stored.clone(), stored);
        let (pw, uw, rw) = (writes.clone(), writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/agent-runs",
                axum::routing::post(move |Json(b): Json<serde_json::Value>| {
                    let rw = rw.clone();
                    async move {
                        rw.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/agent-runs".into(), b));
                        (
                            axum::http::StatusCode::from_u16(post_status).unwrap(),
                            Json(json!({ "recorded": true })),
                        )
                    }
                }),
            )
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

    /// The firing end to end: the finish record posted FIRST, then one
    /// merge and one flip, and a redelivery of the same event writes
    /// nothing more (bb32b2a0).
    #[tokio::test]
    async fn the_green_lands_the_report_and_its_cost_row_once() {
        let (base, writes) = mock(run(Some("packet x, branch y")), 200).await;
        let h =
            JobsCompleteStepFromRecord::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the firing runs");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 3, "the post, the merge, then the flip: {w:?}");
        assert_eq!(
            w[0].1, "/api/agent-runs",
            "the record goes before the completion"
        );
        assert_eq!(w[0].2["outcome"], "success");
        assert_eq!(w[0].2["branch"], "fix/the-car");
        assert_eq!(w[0].2["run_id"], RUN);
        assert_eq!(w[1].1, "run-reported/metadata");
        assert_eq!(w[1].2["summary"], "packet x, branch y");
        assert_eq!(w[1].2["from_record"]["event"], "ev-1");
        assert!(
            w[1].2["from_record"]["posted"]
                .as_str()
                .is_some_and(|p| p.starts_with("recorded")),
            "{}",
            w[1].2
        );
        assert_eq!(w[2].2, json!({ "status": "completed" }));

        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the redelivery runs");
        assert_eq!(writes.lock().unwrap().len(), 3, "nothing is written twice");
    }

    /// A refusal no redelivery repairs does not hold the landing
    /// hostage: the step completes and its evidence carries the refusal.
    /// 400 is what the agent-runs door answers for its OWN refusals
    /// (`AgentRunError::BadRequest`, review de8a09bd B1) — the car's
    /// first version mocked only a 422 the door never sends for them.
    #[tokio::test]
    async fn a_refused_record_still_lands_the_report_and_says_so() {
        for status in [400, 422] {
            let (base, writes) = mock(run(Some("r")), status).await;
            let h = JobsCompleteStepFromRecord::with_client(
                crate::handlers::common::api_client(),
                &base,
            );
            h.invoke(&args(), &ctx(ready("run-reported")))
                .await
                .unwrap_or_else(|e| panic!("a {status} lands the step: {e}"));
            let w = writes.lock().unwrap().clone();
            assert_eq!(w.len(), 3, "{status}: {w:?}");
            assert!(
                w[1].2["from_record"]["posted"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("refused") && p.contains(&status.to_string())),
                "{status}: {}",
                w[1].2
            );
            assert_eq!(w[2].2, json!({ "status": "completed" }));
        }
    }

    /// A record naming ANOTHER run is refused, never posted: an
    /// insert-once row for run B written by run A's landing would take
    /// B's place (review de8a09bd N1). The step still lands.
    #[tokio::test]
    async fn a_record_naming_another_run_is_refused_and_never_posted() {
        let mut r = run(Some("r"));
        r["metadata"]["finish_record"]["run_id"] = json!("55555555-5555-5555-5555-555555555555");
        let (base, writes) = mock(r, 200).await;
        let h =
            JobsCompleteStepFromRecord::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the firing runs");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "no post: {w:?}");
        assert!(
            w[0].2["from_record"]["posted"]
                .as_str()
                .is_some_and(|p| p.starts_with("refused") && p.contains("55555555")),
            "{}",
            w[0].2
        );
    }

    /// A transient failure is retried: nothing is completed, so the
    /// redelivery posts again (insert-once makes that harmless).
    #[tokio::test]
    async fn a_transient_post_failure_retries_before_anything_completes() {
        let (base, writes) = mock(run(Some("r")), 503).await;
        let h =
            JobsCompleteStepFromRecord::with_client(crate::handlers::common::api_client(), &base);
        let err = h
            .invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect_err("a 503 is retried");
        assert!(matches!(err, HandlerError::Downstream(_)), "{err}");
        assert_eq!(writes.lock().unwrap().len(), 1, "only the post was tried");
    }

    /// The body: the implied outcome OVERRIDES any stored one (review
    /// de8a09bd N1 — `reported` opening means success, and nothing on
    /// the packet outranks that), the run id is the packet's own, and
    /// the green's branch fills only what the record left unset.
    #[test]
    fn the_posted_body_takes_the_implied_outcome_and_fills_the_branch() {
        let a = args();
        let p = post_args(&a, "t").unwrap().expect("the rule posts");
        let body = post_body(&run(Some("r")), &p)
            .expect("the packet holds a record")
            .expect("the record names this run");
        assert_eq!(body["outcome"], "success");
        assert_eq!(body["branch"], "fix/the-car");
        assert_eq!(body["run_id"], RUN);
        assert_eq!(body["total_tokens"], 761000);

        let mut held = run(Some("r"));
        held["metadata"]["finish_record"]["branch"] = json!("tenant/branch");
        held["metadata"]["finish_record"]["outcome"] = json!("cancelled");
        held["metadata"]["finish_record"]
            .as_object_mut()
            .unwrap()
            .remove("run_id");
        let body = post_body(&held, &p).unwrap().unwrap();
        assert_eq!(body["branch"], "tenant/branch", "a stored branch is kept");
        assert_eq!(body["outcome"], "success", "the implied outcome wins");
        assert_eq!(body["run_id"], RUN, "an unset run id is the packet's");

        let mut merged = run(Some("r"));
        merged["steps"][0]["metadata"] =
            json!({ "result": "gated", "car": { "branch": "fix/merged" } });
        assert_eq!(
            post_body(&merged, &p).unwrap().unwrap()["branch"],
            "fix/merged"
        );

        let mut none = run(Some("r"));
        none["metadata"]["finish_record"] = serde_json::Value::Null;
        assert!(post_body(&none, &p).is_none(), "no record, nothing to post");
    }

    /// A packet from before `finish_record` existed still lands, and the
    /// step says nothing was posted.
    #[tokio::test]
    async fn a_packet_holding_no_finish_record_lands_and_says_so() {
        let mut r = run(Some("r"));
        r["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("finish_record");
        let (base, writes) = mock(r, 200).await;
        let h =
            JobsCompleteStepFromRecord::with_client(crate::handlers::common::api_client(), &base);
        h.invoke(&args(), &ctx(ready("run-reported")))
            .await
            .expect("the firing runs");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "no post: {w:?}");
        assert!(
            w[0].2["from_record"]["posted"]
                .as_str()
                .is_some_and(|p| p.starts_with("absent")),
            "{}",
            w[0].2
        );
    }

    /// Half a post is rule authoring, and permanent.
    #[test]
    fn half_a_post_is_refused() {
        let a: Vec<_> = args()
            .into_iter()
            .filter(|(k, _)| k != "post_record")
            .collect();
        assert!(matches!(
            post_args(&a, "t"),
            Err(HandlerError::Permanent(_))
        ));
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
            Some(vec!["jobs.step.completed", "agents.run.recorded"]),
            "the cascade table knows what this handler emits"
        );
    }
}
