-- aa0f88c1: membership replay needs the ORIGINAL producing transaction,
-- not the relay batch or equal event timestamps. All sibling inserts
-- and their member list commit together. Legacy rows are not backfilled.
CREATE TABLE original_event_groups (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    producer_xid XID8 NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    event_count BIGINT,
    member_hash TEXT,
    CHECK ((event_count IS NULL AND member_hash IS NULL)
        OR (event_count > 0 AND member_hash IS NOT NULL))
);
CREATE TABLE original_event_group_members (
    event_id UUID PRIMARY KEY,
    group_id UUID NOT NULL REFERENCES original_event_groups(id),
    member_seq BIGINT NOT NULL
);
CREATE INDEX original_event_group_members_roster
    ON original_event_group_members(group_id,member_seq);

ALTER TABLE event_outbox ADD COLUMN original_group_id UUID
    REFERENCES original_event_groups(id);
ALTER TABLE audit_log ADD COLUMN original_group JSONB;

-- Only an outbox INSERT trigger may construct the witness, and only
-- inside its original transaction. Later delivery/pruning cannot edit it.
CREATE FUNCTION protect_original_event_group() RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'TRUNCATE' OR TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'original event groups are append-only';
    END IF;
    IF pg_trigger_depth() < 2 OR NEW.producer_xid <> pg_current_xact_id()
       OR (TG_OP = 'UPDATE' AND
           (OLD.producer_xid <> NEW.producer_xid OR OLD.id <> NEW.id
            OR OLD.schema_version <> NEW.schema_version OR OLD.event_count IS NOT NULL)) THEN
        RAISE EXCEPTION 'only the original outbox transaction may construct its group';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER original_event_groups_guard
    BEFORE INSERT OR UPDATE OR DELETE ON original_event_groups
    FOR EACH ROW EXECUTE FUNCTION protect_original_event_group();
CREATE TRIGGER original_event_groups_no_truncate
    BEFORE TRUNCATE ON original_event_groups
    FOR EACH STATEMENT EXECUTE FUNCTION protect_original_event_group();

CREATE FUNCTION capture_original_event_group() RETURNS TRIGGER AS $$
DECLARE group_id UUID;
BEGIN
    -- xid8 is allocated by Postgres, not by a caller or the relay.
    -- Its unique index is local to one DB's immutable evidence lineage.
    SELECT id INTO group_id FROM original_event_groups
        WHERE producer_xid = pg_current_xact_id();
    IF group_id IS NULL THEN
        INSERT INTO original_event_groups(producer_xid)
            VALUES (pg_current_xact_id()) RETURNING id INTO group_id;
    END IF;
    NEW.original_group_id := group_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER event_outbox_original_group
    BEFORE INSERT ON event_outbox
    FOR EACH ROW EXECUTE FUNCTION capture_original_event_group();

CREATE FUNCTION append_original_event_member() RETURNS TRIGGER AS $$
BEGIN
    IF EXISTS(SELECT 1 FROM original_event_groups
        WHERE id=NEW.original_group_id AND event_count IS NOT NULL) THEN
        RAISE EXCEPTION 'cannot append to a sealed original transaction group';
    END IF;
    INSERT INTO original_event_group_members(event_id,group_id,member_seq)
        VALUES(NEW.event_id,NEW.original_group_id,NEW.id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER event_outbox_original_member
    AFTER INSERT ON event_outbox
    FOR EACH ROW EXECUTE FUNCTION append_original_event_member();

CREATE FUNCTION protect_original_event_member() RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP <> 'INSERT' OR pg_trigger_depth() < 2 OR NOT EXISTS(
        SELECT 1 FROM original_event_groups WHERE id=NEW.group_id
        AND producer_xid=pg_current_xact_id() AND event_count IS NULL) THEN
        RAISE EXCEPTION 'original transaction members are append-only producer facts';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER original_event_group_members_guard
    BEFORE INSERT OR UPDATE OR DELETE ON original_event_group_members
    FOR EACH ROW EXECUTE FUNCTION protect_original_event_member();
CREATE TRIGGER original_event_group_members_no_truncate
    BEFORE TRUNCATE ON original_event_group_members
    FOR EACH STATEMENT EXECUTE FUNCTION protect_original_event_member();

-- The deferred producer trigger sees the final committed roster once;
-- following queued sibling triggers only read its already-sealed header.
-- No audit sibling repeats that roster, and no per-row array append
-- rewrites a growing manifest throughout a bulk transaction.
CREATE FUNCTION original_group_member_hash(group_id UUID, member_limit BIGINT DEFAULT NULL) RETURNS TEXT
LANGUAGE SQL STABLE AS $$
    SELECT encode(digest(convert_to(jsonb_agg(event_id ORDER BY member_seq)::text,
        'UTF8'),'sha256'),'hex') FROM (
        SELECT event_id,member_seq FROM original_event_group_members
        WHERE original_event_group_members.group_id=$1 ORDER BY member_seq LIMIT $2
    ) members;
$$;
CREATE FUNCTION seal_original_event_group() RETURNS TRIGGER AS $$
DECLARE roster JSONB; sealed BIGINT;
BEGIN
    SELECT event_count INTO sealed FROM original_event_groups WHERE id=NEW.group_id FOR UPDATE;
    IF sealed IS NOT NULL THEN RETURN NEW; END IF;
    SELECT jsonb_agg(event_id ORDER BY member_seq) INTO roster
        FROM original_event_group_members WHERE group_id=NEW.group_id;
    UPDATE original_event_groups SET event_count=jsonb_array_length(roster),
        member_hash=original_group_member_hash(NEW.group_id)
        WHERE id=NEW.group_id;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE CONSTRAINT TRIGGER original_event_group_seal
    AFTER INSERT ON original_event_group_members DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION seal_original_event_group();

CREATE FUNCTION conserve_original_event_group() RETURNS TRIGGER AS $$
BEGIN
    -- Never accept a caller-authored audit witness. Its source is the
    -- committed producer manifest, still present after outbox retention.
    IF NEW.original_group IS NOT NULL THEN
        RAISE EXCEPTION 'original group must be copied from its producing transaction';
    END IF;
    SELECT jsonb_build_object('id', g.id, 'schema_version', g.schema_version,
                             'event_count',g.event_count,'member_hash',g.member_hash)
        INTO NEW.original_group
        FROM original_event_groups g JOIN original_event_group_members m ON m.group_id=g.id
        WHERE m.event_id=NEW.event_id AND g.event_count IS NOT NULL
          AND g.producer_xid <> pg_current_xact_id();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER audit_log_a_original_group
    BEFORE INSERT ON audit_log
    FOR EACH ROW EXECUTE FUNCTION conserve_original_event_group();

-- One byte definition serves both the writer and verifier. NULL is
-- byte-for-byte legacy encoding; existing chain links never change.
CREATE FUNCTION audit_log_canonical(
    event_id UUID, event_timestamp TIMESTAMPTZ, source TEXT, kind TEXT,
    payload JSONB, original_group JSONB
) RETURNS TEXT LANGUAGE SQL STABLE AS $$
    SELECT COALESCE(event_id::text, '') || '|'
        || COALESCE(event_timestamp::text, '') || '|'
        || COALESCE(source, '') || '|'
        || COALESCE(kind, '') || '|'
        || COALESCE(payload::text, '')
        || CASE WHEN original_group IS NULL THEN ''
                ELSE '|original-group-v1|' || original_group::text END;
$$;

CREATE OR REPLACE FUNCTION audit_log_compute_row_hash() RETURNS TRIGGER AS $$
DECLARE predecessor BYTEA;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('audit_log'));
    NEW.id := nextval('audit_log_id_seq');
    NEW.created_at := clock_timestamp();
    SELECT row_hash INTO predecessor FROM audit_log ORDER BY id DESC LIMIT 1;
    predecessor := COALESCE(predecessor, decode(repeat('00', 32), 'hex'));
    NEW.prev_hash := predecessor;
    NEW.row_hash := digest(predecessor || convert_to(audit_log_canonical(
        NEW.event_id, NEW.timestamp, NEW.source, NEW.kind, NEW.payload,
        NEW.original_group), 'UTF8'), 'sha256');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
