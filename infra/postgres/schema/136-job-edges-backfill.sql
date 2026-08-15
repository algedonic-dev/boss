-- 136-job-edges-backfill.sql — normalise the job edges written before
-- the trigger learned to.
--
-- 125-job-edges-normalize.sql taught `check_job_edges` to rewrite a
-- resolvable prefix into the full id on every write. It did not touch
-- the rows already on disk, so edges written earlier still hold 8-char
-- prefixes.
--
-- THAT IS NOT COSMETIC, AND IT DEAD-LETTERED IN PRODUCTION. The
-- feedback obligation (`jobs.complete_linked_step`) passes a car's
-- `backlog_item` straight to `GET /api/jobs/{id}`, which requires a
-- full UUID. When train 20260815-0621 merged at 16:30:46Z, car
-- bc6c061a's edge — the string `bb86d687` — produced eight NAK
-- redeliveries and then a dead letter. The linked packet only closed
-- because a DIFFERENT car named the same target with a full id, which
-- is why the failure was nearly invisible while it was happening.
-- Seven rows carry a prefix edge today (`d99b310d`).
--
-- The prefix was always legal — `job_edges` documents that a stored
-- value may be a `>= 8-char` prefix, and the resolver honours that.
-- The disagreement was between the registry, which accepted a prefix,
-- and a consumer that could not resolve one. Normalising the data is
-- the smaller half of closing that; the handler learning to resolve is
-- the other, and is deliberately NOT here (see below).
--
-- USES THE RESOLVER THE PREVIOUS CAR ALREADY ADDED rather than
-- reimplementing prefix matching: one definition of "which Job does
-- this string mean", and `job_edge_resolve_id` already refuses an
-- ambiguous prefix by returning NULL. A row whose edge cannot be
-- resolved is LEFT ALONE — this migration corrects spellings, it does
-- not decide what an unresolvable reference meant.
--
-- Single-value edges only. `job_id_list` edges take the same treatment
-- but need an array rebuild; none of the seven is a list, and writing
-- untested array surgery to cover a case with no instances is how a
-- migration earns a rollback.
UPDATE jobs j
   SET metadata = jsonb_set(
           j.metadata,
           ARRAY[e.field_path],
           to_jsonb(job_edge_resolve_id(j.metadata ->> e.field_path))
       )
  FROM job_edges e
 WHERE (e.source_kind = j.kind OR e.source_kind = '*')
   AND e.field_kind <> 'job_id_list'
   AND j.metadata ? e.field_path
   AND j.metadata ->> e.field_path IS NOT NULL
   AND j.metadata ->> e.field_path <> ''
   AND job_edge_resolve_id(j.metadata ->> e.field_path) IS NOT NULL
   AND job_edge_resolve_id(j.metadata ->> e.field_path) <> j.metadata ->> e.field_path;
