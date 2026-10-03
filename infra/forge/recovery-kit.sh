#!/usr/bin/env bash
# recovery-kit.sh — ONE object that can rebuild the estate (backlog
# c1bb822e). The forge cuts it into a root-only tmpfs; any machine on the
# estate's tunnel writes it through gpg onto a stick.
#
#   recovery-kit.sh --assemble   the ops verb cut-recovery-kit (root, under the ops runner)
#   recovery-kit.sh --discard    the ops verb discard-recovery-kit: the tmpfs, kit and all
#
# The workstation's side is two more files, deliberately NOT this one:
#   infra/forge/recovery-kit-read.sh     installed root-owned as
#       /usr/local/libexec/boss/recovery-kit-read, the only thing sudo
#       grants — the manifest, the README, the stream, the write record
#   infra/forge/write-recovery-kit.sh    run on the writing machine, from
#       its own checkout: ssh, gpg, sync, re-mount, read back, record
#
# WHY (David, 2026-09-21, preparing the forge disk swap): "a ONE-STICK
# RECOVERY DRIVE — one physical object that can rebuild the estate." The
# forge backup (forge-backup.sh, 121831e6) covers the forge's DATA and
# none of its credentials, which are scattered by nature — a token here,
# a kubeconfig there, a registration inside a running service — the
# shape of thing discovered missing DURING a recovery. And the Talos
# secrets bundle was in no tree and no backup at all: losing the forge
# costs a rebuild, losing the bundle means no node can join the existing
# cluster. David's decisions (2026-09-29, on the packet): an ordinary
# stick, gpg SYMMETRIC encryption, the passphrase set by him at write
# time and never seen, stored, logged, printed or transmitted by BOSS;
# the bundle DERIVED by BOSS, because nobody holds it; and written from
# a workstation, streaming — "this will also prove how any machine that
# sets up the tunnel can create the emergency recovery media."
#
# --assemble mounts a tmpfs, PROVES it is one (findmnt) before a single
# credential is read, copies and derives each item into it, tars them,
# and prints the MANIFEST and the one line that writes a stick. Nothing
# it prints is a value: the output is recorded on the ops-request packet,
# which everyone with the queue can read. Credentials go file-to-file
# under umask 077 and are compared by digest, never echoed.
#
# THE TMPFS, not a disk. /run/boss-recovery-kit, mode 0700,
# nosuid/nodev/noexec, 4g, and `noswap` where the kernel has it (6.4+).
# Where it does not, the mount goes ahead and a swap device `swapon`
# lists is named in the manifest as a WARNING — a tmpfs page can reach
# that disk (review of 3d12b774, finding 7). A path that is not a tmpfs
# after the mount is refused before anything is read into it. From the
# mount on, an EXIT trap discards everything until the kit's state is
# written, so a killed or failed cut (the runner's 900 s, a set -u abort,
# a SIGTERM) leaves no credential resident (finding 5). A cut kit lives
# until the writing machine's --written matches (the reader discards it
# then), --discard, the next cut, or a reboot; the reader refuses to
# stream one older than 24 h.
#
# THE FORGE ARCHIVE is infra/forge/forge-backup.sh's, called with its
# destination in the tmpfs — ONE definition of how the forge is dumped
# consistently (forgejo dump through Forgejo's own connection, never a cp
# of the live WAL database; the registry skipped; verified before it is
# called one). Floor 0, ceiling BOSS_RECOVERY_KIT_FORGE_MAX_MB (1536).
#
# THE TALOS BUNDLE is derived from a live control-plane node: `talosctl
# read /system/state/config.yaml` through /etc/boss-ops/talosconfig, or
# `get machineconfig -o yaml` with its spec dedented, tried against each
# estate node whose role is talos-control-plane until one answers, then
# `talosctl gen secrets --from-controlplane-config`. PROVEN by effect:
# the bundle's os CA must equal the talosconfig's `ca` and its k8s CA the
# kubeconfig's certificate-authority-data. A bundle that differs is not
# this cluster's: talos/ is dropped and named `MISSING talos/ — REFUSED
# …`, and the REST of the kit is still cut (finding 4) — a kit without
# the PKI still rebuilds the forge; a refusal that leaves no stick at all
# is the costlier failure under DR rule 62dac114.
#
# MISSING IS NAMED, NEVER SILENT. An item that cannot be found, read or
# proven is a `MISSING <path> — <why>` line; the kit is still cut; exit 3,
# INCOMPLETE on the first line, `completeness INCOMPLETE missing=<list>`
# in the manifest and `complete=no` in the state the reader serves — so
# the stick's file name and the packet's recovery_kit_written say so too
# (finding 3). 0 means every item is here; 1 means nothing was kept.
#
# THE VERSION is the first 12 hex of sha256 over the manifest's ITEM
# lines (`ITEM <sha256> <bytes> <path>`, each ending in a newline).
#
# ENV (the seams the tests use; every default is the forge's)
#   BOSS_RECOVERY_KIT_DIR          the tmpfs mount point (/run/boss-recovery-kit)
#   BOSS_RECOVERY_KIT_SIZE         its size (4g)
#   BOSS_RECOVERY_KIT_UID          the uid that must run it (0)
#   BOSS_RECOVERY_KIT_FORGE_MAX_MB the forge dump's ceiling (1536)
#   BOSS_RECOVERY_KIT_CALL_TIMEOUT seconds any one talosctl/kubectl call may take (60)
#   BOSS_RECOVERY_KIT_FORGE_TIMEOUT seconds the forge dump may take (600; the verb has 900)
#   BOSS_RECOVERY_KIT_SUDOERS_DIR  /etc/sudoers.d
#   BOSS_RECOVERY_KIT_RUNNER_UNIT  forgejo-runner.service
#   BOSS_OPS_DIR                   /etc/boss-ops (talosconfig, kubeconfig)
#   BOSS_ESTATE_SOURCE             infra/estate/estate.toml (the control-plane nodes)
#   BOSS_SOR_ENV                   /etc/boss/sor.env (BOSS_FORGE_HOST)
#   OPS_REQUEST_ID                 set by the ops runner: the packet the reader records on
#
# Tested against stubs in crates/core/boss-testing/tests/recovery_kit_sh.rs,
# the writer and the reader included.
set -uo pipefail

ME="recovery-kit"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$REPO/infra/lib/sor.sh"

DIR="${BOSS_RECOVERY_KIT_DIR:-/run/boss-recovery-kit}"
SIZE="${BOSS_RECOVERY_KIT_SIZE:-4g}"
WANT_UID="${BOSS_RECOVERY_KIT_UID:-0}"
FORGE_MAX_MB="${BOSS_RECOVERY_KIT_FORGE_MAX_MB:-1536}"
CALL_TIMEOUT="${BOSS_RECOVERY_KIT_CALL_TIMEOUT:-60}"
FORGE_TIMEOUT="${BOSS_RECOVERY_KIT_FORGE_TIMEOUT:-600}"
SUDOERS_DIR="${BOSS_RECOVERY_KIT_SUDOERS_DIR:-/etc/sudoers.d}"
RUNNER_UNIT="${BOSS_RECOVERY_KIT_RUNNER_UNIT:-forgejo-runner.service}"
OPS_DIR="${BOSS_OPS_DIR:-/etc/boss-ops}"
ESTATE="${BOSS_ESTATE_SOURCE:-$REPO/infra/estate/estate.toml}"
README_SRC="$HERE/recovery-kit-README.txt"
WRITER="infra/forge/write-recovery-kit.sh"
READER="/usr/local/libexec/boss/recovery-kit-read"
# The offsite bucket's key: the Secret the nightly backup mounts
# (infra/cluster/manifests/boss-backup.yaml, keys sa.json and bucket).
GCS_NS="boss"
GCS_SECRET="boss-gcs-offsite"

umask 077

say() { echo "$ME: $*" >&2; }
die() { # <exit> <why>
    say "REFUSED — $2"
    exit "$1"
}

need_uid() {
    [ "$(id -u)" = "$WANT_UID" ] \
        || die 1 "must run as uid $WANT_UID (root): the kit lives in a root-only tmpfs. File the $1 ops verb"
}

# ONE run at a time: the reader must never serve a kit an assemble is
# replacing. The lock sits beside the mount point, not in it; the reader
# takes the same one.
take_lock() {
    exec 9>"$DIR.lock" || die 1 "cannot open the lock $DIR.lock"
    flock -n 9 || die 1 "another recovery-kit run holds $DIR.lock — wait for it, or read its packet"
}

is_tmpfs() { [ "$(findmnt -n -o FSTYPE --mountpoint "$DIR" 2>/dev/null)" = "tmpfs" ]; }

wipe() {
    if [ -d "$DIR" ]; then
        find "$DIR" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} + 2>/dev/null
    fi
}

discard() {
    wipe
    if [ -n "$(findmnt -n -o FSTYPE --mountpoint "$DIR" 2>/dev/null)" ]; then
        umount "$DIR" || say "could not unmount $DIR — its contents were removed; a reboot clears it"
    fi
}

state() { sed -n "s/^$1=//p" "$DIR/state" 2>/dev/null | sed -n 1p; }

# ---------------------------------------------------------------------------
# The items
# ---------------------------------------------------------------------------
MISSING=""
N_MISSING=0
MISSING_PATHS=""
missing() { # <path> <why>
    MISSING="${MISSING}MISSING $1 — $2"$'\n'
    N_MISSING=$((N_MISSING + 1))
    MISSING_PATHS="${MISSING_PATHS:+$MISSING_PATHS,}$1"
}

# take <kit path> <source file> — a byte copy under umask 077. The
# value never passes through a variable or an argv.
take() {
    local dst="$KIT/$1" src="$2"
    if [ ! -e "$src" ]; then missing "$1" "$src does not exist"; return 1; fi
    if [ ! -r "$src" ]; then missing "$1" "$src is not readable by $(id -un)"; return 1; fi
    if [ ! -s "$src" ]; then missing "$1" "$src is empty"; return 1; fi
    mkdir -p "$(dirname "$dst")" && cat "$src" > "$dst" \
        || { rm -f "$dst"; missing "$1" "copying $src failed"; return 1; }
}

forge_archive() {
    mkdir -p "$KIT/forge"
    # Bounded like every other call here (re-review of 416f3899, N6): a
    # signal is deferred until the child returns, and the runner's own
    # timeout sends one TERM and no KILL, so an unbounded dump could hold
    # the kit resident past the verb's 900 s.
    if timeout "$FORGE_TIMEOUT" env -u BOSS_RUN_SUMMARY_FILE \
        BOSS_FORGE_BACKUP_DIR="$KIT/forge" BOSS_FORGE_BACKUP_KEEP=1 \
        BOSS_FORGE_BACKUP_FLOOR_GB=0 BOSS_FORGE_BACKUP_MAX_MB="$FORGE_MAX_MB" \
        bash "$HERE/forge-backup.sh" > "$WORK/forge-backup.log" 2>&1; then
        rm -f "$KIT/forge/.latest"
    else
        # forge-backup never prints a value (its header: "Nothing here
        # prints its contents"), so its whole account is safe to show.
        missing "forge/" "forge-backup.sh refused or outlasted ${FORGE_TIMEOUT}s; its account follows the manifest"
        rm -rf "$KIT/forge"
        FORGE_LOG_SHOW=1
    fi
}

runner_registration() {
    local wd user
    wd="$(systemctl show -p WorkingDirectory --value "$RUNNER_UNIT" 2>/dev/null)"
    if [ -z "$wd" ]; then
        user="$(systemctl show -p User --value "$RUNNER_UNIT" 2>/dev/null)"
        if [ -n "$user" ]; then wd="$(getent passwd "$user" | cut -d: -f6)"; else wd="/"; fi
    fi
    RUNNER_FROM="${wd%/}/.runner"
    take runner/.runner "$RUNNER_FROM"
}

sudoers_entries() {
    local f n=0
    if [ ! -d "$SUDOERS_DIR" ]; then missing "sudoers/" "$SUDOERS_DIR does not exist"; return; fi
    for f in "$SUDOERS_DIR"/*; do
        [ -f "$f" ] || continue
        case "$(basename "$f")" in README) continue ;; esac
        take "sudoers/$(basename "$f")" "$f" && n=$((n + 1))
    done
    [ "$n" -gt 0 ] || missing "sudoers/" "$SUDOERS_DIR holds no entry (the ops-runner entry the packet names is not there)"
}

# secret_key <kit path> <data key> — one key of the offsite Secret,
# decoded file-to-file.
secret_key() {
    local rel="$1" key="$2" b64="$WORK/secret.b64"
    if ! timeout "$CALL_TIMEOUT" kubectl --kubeconfig "$OPS_DIR/kubeconfig" -n "$GCS_NS" get secret "$GCS_SECRET" \
        -o "jsonpath={.data.${key//./\\.}}" > "$b64" 2> "$WORK/kubectl.err"; then
        missing "$rel" "kubectl could not read secret $GCS_NS/$GCS_SECRET within ${CALL_TIMEOUT}s: $(head -c 200 "$WORK/kubectl.err" | tr '\n' ' ')"
        rm -f "$b64"; return 1
    fi
    if [ ! -s "$b64" ]; then missing "$rel" "secret $GCS_NS/$GCS_SECRET has no key $key"; rm -f "$b64"; return 1; fi
    mkdir -p "$(dirname "$KIT/$rel")"
    base64 -d < "$b64" > "$KIT/$rel" 2>/dev/null \
        || { rm -f "$KIT/$rel" "$b64"; missing "$rel" "the secret's $key is not base64"; return 1; }
    rm -f "$b64"
}

offsite_key() {
    if [ ! -r "$OPS_DIR/kubeconfig" ]; then
        missing "gcs/" "no kubeconfig at $OPS_DIR/kubeconfig to read secret $GCS_NS/$GCS_SECRET with"
        return
    fi
    secret_key gcs/sa.json sa.json
    secret_key gcs/bucket bucket
}

# control_planes — the addresses of every estate node whose role is
# talos-control-plane, in file order.
control_planes() {
    awk '
        function flush() { if (role == "talos-control-plane" && addr != "") print addr; role = ""; addr = "" }
        /^\[/ { flush(); next }
        /^address[[:space:]]*=/ { v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); addr = v }
        /^role[[:space:]]*=/ { v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); role = v }
        END { flush() }
    ' "$ESTATE" 2>/dev/null
}

# yaml_at <file> <dotted.path> — the scalar at a nested key of the plain
# block YAML talosctl writes. Printed only into a pipe, never shown.
yaml_at() {
    awk -v want="$2" '
        {
            match($0, /^ */); ind = RLENGTH; rest = substr($0, ind + 1)
            if (rest == "" || rest ~ /^#/ || rest ~ /^- /) next
            c = index(rest, ":"); if (c == 0) next
            key = substr(rest, 1, c - 1); val = substr(rest, c + 1); sub(/^ +/, "", val)
            while (n > 0 && lvl[n] >= ind) n--
            n++; lvl[n] = ind; name[n] = key
            path = name[1]; for (i = 2; i <= n; i++) path = path "." name[i]
            if (path == want) { gsub(/"/, "", val); print val; exit }
        }
    ' "$1"
}

# digest_of_b64 — sha256 of a base64 PEM, whitespace-insensitive, read
# from stdin. The certificate is public; the digest is compared, never shown.
digest_of_b64() { tr -d ' \t\r\n"' | base64 -d 2>/dev/null | tr -d ' \t\r\n' | sha256sum | cut -d' ' -f1; }

# any_value_matches <digest> <file> <key regex> — does any `<key>: <b64>`
# line of the file decode to that digest?
any_value_matches() {
    local want="$1" v
    while IFS= read -r v; do
        [ -n "$v" ] || continue
        [ "$(printf '%s' "$v" | digest_of_b64)" = "$want" ] && return 0
    done < <(sed -n "s/^[[:space:]]*$3:[[:space:]]*//p" "$2")
    return 1
}

# refuse_talos <why> — the bundle is not provably this cluster's: drop
# talos/ and name it, and let the rest of the kit stand.
refuse_talos() {
    rm -rf "$KIT/talos"
    missing "talos/" "REFUSED — $1"
    TALOS_VERIFIED="REFUSED (talos/ dropped; see MISSING)"
}

talos_bundle() {
    local tc="$OPS_DIR/talosconfig" node got=""
    TALOS_VERIFIED="not derived"
    if [ ! -r "$tc" ]; then
        missing "talos/" "no talosconfig at $tc — the bundle is derived through it"
        return
    fi
    mkdir -p "$KIT/talos"
    # Two reads of the same config, the raw file first: `read` hands back
    # the document the node boots from; `get machineconfig -o yaml` (the
    # read infra/cluster/talos/README.md already uses) wraps it in a
    # resource whose `spec:` block is the config, four spaces in. Either
    # is proven the same way below, so the second is a fallback, not a guess.
    for node in $(control_planes); do
        if timeout "$CALL_TIMEOUT" talosctl --talosconfig "$tc" -e "$node" -n "$node" read /system/state/config.yaml \
            > "$KIT/talos/controlplane.yaml" 2>> "$WORK/talosctl.err" \
            && [ -s "$KIT/talos/controlplane.yaml" ]; then
            got="$node"; break
        fi
        if timeout "$CALL_TIMEOUT" talosctl --talosconfig "$tc" -e "$node" -n "$node" get machineconfig v1alpha1 -o yaml \
            > "$WORK/mc.yaml" 2>> "$WORK/talosctl.err"; then
            awk '/^spec:/ { on = 1; next } on && /^[^ ]/ { exit } on { sub(/^    /, ""); print }' \
                "$WORK/mc.yaml" > "$KIT/talos/controlplane.yaml"
            rm -f "$WORK/mc.yaml"
            if [ -s "$KIT/talos/controlplane.yaml" ]; then got="$node"; break; fi
        fi
    done
    if [ -z "$got" ]; then
        rm -rf "$KIT/talos"
        missing "talos/" "no control-plane node in $(basename "$ESTATE") gave its machine config to talosctl read or get machineconfig within ${CALL_TIMEOUT}s ($(control_planes | tr '\n' ' ')): $(tail -c 300 "$WORK/talosctl.err" 2>/dev/null | tr '\n' ' ')"
        return
    fi
    TALOS_FROM="$got"
    if ! timeout "$CALL_TIMEOUT" talosctl gen secrets --from-controlplane-config "$KIT/talos/controlplane.yaml" \
        --output-file "$KIT/talos/secrets.yaml" > /dev/null 2>> "$WORK/talosctl.err" \
        || [ ! -s "$KIT/talos/secrets.yaml" ]; then
        rm -f "$KIT/talos/secrets.yaml"
        missing "talos/secrets.yaml" "talosctl gen secrets refused the control-plane config read from $got: $(tail -c 300 "$WORK/talosctl.err" | tr '\n' ' ')"
        return
    fi

    # THE PROOF BY EFFECT: the derived bundle's CAs are the ones this
    # cluster's own clients trust. An absent CA would hash to the digest
    # of nothing, so it is refused by name rather than read as a mismatch.
    local os_digest k8s_digest
    if [ -z "$(yaml_at "$KIT/talos/secrets.yaml" certs.os.crt)" ] || [ -z "$(yaml_at "$KIT/talos/secrets.yaml" certs.k8s.crt)" ]; then
        refuse_talos "the bundle talosctl derived from $got carries no certs.os.crt or certs.k8s.crt, so it cannot be proven this cluster's"
        return
    fi
    os_digest="$(yaml_at "$KIT/talos/secrets.yaml" certs.os.crt | digest_of_b64)"
    k8s_digest="$(yaml_at "$KIT/talos/secrets.yaml" certs.k8s.crt | digest_of_b64)"
    if ! any_value_matches "$os_digest" "$tc" ca; then
        refuse_talos "the derived bundle's os CA DIFFERS from the talosconfig's ca: the config read from $got is not this cluster's"
        return
    fi
    if [ -r "$OPS_DIR/kubeconfig" ]; then
        if ! any_value_matches "$k8s_digest" "$OPS_DIR/kubeconfig" certificate-authority-data; then
            refuse_talos "the derived bundle's k8s CA DIFFERS from the kubeconfig's certificate-authority-data: the config read from $got is not this cluster's"
            return
        fi
        TALOS_VERIFIED="os CA = talosconfig ca; k8s CA = kubeconfig certificate-authority-data"
    else
        TALOS_VERIFIED="os CA = talosconfig ca; k8s CA UNCHECKED (no kubeconfig)"
    fi
}

# ---------------------------------------------------------------------------
# --assemble
# ---------------------------------------------------------------------------
mount_tmpfs() {
    local opts="size=$SIZE,mode=0700,uid=0,gid=0,nosuid,nodev,noexec" swaps
    SWAP_NOTE="noswap"
    if mount -t tmpfs -o "$opts,noswap" boss-recovery-kit "$DIR" 2> "$DIR.mount-err"; then
        rm -f "$DIR.mount-err"
        return 0
    fi
    # An older kernel refuses the option; mount without it, and say what
    # that means on THIS host.
    mount -t tmpfs -o "$opts" boss-recovery-kit "$DIR" 2>> "$DIR.mount-err" \
        || { cat "$DIR.mount-err" >&2; rm -f "$DIR.mount-err"; return 1; }
    rm -f "$DIR.mount-err"
    swaps="$(swapon --show=NAME --noheadings 2>/dev/null | tr '\n' ' ')"
    if [ -n "${swaps// /}" ]; then
        SWAP_NOTE="WARNING: this kernel refused noswap and swap is on (${swaps% }) — a tmpfs page of this kit can reach that disk"
        say "$SWAP_NOTE"
    else
        SWAP_NOTE="no noswap on this kernel; swapon lists no swap device"
    fi
}

mode_assemble() {
    need_uid cut-recovery-kit
    for t in findmnt mount umount tar sha256sum base64 flock awk timeout; do
        command -v "$t" >/dev/null 2>&1 || die 1 "$t is not installed — nothing was read"
    done
    [ -r "$README_SRC" ] || die 1 "$README_SRC is missing from the checkout"
    take_lock

    local old
    old="$(state version)"
    if is_tmpfs; then
        wipe
        [ -n "$old" ] && say "replacing kit $old, which was still in $DIR"
        SWAP_NOTE="(the tmpfs was already mounted by an earlier cut)"
    else
        mkdir -p "$DIR" && chmod 0700 "$DIR" || die 1 "cannot create $DIR"
        mount_tmpfs || die 1 "mounting a tmpfs on $DIR failed — nothing was read"
    fi
    # From here until the state is written, ANY exit discards: a cut that
    # did not finish leaves no credential resident (review finding 5).
    # A signal (the runner's 900 s is a SIGTERM) becomes an exit, so the
    # EXIT trap runs for it too, once any bounded call in flight returns.
    trap 'discard' EXIT
    trap 'exit 143' TERM INT HUP
    is_tmpfs || die 1 "$DIR is not a tmpfs after mounting it ($(findmnt -n -o FSTYPE --mountpoint "$DIR" 2>/dev/null || echo 'not a mount point')) — no credential is written onto a disk"
    chmod 0700 "$DIR"

    WORK="$DIR/.work"; KIT="$DIR/kit"
    mkdir -p "$WORK" "$KIT"
    FORGE_LOG_SHOW=""; TALOS_FROM=""; RUNNER_FROM=""

    forge_archive
    # No GitHub token rides the kit (backlog d2b7c947, 2026-09-30). The
    # one it took was the personal access token of David's own GitHub
    # account, retired with the public fork it pushed to; every GitHub
    # credential the estate holds now is an installation token of the
    # organisation's App, minted per act and revoked, which a stick
    # cannot usefully keep.
    take secrets/talosconfig "$OPS_DIR/talosconfig"
    take secrets/kubeconfig "$OPS_DIR/kubeconfig"
    runner_registration
    sudoers_entries
    offsite_key
    talos_bundle

    # --- the manifest --------------------------------------------------
    local items taken_epoch taken_at full version n_items completeness writer_sha
    items="$(cd "$KIT" && find . -type f -print0 | sort -z | xargs -0 -r stat -c '%s %n' \
        | while read -r bytes path; do
            printf 'ITEM %s %s %s\n' "$(sha256sum "$path" | cut -d' ' -f1)" "$bytes" "${path#./}"
        done)"
    [ -n "$items" ] || die 1 "no item could be taken — nothing is kept"
    full="$(printf '%s\n' "$items" | sha256sum | cut -d' ' -f1)"
    version="${full:0:12}"
    taken_epoch="$(date -u +%s)"
    taken_at="$(date -u -d "@$taken_epoch" +%Y-%m-%dT%H:%M:%SZ)"
    n_items="$(printf '%s\n' "$items" | grep -c '^ITEM ')"
    if [ "$N_MISSING" -eq 0 ]; then completeness="complete"; else completeness="INCOMPLETE missing=$MISSING_PATHS"; fi
    writer_sha="$(sha256sum "$REPO/$WRITER" 2>/dev/null | cut -d' ' -f1)"

    {
        echo "BOSS recovery kit — MANIFEST (names, sizes and sha256 only; no secret value)"
        echo "kit_version $version"
        echo "kit_version_sha256 $full"
        echo "completeness $completeness"
        echo "taken_at $taken_at"
        echo "taken_on ${HOST_ID:-$(hostname)}"
        echo "cut_by ${OPS_REQUEST_ID:-a hand run}"
        echo "items $n_items present, $N_MISSING missing"
        printf '%s\n' "$items"
        printf '%s' "$MISSING"
        echo "talos_bundle_verified ${TALOS_VERIFIED}${TALOS_FROM:+ (config read from $TALOS_FROM)}"
        [ -n "$RUNNER_FROM" ] && echo "runner_registration_from $RUNNER_FROM"
        echo "tmpfs_swap $SWAP_NOTE"
        echo "writer $WRITER sha256 ${writer_sha:-unreadable}"
    } > "$WORK/MANIFEST.items"

    sed -e "s|{{VERSION}}|$version|g" -e "s|{{TAKEN_AT}}|$taken_at|g" \
        -e "s|{{FORGE_HOST}}|${BOSS_FORGE_HOST:-(unknown)}|g" -e "s|{{CHECKOUT}}|$REPO|g" \
        -e "s|{{READER}}|$READER|g" \
        "$README_SRC" > "$DIR/README.txt"
    if [ "$N_MISSING" -gt 0 ]; then
        { echo; echo "INCOMPLETE — MISSING WHEN THIS KIT WAS CUT, not on this stick:"; printf '%s' "$MISSING"; } >> "$DIR/README.txt"
    fi

    # --- the tar: README and the item manifest inside, under a named dir.
    local named="$DIR/boss-recovery-kit-$version"
    mv "$KIT" "$named" || die 1 "could not name the kit directory"
    cp "$DIR/README.txt" "$named/README.txt"
    cp "$WORK/MANIFEST.items" "$named/MANIFEST.txt"
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$taken_epoch" \
        -C "$DIR" -cf "$DIR/kit.tar" "boss-recovery-kit-$version" 2> "$WORK/tar.err" \
        || { cat "$WORK/tar.err" >&2; die 1 "tar failed — nothing is kept"; }
    rm -rf "$named"
    local tar_sha tar_bytes complete_flag
    tar_sha="$(sha256sum "$DIR/kit.tar" | cut -d' ' -f1)"
    tar_bytes="$(stat -c %s "$DIR/kit.tar")"
    { cat "$WORK/MANIFEST.items"; echo "tar_sha256 $tar_sha"; echo "tar_bytes $tar_bytes"; } > "$DIR/MANIFEST.txt"
    if [ "$N_MISSING" -eq 0 ]; then complete_flag=yes; else complete_flag=no; fi
    printf 'version=%s\ntaken_epoch=%s\nrequest=%s\ncomplete=%s\nmissing=%s\n' \
        "$version" "$taken_epoch" "${OPS_REQUEST_ID:-}" "$complete_flag" "$MISSING_PATHS" > "$DIR/state"
    # READ BACK before the kit is called cut (backlog fdbb447e part 1: a
    # MUTATING verb's success line follows a read of its effect). This
    # script runs without -e, so neither write above was checked: a state
    # file that never landed, or a manifest that does not vouch for the
    # tar on disk, still reached COMPLETE. Both are re-read now; a
    # mismatch dies inside the EXIT trap, so nothing is left resident.
    local back_sha
    back_sha="$(sha256sum "$DIR/kit.tar" | cut -d' ' -f1)"
    case "$back_sha" in
        ''|*[!0-9a-f]*) die 1 "kit.tar could not be re-hashed after the cut — nothing is kept" ;;
    esac
    [ "$(sed -n 's/^tar_sha256 //p' "$DIR/MANIFEST.txt")" = "$back_sha" ] \
        || die 1 "MANIFEST.txt does not read back as vouching for kit.tar (sha256 $back_sha) — nothing is kept"
    [ "$(state version)" = "$version" ] \
        || die 1 "$DIR/state does not read back as kit $version — nothing is kept"
    # The kit is whole and servable: from here an exit keeps it.
    trap - EXIT TERM INT HUP

    # --- what the packet records ----------------------------------------
    if [ "$N_MISSING" -gt 0 ]; then
        echo "INCOMPLETE — kit $version is cut and writable, and $N_MISSING item(s) are MISSING (named below). A stick written from it is named -INCOMPLETE and restores everything else."
    else
        echo "COMPLETE — kit $version is cut: $n_items items, every one present."
    fi
    echo
    cat "$DIR/MANIFEST.txt"
    if [ -n "$FORGE_LOG_SHOW" ]; then
        echo
        echo "forge-backup.sh's account of the refused dump:"
        sed 's/^/    /' "$WORK/forge-backup.log"
    fi
    rm -rf "$WORK"
    echo
    echo "WRITE IT — on a machine that has joined the estate's tunnel, with the stick mounted, from the root"
    echo "of a checkout of this repository whose $WRITER has the sha256 above, run the"
    echo "line below with STICK_MOUNT_PATH replaced by the stick's mount path. gpg asks for the passphrase to"
    echo "set; the writer then re-mounts the stick and gpg asks again to read it back. The kit waits in the"
    echo "forge's RAM for 24h, and is discarded once a write reads back."
    echo
    echo "bash $WRITER $version STICK_MOUNT_PATH"
    echo
    echo "The writer's prerequisites, the forced-command key and the restore order are in the README it writes"
    echo "beside the kit. discard-recovery-kit drops the kit from RAM without writing it."
    [ "$N_MISSING" -eq 0 ] || exit 3
    exit 0
}

case "${1:-}" in
    --assemble) mode_assemble ;;
    --discard)
        need_uid discard-recovery-kit
        take_lock
        if is_tmpfs; then
            gone="$(state version)"
            discard
            # READ BACK (backlog fdbb447e part 1). discard() only SAYS
            # when its umount fails and carries on, so this line used to
            # read "unmounted" over a tmpfs still mounted. What must hold
            # is that no credential is resident: the directory is read
            # back empty, and where it stands is reported as measured.
            if ! left="$(find "$DIR" -mindepth 1 -maxdepth 1 -print -quit 2>&1)"; then
                say "FAILED — $DIR could not be read back after the discard ($left); whether kit ${gone:-(none cut)} is gone is unknown"
                exit 1
            fi
            if [ -n "$left" ]; then
                say "FAILED — kit ${gone:-(none cut)} is NOT discarded: $DIR still holds $left"
                exit 1
            fi
            if is_tmpfs; then
                where="still a mounted tmpfs (the umount failed above; a reboot clears it)"
            else
                where="unmounted"
            fi
            echo "$ME: kit ${gone:-(none cut)} discarded; $DIR read back empty and $where"
        else
            # "Not a mounted tmpfs" is findmnt's silence, and findmnt is
            # silent on its own ERROR too (the delta review of b3ffc175,
            # backlog 1058e686): read that way, a kit still resident was
            # reported as none cut and the run judged proven. So the
            # directory itself is read back — nothing may be in it, on a
            # disk or in a tmpfs findmnt could not see — before the verb
            # may say there is nothing to discard.
            if [ -e "$DIR" ] && ! left="$(find "$DIR" -mindepth 1 -maxdepth 1 -print -quit 2>&1)"; then
                say "FAILED — findmnt does not show $DIR as a mounted tmpfs, and $DIR could not be read back ($left); whether a kit is resident is unknown"
                exit 1
            fi
            if [ -n "${left:-}" ]; then
                say "FAILED — findmnt does not show $DIR as a mounted tmpfs, yet $DIR holds $left; a kit may be resident — nothing was discarded"
                exit 1
            fi
            echo "$ME: no kit is cut ($DIR is not a mounted tmpfs) — nothing to discard"
        fi
        ;;
    *)
        echo "usage: $ME --assemble | --discard   (a workstation uses $READER and $WRITER)" >&2
        exit 2
        ;;
esac
