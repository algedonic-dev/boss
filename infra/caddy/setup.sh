#!/usr/bin/env bash
# --------------------------------------------------------------------------
# Boss — Caddy front door setup
#
# Installs Caddy, drops in the Caddyfile, wires up the hostname, and starts
# the service. Caddy handles Let's Encrypt issuance + renewal automatically
# as long as DNS resolves and ports 80/443 are reachable.
#
# Usage:
#   sudo BOSS_HOSTNAME=example.com ./setup.sh
#
# Environment:
#   BOSS_HOSTNAME — public hostname that resolves to this VM (required)
# --------------------------------------------------------------------------
set -euo pipefail

BOSS_HOSTNAME="${BOSS_HOSTNAME:?BOSS_HOSTNAME is required (e.g. example.com)}"
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

# ---------- install caddy via official apt repo ----------
if ! command -v caddy >/dev/null 2>&1; then
    echo "==> Installing Caddy"
    sudo apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
        | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
        | sudo tee /etc/apt/sources.list.d/caddy-stable.list
    sudo apt-get update -qq
    sudo apt-get install -y caddy
else
    echo "==> Using existing caddy ($(caddy version))"
fi

# ---------- config ----------
echo "==> Installing Caddyfile for ${BOSS_HOSTNAME}"
sudo install -m 0644 "${REPO_ROOT}/infra/caddy/Caddyfile" /etc/caddy/Caddyfile

# ---------- environment (hostname) ----------
sudo mkdir -p /etc/systemd/system/caddy.service.d
sudo tee /etc/systemd/system/caddy.service.d/hostname.conf >/dev/null <<EOF
[Service]
Environment=BOSS_HOSTNAME=${BOSS_HOSTNAME}
EOF

# ---------- start ----------
sudo systemctl daemon-reload
sudo systemctl enable --now caddy
sudo systemctl restart caddy

echo "==> Status:"
sudo systemctl --no-pager status caddy | head -n 10 || true

echo
echo "Caddy listening on :80, :443"
echo "Certificate issuance logs: journalctl -u caddy -f"
echo "Test (after cert issues): curl -v https://${BOSS_HOSTNAME}/health"
echo
echo "If LE issuance fails (rate limit or DNS not resolving yet), the"
echo "Caddyfile can be switched to 'tls internal' for self-signed certs"
echo "— browser warning only, but useful for bring-up."
