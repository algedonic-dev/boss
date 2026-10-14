#!/usr/bin/env bash
# Install the fixed root-owned receiver closure, not a credential or key.
# Human scope/enrollment and registry publication remain separate doors.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
LIBEXEC="${INSTALL_RECV_LIBEXEC:-/usr/local/libexec/boss}"
SUDOERS="${INSTALL_RECV_SUDOERS_DIR:-/etc/sudoers.d}"
VISUDO="${INSTALL_VISUDO:-visudo}"
OWNER="${INSTALL_RECV_OWNER-root:root}"
# The enrolled account does not become root when the executable source
# moves to root's tree. Until explicitly configured from enrollment,
# derive only the owner of the existing unprivileged checkout, never
# execute any of its bytes. A missing checkout refuses, never guesses.
ACCOUNT="${INSTALL_RECV_USER:-$(stat -c %U "${BOSS_GCP_DEPOSIT_CHECKOUT:-/opt/boss}")}"
ME=install-root-tree-token-receiver
fail() { echo "$ME: failed ($1); receiver not enrolled" >&2; exit 1; }
[ "$#" -eq 0 ] || fail arguments
[[ "$ACCOUNT" =~ ^[a-z_][a-z0-9_-]*$ ]] && [ "$ACCOUNT" != root ] || fail deposit-account
. "$INFRA/lib/host-check.sh"
host_seam INSTALL_RECV_LIBEXEC /usr/local/libexec/boss
host_seam INSTALL_RECV_SUDOERS_DIR /etc/sudoers.d
host_check boss-gcp "$ME"
[ ! -L "$LIBEXEC" ] && [ ! -L "$SUDOERS" ] || fail destination-link
install -d -m 0755 "$LIBEXEC" "$SUDOERS" || fail directories
STAGE="$(mktemp -d "$LIBEXEC/.root-tree-delivery.XXXXXX")" || fail staging
GRANT="$(mktemp "$SUDOERS/.boss-root-tree-token-recv.XXXXXX")" || fail grant-staging
cleanup() { [ -z "$STAGE" ] || rm -rf "$STAGE"; [ -z "$GRANT" ] || rm -f "$GRANT"; }
trap cleanup EXIT
chmod 0755 "$STAGE"
# Complete sourced closure, including the estate consumed by host-check.
FILES=(gcp/root-tree-token-recv.sh gcp/root-tree-credential.sh lib/host-check.sh lib/jq.sh lib/sor.sh lib/sor-reader.sh lib/secret-header.sh estate/estate.toml)
for FILE in "${FILES[@]}"; do
    install -d -m 0755 "$STAGE/infra/$(dirname "$FILE")" || fail closure-directory
    MODE=0644
    case "$FILE" in gcp/root-tree-token-recv.sh | gcp/root-tree-credential.sh) MODE=0755 ;; esac
    install -m "$MODE" "$INFRA/$FILE" "$STAGE/infra/$FILE" || fail closure-copy
done
[ -z "$OWNER" ] || chown -R "$OWNER" "$STAGE" || fail closure-owner
RECEIVER="$LIBEXEC/root-tree-delivery/infra/gcp/root-tree-token-recv.sh"
{
    printf '# Rendered by install-root-tree-token-receiver; fixed-purpose receiver only.\n'
    printf 'Defaults!%s env_reset\n' "$RECEIVER"
    printf 'Defaults!%s secure_path="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"\n' "$RECEIVER"
    printf '%s ALL=(root) NOPASSWD: %s ""\n' "$ACCOUNT" "$RECEIVER"
} > "$GRANT"
chmod 0440 "$GRANT"
[ -z "$OWNER" ] || chown "$OWNER" "$GRANT" || fail grant-owner
"$VISUDO" -cf "$GRANT" > /dev/null || fail grant-validation
# Publish only a complete staged closure. Existing receiver remains usable
# on a failed copy; an interrupted swap refuses rather than executing part.
OLD=""
if [ -e "$LIBEXEC/root-tree-delivery" ]; then
    OLD="$LIBEXEC/.root-tree-delivery.old.$$"
    mv -T "$LIBEXEC/root-tree-delivery" "$OLD" || fail old-copy
fi
if ! mv -T "$STAGE" "$LIBEXEC/root-tree-delivery"; then
    [ -z "$OLD" ] || mv -T "$OLD" "$LIBEXEC/root-tree-delivery"
    fail closure-publication
fi
STAGE=""
mv -fT "$GRANT" "$SUDOERS/boss-root-tree-token-recv" || fail grant-publication
GRANT=""
[ -z "$OLD" ] || rm -rf "$OLD"
printf '%s\n' "$ME: root_tree_token_receiver installed; no credential, key or enrollment changed"
