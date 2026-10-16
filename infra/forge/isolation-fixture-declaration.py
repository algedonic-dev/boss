#!/usr/bin/python3 -I
"""Read only the original forge journal; copied shapes are not declarations.

No journal is created, fetched, repaired or borrowed here. Readers on a
different host without the protected original record answer UNKNOWN.
"""
import importlib.util
import json
from pathlib import Path
import sys

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("fixture_declaration_kernel", HERE / "isolation-fixture.py")
kernel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kernel)


def names(live, store):
    if not isinstance(live, dict) or not isinstance(live.get("items"), list):
        raise kernel.Unknown("fixture native collection incomplete")
    result = []
    for actual in live["items"]:
        request = kernel.identifier(actual["metadata"]["labels"]["boss.dev/request"])
        state = store.read(request)
        if state is None:
            raise kernel.Unknown("original fixture journal is unavailable")
        if state["plan"]["request"] != request:
            raise kernel.Unknown("original fixture journal names another request")
        if actual.get("kind", "").lower() + "_uid" not in state.get("binding", {}):
            raise kernel.Unknown("fixture original create acknowledgement is unresolved")
        if kernel.recognized_original(state, actual):
            result.append(actual["metadata"]["name"])
    if len(result) != len(set(result)):
        raise kernel.Unknown("duplicate fixture native identity")
    return result


def main(argv):
    if len(argv) == 2 and argv[0] == "--declaration":
        if Path(argv[1]).read_bytes() != (HERE / "isolation-fixture.py").read_bytes():
            raise kernel.Unknown("candidate factory differs from trusted declaration reader")
        for kind in ("Job", "Service"):
            print(kind + "\tboss-dev\tboss.dev/purpose=isolation-fixture\tinfra/forge/isolation-fixture.py")
    elif len(argv) == 2 and argv[0] == "--names":
        live = json.loads(Path(argv[1]).read_text())
        # An empty successful native collection needs no original journal.
        if live.get("items") == []:
            return
        store = kernel.JournalStore(kernel.JOURNAL_ROOT, readonly=True)
        for name in names(live, store):
            print(name)
    else:
        raise kernel.Unknown("unknown fixed fixture declaration operation")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (OSError, ValueError, KeyError, TypeError, kernel.Unknown):
        print("fixture declaration UNKNOWN: protected original request/UID/shape unavailable or invalid", file=sys.stderr)
        sys.exit(4)
