#!/usr/bin/env bash
# READ-ONLY, no arguments. Approved design 25bdb2cc Q1: fixed native
# discovery inside the existing operator boundary; no raw configuration,
# logs, native objects or diagnostic text reaches an ops receipt.
# This is configuration discovery, never a retained-history verdict.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

unavailable() {
    printf '{"schema":"boss.admission-source-discovery.v1","scope":"configuration_discovery_only","history_verdict":"unavailable","roster":{"state":"unavailable","reason":"%s"},"nodes":[]}\n' "$1"
    exit 4
}
[ "$#" -eq 0 ] || unavailable unexpected_arguments
# These helpers resolve the estate's existing credential/image identity.
# Their diagnostic text is not this reader's output contract.
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh" 2>/dev/null || unavailable operator_configuration_unavailable
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh" 2>/dev/null || unavailable operator_configuration_unavailable
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh" 2>/dev/null || unavailable operator_configuration_unavailable
command -v jq >/dev/null 2>&1 || unavailable operator_tool_unavailable
url="${BOSS_ESTATE_NODES_URL:-}"
if [ -z "$url" ]; then
    [ -n "${BOSS_JOBS_URL:-}" ] || unavailable estate_unavailable
    url="$BOSS_JOBS_URL/api/estate/nodes"
fi
# The machine token, PRESENTED and never required (design 6805c764;
# backlog 44b2087e): this read went out with x-boss-user alone, and the
# jobs gate recorded it as a caller it would refuse. Asked of the one
# shell reader here, in the script's own shell, because the read runs
# inside $(…). No token, a host off the list, a slot no header can
# carry, no lib beside this file, a header file that cannot be written:
# each leaves MT_HDR empty and the read exactly the read it was. The
# reader's own words are silenced like every helper's above — the ops
# runner folds this verb's stderr into its receipt.
MT_HDR=""
SECRET_LIB="$HERE/../lib/secret-header.sh"
if [ -r "$SECRET_LIB" ]; then
    # shellcheck source=infra/lib/secret-header.sh
    . "$SECRET_LIB" 2>/dev/null
    machine_token_header MT_HDR "$url" 2>/dev/null || MT_HDR=""
fi
# Only estate registry data is transported on stdin. All Kubernetes and
# sensitive Talos bytes stay in the bounded container process's pipes.
registry="$(curl -sS --max-time 15 --max-filesize 1048576 \
    ${MT_HDR:+-H "$MT_HDR"} \
    -H "x-boss-user: $(sor_reader_header 'automation:discover-admission-source')" \
    -w $'\n%{http_code}' "$url" 2>/dev/null)" || unavailable estate_unavailable
# The header file has served its one request: remove it NOW rather than
# at exit, so it does not sit beside a privileged container for the
# fourteen minutes that may follow, and a verb killed in there leaves
# nothing. (A read that failed has already exited through the reader's
# own EXIT trap.)
if declare -F secret_header_close >/dev/null; then secret_header_close 2>/dev/null; fi
MT_HDR=""
code="${registry##*$'\n'}"
case "$code" in 200 | 000) ;; *) unavailable estate_http_unavailable ;; esac
registry="${registry%$'\n'*}"
ops_image_ready >/dev/null 2>&1 || unavailable operator_image_unavailable
image="$(ops_image 2>/dev/null)" || unavailable operator_image_unavailable

# The source program and declaration are fixed checkout files, mounted
# read-only alongside the existing credentials. No host scratch for raw
# protected input; read-only root, ephemeral tmpfs and no capabilities.
# Both bounds matter: killing only the Docker client can leave a container.
status=0
report="$(timeout -k 5 855 sudo -n docker run --rm -i --network host \
    --read-only --cap-drop ALL --security-opt no-new-privileges --ulimit core=0 \
    --tmpfs /tmp:rw,noexec,nosuid,size=8m \
    --mount "type=bind,src=$(ops_kubeconfig),dst=/kc,readonly" \
    --mount "type=bind,src=${BOSS_OPS_DIR:-/etc/boss-ops}/talosconfig,dst=/tc,readonly" \
    --mount "type=bind,src=${BOSS_TALOSCTL:-/usr/local/bin/talosctl},dst=/talosctl,readonly" \
    --mount "type=bind,src=$HERE/admission-source.py,dst=/discovery.py,readonly" \
    --mount "type=bind,src=$HERE/../ops/admission-discovery-target.json,dst=/target.json,readonly" \
    --entrypoint timeout "$image" -k 5 850 python3 -B /discovery.py \
    <<< "$registry" 2>/dev/null)" || status=$?
case "$status" in 0 | 4) ;; *) unavailable operator_read_unavailable ;; esac
# The host accepts exactly one report and the fixed top-level contract.
# It never echoes a malformed container answer or a Docker error.
if ! jq_doc_text "$report" || ! jq -e -s -f "$HERE/admission-source-report.jq" <<< "$report" >/dev/null 2>&1; then
    unavailable operator_report_unavailable
fi
printf '%s\n' "$report"
exit "$status"
