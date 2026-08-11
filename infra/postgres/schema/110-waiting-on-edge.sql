-- 110-waiting-on-edge.sql — a cross-job wait becomes a first-class
-- edge (e9291570, reopening 50e78d70: six of eight "sitting" Jobs
-- were truthfully waiting on another Job, but the wait lived in
-- metadata prose no board renders — blocked and stalled looked
-- identical; in network terms the packet displayed at the wrong
-- station).
--
-- Three pieces:
--   1. The guard learns a '*' wildcard source_kind, so an edge every
--      kind carries is declared ONCE (a per-kind roster would drift
--      the way the gate definition did — CLAUDE.md §9a).
--   2. The ('*', 'waiting_on') edge, dialed to 'abort' from birth:
--      measured live 2026-08-10, zero rows carry the key, so there
--      is no folklore to grandfather and a dangling wait is exactly
--      the invisible-sitting disease this cures.
--   3. The clear-on-close rule: when any Job closes, the dispatcher
--      clears `waiting_on` on its waiters through jobs-api, whose
--      update path re-evaluates metadata-gated steps (aa9980c8) —
--      so a woken wait is a woken step, not a stale grey badge.

-- 1. Wildcard-aware guard (replaces 104's; body otherwise identical).
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
         WHERE source_kind = NEW.kind OR source_kind = '*'
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

-- 2. The edge itself. An empty-string value means "cleared" and
--    resolves trivially (job_edge_resolves returns true for '') —
--    the clear-on-close write stays legal under its own guard.
INSERT INTO job_edges (source_kind, field_path, field_kind, on_missing, description) VALUES
  ('*', 'waiting_on', 'job_id', 'abort',
   'The Job whose closure this Job waits on. Boards render the wait; the dispatcher clears it when the blocker closes.')
ON CONFLICT (source_kind, field_path) DO NOTHING;

-- 3. Clear-on-close. `jobs.job.closed` carries the flat `id` of the
--    closing Job (one emit site, jobs-api). No when-clause: every
--    close runs one indexed no-op query in the common case.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('jobs-clear-waiting-on', 1, 'active', 'jobs.job.closed',
   NULL,
   '[{"handler":"jobs.clear_waiting","args":{}}]'::jsonb,
   NULL, NULL, NULL, NULL)
ON CONFLICT (name, version) DO NOTHING;
