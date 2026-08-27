-- 125-job-edges-normalize.sql — a job edge is stored as the id it
-- resolves to, not as the prefix somebody typed.
--
-- MEASURED FAILURE, 2026-08-14. Train 656fe30e landed eight cars. Five
-- of them named a `user-feedback` packet through
-- `metadata.backlog_item` so the feedback-obligation rule would close
-- those packets on merge. The rule fired, the handler ran, and every
-- one of them failed:
--
--   complete-feedback-branch-on-car-merged/jobs.complete_linked_step:
--   GET /api/jobs/40fe7291 returned 400 Bad Request: invalid job id
--
-- then NAK'd for redelivery, six times, on its way to the dead letter.
-- The five packets stayed open on work that had already shipped.
--
-- THE DEFECT IS NOT THE SHORT ID. It is that two halves of the system
-- disagree about what a job reference IS, and both are documented.
-- `job_edge_resolves` (104) deliberately accepts an 8+ character
-- prefix when it matches exactly one row — that is what let the write
-- succeed, and it is a real convenience, because every operator and
-- agent reads ids as the 8-char prefix the UI and the logs print.
-- `GET /api/jobs/{id}` requires a full UUID and 400s on anything else.
-- So the write path admitted a value the read path cannot use, and the
-- gap showed up in a reactor six retries later rather than at the
-- write. A fact that lives twice drifted (CLAUDE.md 9a).
--
-- THREE WAYS TO RECONCILE, and why this one. Tightening the guard to
-- demand full UUIDs would reject a convenience that is genuinely used
-- and would not fix the 12 prefix values already stored (11
-- backlog_item, 1 train — triggers do not revisit existing rows).
-- Widening `GET /api/jobs/{id}` to resolve prefixes changes the read
-- semantics of every consumer of the jobs API to fix one class of
-- writer. Normalizing at the write edge is the smallest change that
-- removes the disagreement instead of moving it: the guard ALREADY
-- computes the resolution in order to validate, so it can store what
-- it resolved. Prefixes stay legal to write, and nothing downstream
-- ever sees one.
--
-- This is normalization, not interpretation — the value written back
-- is the row the guard already proved unique. An ambiguous or
-- unresolvable prefix still aborts exactly as before.
--
-- Existing rows are NOT rewritten. A BEFORE trigger only sees writes,
-- and a data migration over historical metadata is a separate decision
-- with its own blast radius; the 12 known rows are on closed jobs whose
-- obligations have already been settled by hand.

-- Resolve a candidate to the full job id it names, or NULL when it
-- names none / more than one. Same acceptance rule as
-- `job_edge_resolves`, which now delegates to this so the two cannot
-- drift (there is one definition of "resolves", and it returns the id).
CREATE OR REPLACE FUNCTION job_edge_resolve_id(candidate TEXT)
RETURNS TEXT AS $$
DECLARE
    hit TEXT;
    n   BIGINT;
BEGIN
    IF candidate IS NULL OR candidate = '' THEN
        RETURN NULL;
    END IF;
    IF EXISTS (SELECT 1 FROM jobs WHERE id::text = candidate) THEN
        RETURN candidate;
    END IF;
    IF length(candidate) >= 8 THEN
        SELECT count(*) INTO n FROM jobs WHERE id::text LIKE candidate || '%';
        IF n = 1 THEN
            SELECT id::text INTO hit FROM jobs WHERE id::text LIKE candidate || '%';
            RETURN hit;
        END IF;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Unchanged contract, one definition underneath: an empty ref makes no
-- claim and passes; anything else resolves or it does not.
CREATE OR REPLACE FUNCTION job_edge_resolves(candidate TEXT)
RETURNS BOOLEAN AS $$
BEGIN
    IF candidate IS NULL OR candidate = '' THEN
        RETURN TRUE;
    END IF;
    RETURN job_edge_resolve_id(candidate) IS NOT NULL;
END;
$$ LANGUAGE plpgsql;

-- The guard, now storing what it resolved. The only behavioural change
-- is the two `NEW.metadata := jsonb_set(...)` lines, which fire when
-- the written value differs from the id it resolves to.
--
-- Carries 110's wildcard scope (`OR source_kind = '*'`), and that is
-- not a detail: this function is defined across three migrations, so a
-- CREATE OR REPLACE written from 104's text alone silently REVERTS
-- 110. Which is exactly what the first draft of this file did — the
-- `waiting_on` wildcard stopped being enforced and
-- `wildcard_guard_applies_to_every_kind` went red. Anything that
-- redefines this function again must diff against the LATEST
-- definition, not the first one.
CREATE OR REPLACE FUNCTION check_job_edges()
RETURNS TRIGGER AS $$
DECLARE
    edge      RECORD;
    raw       JSONB;
    candidate TEXT;
    resolved  TEXT;
    items     JSONB;
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
            items := '[]'::jsonb;
            FOR candidate IN SELECT jsonb_array_elements_text(raw)
            LOOP
                resolved := job_edge_resolve_id(candidate);
                IF resolved IS NULL THEN
                    IF edge.on_missing = 'abort' THEN
                        RAISE EXCEPTION
                            'job edge %.% references unresolvable Job %',
                            NEW.kind, edge.field_path, candidate
                            USING ERRCODE = 'foreign_key_violation';
                    ELSE
                        RAISE WARNING
                            'job edge %.% references unresolvable Job % (on_missing=warn)',
                            NEW.kind, edge.field_path, candidate;
                        resolved := candidate; -- warn keeps what was written
                    END IF;
                END IF;
                items := items || to_jsonb(resolved);
            END LOOP;
            IF items <> raw THEN
                NEW.metadata := jsonb_set(NEW.metadata, ARRAY[edge.field_path], items);
            END IF;
        ELSE
            candidate := NEW.metadata ->> edge.field_path;
            IF candidate IS NULL OR candidate = '' THEN
                CONTINUE;
            END IF;
            resolved := job_edge_resolve_id(candidate);
            IF resolved IS NULL THEN
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
            ELSIF resolved <> candidate THEN
                NEW.metadata := jsonb_set(
                    NEW.metadata, ARRAY[edge.field_path], to_jsonb(resolved));
            END IF;
        END IF;
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
