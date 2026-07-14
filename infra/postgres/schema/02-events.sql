-- =========================================================================
-- 02-events.sql — Events — audit_log (hash-chained, append-only) + messages_events + ref-check registry.
-- =========================================================================


-- =========================================================================
-- Audit log — persisted domain events from NATS
-- =========================================================================

-- ## messages_events — separate event log for boss-messages
--
-- Messages (DMs + system signals + notifications) need a retention
-- policy the compliance-grade audit_log can't have: audit_log is
-- hash-chained + REVOKE-protected and never gets purged, but operators
-- legitimately expire old messages (PII, stale broadcasts, employee
-- privacy). So messages get their own log.
--
-- This table mirrors audit_log's shape so boss-events can reuse the
-- writer pattern, but deliberately:
--   - has NO mutation trigger (DELETE is allowed; UPDATE still
--     allowed because retention jobs may need to redact PII)
--   - has NO hash chain (rows can disappear, which would constantly
--     break the chain)
--   - REVOKEs nothing (operators can purge)
--
-- audit_log is unaffected — every other domain event keeps landing
-- there with all its compliance guarantees.
CREATE TABLE IF NOT EXISTS messages_events (
  id          BIGSERIAL PRIMARY KEY,
  event_id    UUID NOT NULL,
  timestamp   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  source      TEXT NOT NULL,
  kind        TEXT NOT NULL,
  payload     JSONB NOT NULL DEFAULT '{}',
  created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS messages_events_kind ON messages_events(kind);

CREATE INDEX IF NOT EXISTS messages_events_timestamp ON messages_events(timestamp DESC);

CREATE INDEX IF NOT EXISTS messages_events_event_id ON messages_events(event_id);


CREATE TABLE IF NOT EXISTS audit_log (
  -- `id` and `created_at` are NOT defaulted at the column level —
  -- the BEFORE INSERT trigger (audit_log_compute_row_hash, below)
  -- assigns both AFTER acquiring the chain-hash advisory lock so
  -- they're monotonic with respect to commit order. Defaulting at
  -- the column level would let concurrent writers acquire ids /
  -- timestamps before lock acquisition, breaking the verifier's
  -- id-walk hash chain. The trigger draws ids from
  -- `audit_log_id_seq` (declared below) directly.
  id          BIGINT PRIMARY KEY,
  event_id    UUID NOT NULL,
  timestamp   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  source      TEXT NOT NULL,
  kind        TEXT NOT NULL,
  payload     JSONB NOT NULL DEFAULT '{}',
  created_at  TIMESTAMPTZ NOT NULL,
  -- Layer 2 of immutable-audit-log: each row stores its predecessor's
  -- row_hash + its own. `prev_hash` of the genesis row is 32 zero
  -- bytes. Both fields are filled by the BEFORE INSERT trigger; the
  -- writer never sets them, so a writer-side compromise that omits
  -- the chain still produces a valid chain.
  prev_hash   BYTEA NOT NULL,
  row_hash    BYTEA NOT NULL
);


-- Sequence the BEFORE INSERT trigger draws `id` from after taking
-- the chain-hash advisory lock — ids stay monotonic with commit
-- order, which the verifier's id-walk depends on.
CREATE SEQUENCE IF NOT EXISTS audit_log_id_seq OWNED BY audit_log.id;


CREATE INDEX IF NOT EXISTS audit_log_kind ON audit_log(kind);

CREATE INDEX IF NOT EXISTS audit_log_source ON audit_log(source);

CREATE INDEX IF NOT EXISTS audit_log_timestamp ON audit_log(timestamp DESC);

-- UNIQUE: the outbox relay's idempotence backstop — a crash between
-- its audit INSERT and its outbox mark must not double-insert on
-- retry (the relay pre-checks with NOT EXISTS; this constraint makes
-- a race a loud error instead of a silent duplicate). Emitters mint
-- a fresh UUID per event, so uniqueness holds for every legacy write
-- path too (verified 0 duplicates on the live playground log,
-- 2026-07-13). Replaces the earlier non-unique audit_log_event_id.
DROP INDEX IF EXISTS audit_log_event_id;
CREATE UNIQUE INDEX IF NOT EXISTS audit_log_event_id_unique ON audit_log(event_id);


-- Layer 1 of the immutable-audit-log story
-- (docs/architecture-decisions.md §Correctness protocol & the audit log):
-- block UPDATE / DELETE / TRUNCATE at the SQL level. The trigger fires for
-- any role, including superusers — disabling it (DROP TRIGGER, ALTER TABLE
-- DISABLE TRIGGER) leaves a DDL trail in pg_log. Layers 2 (hash chain) and
-- 3 (Merkle anchor) build on top of this baseline.
CREATE OR REPLACE FUNCTION audit_log_reject_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only: % rejected', TG_OP
        USING ERRCODE = 'raise_exception';
END;
$$ LANGUAGE plpgsql;


DROP TRIGGER IF EXISTS audit_log_reject_row_mutation_trg ON audit_log;

CREATE TRIGGER audit_log_reject_row_mutation_trg
    BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_reject_mutation();


DROP TRIGGER IF EXISTS audit_log_reject_truncate_trg ON audit_log;

CREATE TRIGGER audit_log_reject_truncate_trg
    BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION audit_log_reject_mutation();


REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM PUBLIC;


-- Layer 2 of immutable-audit-log: every INSERT computes the row's
-- chain link. The advisory lock serializes concurrent inserts so
-- two writers can't both read the same chain tail and produce a
-- fork. Reads are unaffected — only insert ordering serializes.
--
-- Canonical bytes: pipe-joined event_id|timestamp|source|kind|payload.
-- payload is jsonb whose ::text form is deterministic per Postgres
-- version, so verifying clients reproduce the same bytes by running
-- the same query rather than reimplementing the encoding.
--
-- Genesis row: `prev_hash` defaults to 32 zero bytes when audit_log
-- is empty. Hash construction follows the standard tamper-evident
-- ledger shape: row_hash = sha256(prev_hash || canonical).
--
-- The trigger assumes READ COMMITTED isolation (sqlx default). Under
-- REPEATABLE READ the snapshot taken at transaction start would not
-- see commits from a concurrent writer that just released the lock,
-- silently forking the chain. If a future caller needs a stricter
-- isolation level, switch to LOCK TABLE audit_log IN EXCLUSIVE MODE.
CREATE OR REPLACE FUNCTION audit_log_compute_row_hash()
RETURNS TRIGGER AS $$
DECLARE
    prev_hash BYTEA;
    canonical TEXT;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('audit_log'));

    -- Allocate `id` + `created_at` AFTER the lock so they're
    -- monotonic with respect to commit order. Otherwise concurrent
    -- writers can end up with id-order ≠ lock-order (BIGSERIAL
    -- pre-allocates the id value before the BEFORE-INSERT trigger
    -- fires) or created_at-order ≠ lock-order (DEFAULT NOW()
    -- captures the timestamp before lock acquisition). Either case
    -- breaks the verifier's id-ordered hash-chain walk + the
    -- "created_at must be monotonic" invariant.
    --
    -- We pair this with `ALTER COLUMN id DROP DEFAULT` and
    -- `ALTER COLUMN created_at DROP DEFAULT` below so the only
    -- writer of those columns is this trigger.
    NEW.id := nextval('audit_log_id_seq');
    NEW.created_at := clock_timestamp();

    SELECT row_hash INTO prev_hash
      FROM audit_log
      ORDER BY id DESC
      LIMIT 1;

    IF prev_hash IS NULL THEN
        prev_hash := decode(repeat('00', 32), 'hex');
    END IF;

    canonical := COALESCE(NEW.event_id::text, '') || '|'
              || COALESCE(NEW.timestamp::text, '')  || '|'
              || COALESCE(NEW.source, '')          || '|'
              || COALESCE(NEW.kind, '')            || '|'
              || COALESCE(NEW.payload::text, '');

    -- `canonical::bytea` interprets the TEXT as a bytea-format
    -- literal (`\x...` hex or `\nnn` octal escape). Any payload
    -- containing a backslash followed by characters Postgres can't
    -- decode as escape (e.g. JSON `\n` line breaks, the literal
    -- string `í`, even unicode glyphs that round-trip as
    -- escape-looking sequences) makes the cast fail with
    -- `invalid input syntax for type bytea`. The trigger then
    -- aborts the INSERT — silently, since the boss-events
    -- audit-writer absorbs the error to keep the domain write
    -- path's fire-and-forget contract.
    --
    -- `convert_to(text, 'UTF8')` is the safe way to encode TEXT
    -- to BYTEA via a known charset, no escape interpretation.
    -- Identical hash output for any payload that previously
    -- succeeded; brings any previously-failing payload into the
    -- chain.
    NEW.prev_hash := prev_hash;
    NEW.row_hash  := digest(prev_hash || convert_to(canonical, 'UTF8'), 'sha256');

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;


DROP TRIGGER IF EXISTS audit_log_compute_row_hash_trg ON audit_log;

CREATE TRIGGER audit_log_compute_row_hash_trg
    BEFORE INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_compute_row_hash();


-- =========================================================================
-- Layer 4 of immutable-audit-log: dangling-ref BEFORE INSERT trigger.
-- =========================================================================
-- An event whose payload names a subject_id / part_sku / account_id /
-- product_sku / vendor_id that doesn't exist in the corresponding
-- projection is the upstream cause of the bug class
-- "SPA link points at /catalog/SKU that 404s." The link came from a
-- projection row whose payload field referenced a nonexistent entity
-- — a bug that survives audit_log replay forever. Blocking the
-- offending INSERT at the source is the only fix that doesn't
-- require a separate scrub pass.
--
-- Per-event-kind rules live in `audit_log_ref_checks` (registry).
-- The trigger reads the registry, extracts the payload field for
-- each matching rule, and checks that the value exists in
-- (ref_table, ref_column). Empty value or NULL skips the check
-- (some events optionally carry a ref, e.g., audit metadata that
-- happens to include account_id only when scoped).
--
-- Escape hatch: when `audit_log.ref_check` is set to 'off' in the
-- session, every check is bypassed. The bundle-import path uses
-- this so the audit_log restore runs before the projections it
-- references have been rebuilt (the import order is audit_log first,
-- rebuild-all second).
CREATE TABLE IF NOT EXISTS audit_log_ref_checks (
    event_kind TEXT NOT NULL,
    field_path TEXT NOT NULL,
    ref_table  TEXT NOT NULL,
    ref_column TEXT NOT NULL,
    PRIMARY KEY (event_kind, field_path)
);


CREATE OR REPLACE FUNCTION audit_log_check_refs()
RETURNS TRIGGER AS $$
DECLARE
    rule        RECORD;
    ref_value   TEXT;
    found       BOOLEAN;
    check_sql   TEXT;
BEGIN
    -- Escape hatch for bundle-import / restore sessions.
    BEGIN
        IF current_setting('audit_log.ref_check', true) = 'off' THEN
            RETURN NEW;
        END IF;
    EXCEPTION WHEN OTHERS THEN
        -- Setting not defined → fall through (check enabled).
        NULL;
    END;

    FOR rule IN
        SELECT field_path, ref_table, ref_column
          FROM audit_log_ref_checks
         WHERE event_kind = NEW.kind
    LOOP
        ref_value := NEW.payload ->> rule.field_path;
        IF ref_value IS NULL OR ref_value = '' THEN
            CONTINUE;
        END IF;
        check_sql := format(
            'SELECT EXISTS (SELECT 1 FROM %I WHERE %I = $1)',
            rule.ref_table, rule.ref_column
        );
        EXECUTE check_sql INTO found USING ref_value;
        IF NOT found THEN
            RAISE EXCEPTION
                'audit_log: event % references non-existent %.% = %',
                NEW.kind, rule.ref_table, rule.ref_column, ref_value
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;


DROP TRIGGER IF EXISTS audit_log_check_refs_trg ON audit_log;

CREATE TRIGGER audit_log_check_refs_trg
    BEFORE INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_check_refs();


-- ---------------------------------------------------------------------------
-- Transactional event outbox
-- (docs/design/transactional-audit-log.md — Option B).
--
-- A state-changing operation INSERTs its event here INSIDE the same
-- transaction as the state change, so the durable hand-off is atomic:
-- either both commit or neither. The single relay
-- (`boss-event-relay`) drains rows in id order, inserts them into
-- audit_log (where the chain-hash trigger runs exactly as today —
-- one writer, no lock contention) and publishes to NATS, then stamps
-- `delivered_at`. Plain BIGSERIAL — deliberately NO chain trigger and
-- NO global advisory lock here; holding the chain lock for a domain
-- transaction's lifetime would serialize every writer in the system.
CREATE TABLE IF NOT EXISTS event_outbox (
    id           BIGSERIAL PRIMARY KEY,
    event_id     UUID NOT NULL UNIQUE,
    timestamp    TIMESTAMPTZ NOT NULL,
    source       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    payload      JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- NULL = pending; the relay stamps this only after the audit
    -- INSERT is committed AND the NATS publish succeeded, so a crash
    -- anywhere retries the row (audit side is idempotent by
    -- event_id, consumers are idempotent by the NAK-redelivery
    -- contract).
    delivered_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS event_outbox_pending
    ON event_outbox(id) WHERE delivered_at IS NULL;

-- Referential guarding runs HERE, inside the domain transaction —
-- same rules table, same function as the audit_log trigger. A
-- payload referencing a missing projection row now ABORTS the whole
-- domain write (the correct outcome the audit-side trigger could
-- never deliver post-commit: it punched provenance holes instead —
-- the 2026-07-13 phantom-account incident). The audit_log-side
-- trigger stays as belt-and-braces for the relay + legacy writers.
DROP TRIGGER IF EXISTS event_outbox_check_refs_trg ON event_outbox;

CREATE TRIGGER event_outbox_check_refs_trg
    BEFORE INSERT ON event_outbox
    FOR EACH ROW EXECUTE FUNCTION audit_log_check_refs();

