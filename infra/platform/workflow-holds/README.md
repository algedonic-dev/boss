# Workflow holds — kinds the unattended drift publish must leave alone

One file per held kind, `<kind>.toml`, named for the workflow kind it
holds (the row is `infra/platform/workflows/<kind>.toml`). **Holding a
kind is dropping a file in. Lifting a hold is a car too, and which car
depends on why the kind is held:** a kind held only by its file is
lifted by deleting the file; a kind held BY DEFAULT (its row declares a
`writer` or an `executor`, below) stays held when its file is deleted,
and is lifted by a file that says `drift_publish = "released"`. Nothing
is appended anywhere, so two cars holding two kinds touch no shared
line.

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

- `drift_publish` is `"held"` or `"released"`. Required.
- `why` is required, a sentence of at least 20 characters.
- `lifts` is required on a held kind and names, by its 8-hex id, the item
  or design whose completion lifts the hold.
- No other key is read, and the file carries no `kind =` line: the kind is
  the file name.

## The default, and the override in both directions

A row that declares a field `writer` or a step `executor` is a refusal row
by construction, so it is **held by default, with no file here**:
forgetting to declare a hold cannot publish a refusal unattended. A file
overrides the default either way:

- `drift_publish = "held"` holds any kind, and puts a `why` and a `lifts`
  on a kind the default already holds.
- `drift_publish = "released"` hands a default-held row back to the drift
  publish, on the record, once its deliberate publish and control are
  done. A `released` file for a row that declares no writer and no
  executor releases nothing and is refused as residue.

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
  `publish` on `workflow` from running that hand door, at any hour,
  without David. Who may run it, and whether it should need a passkey, is
  a policy question this directory does not answer.
- **Fail closed.** A file here that does not parse, lacks its `why`,
  carries an unknown key or state, names a kind the bundle does not
  author, or is neither `README.md` nor `<kind>.toml` — or this path
  being anything but a directory, or a row file that does not parse — makes
  `publish-workflow` refuse every mode and `publish-drift` refuse the
  whole run in both modes (exit 78: nothing published). The pin
  `crates/core/boss-testing/tests/publish_drift_sh.rs`
  (`the_shipped_holds_are_readable`) refuses such a file on the car that
  writes it.

A hold does not reach the seed: a kind with no live row at all is
admitted by the seed (insert-if-missing) when its file lands, hold or no
hold. A hold covers a kind the registry already has.
