//! A materialized, sequence-addressed lens over resource facts. No claim,
//! capacity measurement, lease or reservation authority lives on this port.
use crate::{
    flow_resources::{Allocation, Amount, Claim},
    pools::PoolSpec,
    regions::RegionState,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

mod memory;
pub use memory::InMemoryClaimsHistory;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::PgClaimsHistory;
#[cfg(feature = "postgres")]
pub mod source;

pub const LEASES_UNAVAILABLE: &str = "leases arrive with car 2b";

/// The pre-commit statement budget every operation of the Postgres history
/// spends across ALL its statements, measured from just before it opens its
/// transaction, and the budget the source's audit-tail read spends. This is
/// the production latency claim; the installed reader's probe proves it,
/// not a test on a shared fixture database.
pub(crate) const STATEMENT_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(all(test, feature = "postgres"))]
mod roster_tests {
    #[test]
    fn resource_producers_fold_and_native_selection_have_one_roster() {
        use std::collections::BTreeSet;
        let declared: BTreeSet<_> = crate::flow_resources::FACT_KINDS.iter().copied().collect();
        let fold: BTreeSet<_> = super::memory::HANDLED_RESOURCE_KINDS
            .iter()
            .copied()
            .collect();
        assert_eq!(
            fold,
            declared,
            "resource kind drift: missing in fold={:?}, undeclared in fold={:?}",
            declared.difference(&fold).collect::<Vec<_>>(),
            fold.difference(&declared).collect::<Vec<_>>()
        );
        let generic: BTreeSet<_> = [
            crate::events::JOB_CREATED,
            crate::events::JOB_UPDATED,
            crate::events::STEP_CREATED,
            crate::events::STEP_UPDATED,
            crate::events::STATION_CLAIMED,
            crate::events::STATION_SELECTION_EVIDENCE,
        ]
        .into_iter()
        .collect();
        let selected: BTreeSet<_> = super::source::selected_kinds()
            .into_iter()
            .filter(|kind| !generic.contains(kind))
            .collect();
        assert_eq!(
            selected, declared,
            "resource kind drift in actual native selection"
        );
        assert_eq!(
            declared.len(),
            crate::flow_resources::FACT_KINDS.len(),
            "duplicate resource producer kind"
        );
        assert_eq!(
            fold.len(),
            super::memory::HANDLED_RESOURCE_KINDS.len(),
            "duplicate folded resource kind"
        );
        // Merely adding a name to both lists cannot turn an unimplemented
        // match arm into coverage. A malformed declared fact must be parsed
        // (and refused) or explicitly named, never return an empty fold.
        for kind in declared {
            let at = chrono::DateTime::from_timestamp(0, 0).unwrap();
            let fact = super::SequencedFact {
                seq: 1,
                recorded_at: at,
                event_id: uuid::Uuid::new_v4(),
                event_at: at,
                source: "jobs".into(),
                kind: kind.into(),
                payload: "{}".into(),
                original_group: None,
            };
            assert!(
                !matches!(super::memory::projected_rows(&fact,None,None,None,None),Ok(rows) if rows.is_empty()),
                "declared resource kind silently skipped by fold: {kind}"
            );
        }
    }
}
pub(super) const MAX_READING_BYTES: usize = 8 * 1024 * 1024;
struct ReadingBytes(usize);
impl std::io::Write for ReadingBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_READING_BYTES)
            .ok_or_else(|| std::io::Error::other("claims materialized byte bound exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn check_bytes(value: &impl Serialize) -> Result<(), ClaimsError> {
    serde_json::to_writer(&mut ReadingBytes(0), value)
        .map_err(|e| ClaimsError::Unavailable(format!("materialized byte bound: {e}")))
}
fn check_rows<'a>(rows: impl IntoIterator<Item = &'a memory::Row>) -> Result<(), ClaimsError> {
    let mut bytes = ReadingBytes(0);
    for (n, row) in rows.into_iter().enumerate() {
        if n >= 4096 {
            return Err(ClaimsError::Unavailable(
                "claims projection read exceeds4096row bound".into(),
            ));
        }
        serde_json::to_writer(&mut bytes, row)
            .map_err(|e| ClaimsError::Unavailable(format!("materialized byte bound: {e}")))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionCursor {
    pub generation: i64,
    pub through: i64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SourceObservation {
    pub audit_head: i64,
    pub observed_at: DateTime<Utc>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionBasis {
    pub generation: i64,
    pub through: i64,
    pub observed_audit_head: Option<i64>,
    pub source_observed_at: Option<DateTime<Utc>>,
    /// Ordinal distance, not an event count: audit sequences may have gaps.
    pub sequence_distance: Option<i64>,
    pub source_unavailable_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub unknown_kinds: Vec<UnknownKind>,
    #[serde(default)]
    pub other_waits: Vec<OtherWait>,
    #[serde(default)]
    pub wait_attention: Vec<WaitAttention>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherWait {
    pub kind: String,
    pub count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitAttention {
    pub kind: String,
    pub job_id: String,
    pub step_id: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownKind {
    pub kind: String,
    pub first_seq: i64,
    pub count: u64,
}
pub(super) fn projection_basis(
    cursor: ProjectionCursor,
    source: Option<SourceObservation>,
    failed: Option<DateTime<Utc>>,
) -> ProjectionBasis {
    ProjectionBasis {
        generation: cursor.generation,
        through: cursor.through,
        observed_audit_head: source.map(|s| s.audit_head),
        source_observed_at: source.map(|s| s.observed_at),
        sequence_distance: source
            .and_then(|s| s.audit_head.checked_sub(cursor.through))
            .filter(|d| *d >= 0),
        source_unavailable_at: failed,
        unknown_kinds: Vec::new(),
        other_waits: Vec::new(),
        wait_attention: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedFact {
    pub seq: i64,
    pub recorded_at: DateTime<Utc>,
    pub event_id: uuid::Uuid,
    pub event_at: DateTime<Utc>,
    pub source: String,
    pub kind: String,
    /// Preserve the original JSON text before any generic numeric decoder.
    pub payload: String,
    /// The original roster/sequence evidence, never a delivery attempt UUID.
    pub original_group: Option<boss_events::original_group::OriginalGroup>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryBatch {
    pub through: i64,
    pub through_at: DateTime<Utc>,
    pub facts: Vec<SequencedFact>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenClaim {
    pub step_id: String,
    pub job_id: String,
    pub holder: Option<String>,
    /// The holder's executor kind — person, robot or automaton (factory
    /// taxonomy M1, backlog 3ff2ef60); `None` only when nobody holds it.
    pub executor_kind: Option<boss_core::actor::ExecutorKind>,
    /// How the kind is known: `recorded` by the claim door on the
    /// claim's fact, or `derived-from-id` for a claim recorded before it
    /// did — the backfill, which is this fold and never a hand row.
    pub executor_kind_basis: Option<String>,
    /// Executing run edge recorded on the step at this cut, never actor inference.
    pub run: Option<String>,
    pub run_unavailable: Option<String>,
    /// Original station-claim receipt provenance, never current membership.
    pub station: Option<String>,
    #[serde(default)]
    pub station_id: Option<String>,
    #[serde(default)]
    pub station_resolution: Option<String>,
    #[serde(default)]
    pub station_resolution_reason: Option<String>,
    pub station_unavailable: Option<String>,
    /// Owning packet admission, distinct from activation and run admission.
    pub packet_opened_at: Option<DateTime<Utc>>,
    pub packet_opened_at_unavailable: Option<String>,
    pub resource_acquisition: Option<Claim>,
    /// What the claim holds, exactly as its token recorded it. Every
    /// installed CLI decodes this into main's three-field `Allocation`,
    /// which refuses an unknown field, so no reserved/borrowed split is
    /// stated here until the claim doors write version 3 and those
    /// readers have gone first (review bf361c75 P2; item bf2310ed).
    pub allocations: Vec<Allocation>,
    pub since: DateTime<Utc>,
    pub lease_remaining: Option<i64>,
    pub lease_unavailable: String,
    pub identity_binding: Option<crate::resource_leases::IdentityBinding>,
    pub lease_expired: bool,
    pub stock_obligations: Vec<crate::resource_leases::StockObligation>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaitingOn {
    pub step_id: String,
    pub job_id: String,
    pub pool: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolReading {
    pub pool_key: String,
    /// Missing declaration remains unknown; never invent a registry version.
    pub pool: Option<PoolSpec>,
    pub reserved: Amount,
    pub failed_reservations: u64,
    pub releases: u64,
    pub state: RegionState,
    pub band: Option<String>,
}
/// THE BINDING CONSTRAINT among `pools` (design a236103a D6): the pool
/// with the most failed reservations in the window; equal counts choose
/// the smaller registry key; none when nothing was refused.
pub fn binding<'a>(pools: impl IntoIterator<Item = &'a PoolReading>) -> Option<&'a PoolReading> {
    pools
        .into_iter()
        .filter(|p| p.failed_reservations > 0)
        .max_by(|a, b| {
            a.failed_reservations
                .cmp(&b.failed_reservations)
                .then_with(|| b.pool_key.cmp(&a.pool_key))
        })
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimsReading {
    pub projection: ProjectionBasis,
    pub as_of: i64,
    pub as_of_at: DateTime<Utc>,
    pub retained_from: DateTime<Utc>,
    pub retention_hours: i64,
    pub window_hours: i64,
    pub claims: Vec<OpenClaim>,
    pub waiting_on: Vec<WaitingOn>,
    pub pools: Vec<PoolReading>,
    pub binding: Option<String>,
}
/// One finished span of a [`ReadTrace`]: the statement and its time.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadQueryTime {
    pub query: String,
    pub elapsed_ms: u64,
}
/// An explicit, request-owned observer survives cancellation of the read
/// future. The HTTP deadline can name the query it interrupted, without a
/// singleton, another read, or extending either existing deadline. Built
/// for the retired cuts index and kept, renamed, for the head read
/// (backlog 81f8e468, 4bec011d).
#[derive(Clone, Default)]
pub struct ReadTrace(std::sync::Arc<std::sync::Mutex<ReadTraceState>>);
#[derive(Default)]
struct ReadTraceState {
    active: Option<(&'static str, tokio::time::Instant)>,
    finished: Vec<ReadQueryTime>,
}
impl ReadTrace {
    pub fn start(&self, query: &'static str) {
        if let Ok(mut state) = self.0.lock() {
            state.active = Some((query, tokio::time::Instant::now()));
        }
    }
    pub fn finish(&self) {
        if let Ok(mut state) = self.0.lock()
            && let Some((query, start)) = state.active.take()
        {
            state.finished.push(ReadQueryTime {
                query: query.into(),
                elapsed_ms: start.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            });
        }
    }
    pub fn describe(&self) -> String {
        self.0
            .lock()
            .map(|state| {
                let active = state
                    .active
                    .map(|(query, start)| {
                        format!("{query} elapsed_ms={}", start.elapsed().as_millis())
                    })
                    .unwrap_or_else(|| "none".into());
                format!(
                    "active_query={active}; completed_query_ms={:?}",
                    state.finished
                )
            })
            .unwrap_or_else(|_| "query trace unavailable: observer lock poisoned".into())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ClaimsError {
    #[error("claims history unavailable: {0}")]
    Unavailable(String),
    #[error("invalid claims history request: {0}")]
    Invalid(String),
    #[error("claims history outside retention: {0}")]
    OutsideRetention(String),
}
#[async_trait]
pub trait ClaimsHistory: Send + Sync {
    /// The head or historical read with a request-owned observer, so the
    /// HTTP deadline names the statement it interrupted (backlog 81f8e468).
    /// An adapter with no SQL stages keeps its read behavior and is one
    /// `adapter` span.
    async fn read_traced(
        &self,
        as_of: Option<i64>,
        window_hours: i64,
        now: DateTime<Utc>,
        trace: ReadTrace,
    ) -> Result<ClaimsReading, ClaimsError> {
        trace.start("adapter");
        let result = self.read(as_of, window_hours, now).await;
        trace.finish();
        result
    }
    async fn prune(
        &self,
        _expected: ProjectionCursor,
        _now: DateTime<Utc>,
        _limit: usize,
    ) -> Result<usize, ClaimsError> {
        Err(ClaimsError::Unavailable(
            "bounded retention not implemented".into(),
        ))
    }
    async fn cursor(&self) -> Result<ProjectionCursor, ClaimsError> {
        Err(ClaimsError::Unavailable(
            "projection cursor not implemented".into(),
        ))
    }
    async fn begin_rebuild(
        &self,
        _expected: ProjectionCursor,
    ) -> Result<ProjectionCursor, ClaimsError> {
        Err(ClaimsError::Unavailable(
            "projection rebuild not implemented".into(),
        ))
    }
    async fn advance(
        &self,
        _expected: ProjectionCursor,
        _batch: &HistoryBatch,
        _source: SourceObservation,
    ) -> Result<(), ClaimsError> {
        Err(ClaimsError::Unavailable(
            "projection advance not implemented".into(),
        ))
    }
    async fn source_failed(
        &self,
        _expected: ProjectionCursor,
        _at: DateTime<Utc>,
    ) -> Result<(), ClaimsError> {
        Err(ClaimsError::Unavailable(
            "source failure observation not implemented".into(),
        ))
    }
    /// The deadline the source's audit-tail read spends, measured when the
    /// read starts: production's [`STATEMENT_BUDGET`] from now. A history
    /// built for a fold fixture answers its own, so the tail read it is
    /// advanced through outlasts a loaded gate drive as its statements do;
    /// until backlog ebbf38c6 the tail took a literal 2 s whatever the
    /// history had been given. A decorator delegates this to what it wraps.
    fn source_deadline(&self) -> tokio::time::Instant {
        tokio::time::Instant::now() + STATEMENT_BUDGET
    }
    #[cfg(feature = "test-support")]
    async fn materialize(&self, _batch: &HistoryBatch) -> Result<(), ClaimsError> {
        Err(ClaimsError::Unavailable(
            "materialization not implemented".into(),
        ))
    }
    async fn read(
        &self,
        _as_of: Option<i64>,
        _window_hours: i64,
        _now: DateTime<Utc>,
    ) -> Result<ClaimsReading, ClaimsError> {
        Err(ClaimsError::Unavailable("read not implemented".into()))
    }
}
