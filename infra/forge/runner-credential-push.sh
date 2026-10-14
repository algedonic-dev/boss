#!/usr/bin/env bash
#
# runner-credential-push — the forge's half of delivering boss-gcp its ops
# runner credential (backlog 1e50e66b). forge-converge runs it on every
# tick, after the forge's own deposit (runner-credential-deposit.sh).
#
#   runner-credential-push.sh        (no arguments)
#
# WHY THE FORGE. The deposit key's scope, signed by David on its
# prepare-a-deposit-key packet (c7f12e6f, 2026-10-09), names its consumer:
# "the forge runner-credential sender (infra/gcp/runner-credential-send.sh)".
# And the forge already holds every read this needs — its admin kubeconfig
# (root material, /etc/boss-ops/kubeconfig) reads Secret
# boss/ops-runner-credential for its own deposit every tick — so the
# delivery adds no new holder of any runner's credential: no ServiceAccount,
# no Role, no mount. The sender is the transport; this script is its
# deployment, and nothing else.
#
# ONE PASS:
#   1. The transport key, Secret boss/runner-deposit-key (`private_key`, and
#      `minted_for`: the prepare-a-deposit-key packet the broker prepared it
#      on, whose signed enroll step the sender checks the key against). No
#      Secret or no key is `not yet`: the broker has not prepared one.
#   2. boss-gcp's pinned host key and the deposit account, ConfigMap
#      boss/break-glass-deposit-known-hosts — the one pin every push to
#      boss-gcp uses (the break-glass deposit's and the machine token
#      push's): one host, one pin (CLAUDE.md §9a). Absent is a red: a key
#      that may reach the receiver must never reach an unpinned host.
#   3. The sender, with those in a private scratch directory (this unit's
#      RuntimeDirectory, a tmpfs, when it has one) removed on every exit, and
#      the time the forge's tree last changed the receiver, for its bounded
#      `not yet` while boss-gcp's converge catches up.
#
# EVERY END NAMES ITS CAUSE on the forge-converge packet, as
# `runner_credential_boss_gcp`: delivered, verified, nothing to do, not yet
# and why, or the sender's own RED line. The sender never prints a value,
# so its stderr goes to the journal whole.
#
# Exit: 0 delivered, verified, nothing to do, or not yet; anything else is
# the sender's (1 failed or RED, 78 refused), or 1 when this pass could not
# read its own inputs — carried by forge-converge, which reds the run.
set +x
set -euo pipefail

ME=runner-credential-push
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
REPO="$(cd "$INFRA/.." && pwd)"
[ "$#" -eq 0 ] || { echo "$ME: REFUSED — takes no arguments" >&2; exit 78; }

# shellcheck source=infra/run-summary.sh
. "$INFRA/run-summary.sh"
# shellcheck source=infra/lib/jq.sh
. "$INFRA/lib/jq.sh"

# finish RC LINE — the one way out: the line on the packet, then the exit.
finish() {
    run_summary_field runner_credential_boss_gcp "$2"
    echo "$ME: $2"
    exit "$1"
}
# The first lines of a captured stderr, as one bounded printable line.
first_lines() { head -n 3 "$1" | tr '\n' ' ' | LC_ALL=C tr -c '[:print:]' '?' | cut -c1-300; }

# --- the cluster, as the forge reaches it ------------------------------
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    KUBECTL_LINE="$BOSS_DEPOSIT_KUBECTL"
else
    # The forge's admin kubeconfig through the kubectl container every
    # forge script uses (kubectl is not on the host); the same line
    # runner-credential-deposit.sh reads with. ABSENT is not ready — root
    # material David places (backlog 714bc71f) — never a red. With no
    # container log: the deposit PRIVATE key this reads, and the runner
    # credentials the sender reads through this same line, reach the
    # script through the attach and never the docker daemon's log driver
    # (backlog aa700921; the system daemon logs json-file, measured
    # 2026-10-10).
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    if [ ! -e "$KC" ] && [ ! -L "$KC" ]; then
        finish 0 "not ready: $KC absent — root material, placed by David (design 835c0c9c); nothing was read or sent"
    elif [ ! -r "$KC" ]; then
        finish 1 "unreadable: $KC is present but unreadable; nothing was read or sent"
    fi
    KUBECTL_LINE="docker run --rm --log-driver=none --network host -v $KC:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
fi
read -r -a KUBECTL <<< "$KUBECTL_LINE"

base="${RUNTIME_DIRECTORY:-}"
base="${base%%:*}"
[ -n "$base" ] && [ -d "$base" ] || base="${TMPDIR:-/tmp}"
umask 077
WORK="$(mktemp -d "$base/runner-push.XXXXXX")" || finish 1 "failed: no private scratch directory in $base; nothing was read or sent"
trap 'rm -rf "$WORK"' EXIT

# --- 1. the transport key ----------------------------------------------
if ! "${KUBECTL[@]}" -n boss get secret runner-deposit-key -o json > "$WORK/key.json" 2> "$WORK/err"; then
    if grep -q 'NotFound' "$WORK/err"; then
        finish 0 "not yet: Secret boss/runner-deposit-key does not exist (the cluster converge creates it empty from infra/dispatcher/rules/broker-prepares-the-runner-deposit-key.toml); nothing was sent"
    fi
    finish 1 "unreadable: Secret boss/runner-deposit-key: $(first_lines "$WORK/err")"
fi
jq_doc_file "$WORK/key.json" || finish 1 "unreadable: Secret boss/runner-deposit-key is not one document"
PRIVATE_B64="$(jq -r '.data.private_key // empty' "$WORK/key.json")" \
    || finish 1 "unreadable: Secret boss/runner-deposit-key's private_key could not be read; nothing was sent"
FOR_B64="$(jq -r '.data.minted_for // empty' "$WORK/key.json")" \
    || finish 1 "unreadable: Secret boss/runner-deposit-key's minted_for could not be read; nothing was sent"
rm -f "$WORK/key.json"
if [ -z "$PRIVATE_B64" ]; then
    finish 0 "not yet: Secret boss/runner-deposit-key holds no key — the broker has not prepared one (a prepare-a-deposit-key packet on runner-deposit-key, scoped); nothing was sent"
fi
printf '%s' "$PRIVATE_B64" | base64 -d > "$WORK/key" 2>/dev/null \
    || finish 1 "unreadable: private_key of Secret boss/runner-deposit-key is not base64; nothing was sent"
PRIVATE_B64=""
FOR="$(printf '%s' "$FOR_B64" | base64 -d 2>/dev/null | tr -d '[:space:]')" \
    || finish 1 "unreadable: minted_for of Secret boss/runner-deposit-key is not base64; nothing was sent"
[[ $FOR =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || finish 1 "unreadable: Secret boss/runner-deposit-key holds a key but no minted_for packet id, so its enrollment cannot be checked; nothing was sent"

# --- 2. the pin and the account ----------------------------------------
"${KUBECTL[@]}" -n boss get configmap break-glass-deposit-known-hosts -o json > "$WORK/pin.json" 2> "$WORK/err" \
    || finish 1 "unreadable: ConfigMap boss/break-glass-deposit-known-hosts, boss-gcp's pinned host key: $(first_lines "$WORK/err"); nothing was sent"
jq_doc_file "$WORK/pin.json" || finish 1 "unreadable: ConfigMap boss/break-glass-deposit-known-hosts is not one document; nothing was sent"
jq -r '.data.known_hosts // empty' "$WORK/pin.json" > "$WORK/known_hosts" 2>/dev/null \
    || finish 1 "unreadable: known_hosts of ConfigMap boss/break-glass-deposit-known-hosts could not be read; nothing was sent"
TARGET="$(jq -r '.data.target // empty' "$WORK/pin.json" 2>/dev/null)" \
    || finish 1 "unreadable: target of ConfigMap boss/break-glass-deposit-known-hosts could not be read; nothing was sent"
[ -s "$WORK/known_hosts" ] && [ -n "$TARGET" ] \
    || finish 1 "not configured: ConfigMap boss/break-glass-deposit-known-hosts carries no known_hosts or no target, and the credential is never sent to a host this pass cannot authenticate; nothing was sent"

# --- 3. the sender -----------------------------------------------------
# When the tree last changed the receiver: the sender's bound on an older
# receiver's `not yet`. Unknown is no grace at all.
#
# FROM ROOT'S STORE WHEN THIS RUNS FROM ROOT'S TREE (review f15070d2 of car
# 2c628718, B2). forge-converge runs this out of a generation of root's
# tree (infra/forge/root-tree.sh): an export with no .git, so `git -C` on
# it answers "not a git repository" — measured, rc 128 — and the first
# verify after landing met boss-gcp's older receiver with no grace. A
# generation says which commit it is in .boss-tree-sha, and the history
# is root's bare store beside it, read the way root-tree.sh reads it (no
# account's config, no hook), at THAT commit: the receiver this sender
# was built against. A checkout — the old unit, a hand run — has its own
# .git and is read as before.
since_unread=""
if [ -n "${BOSS_RUNNER_RECV_SINCE+set}" ]; then
    SINCE="$BOSS_RUNNER_RECV_SINCE"
elif [ -f "$REPO/.boss-tree-sha" ]; then
    TREE_SHA="$(cat -- "$REPO/.boss-tree-sha" 2>/dev/null)" || since_unread=1
    if [ -z "$since_unread" ] && [[ $TREE_SHA =~ ^[0-9a-f]{40,64}$ ]]; then
        SINCE="$(GIT_CONFIG_GLOBAL=/dev/null git -c core.hooksPath=/dev/null \
            --git-dir="${BOSS_ROOT_TREE:-/var/lib/boss/tree}/repo.git" \
            log -1 --format=%ct "$TREE_SHA" -- infra/gcp/runner-credential-recv.sh 2>/dev/null)" \
            || since_unread=1
    else
        since_unread=1
    fi
else
    SINCE="$(git -C "$REPO" log -1 --format=%ct -- infra/gcp/runner-credential-recv.sh 2>/dev/null)" \
        || since_unread=1
fi
# A time git could not give is no grace, never a guess: an older receiver
# is then RED on the first pass that meets it — and the journal says why.
if [ -n "$since_unread" ]; then
    SINCE=""
    echo "$ME: when the tree last changed the receiver could not be read from $REPO; an older receiver on boss-gcp gets no grace this pass" >&2
fi
rc=0
# TMPDIR: the sender's own scratch — its key copy, the Secret it reads —
# goes in this pass's private base too, never the host's /tmp, which a
# SIGKILL would leave holding them (review f15070d2, N1).
TMPDIR="$base" \
BOSS_RUNNER_DEPOSIT_ENROLLMENT="$FOR" \
BOSS_RUNNER_DEPOSIT_KEY="$WORK/key" \
BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS="$WORK/known_hosts" \
BOSS_RUNNER_DEPOSIT_TARGET="$TARGET" \
BOSS_RUNNER_SEND_KUBECTL="$KUBECTL_LINE" \
BOSS_RUNNER_RECV_SINCE="$SINCE" \
    "$INFRA/gcp/runner-credential-send.sh" > "$WORK/out" 2> "$WORK/err" || rc=$?
# The journal keeps everything the sender said; it never says a value.
cat "$WORK/err" >&2
case "$rc" in
    0)
        if jq_doc_file "$WORK/out" && jq -e '.recorded == true' "$WORK/out" >/dev/null 2>&1; then
            finish 0 "delivered: boss-gcp installed ops-runner-credential-boss-gcp for rotation packet $(jq -r '.job_id' "$WORK/out" | cut -c1-8) and recorded it (event $(jq -r '.original_delivery_receipt.event_id' "$WORK/out"))"
        fi
        finish 0 "$(tail -n 1 "$WORK/out" | sed 's/^runner-credential-send: //')"
        ;;
    75)
        finish 0 "$(tail -n 1 "$WORK/out" | sed 's/^runner-credential-send: //')"
        ;;
    *)
        # The sender's RED line when it said one, else its last word.
        if ! line="$(grep -m1 '^RED ' "$WORK/err")"; then
            line="$(tail -n 1 "$WORK/err")"
        fi
        finish "$rc" "${line:-the sender exited $rc and said nothing}"
        ;;
esac
