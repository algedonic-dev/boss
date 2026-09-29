#!/usr/bin/env bash
# write-recovery-kit.sh <version> <stick mount path> — write the recovery
# kit the forge has cut onto a stick, from ANY machine that has joined
# the estate's tunnel (backlog c1bb822e; David 2026-09-29: "this will
# also prove how any machine that sets up the tunnel can create the
# emergency recovery media"). Runs on macOS and Linux (bash 3.2 and up).
#
# WHY A FILE IN THE TREE and not a line pasted out of the packet (review
# of car 3d12b774, finding 9): what sits next to the plaintext pipe must
# be something a reviewer read, and the ops-request's recorded output is
# a record anyone with the queue can rewrite. The packet prints only
#   bash infra/forge/write-recovery-kit.sh <version> STICK_MOUNT_PATH
# and the sha256 this file had when the kit was cut, to compare against
# the checkout it is run from.
#
# WHAT IT DOES, in order — and a step that fails stops it there (set -e):
#   0. The target must be a REMOVABLE device's own mount point (lsblk
#      RM/HOTPLUG on Linux, diskutil Removable Media / Protocol USB on
#      macOS), refused by path before anything is fetched or written —
#      step 3 unmounts what it was given (re-review of 416f3899, N4).
#   1. The MANIFEST and README, beside the kit on the stick, in the
#      clear: names, sizes and hashes; never a value. An INCOMPLETE kit's
#      files are named boss-recovery-kit-<version>-INCOMPLETE.* (finding 3).
#   2. The kit's tar, streamed from the forge's reader straight into gpg
#      --symmetric: the plaintext exists only in that pipe, and gpg's own
#      prompt is the only place the passphrase is typed. AES256, and the
#      strongest key derivation OpenPGP offers — SHA512 iterated to the
#      maximum count (finding 8): the stick lives in a briefcase, and a
#      briefcase can be taken.
#   3. sync, then the stick UNMOUNTED AND MOUNTED again (diskutil on
#      macOS, udisksctl on Linux; otherwise it asks for the stick to be
#      pulled and re-inserted), so the read-back reads the STICK and not
#      the page cache — a counterfeit-capacity or failing stick passes a
#      read of memory (finding 2).
#   4. The stick's copy decrypted (gpg asks for the passphrase AGAIN —
#      --no-symkey-cache — which proves it can be produced), hashed, and
#      compared with the manifest's tar_sha256. Only then is .partial
#      renamed to the kit. A mismatch leaves .partial, which is not a kit.
#   5. The forge told what was read back (--written): it checks the hash
#      against its own copy, records the write on the packet that cut the
#      kit (with the kit's completeness), and discards the kit from RAM.
#
# THE DOOR ON THE FORGE is /usr/local/libexec/boss/recovery-kit-read,
# root-owned, installed by infra/forge/install-recovery-kit-reader.sh and
# the ONLY command its sudoers rule grants. Two ways to reach it:
#   * default: ssh as your user (or BOSS_KIT_SSH_USER) and `sudo -n` it;
#   * BOSS_KIT_SSH_KEY=<private key>: a dedicated key whose forge
#     authorized_keys line is
#       command="set -f; exec sudo -n /usr/local/libexec/boss/recovery-kit-read $SSH_ORIGINAL_COMMAND",restrict <pub>
#     so that key can do nothing but these read legs. Placing that line
#     is David's act; the installer prints it.
# The route is the tunnel: a jump through the hub's overlay address
# (HUB_IP in infra/cluster/wireguard/setup-hub.sh) to the forge's
# address (forge_host in infra/estate/estate.toml), both read from THIS
# checkout. BOSS_KIT_JUMP='' goes direct (a machine on the forge's LAN).
#
# PREREQUISITES of the writing machine: the tunnel joined; an SSH key the
# hub and the forge accept for your user; that user named in the forge's
# sudoers rule (or the dedicated key above); gpg 2.2.7+; the stick
# mounted and writable.
#
# Tested in crates/core/boss-testing/tests/recovery_kit_sh.rs with stub
# ssh, sudo, gpg, sync, findmnt and udisksctl, end to end against the
# real reader.
set -euo pipefail

ME="write-recovery-kit"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
READER="/usr/local/libexec/boss/recovery-kit-read"

say() { echo "$ME: $*" >&2; }
die() { say "STOPPED — $*"; exit 1; }

V="${1:-}"
D="${2:-}"
D="${D%/}"
[[ "$V" =~ ^[0-9a-f]{12}$ ]] || die "usage: $ME <12-hex kit version> <stick mount path> (the version is on the cut-recovery-kit answer)"
[ -d "$D" ] || die "$D is not a directory — is the stick mounted there?"
[ -w "$D" ] || die "$D is not writable"

# --- 0. the target is a stick: a removable device's own mount point ------
# (re-review of 416f3899, N4.) The writer unmounts what it is given, so a
# fixed mount point — /boot/efi, a data disk — must be refused before
# anything is written or unmounted, loudly, naming the path.
# BOSS_KIT_ALLOW_FIXED=1 overrides for a deliberate exception.
PLATFORM="$(uname -s)"
refuse_fixed() { # <what the host said>
    if [ "${BOSS_KIT_ALLOW_FIXED:-}" = "1" ]; then
        say "WARNING — $D is not removable media ($1); writing anyway because BOSS_KIT_ALLOW_FIXED=1"
        return 0
    fi
    die "REFUSED — $D is not removable media ($1). Nothing was fetched or written. Mount the stick and name ITS mount point; BOSS_KIT_ALLOW_FIXED=1 overrides for a deliberate exception"
}
case "$PLATFORM" in
    Darwin)
        info="$(diskutil info "$D")" || die "diskutil cannot describe $D — is the stick mounted there?"
        DEV="$(printf '%s\n' "$info" | sed -n 's/^ *Device Node: *//p' | sed -n 1p)"
        target="$(printf '%s\n' "$info" | sed -n 's/^ *Mount Point: *//p' | sed -n 1p)"
        [ -n "$DEV" ] && [ "${target%/}" = "$D" ] || die "$D is not the stick's mount point (diskutil says ${target:-none})"
        removable="$(printf '%s\n' "$info" | sed -n 's/^ *Removable Media: *//p' | sed -n 1p)"
        protocol="$(printf '%s\n' "$info" | sed -n 's/^ *Protocol: *//p' | sed -n 1p)"
        case "$removable|$protocol" in
            Removable\|*|Yes\|*|*\|USB) ;;
            *) refuse_fixed "diskutil: Removable Media ${removable:-unstated}, Protocol ${protocol:-unstated}" ;;
        esac
        ;;
    *)
        target="$(findmnt -n -o TARGET --target "$D")" || die "findmnt cannot place $D"
        [ "${target%/}" = "$D" ] || die "$D is not the stick's mount point (it lies under ${target:-nothing})"
        DEV="$(findmnt -n -o SOURCE --target "$D")" || die "findmnt names no device under $D"
        flags="$(lsblk -no RM,HOTPLUG "$DEV")" || die "lsblk cannot describe $DEV"
        set -- $flags
        case "${1:-0}${2:-0}" in
            *1*) ;;
            *) refuse_fixed "lsblk: $DEV RM=${1:-unstated} HOTPLUG=${2:-unstated}" ;;
        esac
        ;;
esac

FORGE="$(sed -n 's/^forge_host = "\([^"]*\)".*$/\1/p' "$REPO/infra/estate/estate.toml" | sed -n 1p)"
HUB="$(sed -n 's|^HUB_IP="\([0-9.]*\)/[0-9]*"$|\1|p' "$REPO/infra/cluster/wireguard/setup-hub.sh" | sed -n 1p)"
[ -n "$FORGE" ] || die "infra/estate/estate.toml names no forge_host"
JUMP="${BOSS_KIT_JUMP-$HUB}"
AT="${BOSS_KIT_SSH_USER:+$BOSS_KIT_SSH_USER@}"

SSH_OPTS=(-n)
[ -z "$JUMP" ] || SSH_OPTS+=(-J "$AT$JUMP")
if [ -n "${BOSS_KIT_SSH_KEY:-}" ]; then
    SSH_OPTS+=(-i "$BOSS_KIT_SSH_KEY" -o IdentitiesOnly=yes)
    kit() { ssh "${SSH_OPTS[@]}" "$AT$FORGE" "$@"; }
else
    kit() { ssh "${SSH_OPTS[@]}" "$AT$FORGE" sudo -n "$READER" "$@"; }
fi

if command -v sha256sum >/dev/null 2>&1; then
    hash_of() { sha256sum | cut -d' ' -f1; }
else
    hash_of() { shasum -a 256 | cut -d' ' -f1; }
fi

# gpg's curses pinentry needs to know the terminal; with none (a GUI
# pinentry, as on macOS) there is nothing to tell it.
if [ -t 0 ]; then
    GPG_TTY="$(tty)"
    export GPG_TTY
fi

# --- 1. the manifest decides the name ------------------------------------
tmp_manifest="$D/.boss-recovery-kit-$V.MANIFEST.partial"
kit --manifest "$V" > "$tmp_manifest"
[ "$(sed -n 's/^kit_version //p' "$tmp_manifest")" = "$V" ] || die "the forge's manifest is not kit $V"
completeness="$(sed -n 's/^completeness //p' "$tmp_manifest")"
case "$completeness" in
    complete) base="boss-recovery-kit-$V" ;;
    INCOMPLETE*) base="boss-recovery-kit-$V-INCOMPLETE" ;;
    *) die "the manifest states no completeness — it is not from a reader this writer knows" ;;
esac
want="$(sed -n 's/^tar_sha256 //p' "$tmp_manifest")"
[[ "$want" =~ ^[0-9a-f]{64}$ ]] || die "the manifest names no tar_sha256"
K="$D/$base"
mv -f "$tmp_manifest" "$K.MANIFEST.txt"
kit --readme "$V" > "$K.README.txt"

# --- 2. the stream, through gpg ------------------------------------------
rm -f "$K.tar.gpg.partial"
say "streaming kit $V into gpg — set the passphrase at gpg's prompt"
kit --stream "$V" | gpg --symmetric --no-symkey-cache \
    --s2k-mode 3 --s2k-digest-algo SHA512 --s2k-count 65011712 \
    --cipher-algo AES256 --output "$K.tar.gpg.partial"

# --- 3. to the stick, and back off it ------------------------------------
sync
# Every failure here names the stick's state (re-review N5): an unmount
# that fails leaves it mounted and unverified; a mount that fails leaves
# it UNMOUNTED, holding a .partial that is not a kit.
remount() {
    local new="" out
    local unmounted_msg="the stick ($DEV) is left UNMOUNTED and $base.tar.gpg.partial on it is NOT a kit — mount it and run the writer again"
    case "$PLATFORM" in
        Darwin)
            diskutil unmount "$D" > /dev/null \
                || die "diskutil could not unmount $D — the stick is still mounted and nothing was read back; $base.tar.gpg.partial is NOT a kit"
            diskutil mount "$DEV" > /dev/null || die "diskutil could not mount $DEV again: $unmounted_msg"
            new="$(diskutil info "$DEV" | sed -n 's/^ *Mount Point: *//p' | sed -n 1p)" \
                || die "diskutil cannot say where $DEV mounted: $unmounted_msg"
            ;;
        *)
            if command -v udisksctl > /dev/null 2>&1; then
                udisksctl unmount -b "$DEV" > /dev/null \
                    || die "udisksctl could not unmount $DEV — the stick is still mounted and nothing was read back; $base.tar.gpg.partial is NOT a kit"
                out="$(udisksctl mount -b "$DEV")" || die "udisksctl could not mount $DEV again: $unmounted_msg"
                new="$(printf '%s\n' "$out" | sed -n 's/^Mounted .* at \(.*\)$/\1/p' | sed -n 1p)"
                new="${new%.}"
            else
                say "no udisksctl here. PULL THE STICK OUT and PLUG IT BACK IN now — pressing Enter without re-inserting it"
                say "reads this computer's memory, not the stick, and proves nothing. Then type where it mounted (Enter for $D):"
                read -r new < /dev/tty
                new="${new:-$D}"
            fi
            ;;
    esac
    [ -n "$new" ] && [ -d "$new" ] || die "the stick did not come back mounted: $unmounted_msg"
    D="${new%/}"
}
remount
K="$D/$base"
[ -f "$K.tar.gpg.partial" ] || die "the re-mounted stick holds no $base.tar.gpg.partial"

# --- 4. the read-back, from the stick ------------------------------------
say "reading the stick back — type the passphrase again at gpg's prompt"
got="$(gpg --decrypt --no-symkey-cache "$K.tar.gpg.partial" | hash_of)"
if [ "$got" != "$want" ]; then
    die "READ-BACK $got IS NOT KIT $V ($want) — $K.tar.gpg.partial is NOT a kit; use another stick"
fi
mv -f "$K.tar.gpg.partial" "$K.tar.gpg"
sync
echo "WRITTEN AND READ BACK FROM THE STICK: $K.tar.gpg"
if [ "$completeness" != "complete" ]; then
    echo "INCOMPLETE KIT — ${completeness#INCOMPLETE } is NOT on this stick. Its MANIFEST and README say why; cut again once it is fixed."
fi

# --- 5. the forge's record -----------------------------------------------
kit --written "$V" "$got" || {
    rc=$?
    say "the stick is written and read back; the forge's record of it FAILED (exit $rc) — the reader said why above"
    exit "$rc"
}
