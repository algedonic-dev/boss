//! The log half of a refusing gate's clean window, read from
//! `audit_log` (design 21946380, backlog b0787727): the Postgres
//! adapter of [`GateEvidenceLog`]. `InMemoryGateEvidence` is the
//! double; `the_adapters_agree_on_the_gate_evidence_log_pg.rs` holds
//! the two to one statement.
//!
//! WHY THE AUDIT LOG. The facts arrive through each service's outbox
//! and the relay, exactly as every other fact does, so this read is a
//! projection of the system of record and nothing else: any reader, at
//! any time, rebuilds the same window from it (determinism), and a
//! process restart — every train — loses nothing it held.

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::gate_evidence::{Fact, FactReadBound, Gate, GateEvidenceLog, MAX_WINDOW_FACTS};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgGateEvidence {
    pool: PgPool,
}

impl PgGateEvidence {
    pub fn new(pool: PgPool) -> Self {
        PgGateEvidence { pool }
    }
}

type Row = (Uuid, DateTime<Utc>, String, String, serde_json::Value);

#[async_trait]
impl GateEvidenceLog for PgGateEvidence {
    async fn facts(&self, gate: Gate, from: DateTime<Utc>) -> Result<Vec<Event>, String> {
        // One statement retains the gate's facts in the window (the
        // `audit_log_kind` index), and each service's newest
        // `recording_began` before it and that opening epoch's facts.
        // The order, then the event id in byte order (uuid's own order
        // in Postgres is its bytes), is the double's, so a tie has one
        // answer everywhere.
        let mut rows = sqlx::query_as::<_, Row>(
            "WITH opened AS ( \
                 SELECT DISTINCT ON (payload->>'service') \
                        event_id, timestamp, source, kind, payload FROM audit_log \
                  WHERE kind = $3 AND timestamp < $2 \
                  ORDER BY payload->>'service', timestamp DESC, event_id DESC \
             ) SELECT event_id, timestamp, source, kind, payload FROM ( \
                 SELECT event_id, timestamp, source, kind, payload FROM audit_log \
                  WHERE kind = ANY($1) AND timestamp >= $2 \
                 UNION ALL SELECT * FROM opened \
                 UNION ALL \
                 SELECT e.event_id, e.timestamp, e.source, e.kind, e.payload FROM audit_log e \
                  WHERE e.kind = ANY($1) AND e.kind <> $3 AND e.timestamp < $2 \
                    AND EXISTS (SELECT 1 FROM opened o \
                        WHERE (o.payload->>'service') IS NOT DISTINCT FROM (e.payload->>'service') \
                          AND e.timestamp >= o.timestamp) \
             ) facts ORDER BY timestamp, event_id LIMIT $4",
        )
        .bind(gate.kinds())
        .bind(from)
        .bind(gate.kind(Fact::RecordingBegan))
        .bind((MAX_WINDOW_FACTS + 1) as i64)
        .fetch(&self.pool);
        let mut bound = FactReadBound::default();
        let mut out = Vec::new();
        while let Some((id, timestamp, source, kind, payload)) = rows
            .try_next()
            .await
            .map_err(|e| format!("reading {} facts from audit_log: {e}", gate.prefix()))?
        {
            let event = Event {
                id,
                timestamp,
                source,
                kind,
                payload,
            };
            bound.observe(&event)?;
            out.push(event);
        }
        Ok(out)
    }
}
