//! A packet's budget under the event bound (backlog 4f8783fb, design
//! 6ab85352 car 1, decided 2026-11-13).
//!
//! Every Job or Step write stages ONE state event carrying the whole
//! row (`events.rs`), and the outbox refuses any event that encodes
//! over [`BOUND`] — NATS's 1 MiB payload less a 64 KiB margin. So the
//! cost of a write is the size of the packet, not of the change, and a
//! packet that grows to the bound can take no further write of any kind:
//! no annotation, no step move, no close. Measured 2026-10-15 on
//! 6cd6c935 (a short note refused at 983,662 bytes, answered 500) and
//! 2026-11-13 on 044b189b, whose last staged event sits about 26 bytes
//! under the bound, too close for its own close.
//!
//! The bound stays (the item forbids raising it). Under it each packet
//! keeps a budget, judged on the encoded state event a write is about to
//! stage — measured by the outbox's own [`encoded_len`], so the budget
//! and the bound cannot disagree about a size:
//!
//! - at [`WARN_AT`] (half the bound) a write lands and its answer carries
//!   a [`SizeWarning`], so nobody learns of the wall by hitting it;
//! - at [`BUDGET`] (three quarters) a write that GROWS the row is
//!   refused, naming the size, the budget, the bound and [`REPAIR_DOOR`];
//! - a write that holds or shrinks the row, and a close or a status move,
//!   is never refused by the budget. The 25% reserve (245,760 bytes) is
//!   far more than any close adds (a date, an instant, an outcome: under
//!   a hundred bytes), so closure is guaranteed by construction for every
//!   packet that stayed under its budget;
//! - over the bound itself no write that carries the row can be staged —
//!   the bus could never carry it — and that refusal names the repair
//!   door too, instead of reading as a storage outage;
//! - EXCEPT THE CLOSE (backlog b96df4f8). A packet that grew past the
//!   bound before this budget existed had no reserve to spend, and its
//!   close was refused like any write: 044b189b (~984 KB, 762 keys, used
//!   as a mailbox) stood open from 2026-11-03 with every step terminal,
//!   misleading every liveness reader. A close whose whole-row event is
//!   over the bound stages [`close_event`]'s bounded fact instead — the
//!   close's own fields and the sha256 of the row it leaves — which the
//!   rebuild applies and checks against that digest. Closure no longer
//!   depends on the packet having stayed small.
//!
//! "Grows" is judged on `metadata`, the map every annotation writes:
//! its encoding after the write against its encoding as the write FOUND
//! it under the row lock. The fixed columns move by a few bytes on a
//! status or date change, and those writes are exactly the ones the
//! reserve exists to admit.

use boss_core::event::Event;
use boss_core::job::{Job, JobId, JobStatus};
use boss_core::publisher::EventStamp;
use boss_events::outbox::{MAX_STAGED_EVENT_BYTES, encoded_len};
use serde_json::{Map, Value};

/// The outbox's bound, read from the outbox — never a copy of it.
pub const BOUND: usize = MAX_STAGED_EVENT_BYTES;
/// Half the bound: a write at or past it lands and says so (491,520).
pub const WARN_AT: usize = BOUND / 2;
/// Three quarters of the bound: a write at or past it that grows the
/// packet is refused (737,280).
pub const BUDGET: usize = BOUND / 4 * 3;
/// The recorded door that moves a packet's large values to the file
/// store by hash-verified reference and shrinks the row (design
/// 6ab85352, car 4). Named by every refusal, before it exists, so the
/// refusal says where the way out is rather than that there is none.
pub const REPAIR_DOOR: &str = "boss job externalise";
/// The response header a write past [`WARN_AT`] carries: a
/// [`SizeWarning`] as JSON. A header, not a body, because the merge
/// doors answer 204, which carries no body.
pub const WARNING_HEADER: &str = "boss-size-warning";
/// Every refusal's text starts with this, so a door that only sees the
/// error's words still answers 413 rather than 500.
pub const REFUSAL_PREFIX: &str = "packet event budget refused:";

/// A write the budget or the bound refused. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The packet (Job id) the event is about — a step event names its
    /// Job, because the repair door works on the packet.
    pub packet: String,
    /// The state event's kind (`jobs.job.updated`, `jobs.step.updated`).
    pub kind: String,
    /// What the event would have encoded to.
    pub event_bytes: usize,
    /// True when the event is over the bound itself; false when it is
    /// under the bound and refused only because it grows a packet at or
    /// past its budget.
    pub over_bound: bool,
}

impl Refusal {
    /// The repair door, spelled for this packet.
    pub fn door(&self) -> String {
        door_for(&self.packet)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (packet, kind, bytes, door) = (&self.packet, &self.kind, self.event_bytes, self.door());
        if self.over_bound {
            write!(
                f,
                "{REFUSAL_PREFIX} job {packet}: its {kind} event would encode to {bytes} bytes, \
                 over the {BOUND}-byte event bound, which the bus (NATS max_payload, 1 MiB) \
                 could never carry; the packet budget is {BUDGET} bytes. Nothing was written. \
                 Shrink the packet with `{door}`, which keeps every original, then send this \
                 write again"
            )
        } else {
            write!(
                f,
                "{REFUSAL_PREFIX} this write grows job {packet}, and its {kind} event would \
                 encode to {bytes} bytes, at or over the packet budget of {BUDGET} bytes (three \
                 quarters of the {BOUND}-byte event bound). Nothing was written. A write that \
                 holds or shrinks the packet, a step transition and its close are still \
                 admitted. Move its large values out with `{door}`, which keeps every original"
            )
        }
    }
}

/// What a write past [`WARN_AT`] answers beside its success.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SizeWarning {
    pub event_bytes: usize,
    pub warn_at: usize,
    pub budget: usize,
    pub bound: usize,
    pub door: String,
}

fn door_for(packet: &str) -> String {
    format!("{REPAIR_DOOR} {packet}")
}

/// The packet a state event is about: a step's names its Job
/// (`job_id`), a Job's is its own `id`.
fn packet_of(event: &Event) -> String {
    ["job_id", "id"]
        .iter()
        .find_map(|key| event.payload.get(key).and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

fn bytes_of(event: &Event) -> usize {
    // An event the outbox cannot encode it cannot stage either, so it
    // measures as over everything rather than as nothing.
    encoded_len(event).unwrap_or(usize::MAX)
}

fn metadata_bytes(metadata: &Value) -> usize {
    serde_json::to_vec(metadata).map_or(usize::MAX, |b| b.len())
}

/// Judge one state event before it is staged. `grows` says whether the
/// write makes the row larger; a close, a status move or any write that
/// holds or shrinks the row passes `false` and meets only the bound.
pub fn admit(event: &Event, grows: bool) -> Result<(), Refusal> {
    let event_bytes = bytes_of(event);
    let over_bound = event_bytes > BOUND;
    if over_bound || (grows && event_bytes >= BUDGET) {
        return Err(Refusal {
            packet: packet_of(event),
            kind: event.kind.clone(),
            event_bytes,
            over_bound,
        });
    }
    Ok(())
}

/// Judge a write's state event against the metadata the write FOUND on
/// the row (read under its lock): it grows the packet when the
/// `metadata` it stages encodes larger than that.
pub fn admit_against(event: &Event, found: &Value) -> Result<(), Refusal> {
    let staged = event.payload.get("metadata").unwrap_or(&Value::Null);
    admit(event, metadata_bytes(staged) > metadata_bytes(found))
}

/// Judge a whole-row write's `jobs.job.updated` (the job PUT: its
/// events are built by the door and handed to the adapter). A write that
/// MOVES the status — a hand close, a cancel — meets only the bound, the
/// same as every other close: a finished status does not move again, so
/// that admission is spent once per packet. Any other whole-row write is
/// judged against the metadata the adapter found under its lock.
pub fn admit_whole_row(events: &[Event], moves_status: bool, found: &Value) -> Result<(), Refusal> {
    events
        .iter()
        .filter(|event| event.kind == crate::events::JOB_UPDATED)
        .try_for_each(|event| {
            if moves_status {
                admit(event, false)
            } else {
                admit_against(event, found)
            }
        })
}

/// What a close stages for a packet whose whole row is over the bound:
/// the payload of [`crate::events::JOB_CLOSED_OVER_BOUND`] (backlog
/// b96df4f8). The close's own fields, which the rebuild applies to the
/// row the log has already built, and the digest of the row the close
/// leaves, which the rebuild must reproduce before it goes on — so the
/// record proves the post-close row without carrying it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundedClose {
    /// The packet closed.
    pub id: JobId,
    /// The status the close moved it to.
    pub status: JobStatus,
    pub closed_on: Option<chrono::NaiveDate>,
    /// The metadata keys the close owns (`closed_at`, `outcome`),
    /// merged over the row as it stood — the same merge the close's one
    /// UPDATE made.
    pub merged: Map<String, Value>,
    /// [`row_digest`] of the post-close row.
    pub row_sha256: String,
    pub row_bytes: usize,
    /// What the whole-row event would have encoded to: why this one was
    /// staged instead.
    pub whole_row_event_bytes: usize,
    pub bound: usize,
}

/// The digest and size of a Job row as its whole-row state event
/// carries it — `serde_json::to_vec` of the row's JSON value, without
/// the stamp's `_actor` / `_partition`, which describe the write and
/// not the row. One function, so the close that records a digest and
/// the rebuild that checks it cannot spell the encoding two ways.
pub fn row_digest(job: &Job) -> (String, usize) {
    digest_of(&serde_json::to_value(job).unwrap_or_default())
}

fn digest_of(row: &Value) -> (String, usize) {
    let bytes = serde_json::to_vec(row).unwrap_or_default();
    (boss_core::evidence::sha256_hex(&bytes), bytes.len())
}

/// The state event a close stages for the post-close row `closed`.
///
/// The whole row ([`crate::events::JOB_UPDATED`]) whenever it fits under
/// the bound — every close of a packet that kept its reserve, which is
/// every packet since the budget landed. Over the bound, the
/// [`BoundedClose`] delta instead, never the refusal: that refusal left
/// 044b189b open with every step terminal, and no write could ever have
/// repaired it, because every repair write carried the same row (backlog
/// b96df4f8). Still judged against the bound itself, so it can only be
/// refused if the close's own `merged` keys were somehow over it.
pub fn close_event(
    stamp: &EventStamp,
    closed: &Job,
    merged: &Map<String, Value>,
) -> Result<Event, Refusal> {
    let row = serde_json::to_value(closed).unwrap_or_default();
    let whole = stamp.event(crate::events::JOB_UPDATED, row.clone());
    let whole_row_event_bytes = match admit(&whole, false) {
        Ok(()) => return Ok(whole),
        Err(refusal) => refusal.event_bytes,
    };
    let (row_sha256, row_bytes) = digest_of(&row);
    let delta = BoundedClose {
        id: closed.id,
        status: closed.status,
        closed_on: closed.closed_on,
        merged: merged.clone(),
        row_sha256,
        row_bytes,
        whole_row_event_bytes,
        bound: BOUND,
    };
    let bounded = stamp.event(
        crate::events::JOB_CLOSED_OVER_BOUND,
        serde_json::to_value(&delta).unwrap_or_default(),
    );
    admit(&bounded, false).map(|()| bounded)
}

/// The warning a landed write's state event earns, if it is past
/// [`WARN_AT`].
pub fn warning(event: &Event) -> Option<SizeWarning> {
    let event_bytes = bytes_of(event);
    (event_bytes >= WARN_AT).then(|| SizeWarning {
        event_bytes,
        warn_at: WARN_AT,
        budget: BUDGET,
        bound: BOUND,
        door: door_for(&packet_of(event)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, payload: Value) -> Event {
        Event::new("jobs", kind, payload, chrono::Utc::now())
    }

    /// A job event whose metadata holds one value of `n` bytes.
    fn job_event(n: usize) -> Event {
        event(
            "jobs.job.updated",
            json!({"id": "job-1", "metadata": {"evidence": "e".repeat(n)}}),
        )
    }

    #[test]
    fn the_budget_is_derived_from_the_outbox_bound_and_nothing_else() {
        assert_eq!(BOUND, 983_040);
        assert_eq!(WARN_AT, 491_520);
        assert_eq!(BUDGET, 737_280);
        assert_eq!(BOUND - BUDGET, 245_760, "the reserve a close draws on");
    }

    #[test]
    fn a_growing_event_at_the_budget_is_refused_and_under_it_admitted() {
        let at = job_event(BUDGET);
        assert!(bytes_of(&at) >= BUDGET);
        let refusal = admit(&at, true).expect_err("refused at the budget");
        assert!(!refusal.over_bound);
        assert_eq!(refusal.packet, "job-1");
        assert!(admit(&job_event(BUDGET - 1_000), true).is_ok());
    }

    #[test]
    fn a_write_that_does_not_grow_is_admitted_up_to_the_bound() {
        assert!(admit(&job_event(BOUND - 1_000), false).is_ok());
        let over = admit(&job_event(BOUND), false).expect_err("over the bound");
        assert!(over.over_bound, "only the bound refuses a close: {over}");
    }

    #[test]
    fn growth_is_judged_against_the_metadata_the_write_found() {
        let staged = job_event(BUDGET);
        let same = json!({"evidence": "e".repeat(BUDGET)});
        let smaller = json!({"evidence": "e".repeat(BUDGET - 10)});
        let larger = json!({"evidence": "e".repeat(BUDGET + 10)});
        assert!(admit_against(&staged, &same).is_ok(), "a write that holds");
        assert!(
            admit_against(&staged, &larger).is_ok(),
            "a write that shrinks"
        );
        assert!(
            admit_against(&staged, &smaller).is_err(),
            "a write that grows"
        );
    }

    #[test]
    fn a_step_event_names_the_packet_its_step_belongs_to() {
        let step = event(
            "jobs.step.updated",
            json!({"id": "step-1", "job_id": "job-1", "metadata": {"t": "t".repeat(BUDGET)}}),
        );
        let refusal = admit(&step, true).expect_err("refused");
        assert_eq!(refusal.packet, "job-1");
        assert_eq!(refusal.door(), "boss job externalise job-1");
    }

    #[test]
    fn a_refusal_names_its_size_the_budget_the_bound_and_the_door() {
        for (n, grows) in [(BUDGET, true), (BOUND, false)] {
            let refusal = admit(&job_event(n), grows).expect_err("refused");
            let text = refusal.to_string();
            for needle in [
                REFUSAL_PREFIX.to_string(),
                refusal.event_bytes.to_string(),
                BUDGET.to_string(),
                BOUND.to_string(),
                "boss job externalise job-1".to_string(),
                "Nothing was written".to_string(),
            ] {
                assert!(text.starts_with(REFUSAL_PREFIX), "{text}");
                assert!(text.contains(&needle), "names {needle}: {text}");
            }
        }
    }

    #[test]
    fn a_warning_starts_at_half_the_bound() {
        assert!(warning(&job_event(WARN_AT - 1_000)).is_none());
        let w = warning(&job_event(WARN_AT)).expect("a warning at half");
        assert_eq!(
            (w.warn_at, w.budget, w.bound, w.door.as_str()),
            (WARN_AT, BUDGET, BOUND, "boss job externalise job-1")
        );
        assert!(w.event_bytes >= WARN_AT);
    }
}
