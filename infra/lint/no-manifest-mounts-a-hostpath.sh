#!/usr/bin/env bash
#
# no-manifest-mounts-a-hostpath — no pod this tree declares mounts the
# node's filesystem, because the cluster refuses the mount and the gate
# cannot see the refusal.
#
# WHAT HAPPENED (backlog d42d4967, 2026-09-12). gate-seed-local.yaml
# landed on #340 with a prepare CronJob whose hostPath mount
# (DirectoryOrCreate) was the one thing that would create the local
# PV's directory on w-1. Its manifest comment reasoned that boss-dev
# "carries no pod-security enforce label", so the mount was admitted.
# The label IS absent — and that is exactly why the namespace takes the
# Talos machine-config default, `enforce: baseline`, which forbids
# hostPath volumes. Run by hand, the Job created NO pod in ten minutes
# and no condition; its twin with `emptyDir: {}` in the same slot
# completed in twenty seconds. Nothing in the cluster could then make
# the directory, so the first gate after the train would have hung at
# MountVolume.SetUp — every car blocked, by a manifest the gate had
# passed green, because the gate applies nothing and the only reader of
# an admission verdict is the API server.
#
# WHAT IS ASSERTED. No file under infra/cluster/manifests/ or
# infra/gate-runner/ contains a `hostPath:` volume. Every namespace this
# tree declares is baseline or stricter (boss.yaml labels `boss`
# baseline; boss-dev inherits the same default), so a hostPath anywhere
# here is a pod that will never be admitted. The CronJob it caught is
# gone; the directory is the node's own declaration (Talos
# `machine.files` + `machine.kubelet.extraMounts` on w-1 — see gate-seed-local.yaml).
#
# There is no exemption list, and there still is none. This header used
# to end: "a workload that truly needs the node's filesystem needs a
# namespace labelled `privileged` first, and that is a decision with its
# own car; the day it is taken, this lint learns the namespace, not the
# file." That day was 2026-10-07 (backlog 17f6170c: a daily fstrim of
# the build node's filesystems, in boss-node-maintenance.yaml), and this
# is the lint learning it — FROM THE DECLARATION, not from a name typed
# here: a hostPath is admitted only in a document whose own
# `metadata.namespace` is a namespace some manifest under these roots
# declares with `pod-security.kubernetes.io/enforce: privileged`.
# Everywhere else the mount is still a pod that is never admitted, and
# still refused.
#
# FAILS CLOSED. A document that does not state its namespace in block
# style (`metadata:` then `  namespace: <ns>`, unquoted), or a Namespace
# whose name is not read the same way, matches nothing and is refused:
# the reader is line-based, and what it cannot read it does not admit.
#
# EVERY DOCUMENT, IN EVERY STYLE (review 16efea28, B2). The first
# version of this reader split documents on a line that was exactly
# `---` and looked for `hostPath:` at the start of a line. YAML starts a
# document at ANY line beginning `---` — `--- # comment`, or the dashes
# with the document's first content after them — and writes the same
# mount in flow style as `{hostPath: {path: /}}`. A flow-style DaemonSet
# behind `--- # x` was therefore read as more of the Namespace before
# it, its mount never seen, and this lint printed ok. Now a document
# starts at any line beginning with three dashes, what follows the
# dashes is read as that document's first line, and a mount is the bare
# word `hostPath` anywhere on a line that is not a comment.
#
# ONE FILE'S WORD IS NOT A MOUNT, BY NAME (backlog 17f6170c, the fold
# onto 8eac4893). infra/cluster/manifests/boss-node-maintenance-admission.yaml
# is the admission policy that BOUNDS the privileged namespace, and a
# policy has to spell what it bounds: its CEL reads `v.hostPath` and its
# refusal message says "hostPath". Those four lines are text inside a
# ValidatingAdmissionPolicy, which mounts nothing. So the bare word is
# not counted on a line of THAT file, in a document whose `kind:` line
# reads ValidatingAdmissionPolicy — and nowhere else: any other document
# in that file, and every other file, is read as before. The file's
# Namespace document is still read (it is where the privileged label now
# lives). What keeps a workload out of that file is its own pin,
# the_node_maintenance_namespace_is_bounded_at_admission.rs, which reads
# it with a YAML reader and holds it to exactly three objects — the
# policy, its binding, the Namespace.
#
# WHAT THIS DOES NOT BOUND, and what does. A file that labelled its own
# new namespace privileged would pass here. Which namespace may carry
# that label, what it holds, and exactly which paths its one pod mounts
# is crates/core/boss-testing/tests/the_privileged_namespace_holds_one_workload.rs
# — and, at the API server, the admission policy named above.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

ROOTS=(infra/cluster/manifests infra/gate-runner)
# The one file whose policy text may say the word (see the header).
POLICY_FILE=infra/cluster/manifests/boss-node-maintenance-admission.yaml

# A LINE HERE MUST BE A LINE TO KUBECTL (review 9b05cc55, B2a). awk,
# grep and every reader of these files end a line at LF. A YAML reader
# also ends one at a carriage return, U+0085, U+2028 and U+2029 — so a
# `# comment` followed by one of those and a container was a comment to
# this lint and a second, privileged container to kubectl. Refused
# anywhere, comments included, in every yaml under infra/: this is the
# check a builder runs locally, and the pin holds the same rule. Bytes,
# not characters: LC_ALL=C, fixed strings.
breaks="$(LC_ALL=C grep -rlF --include='*.yaml' --include='*.yml' \
    -e $'\r' -e $'\xc2\x85' -e $'\xe2\x80\xa8' -e $'\xe2\x80\xa9' infra 2>/dev/null | LC_ALL=C sort || true)"
if [ -n "$breaks" ]; then
    echo "no-manifest-mounts-a-hostpath: a yaml under infra/ carries a line break that is not LF (a carriage return, U+0085, U+2028 or U+2029). A YAML reader ends a line there and this lint does not, so what follows it is read by kubectl and by nothing here (9b05cc55):" >&2
    printf '%s\n' "$breaks" | sed 's/^/    /' >&2
    echo "    Remove the character; no file here needs one." >&2
    exit 1
fi

files=()
while IFS= read -r f; do files+=("$f"); done < <(find "${ROOTS[@]}" -name '*.yaml' 2>/dev/null | LC_ALL=C sort)
n="${#files[@]}"
lint_scanned no-manifest-mounts-a-hostpath "$n" "manifest(s) under ${ROOTS[*]}"

# One pass, one document at a time: `NS <name>` for each Namespace
# labelled privileged, `HP <namespace|-> <file>:<line>: <text>` for each
# hostPath volume.
facts="$(awk -v policy_file="$POLICY_FILE" '
    function flush() {
        if (kind == "Namespace" && privileged && name != "") print "NS\t" name
        for (i = 1; i <= nhp; i++) print "HP\t" (ns == "" ? "-" : ns) "\t" hp[i]
        kind = ""; name = ""; ns = ""; privileged = 0; nhp = 0; inmeta = 0
    }
    FNR == 1 && NR > 1 { flush() }
    /^---/ { flush(); sub(/^---[ \t]*/, ""); if ($0 == "") next }
    /^[ \t]*#/ { next }
    /^kind: / { kind = $2 }
    /^metadata:[ \t]*$/ { inmeta = 1; next }
    /^[^ \t]/ { inmeta = 0 }
    inmeta && /^  name: / { name = $2 }
    inmeta && /^  namespace: / { ns = $2 }
    /pod-security\.kubernetes\.io\/enforce:[ \t]*"?privileged"?[ \t]*$/ { privileged = 1 }
    /hostPath/ && !(FILENAME == policy_file && kind == "ValidatingAdmissionPolicy") { hp[++nhp] = FILENAME ":" FNR ": " $0 }
    END { flush() }
' "${files[@]}")"

privileged_ns="$(printf '%s\n' "$facts" | awk -F'\t' '$1 == "NS" { print $2 }' | LC_ALL=C sort -u)"
refused=""
admitted=0
while IFS=$'\t' read -r tag ns where; do
    [ "$tag" = HP ] || continue
    if [ "$ns" != "-" ] && printf '%s\n' "$privileged_ns" | grep -xF -- "$ns" >/dev/null; then
        admitted=$((admitted + 1))
    else
        refused="${refused}    ${where}   [namespace: ${ns}]"$'\n'
    fi
done <<< "$facts"

if [ -n "$refused" ]; then
    echo "no-manifest-mounts-a-hostpath: a pod in this tree mounts the node's filesystem outside a namespace the tree labels privileged. Every other namespace enforces baseline, which refuses hostPath — the pod will never be admitted, and the gate cannot tell you (d42d4967):" >&2
    printf '%s' "$refused" >&2
    echo "    A directory on a node is the NODE's declaration (Talos machine.files + kubelet.extraMounts); a file the pod needs rides a PVC, a ConfigMap or a Secret." >&2
    echo "    Namespaces labelled privileged here: $(printf '%s' "${privileged_ns:-none}" | tr '\n' ' '). A new one is its own car and its own review (17f6170c)." >&2
    exit 1
fi
if [ "$admitted" -eq 0 ]; then
    echo "no-manifest-mounts-a-hostpath: ok — $n manifest(s), no hostPath volume"
else
    echo "no-manifest-mounts-a-hostpath: ok — $n manifest(s), $admitted hostPath volume(s), each in a namespace the tree labels privileged: $(printf '%s' "$privileged_ns" | tr '\n' ' ')"
fi
