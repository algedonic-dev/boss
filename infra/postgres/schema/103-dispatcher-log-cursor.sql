-- 103-dispatcher-log-cursor.sql — the log-as-the-bus cursor
-- (transactional-audit-log Q6, stage 1; backlog 3d6d6bea).
--
-- A log-tailing consumer's entire durable state is one row: the
-- highest audit_log id it has settled. The id-walk is miss-free
-- because insert order ≡ commit order under the single-writer relay
-- (the Q2/Q6 coupling recorded in the design doc) — no dedup table,
-- no ack ledger, a crash re-runs at most the rows past the cursor
-- and every rules handler is already idempotent.
--
-- First-run initialization is new-only: the consumer starts at
-- MAX(audit_log.id) at ensure time, mirroring the JetStream durable
-- consumer's position rather than replaying the whole log. Same
-- per-item durable-advance discipline as dispatcher_clock_cursor.
CREATE TABLE IF NOT EXISTS dispatcher_log_cursor (
    consumer      TEXT PRIMARY KEY,
    last_audit_id BIGINT NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
