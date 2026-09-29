//! `jobs.retract_matching` — a fact that ANSWERS a machine-filed item
//! withdraws every open packet that carries the fact's key.
//!
//! The gap this closes (backlog ac0a0abd, the follow-up to e22b692e):
//! `open-a-packet-when-an-event-is-dead-lettered` files one
//! backlog-item per dead letter, carrying the outbox row as
//! `metadata.outbox_id`, and `boss events redeliver` answers the dead
//! letter by recording `events.outbox.redelivered` or
//! `events.outbox.resolved` naming the same row. Nothing read the act
//! back onto the item, so the item sat open at triage asking for a
//! decision the record already held — the item and the act that
//! answered it were two disconnected records.
//!
//! ## Why this is a handler and not dead-letter code
//!
//! Every noun is a rule arg. The handler knows "find the open packets
//! of `kind` whose `metadata.<match_key>` is `value`; record the act on
//! each and withdraw it" — a shape, not a policy. Which kind, which key,
//! which act and what it says ride the rule row:
//!
//! ```toml
//! [[rule]]
//! on_event = "events.outbox.redelivered"
//! [[rule.do]]
//! handler = "jobs.retract_matching"
//! args = { kind = "\"backlog-item\"", match_key = "\"outbox_id\"", value = "outbox_id", act = "\"redelivered\"", "note.overtaken_by" = "overtaken_by", because = "\"…\"" }
//! ```
//!
//! ## What is written
//!
//! On each matching packet, `answered_by` — the act, who did it (the
//! event's own `_actor` stamp), the event's id and kind, and every
//! `note.<field>` arg — merged through the job metadata door, so the
//! act can be read off the packet without opening the log. Then the
//! packet is WITHDRAWN at the step it is waiting on, by the one
//! judgement every machine that closes its own alarm reads
//! (`common::retraction`, a2d8bad3, e61093a1): an open `triage`, or a
//! READY `build`/`measure` a person routed it to, is completed `stale`
//! — the backlog-item terminal "the claim no longer holds" — with
//! `evidence` naming the act and the rule's `because`; an ACTIVE step
//! belongs to its executor, so the packet is told instead
//! (`common::recovery_note`) and the executor closes it. Never a PATCH
//! on a finished step: the step API refuses one 409, which is the
//! defect e61093a1 cured in the conductor.
//!
//! ## Idempotence
//!
//! JetStream is at-least-once. A withdrawn packet is closed, so a
//! redelivery does not list it. A packet the act was already recorded
//! on — `answered_by.event_id` is this event — and that the machine may
//! only tell, is left alone: this is the told-once guard the estate
//! recovery keeps (`already_told`), keyed on the event rather than the
//! instant, since one packet may be answered by two acts (a failed
//! redelivery re-files the row as a NEW item; its own answer names a new
//! event). The metadata merge goes first, so a redelivery after a
//! completion that raced the terminal still finds the packet answered.

use super::common::{
    Retraction, api_client, complete_step, jobs_where, recovery_note, retraction,
    withdrawal_fields, write_json,
};
use super::jobs_complete_step_matching::scalar_text;
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use boss_dispatcher::rules::jobs_spawn::metadata_arg_json;
use serde_json::{Map, Value as Json, json};
use std::sync::Arc;

/// The handler's registered name, and the `cleared_by` stamp on every
/// step it withdraws.
const HANDLER: &str = "jobs.retract_matching";

/// The packet-metadata key the act is recorded under.
pub(crate) const ANSWERED_BY: &str = "answered_by";

/// The prefix of an arg that is recorded on the packet with the act.
const NOTE: &str = "note.";

pub struct JobsRetractMatching {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsRetractMatching {
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

/// The act one firing records: what it was, who did it, the event that
/// says so, and the rule's notes — decided once, before any write.
struct Act<'a> {
    act: &'a str,
    actor: Option<&'a str>,
    event_id: &'a str,
    event_kind: &'a str,
    rule: &'a str,
    notes: Map<String, Json>,
}

impl Act<'_> {
    /// The `answered_by` object merged onto every matching packet.
    fn record(&self) -> Json {
        let mut m = Map::new();
        m.insert("act".into(), json!(self.act));
        m.insert("actor".into(), json!(self.actor));
        m.insert("event_id".into(), json!(self.event_id));
        m.insert("event_kind".into(), json!(self.event_kind));
        m.insert("rule".into(), json!(self.rule));
        m.extend(self.notes.clone());
        Json::Object(m)
    }

    /// What the record shows, for the withdrawn step's `evidence`.
    fn evidence(&self, match_key: &str, value: &str, because: &str) -> String {
        format!(
            "The `{kind}` fact (event {event}), recorded by {actor}, names this packet's \
             `{match_key}` {value}: the act `{act}` answered what it was filed for, and is \
             recorded on the packet as `{ANSWERED_BY}`. {because}",
            kind = self.event_kind,
            event = self.event_id,
            actor = self.actor.unwrap_or("an unrecorded actor"),
            act = self.act,
        )
    }
}

/// What one matching packet gets.
#[derive(Debug, PartialEq)]
enum Plan {
    /// Record the act, then complete this step `stale`.
    Withdraw { step_id: String },
    /// Record the act and a RECOVERED note; complete nothing.
    Tell { why_open: String },
    /// Nothing: no triage step to judge, or already told of this event.
    Leave(&'static str),
}

/// PURE: the packet's `metadata.<key>` compares equal to `value` as text.
fn carries(job: &Json, key: &str, value: &str) -> bool {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(scalar_text)
        .is_some_and(|v| v == value)
}

/// PURE: where this packet withdraws, by the one retraction judgement
/// (a2d8bad3), and whether it was already told of `event_id`.
fn plan(job: &Json, event_id: &str) -> Plan {
    match retraction(job) {
        None => Plan::Leave("no triage step"),
        Some(Retraction::Complete { step_id, .. }) => Plan::Withdraw { step_id },
        Some(Retraction::Annotate { .. })
            if job
                .pointer(&format!("/metadata/{ANSWERED_BY}/event_id"))
                .and_then(Json::as_str)
                == Some(event_id) =>
        {
            Plan::Leave("already told of this act")
        }
        Some(Retraction::Annotate { why_open }) => Plan::Tell { why_open },
    }
}

/// The `value` arg as the text a packet's key is compared against. A
/// rule that names none, or names a list, is an authoring error.
fn value_text(args: &[(String, Value)]) -> Result<String, HandlerError> {
    let v = arg(args, "value").ok_or_else(|| HandlerError::MissingArg("value".into()))?;
    metadata_arg_json(v)
        .as_ref()
        .and_then(scalar_text)
        .ok_or_else(|| {
            HandlerError::Permanent(format!(
                "{HANDLER}: `value` must resolve to a scalar to match a packet by, got {}",
                v.kind()
            ))
        })
}

/// Every `note.<field>` arg, as the JSON it lands as.
fn notes(args: &[(String, Value)]) -> Map<String, Json> {
    args.iter()
        .filter_map(|(k, v)| Some((k.strip_prefix(NOTE)?.to_string(), metadata_arg_json(v)?)))
        .collect()
}

#[async_trait]
impl Handler for JobsRetractMatching {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let kind = arg_string(args, "kind")?;
        let match_key = arg_string(args, "match_key")?;
        let value = value_text(args)?;
        let act_name = arg_string(args, "act")?;
        let because = arg_string(args, "because")?;
        let rule = ctx.rule_name.as_str();
        let act = Act {
            act: act_name,
            actor: ctx.event_payload.get("_actor").and_then(Json::as_str),
            event_id: &ctx.triggering_event_id,
            event_kind: &ctx.triggering_topic,
            rule,
            notes: notes(args),
        };
        // The act's own instant (eabc5943): the fact's timestamp, and a
        // refusal before any write when it has none — never this
        // dispatcher's clock, which would close the item at the moment
        // it was READ and stamp a replay with a different time.
        let at = ctx.firing_instant()?.to_rfc3339();
        let evidence = act.evidence(match_key, &value, because);

        // Only packets that CARRY the key: an unfiltered read of an
        // open backlog is truncated past a thousand rows (c5ac71de),
        // and the value is compared here, since the list filters on
        // the key's presence alone.
        let open = jobs_where(
            &self.client,
            self.base(),
            &format!("kind={kind}&status=open&metadata_has={match_key}"),
            rule,
        )
        .await?;
        let matching: Vec<&Json> = open
            .iter()
            .filter(|j| carries(j, match_key, &value))
            .collect();
        if matching.is_empty() {
            tracing::info!(
                rule,
                "{HANDLER}: no open {kind} carries {match_key} = {value} — nothing to withdraw"
            );
            return Ok(());
        }
        for job in matching {
            let Some(job_id) = job.get("id").and_then(Json::as_str) else {
                continue;
            };
            let job_url = format!("{}/api/jobs/{job_id}/metadata", self.base());
            match plan(job, act.event_id) {
                Plan::Leave(why) => {
                    tracing::info!(rule, packet = %job_id, "{HANDLER}: left alone — {why}");
                }
                Plan::Withdraw { step_id } => {
                    // The act first, so a redelivery that finds the
                    // packet still open after a completion reads it
                    // answered and does not tell it again.
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &job_url,
                        &json!({ ANSWERED_BY: act.record() }),
                        rule,
                    )
                    .await?;
                    complete_step(
                        &self.client,
                        self.base(),
                        job_id,
                        &step_id,
                        withdrawal_fields(&evidence, HANDLER, &at),
                        rule,
                    )
                    .await?;
                    tracing::info!(rule, packet = %job_id, "{HANDLER}: `{act_name}` recorded, withdrawn stale");
                }
                Plan::Tell { why_open } => {
                    let mut body = recovery_note(&evidence, HANDLER, &at, &why_open);
                    body.insert(ANSWERED_BY.into(), act.record());
                    write_json(
                        &self.client,
                        reqwest::Method::PATCH,
                        &job_url,
                        &Json::Object(body),
                        rule,
                    )
                    .await?;
                    tracing::info!(rule, packet = %job_id, "{HANDLER}: `{act_name}` recorded, told — {why_open}");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::listing_stub::{
        assert_refused_by_name, backlog_listing, no_data_array, serve,
    };

    const ANSWERED: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_ROW: &str = "22222222-2222-2222-2222-222222222222";
    const EVENT: &str = "99999999-0000-0000-0000-000000000009";
    const LISTING: &str = "/api/jobs?kind=backlog-item&status=open&metadata_has=outbox_id";

    fn ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "close-the-dead-letter-item-when-the-row-is-redelivered".into(),
            triggering_event_id: EVENT.into(),
            triggering_topic: "events.outbox.redelivered".into(),
            event_payload: json!({
                "outbox_id": 4242,
                "event_id": "33333333-0000-0000-0000-000000000003",
                "event_kind": "jobs.step.updated",
                "event_source": "jobs",
                "payload_bytes": 1310851,
                "dead_lettered_at": "2026-09-28T04:30:00+00:00",
                "dead_letter_reason": "max payload size exceeded",
                "overtaken_by": 17,
                "_actor": "emp-david",
            }),
            event_timestamp: Some(ACTED_AT.parse().unwrap()),
        }
    }

    /// The instant the redeliver act was recorded — the fact's own
    /// envelope `timestamp`, some minutes before any dispatcher reads it.
    const ACTED_AT: &str = "2026-09-28T04:31:07+00:00";

    fn args() -> Vec<(String, Value)> {
        vec![
            ("kind".into(), Value::String("backlog-item".into())),
            ("match_key".into(), Value::String("outbox_id".into())),
            ("value".into(), Value::Int(4242)),
            ("act".into(), Value::String("redelivered".into())),
            ("note.overtaken_by".into(), Value::Int(17)),
            (
                "because".into(),
                Value::String("The row is pending on the relay's queue again.".into()),
            ),
        ]
    }

    fn step(slug: &str, status: &str, metadata: Json) -> Json {
        json!({ "id": format!("s-{slug}"), "spec_slug": slug, "status": status, "metadata": metadata })
    }

    /// A dead-letter item as the filing rule leaves it, with `steps`.
    fn item(id: &str, outbox_id: Json, steps: Vec<Json>) -> Json {
        json!({
            "id": id,
            "status": "open",
            "metadata": { "outbox_id": outbox_id, "area": "events" },
            "steps": steps,
        })
    }

    fn at_triage(id: &str, outbox_id: Json) -> Json {
        item(
            id,
            outbox_id,
            vec![
                step("filed", "completed", json!({})),
                step(
                    "triage",
                    "ready",
                    json!({ "authority_role": "platform-admin" }),
                ),
                step("build", "pending", json!({})),
            ],
        )
    }

    fn routed_to_build(id: &str, build_status: &str) -> Json {
        item(
            id,
            json!(4242),
            vec![
                step("filed", "completed", json!({})),
                step("triage", "completed", json!({ "disposition": "build" })),
                step("build", build_status, json!({})),
            ],
        )
    }

    /// THE OBLIGATION. The item filed for the redelivered row is told
    /// what answered it and withdrawn at `triage` as `stale`, through
    /// the step merge door then the flip; the item filed for another row
    /// is not touched.
    #[tokio::test]
    async fn the_item_for_the_answered_row_records_the_act_and_is_withdrawn_at_triage() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[
                    at_triage(ANSWERED, json!(4242)),
                    at_triage(OTHER_ROW, json!(4243)),
                ],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx())
            .await
            .expect("the act is read back");

        let sent = stub.sent();
        let paths: Vec<&str> = sent.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                format!("PATCH /api/jobs/{ANSWERED}/metadata"),
                format!("PATCH /api/jobs/{ANSWERED}/steps/s-triage/metadata"),
                format!("PUT /api/jobs/{ANSWERED}/steps/s-triage"),
            ],
            "the act on the packet, then the withdrawal — and nothing on the other row's item"
        );
        let answered = &sent[0].1[ANSWERED_BY];
        assert_eq!(answered["act"], "redelivered");
        assert_eq!(answered["actor"], "emp-david", "the event's own _actor");
        assert_eq!(answered["event_id"], EVENT);
        assert_eq!(answered["event_kind"], "events.outbox.redelivered");
        assert_eq!(answered["overtaken_by"], 17, "every note.* arg rides along");
        assert_eq!(
            answered["rule"],
            "close-the-dead-letter-item-when-the-row-is-redelivered"
        );

        let fields = &sent[1].1;
        assert_eq!(fields["disposition"], "stale");
        assert_eq!(fields["cleared_by"], HANDLER);
        let evidence = fields["evidence"].as_str().expect("evidence is prose");
        for said in [
            "events.outbox.redelivered",
            EVENT,
            "emp-david",
            "outbox_id",
            "4242",
            "pending on the relay's queue again",
        ] {
            assert!(
                evidence.contains(said),
                "evidence names {said:?}: {evidence}"
            );
        }
        assert!(
            fields.get("authority_role").is_none(),
            "the step's own keys are not re-sent (e39a9d2a)"
        );
        assert_eq!(
            sent[2].1,
            json!({ "status": "completed" }),
            "the flip alone"
        );
    }

    /// THE PROVENANCE (backlog eabc5943). The withdrawal and the told
    /// note are stamped with the act's own instant — the event's
    /// timestamp — never the dispatcher's clock at consumption, so a
    /// replay stamps the same time.
    #[tokio::test]
    async fn the_item_is_closed_at_the_acts_instant_not_the_dispatchers_clock() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(&[at_triage(ANSWERED, json!(4242))], Some("outbox_id"), None),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        assert_eq!(stub.sent()[1].1["recovered_at"], ACTED_AT, "withdrawn");

        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[routed_to_build(ANSWERED, "active")],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        assert_eq!(stub.sent()[0].1["recovered_at"], ACTED_AT, "told");
    }

    /// A fact with no instant is refused — permanently, naming what is
    /// missing — and nothing is written: the item stays open for a
    /// person rather than closed at a time the record never held.
    #[tokio::test]
    async fn a_fact_with_no_instant_is_refused_and_writes_nothing() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(&[at_triage(ANSWERED, json!(4242))], Some("outbox_id"), None),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        let without = InvocationContext {
            event_timestamp: None,
            ..ctx()
        };
        let err = h.invoke(&args(), &without).await.expect_err("no instant");
        assert!(err.is_permanent(), "{err}");
        assert!(err.to_string().contains("`timestamp`"), "{err}");
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// A packet whose key is stored as a string still matches an
    /// integer value: both compare as text.
    #[tokio::test]
    async fn the_value_compares_as_text() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[at_triage(ANSWERED, json!("4242"))],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        assert_eq!(stub.sent().len(), 3, "{:?}", stub.writes());
    }

    /// Routed to `build` and the build is READY: withdrawn there, the
    /// terminal triage untouched.
    #[tokio::test]
    async fn a_ready_build_is_withdrawn_where_triage_routed_it() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[routed_to_build(ANSWERED, "ready")],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        let writes = stub.writes();
        assert_eq!(
            writes,
            vec![
                format!("PATCH /api/jobs/{ANSWERED}/metadata"),
                format!("PATCH /api/jobs/{ANSWERED}/steps/s-build/metadata"),
                format!("PUT /api/jobs/{ANSWERED}/steps/s-build"),
            ],
            "never a write to the finished triage"
        );
        assert_eq!(stub.sent()[1].1["disposition"], "stale");
    }

    /// An ACTIVE build has an executor: the packet is told — the act
    /// and a RECOVERED note — and no step is written. Told once: the
    /// same event finding the act already recorded writes nothing.
    #[tokio::test]
    async fn an_active_build_is_told_once_and_never_completed_from_under_its_executor() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[routed_to_build(ANSWERED, "active")],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        let sent = stub.sent();
        assert_eq!(
            sent.len(),
            1,
            "one merge, no step write: {:?}",
            stub.writes()
        );
        assert_eq!(sent[0].0, format!("PATCH /api/jobs/{ANSWERED}/metadata"));
        assert_eq!(sent[0].1[ANSWERED_BY]["act"], "redelivered");
        let note = sent[0].1["recovery"].as_str().expect("a recovery note");
        assert!(
            note.contains("RECOVERED") && note.contains("active"),
            "{note}"
        );
        assert_eq!(sent[0].1["recovered_by"], HANDLER);

        let mut told = routed_to_build(ANSWERED, "active");
        told["metadata"][ANSWERED_BY] = json!({ "act": "redelivered", "event_id": EVENT });
        let stub = serve(vec![(
            LISTING,
            backlog_listing(&[told], Some("outbox_id"), None),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        assert!(stub.writes().is_empty(), "told once: {:?}", stub.writes());
    }

    /// No open item names the row — the item was already closed by a
    /// person, or this is a redelivery of an act already applied: an
    /// answer, not an error, and nothing is written.
    #[tokio::test]
    async fn no_matching_item_writes_nothing() {
        let stub = serve(vec![(
            LISTING,
            backlog_listing(
                &[at_triage(OTHER_ROW, json!(4243))],
                Some("outbox_id"),
                None,
            ),
        )])
        .await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        h.invoke(&args(), &ctx()).await.expect("runs");
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// A listing with no `data` array is no answer (37fc5837): refused,
    /// naming the read, and nothing written.
    #[tokio::test]
    async fn a_listing_with_no_rows_array_is_refused_by_name() {
        let stub = serve(vec![(LISTING, no_data_array())]).await;
        let h = JobsRetractMatching::with_client(crate::handlers::common::api_client(), &stub.base);
        assert_refused_by_name(h.invoke(&args(), &ctx()).await, "GET /api/jobs");
        assert!(stub.writes().is_empty());
    }

    /// A rule that names no value, or a value that is not a scalar, is
    /// an authoring error no redelivery fixes.
    #[tokio::test]
    async fn a_missing_or_non_scalar_value_is_permanent() {
        let h = JobsRetractMatching::with_client(
            crate::handlers::common::api_client(),
            "http://unused",
        );
        let without: Vec<_> = args().into_iter().filter(|(k, _)| k != "value").collect();
        let err = h.invoke(&without, &ctx()).await.expect_err("no value");
        assert!(err.is_permanent(), "{err}");
        let mut listed = args();
        listed[2].1 = Value::List(vec![Value::Int(1)]);
        let err = h.invoke(&listed, &ctx()).await.expect_err("a list");
        assert!(err.is_permanent(), "{err}");
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = JobsRetractMatching::with_client(
            crate::handlers::common::api_client(),
            "http://unused",
        );
        assert_eq!(h.name(), "jobs.retract_matching");
        assert_eq!(
            crate::cascade::handler_emits()
                .get("jobs.retract_matching")
                .cloned(),
            Some(vec!["jobs.job.updated", "jobs.step.completed"]),
            "the cascade table knows what this handler emits"
        );
    }
}
