#!/usr/bin/env bash
#
# registry-token-deposit — the forge host takes the broker-minted
# REGISTRY PUSH token out of its k8s Secret and into a ROOT-ONLY docker
# config, proves it by a login the registry accepts, and records
# delivery so the broker may revoke the old one.
#
#   registry-token-deposit.sh --rule <broker rule .toml> --dest <directory>
#
# WHY IT EXISTS (design c8502e17, question image-build, David 2026-10-07;
# backlog 683d7fe9, the first of its two cars). The cluster image is
# pushed to the forge's own registry with a `docker login` the operator
# once ran by hand: a token in ~david/.docker/config.json that no
# registry row names and no rotation reaches, held by an account a car's
# probe ran as until the probe account landed. The decision moves the
# build to root and the system docker daemon with a token the broker
# mints. This script is the host's half of that token: the broker's
# `credential.rotate.forgejo-registry` handler mints it (write:package,
# proved by a push the registry accepts) into Secret
# boss/forge-registry-push-token, and this takes it from there.
#
# WHAT READS IT (the item's second car). cluster-deploy-runner pushes
# with it, and it and disk-floor-sweep verify a tag against the registry
# with it, as root on the system daemon (DOCKER_CONFIG=<this directory>,
# forge-defaults.sh REGISTRY_PUSH_DIR); mirror-base-images.sh and the
# registry prune and data move read it too. The converge refuses to
# build when this file is absent, so that car does not land before a
# rotation has been minted, delivered here and proven on this host —
# the `registry_token_effect` line below is where that is read.
#
# WHAT IT WRITES. <dest>/config.json, a docker config holding ONE login:
#   {"auths": {"<registry host>": {"auth": "<base64 user:token>"}}}
# — the shape `docker login` writes and infra/forge/registry-login.lib.sh
# reads (the prune's reader, which is how this script reads back what it
# holds: one parse, CLAUDE.md §9a). The directory is root:root 0700 and
# the file 0600, held on every pass, the way machine-token-deposit.sh
# holds /etc/boss/machine-token. A directory, not a file, because docker
# takes its config as a directory (`docker --config`, DOCKER_CONFIG).
#
# ROOT-ONLY, AND WHAT THAT IS WORTH HERE. A recorded probe runs on this
# host as `boss-probe` (infra/forge/probe-account.sh), which holds no
# sudo and no docker; root:root 0700 keeps this from it and from the
# operator's own account. It does not keep it from an account that can
# become root: the operator's account may still hold `sudo -n docker` on
# the host, which reaches any root file. The second car took that road
# away from unattended code — nothing the units run needs it any more —
# and removing the rule itself is the host's owner's act; this file does
# not claim it.
#
# ONE PASS:
#   1. Read the rule's declaration — secret_namespace, secret_name,
#      secret_key, forge_user, the credential id off its `when`. The rule
#      is the one declaration; nothing here repeats it. A rule of any
#      other handler is refused: this deposits the registry kind only.
#   2. Read the Secret's key through the admin kubeconfig, bounded. It
#      must be one token word, or nothing on this host changes.
#   3. A value that differs from the one held is PROVED before it is
#      written: `GET <registry>/v2/token?service=container_registry`
#      with the login, the exchange `docker login` makes, must answer 200
#      with a token. Only then is config.json written — umask 077 into a
#      temp file beside it, fsynced, renamed (`mv -T`: a link planted at
#      the path is replaced, never written through). A value the
#      registry refuses changes nothing and is named.
#   4. Whatever is held is proved the same way on every pass, so the
#      packet always says whether this host could push right now.
#   5. When the host holds the Secret's value and the registry accepts
#      it, an open rotation packet of this credential whose `delivered`
#      step is ready is completed with the last eight — the record the
#      broker's revoke waits for (infra/lib/credential-delivery.sh, the
#      one definition the checkout token's deposit shares).
#
# NOTHING HERE IS EVER REQUIRED. An absent, empty, unreadable or
# malformed Secret, an unplaced kubeconfig, a registry that does not
# answer and an unwritable destination each leave every existing file as
# it was and say so. forge-converge.sh carries this script's exit onto
# its packet and NEVER into its own (CLAUDE.md §Diagnosis, "an arm that
# needs the patient is not an arm").
#
# NO VALUE EVER LEAVES A VARIABLE except into config.json and the 0600
# header file the login is proved with (infra/lib/secret-header.sh's
# private directory, removed on exit): never an argv, a message, a
# summary field or a request line.
# Every line names a path, a length or a LAST EIGHT — the identifier the
# forge shows beside each token, which is how the scoper of a later
# rotation names this credential's old instance without holding it
# (design 1c90d183, D3; `registry_token_last_eight` on the packet).
#
# What a value still passes through, stated and not closed: kubectl's
# stdout carries the base64 value through docker's json-file log for the
# life of the `--rm` container, root-only, as the sibling deposits' does.
#
# EVERY WAIT IS BOUNDED (the machine token deposit's review 9a1e289b,
# B1): each Secret read by kubectl's --request-timeout inside the
# container and `timeout -k` outside, each registry and jobs-API call by
# curl's --max-time, and forge-converge.sh bounds the whole script again.
#
# Exit: 0 done, or nothing to take (not minted, not ready); 1 a fault
# this pass names; 78 a refusal of how it was invoked.

# TRACING OFF, FIRST: an exported SHELLOPTS=xtrace would print every
# expansion, the token among them.
set +x
set -euo pipefail

ME=registry-token-deposit
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
HANDLER=credential.rotate.forgejo-registry

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
say() { echo "$ME: $*"; }

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

# WHICH MACHINE THIS IS, BEFORE ANYTHING IS READ OR WRITTEN
# (infra/lib/host-check.sh carries the incident and the rule). Aimed at
# the directory the forge's build will read, the machine must hold the
# address the estate declares for the forge, or the exit is 78 and
# nothing is read from the cluster or written here. Aimed anywhere else
# it is a test's directory and no identity is asked.
# shellcheck source=infra/lib/host-check.sh
. "$INFRA/lib/host-check.sh"
host_path --dest "$DEST" /etc/boss/registry-push
host_check forge "$ME"

# --- 1. the declaration: the broker rule -------------------------------
[ -r "$RULE" ] || refuse "cannot read the broker rule $RULE"
HANDLER_LINE="$(grep -m1 '^handler = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
[ "$HANDLER_LINE" = "handler = \"$HANDLER\"" ] \
    || refuse "$RULE is not a registry push token's rule ($HANDLER_LINE); this deposits the $HANDLER kind only"
ARGS_LINE="$(grep -m1 '^args = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
WHEN_LINE="$(grep -m1 '^when = ' "$RULE")" || [ $? -eq 1 ] || refuse "cannot read $RULE"
rule_arg() {
    local re="(^|[{ ,])$1 = \"\\\\\"([^\"\\\\]*)\\\\\"\""
    if [[ $ARGS_LINE =~ $re ]]; then printf '%s' "${BASH_REMATCH[2]}"; fi
}
NS="$(rule_arg secret_namespace)"
NAME="$(rule_arg secret_name)"
KEY="$(rule_arg secret_key)"
FORGE_USER="$(rule_arg forge_user)"
DELIVERY="$(rule_arg delivery)"
CRED=""
when_re='subject_id = "([^"]+)"'
if [[ $WHEN_LINE =~ $when_re ]]; then CRED="${BASH_REMATCH[1]}"; fi
[ -n "$NS" ] && [ -n "$NAME" ] && [ -n "$KEY" ] && [ -n "$FORGE_USER" ] && [ -n "$CRED" ] \
    || refuse "$RULE declares no literal secret_namespace / secret_name / secret_key / forge_user, or no subject_id in its when — cannot know what to deposit"
[ "$DELIVERY" = off-host ] \
    || refuse "$RULE declares delivery = '${DELIVERY:-mount}': this host takes the value from the Secret on its own pass, so the rule must make the revoke wait for it (off-host)"
# The user is half of the login this writes: held to the shape the
# broker's own grammar admits, so it cannot carry a `:` into user:token.
[[ $FORGE_USER =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,39}$ ]] \
    || refuse "$RULE declares forge_user '$FORGE_USER', which is not a forge username"

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/sor.sh
. "$INFRA/lib/sor.sh"
# shellcheck source=infra/lib/secret-header.sh
. "$INFRA/lib/secret-header.sh"
# shellcheck source=infra/forge/registry-login.lib.sh
. "$HERE/registry-login.lib.sh"

FILE="$DEST/config.json"
KERR="$(mktemp)"
WORK="$(mktemp -d)"
chmod 700 "$WORK"
TMP=""
cleanup() {
    rm -f "$KERR"
    rm -rf "$WORK"
    if [ -n "$TMP" ]; then rm -f "$TMP"; fi
}
# Set BEFORE the first machine_token_header call, which chains its own
# cleanup in front of this one (infra/lib/secret-header.sh, ORDER).
trap cleanup EXIT

rc=0
SECRET_VALUE="" READ_VALUE="" READ_STATE="" CUR=""
SECRET_STATE="" ACTION="untouched" EFFECT="not asked" DELIVERY_STATE="nothing to record"

last8() { if [ "${#1}" -le 8 ]; then printf '%s' "$1"; else printf '%s' "${1: -8}"; fi; }
kerr() { head -n 1 "$KERR" | cut -c1-200; }

# finish — the one way out after the declaration: what this pass found,
# did and proved, on the converge packet and in the journal.
finish() {
    local held=none
    [ -z "$CUR" ] || held="$(last8 "$CUR")"
    SECRET_VALUE="" READ_VALUE="" CUR="" AUTH_B64="" TOKEN=""
    run_summary_field registry_token_last_eight "$held"
    run_summary_field registry_token_secret "$SECRET_STATE"
    run_summary_field registry_token_action "$ACTION"
    run_summary_field registry_token_effect "$EFFECT"
    run_summary_field registry_token_delivery "$DELIVERY_STATE"
    say "secret $NS/$NAME: $SECRET_STATE"
    say "file $FILE: $ACTION"
    say "effect: $EFFECT"
    say "delivery: $DELIVERY_STATE"
    say "the registry login this host holds ends in $held"
    if [ "$rc" -ne 0 ]; then
        echo "$ME: FAULT (exit $rc) — the lines above name it; what it found is as it found it unless a line above says it was written" >&2
    fi
    exit "$rc"
}

# --- the registry ------------------------------------------------------
# The host:port comes from /etc/boss/sor.env, the one place the estate's
# forge address is rendered (infra/lib/sor.sh). Not sor_require: that
# exits with no line on this run's packet.
REG_HOST="${BOSS_FORGE_REGISTRY_HOST:-}"
REG_URL="${BOSS_REGISTRY_DEPOSIT_URL:-http://$REG_HOST}"
VERIFY_BOUND_S="${BOSS_DEPOSIT_VERIFY_BOUND_S:-15}"
READ_BOUND_S="${BOSS_DEPOSIT_READ_BOUND_S:-25}"
case "$READ_BOUND_S$VERIFY_BOUND_S" in
    '' | *[!0-9]*) refuse "BOSS_DEPOSIT_READ_BOUND_S / BOSS_DEPOSIT_VERIFY_BOUND_S must be whole seconds" ;;
esac

# login_accepted VALUE — the exchange `docker login` makes, with the
# login in the 0600 header file infra/lib/secret-header.sh keeps (never
# an argv, where every local user reads it in ps). 0 accepted; 1 with
# LOGIN_WHY set, and LOGIN_VERDICT `refused` (the registry said no: 401
# or 403) or `unanswered` (anything else — proves nothing either way).
LOGIN_WHY="" LOGIN_VERDICT="" REG_HDR=""
login_accepted() {
    local body="$WORK/login.json" code="" login
    LOGIN_WHY="" LOGIN_VERDICT="" REG_HDR=""
    login="$(printf '%s:%s' "$FORGE_USER" "$1" | base64 -w0)"
    if ! secret_header REG_HDR "Authorization: Basic $login" 2>"$KERR"; then
        LOGIN_VERDICT=unanswered
        LOGIN_WHY="the login's header file could not be written ($(kerr))"
        return 1
    fi
    login=""
    if ! code="$(curl -sS --max-time "$VERIFY_BOUND_S" -H "$REG_HDR" -o "$body" -w '%{http_code}' \
        "$REG_URL/v2/token?service=container_registry" 2>/dev/null)"; then
        code=""
    fi
    # The header file lives in secret-header.sh's 0700 directory until
    # this script exits, which removes it (its chained EXIT trap).
    case "$code" in
        200)
            if [ -n "$(jq -r '.token // empty' "$body" 2>/dev/null)" ]; then
                rm -f "$body"
                return 0
            fi
            LOGIN_VERDICT=unanswered
            LOGIN_WHY="$REG_URL/v2/token answered 200 with no token"
            ;;
        401 | 403)
            LOGIN_VERDICT=refused
            LOGIN_WHY="$REG_URL/v2/token answered HTTP $code to the login as $FORGE_USER"
            ;;
        '' | 000)
            LOGIN_VERDICT=unanswered
            LOGIN_WHY="$REG_URL/v2/token did not answer inside $VERIFY_BOUND_S seconds"
            ;;
        *)
            LOGIN_VERDICT=unanswered
            LOGIN_WHY="$REG_URL/v2/token answered HTTP $code"
            ;;
    esac
    rm -f "$body"
    return 1
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
    ACTION="REFUSED: $DEST exists and is not a directory — docker takes its config as a directory; nothing was read or written"
    finish
fi
if [ -L "$FILE" ] || { [ -e "$FILE" ] && [ ! -f "$FILE" ]; }; then
    rc=1
    SECRET_STATE="not read: the destination was refused first"
    ACTION="REFUSED: $FILE is not the deposit's own file (a link, a directory or a device stands there); nothing was read or written"
    finish
fi
if [ -z "$REG_HOST" ]; then
    rc=1
    SECRET_STATE="not read: the registry is not known"
    ACTION="untouched: BOSS_FORGE_REGISTRY_HOST is unset ($SOR_ENV), so there is no registry to hold a login for"
    finish
fi

# What the host holds now, read the one way the tree reads a docker login.
if [ -f "$FILE" ]; then
    if registry_login_read "$FILE" "$REG_HOST" && [ "$CRED_USER" = "$FORGE_USER" ]; then
        CUR="$TOKEN"
    else
        # Not fatal: an unreadable file is replaced by a value that proves.
        say "the file held is not a login for $REG_HOST as $FORGE_USER (${REGISTRY_LOGIN_WHY:-it names another user}); it is replaced only by a value the registry accepts"
    fi
fi

# --- 2. the Secret -----------------------------------------------------
KC_STATE=""
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    read -r -a KUBECTL <<< "$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig through the kubectl container every
    # forge script uses (kubectl is not on the host). ABSENT is not ready
    # (root material David places, design 835c0c9c), never a red.
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    # No container log: the Secret kubectl prints is this script's alone
    # (backlog aa700921).
    KUBECTL=(docker run --rm --log-driver=none --network host -v "$KC:/kc:ro" alpine/k8s:1.33.3 kubectl --kubeconfig=/kc --request-timeout=15s)
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        KC_STATE="not ready: $KC absent — root material, placed by David (design 835c0c9c)"
    elif [ ! -r "$KC" ]; then
        KC_STATE="unreadable: $KC is present but unreadable"
    fi
fi
# read_secret — one read of the declared key into READ_VALUE (the value,
# or empty) and READ_STATE (what a record may say about it). A function
# because the delivery record reads it a second time.
read_secret() {
    local b64 read_rc=0
    READ_VALUE="" READ_STATE="$KC_STATE"
    [ -z "$READ_STATE" ] || return 0
    b64="$(timeout -k 5 "$READ_BOUND_S" "${KUBECTL[@]}" -n "$NS" get secret "$NAME" -o "jsonpath={.data.$KEY}" 2>"$KERR")" || read_rc=$?
    if [ "$read_rc" -eq 124 ] || [ "$read_rc" -eq 137 ]; then
        READ_STATE="unreadable: the read of $NS/$NAME did not answer inside $READ_BOUND_S seconds and was stopped (a stalled cluster API or docker daemon)"
    elif [ "$read_rc" -eq 0 ]; then
        if [ -z "$b64" ]; then
            READ_STATE="empty: the broker has not minted the registry push token ($NS/$NAME holds no $KEY)"
        elif ! READ_VALUE="$(printf '%s' "$b64" | base64 -d 2>/dev/null)"; then
            READ_VALUE=""
            READ_STATE="unreadable: key $KEY of $NS/$NAME is not base64"
        elif [[ ! $READ_VALUE =~ ^[A-Za-z0-9_-]+$ ]] || [ "${#READ_VALUE}" -gt 4096 ]; then
            READ_STATE="unreadable: key $KEY of $NS/$NAME is not one token word (${#READ_VALUE} bytes)"
            READ_VALUE=""
        else
            READ_STATE="held:$(last8 "$READ_VALUE")"
        fi
    elif grep -q 'NotFound' "$KERR"; then
        READ_STATE="absent: Secret $NS/$NAME does not exist (the cluster converge creates it from the rule)"
    else
        READ_STATE="unreadable: $(head -n 3 "$KERR" | tr '\n' ' ' | cut -c1-300)"
    fi
}
read_secret
SECRET_VALUE="$READ_VALUE" SECRET_STATE="$READ_STATE"
case "$SECRET_STATE" in unreadable*) rc=1 ;; esac

# held_mode PATH MODE — MODE, and root:root when this runs as root.
held_mode() {
    chmod "$2" "$1" || return 1
    if [ "$(id -u)" -eq 0 ]; then chown 0:0 "$1" || return 1; fi
}

# put VALUE — the login for VALUE into $FILE, durable before it is
# visible. Returns 1 with WHY set; the old file is then still whole.
WHY=""
put() {
    WHY=""
    if [ ! -d "$DEST" ]; then
        mkdir -p -m 700 "$DEST" 2>"$KERR" || { WHY="$DEST could not be made ($(kerr))"; return 1; }
    fi
    held_mode "$DEST" 700 2>"$KERR" || { WHY="$DEST's owner and mode could not be held at root:root 0700 ($(kerr))"; return 1; }
    # A temp file a killed pass left: a oneshot never runs beside itself.
    rm -f -- "$DEST"/.config.json.?????? 2>/dev/null || true
    TMP="$(mktemp "$DEST/.config.json.XXXXXX" 2>"$KERR")" \
        || { TMP=""; WHY="a temp file in $DEST could not be made ($(kerr))"; return 1; }
    # The login reaches jq on stdin, never in its argv.
    (umask 077 && printf '%s:%s' "$FORGE_USER" "$1" | base64 -w0 \
        | jq -Rn --arg host "$REG_HOST" '{auths: {($host): {auth: input}}}' > "$TMP") 2>"$KERR" \
        || { WHY="$TMP could not be written ($(kerr))"; return 1; }
    sync -- "$TMP" 2>"$KERR" || { WHY="$TMP could not be fsynced ($(kerr))"; return 1; }
    held_mode "$TMP" 600 2>"$KERR" || { WHY="$TMP's owner and mode could not be set ($(kerr))"; return 1; }
    # -T: a link or a directory planted at the path is never written into.
    mv -fT "$TMP" "$FILE" 2>"$KERR" || { WHY="$FILE could not be replaced ($(kerr))"; return 1; }
    TMP=""
    sync -- "$DEST" 2>"$KERR" || { WHY="$FILE was written, but $DEST could not be fsynced ($(kerr))"; return 1; }
}

# --- 3. a new value is proved, then written ----------------------------
PROVED=""
if [ -z "$SECRET_VALUE" ]; then
    case "$SECRET_STATE" in
        empty* | absent*)
            if [ -n "$CUR" ]; then
                # A Secret that held a token and now holds none is not a
                # state a rotation passes through. The host keeps what works.
                rc=1
                ACTION="untouched: the Secret holds NO token while this host holds a login — kept, not removed"
            else
                ACTION="untouched: nothing to take"
            fi
            ;;
        *) ACTION="untouched: the Secret was not read, so the file here is as it was" ;;
    esac
elif [ "$SECRET_VALUE" = "$CUR" ]; then
    if [ -d "$DEST" ] && held_mode "$DEST" 700 2>"$KERR" && held_mode "$FILE" 600 2>"$KERR"; then
        ACTION="unchanged: the host holds the Secret's value (…$(last8 "$CUR"))"
    else
        rc=1
        ACTION="unchanged, but $DEST and $FILE could not be held at root:root 0700 and 0600 ($(kerr))"
    fi
elif login_accepted "$SECRET_VALUE"; then
    if put "$SECRET_VALUE"; then
        CUR="$SECRET_VALUE"
        PROVED="$CUR"
        ACTION="installed from the Secret (…$(last8 "$CUR")), proved by a login before it was written"
    else
        rc=1
        ACTION="NOT WRITTEN: $WHY; the file here is as it was"
    fi
else
    case "$LOGIN_VERDICT" in
        refused)
            rc=1
            ACTION="NOT WRITTEN: the registry refused the Secret's value (…$(last8 "$SECRET_VALUE")) — $LOGIN_WHY; the file here is as it was"
            ;;
        *) ACTION="not written yet: the Secret's value (…$(last8 "$SECRET_VALUE")) could not be proved — $LOGIN_WHY; the next pass asks again" ;;
    esac
fi

# --- 4. what the host holds, proved on every pass ----------------------
if [ -z "$CUR" ]; then
    EFFECT="nothing to present: this host holds no registry login"
elif [ "$PROVED" = "$CUR" ] || login_accepted "$CUR"; then
    PROVED="$CUR"
    EFFECT="accepted: $REG_URL/v2/token answers the login this host holds (as $FORGE_USER, …$(last8 "$CUR")) with a token"
elif [ "$LOGIN_VERDICT" = refused ]; then
    rc=1
    EFFECT="NOT ACCEPTED: the registry refuses the login this host holds (…$(last8 "$CUR")) — $LOGIN_WHY"
else
    EFFECT="UNVERIFIED: $LOGIN_WHY; the next pass asks again"
fi

# --- 5. the delivery record --------------------------------------------
# shellcheck source=infra/lib/credential-delivery.sh
. "$INFRA/lib/credential-delivery.sh"
# The proof the record asks for is the one this pass already made: the
# registry accepted exactly the value held.
delivery_proof() {
    [ -n "$PROVED" ] && [ "$PROVED" = "$CUR" ] && return 0
    DELIVERY_PROOF_WHY="$LOGIN_WHY"
    return 1
}
if [ -n "$SECRET_VALUE" ] && [ "$CUR" = "$SECRET_VALUE" ]; then
    # A dark jobs API costs each of the record's three calls this many
    # seconds, not the helper's 150: this script is bounded as a whole by
    # forge-converge.sh, and a record cut off mid-write is retried whole
    # by the next pass.
    export BOSS_API_RETRY_DEADLINE="${BOSS_DEPOSIT_RECORD_WAIT:-20}"
    record_credential_delivery "$FILE"
fi

finish
