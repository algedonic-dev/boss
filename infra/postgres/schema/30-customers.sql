-- boss-customers — DTC end-consumers (the /shop channel).
--
-- Q4 of docs/design/subject-identity-and-relationships.md (approved
-- 2026-07-15), second half: customer graduates from a registered-but-
-- inert Subject kind to a real domain crate. The audit found /shop
-- checkouts stuffing the buyer's email/name/phone into Job metadata
-- on a catch-all account — this row is the durable home for the
-- person. Thin by design; purchase history stays derivable from the
-- Jobs/invoices that reference the customer.
--
-- Id convention (R3, one mint authority per kind): when a create
-- carries no id, boss-customers derives `cust-<sha256(email)[..12]>`
-- — deterministic (same buyer, same row, idempotent re-checkout)
-- and PII-free. Sim births pass their own pool ids.
--
-- Write path mirrors boss-campaigns: POST /api/customers lands this
-- row + the subjects identity row + the customers.customer.created
-- outbox event in ONE transaction (#118); the rebuilder reproduces
-- every row from audit_log alone.

CREATE TABLE IF NOT EXISTS customers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    email       TEXT,
    phone       TEXT,
    metadata    JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One row per address: the deterministic id already encodes the
-- email, but the unique index keeps EXPLICIT-id creates from
-- landing a second row for the same person.
CREATE UNIQUE INDEX IF NOT EXISTS customers_email
    ON customers(lower(email)) WHERE email IS NOT NULL;
