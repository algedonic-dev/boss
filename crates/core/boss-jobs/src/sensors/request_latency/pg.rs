//! The private sensor aggregate, independent of every command transaction.
use super::{Hour, Series, hour_of};
use boss_core::request_latency::{Class, Recorder, RecorderError, SLOW_REQUEST, Sample};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// The slowest moments one UTC hour keeps (d9b39685).
const SLOW_PER_HOUR: u8 = 32;
/// A moment larger than this is kept as unavailable, never refused.
const MOMENT_BYTES: usize = 16_384;

pub struct PgLatency {
    pool: PgPool,
    now: std::sync::Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// The last attempt is known uncommitted and may succeed later: set
    /// once the batch passed its own checks, cleared before COMMIT is sent
    /// and on any refusal of the data itself. One worker calls `record`.
    retryable: AtomicBool,
}

impl PgLatency {
    pub fn new(pool: PgPool) -> Self {
        Self::with_clock(pool, std::sync::Arc::new(boss_clock_client::wall_now))
    }
    pub fn with_clock(
        pool: PgPool,
        now: std::sync::Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    ) -> Self {
        Self {
            pool,
            now,
            retryable: AtomicBool::new(false),
        }
    }
    /// A refusal of the stored or offered data: a retry would fail the same way.
    fn data(&self, error: RecorderError) -> RecorderError {
        self.retryable.store(false, Ordering::Release);
        error
    }
    pub async fn read(&self, now: DateTime<Utc>) -> Result<serde_json::Value, RecorderError> {
        <Self as Series>::read(self, now).await
    }
    pub async fn read_slow(
        &self,
        now: DateTime<Utc>,
        hours: u8,
    ) -> Result<serde_json::Value, RecorderError> {
        <Self as Series>::read_slow(self, now, hours).await
    }
}

fn storage(error: impl std::fmt::Display) -> RecorderError {
    RecorderError(error.to_string())
}

#[async_trait::async_trait]
impl Recorder for PgLatency {
    fn recording_now(&self) -> Option<DateTime<Utc>> {
        Some((self.now)())
    }
    fn last_attempt_retryable(&self) -> bool {
        self.retryable.load(Ordering::Acquire)
    }
    async fn record(&self, samples: &[Sample]) -> Result<(), RecorderError> {
        self.retryable.store(false, Ordering::Release);
        if samples.is_empty() || samples.len() > 64 {
            return Err(RecorderError(
                "one bounded batch contains1..64 observations".into(),
            ));
        }
        for sample in samples {
            if sample.build.len() > 128
                || sample.recorder_epoch.len() > 128
                || sample.service.len() > 128
                || sample.method.len() > 16
                || sample.route_template.len() > 512
                || sample.completed_at < sample.recording_started_at
                || sample.recording_ended_at.is_some_and(|ended| {
                    ended != sample.completed_at
                        || sample.class != Class::Recording
                        || sample.duration_ns != 0
                        || sample.status != 0
                })
            {
                return Err(RecorderError("recorder identity exceeds its bound".into()));
            }
        }
        // From here until COMMIT is sent, a storage refusal or a dropped
        // attempt committed nothing, so the worker may retain and retry the
        // batch (d9b39685: 37 of 51 refusals in one hour were statement
        // timeouts here, during the stalls the series exists to record).
        self.retryable.store(true, Ordering::Release);
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // The caller's ONE worker has a1s total attempt bound. These local
        // bounds never change a request/command's pool or transaction settings.
        sqlx::query("SET LOCAL statement_timeout = '500ms'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("SET LOCAL lock_timeout = '500ms'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("SET LOCAL synchronous_commit = off")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // Serialize the bounded-hour telemetry aggregate, not command rows.
        // Future legacy evidence is conserved even outside the read window.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('sensor_request_latency_hours',0))",
        )
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        // One database clock is shared by both backend writers. Capture it
        // after acquiring ownership, inside this independent transaction.
        let (writer_now, anchor): (DateTime<Utc>, i64) = sqlx::query_as("SELECT statement_timestamp(),EXTRACT(EPOCH FROM date_trunc('hour',statement_timestamp(),'UTC'))::bigint")
            .fetch_one(&mut *tx).await.map_err(storage)?;
        let latest = writer_now
            .checked_add_signed(chrono::Duration::minutes(5))
            .ok_or_else(|| self.data(RecorderError("database clock overflow".into())))?;
        let cutoff = anchor
            .checked_sub(71 * 3600)
            .ok_or_else(|| self.data(RecorderError("hour cutoff overflow".into())))?;
        let stored_future: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sensor_request_latency_hours WHERE hour>$1)",
        )
        .bind(hour_of(latest))
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let mut grouped: BTreeMap<i64, Vec<&Sample>> = BTreeMap::new();
        let mut slow: Vec<&Sample> = Vec::new();
        let mut refused = 0_u64;
        let mut dropped = 0_u64;
        let mut gap_since = writer_now;
        for sample in samples {
            if stored_future
                || sample.completed_at > latest
                || hour_of(sample.completed_at) < cutoff
            {
                refused = refused
                    .checked_add(sample.recorder_failures)
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| self.data(super::overflow()))?;
                dropped = dropped
                    .checked_add(sample.dropped_samples)
                    .and_then(|n| n.checked_add(u64::from(sample.class != Class::Recording)))
                    .ok_or_else(|| self.data(super::overflow()))?;
                if let Some(gap) = sample.recording_gap_since {
                    gap_since = gap_since.min(gap);
                }
                continue;
            }
            let hour = hour_of(sample.completed_at);
            grouped.entry(hour).or_default().push(sample);
            if sample.class != Class::Recording
                && u128::from(sample.duration_ns) > SLOW_REQUEST.as_nanos()
            {
                slow.push(sample);
            }
        }
        // A future stored hour is a database-wide recording anomaly: refuse
        // this batch's samples without erasing that evidence. Otherwise reject
        // only out-of-retention or future-stamped members. Ok acknowledges debt persistence, not
        // acceptance of those samples. Debt belongs to the actual
        // database recovery hour, not the rejected producer's future hour.
        if refused > 0 {
            grouped.entry(anchor).or_default();
        }
        for (hour, samples) in grouped {
            let payload: Option<serde_json::Value> = sqlx::query_scalar(
                "SELECT payload FROM sensor_request_latency_hours WHERE hour=$1",
            )
            .bind(hour)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            let mut aggregate: Hour = match payload {
                Some(value) => serde_json::from_value(value).map_err(|e| self.data(storage(e)))?,
                None => Hour::default(),
            };
            aggregate.validate().map_err(|e| self.data(e))?;
            if hour == anchor && refused > 0 {
                aggregate.recorder_failures = aggregate
                    .recorder_failures
                    .checked_add(refused)
                    .ok_or_else(|| self.data(super::overflow()))?;
                aggregate.dropped_samples = aggregate
                    .dropped_samples
                    .checked_add(dropped)
                    .ok_or_else(|| self.data(super::overflow()))?;
                aggregate.recording_gap_since = Some(
                    aggregate
                        .recording_gap_since
                        .map_or(gap_since, |old| old.min(gap_since)),
                );
            }
            for sample in samples {
                aggregate.add(sample).map_err(|e| self.data(e))?;
            }
            aggregate.validate().map_err(|e| self.data(e))?;
            let payload = serde_json::to_value(aggregate).map_err(|e| self.data(storage(e)))?;
            sqlx::query("INSERT INTO sensor_request_latency_hours(hour,payload) VALUES($1,$2) ON CONFLICT(hour) DO UPDATE SET payload=EXCLUDED.payload")
                .bind(hour).bind(payload).execute(&mut *tx).await.map_err(storage)?;
        }
        // A request past the 2 s line keeps the moment it stalled in; each
        // hour keeps its slowest 32. Batches with no slow sample pay nothing
        // here, so the cost lands only in the hours the series is about.
        if !slow.is_empty() {
            let mut hours = BTreeSet::new();
            for sample in slow {
                let duration_ns =
                    i64::try_from(sample.duration_ns).map_err(|e| self.data(storage(e)))?;
                sqlx::query("INSERT INTO sensor_request_latency_slow(hour,duration_ns,sample) VALUES($1,$2,$3)")
                    .bind(hour_of(sample.completed_at))
                    .bind(duration_ns)
                    .bind(moment_row(sample))
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
                hours.insert(hour_of(sample.completed_at));
            }
            for hour in hours {
                sqlx::query("DELETE FROM sensor_request_latency_slow WHERE id IN (SELECT id FROM sensor_request_latency_slow WHERE hour=$1 ORDER BY duration_ns DESC, id OFFSET $2)")
                    .bind(hour)
                    .bind(i64::from(SLOW_PER_HOUR))
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
            }
            sqlx::query("DELETE FROM sensor_request_latency_slow WHERE hour < $1")
                .bind(cutoff)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        sqlx::query("DELETE FROM sensor_request_latency_hours WHERE hour < $1")
            .bind(cutoff)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // Once COMMIT is sent the outcome may be committed even if its
        // acknowledgement is lost; a retry then could count twice.
        self.retryable.store(false, Ordering::Release);
        tx.commit().await.map_err(storage)
    }
}

/// One retained slow sample: its origin, its duration and its moment.
fn moment_row(sample: &Sample) -> serde_json::Value {
    let context = match &sample.context {
        None => serde_json::json!({"state": "not observed",
            "reason": "no slow-moment observer was wired, or the request completed before the timer fired"}),
        Some(context) => {
            let bytes = serde_json::to_vec(context).map_or(usize::MAX, |b| b.len());
            if bytes > MOMENT_BYTES {
                serde_json::json!({"state": "unavailable",
                    "reason": "slow-moment context exceeded its byte bound", "bytes": bytes,
                    "bound_bytes": MOMENT_BYTES})
            } else {
                context.clone()
            }
        }
    };
    serde_json::json!({
        "service": sample.service, "class": sample.class, "build": sample.build,
        "recorder_epoch": sample.recorder_epoch, "completed_at": sample.completed_at,
        "duration_ns": sample.duration_ns, "status": sample.status,
        "method": sample.method, "route_template": sample.route_template,
        "context": context,
    })
}

#[async_trait::async_trait]
impl Series for PgLatency {
    async fn read(&self, now: DateTime<Utc>) -> Result<serde_json::Value, RecorderError> {
        self.read_hours(now, 24).await
    }
    async fn read_hours(
        &self,
        now: DateTime<Utc>,
        requested_hours: u8,
    ) -> Result<serde_json::Value, RecorderError> {
        if !(1..=72).contains(&requested_hours) {
            return Err(RecorderError("hours must be1..72".into()));
        }
        let until = hour_of(now);
        let since = until
            .checked_sub((i64::from(requested_hours) - 1) * 3600)
            .ok_or_else(|| RecorderError("hour cutoff overflow".into()))?;
        let rows = sqlx::query("SELECT hour,payload FROM sensor_request_latency_hours WHERE hour >= $1 AND hour <= $2 ORDER BY hour LIMIT $3")
            .bind(since).bind(until).bind(i64::from(requested_hours)+1).fetch_all(&self.pool).await.map_err(storage)?;
        if rows.len() > usize::from(requested_hours) {
            return Err(RecorderError("hourly roster exceeds72 rows".into()));
        }
        let mut held = BTreeMap::new();
        let mut ends = BTreeMap::new();
        let mut identities = BTreeMap::new();
        for row in rows {
            let hour: i64 = row.try_get("hour").map_err(storage)?;
            let payload: serde_json::Value = row.try_get("payload").map_err(storage)?;
            let data: Hour = serde_json::from_value(payload).map_err(storage)?;
            data.validate()?;
            for (epoch, source) in &data.sources {
                let identity = (
                    source.service.clone(),
                    source.build.clone(),
                    source.recording_started_at,
                );
                if identities
                    .insert(epoch.clone(), identity.clone())
                    .is_some_and(|old| old != identity)
                {
                    return Err(RecorderError(
                        "persisted epoch identity changed across hours".into(),
                    ));
                }
                if let Some(ended) = source.recording_ended_at
                    && ends
                        .insert(epoch.clone(), ended)
                        .is_some_and(|old| old != ended)
                {
                    return Err(RecorderError("persisted epoch has conflicting ends".into()));
                }
            }
            held.insert(hour, data);
        }
        let hours: Vec<_> = (0..requested_hours)
            .map(|offset| {
                let hour = since + i64::from(offset) * 3600;
                match held.get(&hour) {
                    Some(data) => data.snapshot(hour, true),
                    None => Hour::default().snapshot(hour, false),
                }
            })
            .collect();
        let window_since = since;
        let mut writes = super::Metrics::default();
        let mut small_reads = super::Metrics::default();
        let mut bulk = super::Metrics::default();
        for (_, data) in held.range(window_since..=until) {
            writes.merge(&data.writes)?;
            small_reads.merge(&data.small_reads)?;
            bulk.merge(&data.bulk)?;
        }
        Ok(
            serde_json::json!({"measure":"backend handler to response headers; excludes independent recorder/network/body time",
            "units":"nanoseconds", "retained_hours":72, "as_of":now,
            "hour_assignment":"UTC hour of backend response decision/completion, not request start",
            "p99_method":"nearest rank of mergeable binary upper-bound buckets (<0.4% width; exact below512ns), capped by exact maximum",
            "coverage":"missing hours/no samples are UNKNOWN; checkpoints prove recorder health only, never complete HTTP coverage; pending failure counts lost at restart cannot be reconstructed",
            "recorder_failure_unit":"failed persistence attempts plus individually refused samples", "dropped_request_sample_unit":"timings refused by queue/representation, by a failed persistence that may have committed, or retained after a refusal before COMMIT and aged out unrecorded after 5 minutes; a refusal before COMMIT is retried, not dropped; commit-ack loss remains uncertain",
            "slow_moments":"?slow=true reads each retained request over 2 s with what the store was doing while it ran",
            "failure_hour":"failure/drop debt is stored in the successful recovery/checkpoint hour; recording_gap_since preserves the earliest known gap, lost request hours are not reconstructed",
            "recording_epoch_ends":ends,
            "window":{"hour_count":requested_hours,"since_hour_epoch_seconds":window_since,"until_hour_exclusive_epoch_seconds":until+3600,
                "as_of":now,"meaning":"requested UTC-hour buckets, current hour partial; not proof of24h continuous coverage",
                "writes":writes.snapshot(),"small_reads":small_reads.snapshot(),"bulk":bulk.snapshot()},
            "retention_anchor":"database statement_timestamp UTC hour; never a pod clock or stored/producer maximum; future stored hours are never deleted",
            "durability":"telemetry transaction synchronous_commit=off; latest committed telemetry may be lost on crash; never an audit-log guarantee",
            "bulk_history":"bulk is separated only by upgraded producers; older write aggregates may include bulk requests and are not reclassified",
            "cancelled_duration":"elapsed until the handler future was dropped; a lower bound with cancelled status, not completed response headers",
            "epoch_end_meaning":"persisted only after successful HTTP admission close/join and owned recorder drain; missing end is active or unclean/unknown, never a claimed graceful close; historical gaps remain",
            "hours":hours}),
        )
    }
    async fn read_slow(
        &self,
        now: DateTime<Utc>,
        requested_hours: u8,
    ) -> Result<serde_json::Value, RecorderError> {
        if !(1..=72).contains(&requested_hours) {
            return Err(RecorderError("hours must be1..72".into()));
        }
        let until = hour_of(now);
        let since = until
            .checked_sub((i64::from(requested_hours) - 1) * 3600)
            .ok_or_else(|| RecorderError("hour cutoff overflow".into()))?;
        let bound = i64::from(requested_hours) * i64::from(SLOW_PER_HOUR);
        let rows = sqlx::query("SELECT hour,sample FROM sensor_request_latency_slow WHERE hour >= $1 AND hour <= $2 ORDER BY hour, duration_ns DESC, id LIMIT $3")
            .bind(since).bind(until).bind(bound + 1).fetch_all(&self.pool).await.map_err(storage)?;
        if rows.len() > usize::from(requested_hours) * usize::from(SLOW_PER_HOUR) {
            return Err(RecorderError(
                "slow moment roster exceeds its hourly bound".into(),
            ));
        }
        let mut moments = Vec::with_capacity(rows.len());
        for row in rows {
            let hour: i64 = row.try_get("hour").map_err(storage)?;
            let mut sample: serde_json::Value = row.try_get("sample").map_err(storage)?;
            let Some(fields) = sample.as_object_mut() else {
                return Err(RecorderError(
                    "persisted slow moment is not an object".into(),
                ));
            };
            fields.insert("hour_epoch_seconds".into(), hour.into());
            moments.push(sample);
        }
        Ok(serde_json::json!({
            "measure": "requests whose backend handler-to-response-headers time exceeded the 2 s line, each with what the store was doing while it was still running",
            "threshold_ns": SLOW_REQUEST.as_nanos(), "retained_per_hour": SLOW_PER_HOUR,
            "retention": "the slowest per UTC hour, 72 hours; the hour's distribution in the series counts every request",
            "moment": "context.observation is read once per burst at the 2 s mark (shared marks a reused one); missing or unavailable is UNKNOWN, never idle",
            "as_of": now,
            "window": {"hour_count": requested_hours, "since_hour_epoch_seconds": since,
                "until_hour_exclusive_epoch_seconds": until + 3600},
            "rows": moments,
        }))
    }
}
