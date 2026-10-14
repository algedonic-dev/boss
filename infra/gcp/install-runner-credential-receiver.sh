#!/usr/bin/env bash
# install-runner-credential-receiver.sh — gives boss-gcp the root door
# its ops runner credential arrives through (backlog 1e50e66b, R-b of
# its 2026-10-09 ledger), and nothing wider. Called by
# infra/gcp/boss-gcp-converge.sh on every tick, for the ops-runner role.
# The shape is the kubeconfig receiver's installer
# (infra/gcp/install-ops-credential-receiver.sh), for the same reasons;
# each is restated where it binds.
#
# WHY IT EXISTS. The runner deposit key's forced command
# (infra/dispatcher/rules/broker-prepares-the-runner-deposit-key.toml)
# runs a root-owned copy of infra/gcp/runner-credential-recv.sh, and
# until this script nothing put that copy, or the sudoers rule it runs
# under, on the host. prepare-a-deposit-key's enroll procedure tells
# David to sign only once a converge summary records
# `runner_credential_receiver installed` — the converge writes those
# words after this script exits 0, and runner_credential_recv_sh.rs
# holds the two spellings equal (review c59facbf, N1).
#
# WHAT IT INSTALLS
#   * A COPY of the receiver and every file it sources, at their tree
#     paths under $LIBEXEC/runner-delivery/infra/, root:root, each 0755
#     or 0644 by the tree's own execute bit, directories 0755. A copy,
#     outside the checkout, because the account the sudoers rule names
#     owns /opt/boss: a rule pointing into it would be NOPASSWD root for
#     whoever can edit a file there. The converge fast-forwards the
#     checkout to forge main before it runs this, so the files root runs
#     are the files the trains reviewed. The receiver finds its
#     libraries beside itself (`$INFRA/lib/…`), so the tree paths are
#     kept, and FILES below is the whole closure: the receiver, what it
#     sources, and what those source. A test runs a delivery from the
#     installed copy, so a file missing here is a red, not a refusal on
#     the host (runner_credential_recv_sh.rs).
#     The copy is built in a staging directory and swapped in whole, so
#     a file dropped from FILES leaves the host with it, and a pass that
#     fails part-way leaves the last whole copy in place.
#   * /etc/sudoers.d/boss-runner-credential-recv, root:root 0440:
#
#         Defaults!<receiver> env_reset
#         Defaults!<receiver> secure_path="…"
#         <user> ALL=(root) NOPASSWD: <receiver> ""
#
#     `""` is sudo's spelling of NO arguments: sudo matches the whole
#     command line, and the forced command is the receiver with none, so
#     the grant is exactly that one line (the receiver refuses arguments
#     as well; this is the second lock). Rendered to a temporary file,
#     checked with `visudo -cf`, and only then moved into place — a
#     fragment that does not parse breaks sudo for the whole host.
#     `env_reset` is spelled although it is the default: the receiver's
#     test seams are environment variables, and this line is what keeps
#     them from crossing sudo.
#
# THE FORCED-COMMAND KEY is the narrower door, and it is DAVID's to
# place, because authorizing a key is a signed act, not a converge's.
# Its line reaches him WITH the key on the enroll step of the
# prepare-a-deposit-key packet for runner-deposit-key, which the broker
# joins from its rule's forced_command; this script places nothing in
# any authorized_keys file.
#
# ENV (the scratch run the tests drive): the seams of the kubeconfig
# receiver's installer, judged the same way by host_check —
# INSTALL_RECV_LIBEXEC, INSTALL_RECV_SUDOERS_DIR, INSTALL_VISUDO,
# INSTALL_RECV_USER, INSTALL_RECV_OWNER (root:root; empty skips chown
# for a non-root test).
#
# EXIT 0 installed; 1 a step failed, named — the converge records it
# and every other step still runs; 78 not boss-gcp, nothing written.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA="$(cd "$HERE/.." && pwd)"
REPO="$(cd "$INFRA/.." && pwd)"
LIBEXEC="${INSTALL_RECV_LIBEXEC:-/usr/local/libexec/boss}"
SUDOERS_DIR="${INSTALL_RECV_SUDOERS_DIR:-/etc/sudoers.d}"
VISUDO="${INSTALL_VISUDO:-visudo}"
OWNER="${INSTALL_RECV_OWNER-root:root}"
USER_NAME="${INSTALL_RECV_USER:-$(stat -c %U "$REPO")}"
COPY="runner-delivery"
RECEIVER="/usr/local/libexec/boss/$COPY/infra/gcp/runner-credential-recv.sh"
RULE="boss-runner-credential-recv"
SECURE_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ME="install-runner-credential-receiver"

# The receiver's closure, relative to infra/: what runner-credential-recv.sh
# sources, what those source (sor.sh -> sor-reader.sh, runner-delivery-ack.sh
# -> jq.sh), and the curl helper it runs with the lib that helper sources.
FILES=(
    gcp/runner-credential-recv.sh
    lib/jq.sh
    lib/sor.sh
    lib/sor-reader.sh
    lib/secret-header.sh
    lib/runner-delivery-ack.sh
    boss-api-curl.sh
    lib/curl-through-a-roll.sh
)

fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

[[ "$USER_NAME" =~ ^[a-z_][a-z0-9_-]*$ ]] || fail "the deposit account '$USER_NAME' is not an account name sudoers can carry safely"
[ "$USER_NAME" != "root" ] || fail "the checkout is root's — the rule would grant root to root; name the deposit account with INSTALL_RECV_USER"

# WHICH MACHINE THIS IS, BEFORE THE COPY AND THE SUDOERS RULE (backlog
# 62b09c57; infra/lib/host-check.sh), as the kubeconfig receiver's
# installer asks it: with either directory left at the host's default
# this machine must hold the address the estate declares for boss-gcp,
# or the exit is 78 and neither is written.
# shellcheck source=infra/lib/host-check.sh
. "$HERE/../lib/host-check.sh" 2>/dev/null \
    || { echo "$ME: REFUSED — $HERE/../lib/host-check.sh cannot be read, so which machine this is cannot be established; nothing was written" >&2; exit 78; }
host_seam INSTALL_RECV_LIBEXEC /usr/local/libexec/boss
host_seam INSTALL_RECV_SUDOERS_DIR /etc/sudoers.d
host_check boss-gcp "$ME"

install -d -m 0755 "$LIBEXEC" || fail "cannot create $LIBEXEC"
stage="$(mktemp -d "$LIBEXEC/.$COPY.XXXXXX")" || fail "cannot stage a copy in $LIBEXEC"
chmod 0755 "$stage" || { rm -rf "$stage"; fail "cannot open $stage to readers"; }
for f in "${FILES[@]}"; do
    src="$INFRA/$f"
    mode=0644
    [ -x "$src" ] && mode=0755
    install -d -m 0755 "$stage/infra/$(dirname "$f")" \
        && install -m "$mode" "$src" "$stage/infra/$f" \
        || { rm -rf "$stage"; fail "cannot copy infra/$f"; }
done
if [ -n "$OWNER" ]; then
    chown -R "$OWNER" "$stage" || { rm -rf "$stage"; fail "cannot make the copy $OWNER"; }
fi
# The swap: aside, in, gone. Between the two moves the receiver path is
# absent for an instant, and a delivery landing in it fails closed — the
# receiver never runs, so nothing is written or acknowledged, and the
# sender's next pass delivers.
old=""
if [ -e "$LIBEXEC/$COPY" ]; then
    old="$LIBEXEC/.$COPY.old.$$"
    mv -T "$LIBEXEC/$COPY" "$old" || { rm -rf "$stage"; fail "cannot move the last copy aside"; }
fi
if ! mv -T "$stage" "$LIBEXEC/$COPY"; then
    [ -z "$old" ] || mv -T "$old" "$LIBEXEC/$COPY"
    rm -rf "$stage"
    fail "cannot place the copy at $LIBEXEC/$COPY"
fi
[ -z "$old" ] || rm -rf "$old"

tmp="$(mktemp "${SUDOERS_DIR}/.$RULE.XXXXXX")" || fail "cannot stage a file in $SUDOERS_DIR"
{
    echo "# Rendered by infra/gcp/install-runner-credential-receiver.sh on every boss-gcp converge."
    echo "# The one root door the ops runner credential arrives through (backlog 1e50e66b)."
    echo "Defaults!$RECEIVER env_reset"
    echo "Defaults!$RECEIVER secure_path=\"$SECURE_PATH\""
    echo "$USER_NAME ALL=(root) NOPASSWD: $RECEIVER \"\""
} > "$tmp" || { rm -f "$tmp"; fail "cannot write $tmp"; }
chmod 0440 "$tmp"
if [ -n "$OWNER" ]; then chown "$OWNER" "$tmp" || { rm -f "$tmp"; fail "cannot chown $tmp"; }; fi
"$VISUDO" -cf "$tmp" > /dev/null || { rm -f "$tmp"; fail "visudo -cf refused the rendered rule; nothing replaced"; }
mv -f "$tmp" "$SUDOERS_DIR/$RULE" || { rm -f "$tmp"; fail "cannot place $SUDOERS_DIR/$RULE"; }

echo "$ME: $LIBEXEC/$COPY installed (${#FILES[@]} files); $SUDOERS_DIR/$RULE grants its receiver, with no arguments, to $USER_NAME"
echo "$ME: the deposit key's line is on the enroll step of the prepare-a-deposit-key packet for runner-deposit-key; it reaches this receiver only once David places it"
