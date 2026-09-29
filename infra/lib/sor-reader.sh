# sor-reader.sh — the ONE shell spelling of the identity a machine READ
# of the system of record carries. Source it; it defines one function
# and touches nothing else (no env file is read, no variable is set), so
# a lint can source it without inheriting a host's address.
#
# sor_reader_header ID — the `x-boss-user` value, signed as ID (an
# `automation:<who>` slug naming the script that reads).
#
# WHY (backlog e5f7b51e, 2026-09-28). A request with no `x-boss-user`
# is a caller with no name, and the jobs API answers one a SMALLER
# WORLD rather than an error — for the estate reads, a 403, which
# node-roles.sh's converge would otherwise have read as a dark registry
# and answered by installing its cache. So a script that reads signs, as
# the platform's READ role at the auditor tier: core policy grants
# `audit-readonly` Read at Scope::All on every shipped resource and no
# other action anywhere, so nothing signed with this can write.
#
# ONE SHAPE, THREE READERS (CLAUDE.md §9a): node-roles.sh (through
# infra/lib/sor.sh), every lint (infra/lint/lib/sor-read.sh), and — in
# Rust — the recorded probe's and the unnamed CLI read's
# `boss-cli identity.rs reader_header`, which cannot source a shell
# file. `a_read_of_the_record_signs_one_shape` (boss-testing) holds the
# Rust copy equal to this one.
sor_reader_header() { # <automation id>
    printf '{"id":"%s","role":"audit-readonly","access_tier":"auditor","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}' "$1"
}
