-- ba0b7308, 2026-10-29 (design f29798c0 §7, car 1): a node declares its
-- physical drives beside its disk capacity. The WAL stalls of that day
-- were diagnosed from the drive CLASS — DRAM-less OEM client NVMe under
-- etcd and the database on cp-1 and cp-2 — and the registry held each
-- node's disk size and nothing about what the disk is, so the models
-- were read through talos-get five times and copied into a packet
-- (d9b39685). Declared hardware, like disk_gb: the tree's
-- infra/estate/estate.toml, refreshed on a kept node only by an explicit
-- list. A list of {model, size_bytes}; empty is "declares none".
ALTER TABLE nodes ADD COLUMN IF NOT EXISTS disks JSONB NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(disks) = 'array');

UPDATE event_kinds
SET description = 'A tree estate declaration inserted a node, appended roles, or refreshed explicit disk capacity or declared disks. The payload retains the request and declared_by; landed.node and landed.roles identify actual inserts, optional landed.disk_gb holds the actual before/after capacity change, and optional landed.disks the before/after list of declared drives. Requested fields without a landed marker must not be replayed as updates. No fact for a no-op.'
WHERE kind_pattern = 'node.declared' AND source = 'jobs';
