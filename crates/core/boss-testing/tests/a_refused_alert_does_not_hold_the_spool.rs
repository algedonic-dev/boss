//! `infra/forge/alert-lib.sh`'s `alert_replay` is RUN, against a stub
//! jobs API, and a body the API REFUSES no longer holds every alert
//! behind it.
//!
//! THE DEFECT (backlog 9ecd12ea, residue of car 7849553a's review,
//! finding 3, still standing on origin/main 3356517ba on 2026-10-10).
//! The replay stopped at the first kept alert that was not filed,
//! whatever the answer. A body the jobs API judged bad — a 4xx, which
//! the next tick will get again — was therefore retried every five
//! minutes for ever by the cluster watchdog, and every alert raised
//! after it, "cluster DARK: hands needed" among them, waited behind it
//! and was never filed. The dev memory watch had already answered this
//! for its own spool (dev-memory-watch.sh `file_kept_alerts`); the
//! forge's spool now follows the same rule:
//!
//! * no answer or a 5xx is the API failing, not a judgement of the
//!   body: the replay stops and counts nothing against the alert;
//! * any other refusal backs the body off and the ones behind it are
//!   still tried;
//! * at the fifth refusal the body is moved aside to `refused/` with the
//!   answer it got beside it, kept, and counted by `alert_refused_count`.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::process::Command;

const FIXTURE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
export ALERT_SPOOL="$CELL/spool" JOBS_API="http://stub" BOSS_PLATFORM_OWNER=emp-fixture
# No token in reach and no sor.env: this test's answer must not depend on
# what credentials the runner holds.
export BOSS_MACHINE_TOKEN_DIR=/nonexistent/no-machine-token BOSS_SOR_ENV=/nonexistent/no-sor.env
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
. "$REPO/infra/forge/alert-lib.sh"
# The stub API. $CELL/api is what it does: 000 (no answer), 503, or 201
# — and at 201 it REFUSES, 422 with a body, any alert titled `refused`.
curl() {
    local out=/dev/null body api
    while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift ;; esac; shift; done
    body=$(cat)
    printf '%s\n' "$body" >> "$CELL/posted"
    api=$(cat "$CELL/api")
    case "$api" in
        000) printf 000; return 7 ;;
        201) case "$body" in *'"title":"refused'*) printf 'owner cannot be resolved' > "$out"; printf 422; return 0 ;; esac ;;
    esac
    printf '%s' "$api"
}
posts() { [ -f "$CELL/posted" ] && wc -l < "$CELL/posted" | tr -d ' ' || echo 0; }
tries() { ls -1 "$ALERT_SPOOL" | grep -c '\.tries$'; }
mkdir -p "$ALERT_SPOOL"
# The refused body is the OLDER, so the replay meets it first.
printf '%s' "$(alert_body 'refused body' d)" > "$ALERT_SPOOL/2026-01-01T00:00:00Z-1-1-1.json"
printf '%s' "$(alert_body 'cluster DARK later' d)" > "$ALERT_SPOOL/2026-01-01T00:00:01Z-1-2-1.json"

echo 000 > "$CELL/api"
alert_replay 2> /dev/null; echo "dark: rc=$? waiting=$(alert_count) tries=$(tries)"
echo 503 > "$CELL/api"
alert_replay 2> /dev/null; echo "failing: rc=$? waiting=$(alert_count) tries=$(tries)"

echo 201 > "$CELL/api"
alert_replay 2> /dev/null; rc=$?
grep -c '"title":"cluster DARK later"' "$CELL/posted" > /dev/null && later=posted || later=unposted
echo "first refusal: rc=$rc waiting=$(alert_count) later=$later tries=$(cat "$ALERT_SPOOL"/*.tries | cut -d' ' -f1)"

before=$(posts)
alert_replay 2> /dev/null
echo "inside the back-off: new posts=$(( $(posts) - before ))"

for _ in 2 3 4 5; do
    t=$(ls "$ALERT_SPOOL"/*.tries 2> /dev/null) || break
    read -r k _ < "$t"; printf '%s 0\n' "$k" > "$t"
    alert_replay 2> /dev/null
done
echo "moved aside: waiting=$(alert_count) refused=$(alert_refused_count) tries=$(tries)"
echo "why: $(head -n1 "$ALERT_SPOOL"/refused/*.why)"
grep -q 'owner cannot be resolved' "$ALERT_SPOOL"/refused/*.why && echo "why keeps the answer"
grep -q '"title":"refused body"' "$ALERT_SPOOL"/refused/*.json && echo "body kept"
"#;

/// A refusal that judges the CALLER, not the body (review 4d18be3a of car
/// 97edf615, finding 1). With the machine gate enforcing and the forge's
/// token stale or revoked, every POST answers 401 while the watchdog's
/// liveness read (exempt from the gate) still says `ok`, so the replay
/// runs every tick. Counted against each body, five of those ticks moved
/// every kept alert — "cluster DARK" first — into `refused/`, and none was
/// filed once the credential was repaired. 401, 403 (the create door's
/// policy answer, asked of the caller and the kind, never of what the
/// body says), 407, 408 and 429 now stop the replay like silence does.
/// And a curl that exits non-zero after an answer (exit 23: the answer
/// could not be written; 28: timed out after the headers) is no answer,
/// so a 201 it could not record is neither counted nor moved aside.
const CALLER_FIXTURE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
export ALERT_SPOOL="$CELL/spool" JOBS_API="http://stub" BOSS_PLATFORM_OWNER=emp-fixture
export BOSS_MACHINE_TOKEN_DIR=/nonexistent/no-machine-token BOSS_SOR_ENV=/nonexistent/no-sor.env
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS
. "$REPO/infra/forge/alert-lib.sh"
# $CELL/api: a code to answer with, or `201-exit23` (filed, but curl
# could not write the answer and exits 23 after printing the code).
curl() {
    local out=/dev/null api
    while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift ;; esac; shift; done
    cat > /dev/null
    api=$(cat "$CELL/api")
    case "$api" in
        201-exit23) printf 201; return 23 ;;
        201) printf 201; return 0 ;;
    esac
    printf 'refused for who you are' > "$out"; printf '%s' "$api"
}
tries() { ls -1 "$ALERT_SPOOL" | grep -c '\.tries$'; }
mkdir -p "$ALERT_SPOOL"
printf '%s' "$(alert_body 'cluster DARK: hands needed' d)" > "$ALERT_SPOOL/2026-01-01T00:00:00Z-1-1-1.json"
printf '%s' "$(alert_body 'second alert' d)" > "$ALERT_SPOOL/2026-01-01T00:00:01Z-1-2-1.json"
for code in 401 403 407 408 429; do
    echo "$code" > "$CELL/api"
    # Ten ticks, each past any back-off a refusal could have set.
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        for t in "$ALERT_SPOOL"/*.tries; do [ -f "$t" ] || continue; read -r k _ < "$t"; printf '%s 0\n' "$k" > "$t"; done
        alert_replay 2> /dev/null
    done
    echo "$code x10: waiting=$(alert_count) refused=$(alert_refused_count) tries=$(tries)"
done
echo 201-exit23 > "$CELL/api"
alert_replay 2> /dev/null; echo "answer unwritten: rc=$? waiting=$(alert_count) refused=$(alert_refused_count) tries=$(tries) code=$ALERT_POST_CODE"
echo 201 > "$CELL/api"
alert_replay 2> /dev/null; echo "credential fixed: rc=$? waiting=$(alert_count) refused=$(alert_refused_count)"
"#;

#[test]
fn a_refusal_of_the_caller_stops_the_replay_and_counts_nothing_against_the_body() {
    let stdout = run_fixture("a-caller-refusal-does-not-move-the-spool", CALLER_FIXTURE);
    let want = [
        "401 x10: waiting=2 refused=0 tries=0",
        "403 x10: waiting=2 refused=0 tries=0",
        "407 x10: waiting=2 refused=0 tries=0",
        "408 x10: waiting=2 refused=0 tries=0",
        "429 x10: waiting=2 refused=0 tries=0",
        "answer unwritten: rc=1 waiting=2 refused=0 tries=0 code=000",
        "credential fixed: rc=0 waiting=0 refused=0",
    ];
    for line in want {
        assert!(
            stdout.lines().any(|l| l == line),
            "alert_replay: expected the line {line:?}\n{stdout}"
        );
    }
}

/// Run a fixture in its own scratch cell; stdout, with stderr appended
/// under a marker so a failed assertion shows both.
fn run_fixture(name: &str, fixture: &str) -> String {
    let cell = scratch::scratch_dir(name);
    let script = cell.join("fixture.sh");
    scratch::write_exec(&script, fixture);
    let out = Command::new("bash")
        .arg(&script)
        .env("REPO", repo_root())
        .env("CELL", &cell)
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    format!(
        "{}--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_refused_alert_backs_off_and_is_moved_aside_while_the_rest_are_filed() {
    let stdout = run_fixture("a-refused-alert-does-not-hold-the-spool", FIXTURE);
    let want = [
        // Silence and a failing server are not judgements of the body.
        "dark: rc=1 waiting=2 tries=0",
        "failing: rc=1 waiting=2 tries=0",
        // A refusal no longer holds the alert behind it.
        "first refusal: rc=1 waiting=1 later=posted tries=1",
        "inside the back-off: new posts=0",
        "moved aside: waiting=0 refused=1 tries=0",
        "why: refused 5 times, last answer 422:",
        "why keeps the answer",
        "body kept",
    ];
    for line in want {
        assert!(
            stdout.lines().any(|l| l == line),
            "alert_replay: expected the line {line:?}\n{stdout}"
        );
    }
}
