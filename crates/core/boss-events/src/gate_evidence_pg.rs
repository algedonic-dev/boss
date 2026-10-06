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
use boss_core::gate_evidence::{Fact, Gate, GateEvidenceLog};
use chrono::{DateTime, Utc};
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
        // Two reads, one statement: the gate's facts in the window (the
        // `audit_log_kind` index), and each service's newest
        // `recording_began` before it — the mode the window opened in.
        // The order, then the event id in byte order (uuid's own order
        // in Postgres is its bytes), is the double's, so a tie has one
        // answer everywhere.
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT event_id, timestamp, source, kind, payload FROM ( \
                 SELECT event_id, timestamp, source, kind, payload FROM audit_log \
                  WHERE kind = ANY($1) AND timestamp >= $2 \
                 UNION ALL \
                 SELECT * FROM ( \
                     SELECT DISTINCT ON (payload->>'service') \
                            event_id, timestamp, source, kind, payload \
                       FROM audit_log \
                      WHERE kind = $3 AND timestamp < $2 \
                      ORDER BY payload->>'service', timestamp DESC, event_id DESC \
                 ) opened \
             ) facts \
             ORDER BY timestamp, event_id",
        )
        .bind(gate.kinds())
        .bind(from)
        .bind(gate.kind(Fact::RecordingBegan))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("reading {} facts from audit_log: {e}", gate.prefix()))?;
        Ok(rows
            .into_iter()
            .map(|(id, timestamp, source, kind, payload)| Event {
                id,
                timestamp,
                source,
                kind,
                payload,
            })
            .collect())
    }
}
