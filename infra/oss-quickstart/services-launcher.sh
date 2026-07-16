#!/usr/bin/env bash
# Tiny supervisor — starts every BOSS service binary in the
# background, captures their PIDs, and waits. SIGTERM forwards to
# every child; if any child exits, the launcher exits too so
# Docker's restart policy can kick in.
#
# This isn't a real init system (no log rotation, no restart-on-
# crash, no dependency ordering beyond start-everything-in-parallel).
# It's the simplest thing that lets `docker compose up boss-services`
# bring the full stack up. Production deploys use systemd directly;
# tenants who outgrow the docker quickstart have the runbook for
# that path.

set -euo pipefail

# Each entry: "<binary> <port-env-var>=<port> [extra env]"
#
# All binaries read BOSS_POSTGRES_URL + BOSS_NATS_URL from the env;
# the per-service URLs that consumers use are also driven from
# boss-ports defaults, so we don't need to set BOSS_*_URL here as
# long as the hostnames resolve in-container (they do — every
# service runs in this same container, so localhost works).
SERVICES=(
    # Clock first — the authoritative "what time is it" service
    # (boss-clock + boss-clock-client). The brewery-sim probes
    # /api/clock/now at startup and event time is clock-authoritative,
    # so the clock must be listening before the sim (and before any
    # event-emitting service) comes up.
    "boss-clock-api"
    "boss-policy-api"
    "boss-classes-api"
    "boss-locations-api"
    "boss-subject-kinds-api"
    "boss-people-api"
    "boss-accounts-api"
    "boss-assets-api"
    "boss-catalog-api"
    "boss-campaigns-api"
    "boss-customers-api"
    "boss-products-api"
    "boss-commerce-api"
    "boss-inventory-api"
    "boss-shipping-api"
    "boss-messages-api"
    "boss-calendar-api"
    "boss-content-api"
    "boss-ledger-api"
    "boss-docs-api"
    "boss-events-api"
    "boss-jobs-api"
    # boss-observability is a non-`-api` service: NATS aggregator +
    # /api/snapshot for the /ops dashboard. Required for /ops to
    # render anything — without it the gateway's /api/snapshot proxy
    # returns 502 and the page reads as broken. Brewery deploys
    # configure [demo_agents] so the snapshot ships synthetic agent
    # telemetry (with the SPA-side "demo mode" banner on /ops).
    "boss-observability"
    "boss-dispatcher"
    "boss-brewery-sim"
    # Gateway last — depends on every other service being reachable.
    "boss-gateway"
)

PIDS=()

# Generate /etc/boss-*.toml configs at container start. The API
# binaries default --config to /etc/<name>.toml; bare-metal installs
# get these via infra/deploy-services.sh, the docker image via this
# generator. Single-container assumption: every cross-service URL
# is 127.0.0.1:<port>.
if command -v boss-generate-configs >/dev/null 2>&1; then
    boss-generate-configs
else
    echo "WARN: boss-generate-configs not on PATH — /etc/boss-*.toml configs missing; many services will fail" >&2
fi

cleanup() {
    echo "==> launcher SIGTERM — stopping ${#PIDS[@]} children"
    for pid in "${PIDS[@]}"; do
        kill -TERM "$pid" 2>/dev/null || true
    done
    wait
    exit 0
}
trap cleanup TERM INT

# Seed the platform operator-baseline (emp-audit + the bootstrap-admin)
# and then the brewery tenant (JobKinds + policy + accounts/vendors/data)
# before the sim starts. Both go through the public API — and boss-init
# can't do them (the API isn't up during init), so they run here, just
# before the sim opens its first Job. operator-baseline FIRST so the
# platform-admin login + emp-audit exist before the brewery seed + sim.
# Shared with bare-metal bootstrap-local.sh.
publish_brewery_tenant() {
    /opt/boss/infra/seed-operator-baseline.sh
    /opt/boss/infra/seed-brewery-tenant.sh
}

echo "==> boss-launch starting ${#SERVICES[@]} services"
for svc in "${SERVICES[@]}"; do
    # Just before the sim — which posts jobs immediately — make sure the
    # brewery JobKinds + policy grants exist (the jobs-api it needs is up
    # by now, having been started earlier in this loop).
    if [[ "$svc" == "boss-brewery-sim" ]]; then
        publish_brewery_tenant
    fi
    if ! command -v "$svc" >/dev/null 2>&1; then
        echo "    SKIP: $svc (binary not in image)"
        continue
    fi
    echo "    starting $svc"
    "$svc" 2>&1 &
    PIDS+=($!)
    # Tiny stagger so log lines from different services don't
    # interleave during the first second.
    sleep 0.1
done

echo "==> all services up — pid count: ${#PIDS[@]}"
echo "==> SPA: http://localhost:4443"

# Wait for any child to exit. If one dies, exit so docker compose
# restart policy decides what to do.
wait -n
exit_code=$?
echo "==> a child exited with code $exit_code — shutting down"
cleanup
