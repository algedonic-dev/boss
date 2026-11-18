-- Delivery position is transport state, not a simulation/event timestamp.
ALTER TABLE dispatcher_rules ADD COLUMN delivery_lane TEXT;

CREATE TABLE dispatcher_delivery_lanes (
    name TEXT PRIMARY KEY,
    receipt JSONB NOT NULL,
    event_id UUID NOT NULL DEFAULT gen_random_uuid() UNIQUE,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One observation may be redelivered without becoming another persistence sample.
CREATE TABLE estate_comparison_receipts (
    observation_id UUID PRIMARY KEY,
    event_id UUID NOT NULL UNIQUE,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE FUNCTION refuse_delivery_receipt_rewrite() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'delivery receipts are immutable; no cutover or observation identity may be rewritten';
END;
$$;
CREATE TRIGGER dispatcher_delivery_lane_immutable BEFORE UPDATE OR DELETE ON dispatcher_delivery_lanes
FOR EACH ROW EXECUTE FUNCTION refuse_delivery_receipt_rewrite();

-- The first activation freezes the exact active membership/version. The
-- table lock serializes this snapshot with ordinary authoring/seed writes.
CREATE FUNCTION validate_delivery_lane_membership() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actual JSONB;
BEGIN
    LOCK TABLE dispatcher_rules IN SHARE MODE;
    SELECT COALESCE(jsonb_object_agg(name, version), '{}'::jsonb) INTO actual
      FROM dispatcher_rules WHERE status = 'active' AND delivery_lane = NEW.name;
    IF actual = '{}'::jsonb OR actual IS DISTINCT FROM NEW.receipt->'members'
       OR NEW.name IS DISTINCT FROM NEW.receipt->>'lane' THEN
        RAISE EXCEPTION 'delivery lane membership/version differs from its active registry snapshot';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER dispatcher_delivery_lane_membership BEFORE INSERT ON dispatcher_delivery_lanes
FOR EACH ROW EXECUTE FUNCTION validate_delivery_lane_membership();

CREATE FUNCTION freeze_bound_delivery_rule() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' AND EXISTS (
        SELECT 1 FROM dispatcher_delivery_lanes WHERE receipt->'members' ? OLD.name
        AND (receipt->'members'->>OLD.name)::integer = OLD.version
    ) THEN
        IF TG_OP = 'DELETE' THEN
            RAISE EXCEPTION 'bound delivery rule membership/version is immutable';
        ELSIF NEW IS DISTINCT FROM OLD THEN
            RAISE EXCEPTION 'bound delivery rule membership/version is immutable';
        END IF;
    END IF;
    IF TG_OP <> 'DELETE' AND NEW.status = 'active' AND NEW.delivery_lane IS NOT NULL
       AND EXISTS (SELECT 1 FROM dispatcher_delivery_lanes WHERE name = NEW.delivery_lane)
       AND NOT EXISTS (
           SELECT 1 FROM dispatcher_delivery_lanes WHERE name = NEW.delivery_lane
           AND receipt->'members'->>NEW.name = NEW.version::text
       ) THEN
        RAISE EXCEPTION 'a new member/version cannot inherit an existing delivery floor';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER dispatcher_bound_rule_immutable BEFORE INSERT OR UPDATE OR DELETE ON dispatcher_rules
FOR EACH ROW EXECUTE FUNCTION freeze_bound_delivery_rule();

INSERT INTO event_kinds (kind_pattern, source, description, suffix_domain)
VALUES ('dispatcher.delivery_lane.bound', 'dispatcher',
        'A new-only delivery consumer and its immutable registry membership share one committed floor',
        NULL);
CREATE TRIGGER estate_comparison_receipt_immutable BEFORE UPDATE OR DELETE ON estate_comparison_receipts
FOR EACH ROW EXECUTE FUNCTION refuse_delivery_receipt_rewrite();
