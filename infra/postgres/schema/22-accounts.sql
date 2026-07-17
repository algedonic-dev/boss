-- =========================================================================
-- 22-accounts.sql — Accounts — customer directory, contacts, account team, notes, support cases, account facts.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Accounts (customer directory)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS accounts (
    id                TEXT PRIMARY KEY,
    -- Identity-first: only `id` is required. Descriptive fields are
    -- nullable and enriched after the account exists (a prospect can be
    -- opened from an id alone). `tier` is plain TEXT — no DB CHECK;
    -- adding a tier is a Class-registry row under (subject_kind='account',
    -- member_attribute='tier'), validated in the HTTP handler. NULL stays
    -- legal (untiered until classified).
    name              TEXT,
    director          TEXT,
    city              TEXT,
    state             TEXT,
    tier              TEXT,
    customer_since    DATE,
    territory_rep_id  TEXT,
    -- Tenant-extensible account discriminator. Free-text validated
    -- by the Class registry under (subject_kind='account',
    -- member_attribute='type'). Brewery: wholesale-distributor /
    -- bar-restaurant / chain-retail / corporate-event. Used-device-shop:
    -- service-account / lab / clinic. Defaults to 'unspecified' until
    -- the account is classified.
    account_type      TEXT NOT NULL DEFAULT 'unspecified'
);


CREATE INDEX IF NOT EXISTS accounts_tier ON accounts(tier);


CREATE TABLE IF NOT EXISTS account_contacts (
    id                TEXT PRIMARY KEY,
    account_id         TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    -- Free-text per-tenant taxonomy. Brewery uses
    -- buyer / cellar-mgr / events-coord / accounts-payable;
    -- the used-device-shop tenant uses decision-maker /
    -- office-manager / clinical-lead / billing / purchasing.
    -- Class registry validates per-tenant.
    role              TEXT NOT NULL,
    email             TEXT NOT NULL,
    phone             TEXT,
    is_primary        BOOLEAN NOT NULL DEFAULT false,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS account_contacts_account ON account_contacts(account_id);


-- -----------------------------------------------------------------------------
-- CRM: account account team + notes/interactions log
--
-- Both tables are part of the unified account detail view (see
-- docs/architecture-decisions.md §Step UX & frontend). The account team layers a
-- customer-success assignment on top of the existing
-- accounts.territory_rep_id sales-owner relationship without touching it.
-- The notes table is the "who talked to whom about what" feed.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS account_team_members (
    id            TEXT PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    employee_id   TEXT NOT NULL REFERENCES employees(id),
    -- Account-team roles, including `territory-rep`: this join table is
    -- the canonical source of truth for every account-team
    -- relationship. `accounts.territory_rep_id` is a denormalised cache
    -- (fast-path for scope queries) mirrored here on every account
    -- write. role is a Class code in `(subject_kind='employee',
    -- member_attribute='account_team_role')` — no DB CHECK, so adding a
    -- role is a Class-registry row, not a schema migration. Validation
    -- runs in the HTTP handler against the registry; the database
    -- trusts the column.
    role          TEXT NOT NULL,
    assigned_on   DATE NOT NULL DEFAULT CURRENT_DATE,
    notes         TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (account_id, role)  -- exactly one person per role per account
);


CREATE INDEX IF NOT EXISTS account_team_members_account
    ON account_team_members(account_id);

CREATE INDEX IF NOT EXISTS account_team_members_employee
    ON account_team_members(employee_id);


-- Drop any legacy role CHECK so the registry-validated column stays
-- unconstrained at the database (idempotent).
ALTER TABLE account_team_members
    DROP CONSTRAINT IF EXISTS clinic_account_team_role_check;

ALTER TABLE account_team_members
    DROP CONSTRAINT IF EXISTS account_team_members_role_check;


CREATE TABLE IF NOT EXISTS account_notes (
    id            TEXT PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    actor_id     TEXT NOT NULL REFERENCES employees(id),
    -- Note kind. No DB CHECK — adding a kind is a Class-registry row
    -- under (subject_kind='account', member_attribute='note-kind'),
    -- validated in the HTTP handler (`check_note_kind`); the database
    -- trusts the column. Seeded kinds:
    -- note / call / meeting / email / interaction.
    kind          TEXT NOT NULL,
    body          TEXT NOT NULL,
    occurred_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Soft delete. Set when a user clicks delete in the UI; the row
    -- stays queryable by operator tools. Hard deletion is reserved
    -- for a CLI escape hatch behind operator-tier elevation.
    deleted_at    TIMESTAMPTZ,
    deleted_by    TEXT REFERENCES employees(id)
);


-- Partial index keeps the normal "show me notes for this account"
-- query fast while soft-deleted rows sit in the heap for audit.
CREATE INDEX IF NOT EXISTS account_notes_account_time
    ON account_notes(account_id, occurred_at DESC)
    WHERE deleted_at IS NULL;


-- Account facts — accumulated from Jobs/Steps involving accounts.
CREATE TABLE IF NOT EXISTS account_facts (
    id              TEXT PRIMARY KEY,
    account_id     TEXT NOT NULL,
    fact_kind       TEXT NOT NULL,
    occurred_at     DATE NOT NULL,
    actor_id        TEXT,
    job_id          TEXT,
    step_id         TEXT,
    payload         JSONB NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS account_facts_account ON account_facts(account_id, occurred_at DESC);

CREATE INDEX IF NOT EXISTS account_facts_kind ON account_facts(fact_kind);


-- -----------------------------------------------------------------------------
-- Support cases (boss-people)
--
-- First-class support-case entity, distinct from device-event-driven
-- service tickets. Support cases originate from account contact
-- (phone call, email, chat) and are worked by Support Specialists.
-- The sim's support generator (generators/support.rs) creates these
-- via Poisson-sampled daily traffic per account and advances them
-- through an open → assigned → resolved lifecycle.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS support_cases (
    id              TEXT PRIMARY KEY,
    account_id       TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    channel         TEXT NOT NULL CHECK (channel IN (
        'phone', 'email', 'chat'
    )),
    -- Free-text per-tenant taxonomy. Brewery uses
    -- delivery-issue / tasting-request / billing / account;
    -- the used-device-shop tenant uses device-question /
    -- device-issue / billing / training-request / account.
    -- Class registry validates per-tenant.
    category        TEXT NOT NULL,
    subject         TEXT NOT NULL,
    body            TEXT NOT NULL,
    opened_on       DATE NOT NULL,
    assignee_id     TEXT REFERENCES employees(id),
    status          TEXT NOT NULL CHECK (status IN (
        'open', 'assigned', 'in-progress', 'resolved', 'cancelled'
    )) DEFAULT 'open',
    resolved_on     DATE,
    resolution_notes TEXT,
    -- CSAT score 1-5 captured when the case resolves. Nullable
    -- because not every resolved case gets a survey response.
    csat            SMALLINT CHECK (csat IS NULL OR (csat BETWEEN 1 AND 5)),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS support_cases_account ON support_cases(account_id);

CREATE INDEX IF NOT EXISTS support_cases_status ON support_cases(status);

CREATE INDEX IF NOT EXISTS support_cases_assignee ON support_cases(assignee_id)
    WHERE assignee_id IS NOT NULL;

-- Accounts subject-edge seed rows: commerce events that REFERENCE an
-- existing account Subject. R2 — resolved against `subjects`
-- (kind='account') by the subject_edges trigger in 02-events, not the
-- old per-table ref-check. Every account is a Subject (R1), so the
-- resolution is equivalent to the retired `accounts.id` check and
-- also survives an epoch rollover's reprojection.
INSERT INTO subject_edges (source_kind, field_path, target_kind) VALUES
    ('commerce.invoice.created',      'account_id',  'account'),
    ('commerce.invoice.paid',         'account_id',  'account'),
    ('commerce.invoice.past_due',     'account_id',  'account'),
    ('commerce.invoice.written_off',  'account_id',  'account')
ON CONFLICT (source_kind, field_path) DO NOTHING;

