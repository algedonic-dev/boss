# infra/platform/automations/ — every automation that writes, and its role

One file per `automation:*` id that writes to the system of record,
carrying the role that automation signs as today. The automation half of
the agents registry (`automation_actors`; backlog ddf0773e, design
abf9eeae car 1, decided 2026-10-01).

**Why it exists.** Every machine caller asserts `platform-admin` in its
own `x-boss-user`, and every service believes it: the role is a string
spelled in ~57 files, which nothing can audit or narrow. The design moves
the role onto the record — a service resolves the caller's role from a
registry rather than the header (car 2, report mode first). People have
employee rows and agents `agents` rows; until this directory the
automations had no row anywhere. **These rows change no one's access:**
each carries the role its caller already asserts, and nothing decides on
one until car 2's resolver.

**Published by `boss-platform-workflow-seed`**, the same binary and the
same start as the stations, step plugins, cadence and delivery policy —
this directory is found as the sibling of `--seed-path`. INSERT-IF-ABSENT
and nothing else: a row the deployment already holds is KEPT as it is
(the instance is the truth, design e187198f), and the seed's report line
names every declared field the live row differs on. So narrowing a role
in a running instance survives every boot; changing one here changes only
a fresh database.

## Readers, and three rows narrower than they sign (2026-10-07)

The first 24 rows came from a week of the audit log, which lists who
WROTE. The resolver's own tally (`GET /api/jobs/actor-role-reports`) then
named five automation ids with no row, every one of them only reading —
the two host converges and the units observer asking the estate registry
for their node's roles, the recorded probe's reader, and the disk sweep's
train read — and a tree scan found eleven more that had not called in
the window. All sixteen are
rows now. A reader holds `audit-readonly`, the role `sor_reader_header`
already signs it with, so the bundle spells two roles and no third.

Three rows hold LESS than their script asserts: `disk-floor-sweep`,
`prune-registry-versions` and `machine-token-deposit` send
`platform-admin` in their header and make one read each. Their rows hold
`audit-readonly`, each file says why, and nothing decides on a row until
the resolver enforces — until then the tally shows `would_deny` for any
request the narrower role would have refused. Three writers
(`cluster-watchdog`, `install-smoke`, `observe-codebase`) hold
`platform-admin` because no narrower role is granted a write today; that
is the role they sign, not a default.

**The roster is held to the tree** by
`every_script_sender_signs_as_a_registered_automation` (boss-testing): a
script under `infra/` that signs as an `automation:` id no row answers
for fails there, naming the id and the line. It does not see an id signed
from Rust or from a manifest's env, and it lists the one shape it cannot
hold — an ops verb that signs its estate read as `automation:<its own
name>` — as a debt.

**A rule needs no row.** `rule:<name>` on the wire is
`automation:rule:<name>` on the record, and the dispatcher's row answers
for the family through `signs_for`. The resolver reads that field since
2026-10-07; before, it compared ids for equality and reported every
firing rule as unregistered.

## A row

```toml
[[automation]]
id = "automation:gate-runner"     # the id it signs as; the file is named after it
role = "platform-admin"           # the role it signs as today — a Class code under (employee, role)
description = '...'               # what it is and where it runs
signs_for = "automation:rule:"    # optional: the prefix of ids it signs one per firing
```

**A family is one row.** The dispatcher signs every write a rule fires as
`automation:rule:<name>`, and the ops runner each pass as
`automation:ops-runner:<host>:<moment>-<pid>`. A row per member would copy
the `dispatcher_rules` registry (tenant rules included) into this one and
drift from it the day a rule is authored, so the signer's row declares the
prefix in `signs_for` and answers for every member.

## Adding one

When a new automation starts writing, `boss automations unregistered`
names it with its count (it reads the live audit log over a window and
this registry through `GET /api/agents/automations`). Add a file here
named after its id, with the role its `x-boss-user` asserts and the
measurement in a comment, as every row below carries.

Each of the first 24 files' header comments records the week it was
measured (2026-09-24T14:00Z to 2026-10-01T14:00Z, 575,034 audit-log
facts, read through `/api/events/export` in 28 six-hour windows, none at
the export's 50,000-row cap) and where it spells its role. The sixteen
added on 2026-10-07 record the tally read they came from, or that the
tree spelled them and no read had seen them yet.
