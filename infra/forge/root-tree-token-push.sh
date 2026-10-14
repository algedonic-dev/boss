#!/usr/bin/env bash
# Existing forge admin reads only the broker-prepared transport and pin;
# no credential mint, key enrollment or host-key discovery happens here.
set +x
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
. "$INFRA/lib/jq.sh"
ME=root-tree-token-push
[ "$#" -eq 0 ] || { echo "$ME: refused arguments" >&2; exit 78; }
if [ -n "${BOSS_DEPOSIT_KUBECTL:-}" ]; then
    KUBECTL_LINE="$BOSS_DEPOSIT_KUBECTL"
else
    KC="${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
    [ -r "$KC" ] || { echo "$ME: not ready: existing admin kubeconfig unavailable; nothing sent"; exit 0; }
    KUBECTL_LINE="docker run --rm --log-driver=none --network host -v $KC:/kc:ro alpine/k8s:1.33.3 kubectl --kubeconfig=/kc"
fi
read -r -a KUBECTL <<< "$KUBECTL_LINE"
BASE="${RUNTIME_DIRECTORY:-${TMPDIR:-/tmp}}"
BASE="${BASE%%:*}"
umask 077
WORK="$(mktemp -d "$BASE/root-tree-push.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
if ! "${KUBECTL[@]}" -n boss get secret root-tree-deposit-key -o json > "$WORK/key.json" 2> "$WORK/error"; then
    if grep -q NotFound "$WORK/error"; then
        echo "$ME: not yet: named transport Secret absent; nothing sent"
        exit 0
    fi
    cat "$WORK/error" >&2
    echo "$ME: failed transport read" >&2
    exit 1
fi
jq_doc_file "$WORK/key.json" || exit 1
jq -s -e 'length == 1' "$WORK/key.json" >/dev/null 2>&1 || exit 1
if ! jq -e '(.data.private_key // "")|length > 0' "$WORK/key.json" >/dev/null 2>&1; then
    echo "$ME: not yet: broker has not prepared a transport key; nothing sent"
    exit 0
fi
jq -er '.data.private_key' "$WORK/key.json" | base64 -d > "$WORK/key"
ENROLLMENT="$(jq -er '.data.minted_for' "$WORK/key.json" | base64 -d)"
[[ "$ENROLLMENT" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || { echo "$ME: refused missing enrollment binding" >&2; exit 78; }
"${KUBECTL[@]}" -n boss get configmap break-glass-deposit-known-hosts -o json > "$WORK/pin.json" 2> "$WORK/error" || { cat "$WORK/error" >&2; exit 1; }
jq_doc_file "$WORK/pin.json" || exit 1
jq -s -e 'length == 1' "$WORK/pin.json" >/dev/null 2>&1 || exit 1
jq -er '.data.known_hosts' "$WORK/pin.json" > "$WORK/known_hosts"
TARGET="$(jq -er '.data.target' "$WORK/pin.json")"
# Nested sender scratch follows the unit's RuntimeDirectory, not /tmp.
rc=0
TMPDIR="$BASE" \
BOSS_ROOT_TREE_DEPOSIT_ENROLLMENT="$ENROLLMENT" \
BOSS_ROOT_TREE_DEPOSIT_KEY="$WORK/key" \
BOSS_ROOT_TREE_DEPOSIT_KNOWN_HOSTS="$WORK/known_hosts" \
BOSS_ROOT_TREE_DEPOSIT_TARGET="$TARGET" \
BOSS_ROOT_TREE_SEND_KUBECTL="$KUBECTL_LINE" \
    bash "$INFRA/gcp/root-tree-token-send.sh" || rc=$?
# Not enrolled yet is a recorded wait, not a failed deposit.
[ "$rc" != 75 ] || rc=0
exit "$rc"
