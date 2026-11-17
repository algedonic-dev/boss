//! `infra/forge/alert-lib.sh`'s `alert_replay` is RUN against fixture
//! spools, and it refuses — before it reads, posts, moves or removes
//! anything — a spool another account could have written.
//!
//! THE DEFECT (backlog 46c18d11, from re-review 8c585a24 of car
//! 1d368d6c; still standing on origin/main e2b104407 on 2026-10-22).
//! `ALERT_SPOOL` defaults to `/var/tmp/boss-alert-spool`, a name under a
//! sticky world-writable directory, for any caller started without its
//! unit environment. The cluster watchdog runs as root, and
//! `alert_replay` POSTs every `*.json` in the spool with the estate
//! machine token. So any local account that created that directory
//! first, or any spool left writable by another account, could plant a
//! body root then filed with root's credential. The units name their
//! spools under 0700 StateDirectories; nothing stopped a bare run from
//! replaying the default.
//!
//! The replay now refuses a spool that is a symbolic link, is not a
//! directory, is owned by another uid, is writable by group or other,
//! or sits in a parent another account can change (writable by group or
//! other without the sticky bit, or owned by neither this uid nor
//! root). The refusal names the path and the unit environment the
//! caller was expected to carry, and the bodies stay byte for byte.
//!
//! The foreign-owner case is measured with a `stat` stub that reports
//! the runner's uid plus one for the spool: this test runs
//! unprivileged (as 65534 in the gate), and it cannot chown a directory
//! to another account. Every other case is a real
//! directory with a real mode.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::process::Command;

const FIXTURE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
export JOBS_API="http://stub" BOSS_PLATFORM_OWNER=emp-fixture
# No token in reach and no sor.env: this test's answer must not depend on
# what credentials the runner holds.
export BOSS_MACHINE_TOKEN_DIR=/nonexistent/no-machine-token BOSS_SOR_ENV=/nonexistent/no-sor.env
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
. "$REPO/infra/forge/alert-lib.sh"
# The stub API files everything it is handed and counts it.
curl() { cat >> "$CELL/posted"; printf '\n' >> "$CELL/posted"; printf 201; }
posts() { grep -c '"title"' "$CELL/posted" 2> /dev/null || true; }
BODY='{"kind":"backlog-item","title":"planted"}'
# case NAME — a fresh cell: parent 0700, spool 0700, one kept body.
case_cell() {
    local d="$CELL/$1"
    mkdir -p -m 0700 "$d" && mkdir -m 0700 "$d/parent" && mkdir -m 0700 "$d/parent/spool"
    printf '%s' "$BODY" > "$d/parent/spool/2026-01-01T00:00:00Z-1-1-1.json"
    : > "$CELL/posted"
    export ALERT_SPOOL="$d/parent/spool"
}
# report NAME — what one replay did, and whether the body survived.
report() {
    local rc err="$CELL/$1.err" body kept=gone named=unnamed
    alert_replay 2> "$err"; rc=$?
    body=$(cat "$ALERT_SPOOL"/*.json 2> /dev/null) && [ "$body" = "$BODY" ] && kept=intact
    grep -qF "REFUSED to replay $ALERT_SPOOL" "$err" && grep -qF 'ALERT_SPOOL' "$err" && named=named
    echo "$1: rc=$rc posts=$(posts) body=$kept tries=$(ls -1 "$ALERT_SPOOL" 2> /dev/null | grep -c '\.tries$') refusal=$named"
}

case_cell trusted; report trusted

case_cell world-writable; chmod 0777 "$ALERT_SPOOL"; report world-writable
case_cell group-writable; chmod 0770 "$ALERT_SPOOL"; report group-writable

case_cell symlink
mv "$ALERT_SPOOL" "$ALERT_SPOOL.real"; ln -s "$ALERT_SPOOL.real" "$ALERT_SPOOL"; report symlink

case_cell open-parent; chmod 0777 "$(dirname "$ALERT_SPOOL")"; report open-parent
case_cell sticky-parent; chmod 1777 "$(dirname "$ALERT_SPOOL")"; report sticky-parent

case_cell foreign-owner
target="$ALERT_SPOOL" foreign=$(( $(id -u) + 1 ))
stat() {
    local out; out=$(command stat "$@") || return
    if [ "${!#}" = "$target" ]; then printf '%s %s\n' "$foreign" "${out#* }"; else printf '%s\n' "$out"; fi
}
report foreign-owner
unset -f stat
"#;

#[test]
fn a_replay_refuses_a_spool_another_account_could_write_and_keeps_its_bodies() {
    let cell = scratch::scratch_dir("a-root-replay-refuses-a-spool-it-does-not-own");
    let script = cell.join("fixture.sh");
    scratch::write_exec(&script, FIXTURE);
    let out = Command::new("bash")
        .arg(&script)
        .env("REPO", repo_root())
        .env("CELL", &cell)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    let stdout = format!(
        "{}--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let want = [
        // The control: a spool only this account can write is replayed,
        // so every refusal below is the guard and not a broken fixture.
        "trusted: rc=0 posts=1 body=gone tries=0 refusal=unnamed",
        "world-writable: rc=1 posts=0 body=intact tries=0 refusal=named",
        "group-writable: rc=1 posts=0 body=intact tries=0 refusal=named",
        "symlink: rc=1 posts=0 body=intact tries=0 refusal=named",
        "open-parent: rc=1 posts=0 body=intact tries=0 refusal=named",
        // /var/tmp's own shape: sticky, so another account can neither
        // rename nor replace a directory this account made there.
        "sticky-parent: rc=0 posts=1 body=gone tries=0 refusal=unnamed",
        "foreign-owner: rc=1 posts=0 body=intact tries=0 refusal=named",
    ];
    for line in want {
        assert!(
            stdout.lines().any(|l| l == line),
            "alert_replay: expected the line {line:?}\n{stdout}"
        );
    }
}
