#!/usr/bin/env bash
# rollback-to <sha> — roll deploy/boss to a NAMED build, verify it went
# Ready, then READ THE IMAGE BACK. The ops verb `rollback-to`; also
# runnable by hand.
#
# "Roll back" is a target, not a verb (CLAUDE.md §Diagnosis): the sha
# names an image the registry holds; the script never guesses "the
# previous one". Nothing else changes — the converge's stamp stays what
# it was, so the watchdog still knows which build was last converged,
# and main is untouched: a rollback buys time for a fix, it is not one.
#
# THE READ-BACK (backlog 1058e686, car A). `rollout status` reads the
# rollout's PROGRESS, never the image: a patch that changed nothing
# passes it at once, and until this car the verb then printed "Ready on
# REGISTRY:sha" having never read REGISTRY:sha back. So after the wait
# it reads deploy/boss's template image and requires it to equal the
# target, then lists the ReplicaSets and pods labelled app=boss and
# requires the deployment's CURRENT ReplicaSet (the one carrying the
# deployment's revision) to hold at least one Ready pod and EVERY Ready
# pod of deploy/boss to run the target — "every Ready pod" of none would
# be a proof that fails open. The effect line the verb file declares is
# printed only after both reads agree.
#
# A pod's image is read from its SPEC, not containerStatuses[].image:
# the registry also holds `latest`, and containerd may report whichever
# tag of a shared digest it resolved first, so the status name would
# fail a correct rollback. The imageID each Ready pod reports is printed
# beside it for the record.
#
# THE DR RULE. The read-back only REPORTS. The before-read's refusal,
# the patch and the rollout wait are exactly what they were, in the same
# order, and the read-back runs only after the patch and the wait both
# succeeded — nothing it answers can stop, delay or undo a rollback.
# When it disagrees or cannot read, the run exits 1 AFTER the rollback
# was applied and says that it was.
set -uo pipefail
. "$(dirname "$0")/cluster-deploy-lib.sh"
. "$(dirname "$0")/forge-defaults.sh"
. "$(dirname "$0")/../estate/ops-credentials.sh"
. "$(dirname "$0")/../lib/jq.sh"
sha="${1:?rollback-to needs the sha of the build to serve}"
# The admin kubeconfig David places and the converge checks, by the one
# helper (backlog fb444bbb): this runs as root with no HOME under the ops
# runner, and as david by hand — either way through sudo docker, whose
# daemon mounts the root:root 600 file.
K="$(ops_kubectl)"
# The image repo the converge pushes: forge-defaults.sh, from the
# registry host in /etc/boss/sor.env (REGISTRY overrides).
forge_need REGISTRY
target="$REGISTRY:$sha"
# Read before acting: a rollback that cannot read the cluster cannot
# patch it either, and saying so here names the credential rather than
# leaving a bare patch failure (no dry run exists; this read is the
# check infra/forge/admin-kubeconfig-reads.sh makes on its own).
read_err=$(mktemp) || exit 1
trap 'rm -f "$read_err"' EXIT
# The kubectl image first, present and pinned the way the watchdog makes
# it — a bounded pull, and its refusal names the hand fallback (review
# of 8f50d314, M1/L3).
if ! ready=$(ops_image_ready 2>"$read_err") || ! before=$(ops_boss_image 2>"$read_err"); then
    echo "rollback-to: REFUSED — cannot read deploy/boss through $(ops_kubeconfig) (root material, placed by David; design 835c0c9c), so nothing was rolled: $(tr '\n' ' ' < "$read_err")" >&2
    exit 1
fi
echo "rollback-to: kubectl image $ready"
if [ "$before" = "$target" ]; then
    verb="already on"
    was=""
    echo "rollback-to: deploy/boss is already on $target — applying it again, then reading it back"
else
    verb="rolled to"
    was=" (was $before)"
    echo "rollback-to: deploy/boss serves $before — rolling to $target"
fi

# read_back — after the rollout wait: exit 0 and print "<rs>|<n>|<ids>"
# when the template and every Ready pod run $target; exit 1 with the
# disagreement on stdout; exit 2 with what could not be read.
read_back() {
    local dep tmpl rev objs verdict rs n wrong ids
    if ! dep=$($K get deploy boss -n boss -o json --request-timeout=30s 2>"$read_err"); then
        echo "deploy/boss could not be read back: $(tr '\n' ' ' < "$read_err")"
        return 2
    fi
    # jq-1.6 answers 0 over no document at all (backlog d96e38ab), so
    # an empty answer is asked about first — it is not a template.
    if ! jq_doc_text "$dep" \
        || ! tmpl=$(jq -er '.spec.template.spec.containers[0].image' <<<"$dep" 2>"$read_err") \
        || ! rev=$(jq -er '.metadata.annotations["deployment.kubernetes.io/revision"]' <<<"$dep" 2>"$read_err"); then
        echo "deploy/boss read back without a template image or a revision: $(tr '\n' ' ' < "$read_err")"
        return 2
    fi
    if [ "$tmpl" != "$target" ]; then
        echo "deploy/boss's template reads back $tmpl"
        return 1
    fi
    if ! objs=$($K get rs,pods -n boss -l app=boss -o json --request-timeout=30s 2>"$read_err") \
        || ! jq_doc_text "$objs"; then
        echo "the pods of deploy/boss could not be listed: $(tr '\n' ' ' < "$read_err")"
        return 2
    fi
    # One line: current ReplicaSet(s) | Ready pods in it | Ready pods of
    # any deploy/boss ReplicaSet NOT on the target | name@imageID of the
    # current Ready pods. `|` because a tab is IFS whitespace and an
    # empty field would collapse.
    if ! verdict=$(jq -r --arg rev "$rev" --arg img "$target" '
        [.items[] | select(.kind == "ReplicaSet")
          | select(any(.metadata.ownerReferences[]?; .kind == "Deployment" and .name == "boss"))] as $rss
        | [$rss[] | .metadata.name] as $all
        | [$rss[] | select(.metadata.annotations["deployment.kubernetes.io/revision"] == $rev) | .metadata.name] as $cur
        | [.items[] | select(.kind == "Pod" and .metadata.deletionTimestamp == null)
            | select(any(.status.conditions[]?; .type == "Ready" and .status == "True"))
            | . as $p
            | ([.metadata.ownerReferences[]? | select(.kind == "ReplicaSet") | .name
                | select(. as $n | any($all[]; . == $n))] | first) as $rs
            | select($rs != null)
            | $p.spec.containers[0] as $c
            | {name: $p.metadata.name, rs: $rs, image: $c.image,
               id: ([$p.status.containerStatuses[]? | select(.name == $c.name) | .imageID] | first // "?")}] as $ready
        | [$ready[] | select(.rs == ($cur | first))] as $here
        | "\($cur | join(","))|\($here | length)|\([$ready[] | select(.image != $img) | "\(.name) (\(.image))"] | join(", "))|\([$here[] | "\(.name)@\(.id)"] | join(", "))"
        ' <<<"$objs" 2>"$read_err"); then
        echo "the pods of deploy/boss read back unparseable: $(tr '\n' ' ' < "$read_err")"
        return 2
    fi
    IFS='|' read -r rs n wrong ids <<<"$verdict"
    case "$rs" in
        ''|*,*)
            echo "revision $rev of deploy/boss names ${rs:-no} ReplicaSet, not exactly one"
            return 1 ;;
    esac
    if [ -n "$wrong" ]; then
        echo "a Ready pod of deploy/boss runs another image: $wrong"
        return 1
    fi
    case "${n:-0}" in
        0|*[!0-9]*)
            echo "replicaset $rs has no Ready pod"
            return 1 ;;
    esac
    printf '%s|%s|%s\n' "$rs" "$n" "$ids"
}

if _patch_boss_image "$K" "$target" && $K rollout status deploy/boss -n boss --timeout=420s; then
    echo "rollback-to: rollout status says deploy/boss is Ready on $target — reading the image back"
    found=$(read_back)
    rc=$?
    case $rc in
        0)
            IFS='|' read -r rs n ids <<<"$found"
            echo "rollback-to: Ready pod(s) of replicaset $rs, name@imageID: $ids"
            echo "rollback-to: $verb $target — read back: the template and $n Ready pod(s) of replicaset $rs run it$was"
            exit 0 ;;
        1)
            echo "rollback-to: FAILED — the rollback to $target was applied and rollout status said Ready, but $found; deploy/boss is NOT proven restored; hands needed" >&2
            exit 1 ;;
        *)
            echo "rollback-to: CANNOT ANSWER — the rollback to $target was applied and rollout status said Ready, but $found; deploy/boss is not proven restored" >&2
            exit 1 ;;
    esac
fi
echo "rollback-to: $target never went Ready — deploy/boss is NOT restored; hands needed" >&2
exit 1
