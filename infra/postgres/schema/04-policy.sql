-- =========================================================================
-- 04-policy.sql — Policy / Entitlements — row-level authorization tables.
-- =========================================================================


-- ---------------------------------------------------------------------------
-- Policy / Entitlements tables
-- ---------------------------------------------------------------------------
-- Row-level authorization for every domain service. Every HTTP handler
-- consults boss-policy-client, which reads these tables. See
-- docs/design/entitlements-policy.md for the model.

-- The core rule table. `id` is composed as 'role:resource:action' so
-- lookups are fast and the row is self-describing. `scope` is the
-- serialized Scope enum — 'none' | 'self' | 'territory' | 'team' |
-- 'department:<name>' | 'all'.
CREATE TABLE IF NOT EXISTS policy_rules (
    id            TEXT PRIMARY KEY,
    role          TEXT NOT NULL,
    resource      TEXT NOT NULL,
    action        TEXT NOT NULL,
    scope         TEXT NOT NULL,
    active        BOOLEAN NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by    TEXT,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by    TEXT,
    UNIQUE (role, resource, action)
);


CREATE INDEX IF NOT EXISTS policy_rules_lookup
    ON policy_rules(role, resource, action)
    WHERE active;


-- Per-user exceptions (delegation / coverage / temporary elevation).
-- `expires_at` null = permanent; otherwise the client ignores the
-- row past the expiry.
CREATE TABLE IF NOT EXISTS policy_user_overrides (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    resource      TEXT NOT NULL,
    action        TEXT NOT NULL,
    scope         TEXT NOT NULL,
    reason        TEXT NOT NULL,
    expires_at    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_by    TEXT NOT NULL,
    UNIQUE (user_id, resource, action)
);


-- Queries filter active overrides by (expires_at IS NULL OR expires_at > now)
-- at runtime; a predicate with NOW() is rejected by Postgres because NOW()
-- is STABLE not IMMUTABLE. Covering index on (user_id, expires_at) gives
-- the lookup both columns it needs without a time-dependent predicate.
CREATE INDEX IF NOT EXISTS policy_user_overrides_active
    ON policy_user_overrides(user_id, expires_at);


-- Append-only audit of rule + override changes. Never deleted.
CREATE TABLE IF NOT EXISTS policy_rule_audit (
    id            BIGSERIAL PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN (
        'rule.upsert', 'rule.deactivate',
        'override.upsert', 'override.deactivate'
    )),
    target_id     TEXT NOT NULL,
    changed_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    changed_by    TEXT NOT NULL,
    before        JSONB,
    after         JSONB
);


CREATE INDEX IF NOT EXISTS policy_rule_audit_recent ON policy_rule_audit(changed_at DESC);

CREATE INDEX IF NOT EXISTS policy_rule_audit_target ON policy_rule_audit(target_id);

