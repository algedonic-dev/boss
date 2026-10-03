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
# `install-units.sh` owns systemd units on boss-gcp. Nothing owned
# `infra/cluster/manifests/`, so "merged" and "running" were different
# states with no observer. This is the observer.
#
# WHAT IT CHECKS. Every named object in every manifest exists in the
# cluster — and for the kinds where a present-but-WRONG object is the
# realistic failure, that its contents match too.
#
# ONE DIRECTION ONLY, and the other one has its own observer. This walks
# the TREE and asks "is each declared object present?". A manifest that
# was DELETED is not in that walk at all, and the object it left behind
# answers the question correctly by being absent from it — so this script
# is structurally blind to an orphan, and the converge's `kubectl apply`
# has no `--prune` to remove one. That direction is
# infra/lint/a-deleted-manifest-leaves-no-object.sh, which the converge
# runs immediately after this one.
#
# EXISTENCE WAS NOT ENOUGH, measured (95f6aba5). The dev-session Role
# was granted batch/jobs create,get,list,watch by hand on 2026-08-28 to
# close a car, and proven by launching a real gate. The grant was never
# written into boss-dev-access.yaml, so the next converge removed it and
# `boss gate` started failing with `jobs.batch is forbidden`. The Role
# EXISTED throughout; only its rules differed, so this check was green
# across the whole window in which the capability it guards silently
# went away.
#
# Note the direction, because it is the interesting part: the cluster
# had MORE than the tree, and the convergence was CORRECT — it removed
# an undeclared grant. The defect was that nothing could see a live
# capability resting on undeclared state, so a `proven in prod` claim
# was true and load-bearing on something with no owner.
#
# So RBAC objects are compared by content: `rules` for Roles, and
# `roleRef` + `subjects` for bindings. A missing rule is invisible and a
# spurious one is a privilege the tree never granted; both now fail.
#
# ADMISSION POLICIES ARE PRIVILEGE TOO (backlog e4a9a9b3; reviews
# 89c716e0 and 87fc0e0c, N3). The break-glass operator's `patch` is
# narrowed by a ValidatingAdmissionPolicy, and whether that policy
# REPORTS or REFUSES is one field of its binding. Existence-only, a
# binding hand-flipped to Deny — or back from Deny to Warn — a
# `repositories` map edited to admit a foreign image, or a binding-level
# matchResources that judges nothing, all read present. So both kinds
# are compared on their whole `spec`, named down to the field that
# differs. The API server DEFAULTS a few fields the tree may omit
# (failurePolicy Fail, matchPolicy Equivalent, a rule's scope `*`, the
# empty selectors), so each is dropped from BOTH sides at its default
# value: a round-trip is not drift, and a value moved OFF its default —
# failurePolicy Ignore, which fails open — still is.
#
# Other kinds stay existence-only — a full drift diff is `kubectl diff`
# and needs write-shaped permission this credential does not have.
#
# AN INSTANCE THE CONVERGE SKIPPED IS NOT MISSING (backlog 07d7549c;
# measured on the forge journal 2026-09-16 06:06Z, main b4d7a0fd). The
# converge skips an instance whose Secrets are not minted yet, whole and
# by name (cluster-deploy-runner.sh, `instances_skipped`); as first
# landed this check was not told, walked the rendered set, and reported
# the skipped playground's 21 objects MISSING — `60 present, 21 missing`,
# exit 1, `result: exit-code` on the converge packet (b7026689), on
# every train until the ceremony. The skip was right; the check had not
# learned it. So the runner hands this check its `instances_skipped`
# field, verbatim — BOSS_INSTANCES_SKIPPED="<ns> (<reason>: …)[; <ns>
# (…)]", the ONE string the packet carries — and an object ABSENT from a
# namespace named there is counted `skipped` (with the instance and the
# reason), not `missing`. A skipped instance's object that IS present
# counts present: the skip explains an absence, it does not excuse a
# read. A skip the converge did not declare is still a miss, and a miss
# in an applied instance still fails beside the skip. And an absence is
# only what kubectl calls NotFound: a read that timed out or answered
# 5xx is unreadable in a skipped instance as in any other (7c023eaf).
#
# EXIT CODES
#   0  every object present — or absent only in an instance the converge
#      declared skipped — and every RBAC and admission object matches
#      the tree
#   1  something in the tree is not in the cluster, or differs from it
#   2  cannot reach the cluster (no credential, no kubectl), or an
#      object's existence read failed with anything but NotFound (a
#      timeout, a refused connection, a 5xx — in a skipped instance
#      too), or an object that exists could not be READ for content — NOT
#      confused with "nothing is applied", because reporting a missing
#      credential as missing infrastructure is the same class of error
#      this script was written about.
#
# RUN IT with a credential that can read the namespaces in question.
# The boss-dev session credential is namespace-scoped and cannot read
# cluster-scoped objects (Namespace, StorageClass) — those are
# reported as unreadable rather than passed, so a narrow credential
# cannot produce a falsely green run.
set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1
DIR="infra/cluster/manifests"

# WHAT THE TREE DECLARES is the RENDERED set, not the directory (backlog
# 07d7549c): the directory is written for prod, and every instance in
# infra/cluster/instances.toml is that directory rendered with its own
# namespace, tenant, sim flag and hostname — which is exactly what the
# converge applies. So this walks the same render the converge applied,
# one directory per namespace, and a playground object that is missing
# is reported by name like a prod one. The render refuses (exit 2) a
# manifest the roster does not classify; a refusal here is 'unknown',
# not 'clean', for the same reason a missing credential is.
RENDERED=$(mktemp -d) || exit 2
trap 'rm -rf "$RENDERED"' EXIT
if ! "$DIR/../render-instance.sh" --all "$RENDERED"; then
    echo "check-manifests-applied: the instance render refused — cannot say what the tree declares." >&2
    exit 2
fi

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
    for f in "$RENDERED"/*/*.yaml; do
        [ -f "$f" ] || continue
        # No {range .items[*]}: kubectl emits one JSON document per
        # object, not a List, so the template applies per document.
        # The source file rides along so the content check can recover
        # what the tree DECLARES for this object.
        # THE SOURCE FILE GOES FIRST, and namespace stays LAST, because
        # tab is IFS whitespace: bash collapses the two consecutive tabs
        # of a cluster-scoped object's empty namespace, and every field
        # after it shifts left. Appending the filename put it in `ns` for
        # every Namespace and StorageClass in the tree — caught by
        # running this, which reported `(ns infra/cluster/manifests/
        # boss-dev.yaml)`. A trailing empty field is harmless; a middle
        # one is not.
        kubectl create --dry-run=client -o \
            'jsonpath={.kind}{"\t"}{.metadata.name}{"\t"}{.metadata.namespace}{"\n"}' \
            -f "$f" 2>/dev/null | sed "s|^|$f\t|"
    done | grep -v '^[[:space:]]*$' | sort -u
)

# Compare the fields that carry the privilege, canonically.
#
# Order is not meaning here: kubectl returns rules and subjects in
# whatever order the API server holds them, and a list reordered by a
# round-trip is not drift. Both sides are normalised — inner lists
# sorted, then the outer list sorted by its serialisation — so only a
# real difference in what is GRANTED can fail this.
content_drift() { # kind name ns file  -> prints a diff summary, or nothing
    python3 - "$1" "$2" "${3:-}" "$4" <<'PY'
import json, subprocess, sys
kind, name, ns, path = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
ADMISSION = kind.startswith("ValidatingAdmissionPolicy")
FIELDS = ["rules"] if kind.endswith("Role") else ["roleRef", "subjects"]

# What the API server fills in on an admission object the tree may leave
# out, as (key, default value). Dropped from both sides only AT that
# value, so a live value moved off it is still read.
DEFAULTS = [("failurePolicy", "Fail"), ("matchPolicy", "Equivalent"), ("scope", "*")]

def undefault(v):
    if isinstance(v, dict):
        return {k: undefault(x) for k, x in v.items() if (k, x) not in DEFAULTS}
    if isinstance(v, list):
        return [undefault(x) for x in v]
    return v

def norm(v):
    if isinstance(v, dict):
        return {k: norm(x) for k, x in sorted(v.items()) if x not in (None, [], {})}
    if isinstance(v, list):
        return sorted((norm(x) for x in v), key=lambda y: json.dumps(y, sort_keys=True))
    return v

# NOTHING READ IS NEVER CLEAN (review d133e704, M1). The existence read
# has already SUCCEEDED when this runs, so nothing after it will count a
# failure here: until 2026-09-30 an unreadable live or declared object
# exited 0 with a comment saying the existence pass handled it, a body
# that was not JSON died with a traceback and an empty stdout, and the
# caller ignored the status — so a content read that timed out on a
# binding moved Deny->Warn recorded "0 drifted, 0 unreadable". Every way
# this cannot compare now prints `UNREADABLE <why>` and exits 3, and the
# caller counts that object unreadable, which exits the run 2.
def unreadable(why):
    print("UNREADABLE " + " ".join(str(why).split())[:300])
    sys.exit(3)

def run(args, what):
    p = subprocess.run(args, capture_output=True, text=True)
    if p.returncode != 0:
        said = (p.stderr.strip().splitlines() or [f"exit {p.returncode}"])[-1]
        unreadable(f"{what}: {said}")
    if not p.stdout.strip():
        unreadable(f"{what}: an empty body")
    return p.stdout

def main():
    nsargs = ["-n", ns] if ns else []
    live = json.loads(run(["kubectl", "get", kind, name, *nsargs, "-o", "json",
                           "--request-timeout=10s"], "the live read"))
    want = run(["kubectl", "create", "--dry-run=client", "-o", "json", "-f", path],
               "the tree's dry run")
    if not isinstance(live, dict):
        unreadable("the live read: not an object")
    # The file may hold several documents; kubectl prints them concatenated.
    docs, dec = [], json.JSONDecoder()
    i, s = 0, want.strip()
    while i < len(s):
        obj, end = dec.raw_decode(s, i)
        docs.append(obj)
        i = end
        while i < len(s) and s[i] in " \n\r\t":
            i += 1
    want = next(
        (d for d in docs
         if isinstance(d, dict) and d.get("kind") == kind
         and (d.get("metadata") or {}).get("name") == name),
        None,
    )
    if want is None:
        unreadable(f"the tree's dry run holds no {kind}/{name}")
    if ADMISSION:
        # Field by field under spec, so the verdict names what moved.
        live_spec = undefault(live.get("spec") or {})
        want_spec = undefault(want.get("spec") or {})
        pairs = [(f"spec.{k}", live_spec.get(k), want_spec.get(k))
                 for k in sorted(set(live_spec) | set(want_spec))]
    else:
        pairs = [(f, live.get(f), want.get(f)) for f in FIELDS]
    differs = [f for f, a, b in pairs if norm(a) != norm(b)]
    if differs:
        print(", ".join(differs) + " differs")

try:
    main()
except SystemExit:
    raise
except Exception as e:  # a body that is not JSON, a shape nobody expected
    unreadable(f"{type(e).__name__}: {e}")
PY
}

# skip_reason NS — the reason the converge gave for skipping NS, or
# nothing when NS was not skipped. The parse of BOSS_INSTANCES_SKIPPED
# is shared with render-tunnel-config.sh, which reads the same string
# for the same reason (40d46042): one parser, sourced.
# shellcheck source=infra/cluster/instances-skipped.lib.sh
. infra/cluster/instances-skipped.lib.sh

total=$(printf '%s\n' "$inventory" | grep -c . || true)
if [ "$total" -lt 5 ]; then
    echo "check-manifests-applied: only parsed $total object(s) from $DIR —" >&2
    echo "  the scrape broke, so a green result would mean nothing." >&2
    exit 2
fi

missing=0; skipped=0; unreadable=0; present=0; drifted=0
skipped_in=""   # "<ns>: <reason>" per skipped instance with an absence, for the summary
while IFS=$'\t' read -r file kind name ns; do
    [ -n "$kind" ] || continue
    # The instance this object was rendered FOR: the render writes each
    # namespace's copy into its own directory, and a Namespace object
    # carries no namespace of its own.
    instance="${file%/*}"; instance="${instance##*/}"
    if [ -n "$ns" ]; then
        args=(-n "$ns")
    else
        args=()
    fi
    out=$(kubectl get "$kind" "$name" "${args[@]}" --request-timeout=10s 2>&1)
    rc=$?
    if [ "$rc" -eq 0 ]; then
        read_ok=1
        case "$kind" in
            Role|ClusterRole|RoleBinding|ClusterRoleBinding|ValidatingAdmissionPolicy|ValidatingAdmissionPolicyBinding)
                # The helper's STATUS is the verdict on whether it read
                # anything; its output alone is not (review d133e704).
                why=$(content_drift "$kind" "$name" "$ns" "$file")
                crc=$?
                if [ "$crc" -ne 0 ]; then
                    reason="${why#UNREADABLE }"
                    echo "  skip    $kind/$name${ns:+ (ns $ns)} — present, but its content could not be read: ${reason:-the content check died (exit $crc)}"
                    unreadable=$((unreadable + 1))
                    read_ok=0
                elif [ -n "$why" ]; then
                    echo "  DRIFT   $kind/$name${ns:+ (ns $ns)} — $why from $DIR/${file##*/} (rendered for ${file%/*})" >&2
                    drifted=$((drifted + 1))
                fi
                ;;
        esac
        [ "$read_ok" -eq 0 ] || present=$((present + 1))
    elif grep -qiE 'forbidden|cannot list|cannot get' <<<"$out"; then
        # Not visible to THIS credential. Say so; never count it green.
        echo "  skip    $kind/$name${ns:+ (ns $ns)} — not readable by this credential"
        unreadable=$((unreadable + 1))
    elif ! grep -qE '^Error from server \(NotFound\):' <<<"$out"; then
        # ONLY NotFound SAYS ABSENT (backlog 7c023eaf; review 6b7032f5,
        # N1). A timeout, a refused connection or a 5xx read nothing, so
        # it is unknown, not missing — and in a skipped instance it is
        # not the skip either: until 2026-09-30 it fell through to the
        # branches below, MISSING (exit 1) in an applied instance and
        # `skipped` (exit 0) in a skipped one, where a read that never
        # happened hid as the converge's own expected absence. kubectl's
        # last line is the reason, in its own words.
        said=$(printf '%s\n' "$out" | grep -v '^[[:space:]]*$' | tail -n 1)
        echo "  skip    $kind/$name${ns:+ (ns $ns)} — its existence could not be read: ${said:-kubectl exit $rc}"
        unreadable=$((unreadable + 1))
    elif why=$(skip_reason "$instance") && [ -n "$why" ]; then
        # Absent from an instance the converge did not apply — expected,
        # named, and counted apart from a miss.
        echo "  skip    $kind/$name${ns:+ (ns $ns)} — instance $instance skipped by the converge ($why)"
        skipped=$((skipped + 1))
        case "$skipped_in" in
            *"$instance: "*) ;;
            *) skipped_in="${skipped_in:+$skipped_in, }$instance: $why" ;;
        esac
    else
        echo "  MISSING $kind/$name${ns:+ (ns $ns)}" >&2
        missing=$((missing + 1))
    fi
done <<< "$inventory"

echo "check-manifests-applied: $present present, $missing missing, $skipped skipped${skipped_in:+ ($skipped_in)}, $drifted drifted, $unreadable unreadable (of $total)"
if [ "$missing" -gt 0 ]; then
    echo "  A manifest in the tree is not in the cluster. Apply it, or delete it —" >&2
    echo "  a file that describes nothing running is worse than no file, because" >&2
    echo "  it reads as infrastructure that exists." >&2
    exit 1
fi
if [ "$drifted" -gt 0 ]; then
    echo "  An RBAC or admission object in the cluster does not say what the tree" >&2
    echo "  declares. Either direction is a defect: a rule the cluster is MISSING is" >&2
    echo "  a capability about to vanish at the next converge, and a rule it has" >&2
    echo "  EXTRA — or a policy judging, or refusing, other than the tree says — is" >&2
    echo "  a privilege nobody declared and nobody owns." >&2
    exit 1
fi
# UNREADABLE IS NOT CLEAN. The namespace-scoped session credential can
# see 2 of these 24 objects, and an exit 0 on that run would report
# "the cluster matches the tree" having checked 8% of it. That is the
# same false comfort this whole script was written against — a green
# result must mean verified, so a partial view exits 2 (unknown) and
# names the number.
if [ "$unreadable" -gt 0 ]; then
    echo "  $unreadable of $total objects were not readable — by this credential, or" >&2
    echo "  their existence or content read failed (each is named above) — so this run verified" >&2
    echo "  $present. That is 'unknown', not 'clean' — rerun, with a credential that" >&2
    echo "  can read them, before believing the cluster matches." >&2
    exit 2
fi
exit 0
