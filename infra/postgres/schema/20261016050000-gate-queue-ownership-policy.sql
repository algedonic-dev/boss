-- Existing rows remain explicitly client-owned until a recorded policy
-- declaration enables the complete resident protocol after writer drain.
ALTER TABLE delivery_policy ADD COLUMN gate_queue_owner TEXT NOT NULL DEFAULT 'legacy'
    CHECK (gate_queue_owner IN ('legacy', 'resident'));
