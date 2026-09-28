-- 20260927080317-an-asset-parts-write-leaves-a-fact.sql — the two
-- asset Parts writes record what they wrote.
--
-- Origin: backlog d2bea664, an adversarial review of the 6c0f6547 car
-- (2026-09-26). POST /api/assets/assets/{id}/software-config and
-- POST /api/assets/assets/{id}/accessories took no caller, asked no
-- policy, and ran a bare INSERT on the pool: no event reached the
-- outbox, so the log could not say who changed an asset's firmware or
-- attached an accessory, or that anyone had. Both were named `gap` in
-- crates/core/boss-events/writes-without-a-fact.txt when that pin
-- landed (8c271e8f, mechanism A of design 3036296f).
--
-- Each now asks Update on asset and stages ONE fact in the write's own
-- transaction, carrying the row as the write left it (RETURNING), so
-- the payload is value-primary. The kinds sit under `assets.`, not
-- `asset.`: the assets API's NATS ingress subscribes to `asset.>` and
-- decodes every message there as an AssetEvent, which these are not.
--
-- Declared here so the kinds do not ride inside a passing
-- audit-integrity run unread (infra/lint/emitted-kinds-are-declared.sh).

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('assets.software_config.upserted', 'assets', 'An asset''s software config was set (POST /api/assets/assets/{id}/software-config): the row as written — asset_id, firmware_version, modules, license_tier, last_updated_on, updated_at — signed by the caller', NULL),
  ('assets.accessory.attached', 'assets', 'An accessory was attached to an asset (POST /api/assets/assets/{id}/accessories): the row as written — id, asset_id, accessory_kind, serial, installed_on, removed_on, notes — signed by the caller', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
