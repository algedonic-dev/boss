#!/usr/bin/env python3
"""tenant-rows.py — a tenant tree and the live export of it, compared ROW
by row rather than line by line.

    tenant-rows.py report TREE LIVE READ TAKEN
    tenant-rows.py status TREE LIVE READ

TREE is the tenant directory publish-tenant.sh publishes, LIVE a copy of
it that `boss tenant export` has rewritten from the instance. Both read
tenant.toml and every seeds/*.toml and seeds/*.json. READ is the files
the export's report named (comma-joined): any other file was copied from
the tree and never rewritten, so it has no live side, and comparing it
would report the tree agreeing with itself — it is named NOT READ.

`report` prints, per file, how its DECLARED rows (the rows the tree
names) stand live — the same, differing, or not live at all — and how
many live rows the tree does not declare, with the sha256 of the file's
live rows. For each TAKEN file (the file of a registry the publish is
told to --take) every differing row is printed field by field, `-` the
tree's value and `+` the instance's, because those `+` values are what
the take overwrites. For any other file a differing row is named, not
printed: no take, so the publish keeps the live row (insert-if-absent).

`status` prints one line per declared row that is not the same live,
`<file>\\t<row>\\t<differs|absent>`, sorted — the read-back's comparison.

WHY (backlog 1610115d, review c757cf5d F2). The first cut diffed the
hand-written tree against the export's canonical rendering with git: on
the Algedonic tenant all 14 files differed on header comments and key
and row order alone, 210,105 bytes against a runner that refuses a plan
over 102,400 (OPS_OUTPUT_CAP), with the rows a take overwrites buried
in the churn. Both sides now become the same normal form first — one
line per field, `<path> = <json>`, nulls dropped (the export omits
them), an integral float read as the integer it equals (`2.0` and `2`),
a string's trailing newlines dropped (a TOML multi-line string ends in
one) — and rows are matched by identity, so what remains is a difference
in a ROW. Undeclared live rows are counted and not shown, because no
publish writes them: every door is insert-if-absent or overwrites a row
the tree DECLARES, and nothing is ever deleted (tenant_publish.rs). The
export writes the instance whole, platform rows included (classes.json:
93 live rows against 8 declared on that measurement). Their digest is
still in the bytes, so a live edit to ANY row after the approval is a
different plan.

A row's identity is the first IDENTITY key every declared and live row
of its table carries uniquely (qualified by `subject_kind` when it has
one), else the COMPOSITE of the identity-like keys, else the digest of
the row itself — the scheme is chosen once per table and applied to
both sides, or the two sides could not be matched. A nested list of
tables is keyed by its first unique IDENTITY key, else by position (a
workflow's steps carry no name of their own).

Exit 1, naming the file, when a file does not parse: a comparison of a
file this cannot read would read as "the instance agrees".
"""
import hashlib
import json
import os
import sys
import tomllib

IDENTITY = ("id", "code", "slug", "kind", "name", "email", "fact_kind",
            "event_kind", "jurisdiction", "resource")
COMPOSITE = IDENTITY + ("role", "scope", "state", "subject_kind",
                        "source_table", "version", "on_event")


def scalar(value):
    return isinstance(value, (str, int)) and not isinstance(value, bool)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def normal(value):
    if isinstance(value, dict):
        return {k: normal(v) for k, v in value.items() if v is not None}
    if isinstance(value, list):
        return [normal(v) for v in value]
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str):
        return value.rstrip("\n")
    return value


def is_table_list(value):
    return isinstance(value, list) and value and all(isinstance(v, dict) for v in value)


def by_key(key):
    def name(row):
        if not scalar(row.get(key)):
            return None
        ident = f"{key}={row[key]}"
        if key != "subject_kind" and scalar(row.get("subject_kind")):
            ident = f"subject_kind={row['subject_kind']},{ident}"
        return ident
    return name


def composite(row):
    return ",".join(f"{k}={row[k]}" for k in sorted(COMPOSITE) if scalar(row.get(k))) or None


def digest(row):
    return "#" + hashlib.sha256(canonical(row).encode()).hexdigest()[:12]


def scheme(*sides):
    """The one naming that is total and unique on every side given."""
    for name in [by_key(k) for k in IDENTITY] + [composite]:
        ok = True
        for rows in sides:
            idents = [name(r) for r in rows]
            if None in idents or len(set(idents)) != len(idents):
                ok = False
                break
        if ok:
            return name
    return digest


def lines(path, value, out):
    if isinstance(value, dict) and value:
        for key in sorted(value):
            lines(f"{path}.{key}" if path else key, value[key], out)
    elif is_table_list(value):
        name = None
        for key in IDENTITY:
            idents = [by_key(key)(r) for r in value]
            if None not in idents and len(set(idents)) == len(idents):
                name = by_key(key)
                break
        for i, row in enumerate(value):
            lines(f"{path}[{name(row) if name else i}]", row, out)
    else:
        out.append(f"{path} = {canonical(value)}")


def load(path):
    if not os.path.isfile(path):
        return None
    with open(path, "rb") as fh:
        doc = tomllib.load(fh) if path.endswith(".toml") else json.load(fh)
    return normal(doc)


def files_of(root):
    found = set()
    if os.path.isfile(os.path.join(root, "tenant.toml")):
        found.add("tenant.toml")
    seeds = os.path.join(root, "seeds")
    if os.path.isdir(seeds):
        found |= {f"seeds/{n}" for n in os.listdir(seeds)
                  if n.endswith((".toml", ".json")) and os.path.isfile(os.path.join(seeds, n))}
    return found


def rows_of(doc, top):
    """{row identity: [line]} for a file, both sides at once. A file's
    rows are its top-level lists of tables (`[[table]]`, or a JSON file's
    bare array); everything else in it is one row named by its table."""
    tree, live = doc
    out = ({}, {})
    tables = set()
    for side in (tree, live):
        if isinstance(side, list):
            tables.add(top)
        elif isinstance(side, dict):
            tables |= {k for k, v in side.items() if is_table_list(v)}

    def table(side, name):
        if isinstance(side, list):
            return side if name == top and is_table_list(side) else []
        if isinstance(side, dict):
            value = side.get(name)
            return value if is_table_list(value) else []
        return []

    for name in sorted(tables):
        sides = [table(tree, name), table(live, name)]
        namer = scheme(*[s for s in sides if s])
        for i, rows in enumerate(sides):
            for row in rows:
                ident = f"{name}[{namer(row)}]"
                body = []
                lines("", row, body)
                out[i][ident] = body
    # The rest of a dict-shaped file (tenant.toml's [meta], [modules]) is
    # one row per top-level key.
    for i, side in enumerate((tree, live)):
        if isinstance(side, dict):
            for key, value in side.items():
                if key in tables:
                    continue
                body = []
                lines("", value, body)
                out[i][key] = body
    return out


def compare(tree_root, live_root):
    """[(file, tree rows, live rows)] over every file either side has."""
    result = []
    for rel in sorted(files_of(tree_root) | files_of(live_root)):
        top = os.path.splitext(os.path.basename(rel))[0]
        try:
            pair = (load(os.path.join(tree_root, rel)), load(os.path.join(live_root, rel)))
        except (OSError, ValueError, tomllib.TOMLDecodeError) as err:
            raise SystemExit(f"tenant-rows: cannot read {rel}: {err}")
        tree, live = rows_of(pair, top)
        result.append((rel, tree, live))
    return result


def state(tree, live, ident):
    if ident not in live:
        return "absent"
    return "same" if tree[ident] == live[ident] else "differs"


def live_digest(live):
    text = "".join(f"{ident}{line}\n" for ident in sorted(live) for line in live[ident])
    return hashlib.sha256(text.encode()).hexdigest()


def report(tree_root, live_root, read, taken):
    for rel, tree, live in compare(tree_root, live_root):
        if rel not in read:
            print(f"{rel}: NOT READ — boss tenant export has no read of this file's "
                  f"registry, so this plan neither shows nor guards its live rows; "
                  f"no take may name it, and the publish inserts what the instance lacks")
            continue
        states = {i: state(tree, live, i) for i in sorted(tree)}
        differ = [i for i, s in states.items() if s == "differs"]
        absent = [i for i, s in states.items() if s == "absent"]
        undeclared = len(set(live) - set(tree))
        mark = " (TAKEN)" if rel in taken else ""
        print(f"{rel}{mark}: {len(tree)} declared row(s) — "
              f"{len(tree) - len(differ) - len(absent)} the same live, {len(differ)} differ, "
              f"{len(absent)} not live; {undeclared} live row(s) the tree does not declare "
              f"(no publish writes them: counted, not shown); live rows sha256 {live_digest(live)}")
        if absent:
            print(f"  not live, the publish inserts: {', '.join(absent)}")
        if differ and rel not in taken:
            print(f"  differ, the publish keeps the live row (not taken): {', '.join(differ)}")
        if differ and rel in taken:
            for ident in differ:
                print(f"  OVERWRITTEN {ident} — a - line is the tree, a + line is the instance now:")
                t, l = set(tree[ident]), set(live[ident])
                for line in sorted(t - l):
                    print(f"    - {line}")
                for line in sorted(l - t):
                    print(f"    + {line}")


def status(tree_root, live_root, read):
    for rel, tree, live in compare(tree_root, live_root):
        if rel not in read:
            continue
        for ident in sorted(tree):
            s = state(tree, live, ident)
            if s != "same":
                print(f"{rel}\t{ident}\t{s}")


def files(csv):
    return {f for f in csv.split(",") if f}


def main(argv):
    if len(argv) == 6 and argv[1] == "report":
        report(argv[2], argv[3], files(argv[4]), files(argv[5]))
        return 0
    if len(argv) == 5 and argv[1] == "status":
        status(argv[2], argv[3], files(argv[4]))
        return 0
    print("usage: tenant-rows.py report TREE LIVE READ TAKEN | status TREE LIVE READ "
          "(READ and TAKEN: comma-joined tenant-relative files)", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
