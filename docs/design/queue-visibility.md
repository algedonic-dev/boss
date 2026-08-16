# Design: queue visibility — every actor's lens on the one queue

**Status**: in-review — open questions tracked at `/system/design`.
**Superseded in part** by [stations.md](./stations.md), which describes
what shipped. Where the two disagree, stations.md wins.
**Source**: feedback `207236cc` — raised by David in the audit-log Q6
review (2026-08-08): "Every actor needs visibility into their
personal queue and there are lots of abstract groups, like anyone
with a certain skill, that we will have steps queued up for, and of
course agents are actors that will want queues. Do they each just
have a lens onto the one giant queue? What happens when it
inevitably gets too large? Everyone still just has an API onto the
queue and underneath the engineering team makes it work?"
**Related**: [transactional-audit-log.md](./transactional-audit-log.md)
(Q6 — machine consumers of the log) ·
[public-api-mcp.md](./public-api-mcp.md) (`list_my_work`) ·
[human-powered-state-machine.md](./human-powered-state-machine.md)

---

## The three questions, answered up front

1. **A lens onto one giant queue?** Yes — and it is already built.
   The queue is the `steps` projection; a queue is a WHERE clause.
   `GET /api/jobs/assignments` already implements the union lens:
   *mine-by-assignee* OR *claimable-by-my-roles*. The gap is not
   architecture, it is adoption — almost nothing calls it.
2. **When it gets too large?** Two different "too large"s. The
   engineering one scales with work-in-flight, never with history
   (in-flight is 0.15% of the steps table today), and one missing
   index closes the only real gap. The operational one — a deep
   queue — is an *algedonic signal* to surface, not a table to
   shard: depth = arrival rate × wait time, and the fix for a deep
   queue is capacity or priority, a management decision the system
   should make visible rather than absorb.
3. **Everyone just has an API?** Yes. One lens contract for every
   actor that *chooses* work — human, sim, external agent (the MCP
   `list_my_work` primitive is this endpoint). Machines that must
   process *every* event (the dispatcher) are not queue actors;
   they are log consumers with cursors (audit-log Q6). Sets with
   claim semantics for actors; sequences with cursors for machines.
   Keeping those two distinct is the load-bearing line.

## What exists today, measured (playground, 2026-08-08)

**The queue itself.** 64 steps in flight (23 ready, 41 active)
across 63 open Jobs and 411 employees; 42,344 steps total, so the
live set is 0.15% of history. Fourteen actors hold all in-flight
work — max personal depth 22, median 4. Wait times are bimodal at
wall-clock pace: p50 ready→done is 1 second (machine-completed
steps), p90 is 14.6 hours (real work at 1:1 time). Little's law on
the human-timescale flow (1,336 completions/day × 6.3 h mean wait)
implies a steady-state WIP around 350 at current business volume.

**The lens exists server-side.** `GET /api/jobs/assignments`
(`boss-jobs/src/http/jobs.rs`, SQL in `postgres.rs`) returns, for
open Jobs and steps in `ready|active`:

    s.assignee_id = $me
    OR ( s.metadata->>'authority_role' = ANY($my_roles)
         AND (s.assignee_id IS NULL OR s.status = 'active') )

— the personal branch and the group branch of one union, with a
deliberate no-poaching rule (a ready step assigned to someone else
is not claimable). Rows carry Job context (workflow, subject,
priority) so an actor can act without a second fetch.

**Almost nothing consumes it.**

- **My Day does not call it.** The page pulls
  `/api/jobs?status=open&limit=200` and filters client-side for
  steps assigned to the viewer — and renders its own cap warning
  when assignments fall off the 200-job window. The server-side
  lens would return exactly the right rows.
- **The sim workforce** calls `assignments?all_assigned=true` only
  — by design it executes whatever is assigned, but it therefore
  never sees unassigned role-claimable steps.
- **Agents never pull.** StepTypes with `completion = agent` are
  push-executed inline by the dispatcher and never enter any queue.
- **Nothing anywhere passes `roles=`.** The group lens — the
  "anyone with this skill" queue — has zero consumers today.

**Assignment is push, with one deliberate pull carve-out.** The
dispatcher assigns on readiness: deterministic hash spread
(`FNV(step_id) % candidates`) over the active exact-role roster —
no load or queue-depth input, deterministic so rebuilds replay
identically. Steps with *no* role constraint are deliberately left
unassigned for an operator to pull (the alternative auto-assigned
the CEO 23 generic tasks in early testing). So the pull model
already exists as a fallback; it is just unserved by any surface.

**No claim primitive.** Ready→Active is a plain PUT. Nothing
prevents two actors claiming the same role-visible step — harmless
while the group lens has no consumers, load-bearing the day it
gets one.

**Index support is half-built.** The partial index
`steps_assignee (assignee_id) WHERE status IN ('ready','active')`
covers the personal branch precisely. The group branch
(`metadata->>'authority_role'`) has **no index** — invisible at 64
in-flight rows, the first real cost at scale.

**The group vocabulary is roles; skills are dead data.**
`authority_role` (a Class-registry role) is the only group gate in
the work path. An `employee_skills` table and `skill_level` column
exist and are seeded — and nothing reads them; `/api/people`
cannot even filter on skill.

**Awareness is push, work is pull.** The `messages.notify`
dispatcher rule messages the assignee (or, for role-gated steps,
the lowest-id role holder as a deterministic on-call — no
role-wide fan-out) with a deep link to the step. Its own header
names the split: the notification adds awareness; the My Day query
drives work.

**Queue-age metrics have a clock trap.** `audit_log.timestamp` is
sim-time; `created_at` is wall time. Any "how long did someone
wait" number must use `created_at` — the Flow view already learned
this the hard way.

## The shape of the answer

**A station is registry data; its *membership* is a predicate, not
a stored roster.** The station row is real (name, discipline,
capability gate, `wip_limit`); what is derived is who is in its
queue right now — evaluated over packet state at read time, so
there is no second source of truth to drift from `steps` and
nothing to rebuild. A *view* over a station is a lens; the station
itself is a node in the network.

*(Amended 2026-08-13. This paragraph read "a queue is a lens …
never a reified structure", written before the station registry
shipped. `infra/postgres/schema/116-stations.sql` reified the node
and `stations.md` Q1–Q4 ratified it, so the original sentence had
become an argument against the substrate's own nodes. The
distinction it was reaching for — no per-actor roster to drift —
survives intact, one level down.)*

The lenses over those stations are what each actor sees.
Personal queue: the
assignee branch. Group queue: the role branch. Agent queue: the
same lens through the same API (agents are actors; the MCP
`list_my_work` tool wraps this endpoint). Reified per-actor queues
(tables, materialized views, broker state) would be a second
source of truth that can drift from `steps`; the lens cannot
drift, and rebuilds cost nothing because there is nothing to
rebuild.

**Scale is two problems wearing one word.** The lens query costs
O(in-flight), and in-flight is bounded by the business (Little's
law), not by history. Even 100× today's WIP is a few-thousand-row
indexed scan. The genuine engineering item is one expression index
on the group branch. Everything past that — a queue that is "too
large" because work arrives faster than it completes — is an
operational fact that should get *louder*, not hidden: per-lens
depth and oldest-wait are exactly the algedonic telemetry Beer's
model wants flowing upward.

**The API contract holds while the implementation evolves.**
`assignments(assignee_id, roles)` is already the right shape for
all three actor kinds. Browser, sim, and external agents consume
the same lens; policy and provenance apply identically; the
engineering team is free to change what serves it.

## What this deliberately is not

- **Not per-actor queue *storage* — a station row declares the
  queue; membership is still derived.** No per-actor queue tables,
  no broker state per actor. Measurement says the derived
  membership stays cheap far past any visible horizon.
- **Not a workload-balancing scheduler.** Assignment stays
  deterministic (replay depends on it). Whether it should weigh
  load or shift patterns is a separate decision for a separate
  doc, and nothing here forecloses it.
- **Not a second work-discovery path for agents.** Agents get the
  same lens humans get, through the same gateway, under the same
  policy — the MCP doc's "agents are CPUs in the same machine."

## Open questions

### Q3: Roles only — or do skills join the routing vocabulary, or get deleted?

`authority_role` gates everything today. The `employee_skills`
data exists but nothing reads it — and the repo rule is that dead
code gets deleted. Either skills become a real routing term
(people-api filter + dispatcher candidate filter + a `skills=`
lens param) or the table and column go. Keeping unread data
"just in case" is the one option the coding guidelines rule out.


Proposed: **delete the table and the column.** Checked before
proposing: `employee_skills` is written by the people rebuilder and by
the projection that repopulates it, and read by nothing. The single
`.skills` access in the tree is the write path putting them back. So
this is unread data, and the guidelines rule out keeping it.

The reason to delete rather than wire it up is that skills as a
separate table is the WEAKER of two mechanisms we already have for the
same job. Roles are Classes of `employee` Subjects — one `classes`
table keyed `(subject_kind, code)`, tenant-extensible without a fork.
A skill is the same kind of noun. If routing ever needs "can operate
the canning line", that is a Class and a capability term, not a
bespoke table with its own rebuild path and its own TRUNCATE.

Stations sharpen this further since the question was written: a
station already carries `capability: {"roles": [...]}`, checked at the
claim CAS. That is where a skills term would land if it were needed —
one more key in an object that already exists, on a row an operator can
edit. Nothing in the routing done to date has wanted it.

So: drop `employee_skills`, drop `Employee.skills`, and if the need
appears, add `capability.skills` reading Class codes. Deleting is
reversible in an afternoon; carrying an unread table is the thing that
quietly costs.

### Q5: Does assignment strategy become per-step-kind rule data?

Today the strategy is code: hash-spread when a role matches,
leave-unassigned when none does. Whether a step kind is
push-assigned or pooled for pull is a policy choice that differs
by workflow (sign-offs want a named person; generic tasks want a
pool). Registries-over-code says this belongs in the dispatcher
rules registry next to the other routing rules — one `strategy`
field per rule, not new `match` arms.



Proposed: **yes, as rule data — but the vocabulary the question assumed
has been overtaken, and the new one is better.**

When this was written the choice was "push-assigned or pooled for
pull", and pull had no mechanism. It does now. A station is a
data-defined queue that HOLDS packets, and the claim CAS
(`POST /api/jobs/{id}/steps/{step_id}/claim`) makes an actor taking one
safe against a second actor taking the same one — with membership
checked 409 and capability 403 before the compare-and-set. So "pooled
for pull" is no longer a strategy to build; it is what happens when
nothing assigns the step.

That makes the field smaller and more honest than a `strategy` enum.
The real question per rule is only whether to PUSH — name a holder now
— and the existing behaviours are the two values: `assign` (resolve the
authority_role to a holder, hash-spread over the job id, as the code
does today) and `leave` (write no assignee; the step is claimed from
whatever station's predicate matches it).

Put it on the dispatcher rule row beside the other routing terms, so a
sign-off can be push-assigned and a generic task pooled without a
`match` arm, which is registries-over-code applied where it belongs.

One thing to carry into the build rather than discover: a step left
unassigned is only pullable if some station's predicate actually
matches it. Today two do — the dock and the watchlist — so `leave` on
an arbitrary step kind can produce a packet in nobody's queue. The rule
row should be refused at publish time if it says `leave` for a step no
station holds, the same way the fork lint refuses an orphan outcome.

## Decisions

### Q1: Does My Day move onto the assignments lens? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> The page reimplements a weaker client-side version of the lens
> over `/api/jobs?limit=200`, with a self-documented cap bug. Moving
> it to `GET /api/jobs/assignments?assignee_id=me&roles=…` fixes the
> bug, makes the group queue visible to humans for the first time,
> and deletes code. This is close to a defect fix; the open part is
> UX — does My Day show the two branches as one list or as "mine"
> vs "up for grabs"?

Yes — and it already moved. MePage reads GET /api/jobs/assignments (apps/web/src/me/assignments.ts:92); the page's own comment records that this replaced the capped `jobs?status=open` scan filtered client-side. Recording it because the question was answered by building it, and nothing marks a question resolved when the thing that answers it ships.


### Q2: What are the claim semantics for group-visible steps? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Before any surface serves the group lens, Ready→Active needs
> compare-and-set: claim succeeds only if `assignee_id` is still
> NULL (`rows_affected > 0` gates the event, the same
> idempotency-guard shape every other write path uses). A failed
> claim is a normal outcome, not an error. Without this, two actors
> will eventually complete the same step twice.

Settled by the claim CAS, shipped as car a4c8f910: POST /api/jobs/{id}/steps/{step_id}/claim does a compare-and-set on Ready->Active, so two actors cannot claim one step. A claim that names its station additionally checks membership (409) and capability (403) BEFORE the CAS, which is where the group-visible half of this question lands.


### Q4: Is per-lens queue depth an algedonic signal, and where does it surface? (resolved)

Resolved 2026-08-16 — override.

**The question was:**

> Depth and oldest-wait per role lens (computed on wall-clock
> `created_at`) are cheap aggregates over the in-flight set. Are
> they cybernetics telemetry with thresholds (S4 sees "ar-clerk
> queue depth 3× baseline"), a Flow-view panel, or both? And does a
> breach open a Job (the system's native way to make someone own a
> fact)?

Yes, and advisory rather than enforced. A station declares `wip_limit`; the queue envelope reports `over_limit`; lenses warn and telemetry reads it, and nothing blocks on it — the posture stations.md Q3 ratified. It surfaces wherever a lens renders the station, the dock being the first.

## Decision history

_None yet._
