-- aa0f88c1: derive routing identities from the original fact's own
-- full state/receipt, never from today's packet projection. This is
-- an index over existing originals, not historical state backfill.
CREATE FUNCTION original_packet_identity(event_kind TEXT, value JSONB)
RETURNS TEXT LANGUAGE plpgsql IMMUTABLE STRICT AS $$
DECLARE receipt JSONB;
BEGIN
    IF event_kind='jobs.station.claimed' THEN
        IF value ? 'receipt_json' THEN
            BEGIN
                receipt := (value->>'receipt_json')::jsonb;
            EXCEPTION WHEN data_exception THEN
                RETURN NULL;
            END;
            RETURN receipt #>> '{selected,job_id}';
        END IF;
        RETURN value->>'job_id';
    END IF;
    IF event_kind='jobs.step.first_recorded' THEN
        RETURN value #>> '{step,job_id}';
    END IF;
    RETURN COALESCE(value->>'job_id',value->>'id');
END;
$$;
CREATE INDEX audit_original_packet_identity
    ON audit_log(original_packet_identity(kind,payload),id);
CREATE INDEX audit_original_claim_chunk
    ON audit_log((payload->>'claim_event_id'),id)
    WHERE kind='jobs.station.selection_evidence';

-- Old manifest-only claims have no redundant packet field. Resolve
-- their routing identity from bounded original chunks at the same
-- audit cut. This does NOT validate or rewrite their receipt; readers
-- still verify the manifest, bytes, hashes, actor and original group.
CREATE FUNCTION original_claim_chunks(owner_event UUID, cutoff BIGINT, include_pending BOOLEAN)
RETURNS TABLE(id BIGINT,payload JSONB) LANGUAGE SQL STABLE AS $$
    SELECT selected.id,selected.payload FROM (
        SELECT DISTINCT ON(event_id) event_id,id,payload FROM (
            SELECT event_id,id,payload FROM audit_log WHERE id<=cutoff
                AND kind='jobs.station.selection_evidence' AND payload->>'claim_event_id'=owner_event::text
            UNION ALL
            SELECT event_id,id,payload FROM event_outbox WHERE include_pending
                AND kind='jobs.station.selection_evidence' AND payload->>'claim_event_id'=owner_event::text
        ) copies ORDER BY event_id,id
    ) selected LIMIT 4097;
$$;
CREATE FUNCTION original_claim_packet_identity(owner_payload JSONB, owner_event UUID, cutoff BIGINT, include_pending BOOLEAN DEFAULT FALSE)
RETURNS TEXT LANGUAGE plpgsql STABLE STRICT AS $$
DECLARE text_value TEXT; chunk_count BIGINT; byte_count BIGINT;
BEGIN
    IF owner_payload ? 'receipt_json' OR owner_payload ? 'job_id' THEN
        RETURN original_packet_identity('jobs.station.claimed',owner_payload);
    END IF;
    IF NOT owner_payload ? 'receipt_manifest' THEN RETURN NULL; END IF;
    SELECT count(*),sum(octet_length(payload->>'text')) INTO chunk_count,byte_count
        FROM original_claim_chunks(owner_event,cutoff,include_pending);
    IF chunk_count>4096 OR byte_count>67108864 THEN RETURN NULL; END IF;
    SELECT string_agg(payload->>'text','' ORDER BY
            CASE WHEN payload->>'index' ~ '^[0-9]{1,6}$' THEN (payload->>'index')::integer END,id)
        INTO text_value
        FROM original_claim_chunks(owner_event,cutoff,include_pending);
    RETURN original_packet_identity('jobs.station.claimed',jsonb_build_object('receipt_json',text_value));
END;
$$;
