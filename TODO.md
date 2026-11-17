# TODO

Open work is not tracked in this file. BOSS tracks its own work in
BOSS: every defect, request and planned change is a `backlog-item`
packet in the system of record of the instance that builds BOSS, and
each one closes when the change that answers it lands. That instance
is not public; to raise something from outside, open a GitHub issue
(see [CONTRIBUTING.md](CONTRIBUTING.md)).

Done work is summarised in [CHANGELOG.md](CHANGELOG.md); settled
decisions are in
[docs/architecture-decisions.md](docs/architecture-decisions.md).

This file used to hold a roadmap whose sections were dated July and
August 2026. It had fallen behind the tree it described, citing
scripts since deleted, so it was cut to this pointer on 2026-11-16 (backlog
`d83cbd3f`). Its last full text is in git history:

```sh
git show dab00c7bb6f3:TODO.md
```

That text holds the "Post-release — strategic" section, with its
"Production infrastructure template", "Integrated IAM" and "Workflow
modeling UX improvements" entries, among others. It is a record of
what was planned in mid-2026, not a list of work still promised.

Some older code comments and docs cite TODO.md sections that are not
in that text and were not found in any earlier version of the file
either, among them "Strip BatchEngine", "Hourly tick granularity for
the live sim" and the audit_log "id-ordering race". Those citations
lead nowhere; the code beside them is the only record of what they
meant.
