#!/usr/bin/env python3
"""workflow-holds — which platform workflow kinds the tree holds out of the
unattended drift publish, read ONCE, for every reader.

WHY IT EXISTS (backlog 083d240e, found 2026-10-06 by builder run 9f9bb3da)
---------------------------------------------------------------------------
A workflow row that turns on a REFUSAL goes live at a deliberate registry
publish: Pacific hours, David present, a positive control straight after, a
named rollback (design b08725c2; design 09618594 question `signer`). But the
tree publishes changed rows by itself: a boss-gcp converge files
`publish-drift --check`, and a check that answers `refused 0` and `would
publish >= 1` files `publish-drift --for-real`. So a refusal row would have
gone live unattended on the next clean check after its car merged, at any
hour, with no control — and AGAIN after a rollback, because republishing the
old row leaves the tree ahead of live. On 2026-10-06 only an accident held it
off: the newest check (ops-request 090bcfc9) answered `refused 10`, ten
unrelated kinds.

WHAT IS HELD
---------------------------------------------------------------------------
1. BY DEFAULT, every kind whose row declares a field `writer` or a step
   `executor` — a refusal row by construction (boss-jobs field_writer.rs,
   credential_executor.rs). Derived from the row itself, so forgetting to
   declare a hold cannot publish a refusal.
2. BY DECLARATION, infra/platform/workflow-holds/<kind>.toml, one file per
   kind so two cars never contend on a line (CLAUDE.md 9a):

       drift_publish = "held"        # or "released"
       why = '''...'''                # required, a sentence
       lifts = '...'                  # required when held: the item or
                                      # design (an 8-hex id) that lifts it

   `held` holds any kind, and puts a why and a lift on a default hold.
   `released` hands a default-held row back to the drift publish, on the
   record. The kind is the FILE NAME — the file carries no `kind =` line, so
   publish-workflow.sh's history walk (which greps infra/platform for one)
   never reads a hold as a bundle.

A hold binds EVERY UNATTENDED ROAD, and there are two (review R1 of the signer
car, run 7cee49b9): `publish-drift`, and the one-kind `publish-workflow` verb,
which declares no approval. Both pass through infra/gcp/publish-workflow.sh, so
that script asks this reader and refuses a held kind itself; publish-drift.sh
asks it too, to name and count what is held. The one door a hold does NOT bind
is `boss workflow publish <kind> <file>`, run by a person from a checkout: that
is the deliberate act the hold waits for, and the rollback. Publishing does not
lift a hold; a car does. What this does not guarantee: it does not stop an
actor who holds `publish` on `workflow` from running that hand door — who may
is policy's question (and a passkey's), not this file's.

FAIL CLOSED
---------------------------------------------------------------------------
Anything here that cannot be read is a PROBLEM, never an absent hold: a file
that does not parse, a key outside the three, a state outside the two, a
missing or empty `why`, a held file with no `lifts` naming an id, a file for
a kind the bundle does not author, a `released` for a row that declares no
writer or executor (it releases nothing — residue), any entry that is neither
README.md nor <kind>.toml, a holds path that exists and is not a directory
(a regular file, a dangling link), and a row file that does not parse (whether
it declares a writer cannot be told). One problem and the answer is exit 1 with
no rows; publish-drift.sh then refuses the whole run, in both modes.

USAGE
  workflow-holds.py <checkout>

OUTPUT, tab-separated, one line per kind that is not plainly open:
  held      <kind>  <source>  <why>  <lifts>
  released  <kind>  <source>  <why>  -
where <source> is `declared in infra/platform/workflow-holds/<kind>.toml` or
`by default: step <slug> field <name> declares writer <w>` /
`by default: step <slug> declares executor <e>`.
On a problem: `problem<TAB><text>` lines only.

EXIT
  0  read whole
  1  at least one problem — the holds cannot be trusted, hold everything
  2  usage, or no bundle directory in the checkout
"""
import os
import re
import sys
import tomllib

BUNDLE_REL = "infra/platform/workflows"
HOLDS_REL = "infra/platform/workflow-holds"
KIND = re.compile(r"^[a-z][a-z0-9-]{1,60}$")
KEYS = {"drift_publish", "why", "lifts"}
STATES = ("held", "released")
WHY_MIN = 20
ID = re.compile(r"(?<![0-9a-f])[0-9a-f]{8}(?![0-9a-f])")


def one_line(text):
    return " ".join(str(text).split())


def refusal_declarations(doc):
    """Every writer/executor a row file declares, as readable phrases."""
    found = []
    for wf in doc.get("workflow") or []:
        if not isinstance(wf, dict):
            continue
        for step in wf.get("step") or []:
            if not isinstance(step, dict):
                continue
            slug = step.get("title", "?")
            executor = step.get("executor")
            if executor not in (None, ""):
                found.append(f"step {slug} declares executor {executor}")
            for field in step.get("fields") or []:
                if isinstance(field, dict) and field.get("writer") not in (None, ""):
                    found.append(
                        f"step {slug} field {field.get('name', '?')} declares writer {field['writer']}"
                    )
    return found


def main(argv):
    if len(argv) != 2:
        print("usage: workflow-holds.py <checkout>", file=sys.stderr)
        return 2
    repo = argv[1]
    bundle = os.path.join(repo, BUNDLE_REL)
    holds_dir = os.path.join(repo, HOLDS_REL)
    if not os.path.isdir(bundle):
        print(f"workflow-holds: {repo} has no {BUNDLE_REL} directory", file=sys.stderr)
        return 2

    problems = []
    kinds = sorted(f[:-5] for f in os.listdir(bundle) if f.endswith(".toml"))

    # The default, derived from each row.
    default = {}
    for kind in kinds:
        path = os.path.join(bundle, f"{kind}.toml")
        try:
            with open(path, "rb") as fh:
                doc = tomllib.load(fh)
        except Exception as e:  # noqa: BLE001 — every failure is the same refusal
            problems.append(
                f"{BUNDLE_REL}/{kind}.toml could not be read ({one_line(e)}), so whether it declares a writer or an executor cannot be told"
            )
            continue
        declared = refusal_declarations(doc)
        if declared:
            more = f" (and {len(declared) - 1} more)" if len(declared) > 1 else ""
            default[kind] = f"by default: {declared[0]}{more}"

    # The declarations.
    declared_state = {}
    # Only a path that does not exist at all is "no declared holds". A
    # regular file there, or a link that leads nowhere, used to fall
    # through this `isdir` with no else and read the same way — every
    # declared hold gone, exit 0 (review 72485f08 of car ca5d0218, F2).
    if os.path.lexists(holds_dir) and not os.path.isdir(holds_dir):
        problems.append(
            f"{HOLDS_REL} is not a directory (a regular file, or a link that leads nowhere), so the declared holds cannot be read — which is not the same as none being declared"
        )
    if os.path.isdir(holds_dir):
        for name in sorted(os.listdir(holds_dir)):
            rel = f"{HOLDS_REL}/{name}"
            if name == "README.md":
                continue
            if not name.endswith(".toml") or not KIND.match(name[:-5]):
                problems.append(
                    f"{rel} is neither README.md nor <kind>.toml — a hold under a name this reader does not read would be a hold nobody honours"
                )
                continue
            kind = name[:-5]
            if kind not in kinds:
                problems.append(
                    f"{rel} holds a kind the bundle does not author: there is no {BUNDLE_REL}/{kind}.toml (a misspelt hold holds nothing)"
                )
                continue
            try:
                with open(os.path.join(holds_dir, name), "rb") as fh:
                    doc = tomllib.load(fh)
            except Exception as e:  # noqa: BLE001
                problems.append(f"{rel} could not be read as TOML: {one_line(e)}")
                continue
            bad = False
            for key in sorted(set(doc) - KEYS):
                problems.append(
                    f"{rel} carries a key this reader does not know, `{key}` (the keys are drift_publish, why, lifts)"
                )
                bad = True
            state = doc.get("drift_publish")
            if state not in STATES:
                problems.append(
                    f"{rel}: `drift_publish` must be \"held\" or \"released\", and is {state!r}"
                )
                bad = True
            why = doc.get("why")
            if not isinstance(why, str) or len(one_line(why)) < WHY_MIN:
                problems.append(
                    f"{rel}: `why` is required — a sentence of at least {WHY_MIN} characters saying why the kind is {state if state in STATES else 'held or released'}"
                )
                bad = True
            lifts = doc.get("lifts")
            if state == "held" and (not isinstance(lifts, str) or not ID.search(lifts)):
                problems.append(
                    f"{rel}: `lifts` is required on a held kind and names the item or design that lifts it by its 8-hex id"
                )
                bad = True
            if lifts is not None and not isinstance(lifts, str):
                problems.append(f"{rel}: `lifts` must be a string")
                bad = True
            if state == "released" and kind not in default and kind in kinds:
                problems.append(
                    f"{rel} releases nothing: {BUNDLE_REL}/{kind}.toml declares no writer and no executor, so the kind is not held — delete the file"
                )
                bad = True
            if not bad:
                declared_state[kind] = (
                    state,
                    f"declared in {rel}",
                    one_line(why),
                    one_line(lifts) if isinstance(lifts, str) and lifts.strip() else "-",
                )

    if problems:
        for p in problems:
            print(f"problem\t{p}")
        return 1

    for kind in kinds:
        if kind in declared_state:
            state, source, why, lifts = declared_state[kind]
            print("\t".join([state, kind, source, why, lifts]))
        elif kind in default:
            print(
                "\t".join(
                    [
                        "held",
                        kind,
                        default[kind],
                        "a row that declares a writer or an executor turns on a refusal, and a refusal goes live at a deliberate publish (design b08725c2)",
                        f"a {HOLDS_REL}/{kind}.toml saying drift_publish = \"released\", landed by a car",
                    ]
                )
            )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
