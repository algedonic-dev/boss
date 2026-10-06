#!/usr/bin/env bash
# install-ops-credential-receiver.sh — gives boss-gcp the one root door
# the break-glass kubeconfig arrives through (backlog 7336cb5f), and
# nothing wider. Called by infra/gcp/boss-gcp-converge.sh on every tick,
# for the cluster-operator role. The shape is the recovery kit's reader
# door (infra/forge/install-recovery-kit-reader.sh), for the same
# reasons; each is restated where it binds.
#
# WHAT IT INSTALLS
#   * infra/gcp/ops-credential-recv.sh -> $LIBEXEC/ops-credential-recv,
#     root:root 0755. A COPY, outside the checkout: the account the
#     sudoers rule names owns /opt/boss, and a rule pointing into it
#     would be NOPASSWD root for whoever can edit a file there. The
#     converge fast-forwards the checkout to forge main before it runs
#     this, so the file root runs is the file the trains reviewed.
#   * /etc/sudoers.d/boss-ops-credential-recv, root:root 0440:
#
#         Defaults!/usr/local/libexec/boss/ops-credential-recv env_reset
#         Defaults!/usr/local/libexec/boss/ops-credential-recv secure_path="…"
#         <user> ALL=(root) NOPASSWD: /usr/local/libexec/boss/ops-credential-recv kubeconfig
#
#     The ARGUMENT is part of the grant: sudo matches the whole command
#     line, so the account can run the receiver for the kubeconfig and
#     for nothing else. Rendered to a temporary file, checked with
#     `visudo -cf`, and only then moved into place — a fragment that
#     does not parse breaks sudo for the whole host. `env_reset` is
#     spelled although it is the default: the receiver's test seams are
#     environment variables, and this line is what keeps them from
#     crossing sudo.
#
# THE FORCED-COMMAND KEY is the narrower door, and it is DAVID's to
# place, because authorizing a key is a signed act, not a converge's:
# one line in <user>'s ~/.ssh/authorized_keys —
#
#   command="exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig",restrict <the deposit key's public half>
#
# — so the deposit key can do nothing but hand this host a kubeconfig
# the receiver accepts. The public half is minted inside the cluster by
# the deposit CronJob (infra/cluster/manifests/boss-break-glass-deposit.yaml),
# which prints this same line WITH the key filled in; the private half
# never leaves the cluster. This script PRINTS the line and places
# nothing. A test holds the two printed lines equal
# (crates/core/boss-testing/tests/break_glass_deposit_sh.rs).
#
# ENV (the scratch run the test drives): INSTALL_RECV_LIBEXEC,
# INSTALL_RECV_SUDOERS_DIR, INSTALL_VISUDO, INSTALL_RECV_USER,
# INSTALL_RECV_OWNER (root:root; empty skips chown for a non-root test).
#
# EXIT 0 installed; 1 a step failed, named — the converge records it
# and every other step still runs.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
LIBEXEC="${INSTALL_RECV_LIBEXEC:-/usr/local/libexec/boss}"
SUDOERS_DIR="${INSTALL_RECV_SUDOERS_DIR:-/etc/sudoers.d}"
VISUDO="${INSTALL_VISUDO:-visudo}"
OWNER="${INSTALL_RECV_OWNER-root:root}"
USER_NAME="${INSTALL_RECV_USER:-$(stat -c %U "$REPO")}"
RECEIVER="/usr/local/libexec/boss/ops-credential-recv"
SECURE_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ME="install-ops-credential-receiver"

fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

[[ "$USER_NAME" =~ ^[a-z_][a-z0-9_-]*$ ]] || fail "the deposit account '$USER_NAME' is not an account name sudoers can carry safely"
[ "$USER_NAME" != "root" ] || fail "the checkout is root's — the rule would grant root to root; name the deposit account with INSTALL_RECV_USER"

install -d -m 0755 "$LIBEXEC" || fail "cannot create $LIBEXEC"
install -m 0755 "$HERE/ops-credential-recv.sh" "$LIBEXEC/ops-credential-recv" || fail "cannot install the receiver"
if [ -n "$OWNER" ]; then
    chown "$OWNER" "$LIBEXEC/ops-credential-recv" || fail "cannot make the receiver $OWNER"
fi

tmp="$(mktemp "${SUDOERS_DIR}/.boss-ops-credential-recv.XXXXXX")" || fail "cannot stage a file in $SUDOERS_DIR"
{
    echo "# Rendered by infra/gcp/install-ops-credential-receiver.sh on every boss-gcp converge."
    echo "# The one root door the break-glass kubeconfig arrives through (backlog 7336cb5f)."
    echo "Defaults!$RECEIVER env_reset"
    echo "Defaults!$RECEIVER secure_path=\"$SECURE_PATH\""
    echo "$USER_NAME ALL=(root) NOPASSWD: $RECEIVER kubeconfig"
} > "$tmp" || { rm -f "$tmp"; fail "cannot write $tmp"; }
chmod 0440 "$tmp"
if [ -n "$OWNER" ]; then chown "$OWNER" "$tmp" || { rm -f "$tmp"; fail "cannot chown $tmp"; }; fi
"$VISUDO" -cf "$tmp" > /dev/null || { rm -f "$tmp"; fail "visudo -cf refused the rendered rule; nothing replaced"; }
mv -f "$tmp" "$SUDOERS_DIR/boss-ops-credential-recv" || { rm -f "$tmp"; fail "cannot place $SUDOERS_DIR/boss-ops-credential-recv"; }

echo "$ME: $LIBEXEC/ops-credential-recv installed; $SUDOERS_DIR/boss-ops-credential-recv grants it, for the kubeconfig only, to $USER_NAME"
echo "$ME: the narrower door, for David to place in ~$USER_NAME/.ssh/authorized_keys with the deposit key the cluster minted:"
echo "    command=\"exec sudo -n $RECEIVER kubeconfig\",restrict <the deposit key's public half>"
