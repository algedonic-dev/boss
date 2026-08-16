#!/usr/bin/env bash
# Is what's in the tree what's running in the cluster?
#
# WHY THIS EXISTS. `boss-dev.yaml` was merged on train 36 (2026-08-15)
# and applied by hand from a laptop. Nothing recorded that it had been
# applied, and nothing would have said if it hadn't. A day later a
# design doc asserted — in writing, to David — that the dev pod had
# never run, while it was sitting there with 25 hours of uptime and a
# bound 40 Gi volume. The reasoning was "no script applies it and no
# reachable host has kubectl, therefore nobody has one", which was
# sound and wrong.
#
# `deploy-services.sh` owns systemd units on boss-gcp. Nothing owned
# `infra/cluster/manifests/`, so "merged" and "running" were different
# states with no observer. This is the observer.
#
# WHAT IT CHECKS. Every named object in every manifest exists in the
# cluster. Existence, not equality — a full drift diff is `kubectl
# diff` and needs write-shaped permission this credential does not
# have. The failure this catches is the one that actually happened:
# a manifest that was never applied at all.
#
# EXIT CODES
#   0  every object present
#   1  something in the tree is not in the cluster
#   2  cannot reach the cluster (no credential, no kubectl) — NOT
#      confused with "nothing is applied", because reporting a missing
#      credential as missing infrastructure is the same class of error
#      this script was written about.
#
# RUN IT with a credential that can read the namespaces in question.
# The boss-dev session credential is namespace-scoped and cannot read
# cluster-scoped objects (Namespace, StorageClass) — those are
# reported as skipped rather than passed, so a narrow credential
# cannot produce a falsely green run.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
DIR="infra/cluster/manifests"

command -v kubectl >/dev/null 2>&1 || {
    echo "check-manifests-applied: kubectl not found — cannot verify." >&2
    echo "  This is 'unknown', not 'clean'. Install kubectl and point" >&2
    echo "  KUBECONFIG at a credential that can read the boss namespaces." >&2
    exit 2
}
if ! kubectl version -o json --request-timeout=10s >/dev/null 2>&1; then
    echo "check-manifests-applied: cannot reach the cluster API — cannot verify." >&2
    exit 2
fi

# kind/name/namespace for every document, via kubectl's own parser so
# this does not grow a YAML implementation.
inventory=$(
    for f in "$DIR"/*.yaml; do
        [ -f "$f" ] || continue
        # No {range .items[*]}: kubectl emits one JSON document per
        # object, not a List, so the template applies per document.
        kubectl create --dry-run=client -o \
            'jsonpath={.kind}{"\t"}{.metadata.name}{"\t"}{.metadata.namespace}{"\n"}' \
            -f "$f" 2>/dev/null
    done | grep -v '^[[:space:]]*$' | sort -u
)

total=$(printf '%s\n' "$inventory" | grep -c . || true)
if [ "$total" -lt 5 ]; then
    echo "check-manifests-applied: only parsed $total object(s) from $DIR —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 2
fi

missing=0; skipped=0; present=0
while IFS=$'\t' read -r kind name ns; do
    [ -n "$kind" ] || continue
    if [ -n "$ns" ]; then
        args=(-n "$ns")
    else
        args=()
    fi
    out=$(kubectl get "$kind" "$name" "${args[@]}" --request-timeout=10s 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        present=$((present + 1))
    elif printf '%s' "$out" | grep -qiE 'forbidden|cannot list|cannot get'; then
        # Not visible to THIS credential. Say so; never count it green.
        echo "  skip    $kind/$name${ns:+ (ns $ns)} — not readable by this credential"
        skipped=$((skipped + 1))
    else
        echo "  MISSING $kind/$name${ns:+ (ns $ns)}" >&2
        missing=$((missing + 1))
    fi
done <<< "$inventory"

echo "check-manifests-applied: $present present, $missing missing, $skipped unreadable (of $total)"
if [ "$missing" -gt 0 ]; then
    echo "  A manifest in the tree is not in the cluster. Apply it, or delete it —" >&2
    echo "  a file that describes nothing running is worse than no file, because" >&2
    echo "  it reads as infrastructure that exists." >&2
    exit 1
fi
# UNREADABLE IS NOT CLEAN. The namespace-scoped session credential can
# see 2 of these 24 objects, and an exit 0 on that run would report
# "the cluster matches the tree" having checked 8% of it. That is the
# same false comfort this whole script was written against — a green
# result must mean verified, so a partial view exits 2 (unknown) and
# names the number.
if [ "$skipped" -gt 0 ]; then
    echo "  $skipped of $total objects were not readable by this credential, so this" >&2
    echo "  run verified $present. That is 'unknown', not 'clean' — rerun with a" >&2
    echo "  credential that can read them before believing the cluster matches." >&2
    exit 2
fi
exit 0
