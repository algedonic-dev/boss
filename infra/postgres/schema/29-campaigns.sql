-- boss-campaigns — marketing campaigns / launches / promos.
--
-- Q4 of docs/design/subject-identity-and-relationships.md (approved
-- 2026-07-15): campaign graduates from an identity-rows-only kind to
-- a real domain crate. Deliberately THIN — the audit found campaigns
-- referenced by tap-launch Jobs and marketing_assets.linked_campaign_ids
-- with no home; this row is that home. Attributes accrue when the
-- marketing flows need them (adaptability-first, no just-in-case
-- columns).
--
-- Write path (boss-campaigns, hexagonal): POST /api/campaigns inserts
-- this row + the subjects identity row + the campaigns.campaign.created
-- outbox event in ONE transaction (the #118 transactional-outbox
-- pattern — campaigns is the first domain crate born onto it).
-- Rebuild: boss-campaigns' rebuilder reproduces every row from
-- audit_log alone.

CREATE TABLE IF NOT EXISTS campaigns (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    -- Tenant-defined lifecycle ('active', 'ended', ...). Free-text;
    -- the Class registry validates per-tenant when a taxonomy lands.
    status      TEXT NOT NULL DEFAULT 'active',
    starts_on   DATE,
    ends_on     DATE,
    -- Free-form: channel, budget_cents, beer sku, ...
    metadata    JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS campaigns_status ON campaigns(status);
