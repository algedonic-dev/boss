#!/usr/bin/env bash
# Mint the boss-dev session kubeconfig from the cluster, and print it.
#
# WHAT THIS IS FOR. `boss-dev-access.yaml` declares a ServiceAccount
# whose credential reaches the dev pod and nothing else. This turns
# that ServiceAccount into a kubeconfig a durable session can use from
# boss-gcp — the answer to design Q2, which was explicit that the
# session gets a namespace-scoped credential and NOT a copy of the
# admin config.
#
# It is a script rather than a runbook paragraph because the last
# hand-run step in this area is exactly what went wrong:
# `boss-dev.yaml` was applied by hand on 2026-08-15, nothing recorded
# that it had been, and a day later a design doc asserted the pod had
# never run while it was sitting there with 25 hours of uptime.
#
# RUN IT with the ADMIN config (it reads a Secret, which the minted
# credential deliberately cannot do):
#
#   KUBECONFIG=~/talos-homelab/v2/kubeconfig \
#     infra/cluster/dev-session-kubeconfig.sh > /tmp/boss-dev.kubeconfig
#
# Then place it on the session host and point KUBECONFIG at it. The
# output contains a bearer token: treat it like the forge token —
# 0600, never committed, never echoed into a transcript.
#
# VERIFY, do not assume — the last two claims in this area that went
# unverified were both wrong:
#
#   KUBECONFIG=/tmp/boss-dev.kubeconfig kubectl auth can-i --list -n boss-dev
#   KUBECONFIG=/tmp/boss-dev.kubeconfig kubectl auth can-i get pods -n boss
#     → must print "no"
set -euo pipefail

NS=boss-dev
SA=dev-session
SECRET=dev-session-token
CTX_NAME=boss-dev-session

command -v kubectl >/dev/null 2>&1 || {
    echo "dev-session-kubeconfig: kubectl not found" >&2
    exit 1
}

# The apiserver address the session will dial. Taken from the CURRENT
# context rather than hardcoded: the VIP has moved once already, and a
# baked-in address is a fact living twice (CLAUDE.md 9a).
server=$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')
[ -n "$server" ] || { echo "dev-session-kubeconfig: no server in the current context" >&2; exit 1; }

if ! kubectl get sa "$SA" -n "$NS" >/dev/null 2>&1; then
    echo "dev-session-kubeconfig: ServiceAccount $NS/$SA not found." >&2
    echo "                        Apply infra/cluster/manifests/boss-dev-access.yaml first." >&2
    exit 1
fi

# The token Secret is populated by the controller a moment after the
# Secret is created, so a fresh apply can race this. Wait rather than
# emit a kubeconfig with an empty token — which would authenticate as
# anonymous and fail later, somewhere less obvious.
token=""
for _ in $(seq 1 10); do
    token=$(kubectl get secret "$SECRET" -n "$NS" -o jsonpath='{.data.token}' 2>/dev/null || true)
    [ -n "$token" ] && break
    sleep 1
done
[ -n "$token" ] || {
    echo "dev-session-kubeconfig: $NS/$SECRET has no token after 10s." >&2
    echo "                        Check the annotation names the ServiceAccount." >&2
    exit 1
}
token=$(printf '%s' "$token" | base64 -d)

ca=$(kubectl get secret "$SECRET" -n "$NS" -o jsonpath='{.data.ca\.crt}' 2>/dev/null || true)
[ -n "$ca" ] || {
    echo "dev-session-kubeconfig: $NS/$SECRET carries no ca.crt." >&2
    exit 1
}

cat <<YAML
apiVersion: v1
kind: Config
clusters:
  - name: ${CTX_NAME}
    cluster:
      server: ${server}
      certificate-authority-data: ${ca}
users:
  - name: ${CTX_NAME}
    user:
      token: ${token}
contexts:
  - name: ${CTX_NAME}
    context:
      cluster: ${CTX_NAME}
      user: ${CTX_NAME}
      namespace: ${NS}
current-context: ${CTX_NAME}
YAML
