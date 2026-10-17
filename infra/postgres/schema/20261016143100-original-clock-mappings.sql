-- aa0f88c1: parameters are originals only when their producer records
-- them. No migration backfills today's sim_clock as past clock evidence.
CREATE TABLE clock_mapping_mode (
    id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    mode TEXT NOT NULL CHECK (mode IN ('wall', 'sim')),
    epoch UUID NOT NULL
);
CREATE TABLE clock_mapping_head (
    id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    revision UUID
);
INSERT INTO clock_mapping_head(id) VALUES (1);

CREATE FUNCTION original_clock_params(value sim_clock) RETURNS JSONB
LANGUAGE SQL STABLE AS $$
    SELECT CASE WHEN value IS NULL THEN NULL ELSE jsonb_build_object(
        'epoch_start', value.epoch_start_date,
        'epoch_end', value.epoch_end_date,
        'warp_factor', value.warp_factor,
        'wall_anchor', value.wall_anchor,
        'paused', value.paused,
        'paused_at', value.paused_at,
        'paused_offset_seconds', value.paused_offset_seconds,
        'restart_in_progress', value.restart_in_progress) END;
$$;

CREATE FUNCTION capture_original_clock_mapping() RETURNS TRIGGER AS $$
DECLARE
    current_mode TEXT;
    current_epoch UUID;
    previous_revision UUID;
    next_revision UUID := gen_random_uuid();
    params JSONB;
    prior JSONB;
    captured_at TIMESTAMPTZ;
BEGIN
    -- This lock conserves the order of mode and direct parameter
    -- writers, without treating equal timestamps as a boundary.
    SELECT revision INTO previous_revision FROM clock_mapping_head
        WHERE id=1 FOR UPDATE;
    captured_at := clock_timestamp();
    IF TG_TABLE_NAME = 'sim_clock' THEN
        params := CASE WHEN TG_OP IN ('DELETE','TRUNCATE') THEN NULL ELSE original_clock_params(NEW) END;
        IF TG_OP='TRUNCATE' THEN
            SELECT original_clock_params(s) INTO prior FROM sim_clock s WHERE id=1;
        ELSE
            prior := CASE WHEN TG_OP='INSERT' THEN NULL ELSE original_clock_params(OLD) END;
        END IF;
        SELECT mode,epoch INTO current_mode,current_epoch FROM clock_mapping_mode WHERE id=1;
    ELSE
        SELECT original_clock_params(s) INTO params FROM sim_clock s WHERE id=1;
        current_mode := CASE WHEN TG_OP IN ('DELETE','TRUNCATE') THEN NULL ELSE NEW.mode END;
        current_epoch := CASE WHEN TG_OP IN ('DELETE','TRUNCATE') THEN NULL ELSE NEW.epoch END;
        prior := NULL;
    END IF;
    INSERT INTO event_outbox(event_id,timestamp,source,kind,payload)
        VALUES(next_revision,captured_at,'clock','clock.mapping.recorded',
            jsonb_build_object('schema_version',1,'revision',next_revision,
                'previous_revision',previous_revision,'mode',current_mode,
                'epoch',current_epoch,'params',params,'before',prior,
                'captured_at',captured_at,'writer_table',TG_TABLE_NAME,
                'operation',TG_OP));
    UPDATE clock_mapping_head SET revision=next_revision WHERE id=1;
    IF TG_OP='TRUNCATE' THEN RETURN NULL; END IF;
    RETURN COALESCE(NEW,OLD);
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER sim_clock_original_mapping
    AFTER INSERT OR UPDATE OR DELETE ON sim_clock
    FOR EACH ROW EXECUTE FUNCTION capture_original_clock_mapping();
CREATE TRIGGER clock_mode_original_mapping
    AFTER INSERT OR UPDATE OR DELETE ON clock_mapping_mode
    FOR EACH ROW EXECUTE FUNCTION capture_original_clock_mapping();
CREATE TRIGGER sim_clock_original_truncate
    BEFORE TRUNCATE ON sim_clock
    FOR EACH STATEMENT EXECUTE FUNCTION capture_original_clock_mapping();
CREATE TRIGGER clock_mode_original_truncate
    BEFORE TRUNCATE ON clock_mapping_mode
    FOR EACH STATEMENT EXECUTE FUNCTION capture_original_clock_mapping();

INSERT INTO event_kinds(kind_pattern,source,description,payload_fields)
    VALUES('clock.mapping.recorded','clock',
        'Original clock mode/epoch/parameter revision; activation is bounded by capture and audit insertion',
        '[{"name":"schema_version","type":"integer"},
          {"name":"revision","type":"uuid"},
          {"name":"previous_revision","type":"uuid-or-null"},
          {"name":"mode","type":"string-or-null"},
          {"name":"epoch","type":"uuid-or-null"},
          {"name":"params","type":"object-or-null"},
          {"name":"before","type":"object-or-null"},
          {"name":"captured_at","type":"timestamp"},
          {"name":"writer_table","type":"string"},
          {"name":"operation","type":"string"}]');
