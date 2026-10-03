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

Each file's header comment records the week it was measured
(2026-09-24T14:00Z to 2026-10-01T14:00Z, 575,034 audit-log facts, read
through `/api/events/export` in 28 six-hour windows, none at the export's
50,000-row cap) and where it spells its role.
