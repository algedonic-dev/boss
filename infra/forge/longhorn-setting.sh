#!/usr/bin/env bash
# longhorn-setting.sh — READ one Longhorn setting from a fixed allow-list:
# the value it stands at and Longhorn's own definition of it. The ops verb
# `read-longhorn-setting`.
#
#   longhorn-setting.sh <setting>
#
# WHY IT EXISTS (backlog e14a472d, option C; review 0e863731 of car 1).
# The dev-build workers' class places replicas only on nodes Longhorn
# tags dev-build, and node-converge puts that tag on w-1 and w-2. Before
# it does, the live allow-empty-node-selector-volume must be READ: when
# it is false, a node carrying ANY tag takes no replica of a volume whose
# class names no selector — /work and the gate disk — so tagging the two
# workers would move every new untagged replica onto the control planes.
# The tree never sets it and Longhorn's default is true, but a default is
# not a reading, and no door read it: longhorn-drain-policy.sh reads only
# node-drain-policy, fixed in a script that also carries a passkey-signed
# write. This is its sibling and not an argument added to it, so the
# write's script and its daily check stay exactly as they were.
#
# THE BOUND is this script's:
#   * ONE setting per run, named from ALLOWED below and nothing else — a
#     word off the list is refused (exit 78) before any door opens. The
#     verb file's `one_of` offers exactly this list, and
#     crates/core/boss-testing/tests/read_longhorn_setting_sh.rs holds
#     the two equal (CLAUDE.md §9a). Adding a setting is adding a word to
#     both, reviewed;
#   * kubectl GET only, through the forge's ops_kubectl door (the admin
#     kubeconfig David placed, infra/estate/ops-credentials.sh): the
#     stored settings.longhorn.io object, and Longhorn's definition from
#     its manager API through the API server's service proxy, the way
#     longhorn-drain-policy.sh reads node-drain-policy's. There is no
#     write in this file, and the same test refuses a door call that is
#     not a get.
#
# THE SECOND WORD, allow-empty-disk-selector-volume (backlog 46328c3a,
# design 3c475a19 D2), is the same question one level down: when it is
# false a TAGGED disk takes no replica of a volume with no disk selector
# — every durable volume alive today. So it says whether w-1's scratch
# disk, once tagged `scratch`, is closed to durable replicas (false) or
# only watched for them (true, Longhorn's default), and it must read
# true before any default disk is tagged `durable` (design car 5), or
# those volumes are left nowhere to rebuild. The scratch disk's
# sequence (infra/cluster/talos/README.md) reads it first.
#
# THE THIRD WORD, guaranteed-instance-manager-cpu (backlog 2b7c78f8), is
# CPU: the percentage of each node's allocatable CPU that Longhorn's
# instance manager pod requests there. On w-1, the build node, that
# request stood beside three gates and the dev-build workers and no
# station row charged it, so the capacity record admitted builders the
# scheduler refused `Insufficient cpu`. Longhorn's default (12, so 3.84
# of w-1's 32) is not a reading; this is. A per-node override on the
# Longhorn node object is not read here — `node-pods <node>` prints the
# instance manager pod's own request, which is what the scheduler counts.
#
# THE ANSWER names the value, whether Longhorn's API agrees and reports
# it applied, Longhorn's default and type (read, never typed here), and
# Longhorn's own words for the setting. Its last line is the verdict
# `READ — <setting> <value>, default <default>, non_default <0|1>`.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look, a definition
# without a default, or a stored value Longhorn's API disagrees with is
# CANNOT ANSWER and exit 1, never a default. Exit 78 is a refusal.
set -uo pipefail

ME=read-longhorn-setting
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

# The settings this verb may read, space-separated, one line.
ALLOWED="allow-empty-node-selector-volume allow-empty-disk-selector-volume guaranteed-instance-manager-cpu"

LH=longhorn-system

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread setting has no answer"; }

[ $# -eq 1 ] || refuse "usage: longhorn-setting.sh <setting> — one setting, one of: $ALLOWED"
SETTING=""
for s in $ALLOWED; do
    [ "$1" = "$s" ] && SETTING="$s"
done
[ -n "$SETTING" ] || refuse "'$1' is not a setting this verb reads — it reads only: $ALLOWED"
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — Longhorn's answer cannot be read, and no evidence is not an answer"

# Measured 2026-09-30 against Longhorn v1.11.3 (longhorn-drain-policy.sh):
# the definition answers `default` (and, for a choice, `options`) under
# `.definition`; the CR carries only the value.
DEFINITION="/api/v1/namespaces/$LH/services/longhorn-backend:9500/proxy/v1/settings/$SETTING"

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# The stored setting.
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K get settings.longhorn.io "$SETTING" -n "$LH" -o json --request-timeout=20s \
        > "$WORK/setting.json" 2> "$WORK/setting.err"; then
    unread "settings.longhorn.io $SETTING: $(tr '\n' ' ' < "$WORK/setting.err")"
fi
jq_doc_file "$WORK/setting.json" \
    && jq -e --arg s "$SETTING" '.metadata.name == $s and (.value | type == "string") and .value != ""' \
        "$WORK/setting.json" > /dev/null 2>&1 \
    || unread "settings.longhorn.io $SETTING did not read back as that setting with a value (its first 200 bytes: '$(head -c 200 "$WORK/setting.json" | tr '\n' ' ')')"
CUR="$(jq -r '.value' "$WORK/setting.json")"

# Longhorn's definition of it.
# shellcheck disable=SC2086
if ! $K get --raw "$DEFINITION" --request-timeout=20s > "$WORK/def.json" 2> "$WORK/def.err"; then
    unread "Longhorn's definition of $SETTING ($DEFINITION): $(tr '\n' ' ' < "$WORK/def.err")"
fi
jq_doc_file "$WORK/def.json" \
    && jq -e --arg s "$SETTING" '.id == $s
        and (.definition.default | type == "string" and . != "")
        and (.value | type == "string")' "$WORK/def.json" > /dev/null 2>&1 \
    || unread "Longhorn's API did not answer a definition of $SETTING with its default (its first 200 bytes: '$(head -c 200 "$WORK/def.json" | tr '\n' ' ')')"
DEF="$(jq -r '.definition.default' "$WORK/def.json")"
API_VALUE="$(jq -r '.value' "$WORK/def.json")"
APPLIED="$(jq -r 'if .applied == true then "true" elif .applied == false then "false" else "unreported" end' "$WORK/def.json")"
TYPE="$(jq -r '.definition.type // "unreported"' "$WORK/def.json")"
OPTIONS="$(jq -r 'if (.definition.options | type) == "array" and (.definition.options | length) > 0 then "; options: " + (.definition.options | join(", ")) else "" end' "$WORK/def.json")"
WORDS="$(jq -r '(.definition.description // "") | split("\n") | map(select(. != "")) | .[0] // "its definition carries no description"' "$WORK/def.json")"

[ "$CUR" = "$API_VALUE" ] \
    || unread "the stored setting says $SETTING is '$CUR' and Longhorn's API says '$API_VALUE' — they disagree, so which one stands is not known"

echo "$ME: $SETTING is $CUR (settings.longhorn.io, and Longhorn's API agrees; applied $APPLIED); Longhorn's default is $DEF; type $TYPE$OPTIONS"
echo "$ME: Longhorn's words for $SETTING: $WORDS"
if [ "$CUR" = "$DEF" ]; then
    echo "$ME: READ — $SETTING $CUR, default $DEF, non_default 0"
else
    echo "$ME: NON-DEFAULT — $SETTING is $CUR, not Longhorn's default $DEF"
    echo "$ME: READ — $SETTING $CUR, default $DEF, non_default 1"
fi
exit 0
