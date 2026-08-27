-- 138-station-lens.sql — a station can carry the page context of the
-- surface that renders it, so a lens draws itself from the same call
-- that fetched its packets.
--
-- Origin (David, feedback 3f5f7f63): "the Design Review page should
-- really just be a custom view onto a particular queue or set of
-- queues. That is what many of our pages fundamentally devolve into.
-- Here is what is in queue, presented usefully, with context about
-- how that queue has been flowing recently."
--
-- The defect this closes is not cosmetic. `/system/design` defined its
-- own queue in the browser — `GET /api/jobs?kind=design-doc-review&
-- status=open` filtered client-side — while the `design-review`
-- station defined the same queue as a predicate the server evaluates.
-- Two definitions of one queue drift, and the drift is invisible: a
-- packet the station holds can be missing from the page that exists to
-- show it. The page now reads the station's queue, which makes the
-- registry row the single definition.
--
-- WHY THE PAGE CONTEXT LIVES ON THE STATION ROW
-- ---------------------------------------------
-- 119-station-upstream.sql proved the shape for one affordance: the
-- row declares it, `GET /api/stations/{name}/queue` echoes it, and any
-- lens renders it with no frontend change. A page's header and panel
-- set are the same kind of fact — declarations about how this queue is
-- presented — and keeping them in the component means every new lens
-- is a new Svelte file rather than a row (CLAUDE.md §9, and
-- views-as-queue-lenses.md's claim that a view is three declarations,
-- not a page of code).
--
-- WHAT THIS COLUMN DELIBERATELY DOES NOT HOLD
-- -------------------------------------------
-- Panel *data*, and the URLs it would come from. A panel key names a
-- renderer the surface already ships — the `step_plugins` idiom, where
-- the row names the bundle and the bundle knows its own source. Two
-- reasons, both concrete: a registry row holding fetch URLs is a row
-- that can point a browser anywhere, and a registry row holding panel
-- contents would make `boss-jobs` a client of every service a page
-- reads from. The design corpus is `boss-docs-api`'s to serve. The
-- station registry's business is the queue and how it is framed.
--
-- Consequence worth stating plainly: this does not reduce the page to
-- one fetch. `/system/design` still reads the doc corpus and the
-- indexer's rejections from `/api/design/*`, because those describe
-- docs that have NO packet yet — which is exactly the set you need in
-- order to start a review. What collapses to one definition is the
-- QUEUE, not the page.

ALTER TABLE stations
    ADD COLUMN IF NOT EXISTS lens JSONB;

COMMENT ON COLUMN stations.lens IS
  'Optional page context for a surface that renders this station whole: '
  '{"eyebrow": "...", "title": "Design review", "subtitle": "...", '
  '"panels": ["rejections", "corpus"]}. `title` is required; `panels` is '
  'an ordered list of renderer keys the surface already ships, never '
  'URLs and never panel data. NULL = no page claims this station, which '
  'is the common case — a station is a queue, and most queues are read '
  'by a lens that has its own identity.';


-- The design-review station, filled in.
--
-- UPDATE rather than a version bump, exactly as 119 did for
-- `upstream`: this is platform-declared presentation on the platform's
-- own seed, in the same category as `title`. Scoped to the ACTIVE row
-- so it lands whatever version history a cluster has, and guarded on
-- `lens IS NULL` so a deployment that published its own lens keeps it.
--
-- The strings are the ones `/system/design` rendered as literals
-- before this row existed, moved rather than reinvented — a migration
-- that quietly reworded an operator-facing header would be a design
-- change wearing a schema change's clothes.
--
-- Panel order is the reading order and it is load-bearing: `rejections`
-- names docs the indexer REFUSED, so the corpus below it is known to be
-- incomplete until that panel is empty. Showing the corpus first would
-- present a partial list as the whole one — how
-- transactional-audit-log.md stayed invisible for six days.
UPDATE stations
   SET lens = '{
         "eyebrow": "System Model · Design review",
         "title": "Design review",
         "subtitle": "Open questions, pending decisions, ADRs",
         "panels": ["rejections", "corpus"]
       }'::jsonb
 WHERE name = 'design-review' AND status = 'active' AND lens IS NULL;
