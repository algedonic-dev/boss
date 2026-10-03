-- 20261001080134-an-open-ticket-is-keyed-by-its-asset.sql — an open
-- service ticket is one row per (asset, ticket), not per ticket id
-- (backlog b8099caf).
--
-- 21-assets.sql made `ticket_id` the primary key of asset_open_tickets.
-- A ticket id is the caller's: POST /api/assets/events takes the
-- `job_id` of a ServiceJobOpened from the body, nothing mints it, and
-- nothing refuses one service job opening on two assets. The projection
-- counts open tickets PER ASSET, so for such a job PgAssets dropped the
-- second asset's row on conflict and a close on either asset deleted
-- the other's row too — the per-account count read off this table
-- drifted from the per-asset counts (conservation). PgAssets now
-- inserts ON CONFLICT (asset_id, ticket_id) and closes by both.
--
-- Conservation of the rows already here: under the old key every
-- ticket id is unique, so every (asset_id, ticket_id) pair is unique
-- too and the wider key admits every existing row unchanged. The test
-- `the_wider_ticket_key_keeps_every_row_and_count` in
-- crates/modules/boss-assets/tests/projection_persistence.rs rebuilds
-- the old key over a fleet, applies this file, and holds each
-- account's count equal before and after.
--
-- 21-assets.sql is history (migrations are append-only), so the old
-- primary key is replaced here rather than edited there.
ALTER TABLE asset_open_tickets DROP CONSTRAINT IF EXISTS asset_open_tickets_pkey;
ALTER TABLE asset_open_tickets ADD CONSTRAINT asset_open_tickets_pkey
    PRIMARY KEY (asset_id, ticket_id);
