-- =========================================================================
-- 28-scheduling.sql — Scheduling — field-service tech availability, assignments, shift patterns, ICS tokens.
-- =========================================================================


-- -----------------------------------------------------------------------------
-- Field-service scheduling — tech availability, assignments, shift
-- patterns, ICS tokens. Scheduling is a cross-Job concern, so it gets
-- its own tables (not step metadata). One table per concept;
-- range-queryable; FK'd to employees + jobs.
-- -----------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS tech_availability (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id    TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN (
        'available', 'pto', 'sick', 'holiday', 'training', 'blocked'
    )),
    starts_at      TIMESTAMPTZ NOT NULL,
    ends_at        TIMESTAMPTZ NOT NULL,
    notes          TEXT,
    source         TEXT NOT NULL DEFAULT 'manual'
                   CHECK (source IN ('manual', 'shift-pattern', 'import')),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS tech_availability_emp_range
    ON tech_availability (employee_id, starts_at, ends_at);

CREATE INDEX IF NOT EXISTS tech_availability_range
    ON tech_availability (starts_at, ends_at);


CREATE TABLE IF NOT EXISTS scheduled_assignments (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tech_id        TEXT NOT NULL,
    target_job_id  UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN (
        'wo', 'pm', 'training', 'diag-call', 'travel', 'install'
    )),
    starts_at      TIMESTAMPTZ NOT NULL,
    ends_at        TIMESTAMPTZ NOT NULL,
    status         TEXT NOT NULL CHECK (status IN (
        'tentative', 'confirmed', 'completed', 'cancelled', 'no-show'
    )),
    notes          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS scheduled_assignments_tech_range
    ON scheduled_assignments (tech_id, starts_at, ends_at);

CREATE INDEX IF NOT EXISTS scheduled_assignments_job
    ON scheduled_assignments (target_job_id);

CREATE INDEX IF NOT EXISTS scheduled_assignments_range
    ON scheduled_assignments (starts_at, ends_at)
    WHERE status IN ('tentative', 'confirmed');


CREATE TABLE IF NOT EXISTS tech_shift_patterns (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    employee_id     TEXT NOT NULL,
    day_of_week     SMALLINT NOT NULL CHECK (day_of_week BETWEEN 0 AND 6),
    starts_at_time  TIME NOT NULL,
    ends_at_time    TIME NOT NULL,
    timezone        TEXT NOT NULL DEFAULT 'America/Los_Angeles',
    effective_from  DATE NOT NULL DEFAULT CURRENT_DATE,
    effective_to    DATE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (ends_at_time > starts_at_time),
    UNIQUE (employee_id, day_of_week, effective_from)
);


-- Per-tech ICS calendar-feed tokens. One active token per employee;
-- rotating generates a new random string, revoking the old URL. The
-- URL itself is the authentication — generated tokens are 32 bytes of
-- crypto randomness base64url-encoded, which gives enough entropy to
-- reject brute-force guesses without rate-limiting. Public ICS reads
-- at /ics/{token}.ics join back to this row to find the employee.
CREATE TABLE IF NOT EXISTS tech_calendar_tokens (
    employee_id     TEXT PRIMARY KEY,
    token           TEXT NOT NULL UNIQUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS tech_calendar_tokens_lookup
    ON tech_calendar_tokens (token);

