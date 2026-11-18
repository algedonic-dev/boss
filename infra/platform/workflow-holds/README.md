# Workflow holds — kinds the unattended drift publish must leave alone

One file per held kind, `<kind>.toml`, named for the workflow kind it
holds (the row is `infra/platform/workflows/<kind>.toml`). **Holding a
kind is dropping a file in, and lifting it is a car that deletes the
file.** Nothing is appended anywhere, so two cars holding two kinds touch
no shared line.

## A hold is a timing hold; the passkey is the server's (backlog a4b57aca)

Since 2026-11-08 a hold says only WHEN a row may go live — "after the
stations it names read back", the deliberate publish with a positive
control and a named rollback of design b08725c2 — and an agent may
declare one for its own row. Whether a publish needs a PERSON is no longer
this directory's question. `POST /api/workflows/{kind}/publish` answers it
on the LIVE row: a row with a presence step, or one that admits or fulfils
a packet of such a row through a dispatcher rule (closed transitively), is
published only with a passkey-signed publish-a-protocol packet; every other
row, a new kind and a candidate that adds presence among them, an agent
publishes, and the drift publish may publish it unattended
(`crates/core/boss-jobs/src/publish_passkey.rs`). So the default holds
(any row declaring a `writer`, an `executor` or a `resources` bill) and the
`released` state that answered them are retired.

**Five files stay although the server now guards their rows' passkey**:
`ops-request` and `publish-a-protocol`, and, since car db099d49 moved
their owners (design c0ae096f; review 6254feec), `break-glass-enrolment`,
`enroll-a-managed-agent` and `workflow-design`. A file here is still the
only thing that makes `publish-drift --file-held` open the
publish-a-protocol packet a person signs for an edit of the row;
deleted, an edit of one would reach the drift publish, be refused by the
server, leave a draft that blocks every later check, and reach no
signer. The pin
`every_moved_row_the_server_protects_waits_for_its_signed_publish`
(crates/core/boss-jobs/tests/station_tree.rs) holds the three moved rows
to the server's own `publish_passkey::protected_kinds`. Deriving
`--file-held` from the server's protected set retires them (and so the
rest of the approval half of this directory) in a later car. They also
give `infra/platform/delivery-policy/train-conductor.toml` the names it
reads here as `protected_names_from`: the first two keep delivery
protection unchanged, and the three moved rows join it, so
`infra/forge/delivery-plan.py` counts any edit of those rows protected,
as a passkey row's edit should be.

## Why this exists (backlog 083d240e)

A workflow row that turns on a REFUSAL goes live at a deliberate registry
publish: Pacific hours, David present, a positive control straight after,
a named rollback (design b08725c2; design 09618594, question `signer`).
But the tree publishes changed rows by itself. After a boss-gcp converge,
`check-publish-drift-on-boss-gcp-converge-moved` files `publish-drift
--check`, and `publish-drift-for-real-on-clean-check` files `publish-drift
--for-real` whenever the check answers `refused 0` and `would publish` at
least 1. So a refusal row would have published itself on the first clean
check after its car merged, at any hour, with no control, and again after
a rollback, because republishing the old row leaves the tree ahead of
live. Found 2026-10-06 by builder run 9f9bb3da; that day only ten
unrelated refused kinds held it off (ops-request 090bcfc9).

## The shape

```toml
drift_publish = "held"
why = '''One or more sentences: what this row turns on, and why it must
go live at a deliberate publish rather than on the next clean check.'''
lifts = 'backlog 6c9183de: the car that follows the deliberate publish and its positive control deletes this file'
```

- `drift_publish` is `"held"`. Required.
- `why` is required, a sentence of at least 20 characters.
- `lifts` is required and names, by its 8-hex id, the item
  or design whose completion lifts the hold.
- No other key is read, and the file carries no `kind =` line: the kind is
  the file name.


## What a hold does, and what it does not

`infra/gcp/workflow-holds.py` is the one reader. **A hold stops every
unattended road, and there are two**: `publish-drift`, which a rule files
by itself, and the one-kind `publish-workflow` verb, which declares no
approval, so any actor who may file an ops-request can ask for it (review
R1 of the signer car, run 7cee49b9). Both pass through
`infra/gcp/publish-workflow.sh`, so the hold sits there, and
`infra/gcp/publish-drift.sh` reads it as well to say it out loud.

- `publish-workflow <kind>` refuses a held kind (exit 9) in its real mode
  and under `--force-tree`, before it touches the registry or the CLI,
  naming the kind, the hold's source, its why, what lifts it and the
  deliberate door. `--check` still answers: it says HELD, then what the
  registry holds, and where it would have said `--check ok` it says
  `--check HELD` (exit 9).
- `publish-drift --check` names every held kind, every run, with its why
  and what lifts it, and counts it as `held` in the verdict line: never in
  `would publish`, never in `refused`. A held kind does not stop the
  other kinds from publishing.
- `publish-drift --for-real` never publishes a held kind. It reads the
  holds again before every publish, and the sub-verb reads them once more
  at the publish itself, so a check that predates a hold cannot publish
  through it.
- **The one door a hold does not bind is the deliberate one:** `boss
  workflow publish <kind> infra/platform/workflows/<kind>.toml`, run by a
  person from a checkout. That is the act the hold waits for, and the
  same act is the rollback. The CLI says the kind is held and stays held.
  Publishing does not lift a hold; only a car that changes this directory
  does. That is what makes a rollback stick.
- **No machine voice asks for that door on a held kind** (backlog
  c6bd9f18). Four voices name `boss workflow publish` as a remedy: the
  item `infra/forge/registry-drift.sh` files after a converge, the
  report of `infra/lint/the-live-protocols-are-the-authored-protocols.sh`,
  the refusal `boss dispatch` gives a step with no agent block, and the
  drain's standing prompt (`infra/dev/drain.md`, after each landing).
  The first three ask this reader first, and the prompt tells its
  reader to. For a held kind each says it is held, why, and what lifts
  it, and names no publish. The converge files no item for a held kind
  the tree is ahead on by an edit (that is the hold working, and
  `lifts` already names the item that ends it) and says it on every run
  instead, on its journal and in the `registry_drift` summary on its
  packet; a held kind where live may carry work of its own (both moved,
  side not measured) is filed, with the hold and no publish. An item
  ALREADY OPEN for a held kind that still names the publish is
  corrected by the converge through the metadata door and marked
  `publish_withdrawn`; the mark, not the item's prose, says it is done
  (a hold's own `why` may name the door), a hold whose text changes
  re-corrects its item once, and a correction is counted only when the
  item reads back corrected. If the holds cannot be read, none of the three
  names a publish for any kind, and the converge files one item saying
  so.
- **What this does not guarantee.** It does not stop an actor who holds
  `publish` on `workflow` from running that hand door at any hour. Whether
  that publish needs a passkey is the server's answer (above), not this
  directory's.
- **Fail closed.** A file here that does not parse, lacks its `why`,
  carries an unknown key or a state other than `held`, names a kind the
  bundle does not author, or is neither `README.md` nor `<kind>.toml` — or
  this path being anything but a directory — makes
  `publish-workflow` refuse every mode and `publish-drift` refuse the
  whole run in both modes (exit 78: nothing published). The pin
  `crates/core/boss-testing/tests/publish_drift_sh.rs`
  (`the_shipped_holds_are_readable`) refuses such a file on the car that
  writes it.

A hold does not reach the seed: a kind with no live row at all is
admitted by the seed (insert-if-missing) when its file lands, hold or no
hold. A hold covers a kind the registry already has.
