-- 153-excise-rate-schedules.sql — graduated excise-tax rates as
-- registry data.
--
-- Origin: docs/design/brewery-fidelity.md, Q4 decided by David in the
-- design review (packet 4a39c1df, 2026-08-22): "Registry data.
-- Graduated TTB tiers and state rates land as versionable registry
-- rows the accrual handler reads." The audit that bought this: the
-- flat $3.50/bbl rule arg understates the tenant's excise liability
-- ~3.7× at its stated ~262k bbl/yr, because the real TTB curve
-- (26 USC 5051) is $3.50/bbl only for the first 60,000 bbl of the
-- calendar year and $16.00/bbl above.
--
-- Same reference-data posture as `tax_kinds` (40-ledger.sql): a new
-- tax regime is a row here, not a code change. Versioning follows the
-- registry convention — append a row with a later `effective_from`
-- rather than mutating history; resolution picks the newest row whose
-- `effective_from` is on or before the accrual's posted_on date.
--
-- `tiers` is an ordered JSONB list of bands:
--   [{"up_to_bbl": 60000,   "rate_cents_per_bbl": 350},
--    {"up_to_bbl": 6000000, "rate_cents_per_bbl": 1600}]
-- `up_to_bbl` is the inclusive cumulative calendar-year bound; only
-- the last tier may omit it (unbounded). Shape is validated by the
-- ledger's PUT endpoint (boss-ledger::excise::validate_tiers) — the
-- same place the tier walk lives — so the constraint has one home.
--
-- The platform ships NO rows: rates are regulator data, so they are
-- tenant seeds (the brewery seeds US-FEDERAL via `prepare`, from
-- examples/brewery/seeds/excise_rates.toml). A jurisdiction with no
-- row falls back to the dispatcher rule's flat `rate_cents_per_bbl`
-- arg, loudly, so existing rules.toml deployments keep working.

CREATE TABLE IF NOT EXISTS excise_rate_schedules (
    jurisdiction   TEXT NOT NULL,          -- 'US-FEDERAL', 'US-OR', ...
    effective_from DATE NOT NULL,
    tiers          JSONB NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (jurisdiction, effective_from)
);
