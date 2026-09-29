#!/usr/bin/env bash
# install-recovery-kit-reader.sh — gives the forge the one root door a
# workstation uses to write a recovery kit onto a stick (backlog
# c1bb822e), and nothing wider. Called by infra/forge/install.sh on every
# converge tick.
#
# WHAT IT INSTALLS
#   * infra/forge/recovery-kit-read.sh -> $LIBEXEC/recovery-kit-read,
#     root:root 0755. A COPY, outside the checkout: the account the
#     sudoers rule names owns the checkout, and a rule pointing into it
#     would be NOPASSWD root for whoever can edit a file there (the
#     adversarial review of car 3d12b774, finding 1). The copy sources
#     nothing, and the converge force-checks-out forge main before it
#     runs this, so the file root runs is the file the trains reviewed.
#   * /etc/sudoers.d/boss-recovery-kit, root:root 0440:
#
#         Defaults!/usr/local/libexec/boss/recovery-kit-read env_reset
#         Defaults!/usr/local/libexec/boss/recovery-kit-read secure_path="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
#         <user> ALL=(root) NOPASSWD: /usr/local/libexec/boss/recovery-kit-read
#
#     rendered to a temporary file, checked with `visudo -cf`, and only
#     then moved into place — a sudoers fragment that does not parse
#     breaks sudo for the whole host. `env_reset` is the default and is
#     spelled anyway: the reader's test seams are environment variables,
#     and this line is what guarantees none of them crosses sudo.
#     <user> is the owner of the checkout this runs from (the forge
#     user, whose key the tunnel's machines use), or INSTALL_KIT_USER.
#
# THE FORCED-COMMAND KEY is the narrower door, and it is DAVID's to
# place, because authorizing a key is a signed act, not a converge's:
# a dedicated key on the writing machine, and on the forge one line in
# <user>'s ~/.ssh/authorized_keys —
#
#   command="set -f; exec sudo -n /usr/local/libexec/boss/recovery-kit-read $SSH_ORIGINAL_COMMAND",restrict <the key's public half>
#
# — so that key can do nothing but the reader's four read legs (`set -f`
# keeps a `*` in the request from globbing; the reader refuses anything
# that is not one of its four modes and a hex argument). The writer uses
# it when BOSS_KIT_SSH_KEY names the private half. This script PRINTS
# that line, with the user filled in, and places nothing.
#
# ENV (the scratch run the test drives): INSTALL_KIT_LIBEXEC,
# INSTALL_KIT_SUDOERS_DIR, INSTALL_VISUDO, INSTALL_KIT_USER,
# INSTALL_KIT_OWNER (root:root; empty skips chown for a non-root test).
#
# EXIT 0 installed; 1 a step failed, named — the caller carries the code
# so every other unit still converges. Pinned by
# crates/core/boss-testing/tests/recovery_kit_sh.rs.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
LIBEXEC="${INSTALL_KIT_LIBEXEC:-/usr/local/libexec/boss}"
SUDOERS_DIR="${INSTALL_KIT_SUDOERS_DIR:-/etc/sudoers.d}"
VISUDO="${INSTALL_VISUDO:-visudo}"
OWNER="${INSTALL_KIT_OWNER-root:root}"
USER_NAME="${INSTALL_KIT_USER:-$(stat -c %U "$REPO")}"
READER="/usr/local/libexec/boss/recovery-kit-read"
# The PATH sudo hands the reader (re-review of 416f3899, N1) — the same
# list the reader sets for itself, so neither depends on the other.
SECURE_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ME="install-recovery-kit-reader"

fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

[[ "$USER_NAME" =~ ^[a-z_][a-z0-9_-]*$ ]] || fail "the kit user '$USER_NAME' is not an account name sudoers can carry safely"
[ "$USER_NAME" != "root" ] || fail "the checkout is root's — the rule would grant root to root; name the forge user with INSTALL_KIT_USER"

install -d -m 0755 "$LIBEXEC" || fail "cannot create $LIBEXEC"
install -m 0755 "$HERE/recovery-kit-read.sh" "$LIBEXEC/recovery-kit-read" || fail "cannot install the reader"
if [ -n "$OWNER" ]; then
    chown "$OWNER" "$LIBEXEC/recovery-kit-read" || fail "cannot make the reader $OWNER"
fi

tmp="$(mktemp "${SUDOERS_DIR}/.boss-recovery-kit.XXXXXX")" || fail "cannot stage a file in $SUDOERS_DIR"
{
    echo "# Rendered by infra/forge/install-recovery-kit-reader.sh on every forge converge."
    echo "# The one root door a workstation has to a recovery kit (backlog c1bb822e)."
    echo "Defaults!$READER env_reset"
    echo "Defaults!$READER secure_path=\"$SECURE_PATH\""
    echo "$USER_NAME ALL=(root) NOPASSWD: $READER"
} > "$tmp" || { rm -f "$tmp"; fail "cannot write $tmp"; }
chmod 0440 "$tmp"
if [ -n "$OWNER" ]; then chown "$OWNER" "$tmp" || { rm -f "$tmp"; fail "cannot chown $tmp"; }; fi
"$VISUDO" -cf "$tmp" > /dev/null || { rm -f "$tmp"; fail "visudo -cf refused the rendered rule; nothing replaced"; }
mv -f "$tmp" "$SUDOERS_DIR/boss-recovery-kit" || { rm -f "$tmp"; fail "cannot place $SUDOERS_DIR/boss-recovery-kit"; }

echo "$ME: $LIBEXEC/recovery-kit-read installed; $SUDOERS_DIR/boss-recovery-kit grants it (and only it) to $USER_NAME"
echo "$ME: the narrower door, for David to place in ~$USER_NAME/.ssh/authorized_keys with a dedicated key:"
echo "    command=\"set -f; exec sudo -n $READER \$SSH_ORIGINAL_COMMAND\",restrict <public key>"
