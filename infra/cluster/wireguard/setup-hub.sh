#!/usr/bin/env bash
# WireGuard hub on the GCP box (dev-cluster Q2: bare WireGuard, decided
# 2026-08-10). Topology: the GCP box is the HUB — stable public IP,
# listening on UDP 51820, overlay 10.99.0.0/24, hub at 10.99.0.1. The
# cluster nodes are SPOKES that dial OUT to the hub (residential NAT
# never needs an inbound hole; PersistentKeepalive holds the mapping).
#
# Idempotent: safe to re-run. Generates the hub key once, writes
# /etc/wireguard/wg0.conf once (never overwrites — peers get APPENDED
# by add-peer.sh so a re-run cannot drop them), enables wg-quick@wg0.
#
# After running, the outputs a cluster node needs are:
#   - the hub public key (printed)
#   - the endpoint <GCP static IP>:51820
#   - a spoke config from peer-template.conf
# Then: ./add-peer.sh <name> <spoke-pubkey> <10.99.0.N> on the hub.
#
# The GCP firewall must allow udp:51820 (one-time):
#   gcloud compute firewall-rules create allow-wireguard \
#     --allow udp:51820 --direction INGRESS

set -euo pipefail

WG_DIR=/etc/wireguard
CONF="$WG_DIR/wg0.conf"
HUB_IP="10.99.0.1/24"
PORT=51820

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
command -v wg >/dev/null || { echo "wireguard-tools not installed" >&2; exit 1; }

umask 077
mkdir -p "$WG_DIR"

if [[ ! -f "$WG_DIR/hub.key" ]]; then
    wg genkey > "$WG_DIR/hub.key"
    wg pubkey < "$WG_DIR/hub.key" > "$WG_DIR/hub.pub"
    echo "==> generated hub keypair"
fi

if [[ ! -f "$CONF" ]]; then
    cat > "$CONF" <<EOF
# WireGuard hub — GCP box. Managed by infra/cluster/wireguard/.
# Peers are appended by add-peer.sh; do not inline them here.
[Interface]
Address = $HUB_IP
ListenPort = $PORT
PrivateKey = $(cat "$WG_DIR/hub.key")
EOF
    echo "==> wrote $CONF"
fi

systemctl enable --now wg-quick@wg0 2>/dev/null || systemctl restart wg-quick@wg0

echo "==> hub up: $(wg show wg0 listen-port 2>/dev/null || echo '(interface pending)')"
echo "==> hub public key (a spoke needs this): $(cat "$WG_DIR/hub.pub")"
echo "==> endpoint: <this box's static IP>:$PORT"
