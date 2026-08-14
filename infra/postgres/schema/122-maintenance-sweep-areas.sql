-- 122-maintenance-sweep-areas.sql — three more maintenance areas, each
-- with a measured incident behind it rather than a guess.
--
-- 121 seeded the protocol and one area (disk headroom) and said the
-- quiet part out loud: "David expects several of these — infra, latency
-- report, and others". This is the follow-through. Filed as d0092947.
--
-- WHY SEED SEVERAL AT ONCE rather than adding one and waiting. David,
-- 2026-08-14: "introducing and changing protocol is cheap, so we can
-- create as many as we need and edit them to find the right operating
-- cadence", and "I want us to try and get in the motion of deploying
-- protocols quickly, editing a lot, and quickly learning to zero in on
-- 'good' protocols that keep jobs flowing". Guessing one correct
-- cadence up front is slower AND less accurate than publishing several
-- and reading the `clear` vs `remediated` counts. Those two terminals
-- exist precisely so "is this cadence right?" is a query rather than an
-- opinion; a cadence that only ever produces `clear` is too tight, and
-- one that mostly produces `remediated` is too loose.
--
-- All three areas share the disguise that makes them worth a sweep:
-- INVISIBLE UNTIL EXPENSIVE. None of them announces itself, each was
-- found by paying for it once, and none would be caught by looking at
-- the thing that eventually broke.
--
-- Every rule below was validated against the RUNNING dispatcher's
-- /api/dispatcher/rules/_validate before being written here — the loop
-- the 2026-08-13 outage did without, and the same discipline 121
-- recorded. Its honest limit is unchanged: the check proves the trigger
-- XOR, the topic pattern and expression parsing; it does not resolve
-- handler names. `jobs.spawn` is reused unchanged from 121, which is
-- live and proven, and no rule below invents a field name.
--
-- Cadence is `daily` for all three, matching David's standing
-- instruction on the protocol: "start with daily for the most part
-- until we get evidence that maintenance is overwhelming capacity."
-- If the dock floods, the fix is a `schedule_cadence` edit on these
-- rows, not a deploy.


-- 1. STALE BUILD CACHES. The disk problem 121 was written about had a
-- cause that a disk sweep would not have found: 69GB of cargo target
-- directories under .claude/worktrees, belonging to branches that
-- shipped days earlier. One worktree alone held 48GB. Cleaning the main
-- tree recovered 9GB; the worktrees recovered 67GB — so an inspection
-- pointed at the repo would have reported the small number and moved
-- on. This is deliberately a SEPARATE area from disk-headroom rather
-- than a bigger disk check: the two look at different places, and
-- collapsing them is how the 67GB stayed invisible the first time.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-build-caches-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"stale-build-caches\"","title":"\"Stale build cache sweep\"","metadata.target":"\"stale-build-caches\"","metadata.area":"\"infra\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;


-- 2. IMAGE FRESHNESS. The CI runner's rootful docker held a 31-hour-old
-- boss-ci image while the registry served a newer one. build.sh's own
-- header warns that a registry retag does NOT refresh a runner's local
-- tag — the warning existed and the drift happened anyway, which is the
-- signal that a comment is not a mechanism (CLAUDE.md 9a). It cost a
-- 25-minute mystery once already, and it fails in the worst direction:
-- CI goes green or red against an image nobody is looking at.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-image-freshness-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"image-freshness\"","title":"\"CI image freshness sweep\"","metadata.target":"\"image-freshness\"","metadata.area":"\"ci\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;


-- 3. DEPLOY CONVERGENCE LAG. The converge runner failed silently for
-- hours on a missing `-i` in a docker invocation, and the cluster sat on
-- a stale image while forge main moved on. A failed systemd oneshot
-- notifies nobody: the timer keeps firing, the unit keeps failing, and
-- the only symptom is that deployed behaviour quietly disagrees with the
-- tree. This is the same family as the two above and the one that most
-- directly undermines "the cluster is the system of record" — an
-- unconverged cluster is a system of record telling you about a commit
-- that is no longer true.
INSERT INTO dispatcher_rules (name, version, status, on_event, when_expr, do_steps, delay, schedule_cadence, schedule_anchor, schedule_calendar) VALUES
  ('maintenance-sweep-converge-lag-daily', 1, 'active', NULL, NULL,
   '[{"handler":"jobs.spawn","args":{"kind":"\"maintenance-sweep\"","subject_kind":"\"custom\"","subject":"\"deploy-convergence\"","title":"\"Deploy convergence sweep\"","metadata.target":"\"deploy-convergence\"","metadata.area":"\"deploy\""}}]'::jsonb,
   NULL, 'daily', DATE '2026-08-14', NULL)
ON CONFLICT (name, version) DO NOTHING;
