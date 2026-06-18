-- =========================================================================
-- 00-extensions.sql — Required Postgres extensions.
-- =========================================================================

-- =============================================================================
-- BOSS — Postgres schema
--
-- The schema is split across numbered files, one per domain, applied in
-- order. This file is canonical: Rust domain types mirror these
-- structures, and adapters map rows → domain types.
--
-- Conventions:
--   - snake_case, plural table names
--   - timestamps are TIMESTAMPTZ, UTC
--   - CHECK constraints on closed enums (mirror the Rust enum variants);
--     tenant-extensible taxonomies live in the Class registry instead
--   - triggers / stored procs reserved for cross-row invariants and
--     append-only enforcement (audit_log immutability, GL period
--     locking, GL balance check) — domain logic stays in services
-- =============================================================================

-- Required Postgres extensions. `btree_gist` lets the GIST
-- exclusion constraint on `calendar_reservations` mix an equality
-- predicate on a TEXT column (`resource_kind:resource_id`) with
-- the range-overlap predicate on `tstzrange` (see
-- docs/architecture-decisions.md §Calendar).
-- `pgcrypto` provides `digest()` for the audit_log hash chain
-- (docs/architecture-decisions.md §Correctness protocol & the
-- audit log).
CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE EXTENSION IF NOT EXISTS pgcrypto;

