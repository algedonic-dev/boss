# uxprobe — browser-truth checks against the live gateway

**Status**: living

Registry rows, ConfigMaps, and green gates say what *should* render.
This tool asks a real Chromium what *does* — because on 2026-08-19
the two disagreed for days: briefs rendered as raw markdown, the same
brief rendered twice on one screen, and a "Decide the design" step's
whole decision affordance turned out to be one Start button. Every
one of those had passed row-level verification. ("Verify at the
consuming layer" — this is that layer.)

## Run it

```sh
# 1. Tunnel to the gateway (it lives on the cluster LB, port 80):
ssh -f -N -L 18080:10.20.0.30:80 boss-gcp

# 2. First time: install the driver
cd infra/uxprobe && npm install && npx playwright install chromium

# 3. Probe a step surface (guest session, read-only):
BASE=http://127.0.0.1:18080 JOB=<job-uuid> STEP=<step-uuid> node probe.mjs
```

Screenshots land beside the script (`probe-*.png`); the console log
reports the rendered text, the interactive-control inventory, and
every HTTP >= 400 the page triggered.

## How it authenticates

`POST /api/auth/guest` mints a read-only session (rendering is a
read). The cookie is minted `Secure`, which a plain-http tunnel
origin would drop — so the probe captures the value and re-injects it
`secure: false` into the browser context. This is a probe-side
workaround, not a server bug: production traffic rides the TLS front
(10.20.0.33), where `Secure` is correct.

Guest cannot exercise writes. A write-path probe needs a real
credential and is deliberately out of scope — token administration is
David's, and a browser automation holding a real operator password is
a bigger decision than a render check.

## What it cannot see

Per-user queues (My Day renders the guest's empty board), policy-
scoped rows, and anything behind a write. If a defect only reproduces
as a signed-in operator, the probe narrows it to "renders for guest /
differs for you", which is still half the diagnosis.
