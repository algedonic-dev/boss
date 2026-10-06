-- 20261001092328-a-valueless-consume-is-guarded-by-its-delivery.sql —
-- a consume that drained no value records its proof of delivery
-- (backlog 55f69172).
--
-- `consume_part_at` guards a redelivered consume by the fact it wrote
-- under the delivery's source_id. A consume of a row holding no value
-- drains no cents and so wrote no `finance.inventory.transferred`
-- fact: nothing for the guard to find, and a redelivery took the units
-- a second time, on Postgres and on the double alike. Such a consume
-- now writes the GL-inert `finance.inventory.consumed` marker keyed
-- `(inventory_consume, source_id)` in its own transaction, and records
-- the marker's payload as its rebuild source:
--
--   * `inventory.item.consume_recorded` — part_sku, qty, source_id,
--     consumed_on (the marker's happened_on).
--
-- Emitted by boss-inventory (source `inventory`); reprojected into the
-- inert fact by boss-ledger's rebuild_facts, beside the goods-receipt's
-- `inventory.item.received`. Declared in the same car that first emits
-- it: an emitted-but-undeclared kind is the defect class the audit
-- integrity check exists to catch.
INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain) VALUES
  ('inventory.item.consume_recorded', 'inventory', 'A consume that drained no value was applied (its GL-inert proof of delivery, the guard a redelivery is refused by)', NULL)
ON CONFLICT (kind_pattern) DO NOTHING;
