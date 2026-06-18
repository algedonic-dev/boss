-- =========================================================================
-- 10-people.sql — People (FOUNDATION) — employees, skills, certs, requisitions, HR changes, WebAuthn.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- People (employees, org chart, certifications, requisitions)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS employees (
    id                  TEXT PRIMARY KEY,
    -- Identity-first: only `id` is required (a record can be opened at
    -- offer-acceptance before HR details are finalized). Descriptive
    -- columns are nullable and enriched as onboarding proceeds.
    name                TEXT,
    email               TEXT,
    -- Validated by the Class registry (subject_kind = 'employee',
    -- member_attribute = 'role') in boss-people-api on write — no DB
    -- CHECK, so a tenant adds a role without a schema migration. See
    -- docs/design/class-registry.md.
    role                TEXT,
    -- Validated by the Class registry (subject_kind = 'employee',
    -- member_attribute = 'department') in boss-people-api; no DB CHECK.
    -- See docs/design/class-registry.md.
    department          TEXT,
    -- Numeric range, not a closed enum; CHECK stays. Not a Class
    -- registry candidate.
    skill_level         SMALLINT CHECK (skill_level IS NULL OR skill_level BETWEEN 1 AND 5),
    hire_date           DATE,
    -- FK into the Locations registry. boss-people-api also validates
    -- `location_exists(id)` against the locations service before commit
    -- (docs/architecture-decisions.md §Locations).
    location            TEXT REFERENCES locations(id),
    manager_id          TEXT REFERENCES employees(id),
    -- Validated by the Class registry (subject_kind = 'employee',
    -- member_attribute = 'employment_type') in boss-people-api; no DB
    -- CHECK. See docs/design/class-registry.md.
    employment_type     TEXT,
    -- Validated by the Class registry (subject_kind = 'employee',
    -- member_attribute = 'status') in boss-people-api; no DB CHECK.
    -- See docs/design/class-registry.md.
    status              TEXT,
    -- Per-employee annual gross compensation in cents. Nullable; a row
    -- with NULL salary is skipped by the payroll run so a missing value
    -- never produces a zero paycheck
    -- (docs/architecture-decisions.md §Simulator).
    annual_salary_cents BIGINT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS employees_department ON employees(department);

CREATE INDEX IF NOT EXISTS employees_role ON employees(role);

CREATE INDEX IF NOT EXISTS employees_manager ON employees(manager_id) WHERE manager_id IS NOT NULL;

-- Email uniqueness — the OSS quickstart auth keys credentials by
-- email, and Authelia / OIDC will too. Enforced here AND in
-- boss-people-api (validate_email), so a bad seed fails loudly at
-- load instead of at first login.
CREATE UNIQUE INDEX IF NOT EXISTS employees_email_unique
    ON employees (LOWER(email));

-- Platform-operator rows (the humans running the deployment) are
-- deliberately NOT seeded here. They seed through the audit log:
-- `boss-operator-baseline-seed` (binary at
-- crates/boss-people/src/bin/boss_operator_baseline_seed.rs) emits the
-- canonical `people.employee.created` events from
-- `infra/operator-baseline/operator_hires.toml`, and the rebuilder
-- materializes the projection rows from those events — same shape as
-- every other employee, no schema-side bypass. This is what lets them
-- survive `boss-rebuild-all` (which TRUNCATEs employees CASCADE then
-- replays from audit_log): a schema-INSERTed row would have no event
-- to replay from and vanish.
--
-- Bootstrap orchestration is in `infra/postgres/bootstrap-db.sh`; the
-- `--init` mode runs `boss-operator-baseline-seed` after the schema
-- apply so a fresh DB lands these rows via the audit-log-rooted path.

CREATE TABLE IF NOT EXISTS employee_skills (
    employee_id     TEXT NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    skill           TEXT NOT NULL,
    PRIMARY KEY (employee_id, skill)
);


CREATE TABLE IF NOT EXISTS employee_certifications (
    id              BIGSERIAL PRIMARY KEY,
    employee_id     TEXT NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    issuing_body    TEXT NOT NULL,
    issued_on       DATE NOT NULL,
    expires_on      DATE
);


CREATE INDEX IF NOT EXISTS employee_certifications_emp ON employee_certifications(employee_id);


CREATE TABLE IF NOT EXISTS requisitions (
    id                  TEXT PRIMARY KEY,
    role                TEXT NOT NULL,
    department          TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN (
        'open', 'interviewing', 'offer-out', 'filled', 'closed'
    )),
    opened_on           DATE NOT NULL,
    target_fill_date    DATE NOT NULL,
    -- FK into the Locations registry, same treatment as
    -- employees.location.
    location            TEXT NOT NULL REFERENCES locations(id),
    headcount           SMALLINT NOT NULL DEFAULT 1,
    hiring_manager_id   TEXT NOT NULL REFERENCES employees(id),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


-- -----------------------------------------------------------------------------
-- HR workflows (onboarding / offboarding)
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS employee_changes (
    id              BIGSERIAL PRIMARY KEY,
    employee_id     TEXT NOT NULL REFERENCES employees(id),
    kind            TEXT NOT NULL CHECK (kind IN (
        'onboard', 'offboard', 'role-change', 'department-change',
        'leave-start', 'leave-end', 'promotion', 'transfer'
    )),
    from_value      TEXT,
    to_value        TEXT,
    effective_date  DATE NOT NULL,
    notes           TEXT,
    initiated_by    TEXT REFERENCES employees(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);


CREATE INDEX IF NOT EXISTS employee_changes_emp ON employee_changes(employee_id, created_at DESC);


-- =========================================================================
-- Access control — WebAuthn credentials + access tiers
-- =========================================================================

-- FIDO2 / WebAuthn hardware key credentials.
-- Each employee can register multiple keys. The access_tier determines
-- what operations the key unlocks:
--   operator — full system access (CLI, agents, direct DB, SSH)
--   user     — frontend-only (workbenches, dashboards, curated experience)
CREATE TABLE IF NOT EXISTS webauthn_credentials (
  id              BIGSERIAL PRIMARY KEY,
  employee_id     TEXT NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
  credential_id   BYTEA NOT NULL UNIQUE,
  public_key      BYTEA NOT NULL,
  sign_count      INTEGER NOT NULL DEFAULT 0,
  label           TEXT NOT NULL DEFAULT 'default',
  access_tier     TEXT NOT NULL CHECK (access_tier IN ('operator', 'user')) DEFAULT 'user',
  registered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  last_used_at    TIMESTAMPTZ
);


CREATE INDEX IF NOT EXISTS webauthn_credentials_employee ON webauthn_credentials(employee_id);


-- Challenges for WebAuthn registration and authentication flows.
-- Short-lived (5 min TTL), cleaned up by the gateway.
CREATE TABLE IF NOT EXISTS webauthn_challenges (
  id              TEXT PRIMARY KEY,
  employee_id     TEXT,
  challenge       BYTEA NOT NULL,
  flow            TEXT NOT NULL CHECK (flow IN ('register', 'authenticate')),
  created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  expires_at      TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '5 minutes'
);

