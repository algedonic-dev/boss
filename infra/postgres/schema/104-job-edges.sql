-- 104-job-edges.sql — job-to-job links become data
-- (docs/design/department-flow-dashboards.md Q1, decided 2026-08-09:
-- registry, the subject_edges shape).
--
-- The department's inter-workflow topology was folklore: job
-- metadata keys (`backlog_item`, `train`, `boarded_jobs`) that
-- reference other Jobs, declared nowhere, drawable only by
-- hardcoding key names. This registry declares them, so instruments
-- derive the topology and the write path can check link integrity.
--
-- MEASURED REALITY the defaults encode (2026-08-09 live audit): the
-- folklore is dirty — `train` carries full Job UUIDs, but
-- `backlog_item` / `boarded_jobs` values are mostly 8-char id
-- PREFIXES (14 of ~15 dangle under exact match). Resolution is
-- therefore prefix-aware (an unambiguous prefix of length >= 8
-- resolves), and `on_missing` defaults to 'warn' — visibility
-- without breaking the writers that built the folklore. Tightening
-- to 'abort' is a one-row UPDATE per edge once the values are
-- cleaned, the same dial subject_edges has.
CREATE TABLE IF NOT EXISTS job_edges (
    source_kind TEXT NOT NULL,
    field_path  TEXT NOT NULL,
    -- 'job_id' | 'job_id_list' — boarded_jobs is a JSON array.
    field_kind  TEXT NOT NULL DEFAULT 'job_id'
        CHECK (field_kind IN ('job_id', 'job_id_list')),
    on_missing  TEXT NOT NULL DEFAULT 'warn'
        CHECK (on_missing IN ('warn', 'abort')),
    description TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (source_kind, field_path)
);

INSERT INTO job_edges (source_kind, field_path, field_kind, description) VALUES
  ('ship-a-change', 'backlog_item', 'job_id',
   'The backlog/feedback Job this change answers'),
  ('ship-a-change', 'train',        'job_id',
   'The pr-train Job this change boarded'),
  ('pr-train',      'boarded_jobs', 'job_id_list',
   'The ship-a-change passengers this train carried')
ON CONFLICT (source_kind, field_path) DO NOTHING;

-- Resolve one candidate value against jobs.id: exact match, else an
-- unambiguous prefix of length >= 8. Returns true when it resolves.
CREATE OR REPLACE FUNCTION job_edge_resolves(candidate TEXT)
RETURNS BOOLEAN AS $$
DECLARE
    n BIGINT;
BEGIN
    IF candidate IS NULL OR candidate = '' THEN
        RETURN TRUE; -- absent ref = no claim to check
    END IF;
    IF EXISTS (SELECT 1 FROM jobs WHERE id::text = candidate) THEN
        RETURN TRUE;
    END IF;
    IF length(candidate) >= 8 THEN
        SELECT count(*) INTO n FROM jobs WHERE id::text LIKE candidate || '%';
        RETURN n = 1;
    END IF;
    RETURN FALSE;
END;
$$ LANGUAGE plpgsql;

-- Link-integrity guard on the jobs write path — the same disease
-- class as a phantom subject, the same on_missing dial as
-- subject_edges, the same restore escape hatch.
CREATE OR REPLACE FUNCTION check_job_edges()
RETURNS TRIGGER AS $$
DECLARE
    edge      RECORD;
    raw       JSONB;
    candidate TEXT;
    ok        BOOLEAN;
BEGIN
    BEGIN
        IF current_setting('audit_log.ref_check', true) = 'off' THEN
            RETURN NEW;
        END IF;
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;

    FOR edge IN
        SELECT field_path, field_kind, on_missing
          FROM job_edges
         WHERE source_kind = NEW.kind
    LOOP
        raw := NEW.metadata -> edge.field_path;
        IF raw IS NULL THEN
            CONTINUE;
        END IF;
        IF edge.field_kind = 'job_id_list' THEN
            IF jsonb_typeof(raw) <> 'array' THEN
                CONTINUE;
            END IF;
            FOR candidate IN SELECT jsonb_array_elements_text(raw)
            LOOP
                ok := job_edge_resolves(candidate);
                IF NOT ok THEN
                    IF edge.on_missing = 'abort' THEN
                        RAISE EXCEPTION
                            'job edge %.% references unresolvable Job %',
                            NEW.kind, edge.field_path, candidate
                            USING ERRCODE = 'foreign_key_violation';
                    ELSE
                        RAISE WARNING
                            'job edge %.% references unresolvable Job % (on_missing=warn)',
                            NEW.kind, edge.field_path, candidate;
                    END IF;
                END IF;
            END LOOP;
        ELSE
            candidate := NEW.metadata ->> edge.field_path;
            ok := job_edge_resolves(candidate);
            IF NOT ok THEN
                IF edge.on_missing = 'abort' THEN
                    RAISE EXCEPTION
                        'job edge %.% references unresolvable Job %',
                        NEW.kind, edge.field_path, candidate
                        USING ERRCODE = 'foreign_key_violation';
                ELSE
                    RAISE WARNING
                        'job edge %.% references unresolvable Job % (on_missing=warn)',
                        NEW.kind, edge.field_path, candidate;
                END IF;
            END IF;
        END IF;
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS jobs_check_job_edges_trg ON jobs;

CREATE TRIGGER jobs_check_job_edges_trg
    BEFORE INSERT OR UPDATE OF metadata ON jobs
    FOR EACH ROW EXECUTE FUNCTION check_job_edges();
