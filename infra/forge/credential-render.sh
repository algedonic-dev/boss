#!/usr/bin/env bash
#
# credential-render — the forge host takes a GitHub App installation
# token the credential broker keeps in a k8s Secret and renders it into
# the root-only FILE its consumer reads, on every forge-converge tick.
#
#   credential-render.sh --rule <broker rule .toml> --dest <token file>
#
# WHY IT EXISTS (design 76155676, David 2026-09-27; backlog 81eb6d4d).
# The disaster-recovery copy of forge main is pushed to
# algedonic-dev/boss-dr by infra/forge/offsite-push.sh, which reads its
# GitHub token from a file (/etc/boss-publish/github-dr.token, root:root
# 0600). That token is a GitHub App INSTALLATION TOKEN: minted by the
# broker's `credential.rotate.github-app-installation` handler into Secret
# boss/github-dr-push-token, it lives ONE HOUR, and the broker re-mints it
# every quarter hour once fewer than forty minutes remain. So a file
# filled once is dead within the hour: this runs on EVERY tick, BEFORE the
# push (forge-converge.sh; the order is pinned by credential_render_sh.rs).
#
# ONE PASS. Read the rule's declaration (secret_namespace, secret_name,
# secret_key, credential_id — the rule is the one declaration; nothing
# here repeats it, CLAUDE.md §9a), then the Secret's token and the expiry
# the broker wrote beside it (`<key>.expires-at`) in one read, and leave
# the file in exactly one of two states:
#   - a LIVE token (expiry more than a minute ahead): written to a temp
#     file beside it with umask 077, renamed into place, its expiry beside
#     it in `<dest>.expires-at` — or left alone when it already holds that;
#   - NO FILE. A Secret that is empty (the root not yet placed), absent,
#     or holding an EXPIRED token (the refresh stopped) removes the file,
#     so the push meets an empty slot and refuses loudly (its exit 4) —
#     never a push with a dead token, and never a silent skip.
# When the Secret cannot be read at all (the admin kubeconfig not placed,
# or unreadable), a held token is KEPT while its own recorded expiry is
# live and removed once it is not — the same two states.
#
# NO VALUE EVER LEAVES A VARIABLE except into the file: it reaches disk
# through printf (a builtin), never an argv, and every message names the
# credential, the path, the last eight and the expiry — never the value.
#
# Exit: 0 done (including "empty", "absent" and "not ready"); 1 a fault
# this pass names on the converge packet (an expired token, an
# unreadable Secret or kubeconfig); 78 a refusal of how it was invoked.
#
#   credential-render.sh --rule <rule> --dest <token file> --expire <used token file>
#
# THE ACT IS OVER (design 76c46869, backlog bf8726c9). A per-act token —
# the algedonic-dev admin token infra/forge/github-act.sh acts with — is
# revoked by the verb once its write has read GitHub back, and a revoked
# token must never read as live: the broker's approval mint re-mints only
# when fewer than forty minutes remain, so a revoked token left under its
# old expiry would be handed to the next act approved within ~20 minutes.
# So, as the broker's own revoke does first, this sets the Secret's
# `<key>.expires-at` to now — ONLY while the Secret still holds the value
# the act used (the file names it; a token minted since for another act
# is left alone), and only on the object it read (a JSON patch that tests
# the resourceVersion, so a mint landing between the read and the write
# is not marked). The slot is removed either way: the next act renders
# its own. Exit 0 marked, left or absent; 1 not marked; 78 as above.
#
#   credential-render.sh --rule <rule> --dest <token file> --request <ops-request id> [--expire …]
#
# ONE TOKEN PER REQUEST (backlog 4ce4ec55; the review of car 57a2c56e,
# finding 1). The per-act admin token lived under ONE key per owner, and
# two GitHub writes approved before the first ran shared it: approval 2
# found approval 1's token fresh and minted nothing, write 1 marked the
# key expired and revoked the token, and write 2's plan re-run rendered
# the expired key and refused — its single-use approval spent. So a rule
# whose args name a `request_id` (the ops-request it fired for) keys its
# token `<secret_key>-<request id>`, and this renders — and with --expire
# marks — THAT key alone, for the request --request names (github-act.sh
# passes the OPS_REQUEST_ID the ops runner hands it). Such a rule without
# --request, a standing rule (the DR token's) with one, or a request that
# is not a lowercase packet uuid — it becomes part of a Secret key — is
# refused, 78.
#   The slot FILE is still one per owner, and a plan keeps it, so the
# next request often finds a sibling's live token there (the review of
# c21401da, F1). So a per-request render names its request beside the
# token (`<dest>.request`), and a Secret it cannot read keeps a held
# token only when the slot names THIS request — otherwise it removes it,
# and the act refuses on its empty slot rather than act with, then
# revoke, another request's token. And the key it reads must say it was
# minted for this request (`<key>.minted-for`, which the broker writes in
# the same patch as the token), or nothing is rendered (exit 1).
set -euo pipefail

ME=credential-render
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

RULE="" DEST="" USED_FILE="" REQUEST="" REQUEST_GIVEN=0
while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || refuse "$1 needs a value"
    case "$1" in
        --rule) RULE="$2" ;;
        --dest) DEST="$2" ;;
        --expire) USED_FILE="$2" ;;
        --request) REQUEST="$2" REQUEST_GIVEN=1 ;;
        *) refuse "unknown argument $1 (usage: $ME --rule FILE --dest FILE [--request OPS-REQUEST-ID] [--expire USED-TOKEN-FILE])" ;;
    esac
    shift 2
done
[ -n "$RULE" ] && [ -n "$DEST" ] || refuse "--rule and --dest are both required"
USED=""
if [ -n "$USED_FILE" ]; then
    [ ! -L "$USED_FILE" ] && [ -f "$USED_FILE" ] && [ -r "$USED_FILE" ] \
        || refuse "--expire names $USED_FILE, which is not a readable file holding the token the act used"
    IFS= read -r USED < "$USED_FILE" || [ -n "$USED" ] || refuse "cannot read $USED_FILE"
    [ -n "$USED" ] && [[ ! $USED =~ [[:space:]] ]] || refuse "$USED_FILE does not hold one token"
fi

# --- the declaration: the broker rule ----------------------------------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
HANDLER_LINE="$(grep -m1 '^handler = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
[ "$HANDLER_LINE" = 'handler = "credential.rotate.github-app-installation"' ] \
    || refuse "$RULE is not a GitHub App installation-token rule ($HANDLER_LINE); this renders only a token the broker mints"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
KEY="$(rule_arg secret_key)"
CRED="$(rule_arg credential_id)"
[ -n "$NS" ] && [ -n "$NAME" ] && [ -n "$KEY" ] && [ -n "$CRED" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name / secret_key / credential_id — cannot know what to render"
# A rule naming `request_id` keys its token per ops-request (header, ONE
# TOKEN PER REQUEST): the key is the request's own, or nothing is read.
PER_REQUEST_RE='(^|[{ ,])request_id = '
if [[ $ARGS_LINE =~ $PER_REQUEST_RE ]]; then
    [ "$REQUEST_GIVEN" -eq 1 ] \
        || refuse "$RULE mints a token per ops-request (its request_id arg), so its Secret holds no key but a request's own — name the request with --request <ops-request id>"
    [[ $REQUEST =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
        || refuse "--request '${REQUEST:0:80}' is not an ops-request id (a lowercase packet uuid) — it becomes part of the Secret key read"
    KEY="$KEY-$REQUEST"
elif [ "$REQUEST_GIVEN" -eq 1 ]; then
    refuse "$RULE keeps one standing key ($KEY) and names no request_id — --request names nothing there"
fi
EXPIRES_KEY="$KEY.expires-at"
MINTED_KEY="$KEY.minted-for"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
FIELD="render_${CRED//-/_}"

KC_STATE=""
if [ -n "${BOSS_RENDER_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_RENDER_KUBECTL"
else
    # The forge's admin kubeconfig, through the kubectl container every
    # forge script uses (kubectl is not on the host) — as
    # credential-deposit.sh reads the checkout token's Secret. ABSENT is
    # not ready (root material David places), not a red.
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    KUBECTL=(docker run --rm --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc)
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c)"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable"
    fi
fi

last8() { if [ "${#1}" -le 8 ]; then printf '%s' "$1"; else printf '%s' "${1: -8}"; fi; }
# live <rfc3339> — the instant is more than a minute ahead.
live() {
    local at
    [ -n "${1:-}" ] || return 1
    at="$(date -u -d "$1" +%s 2>/dev/null)" || return 1
    [ "$at" -gt $(($(date -u +%s) + 60)) ]
}
drop() { rm -f -- "$DEST" "$DEST.expires-at" "$DEST.request"; }
# put <file> <value> — umask 077, temp beside it, renamed into place; -T
# so a link planted at the path is replaced, never written through.
put() {
    local tmp
    tmp="$(mktemp "$(dirname "$1")/.$(basename "$1").XXXXXX")"
    (umask 077 && printf '%s' "$2" > "$tmp")
    chmod 600 "$tmp"
    mv -fT "$tmp" "$1"
}

HELD_EXPIRES="" HELD_REQUEST=""
if [ ! -L "$DEST.expires-at" ] && [ -s "$DEST.expires-at" ]; then
    HELD_EXPIRES="$(cat -- "$DEST.expires-at")"
fi
if [ ! -L "$DEST.request" ] && [ -s "$DEST.request" ]; then
    HELD_REQUEST="$(cat -- "$DEST.request")"
fi
# held_is_ours — the slot holds this invocation's token. The slot file is
# per OWNER and a plan keeps it, so a per-request render must not take a
# token some other request's render left there (the review of c21401da,
# F1: request 2 acted with request 1's token and then revoked it). Each
# per-request render writes `<dest>.request` beside the token, and only a
# slot naming THIS request is its own; a standing slot has one owner.
held_is_ours() { [ "$REQUEST_GIVEN" -eq 0 ] || [ "$HELD_REQUEST" = "$REQUEST" ]; }
# keep_or_drop <why> — the Secret could not be read: a held token stays
# only while its own recorded expiry is live, and only if it is ours.
keep_or_drop() {
    if [ ! -L "$DEST" ] && [ -s "$DEST" ] && live "$HELD_EXPIRES" && held_is_ours; then
        STATE="kept: $1; the held token is valid until $HELD_EXPIRES"
    else
        drop
        STATE="removed: $1, and no live token of this $([ "$REQUEST_GIVEN" -eq 1 ] && echo "request ($REQUEST)" || echo credential) is held — the push refuses on its empty slot"
    fi
}

rc=0 STATE="" TOKEN="" KERR="$(mktemp)"
trap 'rm -f "$KERR"' EXIT

# --- --expire: the act that used the token is over ----------------------
if [ -n "$USED_FILE" ]; then
    drop
    kerr() { head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300; }
    STILL="the Secret records a live expiry for a revoked token until the broker next mints"
    if [ -n "$KC_STATE" ]; then
        rc=1
        STATE="not marked: $KC_STATE — $STILL"
    elif READ="$("${KUBECTL[@]}" -n "$NS" get secret "$NAME" \
        -o "jsonpath={.data.${KEY//./\\.}}|{.metadata.resourceVersion}" 2>"$KERR")"; then
        TOKEN_B64="${READ%%|*}" RV="${READ#*|}"
        [ "$READ" != "$TOKEN_B64" ] || RV=""
        if ! TOKEN="$(printf '%s' "$TOKEN_B64" | base64 -d 2>/dev/null)"; then
            TOKEN="" rc=1
            STATE="not marked: key $KEY of $NS/$NAME does not decode, so whether it holds the token the act used cannot be read — $STILL"
        elif [ "$TOKEN" != "$USED" ]; then
            HELD="no token"
            [ -z "$TOKEN" ] || HELD="…$(last8 "$TOKEN")"
            STATE="left: $NS/$NAME holds $HELD, not the token the act used (…$(last8 "$USED")) — minted for another act since; its expiry stands"
        elif [[ ! $RV =~ ^[0-9]+$ ]]; then
            rc=1
            STATE="not marked: $NS/$NAME answered no resourceVersion ('$RV') to make the write conditional on — $STILL"
        else
            NOW="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            NOW_B64="$(printf '%s' "$NOW" | base64 -w0)"
            PATCH="[{\"op\":\"test\",\"path\":\"/metadata/resourceVersion\",\"value\":\"$RV\"},{\"op\":\"add\",\"path\":\"/data/$EXPIRES_KEY\",\"value\":\"$NOW_B64\"}]"
            if "${KUBECTL[@]}" -n "$NS" patch secret "$NAME" --type json -p "$PATCH" >/dev/null 2>"$KERR"; then
                STATE="marked expired: …$(last8 "$USED") in $NS/$NAME reads expired at $NOW — the broker's next mint replaces it"
            else
                rc=1
                STATE="not marked: $NS/$NAME moved since it was read, or refused the write ($(kerr)) — $STILL"
            fi
        fi
    elif grep -q 'NotFound' "$KERR"; then
        STATE="absent: Secret $NS/$NAME does not exist, so nothing holds the token the act used"
    else
        rc=1
        STATE="not marked: $(kerr) — $STILL"
    fi
    TOKEN="" USED=""
    say "$CRED → $DEST: $STATE"
    run_summary_field "$FIELD" "$STATE"
    exit "$rc"
fi

if [ -n "$KC_STATE" ]; then
    case "$KC_STATE" in unreadable*) rc=1 ;; esac
    keep_or_drop "$KC_STATE"
elif READ="$("${KUBECTL[@]}" -n "$NS" get secret "$NAME" \
    -o "jsonpath={.data.${KEY//./\\.}}|{.data.${EXPIRES_KEY//./\\.}}|{.data.${MINTED_KEY//./\\.}}" 2>"$KERR")"; then
    # Base64 holds no '|': three fields, any of them empty when absent.
    IFS='|' read -r TOKEN_B64 EXPIRES_B64 MINTED_B64 <<<"$READ"
    MINTED=""
    if [ -n "$MINTED_B64" ] && ! MINTED="$(printf '%s' "$MINTED_B64" | base64 -d 2>/dev/null)"; then
        MINTED="not base64"
    fi
    EXPIRES=""
    # An expiry that does not decode is named as such, and is not live.
    if [ -n "$EXPIRES_B64" ] && ! EXPIRES="$(printf '%s' "$EXPIRES_B64" | base64 -d 2>/dev/null)"; then
        EXPIRES="not base64"
    fi
    if [ -z "$TOKEN_B64" ]; then
        drop
        STATE="empty: the broker has not filled key $KEY of $NS/$NAME (is the GitHub App root placed in boss-credential-broker-root?) — the push refuses on its empty slot"
    elif ! TOKEN="$(printf '%s' "$TOKEN_B64" | base64 -d 2>/dev/null)" \
        || [ -z "$TOKEN" ] || [[ $TOKEN =~ [[:space:]] ]]; then
        TOKEN="" rc=1
        drop
        STATE="unreadable: key $KEY of $NS/$NAME is not one token"
    elif ! live "$EXPIRES"; then
        drop
        rc=1
        STATE="expired: $NS/$NAME holds a token (…$(last8 "$TOKEN")) whose recorded expiry '${EXPIRES:-none}' is not live — the broker's refresh has stopped; the push refuses on its empty slot"
    elif [ "$REQUEST_GIVEN" -eq 1 ] && [ "$MINTED" != "$REQUEST" ]; then
        # The key is named after the request, and the broker writes whom it
        # minted the token for beside it in the same patch: the two agree,
        # or the token is someone else's however it got there.
        drop
        rc=1
        STATE="not ours: key $KEY of $NS/$NAME holds a token (…$(last8 "$TOKEN")) minted for '${MINTED:-nothing recorded}', not request $REQUEST — never rendered; the act refuses on its empty slot"
    elif [ ! -L "$DEST" ] && [ -s "$DEST" ] && [ "$(cat -- "$DEST")" = "$TOKEN" ] \
        && [ "$HELD_EXPIRES" = "$EXPIRES" ] && held_is_ours; then
        STATE="held: …$(last8 "$TOKEN"), valid until $EXPIRES"
    else
        mkdir -p -m 700 "$(dirname "$DEST")"
        # A slot is never left naming one request beside another's token:
        # another's is removed first, then the request is named, then its
        # token written — a death between leaves a named, empty slot.
        if [ "$REQUEST_GIVEN" -eq 1 ]; then
            held_is_ours || drop
            put "$DEST.request" "$REQUEST"
        fi
        put "$DEST" "$TOKEN"
        put "$DEST.expires-at" "$EXPIRES"
        STATE="rendered: …$(last8 "$TOKEN"), valid until $EXPIRES"
    fi
    TOKEN=""
elif grep -q 'NotFound' "$KERR"; then
    keep_or_drop "absent: Secret $NS/$NAME does not exist (the cluster converge creates it from the rule)"
else
    rc=1
    keep_or_drop "unreadable: $(head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300)"
fi

say "$CRED → $DEST: $STATE"
run_summary_field "$FIELD" "$STATE"
exit "$rc"
