#!/usr/bin/env bash
# admin-kubeconfig-reads — read-only: does the admin kubeconfig the
# acting loops use still read the cluster? (backlog fb444bbb, car 1,
# 2026-09-28.)
#
# rollback-to has no dry run, and the watchdog reads the cluster in
# earnest only when it must roll back — so without this the first proof
# that /etc/boss-ops/kubeconfig works for them would be an outage. This
# makes the same read they make (ops_boss_image: deploy/boss's image,
# through ops_kubectl's sudo docker --mount) and nothing else: it never
# patches, and it is safe to run from a recorded probe or by hand.
#
#   admin-kubeconfig-reads: deploy/boss serves <image> through <path>   exit 0
#   admin-kubeconfig-reads: REFUSED — ... <path> ...: <docker's words>  exit 1
set -uo pipefail
# The registry host the mirrored kubectl image is spelled from, off
# /etc/boss/sor.env — run by hand, no unit hands it over (cf321ffd).
. "$(dirname "$0")/../lib/sor.sh"
. "$(dirname "$0")/../estate/ops-credentials.sh"
kc=$(ops_kubeconfig)
err=$(mktemp) || exit 1
trap 'rm -f "$err"' EXIT
if ready=$(ops_image_ready 2>"$err") && image=$(ops_boss_image 2>"$err") && [ -n "$image" ]; then
    echo "admin-kubeconfig-reads: kubectl image $ready"
    echo "admin-kubeconfig-reads: deploy/boss serves $image through $kc"
    exit 0
fi
echo "admin-kubeconfig-reads: REFUSED — could not read deploy/boss through $kc (root material, placed by David; design 835c0c9c): $(tr '\n' ' ' < "$err")" >&2
exit 1
