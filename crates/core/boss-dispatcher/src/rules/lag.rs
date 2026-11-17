//! Read-only JetStream lag sampling. No delivery, ACK or cursor mutation.
use serde_json::{Value, json};

pub const LAG_SAMPLE_MAX_AGE_SECONDS: i64 = 30;
pub const LAG_PENDING_THRESHOLD: u64 = 1000;
pub const LAG_AGE_THRESHOLD_SECONDS: i64 = 300;

#[derive(Clone, Debug)]
pub struct ConsumerLag {
    pub observed_at_unix: i64,
    pub pending: u64,
    pub ack_pending: usize,
    pub num_redelivered: usize,
    pub ack_floor_stream_sequence: u64,
    pub oldest_unacked_sequence: Option<u64>,
    pub oldest_unacked_published_unix: Option<i64>,
    pub age_unavailable: Option<String>,
}

impl ConsumerLag {
    fn from_info(info: &async_nats::jetstream::consumer::Info, observed_at_unix: i64) -> Self {
        Self {
            observed_at_unix,
            pending: info.num_pending,
            ack_pending: info.num_ack_pending,
            num_redelivered: info.num_redelivered,
            ack_floor_stream_sequence: info.ack_floor.stream_sequence,
            oldest_unacked_sequence: None,
            oldest_unacked_published_unix: None,
            age_unavailable: None,
        }
    }

    pub fn snapshot(&self, now: i64) -> Value {
        let age = self
            .oldest_unacked_published_unix
            .and_then(|at| now.checked_sub(at))
            .filter(|n| *n >= 0);
        let fresh = now
            .checked_sub(self.observed_at_unix)
            .is_some_and(|age| (0..=LAG_SAMPLE_MAX_AGE_SECONDS).contains(&age));
        json!({
            "status": if fresh { "observed" } else { "stale" },
            "source": "jetstream-consumer", "observed_at_unix": self.observed_at_unix,
            "pending": self.pending, "ack_pending": self.ack_pending,
            "num_redelivered": self.num_redelivered,
            "ack_floor_stream_sequence": self.ack_floor_stream_sequence,
            "oldest_unacked_sequence": self.oldest_unacked_sequence,
            "oldest_unacked_published_unix": self.oldest_unacked_published_unix,
            "oldest_unacked_age_seconds": age,
            "age_unavailable": self.age_unavailable,
            "pending_threshold": LAG_PENDING_THRESHOLD,
            "age_threshold_seconds": LAG_AGE_THRESHOLD_SECONDS,
        })
    }
}

/// Fixed two-second budget around all reads. Subject-filtered message
/// GET reads the first retained message beyond the ACK floor, not a
/// consumer fetch. A moving floor invalidates the age, not the counts.
pub async fn sample(js: &async_nats::jetstream::Context) -> Result<ConsumerLag, String> {
    sample_consumer(js, "dispatcher-rules").await
}

pub async fn sample_consumer(
    js: &async_nats::jetstream::Context,
    consumer: &str,
) -> Result<ConsumerLag, String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        samples(|| sample_once(js, consumer)),
    )
    .await
    .map_err(|_| "consumer lag read exceeded two-second budget".to_string())?
}

enum Attempt {
    Settled(ConsumerLag),
    Resample(ConsumerLag),
}

async fn samples<F, Fut>(mut read: F) -> Result<ConsumerLag, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Attempt, String>>,
{
    for attempt in 0..3 {
        match read().await? {
            Attempt::Settled(value) => return Ok(value),
            Attempt::Resample(mut value) if attempt == 2 => {
                value.age_unavailable =
                    Some("ACK floor continued moving across three reads".into());
                return Ok(value);
            }
            Attempt::Resample(_) => {}
        }
    }
    Err("consumer lag attempts exhausted".into())
}

async fn sample_once(
    js: &async_nats::jetstream::Context,
    consumer: &str,
) -> Result<Attempt, String> {
    let stream = js
        .get_stream(boss_nats::durable::STREAM_NAME)
        .await
        .map_err(|_| "stream metadata unavailable")?;
    let info = stream
        .consumer_info(consumer)
        .await
        .map_err(|_| "rules consumer metadata unavailable")?;
    if info.stream_name != boss_nats::durable::STREAM_NAME || info.name != consumer {
        return Err("rules consumer identity mismatch".to_string());
    }
    let mut result = ConsumerLag::from_info(&info, boss_clock_client::wall_now().timestamp());
    if info.num_pending == 0 && info.num_ack_pending == 0 {
        return Ok(Attempt::Settled(result));
    }
    let mut filters = info.config.filter_subjects.clone();
    if filters.is_empty() && !info.config.filter_subject.is_empty() {
        filters.push(info.config.filter_subject.clone());
    }
    if filters.is_empty() || filters.len() > 16 {
        result.age_unavailable =
            Some("consumer filters unavailable or outside bounded reader".into());
        return Ok(Attempt::Settled(result));
    }
    let Some(next) = info.ack_floor.stream_sequence.checked_add(1) else {
        return Err("ACK floor sequence overflow".into());
    };
    if stream.cached_info().state.first_sequence > next {
        result.age_unavailable = Some("ACK floor precedes retained stream history".into());
        return Ok(Attempt::Settled(result));
    }
    let mut oldest = None;
    for filter in filters {
        match stream
            .raw_message_builder()
            .sequence(next)
            .next_by_subject(filter)
            .send()
            .await
        {
            Ok(message) => {
                if message.sequence < next {
                    return Err("message sequence precedes ACK floor".into());
                }
                if oldest
                    .as_ref()
                    .is_none_or(|(seq, _)| message.sequence < *seq)
                {
                    oldest = Some((message.sequence, message.time.unix_timestamp()));
                }
            }
            Err(error)
                if error.kind()
                    == async_nats::jetstream::stream::RawMessageErrorKind::NoMessageFound => {}
            Err(_) => {
                result.age_unavailable = Some("retained message metadata unavailable".into());
                return Ok(Attempt::Settled(result));
            }
        }
    }
    let after = stream
        .consumer_info(consumer)
        .await
        .map_err(|_| "consumer recheck unavailable")?;
    if after.created != info.created || after.config != info.config {
        return Err("consumer identity or configuration changed during observation".into());
    }
    result.pending = after.num_pending;
    result.ack_pending = after.num_ack_pending;
    result.num_redelivered = after.num_redelivered;
    Ok(finish_sample(
        result,
        info.ack_floor.stream_sequence,
        after.ack_floor.stream_sequence,
        oldest,
    ))
}

fn finish_sample(
    mut result: ConsumerLag,
    before: u64,
    after: u64,
    oldest: Option<(u64, i64)>,
) -> Attempt {
    result.ack_floor_stream_sequence = after;
    if after != before && oldest.is_none_or(|(sequence, _)| sequence <= after) {
        result.oldest_unacked_sequence = None;
        result.oldest_unacked_published_unix = None;
        result.age_unavailable = Some("ACK floor moved beyond the sampled message".into());
        return Attempt::Resample(result);
    }
    if let Some((sequence, published)) = oldest {
        result.oldest_unacked_sequence = Some(sequence);
        result.oldest_unacked_published_unix = Some(published);
    } else {
        result.age_unavailable = Some("no retained matching message beyond ACK floor".into());
    }
    Attempt::Settled(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unresolved() -> ConsumerLag {
        let mut value = reading();
        value.oldest_unacked_sequence = None;
        value.oldest_unacked_published_unix = None;
        value
    }

    #[test]
    fn the_native_consumer_counter_reaches_the_public_reading() {
        let info: async_nats::jetstream::consumer::Info = serde_json::from_value(json!({
            "stream_name":"BOSS_EVENTS","name":"dispatcher-rules","created":"2026-10-31T09:00:00Z",
            "config":boss_nats::durable::consumer_config("dispatcher-rules",vec!["jobs.>".into()]),
            "delivered":{"consumer_seq":40,"stream_seq":100},"ack_floor":{"consumer_seq":30,"stream_seq":90},
            "num_pending":1234,"num_ack_pending":12,"num_redelivered":267,"num_waiting":0
        })).unwrap();
        let value = ConsumerLag::from_info(&info, 1000).snapshot(1000);
        assert_eq!(value["pending"], 1234);
        assert_eq!(value["ack_pending"], 12);
        assert_eq!(value["num_redelivered"], 267);
    }

    #[tokio::test]
    async fn continuous_progress_stays_unknown_after_three_attempts_without_extending_the_budget() {
        let attempts = std::cell::Cell::new(0);
        let value = samples(|| {
            attempts.set(attempts.get() + 1);
            std::future::ready(Ok(finish_sample(unresolved(), 90, 91, Some((91, 100)))))
        })
        .await
        .unwrap();
        assert_eq!(attempts.get(), 3);
        assert!(value.oldest_unacked_published_unix.is_none());
        assert!(value.age_unavailable.unwrap().contains("three reads"));
    }

    #[tokio::test]
    async fn a_consumed_sample_is_read_again_before_claiming_the_backlog_age() {
        let attempts = std::cell::Cell::new(0);
        let value = samples(|| {
            let n = attempts.get();
            attempts.set(n + 1);
            std::future::ready(Ok(if n == 0 {
                finish_sample(unresolved(), 90, 91, Some((91, 100)))
            } else {
                finish_sample(unresolved(), 91, 91, Some((92, 400)))
            }))
        })
        .await
        .unwrap();
        assert_eq!(
            value.oldest_unacked_sequence,
            Some(92),
            "the ACKed message is not the remaining backlog"
        );
        assert_eq!(value.snapshot(1000)["oldest_unacked_age_seconds"], 600);
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn progress_below_the_sampled_message_does_not_invalidate_its_age() {
        let Attempt::Settled(value) = finish_sample(unresolved(), 90, 91, Some((92, 400))) else {
            panic!("sample remains beyond ACK floor")
        };
        assert_eq!(value.oldest_unacked_sequence, Some(92));
    }

    #[test]
    fn redeliveries_are_visible_separately_from_pending_and_ack_pending() {
        let value = reading().snapshot(1000);
        assert_eq!(value["num_redelivered"], 0, "a missing counter is not zero");
        let mut sample = reading();
        sample.num_redelivered = 267;
        assert_eq!(sample.snapshot(1000)["num_redelivered"], 267);
    }

    #[test]
    fn clock_extremes_are_stale_instead_of_panicking() {
        let mut value = reading();
        for at in [i64::MIN, i64::MAX] {
            value.observed_at_unix = at;
            assert_eq!(value.snapshot(1000)["status"], "stale");
        }
    }

    fn reading() -> ConsumerLag {
        ConsumerLag {
            observed_at_unix: 1000,
            pending: 10,
            ack_pending: 2,
            num_redelivered: 0,
            ack_floor_stream_sequence: 90,
            oldest_unacked_sequence: Some(91),
            oldest_unacked_published_unix: Some(400),
            age_unavailable: None,
        }
    }

    #[test]
    fn lag_age_uses_retained_publication_not_ack_activity_or_delivery_time() {
        let value = reading().snapshot(1005);
        assert_eq!(value["oldest_unacked_age_seconds"], 605);
        assert_eq!(value["status"], "observed");
        assert_eq!(value["pending"], 10);
        assert_eq!(reading().snapshot(1031)["status"], "stale");
        assert_eq!(reading().snapshot(999)["status"], "stale");
    }

    #[test]
    fn missing_or_future_publication_is_unknown_never_zero_age() {
        let mut sample = reading();
        sample.oldest_unacked_published_unix = None;
        sample.age_unavailable = Some("retained history unavailable".into());
        assert!(sample.snapshot(1000)["oldest_unacked_age_seconds"].is_null());
        sample.oldest_unacked_published_unix = Some(1001);
        assert!(sample.snapshot(1000)["oldest_unacked_age_seconds"].is_null());
        sample.pending = 0;
        sample.ack_pending = 0;
        sample.oldest_unacked_sequence = None;
        sample.oldest_unacked_published_unix = None;
        assert_eq!(sample.snapshot(1000)["pending"], 0);
        assert!(sample.snapshot(1000)["oldest_unacked_age_seconds"].is_null());
    }
}
