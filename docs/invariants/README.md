# The invariant register

Every load-bearing invariant declares how it is held
(`docs/design/design-conformance.md`, "The mechanism"). A claim with
no enforcement is indistinguishable, at a glance, from one with
enforcement; this directory is what makes the difference visible.

**One invariant per file, named `<id>.toml`.** That is a mechanism,
not a filing preference. The register used to be a single
`docs/invariants.toml` whose entries appended at the tail, so any two
cars registering a learning on the same day conflicted on the same
line — the exact shape `CLAUDE.md` §9a describes for the schema
manifest that was eventually deleted for it. On 2026-08-15 resolving
one such conflict dropped an entry's `[[invariant]]` header, folding
its keys into the entry above; the lint printed `ok` on the result
(b071994b). Adding an invariant now touches no shared line at all,
and `infra/lint/invariant-register.sh` rejects a file holding
anything other than exactly one entry whose `id` matches its
filename.

The topical grouping the single file carried (tiering, the event
log, the clock, and so on) is gone with it. Nothing load-bearing
went with it: the drift entries say `DOES NOT HOLD TODAY` in their
own `note`, and dated learnings carry their dates the same way.

## Fields

All seven keys are required on every entry;
`infra/lint/invariant-register.sh` checks the shape, never the claim.

| key | meaning |
|---|---|
| `id` | Stable slug, unique across the directory, and the file's name. Never recycled: a conformance finding cites an id, so an id that changes meaning silently rewrites history. |
| `claim` | One sentence, quotable, in the design's own words. |
| `source` | The doc or file that states it. |
| `enforcement` | `enforced` \| `checked` \| `unenforced` |
| `mechanism` | `enforced` → the lint/test path (must exist on disk). `checked` → the verification method. `unenforced` → empty. |
| `last_verified` | `YYYY-MM-DD`. Required for `checked`. |
| `note` | Why unenforced, or what would enforce it. Required for `unenforced`. |

The three enforcement classes:

- **enforced** — a lint, a test, or a type makes violation impossible
  or loud.
- **checked** — verified periodically by inspection or by a protocol
  that produces findings.
- **unenforced** — nothing holds it. This is not a sin; it is debt,
  and writing it down is what makes it visible.

The lint enforces the DECLARATION, not the strength. An author may
write `unenforced`, and that honesty is the point — anything else
turns the rule into pressure to fake enforcement
(design-conformance Q2).

## Provenance

Seeded 2026-08-13 by audit, not by transcription: every `enforced`
entry was opened and read to confirm the mechanism actually checks
the claim. Three claims that read as enforced did not survive that
reading and are recorded `unenforced` — see
`dispatcher-rules-only-shrink`, `gate-roster-is-complete`, and
`spa-lists-are-generated`.
