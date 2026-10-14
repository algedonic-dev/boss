#!/usr/bin/env python3
"""workflow-row-compare — does the tree's row for one kind say what a
registry row says, key for key?

    python3 workflow-row-compare.py <toml> <live.json> <kind>

Exit 0 and print EQUAL when the kind's row in the TOML renders to the
registry row; exit 1 and print one `DIFF\\t<path>\\ttree=…\\tlive=…` line
per differing key, with a window of each side around where they first
part (both full copies stay readable at their homes, the file and GET
/api/workflows/<kind>, so the window discards no only-copy). Exit 3 when the TOML does not parse or
lacks the kind (a historical revision may legitimately lack it).

A path names the step by its title and the field by its name:
`steps[approve].fields[decision].writer`. It reads nothing but its two
files, so a test and an operator can run it offline over a saved row.

ONE READER: infra/gcp/publish-workflow.sh, at its three comparisons
(tree vs live, each historical revision vs live, tree vs the row read
back after a publish). publish-drift.sh asks that script per kind.

WHY EVERY KEY, AND NOT A LIST (backlog aa28f921; review 4746c046, F1)
---------------------------------------------------------------------
Until 2026-10-08 this was a heredoc inside publish-workflow.sh holding
a CURATED view: label, description, category, subject_kinds, and per
step nine named keys, with each field reduced to (name, type,
required). A curated list answers the questions its author thought of.
Measured by that review, offline, with the comparator lifted from the
branch: the ops-request tree row against a live-shaped row with every
`writer` stripped read EQUAL, and a changed writer plus an added
`executor` read EQUAL. Those are the keys that decide WHO MAY WRITE a
field and who may run a step, so the one drift nothing could see was
the drift of a refusal: the tree says `writer = "signer"`, live does
not, the verb answers "nothing to publish" forever, and the CLI's
"confirmed equal" after a publish proved nothing about them. The
script's own header listed sign_offs_required, metadata_schema,
entitlements and the workflow `metadata` as left out too, and
assurance_required, terminal, claimable and the three duration keys
were out without being listed anywhere.

So there is no list of compared keys. At each level the loader gives a
shape to — the workflow row, a step, a field, an agent block — the keys
compared are the UNION of the keys either side states, minus EXCLUDED
below. A key added to StepToml or StepField tomorrow is compared the
day a file first uses it, with no edit here (CLAUDE.md §9a: collapse
the list rather than ask the next person to extend it). The drift lint
reached the same rule for step keys on 2026-09-28 (backlog 462cdfe3).

THE THREE THINGS IT MUST KNOW ABOUT THE LOADER, each in one place:

  EXCLUDED   keys that are not a claim the file makes, with the reason.
  SILENT     what the registry holds where the file says nothing — the
             serde default the loader writes. Silence and that value
             are one claim.
  PROJECTED  the one key the loader writes FROM another.

A new key whose default is not "absent" and is not named in SILENT
reads as a DIFF on every row that omits it. That is the loud direction,
and crates/core/boss-testing/tests/workflow_row_compare_py.rs holds it
shut: every shipped kind file, rendered by the real seed loader, must
compare EQUAL to itself.
"""
import json
import sys
import tomllib

# Keys that are not a claim the file makes about the row. Everything
# else either side states is compared. One table, each entry with its
# reason; the test named above refuses an entry without one.
EXCLUDED = {
    "workflow": {
        "kind": "the row is SELECTED by it — both sides are the same kind by construction",
        "version": "assigned by the registry (max+1); a workflow file declares none",
        "status": "the registry's lifecycle column; no TOML key",
        "created_at": "stamped by the registry at insert; no TOML key",
        "authoring_job_id": "set by the authoring surface; no TOML key",
        "owning_team": "the loader STAMPS its default_owner and refuses a file that disagrees (seed_loader.rs), so no publish could ever move it",
    },
    "step": {},
    "field": {},
    "agent": {},
}

# What the registry holds where the file is silent, when that is not
# simply "nothing" (null, false, "", [] and {} are all read as nothing
# — see `absent`). Applied only where the key is MISSING: an explicit
# null on either side stays a finding against a stated value.
SILENT = {
    "field": {"filled_by": "executor"},  # StepField's serde default (boss-core job.rs)
    "agent": {"executor_provenance": "advisory"},  # AgentSpec's legacy default
}


def project(level, d):
    """The one key the loader writes from another.

    A step's `audience` is projected onto `authority_role`
    (boss-jobs/src/audience.rs `selectors_for`: role -> authority_role)
    when the file names no bare authority_role, so a file that says
    `audience = { role = "x" }` renders to a row carrying BOTH. Without
    this a correct publish read NOT CONFIRMED on 2026-09-16 02:10Z
    (backlog-item v8, ops-request deb264ed).
    """
    out = dict(d)
    for key, value in SILENT.get(level, {}).items():
        out.setdefault(key, value)
    if level == "step" and absent(out.get("authority_role")):
        aud = out.get("audience")
        if isinstance(aud, dict) and isinstance(aud.get("role"), str):
            out["authority_role"] = aud["role"]
    return out


def absent(v):
    # The registry hands back null where the file has no key, and false
    # / "" / [] / {} where a serde default filled it; the file never
    # distinguishes them. `0` is a value, not silence.
    return v is None or v is False or (isinstance(v, (str, list, dict)) and len(v) == 0)


def value(v):
    """A stated value, with numbers read as floats: TOML `5`, JSON `5.0`."""
    if isinstance(v, bool):
        return v
    if isinstance(v, (int, float)):
        return float(v)
    if isinstance(v, dict):
        return {k: value(x) for k, x in v.items()}
    if isinstance(v, list):
        return [value(x) for x in v]
    return v


def leaf(v):
    return None if absent(v) else value(v)


# Where a key holds a structure the loader gives a shape to, rather
# than a value: (level, key) -> (the level of what it holds, and for a
# list, the key its elements are named by).
NESTED = {
    ("workflow", "steps"): ("step", "title"),
    ("step", "fields"): ("field", "name"),
    ("step", "agent"): ("agent", None),
}


def names(items, by):
    """The elements' names when they can be matched by name, else None."""
    if not isinstance(items, list) or not all(isinstance(x, dict) for x in items):
        return None
    got = [x.get(by) for x in items]
    if not all(isinstance(n, str) and n for n in got) or len(set(got)) != len(got):
        return None
    return got


def diffs(level, tree, live, path=""):
    tree, live = project(level, tree), project(level, live)
    for key in sorted((set(tree) | set(live)) - set(EXCLUDED[level])):
        a, b = tree.get(key), live.get(key)
        here = f"{path}.{key}" if path else key
        inner, by = NESTED.get((level, key), (None, None))
        if inner and by:
            a, b = a or [], b or []
            an, bn = names(a, by), names(b, by)
            if an is not None and bn is not None:
                # Which elements, in which order, is ONE finding — a
                # step or a field on one side only is named here once,
                # not once per key against nothing.
                if an != bn:
                    yield (f"{here}.{by}s", ",".join(an) or None, ",".join(bn) or None)
                theirs = {x[by]: x for x in b}
                for x in a:
                    if x[by] in theirs:
                        yield from diffs(inner, x, theirs[x[by]], f"{here}[{x[by]}]")
                continue
        elif inner and isinstance(a, dict) and isinstance(b, dict):
            yield from diffs(inner, a, b, here)
            continue
        if leaf(a) != leaf(b):
            yield (here, leaf(a), leaf(b))


def text(v):
    if v is None:
        return None
    s = v if isinstance(v, str) else json.dumps(v, ensure_ascii=False, sort_keys=True)
    return s.replace("\n", "\\n").replace("\t", " ")


def windows(a, b, width=80):
    """Both sides on one line each, cut AROUND the first difference.

    Cut from the start, two 1,200-character descriptions that part ways
    at character 90 printed the same 80 characters twice — a finding
    that named the key and showed nothing (seen in this script's own
    fixture, 2026-10-08).
    """
    a, b = text(a), text(b)
    at = next((i for i, (x, y) in enumerate(zip(a or "", b or "")) if x != y),
              min(len(a or ""), len(b or "")))
    start = max(0, at - 20) if max(len(a or ""), len(b or "")) > width else 0

    def cut(s):
        if s is None:
            return "<absent>"
        return ("…" if start else "") + s[start:start + width] + ("…" if start + width < len(s) else "")
    return cut(a), cut(b)


def main(argv):
    if len(argv) != 4:
        print("usage: workflow-row-compare.py <toml> <live.json> <kind>", file=sys.stderr)
        return 2
    toml_path, live_path, kind = argv[1], argv[2], argv[3]
    try:
        with open(toml_path, "rb") as fh:
            doc = tomllib.load(fh)
    except Exception as e:
        print(f"UNREADABLE {toml_path}: {e}")
        return 3
    rows = [w for w in (doc.get("workflow") or []) if isinstance(w, dict) and w.get("kind") == kind]
    if not rows:
        print(f"NOKIND {toml_path}")
        return 3
    with open(live_path) as fh:
        live = json.load(fh)
    # The TOML key is `step` ([[workflow.step]]); the row's is `steps`.
    tree = dict(rows[0])
    if "step" in tree:
        tree["steps"] = tree.pop("step")
    found = list(diffs("workflow", tree, live))
    if not found:
        print("EQUAL")
        return 0
    for path, a, b in found:
        ta, tb = windows(a, b)
        print(f"DIFF\t{path}\ttree={ta}\tlive={tb}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
