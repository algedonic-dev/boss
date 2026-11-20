//! NATS runner — subscribes to all topics referenced by the rule
//! registry, feeds incoming events through the matcher, and dispatches
//! matched rules to the handler registry.
//!
//! It once ran beside a step-assignment consumer on `jobs.step.>`; that
//! matchmaker was retired (backlog e1eaf72b), and a ready step now
//! stands at its station until an executor pulls it.

use super::dead_letter::{
    DeadLetterClass, DeadLetterNote, DeadLetterSink, HandlerFailure, annotation_target,
};
use super::expr::HelperResolver;
use super::firings::{Firing, FiringSink, Outcome, dead_letter_id, fired_rules, firing_id};
use super::handler::{self, HandlerRegistry};
use super::registry::{self, Registry};
use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Max events processed in parallel. A strictly-serial loop awaits each
/// event's handlers (1-2 HTTP round-trips) before pulling the next, which
/// caps the runner at ~tens of events/sec; at warp that trails job
/// generation and lets ready markers + steps pile up over a long regen.
/// This is the side-effect path (invoice / COGS / shipping / ledger), so it
/// must keep pace with the 16-wide workforce's completions. Raised 6 → 12
/// now that the cluster runs max_connections=400 (was 100): the fan-out
/// across module services has pool headroom, and a transient "too many
/// clients" blip is NAK'd + redelivered by the JetStream layer rather than
/// lost. Saturate the DB, don't error it — dead-letters reappearing is the
/// signal this went too high.
const MAX_CONCURRENT_EVENTS: usize = 12;

/// PURE: the envelope's own `timestamp` — the instant the fact was
/// recorded, which a handler stamps instead of the dispatcher's clock
/// (backlog eabc5943). `None` for an envelope without a readable one;
/// a handler that needs an instant then refuses by name.
fn envelope_timestamp(envelope: &Value) -> Option<DateTime<Utc>> {
    envelope
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// What the rules runner needs to operate.
#[derive(Clone)]
pub struct RulesRunner {
    pub delivery_routes: Option<Arc<dyn super::delivery::DeliveryRoutes>>,
    pub registry: Registry,
    pub handlers: HandlerRegistry,
    pub helpers: Arc<dyn HelperResolver + Send + Sync>,
    /// Where a dead-letter is recorded so it outlives this pod (see
    /// [`super::dead_letter`]). `None` runs the loop with the log as the
    /// only record — the pre-`a9c498eb` behaviour, kept for the tests
    /// that are about matching and settling rather than about visibility.
    pub dead_letters: Option<Arc<dyn DeadLetterSink>>,
    /// The liveness surface a dead-letter is COUNTED on regardless of
    /// whether it could be recorded (8834804a). A local read that owes
    /// nothing to the jobs API — the arm that still speaks when the
    /// annotation's own write is what failed. `None` in tests about
    /// matching and settling.
    pub live: Option<std::sync::Arc<crate::liveness::DispatcherLiveness>>,
    /// Where a rule's firing is recorded so the hop it moves can prove
    /// it ran (`super::firings`, backlog b14afc48). `None` runs the
    /// loop with the log as the only trace — the pre-b14afc48
    /// behaviour, kept for the tests that are about matching and
    /// settling.
    pub firings: Option<Arc<dyn FiringSink>>,
}

impl RulesRunner {
    /// Compute the set of NATS topics to subscribe to. Walks the
    /// rule registry, deduplicating identical `on_event` patterns.
    /// Schedule-triggered rules contribute nothing — they fire off the
    /// clock stream, not a NATS subscription.
    pub fn subscriptions(&self) -> HashSet<String> {
        self.registry
            .rules()
            .iter()
            .filter_map(|r| r.event_pattern().map(|p| p.raw().to_string()))
            .collect()
    }

    /// Bind a durable JetStream consumer covering every topic the registry
    /// references and run the match-then-dispatch loop. Runs for the life
    /// of its task: a stream that ends is opened again
    /// (`boss_nats::durable::keep_bound`, backlog 2e45840a).
    ///
    /// Durability is the point: each event is ACK'd only after its handlers
    /// succeed. A handler failure NAKs the message, so the server redelivers
    /// it on a backoff schedule and the work self-heals once a transient
    /// condition clears — instead of the silent drop that orphaned Jobs
    /// under plain core-NATS subscribe.
    /// Consume the rules feed from `audit_log` by id cursor instead
    /// of JetStream — log-as-the-bus, stage 1 (transactional-audit-log
    /// Q6). Same `handle`, same Settle semantics; delivery is the
    /// [`crate::rules::log_tail`] contract: per-item durable advance,
    /// retry blocks the cursor on the JetStream pacing schedule,
    /// budget exhaustion dead-letters loudly and moves on. Selected
    /// by `BOSS_RULES_SOURCE=log`; the JetStream path below remains
    /// the default until the cutover is observed.
    pub async fn run_log_tail(
        &self,
        pool: sqlx::PgPool,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
    ) -> Result<()> {
        use crate::rules::log_tail::{LogTail, retry_delay};
        let mut tail = LogTail::new(pool, "dispatcher-rules-log");
        tail.ensure_cursor().await?;
        live.mark_rules_running();
        info!("rules runner: tailing audit_log as 'dispatcher-rules-log' (BOSS_RULES_SOURCE=log)");
        loop {
            let report = tail
                .drain_once(
                    500,
                    |topic, event_id, payload, attempt, recorded_at| async move {
                        self.handle(&topic, &event_id, &payload, attempt, Some(recorded_at))
                            .await
                    },
                )
                .await;
            match report {
                Ok(r) => {
                    if r.processed > 0 || r.dead_lettered > 0 {
                        live.record_rules();
                    }
                    match r.blocked {
                        Some(b) => tokio::time::sleep(retry_delay(b.attempts)).await,
                        // Idle: one poll interval behind the relay's
                        // own ~200ms lag — side effects stay
                        // sub-second behind the write.
                        None => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                    }
                }
                Err(e) => {
                    warn!(error = %e, "rules log tail: drain error; retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }

    pub async fn run(
        &self,
        js: async_nats::jetstream::Context,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
    ) -> Result<()> {
        self.run_delivery(js, live, None).await
    }

    pub async fn run_delivery(
        &self,
        js: async_nats::jetstream::Context,
        live: std::sync::Arc<crate::liveness::DispatcherLiveness>,
        lane: Option<super::delivery::Cutover>,
    ) -> Result<()> {
        // The default retains ALL rules: old backlog up to the cutover
        // still belongs to it. The authoritative fence below selects effects.
        let subjects: Vec<String> = match &lane {
            None => self.subscriptions().into_iter().collect(),
            Some(lane) => self
                .registry
                .rules()
                .iter()
                .filter(|r| r.delivery_lane.as_deref() == Some(lane.lane.as_str()))
                .filter_map(|r| r.event_pattern().map(|p| p.raw().to_string()))
                .collect(),
        };
        if subjects.is_empty() {
            info!("rules runner: no rules registered, nothing to consume");
            return Ok(());
        }
        // One consumer over the coarse (first-token) wildcards covering the
        // registry's subjects — non-overlapping, so the server accepts it and
        // each event arrives exactly once. `handle` re-matches precisely via
        // `match_event`; the few extra subjects pulled in match no rule and
        // ACK as no-ops.
        let filters = if lane.is_none() {
            boss_nats::durable::coarse_filter_subjects(&subjects)
        } else {
            subjects
        };
        // Which consumer this task keeps bound, named in every warning.
        let durable = lane
            .as_ref()
            .map_or("dispatcher-rules", |lane| lane.consumer.as_str());
        info!(?filters, durable, "rules runner: opening durable consumer");
        let created_ns = |stream: &async_nats::jetstream::stream::Stream| {
            stream
                .cached_info()
                .created
                .unix_timestamp_nanos()
                .to_string()
        };
        let runner = Arc::new(self.clone());
        // Kept bound for the life of the task (backlog 2e45840a, design
        // ad9519bc). Car 2 made the default consumer's first open wait
        // for the bus; car 3 does the same for a lane's one-shot
        // `get_consumer`, and opens EITHER again when its bound stream
        // ends — both used to return for good, leaving that consumer's
        // step-completion side effects off until a restart or a registry
        // reload. Readiness reads the consumer not running while it
        // waits: `consume` marks both edges.
        let never = boss_nats::durable::keep_bound(
            durable,
            || async {
                // Ensured when the server says it is gone, not only on a
                // reconnect (backlog 2e45840a, the stream half).
                let stream = boss_nats::durable::bound_stream(&js, boss_nats::durable::STREAM_NAME)
                    .await
                    .map_err(|e| e.to_string())?;
                let messages = match &lane {
                    // A lane's consumer is CREATED only by `bind_lanes`,
                    // under its cutover fence; here it is only read. Its
                    // stream ends like the default's when the consumer
                    // vanishes (backlog 2e45840a), so a lane whose
                    // consumer is gone reads not-running and names it in
                    // the open's warning, instead of reading running
                    // while every pull answers no responders.
                    //
                    // It is deliberately NOT recreated here (decided
                    // 2026-11-11, run b529c357). The cutover receipt pins
                    // this consumer by its creation instant, and the
                    // default consumer stops delivering the lane's rules
                    // above the floor. A recreated lane consumer starts
                    // new-only, so what was published while it was gone
                    // would be delivered by neither — a silent loss —
                    // and `bind_lanes` already refuses that same
                    // recreate on a reload ("use a new lane, never
                    // rewrite its cutover"). A vanished lane is an
                    // operator's act: retire it and bind a new lane.
                    Some(lane) => {
                        let consumer: async_nats::jetstream::consumer::Consumer<
                            async_nats::jetstream::consumer::pull::Config,
                        > = stream
                            .get_consumer(&lane.consumer)
                            .await
                            .map_err(|e| e.to_string())?;
                        boss_nats::durable::until_vanished(
                            consumer.messages().await.map_err(|e| e.to_string())?,
                        )
                    }
                    None => boss_nats::durable::open_durable(
                        &js,
                        boss_nats::durable::STREAM_NAME,
                        "dispatcher-rules",
                        filters.clone(),
                    )
                    .await
                    .map_err(|e| e.to_string())?,
                };
                Ok::<_, String>((created_ns(&stream), messages))
            },
            |(stream_created_ns, messages)| {
                runner
                    .clone()
                    .consume(&js, &live, lane.as_ref(), stream_created_ns, messages)
            },
        )
        .await;
        match never {}
    }

    /// One bound session of the match-then-dispatch loop: mark the
    /// consumer running, process its messages until the stream ends,
    /// mark it stopped. [`Self::run_delivery`] opens the next session.
    async fn consume(
        self: Arc<Self>,
        js: &async_nats::jetstream::Context,
        live: &Arc<crate::liveness::DispatcherLiveness>,
        lane: Option<&super::delivery::Cutover>,
        stream_created_ns: String,
        messages: boss_nats::durable::Messages,
    ) {
        // Consumer bound — the step-completion side-effect path is live (see
        // crate::liveness::DispatcherLiveness; a process that's health-200 but
        // whose rules consumer died runs zero side-effects and stalls Jobs).
        if let Some(lane) = lane {
            live.mark_delivery_lane(&lane.lane, true);
        } else {
            live.mark_rules_running();
        }

        // Process events with bounded concurrency (see MAX_CONCURRENT_EVENTS).
        // `handle` only reads shared state (the rule registry + stateless
        // Arc'd handlers), so distinct events process safely in parallel;
        // a single Job's step chain stays ordered because each event only
        // fires after its predecessor's completion lands.
        let runner = self;
        let processing = messages.for_each_concurrent(MAX_CONCURRENT_EVENTS, |msg| {
            let live = live.clone();
            let runner = runner.clone();
            let lane_name = lane.map(|v| v.lane.clone());
            let stream_created_ns = stream_created_ns.clone();
            async move {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "rules runner: message stream error");
                        return;
                    }
                };
                let subject = msg.subject.to_string();
                let envelope: Value = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        // Unparseable: redelivery can't help — ACK so it
                        // doesn't loop, and move on.
                        debug!(error = %e, subject = %subject, "skip non-JSON event (ACK)");
                        let _ = msg.ack().await;
                        return;
                    }
                };
                // events come as {id, timestamp, source, kind, payload}.
                let event_id = envelope
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let recorded_at = envelope_timestamp(&envelope);
                let position = msg.info().ok().map(|info| super::delivery::Position {
                    lane: lane_name,
                    stream_created_ns,
                    stream_sequence: info.stream_sequence,
                });
                let payload = envelope.get("payload").cloned().unwrap_or(envelope);
                // The delivery count `settle` will read, read here so
                // `handle` knows whether a failure now is the last
                // one the budget allows. Same fallback as `settle`:
                // an unreadable count counts as the final attempt, so
                // a broken message records its dead-letter rather
                // than vanishing into a Term.
                let attempt = msg
                    .info()
                    .map(|i| i.delivered)
                    .unwrap_or(boss_nats::durable::MAX_DELIVER)
                    .max(1) as u32;
                let mut announced_hold = false;
                let outcome = loop {
                    let handling = runner.clone().handle_owned(subject.clone(), event_id.clone(), payload.clone(), attempt, recorded_at, position.clone());
                    tokio::pin!(handling);
                    let outcome = if announced_hold {
                        // The pacing sleep already consumed one heartbeat interval.
                        // Renew before polling a recheck that can itself stay pending.
                        if let Err(error) = msg.ack_with(async_nats::jetstream::AckKind::Progress).await {
                            warn!(%error, %event_id, "held delivery progress failed before routing recheck");
                        }
                        // Keep the established hold alive even when its next
                        // authoritative read is itself pending, not only between reads.
                        loop {
                            tokio::select! {
                                value = &mut handling => break value,
                                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                                    if let Err(error) = msg.ack_with(async_nats::jetstream::AckKind::Progress).await {
                                        warn!(%error, %event_id, "held delivery progress failed while routing read is pending");
                                    }
                                }
                            }
                        }
                    } else { handling.await };
                    if let boss_nats::durable::Settle::Hold(reason) = outcome {
                        if !announced_hold {
                            warn!(%reason, %event_id, "delivery held without spending handler retry budget");
                            announced_hold = true;
                        }
                        if let Err(error) = msg.ack_with(async_nats::jetstream::AckKind::Progress).await {
                            warn!(%error, %event_id, "held delivery progress failed; no effect or acknowledgement claimed");
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    } else { break outcome; }
                };
                // ACK on success; NAK (→ redeliver) on transient failure —
                // dead-letter once the budget is spent; immediate Term on a
                // permanent failure (deterministic data error — every
                // redelivery fails identically, so the budget is noise).
                // `settle_classified` does the failure logging
                // (deliberately NOT phrased as the health-gate pattern for
                // NAKs — a transient that self-heals is not a defect; only
                // a dead-letter is, and permanent failures log as one).
                boss_nats::durable::settle_classified(&msg, outcome).await;
                live.record_rules();
            }
        });
        let sampling = async {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Some(lane) = lane {
                    live.record_delivery_lane_lag(
                        &lane.lane,
                        super::lag::sample_consumer(js, &lane.consumer).await,
                    );
                } else {
                    live.record_rules_lag(super::lag::sample(js).await);
                }
            }
        };
        // The sampler is owned by this consumer loop, not a detached
        // task surviving a registry reload or a stopped consumer.
        tokio::select! { _ = processing => {}, _ = sampling => {} }
        if let Some(lane) = lane {
            live.mark_delivery_lane(&lane.lane, false);
        } else {
            live.mark_rules_stopped();
        }
        info!("rules runner: message stream ended");
    }

    /// Match and dispatch one event.
    async fn handle_owned(
        self: Arc<Self>,
        topic: String,
        event_id: String,
        payload: Value,
        attempt: u32,
        recorded_at: Option<DateTime<Utc>>,
        position: Option<super::delivery::Position>,
    ) -> boss_nats::durable::Settle {
        // A sync helper's block_in_place suspends its whole TASK, not
        // merely one future in for_each_concurrent. Keep each event in
        // its own task under the existing 12-event cap. JoinSet aborts
        // it when the owning loop is dropped or the registry reloads.
        let runner = self.clone();
        let mut owned = tokio::task::JoinSet::new();
        owned.spawn(async move {
            runner
                .handle_delivery(
                    &topic,
                    &event_id,
                    &payload,
                    attempt,
                    recorded_at,
                    position.as_ref(),
                )
                .await
        });
        match owned.join_next().await {
            Some(Ok(outcome)) => outcome,
            _ => {
                warn!("rules runner: owned event task ended before settlement; retrying");
                if boss_nats::durable::is_dead_letter(attempt as i64)
                    && let Some(live) = &self.live
                {
                    live.record_dead_letter(false);
                }
                boss_nats::durable::Settle::Retry(
                    "rules event task failed before settlement".into(),
                )
            }
        }
    }

    /// Match and dispatch one event.
    ///
    /// `attempt` is the 1-based delivery count of THIS presentation, and
    /// it is a parameter rather than something the transport keeps to
    /// itself because the dead-letter has to be recorded here: this is
    /// the only moment the rule, the handler, the attempt count and the
    /// error are all in one place (`a9c498eb`). The transport still owns
    /// the settling; `handle` only has to know that a failure now is the
    /// last one the budget allows.
    ///
    /// `recorded_at` is the event's own timestamp, handed to every
    /// handler on its context (backlog eabc5943).
    async fn handle(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
        attempt: u32,
        recorded_at: Option<DateTime<Utc>>,
    ) -> boss_nats::durable::Settle {
        self.handle_delivery(topic, event_id, payload, attempt, recorded_at, None)
            .await
    }

    pub async fn handle_delivery(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
        attempt: u32,
        recorded_at: Option<DateTime<Utc>>,
        position: Option<&super::delivery::Position>,
    ) -> boss_nats::durable::Settle {
        use boss_nats::durable::Settle;
        let matching_started = std::time::Instant::now();
        let registry::MatchOutcome {
            mut matched,
            skipped,
        } = registry::match_event(&self.registry, topic, payload, self.helpers.as_ref());
        if let Some(live) = &self.live {
            live.record_match_cost(matching_started.elapsed());
        }
        // A synchronous read already in progress cannot be interrupted.
        // Re-enter the cancellation boundary before invoking handlers,
        // so a dropped owner cannot resume into writes after that read.
        tokio::task::yield_now().await;
        // A rule that cannot be evaluated is skipped, LOUDLY, and its
        // neighbours still fire. It used to abort matching for the whole
        // topic, so one bad arg reference silenced every rule bound to
        // that subject until the event dead-lettered.
        //
        // These never retry. A predicate that will not evaluate or an
        // arg that names a field the payload does not carry fails
        // identically on every redelivery - it is a defect in the rule
        // text, not a transient. Burning the redelivery budget on it
        // only delays the same outcome, which is what the eight NAKs
        // before the 2026-08-24 dead-letter bought us.
        for s in &skipped {
            error!(
                rule = %s.rule,
                topic = %topic,
                triggering_event = %event_id,
                class = "permanent",
                error = %s.err,
                "RULE-SKIPPED: rule could not be evaluated and did not fire; \
                 other rules on this topic were unaffected (fix the rule and republish)"
            );
        }
        if matched.is_empty() {
            return Settle::Ack;
        }
        if let Some(routes) = &self.delivery_routes {
            let names = matched
                .iter()
                .map(|r| r.rule_name.clone())
                .collect::<Vec<_>>();
            match routes.permitted(&names, position).await {
                Ok(permitted) => matched.retain(|r| permitted.contains(&r.rule_name)),
                Err(error) => return Settle::Hold(error),
            }
        } else if matched.iter().any(|r| {
            self.registry
                .rules()
                .iter()
                .any(|known| known.name == r.rule_name && known.delivery_lane.is_some())
        }) {
            return Settle::Hold("delivery lane has no authoritative route reader".into());
        }
        if matched.is_empty() {
            return Settle::Ack;
        }
        let handling_started = std::time::Instant::now();
        let dispatched = handler::dispatch(
            &matched,
            &self.handlers,
            event_id,
            topic,
            payload,
            recorded_at,
        )
        .await;
        if let Some(live) = &self.live {
            live.record_dispatch_cost(handling_started.elapsed());
        }
        let results = match dispatched {
            Ok(r) => r,
            Err(e) => {
                return Settle::Retry(format!("dispatching matched rules for {topic}: {e}"));
            }
        };
        // Collect handler failures and propagate them: a failed handler must
        // surface as an `Err` here so the caller NAKs the message and the
        // server redelivers it. The previous version logged failures but
        // returned `Ok`, which under JetStream would ACK away a dropped side
        // effect — exactly the silent-orphaning this layer exists to stop.
        //
        // NOTE: a NAK redelivers the whole event, re-running EVERY matched
        // handler — including any that already succeeded. For multi-handler
        // subjects (`step.done.production-produce`, `step.done.shipment`)
        // the handlers must therefore be idempotent on their source key, or
        // a partial failure double-applies the survivors on retry.
        let mut failures: Vec<HandlerFailure> = Vec::new();
        let mut all_permanent = true;
        // Rule names whose handlers all succeeded, in fire order, each
        // once: a rule with three handlers fired ONCE on this event, not
        // three times (`fired_rules`, shared with the schedule runner).
        let fired = fired_rules(&results);
        for r in results {
            match &r.outcome {
                Ok(()) => {
                    // Per-fire log at DEBUG, not INFO: at warp the runner
                    // fires tens of rules/sec, and an INFO line each
                    // flooded syslog (26G incident, 2026-06-23). Failures
                    // still surface via the `bail!` below.
                    debug!(
                        rule = %r.rule_name,
                        handler = %r.handler,
                        triggering_event = %event_id,
                        "rule fired"
                    );
                }
                Err(e) => {
                    if !e.is_permanent() {
                        all_permanent = false;
                    }
                    failures.push(HandlerFailure {
                        rule: r.rule_name.clone(),
                        handler: r.handler.clone(),
                        error: e.to_string(),
                    });
                }
            }
        }
        // What FIRED is recorded, after the settle is computed from it
        // and never able to change it (b14afc48). Only the rules whose
        // handlers all succeeded: a rule that failed did not move the
        // packet, and its failure has the dead-letter for a record.
        self.record_firings(topic, event_id, payload, &fired).await;
        if !failures.is_empty() {
            let msg = format!(
                "{} handler(s) failed on {topic}: {}",
                failures.len(),
                failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            // Term only when EVERY failure is deterministic: if any
            // transient failure is present, NAK — the idempotent re-run
            // lets the transients converge, and the ride-along permanent
            // failures re-fail harmlessly until the event either fully
            // converges or Terms on a later all-permanent pass.
            let settle = if all_permanent {
                boss_nats::durable::Settle::Permanent(msg)
            } else {
                boss_nats::durable::Settle::Retry(msg)
            };
            // A dead-letter lands on its packet (`a9c498eb`). The settle
            // is already decided above and this cannot change it — see
            // `record_dead_letter`.
            let class = match &settle {
                boss_nats::durable::Settle::Permanent(_) => Some(DeadLetterClass::Permanent),
                boss_nats::durable::Settle::Retry(_)
                    if boss_nats::durable::is_dead_letter(attempt as i64) =>
                {
                    Some(DeadLetterClass::BudgetExhausted)
                }
                // Budget left: the NAK will redeliver and the work may
                // still self-heal. Annotating here would mark a packet
                // troubled that is about to be fine.
                _ => None,
            };
            if let Some(class) = class {
                self.record_dead_letter(topic, event_id, payload, attempt, class, failures)
                    .await;
            }
            return settle;
        }
        boss_nats::durable::Settle::Ack
    }

    /// Record that these rules fired on this event.
    ///
    /// BEST-EFFORT, for the dead-letter's reasons: it returns `()`, its
    /// own failure is logged and dropped, and it runs after the settle
    /// is decided. A firing record that could not be written must never
    /// turn a side effect that DID land into a redelivery — the record
    /// exists to make the hop's silence readable, and an arm that can
    /// break the thing it observes is not an arm.
    async fn record_firings(&self, topic: &str, event_id: &str, payload: &Value, fired: &[String]) {
        let Some(sink) = &self.firings else { return };
        if fired.is_empty() {
            return;
        }
        // `wall_now` because this is a RECORD STAMP, not a business
        // date (boss_clock_client::wall_now): sim time is retired from
        // the record, and the map ages this instant against the same
        // real clock audit_log timestamps are on.
        let fired_at = boss_clock_client::wall_now();
        let simulated = payload
            .get("_simulated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let rows: Vec<Firing> = fired
            .iter()
            .map(|rule| Firing {
                firing_id: firing_id(rule, event_id),
                rule: rule.clone(),
                fired_on: topic.to_string(),
                fired_at,
                detail: serde_json::json!({
                    "event_id": event_id,
                    "simulated": simulated,
                }),
                outcome: Outcome::Fired,
            })
            .collect();
        let recording_started = std::time::Instant::now();
        let recorded = sink.record(&rows).await;
        if let Some(live) = &self.live {
            live.record_firing_cost(recording_started.elapsed(), rows.len());
        }
        if let Err(e) = recorded {
            warn!(
                error = %e,
                topic = %topic,
                triggering_event = %event_id,
                rules = fired.len(),
                "dispatcher firings: the firing record could not be written; \
                 the side effects still landed and the log line is the only trace"
            );
        }
    }

    /// Record a dead-letter whose topic names no packet, as one
    /// `dead-letter` row per failed rule in the firing record (backlog
    /// 4b175523). Until this, such a dead-letter left a log line and an
    /// in-process counter that resets with the pod, so the rules list's
    /// "no failures" could not be told from loss.
    ///
    /// BEST-EFFORT, for the annotation's reasons: the settle is already
    /// decided, a failed write is logged and dropped, and the answer is
    /// only whether a durable record now exists — which is what the
    /// liveness counter's `unrecorded` half asks.
    async fn record_unrouted_dead_letter(&self, note: &DeadLetterNote) -> bool {
        let Some(sink) = &self.firings else {
            warn!(
                topic = %note.topic,
                triggering_event = %note.event_id,
                attempts = note.attempts,
                class = note.class.as_str(),
                "dead-letter carries no packet to annotate and no firing record is wired: \
                 the log line above and the liveness counter are the record"
            );
            return false;
        };
        let failures: Vec<String> = note.failures.iter().map(ToString::to_string).collect();
        let mut rules: Vec<&str> = Vec::new();
        for f in &note.failures {
            if !rules.contains(&f.rule.as_str()) {
                rules.push(&f.rule);
            }
        }
        let recorded_at = boss_clock_client::wall_now();
        let rows: Vec<Firing> = rules
            .iter()
            .map(|rule| Firing {
                firing_id: dead_letter_id(rule, &note.event_id),
                rule: rule.to_string(),
                fired_on: note.topic.clone(),
                fired_at: recorded_at,
                detail: serde_json::json!({
                    "event_id": note.event_id,
                    "attempts": note.attempts,
                    "class": note.class.as_str(),
                    "failures": failures,
                    "simulated": note.simulated,
                }),
                outcome: Outcome::DeadLetter,
            })
            .collect();
        match sink.record(&rows).await {
            Ok(()) => {
                warn!(
                    topic = %note.topic,
                    triggering_event = %note.event_id,
                    attempts = note.attempts,
                    class = note.class.as_str(),
                    "dead-letter carries no packet to annotate: recorded in dispatcher_firings \
                     as outcome dead-letter"
                );
                true
            }
            Err(e) => {
                error!(
                    topic = %note.topic,
                    triggering_event = %note.event_id,
                    attempts = note.attempts,
                    error = %e,
                    "DEAD-LETTER record failed: this topic names no packet, and the \
                     dispatcher_firings write failed, so this pod's log and the liveness \
                     counter are the record"
                );
                false
            }
        }
    }

    /// Land a dead-letter on the packet whose step the handler owed.
    ///
    /// BEST-EFFORT, DELIBERATELY, in three ways — this is the arm, and an
    /// arm that needs the patient is not an arm (CLAUDE.md §Diagnosis):
    ///
    /// 1. It returns `()`. The settle is computed by the caller BEFORE
    ///    this runs and is returned regardless, so a failed annotation
    ///    can never re-enter the redelivery budget it is reporting on —
    ///    which would mean a down jobs API turned every dead-letter into
    ///    eight more deliveries of the same event.
    /// 2. Its own failure is logged and dropped. The `DEAD-LETTER:` line
    ///    the transport writes is still the floor; this only adds a
    ///    record that outlives the pod.
    /// 3. The write carries a timeout (see the sink), so a jobs API that
    ///    accepts and never answers costs the loop seconds, not the loop.
    ///
    /// With no target (a topic whose subject is not a packet) there is
    /// nothing to annotate — pointing the annotation at whatever packet
    /// shares the subject's uuid would file a false record — so the
    /// dead-letter goes to the dispatcher's own firing record instead
    /// ([`Self::record_unrouted_dead_letter`], 4b175523).
    async fn record_dead_letter(
        &self,
        topic: &str,
        event_id: &str,
        payload: &Value,
        attempt: u32,
        class: DeadLetterClass,
        failures: Vec<HandlerFailure>,
    ) {
        let Some(sink) = &self.dead_letters else {
            if let Some(live) = &self.live {
                live.record_dead_letter(false);
            }
            return;
        };
        let note = DeadLetterNote {
            topic: topic.to_string(),
            event_id: event_id.to_string(),
            attempts: attempt,
            class,
            failures,
            // The same read `handler::dispatch` makes per invocation, so
            // an annotation about simulated work is simulated state.
            simulated: payload
                .get("_simulated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        let Some(target) = annotation_target(topic, payload) else {
            let recorded = self.record_unrouted_dead_letter(&note).await;
            if let Some(live) = &self.live {
                live.record_dead_letter(recorded);
            }
            return;
        };
        let recorded = match sink.record(&target, &note).await {
            Ok(()) => {
                info!(
                    job_id = %target.job_id,
                    step_id = ?target.step_id,
                    topic = %topic,
                    attempts = attempt,
                    class = class.as_str(),
                    "dead-letter recorded on its packet"
                );
                true
            }
            // Not an error! return and not a retry: see (1) above.
            Err(e) => {
                error!(
                    job_id = %target.job_id,
                    topic = %topic,
                    attempts = attempt,
                    error = %e,
                    "DEAD-LETTER annotation failed; the packet still looks untouched and this \
                     pod's log and the liveness counter are the record"
                );
                false
            }
        };
        if let Some(live) = &self.live {
            live.record_dead_letter(recorded);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::expr::NoHelpers;
    use super::*;
    use crate::rules::dead_letter::Target;

    /// The first delivery — a failure here still has budget left.
    const FIRST: u32 = 1;
    /// The last delivery the budget allows: a failure here dead-letters.
    const FINAL: u32 = boss_nats::durable::MAX_DELIVER as u32;

    /// A sink that records what it was asked to land, and can be made to
    /// fail the way a down jobs API would.
    struct RecordingDeadLetters {
        landed: tokio::sync::Mutex<Vec<(Target, DeadLetterNote)>>,
        fails: bool,
    }

    impl RecordingDeadLetters {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                landed: tokio::sync::Mutex::new(Vec::new()),
                fails: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                landed: tokio::sync::Mutex::new(Vec::new()),
                fails: true,
            })
        }
        async fn landed(&self) -> Vec<(Target, DeadLetterNote)> {
            self.landed.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl DeadLetterSink for RecordingDeadLetters {
        async fn record(&self, target: &Target, note: &DeadLetterNote) -> Result<(), String> {
            self.landed
                .lock()
                .await
                .push((target.clone(), note.clone()));
            if self.fails {
                return Err("PATCH /api/jobs/j-1/metadata: connection refused".into());
            }
            Ok(())
        }
    }

    use crate::rules::firings::testing::{FailingFirings, RecordingFirings};

    #[test]
    fn subscriptions_dedupes_repeated_topics() {
        let toml = r#"
[[rule]]
name = "r1"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h1"
[[rule]]
name = "r2"
on_event = "step.done.procurement"
[[rule.do]]
handler = "h2"
[[rule]]
name = "r3"
on_event = "inventory.parts.consumed"
[[rule.do]]
handler = "h3"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let runner = RulesRunner {
            delivery_routes: None,
            registry: reg,
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        let subs = runner.subscriptions();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains("step.done.procurement"));
        assert!(subs.contains("inventory.parts.consumed"));
    }

    #[test]
    fn empty_registry_has_no_subscriptions() {
        let runner = RulesRunner {
            delivery_routes: None,
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        assert!(runner.subscriptions().is_empty());
    }

    #[tokio::test]
    async fn handle_propagates_handler_failure() {
        // The load-bearing durable contract: a failed handler must surface
        // as `Err` from `handle` so the caller NAKs the message and the
        // server redelivers it. The pre-JetStream version logged the failure
        // and returned `Ok`, which under a durable consumer would ACK away a
        // dropped side effect — the exact silent-orphaning this layer ends.
        let toml = r#"
[[rule]]
name = "r-fail"
on_event = "step.done.billing"
[[rule.do]]
handler = "boom"
"#;
        let reg = Registry::from_toml(toml).unwrap();
        let mut handlers = HandlerRegistry::new();
        handlers.register(handler::FailingHandler::new("boom", "policy unreachable"));
        let runner = RulesRunner {
            delivery_routes: None,
            registry: reg,
            handlers,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        let payload = serde_json::json!({
            "job_id": "j1", "step_id": "s1", "kind": "billing"
        });
        let res = runner
            .handle("step.done.billing", "evt-1", &payload, FIRST, None)
            .await;
        // A transient failure must surface as Retry so the message NAKs.
        match res {
            boss_nats::durable::Settle::Retry(msg) => assert!(
                msg.contains("boom"),
                "the propagated error should name the failed handler: {msg}"
            ),
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn handle_is_ok_when_no_rule_matches() {
        // No matched rule = nothing to do = ACK (not a failure). Keeps the
        // consumer from NAK-looping the many events no rule cares about.
        let runner = RulesRunner {
            delivery_routes: None,
            registry: Registry::empty(),
            handlers: HandlerRegistry::new(),
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        let res = runner
            .handle(
                "step.done.unmatched",
                "evt",
                &serde_json::json!({}),
                FIRST,
                None,
            )
            .await;
        assert!(
            matches!(res, boss_nats::durable::Settle::Ack),
            "an unmatched event must ACK, not retry"
        );
    }

    /// The envelope's `timestamp` is read as the instant the fact was
    /// recorded; an envelope without one reads as none, never as now.
    #[test]
    fn the_envelopes_timestamp_is_the_recorded_instant() {
        let envelope = serde_json::json!({
            "id": "evt-act",
            "timestamp": "2026-09-28T04:31:07.25Z",
            "kind": "events.outbox.redelivered",
            "payload": {"outbox_id": 4242},
        });
        assert_eq!(
            envelope_timestamp(&envelope).map(|t| t.to_rfc3339()),
            Some("2026-09-28T04:31:07.250+00:00".to_string())
        );
        assert_eq!(envelope_timestamp(&serde_json::json!({"id": "x"})), None);
        assert_eq!(
            envelope_timestamp(&serde_json::json!({"timestamp": "yesterday"})),
            None
        );
    }

    /// Every handler a fact fires is handed the fact's own instant on
    /// its context (backlog eabc5943) — what it stamps is the record's
    /// time, not the moment the dispatcher read it.
    #[tokio::test]
    async fn handle_hands_the_events_timestamp_to_the_handler() {
        let toml = r#"
[[rule]]
name = "r"
on_event = "events.outbox.redelivered"
[[rule.do]]
handler = "rec"
"#;
        let recorder = handler::RecordingHandler::new("rec");
        let mut handlers = HandlerRegistry::new();
        handlers.register(recorder.clone());
        let runner = RulesRunner {
            delivery_routes: None,
            registry: Registry::from_toml(toml).unwrap(),
            handlers,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        let at = DateTime::parse_from_rfc3339("2026-09-28T04:31:07Z")
            .unwrap()
            .with_timezone(&Utc);
        let res = runner
            .handle(
                "events.outbox.redelivered",
                "evt-act",
                &serde_json::json!({"outbox_id": 4242}),
                FIRST,
                Some(at),
            )
            .await;
        assert!(matches!(res, boss_nats::durable::Settle::Ack));
        let calls = recorder.calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].event_timestamp, Some(at));
    }

    /// A delayed rule never fires at match time (backlog 84a010cb). The
    /// authoring validator refuses a `delay`, but an ACTIVE row is loaded
    /// by `load_active_rules` -> `Rule::from_raw`, which never calls it:
    /// a row written before car 4c706c5d, or by any door that skips the
    /// validator, used to match and run its handlers on the triggering
    /// event — a seven-day follow-up admitted the same second. Until the
    /// durable delayed-admission contract (design f1d66249) executes the
    /// delay, the runner refuses the rule loudly and fires nothing.
    #[tokio::test]
    async fn handle_never_fires_a_delayed_rule_at_match_time() {
        let toml = r#"
[[rule]]
name = "follow-up-after-silence"
on_event = "jobs.job.created"
delay = "7d"
[[rule.do]]
handler = "rec"

[[rule]]
name = "immediate-neighbour"
on_event = "jobs.job.created"
[[rule.do]]
handler = "rec"
"#;
        let recorder = handler::RecordingHandler::new("rec");
        let mut handlers = HandlerRegistry::new();
        handlers.register(recorder.clone());
        let runner = RulesRunner {
            delivery_routes: None,
            registry: Registry::from_toml(toml).unwrap(),
            handlers,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        };
        let res = runner
            .handle(
                "jobs.job.created",
                "evt-inquiry",
                &serde_json::json!({"id": "j-1"}),
                FIRST,
                None,
            )
            .await;
        assert!(
            matches!(res, boss_nats::durable::Settle::Ack),
            "a refused delayed rule is permanent, never a retry: {}",
            settle_name(&res)
        );
        // Positive control: the immediate neighbour on the same topic
        // still fires, exactly once — and the delayed rule adds nothing.
        assert_eq!(
            recorder.calls().await.len(),
            1,
            "only the immediate rule's handler ran; the delayed one fired at match time"
        );
    }

    struct FixedHandler {
        name: &'static str,
        result: fn() -> Result<(), crate::rules::handler::HandlerError>,
    }
    #[async_trait::async_trait]
    impl crate::rules::handler::Handler for FixedHandler {
        fn name(&self) -> &'static str {
            self.name
        }
        async fn invoke(
            &self,
            _args: &[(String, crate::rules::expr::Value)],
            _ctx: &crate::rules::handler::InvocationContext,
        ) -> Result<(), crate::rules::handler::HandlerError> {
            (self.result)()
        }
    }

    fn runner_with(
        handlers: Vec<(
            &'static str,
            fn() -> Result<(), crate::rules::handler::HandlerError>,
        )>,
        rules_toml: &str,
    ) -> RulesRunner {
        let mut reg = HandlerRegistry::new();
        for (name, result) in handlers {
            reg.register(std::sync::Arc::new(FixedHandler { name, result }));
        }
        RulesRunner {
            delivery_routes: None,
            registry: Registry::from_toml(rules_toml).unwrap(),
            handlers: reg,
            helpers: Arc::new(NoHelpers),
            dead_letters: None,
            live: None,
            firings: None,
        }
    }

    const TWO_HANDLER_RULES: &str = r#"
[[rule]]
name = "r-perm"
on_event = "step.done.x"
[[rule.do]]
handler = "h.perm"
[[rule]]
name = "r-trans"
on_event = "step.done.x"
[[rule.do]]
handler = "h.trans"
"#;

    /// The real matcher invokes a synchronous helper. This in-memory
    /// port holds the same block_in_place boundary as InventoryHelpers'
    /// HTTP bridge; the signal proves arrival before any timeout judges
    /// the unrelated event's progress. No production delay is inferred.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_waiting_match_helper_does_not_stop_other_events_from_being_polled() {
        struct HeldHelper {
            entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl HelperResolver for HeldHelper {
            fn call(
                &self,
                _name: &str,
                _args: &[crate::rules::expr::Value],
            ) -> Result<crate::rules::expr::Value, crate::rules::expr::EvalError> {
                tokio::task::block_in_place(|| {
                    self.entered
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                });
                Ok(crate::rules::expr::Value::Bool(true))
            }
        }
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut runner = runner_with(
            vec![],
            r#"
[[rule]]
name = "slow"
on_event = "step.done.slow"
when = "gate()"
[[rule.do]]
handler = "h.mark"
[[rule]]
name = "fast"
on_event = "step.done.fast"
[[rule.do]]
handler = "h.mark"
"#,
        );
        let recorder = handler::RecordingHandler::new("h.mark");
        runner.handlers.register(recorder.clone());
        runner.helpers = Arc::new(HeldHelper {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(release_rx),
        });
        let runner = Arc::new(runner);
        let drive = tokio::spawn(async move {
            futures::stream::iter(["step.done.slow", "step.done.fast"])
                .for_each_concurrent(MAX_CONCURRENT_EVENTS, |topic| {
                    let runner = runner.clone();
                    async move {
                        assert!(matches!(
                            runner
                                .handle_owned(
                                    topic.into(),
                                    topic.into(),
                                    serde_json::json!({}),
                                    FIRST,
                                    None,
                                    None
                                )
                                .await,
                            boss_nats::durable::Settle::Ack
                        ));
                    }
                })
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        let progressed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !recorder
                .calls()
                .await
                .iter()
                .any(|call| call.triggering_topic == "step.done.fast")
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        release_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), drive)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recorder.calls().await.len(),
            2,
            "both legitimate handlers still fire after release"
        );
        assert!(
            progressed,
            "a helper suspended the shared stream poller despite its 12-wide limit"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_the_event_owner_cannot_resume_from_a_read_into_a_handler() {
        struct HeldHelper {
            entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl HelperResolver for HeldHelper {
            fn call(
                &self,
                _name: &str,
                _args: &[crate::rules::expr::Value],
            ) -> Result<crate::rules::expr::Value, crate::rules::expr::EvalError> {
                tokio::task::block_in_place(|| {
                    self.entered
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .send(())
                        .unwrap();
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                });
                Ok(crate::rules::expr::Value::Bool(true))
            }
        }
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut runner = runner_with(
            vec![],
            r#"
[[rule]]
name = "slow"
on_event = "step.done.slow"
when = "gate()"
[[rule.do]]
handler = "h.mark"
"#,
        );
        let recorder = handler::RecordingHandler::new("h.mark");
        runner.handlers.register(recorder.clone());
        runner.helpers = Arc::new(HeldHelper {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(release_rx),
        });
        let runner = Arc::new(runner);
        let owner = tokio::spawn(runner.clone().handle_owned(
            "step.done.slow".into(),
            "cancel".into(),
            serde_json::json!({}),
            FIRST,
            None,
            None,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .unwrap()
            .unwrap();
        owner.abort();
        assert!(matches!(owner.await,Err(error) if error.is_cancelled()));
        release_tx.send(()).unwrap();
        // The last owned Arc disappearing is completion evidence, not a
        // sleep standing in for the blocked read's return.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&runner) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            recorder.calls().await.is_empty(),
            "no handler after owner cancellation"
        );
    }

    #[tokio::test]
    async fn an_owned_task_failure_retries_instead_of_acknowledging_work() {
        let mut runner = runner_with(
            vec![
                ("h.perm", || panic!("fixture owned task panic")),
                ("h.trans", || Ok(())),
            ],
            TWO_HANDLER_RULES,
        );
        let live = Arc::new(crate::liveness::DispatcherLiveness::default());
        runner.live = Some(live.clone());
        let settle = Arc::new(runner)
            .handle_owned(
                "step.done.x".into(),
                "panic".into(),
                serde_json::json!({}),
                FINAL,
                None,
                None,
            )
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        assert_eq!(live.snapshot()["dead_letters_unrecorded"], 1);
    }

    /// Measurement, not a model of production SQL latency: the real
    /// handle path pays its recording sink before settling the event.
    #[tokio::test]
    async fn measure_firing_record_cost_on_the_event_settlement_path() {
        struct DelayedRecord {
            calls: std::sync::atomic::AtomicUsize,
            rows: std::sync::atomic::AtomicUsize,
        }
        #[async_trait::async_trait]
        impl FiringSink for DelayedRecord {
            async fn record(&self, rows: &[Firing]) -> Result<(), String> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.rows
                    .fetch_add(rows.len(), std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                Ok(())
            }
        }
        let sink = Arc::new(DelayedRecord {
            calls: std::sync::atomic::AtomicUsize::new(0),
            rows: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut runner = runner_with(
            vec![("h.perm", || Ok(())), ("h.trans", || Ok(()))],
            TWO_HANDLER_RULES,
        );
        let live = Arc::new(crate::liveness::DispatcherLiveness::default());
        runner.live = Some(live.clone());
        runner.firings = Some(sink.clone());
        let start = std::time::Instant::now();
        futures::stream::iter(0..24)
            .for_each_concurrent(MAX_CONCURRENT_EVENTS, |n| {
                let runner = &runner;
                async move {
                    assert!(matches!(
                        runner
                            .handle(
                                "step.done.x",
                                &format!("measure-{n}"),
                                &serde_json::json!({}),
                                FIRST,
                                None
                            )
                            .await,
                        boss_nats::durable::Settle::Ack
                    ));
                }
            })
            .await;
        let elapsed = start.elapsed();
        assert_eq!(sink.calls.load(std::sync::atomic::Ordering::SeqCst), 24);
        assert_eq!(sink.rows.load(std::sync::atomic::Ordering::SeqCst), 48);
        assert!(elapsed >= std::time::Duration::from_millis(240));
        println!(
            "MEASURE events=24 matched_rules=48 handler_failures=0 record_calls=24 record_rows=48 injected_record_ms=120 concurrency={} elapsed_ns={} dedup_lookup_calls=0 (source path has no firing-dedup lookup; fixture latency is not production attribution)",
            MAX_CONCURRENT_EVENTS,
            elapsed.as_nanos()
        );
        assert_eq!(live.observation()["rules_phase_costs"]["events"], 24);
        let costs = live.observation()["rules_phase_costs"].clone();
        assert_eq!(costs["dispatch_calls"], 24);
        assert_eq!(costs["record_calls"], 24);
        assert_eq!(costs["record_rows_attempted"], 48);
        assert!(
            costs["record_nanos"].as_u64().unwrap() >= 24 * 120_000_000,
            "summed completed recording work, not concurrent wall time: {costs}"
        );
    }

    /// Queue item 4 (HandlerError::Permanent): all-deterministic
    /// failures Term immediately; ANY transient in the mix NAKs so the
    /// idempotent re-run can converge the transients.
    #[tokio::test]
    async fn all_permanent_failures_term_immediately() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Permanent("422 second typo".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}), FIRST, None)
            .await
        {
            boss_nats::durable::Settle::Permanent(msg) => {
                assert!(msg.contains("seed typo"), "{msg}");
            }
            other => panic!("expected Permanent, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn any_transient_failure_naks_for_redelivery() {
        use crate::rules::handler::HandlerError;
        let runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(HandlerError::Permanent("422 seed typo".into()))
                }),
                ("h.trans", || {
                    Err(HandlerError::Downstream("503 not yet".into()))
                }),
            ],
            TWO_HANDLER_RULES,
        );
        match runner
            .handle("step.done.x", "e1", &serde_json::json!({}), FIRST, None)
            .await
        {
            boss_nats::durable::Settle::Retry(msg) => {
                assert!(msg.contains("2 handler(s) failed"), "{msg}");
            }
            other => panic!("expected Retry, got {}", settle_name(&other)),
        }
    }

    #[tokio::test]
    async fn success_acks() {
        let runner = runner_with(
            vec![("h.perm", || Ok(())), ("h.trans", || Ok(()))],
            TWO_HANDLER_RULES,
        );
        assert!(matches!(
            runner
                .handle("step.done.x", "e1", &serde_json::json!({}), FIRST, None)
                .await,
            boss_nats::durable::Settle::Ack
        ));
    }

    // -----------------------------------------------------------------
    // A dead-letter lands on its packet (`a9c498eb`)
    // -----------------------------------------------------------------

    const ONE_RULE: &str = r#"
[[rule]]
name = "inspect-empty-decisions-sweep-on-step-ready"
on_event = "step.ready.checklist"
[[rule.do]]
handler = "maintenance.sweep.inspect"
"#;

    fn sweep_payload() -> Value {
        // The shape `step.ready.*` publishes, as the measured instance
        // carried it: packet f8dedadf's Inspect checklist.
        serde_json::json!({
            "job_id": "f8dedadf-0000-0000-0000-000000000001",
            "step_id": "5ca1ab1e-0000-0000-0000-000000000002",
            "kind": "checklist",
        })
    }

    fn sweep_runner(
        result: fn() -> Result<(), crate::rules::handler::HandlerError>,
        sink: Arc<RecordingDeadLetters>,
    ) -> RulesRunner {
        let mut runner = runner_with(vec![("maintenance.sweep.inspect", result)], ONE_RULE);
        runner.dead_letters = Some(sink);
        runner
    }

    /// THE DEFECT. A handler that fails past its budget left the packet
    /// exactly as it was: the step sitting `ready`, and the only record of
    /// the failure in a pod log that a converge roll erases. The
    /// dead-letter must land on the packet, naming the rule, the handler,
    /// the attempt count and the error.
    #[tokio::test]
    async fn a_budget_exhausted_failure_lands_on_the_packet() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(
            || {
                Err(HandlerError::Downstream(
                    "GET /api/jobs returned 503".into(),
                ))
            },
            sink.clone(),
        );
        let settle = runner
            .handle(
                "step.ready.checklist",
                "evt-sweep",
                &sweep_payload(),
                FINAL,
                None,
            )
            .await;
        assert!(
            matches!(settle, boss_nats::durable::Settle::Retry(_)),
            "the settle is unchanged: the transport still Terms the spent message"
        );
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 1, "one dead-letter, landed once");
        let (target, note) = &landed[0];
        assert_eq!(target.job_id, "f8dedadf-0000-0000-0000-000000000001");
        assert_eq!(
            target.step_id.as_deref(),
            Some("5ca1ab1e-0000-0000-0000-000000000002"),
            "the troubled step is named: it is the one a reader finds `ready`"
        );
        assert_eq!(note.attempts, FINAL);
        assert_eq!(note.class, DeadLetterClass::BudgetExhausted);
        assert_eq!(note.topic, "step.ready.checklist");
        assert_eq!(note.event_id, "evt-sweep");
        let f = note.failures.first().expect("one failure");
        assert_eq!(f.rule, "inspect-empty-decisions-sweep-on-step-ready");
        assert_eq!(f.handler, "maintenance.sweep.inspect");
        assert!(
            f.error.contains("503"),
            "the note carries what failed, not that something did: {}",
            f.error
        );
    }

    /// A permanent failure dead-letters on its FIRST delivery (it skips
    /// the budget), so the annotation must not wait for the count.
    #[tokio::test]
    async fn a_permanent_failure_lands_on_its_first_delivery() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(
            || Err(HandlerError::Permanent("422 unknown sweep target".into())),
            sink.clone(),
        );
        let settle = runner
            .handle(
                "step.ready.checklist",
                "evt-sweep",
                &sweep_payload(),
                FIRST,
                None,
            )
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Permanent(_)));
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 1, "a Term IS a dead-letter: {landed:?}");
        assert_eq!(landed[0].1.class, DeadLetterClass::Permanent);
        assert_eq!(landed[0].1.attempts, FIRST);
    }

    /// With budget left the NAK may still converge. Marking the packet
    /// troubled now would cry wolf on work that is about to be fine.
    #[tokio::test]
    async fn a_transient_failure_with_budget_left_annotates_nothing() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        for attempt in FIRST..FINAL {
            let settle = runner
                .handle(
                    "step.ready.checklist",
                    "evt-sweep",
                    &sweep_payload(),
                    attempt,
                    None,
                )
                .await;
            assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        }
        assert!(
            sink.landed().await.is_empty(),
            "nothing is annotated while redelivery can still succeed"
        );
    }

    /// BEST-EFFORT. The annotation is itself a jobs-API write, and the
    /// jobs API may be the very thing that failed. Its failure must not
    /// change the settle — or a down API would turn one dead-letter into
    /// eight more deliveries of the same event.
    #[tokio::test]
    async fn an_annotation_that_fails_does_not_change_the_settle() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::failing();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        let settle = runner
            .handle(
                "step.ready.checklist",
                "evt-sweep",
                &sweep_payload(),
                FINAL,
                None,
            )
            .await;
        match settle {
            boss_nats::durable::Settle::Retry(msg) => assert!(
                msg.contains("maintenance.sweep.inspect"),
                "the settle still names the handler that failed: {msg}"
            ),
            other => panic!(
                "a failed annotation must leave the settle alone, got {}",
                settle_name(&other)
            ),
        }
        assert_eq!(
            sink.landed().await.len(),
            1,
            "it was attempted once and not retried"
        );
    }

    /// IDEMPOTENCE. A redelivery (or a restart re-presenting the row with
    /// a fresh budget) must leave one annotation's worth of metadata: one
    /// top-level key, overwritten. See `dead_letter::annotation_patch`
    /// for the merge itself.
    #[tokio::test]
    async fn two_dead_letter_deliveries_write_one_metadata_key() {
        use crate::rules::dead_letter::{METADATA_KEY, annotation_patch};
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        for _ in 0..2 {
            runner
                .handle(
                    "step.ready.checklist",
                    "evt-sweep",
                    &sweep_payload(),
                    FINAL,
                    None,
                )
                .await;
        }
        let landed = sink.landed().await;
        assert_eq!(landed.len(), 2, "both deliveries annotate");
        let mut metadata = serde_json::Map::new();
        for (_, note) in &landed {
            for (k, v) in annotation_patch(note, chrono::Utc::now())
                .as_object()
                .expect("patch is an object")
            {
                metadata.insert(k.clone(), v.clone());
            }
        }
        assert_eq!(
            metadata.keys().collect::<Vec<_>>(),
            vec![METADATA_KEY],
            "two deliveries, one annotation's worth of metadata"
        );
    }

    /// A topic whose subject is not a packet has nothing to annotate. The
    /// honest answer is the log line, NOT a guess at which job shares the
    /// subject's uuid.
    #[tokio::test]
    async fn an_event_with_no_packet_annotates_nothing() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let mut runner = runner_with(
            vec![("h.invoice", || Err(HandlerError::Downstream("503".into())))],
            r#"
[[rule]]
name = "r-invoice"
on_event = "commerce.invoice.paid"
[[rule.do]]
handler = "h.invoice"
"#,
        );
        runner.dead_letters = Some(sink.clone());
        let settle = runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv",
                &serde_json::json!({"id": "inv-1"}),
                FINAL,
                None,
            )
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        assert!(
            sink.landed().await.is_empty(),
            "no packet means no annotation, not a fabricated one"
        );
    }

    /// THE COUNTER THAT OWES NOTHING TO THE JOBS API (8834804a). A
    /// dead-letter with no packet, or whose annotation write itself
    /// failed, had a pod's log as its only record. Both now count on
    /// the liveness surface (/api/dispatcher/health), which is a local
    /// read: total dead-letters, how many left NO durable record, and
    /// when the last one happened — so an outage of the very API the
    /// annotation needs still leaves a number a reader can see.
    #[tokio::test]
    async fn a_dead_letter_is_counted_on_the_liveness_surface_even_when_it_cannot_be_recorded() {
        use crate::rules::handler::HandlerError;
        let live = std::sync::Arc::new(crate::liveness::DispatcherLiveness::default());
        // No packet to annotate: counted, and counted as unrecorded.
        let sink = RecordingDeadLetters::new();
        let mut runner = runner_with(
            vec![("h.invoice", || Err(HandlerError::Downstream("503".into())))],
            r#"
[[rule]]
name = "r-invoice"
on_event = "commerce.invoice.paid"
[[rule.do]]
handler = "h.invoice"
"#,
        );
        runner.dead_letters = Some(sink.clone());
        runner.live = Some(live.clone());
        runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv",
                &serde_json::json!({"id": "inv-1"}),
                FINAL,
                None,
            )
            .await;
        let snap = live.snapshot();
        assert_eq!(snap["dead_letters"], 1, "{snap}");
        assert_eq!(
            snap["dead_letters_unrecorded"], 1,
            "no packet: no durable record: {snap}"
        );
        assert!(
            snap["last_dead_letter_unix"].as_i64().unwrap_or(0) > 0,
            "{snap}"
        );

        // A recorded one: counted, not unrecorded.
        let mut runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        runner.live = Some(live.clone());
        runner
            .handle(
                "step.ready.checklist",
                "evt-sweep",
                &sweep_payload(),
                FINAL,
                None,
            )
            .await;
        let snap = live.snapshot();
        assert_eq!(snap["dead_letters"], 2, "{snap}");
        assert_eq!(
            snap["dead_letters_unrecorded"], 1,
            "the landed one is recorded: {snap}"
        );

        // A sink that fails is the jobs API being what broke: counted as unrecorded.
        let mut runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        runner.dead_letters = Some(Arc::new(FailingDeadLetters));
        runner.live = Some(live.clone());
        runner
            .handle(
                "step.ready.checklist",
                "evt-sweep-2",
                &sweep_payload(),
                FINAL,
                None,
            )
            .await;
        let snap = live.snapshot();
        assert_eq!(snap["dead_letters"], 3, "{snap}");
        assert_eq!(
            snap["dead_letters_unrecorded"], 2,
            "a failed write left no record: {snap}"
        );
    }

    fn invoice_runner() -> RulesRunner {
        use crate::rules::handler::HandlerError;
        let mut runner = runner_with(
            vec![("h.invoice", || Err(HandlerError::Downstream("503".into())))],
            r#"
[[rule]]
name = "r-invoice"
on_event = "commerce.invoice.paid"
[[rule.do]]
handler = "h.invoice"
"#,
        );
        runner.dead_letters = Some(RecordingDeadLetters::new());
        runner
    }

    /// 4b175523. A dead-letter on a topic that names no packet left a
    /// log line and a counter that resets with the pod, so the rules
    /// list's "no failures" could not be told from loss. It is now a
    /// durable `dead-letter` row in the firing record, idempotent on
    /// the rule and the event, and the counter calls it recorded.
    #[tokio::test]
    async fn a_dead_letter_with_no_packet_is_recorded_in_the_firing_record() {
        let live = Arc::new(crate::liveness::DispatcherLiveness::default());
        let firings = RecordingFirings::new();
        let mut runner = invoice_runner();
        runner.firings = Some(firings.clone());
        runner.live = Some(live.clone());
        let settle = runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv",
                &serde_json::json!({"id": "inv-1"}),
                FINAL,
                None,
            )
            .await;
        assert!(matches!(settle, boss_nats::durable::Settle::Retry(_)));
        let rows = firings.recorded.lock().await.clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].outcome, Outcome::DeadLetter);
        assert_eq!(rows[0].rule, "r-invoice");
        assert_eq!(rows[0].firing_id, "dead-letter:r-invoice:evt-inv");
        assert_eq!(rows[0].fired_on, "commerce.invoice.paid");
        assert_eq!(rows[0].detail["attempts"], FINAL);
        assert_eq!(rows[0].detail["class"], "budget-exhausted");
        assert!(
            rows[0].detail["failures"][0]
                .as_str()
                .is_some_and(|f| f.starts_with("r-invoice/h.invoice: ") && f.contains("503")),
            "the row carries what failed: {}",
            rows[0].detail
        );
        let snap = live.snapshot();
        assert_eq!(snap["dead_letters"], 1, "{snap}");
        assert_eq!(
            snap["dead_letters_unrecorded"], 0,
            "a durable row is a record: {snap}"
        );

        // With budget left nothing is recorded: the NAK may still converge.
        let firings = RecordingFirings::new();
        let mut runner = invoice_runner();
        runner.firings = Some(firings.clone());
        runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv-2",
                &serde_json::json!({"id": "inv-2"}),
                FIRST,
                None,
            )
            .await;
        assert!(firings.recorded.lock().await.is_empty());
    }

    /// BEST-EFFORT: the durable write failing leaves the settle alone
    /// and counts the dead-letter as unrecorded.
    #[tokio::test]
    async fn a_dead_letter_record_that_cannot_be_written_never_changes_the_settle() {
        let live = Arc::new(crate::liveness::DispatcherLiveness::default());
        let mut runner = invoice_runner();
        runner.firings = Some(Arc::new(FailingFirings));
        runner.live = Some(live.clone());
        let settle = runner
            .handle(
                "commerce.invoice.paid",
                "evt-inv",
                &serde_json::json!({"id": "inv-1"}),
                FINAL,
                None,
            )
            .await;
        assert!(
            matches!(settle, boss_nats::durable::Settle::Retry(_)),
            "the settle is the transport's, whatever the record did"
        );
        let snap = live.snapshot();
        assert_eq!(snap["dead_letters_unrecorded"], 1, "{snap}");
    }

    struct FailingDeadLetters;
    #[async_trait::async_trait]
    impl DeadLetterSink for FailingDeadLetters {
        async fn record(&self, _t: &Target, _n: &DeadLetterNote) -> Result<(), String> {
            Err("PATCH returned 503".into())
        }
    }

    /// Sim-ness is inherited from the triggering event, the same read
    /// `handler::dispatch` makes, so an annotation about simulated work
    /// is simulated state.
    #[tokio::test]
    async fn the_annotation_inherits_the_events_sim_ness() {
        use crate::rules::handler::HandlerError;
        let sink = RecordingDeadLetters::new();
        let runner = sweep_runner(|| Err(HandlerError::Downstream("503".into())), sink.clone());
        let mut payload = sweep_payload();
        payload["_simulated"] = serde_json::json!(true);
        runner
            .handle("step.ready.checklist", "evt-sweep", &payload, FINAL, None)
            .await;
        assert!(sink.landed().await[0].1.simulated);
    }

    /// Both transports must agree on when the budget is spent, because
    /// the runner predicts the dead-letter from the attempt count. The
    /// log tail derives its budget from the JetStream one; this is the
    /// assertion that the derivation is the whole of it.
    #[test]
    fn both_transports_share_one_retry_budget() {
        assert_eq!(
            crate::rules::log_tail::MAX_ATTEMPTS as i64,
            boss_nats::durable::MAX_DELIVER
        );
        assert!(boss_nats::durable::is_dead_letter(FINAL as i64));
        assert!(!boss_nats::durable::is_dead_letter(FINAL as i64 - 1));
    }

    fn settle_name(s: &boss_nats::durable::Settle) -> &'static str {
        match s {
            boss_nats::durable::Settle::Ack => "Ack",
            boss_nats::durable::Settle::Hold(_) => "Hold",
            boss_nats::durable::Settle::Retry(_) => "Retry",
            boss_nats::durable::Settle::Permanent(_) => "Permanent",
        }
    }

    #[tokio::test]
    async fn a_rule_that_fired_is_recorded_with_its_events_own_id() {
        // b14afc48: the hop a dispatcher rule moves could not prove it
        // ran, because nothing wrote down that a rule fired.
        let firings = RecordingFirings::new();
        let mut runner = runner_with(vec![("h.ok", || Ok(()))], ONE_OK_RULE);
        runner.firings = Some(firings.clone());
        let res = runner
            .handle("step.done.x", "evt-7", &serde_json::json!({}), FIRST, None)
            .await;
        assert!(matches!(res, boss_nats::durable::Settle::Ack));
        let rows = firings.recorded.lock().await.clone();
        assert_eq!(rows.len(), 1, "one firing per rule that fired: {rows:?}");
        assert_eq!(rows[0].rule, "r-ok");
        assert_eq!(rows[0].fired_on, "step.done.x");
        assert_eq!(
            rows[0].firing_id, "dispatcher:r-ok:evt-7",
            "the id is the rule and the triggering event, so a redelivery dedupes"
        );
        assert_eq!(
            rows[0].detail.get("event_id").and_then(|v| v.as_str()),
            Some("evt-7")
        );
    }

    #[tokio::test]
    async fn a_rule_whose_handler_failed_is_not_recorded_as_having_fired() {
        // The record says what RAN. A rule whose handler failed did not
        // move the packet — its record is the dead-letter, and a firing
        // row for it would be the false green this surface exists to
        // refuse. The rule that DID succeed on the same event is still
        // recorded: one event, two rules, two different truths.
        let firings = RecordingFirings::new();
        let mut runner = runner_with(
            vec![
                ("h.perm", || {
                    Err(crate::rules::handler::HandlerError::Permanent(
                        "bad arg".into(),
                    ))
                }),
                ("h.trans", || Ok(())),
            ],
            TWO_HANDLER_RULES,
        );
        runner.firings = Some(firings.clone());
        let _ = runner
            .handle("step.done.x", "evt-9", &serde_json::json!({}), FIRST, None)
            .await;
        assert_eq!(
            firings.rules().await,
            vec!["r-trans".to_string()],
            "only the rule whose handlers succeeded is recorded"
        );
    }

    #[tokio::test]
    async fn a_firing_record_that_cannot_be_written_never_changes_the_settle() {
        // The arm must not break the thing it observes: a side effect
        // that landed must not be redelivered because its firing row
        // could not be written.
        let mut runner = runner_with(vec![("h.ok", || Ok(()))], ONE_OK_RULE);
        runner.firings = Some(Arc::new(FailingFirings));
        let res = runner
            .handle("step.done.x", "evt-11", &serde_json::json!({}), FIRST, None)
            .await;
        assert!(
            matches!(res, boss_nats::durable::Settle::Ack),
            "a failed firing record must still ACK the delivered event"
        );
    }

    const ONE_OK_RULE: &str = r#"
[[rule]]
name = "r-ok"
on_event = "step.done.x"
[[rule.do]]
handler = "h.ok"
"#;
    struct UnknownDeliveryRoutes;
    #[async_trait::async_trait]
    impl super::super::delivery::DeliveryRoutes for UnknownDeliveryRoutes {
        async fn permitted(
            &self,
            _names: &[String],
            _position: Option<&super::super::delivery::Position>,
        ) -> Result<std::collections::BTreeSet<String>, String> {
            Err("authoritative cutover temporarily unavailable".into())
        }
    }
    #[tokio::test]
    async fn unknown_delivery_routes_hold_without_spending_handler_retry_budget() {
        let mut runner = runner_with(vec![("h.ok", || Ok(()))], ONE_OK_RULE);
        let ordinary = runner
            .handle(
                "step.done.x",
                "evt-ordinary",
                &serde_json::json!({}),
                FIRST,
                None,
            )
            .await;
        assert_eq!(
            settle_name(&ordinary),
            "Ack",
            "ordinary effects remain admitted"
        );
        runner.delivery_routes = Some(Arc::new(UnknownDeliveryRoutes));
        for attempt in [FIRST, FINAL, FINAL + 1] {
            let outcome = runner
                .handle(
                    "step.done.x",
                    "evt-held",
                    &serde_json::json!({}),
                    attempt,
                    None,
                )
                .await;
            assert_eq!(
                settle_name(&outcome),
                "Hold",
                "unknown routing is not a handler failure or a spent delivery attempt"
            );
        }
    }

    /// Longer than one request on a dark bus takes to give up: the
    /// client's ten-second request timeout (the window car 2 measured,
    /// `boss_nats::durable`'s `DARK_WINDOW`).
    const DARK_WINDOW: std::time::Duration = std::time::Duration::from_secs(15);

    /// Backlog 2e45840a, design ad9519bc car 3: an independent delivery
    /// lane opened its consumer ONCE — `get_stream` then `get_consumer`
    /// — and on a dark bus that one-shot read timed out and the lane's
    /// task returned, so the lane's rules stayed off until a registry
    /// reload. Car 2 gave the default consumer a capped-backoff open
    /// and left this one; the control is car 2's
    /// `a_one_shot_open_on_a_dark_bus_gives_up_inside_the_window`.
    #[tokio::test]
    async fn a_delivery_lane_on_a_dark_bus_waits_rather_than_giving_up() {
        let dark = boss_testing::dark_port();
        let bus = boss_nats::NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a connect error (car 1)");
        let runner = runner_with(
            vec![],
            r#"
[[rule]]
name = "watch"
on_event = "jobs.one"
delivery_lane = "watch"
[[rule.do]]
handler = "h.mark"
"#,
        );
        let lane = super::super::delivery::Cutover {
            lane: "watch".into(),
            consumer: "dispatcher-lane-watch".into(),
            stream_created_ns: "0".into(),
            consumer_created_ns: "0".into(),
            created_at: "2026-11-10T00:00:00Z".into(),
            after_stream_sequence: 0,
            members: std::collections::BTreeMap::from([("watch".to_string(), 1)]),
        };
        let live = Arc::new(crate::liveness::DispatcherLiveness::default());
        let ran = tokio::time::timeout(
            DARK_WINDOW,
            runner.run_delivery(bus.jetstream(), live, Some(lane)),
        )
        .await;
        assert!(
            ran.is_err(),
            "nothing listens at {}; the lane must still be waiting for the bus, not answered: {ran:?}",
            dark.addr
        );
    }
}
