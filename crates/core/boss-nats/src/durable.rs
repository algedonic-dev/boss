//! JetStream durability layer for the event bus.
//!
//! Plain core NATS is at-most-once: a message is delivered once and, if
//! the subscriber's handler fails, the work is simply lost. For the
//! dispatcher's consumers that meant a transient hiccup — a briefly
//! unreachable policy-api, a saturated Postgres — silently *orphaned* the
//! Job whose step-completion event got dropped.
//!
//! This module adds a durable delivery buffer on top of the same NATS
//! server. A JetStream stream ([`STREAM_NAME`]) captures the job/step
//! lifecycle subjects; the dispatcher binds **durable pull consumers**
//! ([`open_durable`]) that explicitly ACK on success and NAK on failure.
//! A NAK redelivers the message on a backoff schedule, so a transient
//! failure self-heals once the underlying condition clears, and only a
//! genuinely-unprocessable event (exhausting [`MAX_DELIVER`]) is dropped —
//! loudly, as a dead-letter.
//!
//! This is **not** a second system of record. The Postgres `audit_log`
//! remains authoritative; the stream is a bounded, expiring buffer whose
//! sole job is reliable in-flight delivery. Publishers call
//! `EventBus::publish`; the NATS adapter publishes a subject the stream
//! covers ([`stream_captures`]) through JetStream and answers Ok only on
//! the stream's PubAck, so a publisher's Ok means the buffer holds the
//! event (backlog 2e45840a). A core publish to a covered subject is
//! still captured — it is just never confirmed.

use async_nats::jetstream::consumer::pull::{MessagesError, MessagesErrorKind};
use async_nats::jetstream::consumer::{AckPolicy, pull};
use async_nats::jetstream::context::{GetStreamError, GetStreamErrorKind};
use async_nats::jetstream::{AckKind, Context, ErrorCode, Message, stream};
use std::time::Duration;
use tracing::{error, info, warn};

/// Canonical durable stream name. One stream buffers every subject the
/// dispatcher must consume reliably.
pub const STREAM_NAME: &str = "BOSS_EVENTS";

/// Redelivery budget: how many times a NAK'd message is retried before it
/// is dead-lettered. Sized (with [`redelivery_backoff`]) to ride out a
/// multi-minute transient — DB saturation, a briefly-unreachable
/// downstream — then give up rather than loop a poison event forever.
pub const MAX_DELIVER: i64 = 8;

/// How long the server waits for an ACK before assuming the consumer died
/// and redelivering. The consumers NAK explicitly on handler failure, so
/// this is the crash-safety backstop, not the normal retry path.
///
/// This only holds if no `backoff` schedule is set on the consumer: the
/// server treats a backoff list as the per-attempt ack deadline, so a
/// schedule starting at 1s silently shrank the effective ack_wait to 1s
/// — any event whose queue-wait + handler time crossed 1s was redelivered
/// while its first delivery was still succeeding, and every side effect
/// ran twice with no NAK anywhere in the logs (facts/JEs survived on
/// idempotency keys; unconditional audit-event emits doubled). Retry
/// pacing therefore lives in [`settle`]'s explicit NAK delay, never in
/// the consumer config.
const ACK_WAIT: Duration = Duration::from_secs(30);

/// Subjects the stream captures. MUST cover every family the rule
/// registry references: a durable consumer's filter may legally name
/// subjects the stream doesn't ingest, and the result is zero
/// deliveries with zero errors — perfectly silent dead air. That is
/// exactly how 6b's `commerce.invoice.created` rule (the consume that
/// owns COGS) fired zero times across a full year run while every
/// gate stayed green (2026-07-10): the filter accepted the topic, the
/// stream never stored the messages. The
/// `stream_covers_every_rule_topic` test in boss-dispatcher pins this
/// list against the shipped rule registry — and writing that test
/// immediately surfaced a SECOND casualty: the tax-filing webhook
/// forward (`ledger.tax_filing_filed`) had been dead air the same
/// way. Domain families with no rule topic still flow over core NATS
/// only; the test starts failing the moment a rule consumes one.
pub fn stream_subjects() -> Vec<String> {
    // `asset.>` joined 2026-08-08 (feedback `0da79b36`): the assets
    // ingress rebuilds its repository from these events, and consuming
    // them at-most-once off core NATS silently dropped writes. The
    // subject must be HERE for the `assets-ingress` durable consumer
    // to receive anything — `ensure_stream` reconciles a live broker's
    // subject list on every service start, so adding a subject in code
    // reaches prod on deploy.
    [
        "jobs.>",
        "step.>",
        "inventory.>",
        "commerce.>",
        "ledger.>",
        "asset.>",
        // `docs.>` joined 2026-08-10 (dogfooding arc e556c000, S2):
        // the design-review-spawn rule consumed `docs.design.indexed`,
        // and the coverage tripwire below this comment's lineage
        // (stream_covers_every_rule_topic) failed the build until the
        // family flowed into the stream — the exact silent-zero class
        // it was written for, caught at compile-adjacent time instead
        // of in production.
        //
        // ITS ONE CONSUMER IS GONE (2026-09-10, backlog f5da586c): the
        // rule was retired with the corpus index, and nothing publishes
        // a `docs.*` event any more. The subject is KEPT ANYWAY, which
        // is a deliberate exception to deleting what has no caller.
        // `ensure_stream` reconciles a LIVE broker's subject list on
        // every service start, so narrowing this array is not an edit
        // to a config file — it is a stream update against production,
        // and a stream update that drops a subject a persisted durable
        // consumer still filters on can be refused. A refusal here
        // fails `ensure_stream`, which fails the start of every service
        // that calls it. That trade — an outage risk on every boot
        // against one unmatched subject pattern, which costs nothing
        // because nothing publishes to it — is not worth taking inside
        // a deletion. Retained `docs.*` messages also still sit in the
        // buffer until the age cap expires them, and a subject the
        // stream no longer lists is a message nothing can replay.
        "docs.>",
        // `events.>` joined 2026-09-28 (backlog e4019cbc): the outbox
        // relay's `events.outbox.dead_lettered` alarm is read by the
        // dispatcher rule that files its backlog-item, and a rule
        // topic the stream does not capture is delivered to nobody.
        // Adding a subject is the safe direction of the live
        // reconcile — the risk the `docs.>` note above describes is
        // dropping one.
        "events.>",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Does the [`STREAM_NAME`] stream capture a publish to `subject`? The
/// NATS adapter asks this of every publish (backlog 2e45840a): a subject
/// the stream captures is published through JetStream and is Ok only on
/// the stream's PubAck, because the outbox relay stamps `delivered_at` on
/// Ok and the stream is the delivery buffer the durable consumers read.
/// Any other subject has no stream to ack it, so it goes as core NATS. A
/// subject the client would refuse to send is captured by nothing, so it
/// takes the core path, whose refusal the relay dead-letters (e4019cbc).
pub fn stream_captures(subject: &str) -> bool {
    async_nats::Subject::from(subject).is_valid()
        && stream_subjects().iter().any(|pattern| {
            pattern
                .strip_suffix('>')
                .is_some_and(|family| subject.starts_with(family) && subject.len() > family.len())
                || pattern == subject
        })
}

/// How long the stream remembers a `Nats-Msg-Id` and drops a second
/// publish carrying it (backlog 2e45840a). The adapter sends every
/// stream-captured event with its event id as `Nats-Msg-Id`, and the
/// relay re-publishes a row whose ack it never saw — so a publish the
/// stream stored but whose ack was lost in transit comes back within one
/// ack wait plus one relay pass, and this window drops it instead of
/// handing every durable consumer the event twice. Two minutes is the
/// server's own default, which the live stream has had since it was
/// created without one (the config left the field zero, so the server
/// chose); naming it makes it this crate's value, held by
/// [`ensure_stream`], and pinned above [`crate::bus::PUBLISH_ACK_WAIT`].
/// A retry later than this (a bus dark for longer, with the ack lost
/// just before) is stored again; consumers already tolerate that — the
/// standing at-least-once contract — this only makes it rare.
pub const DUPLICATE_WINDOW: Duration = Duration::from_secs(2 * 60);

/// Per-attempt redelivery spacing. Entry `i` is the delay before delivery
/// `i+2` (delivery 1 is the original). The schedule widens so a transient
/// condition has time to clear, and the tail repeats at 30s until
/// [`MAX_DELIVER`] is hit.
///
/// Supplied per-NAK via [`redelivery_delay`] — NEVER configured as the
/// consumer's server-side `backoff`, because a backoff schedule replaces
/// `ack_wait` as the per-attempt ack deadline and silently redelivers
/// slow-but-healthy deliveries (see [`ACK_WAIT`]). An explicit NAK delay
/// paces only genuine failures.
fn redelivery_backoff() -> Vec<Duration> {
    vec![
        Duration::from_secs(1),
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(10),
        Duration::from_secs(20),
        Duration::from_secs(30),
        Duration::from_secs(30),
    ]
}

/// The NAK delay for a failed delivery, by its (1-based) attempt number:
/// the [`redelivery_backoff`] schedule, clamped to its 30s tail.
fn redelivery_delay(delivered: i64) -> Duration {
    let sched = redelivery_backoff();
    let idx = usize::try_from(delivered.max(1) - 1)
        .unwrap_or(0)
        .min(sched.len() - 1);
    sched[idx]
}

/// Errors from the durable layer. Kept stringly-typed so the async-nats
/// error zoo doesn't leak into the public signature.
#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error("jetstream stream error: {0}")]
    Stream(String),
    #[error("jetstream consumer error: {0}")]
    Consumer(String),
}

/// The canonical [`STREAM_NAME`] stream configuration. A bounded,
/// file-backed `Limits`-retention buffer: messages are kept until they age
/// past [`max_age`](stream::Config::max_age) or the byte cap is hit,
/// independent of ACKs, so multiple durable consumers can each read the
/// full set. (`WorkQueue` retention — delete-on-first-ack — would force
/// non-overlapping consumer filters and couple the two dispatcher loops;
/// `Limits` keeps them independent.)
pub fn stream_config() -> stream::Config {
    stream::Config {
        name: STREAM_NAME.to_string(),
        subjects: stream_subjects(),
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        // 3 days wall-clock. Real (wall-paced) ops keep only a small
        // window; a high-warp regen lands a whole sim-year within minutes
        // of wall time, so nothing ages out mid-run — the byte cap bounds
        // that case.
        max_age: Duration::from_secs(3 * 24 * 60 * 60),
        // 4 GiB ceiling (server file store is 8 GiB). Comfortably exceeds a
        // full sim-year of job/step/inventory events.
        max_bytes: 4 * 1024 * 1024 * 1024,
        duplicate_window: DUPLICATE_WINDOW,
        num_replicas: 1,
        ..Default::default()
    }
}

/// Idempotently ensure the [`STREAM_NAME`] stream exists. Safe to call from
/// every connecting service; the first creates it, the rest no-op.
pub async fn ensure_stream(ctx: &Context) -> Result<(), DurableError> {
    let mut stream = ctx
        .get_or_create_stream(stream_config())
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    // get_or_create returns an EXISTING stream verbatim — a deployed
    // box keeps its old subject list forever unless someone updates
    // it. That drift is how a code-side subjects change silently
    // doesn't apply in prod: the consumer filters accept the new
    // topic, the stream never ingests it. Reconcile on every start.
    let info = stream
        .info()
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    let mut have = info.config.subjects.clone();
    let mut want = stream_subjects();
    have.sort();
    want.sort();
    // The duplicate window is reconciled the same way: the adapter's
    // retry-after-a-lost-ack is dropped only inside it (backlog 2e45840a).
    if have != want || info.config.duplicate_window != DUPLICATE_WINDOW {
        ctx.update_stream(stream_config())
            .await
            .map_err(|e| DurableError::Stream(format!("updating stream subjects: {e}")))?;
    }
    Ok(())
}

/// The stream a durable consumer is opened on. When it is
/// [`STREAM_NAME`] and the server says it is not there, it is ensured
/// first ([`ensures_on_open`]).
///
/// Backlog 2e45840a (design ad9519bc part b, the stream half): car 1
/// ensures the stream on every (re)connect, so a stream deleted while
/// the client STAYED connected came back only on the next reconnect.
/// Until then every consumer's reopen met `stream not found` and
/// waited — and every event the relay published reached no stream,
/// because a core publish to a subject no stream captures answers Ok.
/// A consumer's stream ending is what triggers its reopen, so ensuring
/// here bounds that window to one reopen.
pub async fn bound_stream(
    ctx: &Context,
    stream_name: &str,
) -> Result<stream::Stream, DurableError> {
    match ctx.get_stream(stream_name).await {
        Ok(stream) => Ok(stream),
        Err(e) if ensures_on_open(stream_name, &e) => {
            warn!(
                stream = stream_name,
                "stream is gone while the bus stayed connected; ensuring it before the open"
            );
            ensure_stream(ctx).await?;
            ctx.get_stream(stream_name)
                .await
                .map_err(|e| DurableError::Stream(e.to_string()))
        }
        Err(e) => Err(DurableError::Stream(e.to_string())),
    }
}

/// Does this failed `get_stream` mean the stream is ensured before the
/// open? Only when the server
/// itself answered that [`STREAM_NAME`] does not exist: a dark bus
/// (a request that timed out) is waited out, never answered with a
/// create, and a stream whose configuration this crate does not own (a
/// test's throwaway) is never created by an open.
pub fn ensures_on_open(stream_name: &str, error: &GetStreamError) -> bool {
    stream_name == STREAM_NAME
        && matches!(
            error.kind(),
            GetStreamErrorKind::JetStream(e) if e.error_code() == ErrorCode::STREAM_NOT_FOUND
        )
}

/// Delete the stream (dropping its messages **and** durable consumers) and
/// recreate it empty. For dev/regen resets ONLY: a fresh regen resets
/// Postgres, so the delivery buffer must be cleared too or the durable
/// consumers would replay the previous run's events against the new
/// database (every replayed step-completion 404s → fails the health gate).
/// Never call this on a normal production restart — that would discard
/// in-flight events the whole durability layer exists to protect.
pub async fn reset_stream(ctx: &Context) -> Result<(), DurableError> {
    // Best-effort delete; an absent stream is fine (first run).
    let _ = ctx.delete_stream(STREAM_NAME).await;
    ensure_stream(ctx).await
}

/// Drop every message currently buffered on `stream_name`, keeping the
/// stream and its durable consumers in place.
///
/// This is the delivery-buffer half of a demo epoch restart. That
/// restart trims `audit_log` back to the seed baseline, so anything
/// still sitting in the buffer at that moment belongs to the epoch being
/// deleted — redelivering it against the trimmed database 404s through
/// the whole redelivery budget and dead-letters. The dead-letter count
/// is a release gate whose entire value is reading zero, so a restart
/// that reliably manufactures spurious entries erodes the signal. Same
/// reasoning as the `TRUNCATE event_outbox` one hop upstream: at restart
/// time every pending item is the finished epoch's by definition, and
/// must die with it rather than relay into the new one.
///
/// Purge, NOT [`reset_stream`]: reset deletes the stream and with it
/// every durable consumer, forcing a live dispatcher to re-create its
/// consumers mid-flight. A purge leaves the consumers attached; the
/// server advances their floors past the dropped messages.
pub async fn purge_stream(ctx: &Context, stream_name: &str) -> Result<(), DurableError> {
    let stream = ctx
        .get_stream(stream_name)
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    stream
        .purge()
        .await
        .map_err(|e| DurableError::Stream(e.to_string()))?;
    Ok(())
}

/// Durable pull-consumer configuration for the given durable name and
/// filter subjects. Explicit ACK + bounded redelivery is the whole point:
/// without it a failed handler would drop the event.
pub fn consumer_config(durable: &str, filter_subjects: Vec<String>) -> pull::Config {
    pull::Config {
        durable_name: Some(durable.to_string()),
        name: Some(durable.to_string()),
        ack_policy: AckPolicy::Explicit,
        ack_wait: ACK_WAIT,
        max_deliver: MAX_DELIVER,
        filter_subjects,
        // Deliberately NO `backoff` here: the server would use it as the
        // per-attempt ack deadline (overriding ack_wait), so backoff[0]=1s
        // silently duplicated every delivery slower than 1s. The retry
        // schedule rides each NAK instead (see `settle`).
        ..Default::default()
    }
}

/// Open (creating if needed) a durable pull consumer on [`STREAM_NAME`] and
/// return its message stream. Each yielded [`Message`] must be settled with
/// [`settle`] (ACK on success, NAK/dead-letter on failure) or it will be
/// redelivered after `ack_wait`.
///
/// `stream_name` is a parameter (not hardcoded to [`STREAM_NAME`]) only so
/// tests can exercise the mechanics on a throwaway stream; production
/// passes [`STREAM_NAME`].
///
/// The stream it answers ENDS when its consumer is found to have
/// vanished ([`UntilVanished`]), so a caller under [`keep_bound`] opens
/// it again — and this create-or-update recreates the consumer.
pub async fn open_durable(
    ctx: &Context,
    stream_name: &str,
    durable: &str,
    filter_subjects: Vec<String>,
) -> Result<Messages, DurableError> {
    let stream = bound_stream(ctx, stream_name).await?;
    // create-or-UPDATE (not get_or_create): reconciles a persisted
    // consumer whose config drifted from `consumer_config` — durable
    // consumers outlive binary deploys, so without the update an old
    // config (e.g. the server-side `backoff` that used to override
    // ack_wait) would survive every restart. Update preserves the
    // consumer's cursor; only the config changes.
    let consumer = stream
        .create_consumer(consumer_config(durable, filter_subjects))
        .await
        .map_err(|e| DurableError::Consumer(e.to_string()))?;
    consumer
        .messages()
        .await
        .map(until_vanished)
        .map_err(|e| DurableError::Consumer(e.to_string()))
}

/// A bound durable consumer's message stream, as [`open_durable`]
/// answers it: the client's pull stream, ended when its consumer has
/// vanished.
pub type Messages = UntilVanished<pull::Stream>;

/// End `messages` after the first error saying its consumer is gone.
pub fn until_vanished<S>(messages: S) -> UntilVanished<S> {
    UntilVanished {
        inner: messages,
        vanished: false,
    }
}

/// A pull stream that ENDS once the server has said there is no
/// consumer to pull from, instead of asking again forever.
///
/// Backlog 2e45840a (outage 05fd165a, design ad9519bc part b, the
/// NoResponders remainder): async-nats 0.49.1 ends a pull stream only
/// when the server's `Consumer Deleted` status reaches a pull request
/// that is waiting. A consumer that vanished while nothing was waiting
/// — removed while the client was disconnected, or lost with the
/// server's JetStream state — leaves every later pull request with no
/// subscriber on its `CONSUMER.MSG.NEXT` subject, and the client
/// yields `NoResponders` for each one and asks again. Every consumer
/// loop here skips a stream error and keeps reading, so the stream
/// never ended, [`keep_bound`] never reopened it, and the consumer —
/// whose `create_consumer` would have brought it back — stayed gone
/// for the life of the process, logging one warning per pull.
///
/// Only `NoResponders` ends it. A missed heartbeat is the dark-bus
/// case the client already recovers from by itself (it re-asks for
/// the consumer's info on every reconnect), and ending on it would
/// turn each reconnect into a reopen.
pub struct UntilVanished<S> {
    inner: S,
    vanished: bool,
}

impl<T, S> futures::Stream for UntilVanished<S>
where
    S: futures::Stream<Item = Result<T, MessagesError>> + Unpin,
{
    type Item = Result<T, MessagesError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.vanished {
            return std::task::Poll::Ready(None);
        }
        let polled = std::pin::Pin::new(&mut self.inner).poll_next(cx);
        if let std::task::Poll::Ready(Some(Err(e))) = &polled
            && e.kind() == MessagesErrorKind::NoResponders
        {
            // Yield this one error, so the loop still logs why its
            // session ended, and end on the next poll.
            self.vanished = true;
        }
        polled
    }
}

/// The first wait between two attempts to open a durable consumer.
pub const OPEN_RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between two attempts: a bus dark for an hour costs
/// one attempt (and one warning) every half minute, not a tight loop.
pub const OPEN_RETRY_CAP: Duration = Duration::from_secs(30);

/// The wait after the `attempt`-th failed open (1-based): doubling from
/// [`OPEN_RETRY_FIRST`], stopping at [`OPEN_RETRY_CAP`].
pub fn open_retry_delay(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(16);
    OPEN_RETRY_FIRST
        .saturating_mul(1u32 << doublings)
        .min(OPEN_RETRY_CAP)
}

/// Run `op` until it answers, waiting [`open_retry_delay`] between
/// failures and naming `what` in a warning on each one.
///
/// Backlog 2e45840a (outage 05fd165a, design ad9519bc car 2): since car
/// 1, [`crate::NatsEventBus::connect`] hands back a bus that is still
/// reconnecting when NATS is dark, so a ONE-SHOT open after it meets a
/// JetStream request that times out — and each consumer gave up for the
/// life of its process: the dispatcher exited, the escalation router
/// and the assets ingress fell back to at-most-once. A failure to open
/// is a transport condition, so the answer is to wait for the bus.
pub async fn retry_capped<T, E, F, Fut>(what: &str, mut op: F) -> T
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(value) => {
                if attempt > 0 {
                    info!(
                        consumer = what,
                        attempts = attempt + 1,
                        "durable consumer opened after waiting for the bus"
                    );
                }
                return value;
            }
            Err(e) => {
                attempt = attempt.saturating_add(1);
                let wait = open_retry_delay(attempt);
                // The operation's own error is the cause; the message names
                // none of its own. It used to say "bus dark or stream not
                // ensured" for every failure, including a lane consumer
                // fenced as gone, which is neither (backlog 960fabfc).
                warn!(
                    consumer = what,
                    attempt,
                    retry_in_secs = wait.as_secs(),
                    error = %e,
                    "durable consumer not open yet; retrying"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Keep a durable consumer bound for the life of the caller: open it
/// through [`retry_capped`], hand what opened to `run`, and when `run`
/// returns — the consumer's message stream ENDED — open it again.
///
/// Backlog 2e45840a (outage 05fd165a, design ad9519bc car 3): car 2
/// made the first open wait for the bus, but a bound stream that ends
/// (the client ends a pull stream whose consumer the server deleted, as
/// [`reset_stream`] does to a live dispatcher) still returned for good,
/// and the consumer's rules stayed off until a restart or a registry
/// reload. An ended stream is a transport condition like a failed
/// open, so the answer is the same: open it again.
///
/// The wait before a reopen climbs [`open_retry_delay`] while bound
/// sessions keep ending inside [`OPEN_RETRY_CAP`], and starts over after
/// one that lasted longer — so a stream that ends the moment it opens
/// costs one warning every half minute, not a tight loop. It never
/// returns: `Infallible` says so in the type.
pub async fn keep_bound<T, E, Open, OpenFut, Run, RunFut>(
    what: &str,
    mut open: Open,
    mut run: Run,
) -> std::convert::Infallible
where
    E: std::fmt::Display,
    Open: FnMut() -> OpenFut,
    OpenFut: std::future::Future<Output = Result<T, E>>,
    Run: FnMut(T) -> RunFut,
    RunFut: std::future::Future<Output = ()>,
{
    let mut quick_ends: u32 = 0;
    loop {
        let bound = retry_capped(what, &mut open).await;
        let since = tokio::time::Instant::now();
        run(bound).await;
        quick_ends = if since.elapsed() >= OPEN_RETRY_CAP {
            1
        } else {
            quick_ends.saturating_add(1)
        };
        let wait = open_retry_delay(quick_ends);
        warn!(
            consumer = what,
            retry_in_secs = wait.as_secs(),
            "durable consumer's message stream ended; re-opening it"
        );
        tokio::time::sleep(wait).await;
    }
}

/// Pure policy: has this message exhausted its redelivery budget? The
/// server reports `delivered` as a 1-based attempt count, so the
/// [`MAX_DELIVER`]-th delivery is the last.
pub fn is_dead_letter(delivered: i64) -> bool {
    delivered >= MAX_DELIVER
}

/// Settle a delivered message per the reliability policy:
/// - `Ok`  → ACK; the stream advances past it.
/// - `Err` before the budget is spent → NAK; the server redelivers it on
///   the [`redelivery_backoff`] schedule once the transient clears.
/// - `Err` on the final attempt → dead-letter: a loud `error!` (the source
///   event is recoverable from `audit_log`) and `Term` so it stops cycling.
///
/// Generic over the error type so callers can pass their own (`anyhow`,
/// `&str`, …) without this crate depending on it.
/// Handler settlement plus an explicit routing hold. `Retry` NAKs for redelivery (converges
/// once the transient cause clears, dead-letters after the budget);
/// `Permanent` skips the budget entirely: a deterministic data error
/// (a seed typo, a malformed body) fails identically on every
/// redelivery, so the 8 NAKs it used to burn were pure noise and a
/// multi-minute delay before the SAME dead-letter. The log line is the
/// same `DEAD-LETTER:` pattern either way — the health gates and
/// operators see one vocabulary, with `class=permanent` marking the
/// fast path.
pub enum Settle {
    Ack,
    /// Routing is unknown, not a failed handler. The owning runner rechecks
    /// while sending progress; it must not NAK, ACK, or exhaust retry budget.
    Hold(String),
    Retry(String),
    Permanent(String),
}

pub async fn settle_classified(msg: &Message, outcome: Settle) {
    match outcome {
        Settle::Ack => {
            if let Err(e) = msg.ack().await {
                warn!(subject = %msg.subject, error = %e, "JetStream ACK failed; message will redeliver");
            }
        }
        Settle::Permanent(err) => {
            let delivered = msg.info().map(|i| i.delivered).unwrap_or(MAX_DELIVER);
            error!(
                subject = %msg.subject,
                delivered,
                class = "permanent",
                error = %err,
                "DEAD-LETTER: deterministic data error; terminating without redelivery (source recoverable from audit_log)"
            );
            let _ = msg.ack_with(AckKind::Term).await;
        }
        Settle::Hold(reason) => {
            // A caller without a recheck loop still cannot discard unknown work.
            warn!(%reason, "delivery held; caller must recheck authoritative routing");
            loop {
                if let Err(error) = msg.ack_with(AckKind::Progress).await {
                    warn!(%error, "held delivery progress unread; source remains pending");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        Settle::Retry(err) => settle(msg, Err::<(), _>(err)).await,
    }
}

pub async fn settle<E: std::fmt::Display>(msg: &Message, outcome: Result<(), E>) {
    match outcome {
        Ok(()) => {
            if let Err(e) = msg.ack().await {
                warn!(subject = %msg.subject, error = %e, "JetStream ACK failed; message will redeliver");
            }
        }
        Err(err) => {
            // `info()` only fails to parse on a non-JetStream message,
            // which can't reach here; treat an unreadable count as the
            // final attempt so a broken message can't loop forever.
            let delivered = msg.info().map(|i| i.delivered).unwrap_or(MAX_DELIVER);
            if is_dead_letter(delivered) {
                error!(
                    subject = %msg.subject,
                    delivered,
                    max_deliver = MAX_DELIVER,
                    error = %err,
                    "DEAD-LETTER: event exhausted redelivery budget; dropping (source recoverable from audit_log)"
                );
                let _ = msg.ack_with(AckKind::Term).await;
            } else {
                // Pacing rides the NAK itself (a bare NAK redelivers
                // immediately) — the consumer carries no server-side
                // backoff, which would double as the ack deadline.
                let delay = redelivery_delay(delivered);
                warn!(
                    subject = %msg.subject,
                    delivered,
                    retry_in_secs = delay.as_secs(),
                    error = %err,
                    "handler failed; NAK for redelivery"
                );
                let _ = msg.ack_with(AckKind::Nak(Some(delay))).await;
            }
        }
    }
}

/// Collapse a set of rule subjects to the minimal non-overlapping set of
/// first-token wildcards suitable as JetStream consumer `filter_subjects`.
///
/// The rules registry references many precise subjects (`step.done.*`,
/// `step.ready.*`, `inventory.item.consumed`, `jobs.job.closed`). A single
/// pull consumer can't take overlapping filters (the server rejects
/// `step.ready.*` alongside `step.ready.delegate-subjob`), and one consumer
/// per subject would redeliver an event once per matching filter. Coarsening
/// every subject to its `<first-token>.>` wildcard yields a deduplicated,
/// trivially-non-overlapping set (distinct first tokens never overlap); the
/// rules runner's own `match_event` still does the precise matching, and the
/// few extra subjects pulled in (e.g. `jobs.step.*` under `jobs.>`) match no
/// rule and are ACK'd as cheap no-ops. Growth-proof: a new `step.foo.*` rule
/// is already covered by `step.>`.
pub fn coarse_filter_subjects(subjects: &[String]) -> Vec<String> {
    let mut out: Vec<String> = subjects
        .iter()
        .filter_map(|s| s.split('.').next())
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}.>"))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_config_is_a_bounded_file_buffer() {
        let c = stream_config();
        assert_eq!(c.name, "BOSS_EVENTS");
        assert_eq!(
            c.subjects,
            vec![
                "jobs.>",
                "step.>",
                "inventory.>",
                "commerce.>",
                "ledger.>",
                "asset.>",
                "docs.>",
                "events.>"
            ]
        );
        assert_eq!(c.retention, stream::RetentionPolicy::Limits);
        assert_eq!(c.storage, stream::StorageType::File);
        assert!(c.max_bytes > 0, "byte cap must bound the buffer");
        assert!(!c.max_age.is_zero(), "age cap must expire the buffer");
    }

    /// Backlog 2e45840a: the relay re-publishes a row whose ack it did
    /// not see, so the stream must still remember the first publish's
    /// `Nats-Msg-Id` when the retry arrives — at the earliest one ack
    /// wait later. A window shorter than that wait stores every
    /// lost-ack retry twice; zero hands the choice back to the server.
    #[test]
    fn the_duplicate_window_is_ours_and_outlasts_the_ack_wait() {
        assert_eq!(stream_config().duplicate_window, DUPLICATE_WINDOW);
        assert!(
            DUPLICATE_WINDOW >= crate::bus::PUBLISH_ACK_WAIT * 10,
            "a lost-ack retry must land inside the window with room for relay passes"
        );
        assert!(DUPLICATE_WINDOW < stream_config().max_age);
    }

    /// The adapter's route: a captured subject waits for the stream's
    /// ack, anything else does not (there is no stream to give one).
    #[test]
    fn the_stream_captures_its_families_and_nothing_else() {
        for captured in [
            "jobs.job.created",
            "step.done.task",
            "events.outbox.dead_lettered",
            "asset.event.recorded",
        ] {
            assert!(stream_captures(captured), "{captured}");
        }
        for live_only in [
            "jobs",
            "jobsx.created",
            "cybernetics.message.enqueued",
            "actor.login.unresolved",
            "jobs.a b",
            "",
        ] {
            assert!(!stream_captures(live_only), "{live_only:?}");
        }
    }

    #[test]
    fn consumer_config_enables_explicit_ack_and_bounded_redelivery() {
        let c = consumer_config("dispatcher-rules", vec!["step.>".to_string()]);
        assert_eq!(c.durable_name.as_deref(), Some("dispatcher-rules"));
        assert!(matches!(c.ack_policy, AckPolicy::Explicit));
        assert_eq!(c.max_deliver, MAX_DELIVER);
        assert_eq!(c.filter_subjects, vec!["step.>".to_string()]);
        // A server-side backoff schedule REPLACES ack_wait as the
        // per-attempt ack deadline — backoff[0]=1s silently redelivered
        // every event slower than 1s and ran its side effects twice.
        // Retry pacing must ride the NAK (redelivery_delay), never the
        // consumer config.
        assert!(
            c.backoff.is_empty(),
            "consumer backoff overrides ack_wait; pacing belongs on the NAK"
        );
        assert_eq!(
            c.ack_wait, ACK_WAIT,
            "ack_wait is the crash backstop and must survive intact"
        );
    }

    #[test]
    fn redelivery_delay_walks_the_schedule_and_clamps_to_the_tail() {
        assert_eq!(redelivery_delay(1), Duration::from_secs(1));
        assert_eq!(redelivery_delay(2), Duration::from_secs(2));
        assert_eq!(redelivery_delay(3), Duration::from_secs(5));
        // Beyond the schedule (and any nonsense attempt number) → 30s tail.
        assert_eq!(redelivery_delay(7), Duration::from_secs(30));
        assert_eq!(redelivery_delay(100), Duration::from_secs(30));
        assert_eq!(redelivery_delay(0), Duration::from_secs(1));
        assert_eq!(
            redelivery_backoff().len() as i64,
            MAX_DELIVER - 1,
            "one gap per redelivery (deliveries = attempts; gaps = attempts-1)"
        );
    }

    #[test]
    fn dead_letter_only_at_the_budget() {
        assert!(!is_dead_letter(1), "first delivery is never a dead-letter");
        assert!(!is_dead_letter(MAX_DELIVER - 1));
        assert!(is_dead_letter(MAX_DELIVER), "the Nth delivery is the last");
        assert!(is_dead_letter(MAX_DELIVER + 1));
    }

    #[test]
    fn coarse_filters_collapse_to_nonoverlapping_first_tokens() {
        let subjects = vec![
            "step.done.*".to_string(),
            "step.ready.*".to_string(),
            "step.ready.delegate-subjob".to_string(),
            "inventory.item.consumed".to_string(),
            "jobs.job.closed".to_string(),
        ];
        // Sorted, deduped, one wildcard per distinct first token — and the
        // overlapping step.ready.* / step.ready.delegate-subjob collapse to
        // a single `step.>` that the server will accept.
        assert_eq!(
            coarse_filter_subjects(&subjects),
            vec![
                "inventory.>".to_string(),
                "jobs.>".to_string(),
                "step.>".to_string(),
            ]
        );
    }

    #[test]
    fn coarse_filters_handle_wildcards_and_single_tokens() {
        let subjects = vec![
            "jobs.step.>".to_string(),
            "jobs.job.closed".to_string(),
            "lonely".to_string(),
        ];
        assert_eq!(
            coarse_filter_subjects(&subjects),
            vec!["jobs.>".to_string(), "lonely.>".to_string()]
        );
    }

    #[test]
    fn coarse_filters_empty_input_is_empty() {
        assert!(coarse_filter_subjects(&[]).is_empty());
    }

    /// Backlog 2e45840a, design ad9519bc car 2: the wait between two
    /// attempts to open a durable consumer doubles from one second and
    /// stops at thirty, so a bus dark for an hour costs one quiet
    /// attempt every half minute rather than a tight loop.
    #[test]
    fn the_open_retry_delay_doubles_to_its_cap() {
        let walked: Vec<u64> = (1..=8).map(|a| open_retry_delay(a).as_secs()).collect();
        assert_eq!(walked, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(
            open_retry_delay(0),
            OPEN_RETRY_FIRST,
            "attempt 0 reads as the first"
        );
        assert_eq!(
            open_retry_delay(u32::MAX),
            OPEN_RETRY_CAP,
            "no overflow at the tail"
        );
    }

    /// The retry hands back what the operation finally answered, after
    /// as many attempts as it took — a failure is a wait, never the end.
    #[tokio::test]
    async fn retry_capped_keeps_trying_until_the_operation_answers() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let got = retry_capped("test-consumer", || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(format!("dark, attempt {n}"))
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(got, 42);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    /// Longer than one request on a dark bus takes to give up: the
    /// async-nats client's request timeout, ten seconds by default
    /// (`ConnectOptions`), not the JetStream context's five — a seven
    /// second window was measured to elapse before the one-shot open
    /// answered, which would have let the test below pass on old code.
    const DARK_WINDOW: Duration = Duration::from_secs(15);

    /// The defect itself. Car 1 made a first connect to a dark bus
    /// return a bus that keeps reconnecting, so every one-shot open
    /// that followed it — the dispatcher's two consumers, the
    /// escalation router's, the assets ingress's — met a request that
    /// timed out and gave up: the dispatcher exited, the other two fell
    /// back to at-most-once for the life of the process. Keeping it
    /// bound through [`keep_bound`] — the one door every durable
    /// consumer opens through since the dispatcher's assignment consumer
    /// moved onto it — WAITS for the bus instead: past a request timeout
    /// it is still opening, and nothing has run on a consumer.
    #[tokio::test]
    async fn keeping_a_durable_consumer_bound_on_a_dark_bus_waits_rather_than_failing() {
        let dark = boss_testing::dark_port();
        let bus = crate::NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a connect error (car 1)");
        let js = bus.jetstream();
        let ran = std::sync::atomic::AtomicBool::new(false);
        let kept = tokio::time::timeout(
            DARK_WINDOW,
            keep_bound(
                "dark-bus-test",
                || open_durable(&js, STREAM_NAME, "dark-bus-test", vec!["jobs.>".into()]),
                |_messages| {
                    ran.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::future::ready(())
                },
            ),
        )
        .await;
        assert!(
            kept.is_err(),
            "nothing listens at {}; the consumer must still be opening, not answered",
            dark.addr
        );
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "a dark bus opened no consumer"
        );
    }

    /// The control for the test above: inside the same window the
    /// ONE-SHOT open on a dark bus answers an error. Without this,
    /// "still pending" could mean only that a dark request hangs past
    /// the window, and the test above would pass on the old code too.
    #[tokio::test]
    async fn a_one_shot_open_on_a_dark_bus_gives_up_inside_the_window() {
        let dark = boss_testing::dark_port();
        let bus = crate::NatsEventBus::connect(&format!("nats://{}", dark.addr))
            .await
            .expect("a dark bus is not a connect error (car 1)");
        let js = bus.jetstream();
        let opened = tokio::time::timeout(
            DARK_WINDOW,
            open_durable(&js, STREAM_NAME, "dark-bus-test", vec!["jobs.>".into()]),
        )
        .await
        .expect("a one-shot open answers inside the window");
        assert!(opened.is_err(), "a dark bus cannot open a consumer");
    }

    /// Drive [`keep_bound`] on a paused clock for `window`, with a `run`
    /// that holds each bound session for `session`, and answer the
    /// instants (seconds from the start) at which it opened.
    async fn opens_within(window: Duration, session: Duration) -> Vec<u64> {
        let start = tokio::time::Instant::now();
        let opened = std::sync::Mutex::new(Vec::new());
        let ran = std::sync::Mutex::new(Vec::new());
        let kept = tokio::time::timeout(
            window,
            keep_bound(
                "test-consumer",
                || {
                    let mut opened = opened.lock().unwrap();
                    opened.push(start.elapsed().as_secs());
                    let n = opened.len();
                    async move { Ok::<_, String>(n) }
                },
                |n| {
                    ran.lock().unwrap().push(n);
                    tokio::time::sleep(session)
                },
            ),
        )
        .await;
        assert!(kept.is_err(), "keep_bound never answers");
        let opened = opened.into_inner().unwrap();
        let ran = ran.into_inner().unwrap();
        assert_eq!(
            ran,
            (1..=opened.len()).collect::<Vec<_>>(),
            "every session runs on what its own open answered"
        );
        opened
    }

    /// Backlog 2e45840a, design ad9519bc car 3, the defect itself: a
    /// bound stream that ENDS is opened again, rather than its consumer
    /// staying off for the life of the process. A stream that ends the
    /// moment it opens is reopened on the open-retry ladder, so it costs
    /// one attempt every half minute at worst, never a tight loop.
    #[tokio::test(start_paused = true)]
    async fn a_consumer_whose_stream_ends_is_opened_again_on_the_ladder() {
        let opens = opens_within(Duration::from_secs(100), Duration::ZERO).await;
        assert_eq!(opens, vec![0, 1, 3, 7, 15, 31, 61, 91]);
    }

    /// A session that stayed bound longer than the cap starts the ladder
    /// over: one stream ending after an hour's work waits one second,
    /// not the thirty a run of instant ends has climbed to.
    #[tokio::test(start_paused = true)]
    async fn a_session_that_lasted_starts_the_reopen_ladder_over() {
        let opens = opens_within(Duration::from_secs(200), Duration::from_secs(60)).await;
        assert_eq!(opens, vec![0, 61, 122, 183]);
    }

    /// What a pull stream yields, as kinds: a message's number, or the
    /// kind of the error the client reported in its place.
    async fn kinds<S>(messages: S) -> Vec<Result<u32, MessagesErrorKind>>
    where
        S: futures::Stream<Item = Result<u32, MessagesError>>,
    {
        use futures::StreamExt;
        messages.map(|m| m.map_err(|e| e.kind())).collect().await
    }

    /// Backlog 2e45840a (design ad9519bc part b, the NoResponders
    /// remainder), the defect itself: the client answers a pull stream
    /// whose consumer vanished with `NoResponders` on every pull, and
    /// never ends it, so `keep_bound` never opened — and recreated —
    /// the consumer. Wrapped, the stream yields that error once (the
    /// consumer loop still logs it) and then ENDS. `take` bounds the
    /// unwrapped behaviour, which would otherwise never finish.
    #[tokio::test]
    async fn a_stream_whose_consumer_vanished_ends_after_its_first_no_responders() {
        use futures::StreamExt;
        let forever = futures::stream::iter([Ok(1)]).chain(futures::stream::repeat_with(|| {
            Err(MessagesError::new(MessagesErrorKind::NoResponders))
        }));
        assert_eq!(
            kinds(until_vanished(forever).take(100)).await,
            vec![Ok(1), Err(MessagesErrorKind::NoResponders)],
            "one NoResponders is reported, and then the stream ends"
        );
    }

    /// The control: every other error passes through without ending the
    /// stream. A missed heartbeat is a dark bus the client recovers from
    /// by itself, and a failed pull is retried by it; neither says the
    /// consumer is gone.
    #[tokio::test]
    async fn other_stream_errors_pass_through_and_the_stream_goes_on() {
        let mixed = futures::stream::iter([
            Ok(1),
            Err(MessagesError::new(MessagesErrorKind::MissingHeartbeat)),
            Ok(2),
            Err(MessagesError::new(MessagesErrorKind::Pull)),
            Ok(3),
            Err(MessagesError::new(MessagesErrorKind::Other)),
            Ok(4),
        ]);
        assert_eq!(
            kinds(until_vanished(mixed)).await,
            vec![
                Ok(1),
                Err(MessagesErrorKind::MissingHeartbeat),
                Ok(2),
                Err(MessagesErrorKind::Pull),
                Ok(3),
                Err(MessagesErrorKind::Other),
                Ok(4),
            ]
        );
    }

    /// The server's own answer for a stream that is not there, as the
    /// client parses it off the wire.
    fn server_error(code: u64) -> async_nats::jetstream::Error {
        serde_json::from_value(serde_json::json!({
            "code": 404,
            "err_code": code,
            "description": "from the server",
        }))
        .unwrap()
    }

    /// Backlog 2e45840a, design ad9519bc part (b), the stream half: a
    /// stream deleted while the client stayed connected was ensured
    /// again only on the next reconnect, so every consumer's reopen met
    /// `stream not found` and waited — and every event the relay
    /// published meanwhile reached no stream. An open now ensures
    /// [`STREAM_NAME`] when the server says it is not there.
    #[test]
    fn an_open_ensures_the_events_stream_when_the_server_says_it_is_gone() {
        let gone = GetStreamError::new(GetStreamErrorKind::JetStream(server_error(
            async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND.0,
        )));
        assert!(ensures_on_open(STREAM_NAME, &gone));
    }

    /// The controls. A dark bus is waited out, never answered with a
    /// create; another server refusal is not an absence; and a stream
    /// whose configuration this crate does not own (a test's throwaway)
    /// is never created by an open.
    #[test]
    fn only_a_missing_events_stream_is_ensured_on_open() {
        let gone = GetStreamError::new(GetStreamErrorKind::JetStream(server_error(
            async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND.0,
        )));
        assert!(!ensures_on_open("test_throwaway", &gone));
        let dark = GetStreamError::new(GetStreamErrorKind::Request);
        assert!(!ensures_on_open(STREAM_NAME, &dark));
        let unavailable = GetStreamError::new(GetStreamErrorKind::JetStream(server_error(
            async_nats::jetstream::ErrorCode::JETSTREAM_NOT_ENABLED.0,
        )));
        assert!(!ensures_on_open(STREAM_NAME, &unavailable));
    }
}
