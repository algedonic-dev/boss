#!/usr/bin/env bash
# dispatcher-actor-stamp — every downstream call a dispatcher handler
# makes must carry the rule-as-actor `x-boss-user` header.
#
# Why this exists. `products.produce` read the ledger with a raw
# `client.post(&url)` and no header. That was harmless for as long as
# `/api/ledger/*` was ungated, and became a hard 403 the moment it was
# — which stopped the WIP→FG cost transfer, so WIP accumulated with
# ZERO credits ($4.9M and climbing), finished goods were never
# produced, and the invoice-consume handler then failed too for want
# of stock. One missing header, three broken invariants, and nothing
# in CI could see it: the handler's own tests mock the downstream
# service, so they pass whether or not the call is authenticated.
#
# The rule is structural, which is why it belongs here rather than in
# a test: an unauthenticated internal call is a latent outage that
# fires when somebody else adds a gate, possibly months later.
#
# Enforcement shape mirrors the other ratchets: grep, explicit
# allow-list, non-zero exit on anything new.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

HANDLERS=crates/orchestrators/boss-dispatcher-handlers/src/handlers
# The assignment dispatcher lives in the core crate and carries its
# identity as a reqwest DEFAULT header, so it has no `.header(...)`
# line to grep for. That is exactly how it stayed invisible while its
# writes landed on simulated Jobs marked real.
ASSIGNMENT=crates/core/boss-dispatcher/src

python3 - "$HANDLERS" "$ASSIGNMENT" <<'PY'
import pathlib, re, sys

handlers = pathlib.Path(sys.argv[1])
assignment = pathlib.Path(sys.argv[2])

# Files whose calls legitimately carry no actor. Keep this empty
# unless there is a recorded reason — a public webhook to a third
# party has no BOSS actor to present, for example.
ALLOW = {
    # Outbound to a counterparty's URL, not a BOSS service: there is
    # no internal identity to stamp.
    "webhook_notify.rs",
}

# `.post(&url)` / `.get(&url)` — a request aimed at a URL variable.
CALL = re.compile(r"\.(post|get)\(\s*&?(url|[a-z_]*url)\b")

failures = []
for path in sorted(handlers.glob("*.rs")) + sorted(assignment.glob("*.rs")):
    if path.name in ALLOW:
        continue
    src = path.read_text()
    # Drop the test module: mocks are not the production call path.
    cut = src.find("#[cfg(test)]")
    if cut != -1:
        src = src[:cut]
    lines = src.splitlines()
    for i, line in enumerate(lines):
        if not CALL.search(line):
            continue
        # The builder chain runs until `.send()`; the header must
        # appear somewhere in it.
        window = []
        for j in range(i, min(i + 20, len(lines))):
            window.append(lines[j])
            if ".send()" in lines[j]:
                break
        chunk = "\n".join(window)
        # Identity may ride on the client's default headers (the
        # assignment dispatcher does that), but sim-ness cannot: it
        # belongs to the event being handled, not to the client.
        if "x-sim-origin" not in chunk:
            failures.append(f"{path}:{i + 1}: no x-sim-origin — {line.strip()}")
        elif "x-boss-user" not in chunk and "default_headers" not in path.read_text():
            failures.append(f"{path}:{i + 1}: no actor — {line.strip()}")

if failures:
    print("FAIL — dispatcher calls missing an actor or sim-origin header:")
    for f in failures:
        print(f"  {f}")
    print()
    print("Stamp it: .header(\"x-boss-user\", dispatcher_actor_header(rule_name))")
    print("or use common::post_json, which does it for you.")
    raise SystemExit(1)

print("ok: every dispatcher downstream call stamps its actor and its origin")
PY
