#!/usr/bin/env bash
#
# machine-token-deposit — the forge host takes the estate machine token,
# all three slots, out of the broker's Secret and into the ROOT-ONLY
# directory every shell caller on this host already reads, on every
# forge-converge tick, and proves it by effect.
#
#   machine-token-deposit.sh --rule <broker rule .toml> --dest <directory>
#
# WHY IT EXISTS (design-doc 20058482, question forge-token, David
# 2026-10-06; backlog 88379df3, part of urgent 2710c8fc). Every machine
# gate is in `report`, and row C of design b08725c2 cannot start its
# 72-hour window while callers miss. Most of this host's callers already
# call infra/lib/secret-header.sh `machine_token_header` — the wrap pair,
# the ops runner, the two credential deposits, read-publish-checks — and
# showed as misses for one reason: no token file existed here. Design
# 6805c764 car 4 said a host would take it through `boss credential pull
# machine-token` with a name-scoped Role; this host already holds the
# admin kubeconfig, so its own converge reads the Secret with no new
# grant, the way credential-render.sh and runner-credential-deposit.sh
# read theirs.
#
# ROOT-ONLY, AND WHAT THAT DOES AND DOES NOT ISOLATE. The directory is
# root:root 0700 and each slot 0600, held on every pass. A car's recorded
# probe runs on this host as BOSS_PROBE_USER (default david, `boss prove
# --unattended`), so a david-readable token would be readable by any
# car's probe text — which the 2026-10-05 token-holder isolation decision
# rules out. Root-only means the units that run as root are stamped and
# the units that run as that same account are NOT: to a non-root caller
# the directory cannot be searched, `machine_token_header` finds no slot
# and sends without one, silently, as it did before this file. A group
# that the probe user does not hold was the other choice the decision
# offered, and it was measured out: a unit with User=david and the group
# still writes the header file as uid david (secret-header.sh, 0600 in a
# 0700 directory of the CALLER's uid), where a probe running beside it as
# the same uid reads it. A shared uid is not a boundary a group can make.
#
# WHAT ROOT-ONLY DOES NOT CLOSE TODAY — two roads, both older than this
# file, by which a probe running as david still reaches this token:
#   1. david holds `sudo -n docker` on this host (infra/estate/
#      ops-credentials.sh reads /etc/boss-ops through it; the grant is
#      host residue, no fragment for it is in this tree), which reaches
#      any root file and the admin kubeconfig itself.
#   2. forge-converge.service runs /home/david/boss/infra/forge/
#      forge-converge.sh AS ROOT out of david's own checkout, and this
#      script and the libraries it sources are executed from that
#      checkout before the converge's checkout -f. Any shell as david
#      can edit what root runs within ten minutes.
# So on this host today the modes below keep the token from a direct
# read and from an accident; they isolate it from no account that a
# probe can already become. What closes both is a probe account of its
# own (design-doc bdc60b65, question probe-user, David 2026-10-06) — a
# separate car. machine_token_deposit_sh.rs says the same.
#
# ONE PASS:
#   1. Read the rule's declaration — secret_namespace, secret_name; the
#      rule is the one declaration and nothing here repeats it
#      (CLAUDE.md §9a).
#   2. Read `current`, `next`, `previous` and the object's
#      resourceVersion in ONE read through the admin kubeconfig. Each
#      slot must decode, be one base64url word and fit MAX_SLOT_BYTES
#      (4096, boss-core machine_token.rs), or the whole read is
#      `malformed` and NOTHING on this host changes.
#   3. With a `current` in hand, make the host's slots equal the
#      Secret's: each slot written umask 077 into a temp file beside it,
#      fsynced, 0600 root:root, renamed into place (`mv -T`: a link
#      planted at the path is replaced, never written through), the
#      directory fsynced; a slot the Secret holds blank is removed. A
#      slot already equal is only held at its mode.
#   4. Read the resourceVersion AGAIN. A Secret that moved under the pass
#      — a promotion landing between the read and the writes — is
#      followed at once, up to three passes, so a pass never ends on a
#      slot set the Secret no longer holds.
#   5. VERIFY BY EFFECT, never by the file existing: the SAME reader
#      every caller uses (`machine_token_header`, reading the directory
#      just written) stamps a GET of the jobs API's
#      /api/machine-gate/accepts, and the gate's own answer — `matched`
#      — is the record: `current`, or what else and why.
#
# NOTHING HERE IS EVER REQUIRED. An absent, blank, unreadable or
# malformed Secret, an unplaced kubeconfig and an unwritable destination
# each leave every existing file exactly as it was and say so; a host
# with no token is a host whose callers send without one, which a gate
# in `report` admits and tallies. forge-converge.sh carries this
# script's exit onto its packet and NEVER into its own: the converge is
# the loop that installs this host's repairs and it owes this nothing
# (CLAUDE.md §Diagnosis, "an arm that needs the patient is not an arm").
#
# ROTATION. The broker's self-issued rotation stages `next`, promotes it
# to `current` (the old value moves to `previous`, still accepted), and
# blanks `previous` only once no gate has seen it presented for
# `drain_minutes` — 1440, the rule's own line. This runs every ten
# minutes (forge-converge.timer), before the converge's first step that
# can fail, so after a promotion this host presents the old value for at
# most one tick, 144 ticks inside the drain; and a caller still
# presenting `previous` HOLDS the revoke open, named on it, rather than
# being stranded by it. The bound that is left: a host dark for longer
# than the drain presents a revoked value from boot until its first
# converge tick (OnBootSec=4min).
#
# NO VALUE EVER LEAVES A VARIABLE except into its slot file and the 0600
# header file curl reads: never an argv, a message, a summary field —
# not even a suffix. Every line names a slot, a path and a LENGTH.
#
# What a value still passes through, stated and not closed: kubectl's
# stdout carries the base64 slots through docker's json-file log for the
# life of the `--rm` container, root-only, as the sibling deposits' does.
#
# EVERY WAIT HERE IS BOUNDED (adversarial review 9a1e289b, B1). This runs
# at the head of the converge, so its TIME is the converge's even though
# its exit is not. Each read of the Secret ends itself — kubectl's
# `--request-timeout` inside the container, because killing the docker
# client leaves a container running — and is killed from outside after
# READ_BOUND_S if it does not (`timeout -k 5`: docker proxies TERM, and a
# client wedged on its daemon can outlive it; infra/estate/
# ops-credentials.sh bounds the same door the same two ways). The verify
# read is bounded by curl's own --max-time. And forge-converge.sh bounds
# the whole script again, so a wait nobody thought of costs one bound and
# never a tick.
#
# Exit: 0 the host's slots equal the Secret's as last read (a Secret
# that moved under all three passes is followed one step behind, and the
# action line says so), or there was nothing to take: not minted, not
# ready, staged only; 1 a fault this pass names; 78 a refusal of how it
# was invoked.

# TRACING OFF, FIRST (review 9a1e289b, F8): an exported SHELLOPTS=xtrace
# starts this shell printing every expansion, slot values among them.
set +x
set -euo pipefail

ME=machine-token-deposit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

# boss-core's MAX_SLOT_BYTES: past this a slot is not a token, to the
# gate and to every reader (machine_token_deposit_sh.rs holds them equal).
MAX_SLOT_BYTES=4096
SLOTS="current next previous"

RULE="" DEST=""
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || refuse "$1 needs a value"
    case "$1" in
        --rule) RULE="$2" ;;
        --dest) DEST="$2" ;;
        *) refuse "unknown argument $1 (usage: $ME --rule FILE --dest DIRECTORY)" ;;
    esac
    shift 2
done
[ -n "$RULE" ] && [ -n "$DEST" ] || refuse "--rule and --dest are both required"
case "$DEST" in
    /*) ;;
    *) refuse "--dest '$DEST' is not an absolute path" ;;
esac

# WHICH MACHINE THIS IS, BEFORE ANYTHING IS READ OR WRITTEN (backlog
# 62b09c57, N7; infra/lib/host-check.sh carries the incident and the
# rule). Aimed at the directory every caller on the forge reads, this
# deposits the estate's machine token: the machine must hold the address
# the estate declares for the forge, or the exit is 78 and nothing is
# read from the cluster or written here. Aimed anywhere else it is a
# test's directory and no identity is asked. Before the summary library
# is sourced, so a refusal is this script's own line and never a field
# on its caller's packet.
# shellcheck source=infra/lib/host-check.sh
. "$INFRA/lib/host-check.sh"
host_path --dest "$DEST" /etc/boss/machine-token
host_check forge "$ME"

# --- 1. the declaration: the broker rule -------------------------------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
HANDLER_LINE="$(grep -m1 '^handler = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
[ "$HANDLER_LINE" = 'handler = "credential.rotate.self-issued"' ] \
    || refuse "$RULE is not the self-issued machine token's rule ($HANDLER_LINE)"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
[ -n "$NS" ] && [ -n "$NAME" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name — cannot know what to deposit"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"

KERR="$(mktemp)"
BODY="$(mktemp)"
TMP=""
cleanup() {
    rm -f "$KERR" "$BODY"
    if [ -n "$TMP" ]; then rm -f "$TMP"; fi
}
# Set BEFORE the first machine_token_header call, which chains its own
# cleanup in front of this one (infra/lib/secret-header.sh, ORDER).
trap cleanup EXIT

rc=0
SECRET_STATE="" ACTION="untouched" EFFECT="not asked"
S_current="" S_next="" S_previous="" S_RV=""

# finish — the one way out after the declaration: what this pass found,
# did and proved, on the converge packet and in the journal, then the
# exit status. A fault is said on stderr too.
finish() {
    S_current="" S_next="" S_previous=""
    run_summary_field machine_token_secret "$SECRET_STATE"
    run_summary_field machine_token_action "$ACTION"
    run_summary_field machine_token_effect "$EFFECT"
    say "secret $NS/$NAME: $SECRET_STATE"
    say "directory $DEST: $ACTION"
    say "effect: $EFFECT"
    if [ "$rc" -ne 0 ]; then
        echo "$ME: FAULT (exit $rc) — the lines above name it; no caller on this host is stopped by it, and the files it found are as it found them unless a line above says a slot was written" >&2
    fi
    exit "$rc"
}

# --- 2. the Secret -----------------------------------------------------
# THE BOUNDS (header, EVERY WAIT HERE IS BOUNDED). A read asks the API
# server for one object: 15 s is far past a healthy answer. The outer
# kill is past that plus a cold image start, and three passes of it plus
# the verify still fit inside the bound forge-converge.sh puts on the
# whole script (machine_token_deposit_sh.rs holds that sum).
REQUEST_TIMEOUT_S=15
READ_BOUND_S="${BOSS_DEPOSIT_READ_BOUND_S:-25}"
VERIFY_BOUND_S="${BOSS_DEPOSIT_VERIFY_BOUND_S:-15}"
case "$READ_BOUND_S$VERIFY_BOUND_S" in
    '' | *[!0-9]*) refuse "BOSS_DEPOSIT_READ_BOUND_S / BOSS_DEPOSIT_VERIFY_BOUND_S must be whole seconds" ;;
esac
KC_STATE=""
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig through the kubectl container every
    # forge script uses (kubectl is not on the host). ABSENT is not ready
    # (root material David places, design 835c0c9c), never a red.
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    # --request-timeout: the read ends itself INSIDE the container, which
    # a kill of the client outside would leave running (see the header).
    KUBECTL=(docker run --rm --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc "--request-timeout=${REQUEST_TIMEOUT_S}s")
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c)"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable"
    fi
fi

# read_secret — the three slots and the object's resourceVersion into
# S_current, S_next, S_previous (each empty when absent or blank) and
# S_RV, from ONE read. Returns 0 with SECRET_STATE naming each slot's
# length; 1 with SECRET_STATE naming why nothing was read, and every
# S_* empty — a read that is not whole is not a read.
read_secret() {
    local out b_current b_next b_previous rv slot b v
    S_current="" S_next="" S_previous="" S_RV=""
    if [ -n "$KC_STATE" ]; then
        SECRET_STATE="$KC_STATE"
        return 1
    fi
    local read_rc=0
    out="$(timeout -k 5 "$READ_BOUND_S" "${KUBECTL[@]}" -n "$NS" get secret "$NAME" \
        -o "jsonpath={.data.current}|{.data.next}|{.data.previous}|{.metadata.resourceVersion}" 2>"$KERR")" || read_rc=$?
    if [ "$read_rc" -ne 0 ]; then
        out=""
        if [ "$read_rc" -eq 124 ] || [ "$read_rc" -eq 137 ]; then
            SECRET_STATE="unreadable: the read of $NS/$NAME did not answer inside $READ_BOUND_S seconds and was stopped (a stalled cluster API or docker daemon)"
        elif grep -q 'NotFound' "$KERR"; then
            SECRET_STATE="absent: Secret $NS/$NAME does not exist (the cluster converge creates it from the rule)"
        else
            SECRET_STATE="unreadable: $(head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300)"
        fi
        return 1
    fi
    # Base64 holds no '|': four fields, any of the first three empty.
    IFS='|' read -r b_current b_next b_previous rv <<< "$out"
    if [[ ! $rv =~ ^[0-9]+$ ]]; then
        SECRET_STATE="unreadable: $NS/$NAME answered no resourceVersion, so a move under the pass could not be seen"
        return 1
    fi
    for slot in $SLOTS; do
        case "$slot" in
            current) b="$b_current" ;;
            next) b="$b_next" ;;
            *) b="$b_previous" ;;
        esac
        [ -n "$b" ] || continue
        if ! v="$(printf '%s' "$b" | base64 -d 2>/dev/null)"; then
            S_current="" S_next="" S_previous=""
            SECRET_STATE="malformed: key $slot of $NS/$NAME is not base64"
            return 1
        fi
        # Leading and trailing whitespace, as boss-core's read trims it.
        v="${v#"${v%%[![:space:]]*}"}"
        v="${v%"${v##*[![:space:]]}"}"
        [ -n "$v" ] || continue
        if [ "${#v}" -gt "$MAX_SLOT_BYTES" ]; then
            S_current="" S_next="" S_previous=""
            SECRET_STATE="malformed: key $slot of $NS/$NAME is ${#v} bytes, past the $MAX_SLOT_BYTES a slot may hold"
            return 1
        fi
        if [[ ! $v =~ ^[A-Za-z0-9_-]+$ ]]; then
            S_current="" S_next="" S_previous=""
            SECRET_STATE="malformed: key $slot of $NS/$NAME is not one base64url word (${#v} bytes)"
            return 1
        fi
        case "$slot" in
            current) S_current="$v" ;;
            next) S_next="$v" ;;
            *) S_previous="$v" ;;
        esac
    done
    v=""
    S_RV="$rv"
    SECRET_STATE="read: current ${#S_current} bytes, next ${#S_next} bytes, previous ${#S_previous} bytes"
    return 0
}

# held_mode PATH MODE — MODE, and root:root when this runs as root.
held_mode() {
    chmod "$2" "$1" || return 1
    if [ "$(id -u)" -eq 0 ]; then chown 0:0 "$1" || return 1; fi
}

kerr() { head -n 1 "$KERR" | cut -c1-200; }

# put SLOT VALUE — VALUE into $DEST/SLOT, durable before it is visible.
# Returns 1 with WHY set; the slot's old file is then still whole.
WHY=""
put() {
    local f="$DEST/$1"
    WHY=""
    TMP="$(mktemp "$DEST/.$1.XXXXXX" 2>"$KERR")" \
        || { TMP=""; WHY="a temp file in $DEST could not be made ($(kerr))"; return 1; }
    (umask 077 && printf '%s' "$2" > "$TMP") 2>"$KERR" \
        || { WHY="$TMP could not be written ($(kerr))"; return 1; }
    sync -- "$TMP" 2>"$KERR" || { WHY="$TMP could not be fsynced ($(kerr))"; return 1; }
    held_mode "$TMP" 600 2>"$KERR" || { WHY="$TMP's owner and mode could not be set ($(kerr))"; return 1; }
    # -T: a link or a directory planted at the path is never written into.
    mv -fT "$TMP" "$f" 2>"$KERR" || { WHY="$f could not be replaced ($(kerr))"; return 1; }
    TMP=""
}

# follow — make the host's slots equal the ones read_secret holds.
# Returns 1 with ACTION naming the first thing that could not be done;
# every slot before it is whole and every slot after it untouched.
follow() {
    local slot v f wrote="" removed="" kept=""
    if [ ! -d "$DEST" ]; then
        mkdir -p -m 700 "$DEST" 2>"$KERR" \
            || { ACTION="NOT WRITTEN: $DEST could not be made ($(kerr)); nothing was replaced"; return 1; }
    fi
    held_mode "$DEST" 700 2>"$KERR" \
        || { ACTION="NOT WRITTEN: $DEST's owner and mode could not be held at root:root 0700 ($(kerr)); nothing was replaced"; return 1; }
    # A temp file a killed pass left: a oneshot never runs beside itself.
    rm -f -- "$DEST"/.current.?????? "$DEST"/.next.?????? "$DEST"/.previous.?????? 2>/dev/null || true
    for slot in $SLOTS; do
        case "$slot" in
            current) v="$S_current" ;;
            next) v="$S_next" ;;
            *) v="$S_previous" ;;
        esac
        f="$DEST/$slot"
        if [ -z "$v" ]; then
            if [ -e "$f" ] || [ -L "$f" ]; then
                rm -f -- "$f" 2>"$KERR" \
                    || { ACTION="NOT WRITTEN: $f, blank in the Secret, could not be removed ($(kerr))${wrote:+; written before it:$wrote}"; return 1; }
                removed="$removed $slot"
            fi
        elif [ -f "$f" ] && [ ! -L "$f" ] && [ "$(cat -- "$f" 2>/dev/null)" = "$v" ]; then
            held_mode "$f" 600 2>"$KERR" \
                || { ACTION="NOT WRITTEN: $f's owner and mode could not be held at root:root 0600 ($(kerr))"; return 1; }
            kept="$kept $slot"
        else
            put "$slot" "$v" \
                || { ACTION="NOT WRITTEN: slot $slot — $WHY${wrote:+; written before it:$wrote}"; return 1; }
            wrote="$wrote $slot(${#v} bytes)"
        fi
    done
    v=""
    sync -- "$DEST" 2>"$KERR" \
        || { ACTION="written, but $DEST could not be fsynced ($(kerr))"; return 1; }
    ACTION="follows the Secret:${wrote:+ wrote$wrote;}${removed:+ removed$removed;}${kept:+ unchanged$kept;}"
    ACTION="${ACTION%;}"
    return 0
}

# --- the destination, before anything is read --------------------------
if [ -L "$DEST" ]; then
    rc=1
    SECRET_STATE="not read: the destination was refused first"
    ACTION="REFUSED: $DEST is a symlink, not the deposit's own directory; nothing was read or written"
    finish
fi
if [ -e "$DEST" ] && [ ! -d "$DEST" ]; then
    rc=1
    SECRET_STATE="not read: the destination was refused first"
    ACTION="REFUSED: $DEST exists and is not a directory — the token is the \`current\` file of a directory (design 6805c764); nothing was read or written"
    finish
fi
HELD=0
if [ -f "$DEST/current" ] && [ -s "$DEST/current" ]; then HELD=1; fi

# --- 3 and 4. follow the Secret, and follow it again if it moved -------
if ! read_secret; then
    case "$SECRET_STATE" in
        "not ready"*) ;;
        *) rc=1 ;;
    esac
    ACTION="untouched: the Secret was not read, so every file here is as it was"
elif [ -z "$S_current" ] && [ -z "$S_next" ] && [ -z "$S_previous" ]; then
    SECRET_STATE="empty: the broker has not minted the machine token ($NS/$NAME holds no slot)"
    if [ "$HELD" -eq 1 ]; then
        # A Secret that held a token and now holds none is not a state
        # the rotation passes through. The host keeps what works.
        rc=1
        ACTION="untouched: the Secret is EMPTY while this host holds a current slot — kept, not removed"
    else
        ACTION="untouched: nothing to take"
    fi
elif [ -z "$S_current" ]; then
    # The first mint, between install and promotion: a `next` no caller
    # may send yet. Nothing a caller reads exists, so nothing is written.
    SECRET_STATE="staged: $SECRET_STATE — no current slot yet (the first mint is between install and promotion)"
    ACTION="untouched: no current slot to hold yet"
    [ "$HELD" -eq 0 ] || { rc=1; ACTION="untouched: the Secret holds NO current slot while this host holds one — kept, not removed"; }
else
    pass=0
    while :; do
        pass=$((pass + 1))
        if ! follow; then
            rc=1
            break
        fi
        seen_rv="$S_RV" seen_state="$SECRET_STATE"
        if ! read_secret; then
            rc=1
            ACTION="$ACTION — but the Secret could not be read AGAIN after the write ($SECRET_STATE), so a move under this pass was not ruled out; the next tick follows it"
            SECRET_STATE="$seen_state"
            break
        fi
        if [ "$S_RV" = "$seen_rv" ]; then
            SECRET_STATE="$seen_state"
            break
        fi
        if [ -z "$S_current" ]; then
            rc=1
            ACTION="$ACTION — then the Secret moved to hold NO current slot; the slots written are kept"
            break
        fi
        if [ "$pass" -ge 3 ]; then
            ACTION="$ACTION — and the Secret moved under each of $pass passes; the next tick follows it"
            break
        fi
        say "the Secret moved under pass $pass (a rotation step landed); following it now"
    done
    [ "$pass" -le 1 ] || ACTION="$ACTION (after $pass passes: the Secret moved mid-deposit)"
fi
S_current="" S_next="" S_previous=""

# --- 5. the effect -----------------------------------------------------
# The reader every caller on this host uses, reading the directory this
# script keeps — not a second way of reading it.
export BOSS_MACHINE_TOKEN_DIR="$DEST"
BOSS_USER='{"id":"automation:machine-token-deposit","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}'
API_CURL="$INFRA/boss-api-curl.sh"
[ -x "$API_CURL" ] || API_CURL=boss-api-curl.sh
verify() {
    local code answer matched mode degraded
    if [ ! -f "$DEST/current" ]; then
        EFFECT="nothing to present: this host holds no current slot, so its callers send without the token"
        return 0
    fi
    if [ -z "${BOSS_JOBS_URL:-}" ]; then
        EFFECT="UNVERIFIED: BOSS_JOBS_URL is unset (/etc/boss/sor.env), so no stamped read could be made; the next tick asks again"
        return 0
    fi
    MT_HDR=""
    if ! machine_token_header MT_HDR "$BOSS_JOBS_URL" 2>"$KERR"; then
        rc=1
        EFFECT="UNVERIFIED: the header file could not be written ($(kerr))"
        return 0
    fi
    if [ -z "$MT_HDR" ]; then
        rc=1
        EFFECT="NOT STAMPED: machine_token_header, reading $DEST, sent no token to the jobs API — $(kerr)"
        EFFECT="${EFFECT% — } (is the jobs API's host in BOSS_MACHINE_TOKEN_HOSTS of /etc/boss/sor.env?)"
        return 0
    fi
    # A dark API costs this pass twenty seconds, not the helper's 150:
    # the converge that deploys the fix is waiting behind this line.
    code=""
    if ! code="$(BOSS_API_RETRY_DEADLINE="${BOSS_DEPOSIT_VERIFY_WAIT:-$VERIFY_BOUND_S}" "$API_CURL" -sS --max-time "$VERIFY_BOUND_S" \
        -o "$BODY" -w '%{http_code}' -H "x-boss-user: $BOSS_USER" -H "$MT_HDR" \
        "$BOSS_JOBS_URL/api/machine-gate/accepts" 2>/dev/null)"; then
        code=""
    fi
    case "$code" in
        200) ;;
        '' | 000)
            EFFECT="UNVERIFIED: the jobs API did not answer GET /api/machine-gate/accepts; the next tick asks again"
            return 0
            ;;
        *)
            rc=1
            EFFECT="UNVERIFIED: GET /api/machine-gate/accepts answered HTTP $code"
            return 0
            ;;
    esac
    # A body that does not parse is its own answer, never an empty match.
    if ! answer="$(jq -r '[(.matched // "" | tostring), (.mode // "unknown" | tostring), (.degraded // false | tostring)] | join("|")' "$BODY" 2>/dev/null)"; then
        rc=1
        EFFECT="UNVERIFIED: GET /api/machine-gate/accepts answered 200 with a body that is not the gate's JSON"
        return 0
    fi
    IFS='|' read -r matched mode degraded <<< "$answer"
    case "$matched" in
        current) EFFECT="matched current: the jobs API's gate (mode $mode) admits this host's stamped read as the current slot" ;;
        next) EFFECT="matched next: the gate (mode $mode) knows this host's current slot as its next — its own mount is one refresh behind the promotion; accepted" ;;
        previous) EFFECT="matched previous: the gate (mode $mode) has promoted past the slot this host holds — the Secret moved after this pass read it; accepted, and the next tick follows" ;;
        none)
            rc=1
            EFFECT="NOT ACCEPTED: the gate (mode $mode) matches the value this host presents to NO slot — this host's callers are tallied as misses"
            ;;
        *)
            rc=1
            EFFECT="UNVERIFIED: GET /api/machine-gate/accepts answered 200 with no matched field"
            ;;
    esac
    [ "$degraded" != true ] || EFFECT="$EFFECT; the gate reports itself DEGRADED (it holds no readable slot of its own)"
    return 0
}
verify

finish
