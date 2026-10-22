#!/usr/bin/env bash
# infra/forge/netboot.sh — network boot from the forge: a blank node
# PXE-boots the cluster's Talos image into maintenance mode with no USB
# (backlog ee31ab73, David 2026-10-19).
#
#   netboot.sh declared                 the declared pair, KEY=VALUE
#   netboot.sh mirror <images-dir>      fetch + verify the pair; prints its dir
#   netboot.sh render-ipxe <base-url>   the iPXE script, to stdout
#   netboot.sh render-dnsmasq <addr> <tftp-root> <script-url>
#   netboot.sh ensure                   the converge step (install.sh calls it)
#   netboot.sh units                    the daemons `ensure` runs, one per line
#
# WHY. w-2 was reinstalled on a new disk and looped on PXE: nothing on
# the LAN answered, so every blank node needed hand-made USB media. The
# forge is managed, on the same L2 as every node, and already the image
# trust root, so it answers — in the shape the conductor decided on the
# packet, which this file does not re-decide:
#
#   1. dnsmasq in PROXY-DHCP mode on the forge's host network. The UniFi
#      gateway keeps handing out every lease; this answers only PXE
#      clients, with a boot file and never an address, and serves no DNS
#      (`port=0`). Firmware PXE gets iPXE over TFTP — undionly.kpxe for
#      BIOS, ipxe.efi for UEFI, from the distro's apt-signed `ipxe`
#      package — and iPXE (user-class iPXE) gets the script's URL.
#   2. The script boots ONE declared Talos kernel and initramfs, mirrored
#      from factory.talos.dev at converge time and served over plain HTTP
#      by netboot-http.py only when each file's sha256 is the declared
#      one. The declaration is infra/cluster/talos/netboot.toml, read
#      here and nowhere else in this tree.
#   3. Scope stops at maintenance mode: the node joins nothing and
#      installs nothing until an operator applies a config with talosctl.
#
# TRUST. Only the NICs the declaration names in netboot_macs get this
# boot; every other PXE client gets no answer at all, and an empty list
# answers nobody (review bcff47dd N1: a LAN-wide answer would boot any
# installed node that tries the network first into maintenance from
# RAM). Every machine that
# PXE-boots here is ours and the image is the cluster's own, but the two
# daemons are an attack surface on the LAN and are written as one: the
# config admits a fixed set of directives (a lease, a route or a resolver
# cannot ride in), the kernel args are refused unless every byte is in a
# set iPXE cannot read as a command, the HTTP server answers three exact
# paths with bytes it verified at start and holds open, and runs as a
# dynamic user that can read nothing else.
#
# LOGS ARE KEPT. dnsmasq logs every PXE exchange (`log-dhcp`) and the
# server every request, both to the journal; nothing here is -q.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ME=netboot

# The one declaration. Overridable for the tests; `ensure` on a real host
# refuses the override (below), so the forge boots the tree's pair.
DECLARATION="${NETBOOT_DECLARATION:-$HERE/../cluster/talos/netboot.toml}"
FACTORY="${NETBOOT_FACTORY_URL:-https://factory.talos.dev}"
# The port the iPXE script and the images are served on, on the forge's
# own LAN address. The car's probe reads this line.
NETBOOT_HTTP_PORT=8079
FILES=(kernel-amd64 initramfs-amd64.xz)
# The two daemons, as forge-netboot-<name>.service: `ensure` brings each
# up and `units` names them for the forge's unit observer (backlog
# 2eae38d4), so what is started and what is watched are one list.
NETBOOT_DAEMONS="dhcp http"

# The kernel args land on an iPXE `kernel` line: `$`, `&&`, `||`, `;`,
# `#` or a quote there is iPXE syntax, so every byte must be one of these.
ARG_BYTES='A-Za-z0-9._=,:/+-'

refuse() { # <exit> <line>
    echo "$ME: $2" >&2
    exit "$1"
}

decl_value() { # <key> — exactly one `key = "value"` line, or exit 2
    local n
    # grep -c exits 1 for "no line" (n is then 0) and 2 for "cannot read".
    n="$(grep -c -E "^$1 = \"" "$DECLARATION")" || [ $? -eq 1 ] \
        || refuse 2 "cannot read the declaration $DECLARATION"
    [ "$n" = 1 ] || refuse 2 "$DECLARATION declares $1 $n time(s), not once"
    sed -n -E "s/^$1 = \"(.*)\"[[:space:]]*$/\1/p" "$DECLARATION"
}

# D_SCHEMATIC, D_VERSION, D_KERNEL_SHA256, D_INITRAMFS_SHA256, D_ARGS —
# each validated, so nothing downstream handles a value it did not shape.
read_declared() {
    [ -r "$DECLARATION" ] || refuse 2 "the declaration $DECLARATION cannot be read"
    D_SCHEMATIC="$(decl_value schematic)"
    D_VERSION="$(decl_value version)"
    D_KERNEL_SHA256="$(decl_value kernel_sha256)"
    D_INITRAMFS_SHA256="$(decl_value initramfs_sha256)"
    D_ARGS="$(decl_value kernel_args)"
    [[ "$D_SCHEMATIC" =~ ^[0-9a-f]{64}$ ]] || refuse 2 "schematic is not a 64-hex Image Factory id: $D_SCHEMATIC"
    [[ "$D_VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || refuse 2 "version is not a Talos tag (vX.Y.Z): $D_VERSION"
    [[ "$D_KERNEL_SHA256" =~ ^[0-9a-f]{64}$ ]] || refuse 2 "kernel_sha256 is not a sha256: $D_KERNEL_SHA256"
    [[ "$D_INITRAMFS_SHA256" =~ ^[0-9a-f]{64}$ ]] || refuse 2 "initramfs_sha256 is not a sha256: $D_INITRAMFS_SHA256"
    [[ "$D_ARGS" =~ ^[$ARG_BYTES]+(\ [$ARG_BYTES]+)*$ ]] \
        || refuse 2 "kernel_args holds a byte outside [$ARG_BYTES] or a doubled space — iPXE could read it as a command: $D_ARGS"
    case " $D_ARGS " in
        *" talos.platform=metal "*) ;;
        *) refuse 2 "kernel_args does not carry talos.platform=metal — these are the factory's metal args: $D_ARGS" ;;
    esac
    # D_MACS — WHO IS ANSWERED (review bcff47dd N1, backlog 8cf6faf7).
    # Lowercase colon-separated only: iPXE prints ${netX/mac} that way,
    # and the script's iseq compares strings. Split by read -a, never by
    # an unquoted expansion, so a `*` cannot glob before it is refused.
    D_MACS="$(decl_value netboot_macs)"
    D_MAC_LIST=()
    if [ -n "$D_MACS" ]; then
        [[ "$D_MACS" =~ ^[0-9a-f:]+(\ [0-9a-f:]+)*$ ]] \
            || refuse 2 "netboot_macs is not lowercase colon-separated MACs separated by single spaces: '$D_MACS'"
        local mac seen=" "
        read -r -a D_MAC_LIST <<<"$D_MACS"
        for mac in "${D_MAC_LIST[@]}"; do
            [[ "$mac" =~ ^[0-9a-f]{2}(:[0-9a-f]{2}){5}$ ]] \
                || refuse 2 "netboot_macs holds '$mac', not a lowercase colon-separated MAC (aa:bb:cc:dd:ee:ff)"
            case "$seen" in *" $mac "*) refuse 2 "netboot_macs names $mac twice" ;; esac
            seen="$seen$mac "
        done
    fi
}

want_sha() { # <file name>
    case "$1" in
        kernel-amd64) echo "$D_KERNEL_SHA256" ;;
        initramfs-amd64.xz) echo "$D_INITRAMFS_SHA256" ;;
    esac
}

sha_of() { sha256sum -- "$1" | awk '{print $1}'; }

# The URL path the pair is served under: the version and schematic are in
# it, so a console that prints what iPXE fetches says which image it is.
served_prefix() { echo "/talos/$D_VERSION/$D_SCHEMATIC"; }

# mirror <images-dir>: <images-dir>/<version>-<schematic>/ holds both
# files with exactly the declared bytes, or this exits 1 having removed
# whatever did not match — a partial download, a changed byte, a file
# swapped on disk. The factory is asked only for what is not already
# here and verified, so a converge tick with the pair in place needs no
# network. Prints the verified directory last.
mirror() {
    local root="$1" gen name want got tmp url
    gen="$root/$D_VERSION-$D_SCHEMATIC"
    install -d -m 0755 -- "$root" "$gen"
    for name in "${FILES[@]}"; do
        want="$(want_sha "$name")"
        if [ -f "$gen/$name" ] && [ ! -L "$gen/$name" ]; then
            got="$(sha_of "$gen/$name")"
            if [ "$got" = "$want" ]; then
                echo "$ME: $name is the declared bytes ($want)" >&2
                continue
            fi
            echo "$ME: $gen/$name is $got, not the declared $want — removed, and fetched again" >&2
        fi
        rm -f -- "$gen/$name"
        tmp="$gen/.$name.partial"
        rm -f -- "$tmp"
        url="$FACTORY/image/$D_SCHEMATIC/$D_VERSION/$name"
        echo "$ME: fetching $url" >&2
        if ! curl -sSfL --connect-timeout 20 --max-time 900 --retry 2 -o "$tmp" -- "$url"; then
            rm -f -- "$tmp"
            echo "$ME: could not fetch $url — nothing is served until it is here and verified" >&2
            return 1
        fi
        got="$(sha_of "$tmp")"
        if [ "$got" != "$want" ]; then
            rm -f -- "$tmp"
            echo "$ME: REFUSED $url — its sha256 is $got, the declaration says $want. Removed; not served." >&2
            return 1
        fi
        chmod 0644 -- "$tmp"
        mv -f -- "$tmp" "$gen/$name"
        echo "$ME: $name verified ($want)" >&2
    done
    # The one line on stdout: the directory that holds the verified pair.
    echo "$gen"
}

# render-ipxe <base-url>. The factory's script, served from here: its
# kernel args verbatim, plus `initrd=initramfs-amd64.xz`. That one is
# OURS and is why this is not a copy: the factory's script expects an
# iPXE new enough to hand a UEFI kernel its initramfs through LoadFile2,
# and the distro's ipxe.efi this host serves may predate it — Debian
# bookworm's is git-20190125, whose binary carries no LoadFile2 or
# initrd.magic at all (read 2026-10-19). Named on the command line, the
# kernel's EFI stub loads the image iPXE registered under that name; on
# BIOS iPXE passes the initrd itself and the arg is inert, and a newer
# iPXE that offers LoadFile2 is preferred by the kernel over it.
#
# THE GATE FIRST (review bcff47dd N1). dnsmasq already answers only the
# declared MACs; this is the second fence, for an iPXE that reached the
# script another way. A NIC the declaration does not name runs `exit`,
# which hands control back to the firmware, and the firmware tries its
# next boot device — the installed disk — instead of booting Talos from
# RAM. Each MAC was validated by read_declared, so no byte here is iPXE
# syntax. ${netX/mac} is iPXE's to expand, never the shell's (SC2016).
# shellcheck disable=SC2016
render_ipxe() {
    local base="$1" at mac
    at="$base$(served_prefix)"
    printf '#!ipxe\n'
    printf '# Rendered by infra/forge/netboot.sh from infra/cluster/talos/netboot.toml:\n'
    printf '# Talos %s, Image Factory schematic %s, booted into maintenance mode.\n' "$D_VERSION" "$D_SCHEMATIC"
    printf '# Only for the NICs netboot_macs declares; every other one exits to its disk.\n'
    for mac in "${D_MAC_LIST[@]}"; do
        printf 'iseq ${netX/mac} %s && goto talos ||\n' "$mac"
    done
    printf 'echo BOSS netboot: ${netX/mac} is not declared in infra/cluster/talos/netboot.toml - exiting to the next boot device\n'
    printf 'exit\n'
    printf ':talos\n'
    printf 'imgfree\n'
    printf 'kernel %s/kernel-amd64 %s initrd=initramfs-amd64.xz\n' "$at" "$D_ARGS"
    printf 'initrd %s/initramfs-amd64.xz\n' "$at"
    printf 'boot\n'
}

# render-dnsmasq <forge-addr> <tftp-root> <script-url>. Proxy-DHCP for PXE
# clients only (decision 1). Every directive here is one the test admits.
#
# ONLY THE DECLARED MACS ARE ANSWERED (review bcff47dd N1, backlog
# 8cf6faf7). Answering every PXE client meant any installed node whose
# firmware tries the network before its disk — a control plane, w-1, the
# forge itself — would boot Talos maintenance from RAM on its next
# reboot. Each declared MAC is tagged `netboot`; `dhcp-ignore` drops
# every client without the tag. In dnsmasq 2.90 (src/rfc2131.c,
# dhcp_reply) both PXE paths, the proxy offer and the port-4011
# boot-server ACK, send NO packet when the client is ignored, so the
# firmware times out and boots its disk. An empty declaration tags no
# one, and answers no one: closed is the default.
render_dnsmasq() {
    local addr="$1" tftp="$2" url="$3" csa mac
    cat <<EOF
# Rendered by infra/forge/netboot.sh — do not edit; the forge converge
# re-renders it every tick. Proxy-DHCP for PXE clients ONLY: the LAN's
# DHCP server keeps every lease, and this hands out none.
# No DNS.
port=0
# Only the forge's LAN address — never a docker bridge or loopback.
listen-address=$addr
# Proxy mode: a boot file for PXE clients, never an address.
dhcp-range=$addr,proxy
# And no lease database at all: there is nothing to record.
leasefile-ro
# Every PXE exchange to the journal.
log-dhcp
# systemd supervises it in the foreground; no pid file.
pid-file=
user=nobody
group=nogroup
enable-tftp
tftp-root=$tftp
# iPXE announces itself; firmware PXE does not.
dhcp-userclass=set:ipxe,iPXE
# Who is answered: the NICs infra/cluster/talos/netboot.toml declares in
# netboot_macs, and nobody else — an undeclared client gets no packet.
dhcp-ignore=tag:!netboot
EOF
    for mac in "${D_MAC_LIST[@]}"; do
        printf 'dhcp-mac=set:netboot,%s\n' "$mac"
    done
    printf 'pxe-service=tag:!ipxe,x86PC,"BOSS netboot: chain to iPXE",undionly.kpxe\n'
    for csa in X86-64_EFI BC_EFI; do
        printf 'pxe-service=tag:!ipxe,%s,"BOSS netboot: chain to iPXE",ipxe.efi\n' "$csa"
    done
    for csa in x86PC X86-64_EFI BC_EFI; do
        printf 'pxe-service=tag:ipxe,%s,"BOSS netboot: Talos maintenance",%s\n' "$csa" "$url"
    done
}

# place <dest> <mode> <src-file>: replace <dest> with <src-file>'s bytes,
# renamed into place. Exit 0 when the bytes changed, 1 when they did not.
place() {
    local dest="$1" mode="$2" src="$3" tmp
    if [ -f "$dest" ] && cmp -s -- "$src" "$dest"; then
        return 1
    fi
    tmp="$(dirname -- "$dest")/.$(basename -- "$dest").new"
    install -m "$mode" -- "$src" "$tmp"
    mv -f -- "$tmp" "$dest"
    return 0
}

place_text() { # <dest> <mode> <text>
    local f rc=0
    f="$(mktemp)"
    printf '%s' "$3" >"$f"
    place "$1" "$2" "$f" || rc=$?
    rm -f -- "$f"
    return "$rc"
}

ensure() {
    # WHICH MACHINE THIS IS, BEFORE ANYTHING IS WRITTEN (infra/lib/
    # host-check.sh carries the incident and the rule). Every place this
    # writes is a seam: all redirected, it is a test and is asked
    # nothing; any one real, this machine must hold the forge's declared
    # address, or the exit is 78 and nothing was written.
    # shellcheck source=infra/lib/host-check.sh
    . "$HERE/../lib/host-check.sh" 2>/dev/null \
        || refuse 78 "REFUSED — $HERE/../lib/host-check.sh cannot be read, so which machine this is cannot be established; nothing was written"
    host_seam INSTALL_ETC /etc/systemd/system
    host_seam INSTALL_SYSTEMCTL systemctl
    host_seam INSTALL_APT_GET
    host_seam INSTALL_NETBOOT_ROOT /var/lib/boss/netboot
    host_check forge "$ME"
    # ON A REAL HOST THE TREE DECIDES WHAT BOOTS. The overrides below are
    # for the tests; a real run that inherited one would serve a pair the
    # tree never declared, so it is refused rather than honoured.
    if [ "${#host_real[@]}" -gt 0 ]; then
        local o
        for o in NETBOOT_DECLARATION NETBOOT_FACTORY_URL NETBOOT_IPXE_DIR NETBOOT_DNSMASQ; do
            [ -z "${!o:-}" ] || refuse 78 "REFUSED — $o is set on a real host; the forge boots the tree's declaration from the factory, nothing else. Nothing was written."
        done
    fi

    local etc systemctl apt root ipxe dnsmasq
    etc="${INSTALL_ETC:-/etc/systemd/system}"
    systemctl="${INSTALL_SYSTEMCTL:-systemctl}"
    apt="${INSTALL_APT_GET:-apt-get}"
    root="${INSTALL_NETBOOT_ROOT:-/var/lib/boss/netboot}"
    ipxe="${NETBOOT_IPXE_DIR:-/usr/lib/ipxe}"
    dnsmasq="${NETBOOT_DNSMASQ:-/usr/sbin/dnsmasq}"
    if [ "$etc" = /etc/systemd/system ] && [ "$(id -u)" -ne 0 ]; then
        refuse 1 "ensure needs root to write $etc"
    fi
    # shellcheck source=infra/run-summary.sh
    . "$HERE/../run-summary.sh"
    # shellcheck source=infra/lib/sor.sh
    . "$HERE/../lib/sor.sh"
    sor_require BOSS_FORGE_HOST
    local addr="$BOSS_FORGE_HOST"
    [[ "$addr" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] \
        || refuse 1 "BOSS_FORGE_HOST is not an IPv4 address dnsmasq can listen on: $addr"
    read_declared

    # The packages: dnsmasq-base is the binary alone — the `dnsmasq`
    # package would also enable the distro's own DNS service on :53 — and
    # ipxe ships the two boot loaders handed out over TFTP.
    if [ ! -x "$dnsmasq" ] || [ ! -e "$ipxe/ipxe.efi" ] || [ ! -e "$ipxe/undionly.kpxe" ]; then
        echo "$ME: installing dnsmasq-base and ipxe"
        if ! DEBIAN_FRONTEND=noninteractive timeout 300 "$apt" install -y \
            -o DPkg::Lock::Timeout=60 --no-install-recommends dnsmasq-base ipxe; then
            run_summary_field netboot "down: apt-get could not install dnsmasq-base ipxe"
            refuse 1 "apt-get could not install dnsmasq-base ipxe — the LAN has no network boot until it does"
        fi
    fi

    local gen
    install -d -m 0755 -- "$root"
    if ! gen="$(mirror "$root/images")" || [ "$gen" != "$root/images/$D_VERSION-$D_SCHEMATIC" ]; then
        # Serve NOTHING rather than a pair the tree no longer declares: a
        # node that finds no answer stays where it is, and this run is red.
        "$systemctl" stop forge-netboot-http.service || true
        "$systemctl" stop forge-netboot-dhcp.service || true
        rm -f -- "$root/serve.env"
        run_summary_field netboot "down: the declared Talos $D_VERSION pair could not be fetched and verified — nothing is served (the journal says which file)"
        refuse 1 "REFUSED to serve: the declared pair is not here verified; network boot is STOPPED"
    fi

    local dhcp_changed=0 http_changed=0 base f u server_sha
    base="http://$addr:$NETBOOT_HTTP_PORT"
    install -d -m 0755 -- "$root/tftp"
    for f in ipxe.efi undionly.kpxe; do
        # install(1) reads through the distro's symlink (/usr/lib/ipxe/
        # ipxe.efi -> /boot/ipxe.efi on Debian) and copies the bytes.
        place "$root/tftp/$f" 0644 "$ipxe/$f" || true
    done
    place_text "$root/dnsmasq.conf" 0644 "$(render_dnsmasq "$addr" "$root/tftp" "$base/boot.ipxe")
" && dhcp_changed=1
    place_text "$root/boot.ipxe" 0644 "$(render_ipxe "$base")
" && http_changed=1
    place "$root/netboot-http.py" 0644 "$HERE/netboot-http.py" && http_changed=1
    server_sha="$(sha_of "$root/netboot-http.py")"
    place_text "$root/serve.env" 0644 "NETBOOT_BIND=$addr
NETBOOT_PORT=$NETBOOT_HTTP_PORT
NETBOOT_IMAGES=$gen
NETBOOT_SCRIPT=$root/boot.ipxe
NETBOOT_PREFIX=$(served_prefix)
NETBOOT_KERNEL_SHA256=$D_KERNEL_SHA256
NETBOOT_INITRAMFS_SHA256=$D_INITRAMFS_SHA256
NETBOOT_SERVER_SHA256=$server_sha
" && http_changed=1
    place "$etc/forge-netboot-dhcp.service" 0644 "$HERE/forge-netboot-dhcp.service" && dhcp_changed=1
    place "$etc/forge-netboot-http.service" 0644 "$HERE/forge-netboot-http.service" && http_changed=1

    # Older generations are not served and not kept.
    for f in "$root/images"/*; do
        [ "$f" = "$gen" ] && continue
        [[ "$(basename -- "$f")" =~ ^v[0-9.]+-[0-9a-f]{64}$ ]] || continue
        rm -rf -- "$f"
        echo "$ME: removed the generation no longer declared: $f"
    done

    "$systemctl" daemon-reload
    local unit changed states="" bad=""
    for u in $NETBOOT_DAEMONS; do
        unit="forge-netboot-$u.service"
        changed=$dhcp_changed
        [ "$u" = http ] && changed=$http_changed
        if [ "$changed" -eq 1 ]; then
            "$systemctl" enable "$unit" || true
            "$systemctl" restart "$unit" || true
        else
            "$systemctl" enable --now "$unit" || true
        fi
        # is-active exits non-zero for every state but active and still
        # prints the state; an empty answer is a read that failed, named.
        local state state_rc=0
        state="$("$systemctl" is-active "$unit" 2>/dev/null)" || state_rc=$?
        [ -n "$state" ] || state="unread(is-active exit $state_rc)"
        printf '  %-24s %s\n' "$unit" "${state:-unknown}"
        states="$states $u=${state:-unknown}"
        [ "$state" = active ] || bad="$bad $unit"
    done
    if [ -n "$bad" ]; then
        run_summary_field netboot "down:$bad not active (journalctl -u <unit>); declared Talos $D_VERSION at $base/boot.ipxe"
        refuse 1 "network boot is DOWN:$bad not active — its journal says why"
    fi
    run_summary_field netboot "serving Talos $D_VERSION schematic ${D_SCHEMATIC:0:12} at $base/boot.ipxe, answering ${#D_MAC_LIST[@]} declared MAC(s);$states"
    echo "$ME: serving Talos $D_VERSION (schematic $D_SCHEMATIC) at $base/boot.ipxe to ${#D_MAC_LIST[@]} declared MAC(s): ${D_MACS:-none} —$states"
}

case "${1:-}" in
    declared)
        read_declared
        printf 'schematic=%s\nversion=%s\nkernel_sha256=%s\ninitramfs_sha256=%s\nkernel_args=%s\nnetboot_macs=%s\n' \
            "$D_SCHEMATIC" "$D_VERSION" "$D_KERNEL_SHA256" "$D_INITRAMFS_SHA256" "$D_ARGS" "$D_MACS"
        ;;
    mirror)
        [ -n "${2:-}" ] || refuse 2 "usage: $0 mirror <images-dir>"
        read_declared
        mirror "$2"
        ;;
    render-ipxe)
        [ -n "${2:-}" ] || refuse 2 "usage: $0 render-ipxe <base-url>"
        read_declared
        render_ipxe "$2"
        ;;
    render-dnsmasq)
        [ "$#" -eq 4 ] || refuse 2 "usage: $0 render-dnsmasq <forge-addr> <tftp-root> <script-url>"
        read_declared
        render_dnsmasq "$2" "$3" "$4"
        ;;
    ensure)
        ensure
        ;;
    units)
        for u in $NETBOOT_DAEMONS; do echo "forge-netboot-$u.service"; done
        ;;
    *)
        refuse 2 "usage: $0 declared|mirror <dir>|render-ipxe <base>|render-dnsmasq <addr> <tftp> <url>|ensure|units"
        ;;
esac
