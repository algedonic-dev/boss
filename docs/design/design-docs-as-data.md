# Design: design docs as data — the discussion moves into BOSS

**Status**: in-review — open questions tracked at `/system/design`.
**Source**: feedback `19614937`, reframed by David 2026-08-08: design
prose rides the PR train because git is its system of record and BOSS
is a read-through cache of the checkout — but the train exists for
*code* review bandwidth, and design review already happens as data in
the UI. Directive: authoring-in-BOSS with export-to-git for settled
residue "is the right end state."
**Related**: [transactional-audit-log.md](./transactional-audit-log.md)
(the write path this content would join) ·
[public-api-mcp.md](./public-api-mcp.md) (agents as authors) ·
[docs/architecture-decisions.md](../architecture-decisions.md) (the
settled-residue destination that already exists)

---

## The inversion, named

Two review pipelines exist and they are crossed. **Code review** asks
"does this break the system": branch → ship-a-change Job → train →
merge → deploy. **Design review** asks "is the thinking right":
review Job → per-question resolutions → decision. The second is BOSS
data end to end — except the *content under review*, which is a
markdown file that must complete the entire code pipeline before the
data pipeline can begin. The author writes for a reader who cannot
read it until a train delivers it.

The repo's own convention already points the right way: "docs that
survive under `docs/design/` are living references, not decision
archives" — settled residue. The discussion was always meant to live
in the tracker. What is missing is a home for the *content*: the
analysis, the measured grounding, the question bodies. Today that
content's only authoring path is a file in the repo, so it pays the
code toll.

## What exists today

- **Git is the source; BOSS is a cache of the checkout.**
  `boss-docs-api` indexes `{repo_root}/docs/design/*.md` (18 docs)
  into `design_docs` + `design_questions` rows; reindex runs at
  startup or via `POST /api/design/reindex`; malformed docs are
  rejected into `/api/design/rejections`. What is reviewable at
  `/system/design` equals whatever branch `/opt/boss` has checked
  out.
- **The decision flow is already data.** Review Jobs
  (`design-doc-review` + the `review-design` step plugin), pending
  resolutions (`design_pending_decisions`), and flush jobs
  (`design_flush_jobs`) are BOSS state. The last act of a decision,
  though, is a *file edit*: `boss docs flush-pending` rewrites the
  markdown (matching `### Qn:` anchors in an `## Open questions`
  section), commits, and pushes.
- **Design state is the only workflow state in BOSS with no
  audit-log provenance.** The `boss-docs` crate contains zero event
  emission — no outbox call, no publisher, nothing. A design
  decision, the highest-leverage state change in the system, is the
  one kind that never lands in the log. (This is not entirely
  accidental: the epoch trim deletes `audit_log` rows past the
  baseline id, and the non-event-sourced `design_*` tables are what
  lets design state survive playground resets. The accident is that
  provenance was traded away for durability when the system needed
  both.)
- **The settled-residue path already exists**: each release, settled
  material folds into `architecture-decisions.md` and the flattened
  source doc is deleted.

## What one evening of real use measured (2026-08-08)

The audit-log Q2/Q6 arc, run through the current pipeline end to end:

1. **Decisions invisible for 41 minutes.** David recorded both
   resolutions through the review UI at 20:03. They sat in
   `design_pending_decisions` — correct, durable, and surfaced
   nowhere — until 20:44, when an agent happened to query the table.
   A decision recorded but unflushed is indistinguishable from no
   decision.
2. **The flush failed on a shape mismatch between the two sources of
   truth.** The doc had been restructured on a branch; the flush's
   anchor-matching rewrite could not find the headings the DB
   decisions pointed at. Recovering took a git revert, a retry
   endpoint, and knowledge that queuing a flush *consumes* the
   pending rows into the job payload.
3. **The flush's default push target is wrong on the primary
   deployment.** It pushes `origin`, which 403s from the playground;
   the code documents the workaround env var. The sanctioned writer
   of decisions fails without operator folklore.
4. **Reviewable = checked out.** Making the updated doc readable at
   `/system/design` before merge required checking its branch out
   globally — blocking the train conductor's clean-on-main deploy
   check — then relying on the persisted index *not* being refreshed
   after switching back. A correctness property held by not running
   a refresh is not a mechanism.
5. **The content round-trip is ~12–22 hours.** Analysis authored
   ~20:30 reaches the UI after the next train window (06:01), a
   human merge, and a deploy. The decision it exists to serve took
   90 seconds once the reader could see it — which happened in chat,
   outside the system, because the pipeline could not serve it. Work
   routing around the model is the exact failure BOSS exists to
   prevent.

None of these are bugs in their components. Every one is the
git-as-source model expressing itself.

## Proposed shape

**Design docs become a versioned registry, written through the same
write path as everything else.** The Workflow registry is the
pattern, wholesale: append-only versions; publishing v(n+1)
supersedes v(n); an open review pins to the version it opened under.

- **Storage.** `design_docs` becomes the system of record:
  `(slug, version, title, status, body_md, authored_by,
  created_at)`, append-only. Questions stay parsed rows per version
  — same parser, same `### Qn:` authoring convention inside
  `body_md`, validated at publish time instead of reindex time (a
  malformed doc is a 422 to its author, not a row in a rejections
  table nobody reads).
- **Events.** Every mutation goes through the outbox:
  `design.doc.published`, `design.question.resolved`,
  `design.doc.status_changed`. Decisions become events with actors;
  Decision-history sections become *renderings* of decision events
  rather than text a worker splices into markdown. The
  anchor-matching rewrite, the flush worker's git dance, and failure
  modes 1–3 above are deleted, not fixed.
- **Authoring.** `/system/design` gains authoring the way
  `/system/workflows` has it, and the same API serves agents — an
  agent posts analysis from its session and the reader sees it
  seconds later, policy-gated and provenance-stamped like any other
  write. Failure modes 4–5 cease to exist.
- **Git keeps the settled residue and the build-time contracts.**
  `architecture-decisions.md` stays in-repo, now *generated* from
  decision events at release time — the flush direction inverted:
  today the DB's decisions flush into git as the last act of
  deciding; here git receives exports of what is already settled.
  Reading-frame docs that code, tests, and CLAUDE.md reference at
  build time stay in the repo as living references.

## What this deliberately is not

- **Not a wiki.** Versions are append-only, publishing is
  policy-gated, questions and resolutions keep their structured
  anchors, and every change has an actor in the log. It is the
  Workflow registry's discipline applied to prose.
- **Not the end of docs-in-repo.** Contract references that pin
  code stay next to the code that honors them; the corpus guard
  keeps pinning whatever remains in the tree. What moves is the
  *discussion* — the state that is decision-shaped.
- **Not a migration emergency.** The two doc branches currently
  parked for the train land under the old model; the 18 existing
  docs import as version-1 rows with their git provenance when the
  registry exists. Expand/contract: both paths live during the
  transition, reindex-from-tree retires at the end.

## Open questions

### Q1: Where exactly is the git/BOSS line?

Proposal: content referenced at build time stays in git — the
reading frames CLAUDE.md links, `architecture-decisions.md`, and
any doc a test cites — and everything decision-shaped (in-review,
reopened, question-carrying) lives in the registry. The sharp
version: *a doc lives in git iff deleting it breaks a build*. Is
that the right knife, and which of the current 18 fall on each
side?

### Q2: How does event-sourced design state survive the epoch trim?

The trim deletes `audit_log` rows past the baseline id; today's
design tables survive resets precisely by being outside the log.
Event-sourcing design state must not make it trimmable. Options: a
kind-allowlist in the trim (design.* rows are platform state, not
demo state — the trim already sanctions a baseline gap, so the
integrity checker can learn a second sanctioned shape); or design
events land in the log for provenance while the registry projection
is the durable read state and rebuilds accept the horizon. The
first is more machinery; the second quietly gives up
rebuild-from-log for one domain. Neither is free — this is the
load-bearing question.

### Q3: What pins doc↔code agreement once discussion docs leave the tree?

`docs_corpus_presents` guards whatever is in the tree at compile
time. For registry docs the equivalent is publish-time validation
(server-side, same parser). But some registry docs will still cite
code facts — lints, file paths, measured contracts. Does the §9a
rule ("a fact that lives twice gets an equality test") get a
registry-side mechanism — e.g. a nightly checker that resolves
cited paths/lints against the deployed tree — or do we accept that
registry prose can drift and rely on the release-time fold review?

### Q4: Who can publish, and as whom?

Authoring is a write like any other: policy-gated, actor-stamped.
Humans via `/system/design`; agents via the API (and eventually
MCP). Is publish authority a role (`design-author` Class), a
policy rule on `(publish, design-doc)`, or Job-mediated (a
`publish-a-doc` Workflow so publications are themselves reviewable
work)? The Job-mediated option is the most BOSS-shaped and the
most ceremony; the policy rule is the least of both.

### Q5: What does the release-time export actually produce?

The fold into `architecture-decisions.md` is currently a human
editorial act. Generated-from-events can mean: a mechanical
appendix (decision entries in order, humans still write the
narrative), or a full generation the release owner edits. And does
the export PR ride the train as an ordinary code change (proposal:
yes — it is the one design artifact that *should*, because it
changes the repo)?

### Q6: What marks the machine-owned region, and what happens to a hand edit inside it?

David, 2026-08-14, choosing between three splits: **"file wins on
prose, data wins on questions — the two live in one file with a
generated section."** That answers Q1 with a sharper knife than Q1
proposed: the line is not per-DOC (a doc lives in git iff deleting it
breaks a build) but per-SECTION inside one doc. Every design doc stays
hand-written in git; only its `## Open questions` block is generated
from the registry.

Proposed: fenced markers around the generated block
(`<!-- boss:questions:begin -->` … `:end`), and the reindex REPLACES
everything between them. A hand edit inside the fence is not merged and
not preserved — it is overwritten on the next publish, exactly as a
generated file is. The fence is what makes that fair rather than
surprising.

The open part is what to do when someone edits inside the fence anyway.
Silently overwriting loses an author's words; refusing to publish blocks
the pipeline on a typo. A third option is to overwrite but record the
discarded text on the question as a comment, so nothing is lost and
nothing is blocked.

### Q7: What is a question's state model, now that it has one at all?

`design_questions` has no `status` column today. A question is "open"
if its `### Qn:` heading is still in the markdown and "resolved" if it
is not — state inferred from the absence of text. That is why answering
one requires a commit, and why a stalled flush silently re-opens a
review on the next index.

Proposed: `open | answered | superseded | withdrawn`, with the answer
and its author on the row. `superseded` matters more than it looks —
today a re-asked question is a new heading with no link to the old one,
so "we already decided this" is unanswerable. Twice on 2026-08-14 the
same ground was covered twice for exactly that reason.

Is four states right, and does an `answered` question stay visible in
the doc under its answer, or move to Decision history immediately?

### Q8: At reindex, when the file and the registry disagree, which direction wins?

Q6 says the fence is generated, which settles the normal case. This
asks about the abnormal one: a doc arrives on a branch whose fenced
block does not match the registry — an older export, a rebase, a
hand-written doc committed before its questions were ever registered.

Proposed: the registry wins for anything inside the fence, and a
question that exists ONLY in the file is ingested as new rather than
discarded, so a doc can still be authored offline with its questions
written by hand. The risk is a rebase re-ingesting a question that was
deliberately withdrawn, which argues for matching on a stable
question id rather than on heading text.

## Decision history

_None yet._
