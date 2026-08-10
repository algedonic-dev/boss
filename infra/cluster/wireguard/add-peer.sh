#!/usr/bin/env bash
# Register a cluster spoke on the hub. Appends the peer to wg0.conf
# (so a setup-hub.sh re-run cannot drop it) and hot-adds it to the
# live interface. Usage:
#   sudo ./add-peer.sh <name> <spoke-public-key> <overlay-ip e.g. 10.99.0.2>

set -euo pipefail

NAME="${1:?usage: add-peer.sh <name> <spoke-pubkey> <overlay-ip>}"
PUBKEY="${2:?spoke public key required}"
OVERLAY_IP="${3:?overlay ip required (10.99.0.N)}"
CONF=/etc/wireguard/wg0.conf

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
[[ -f "$CONF" ]] || { echo "hub not set up — run setup-hub.sh first" >&2; exit 1; }

if grep -q "$PUBKEY" "$CONF"; then
    echo "peer already registered: $PUBKEY"
    exit 0
fi

cat >> "$CONF" <<EOF

# peer: $NAME (added $(date -u +%F))
[Peer]
PublicKey = $PUBKEY
AllowedIPs = $OVERLAY_IP/32
EOF

wg set wg0 peer "$PUBKEY" allowed-ips "$OVERLAY_IP/32"
echo "==> peer '$NAME' registered at $OVERLAY_IP"
