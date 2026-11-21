#!/usr/bin/env bash
# chore-suspend.sh — suspend or resume ONE cluster chore (a CronJob the
# tree declares with the label boss-chore=true), through a rendered plan
# a passkey signs. The ops verbs `plan-a-chore-suspend` (--plan) and
# `suspend-a-chore`.
#
#   chore-suspend.sh --plan <namespace> <cronjob> <true|false>
#   chore-suspend.sh <namespace> <cronjob> <true|false> <plan-sha256>
#
# WHY IT EXISTS (backlog d28c0051). On 2026-11-11 ~05:00Z the hourly
# chore boss/boss-search-reindex was spilling about 13 GB of temp per
# run onto pgdata-postgres-0, a volume that was degraded and could not
# be expanded (832386eb, ce08007f). The safe lever was spec.suspend=true,
# and no verb under infra/ops/verbs/ pulled it: the dev session is
# correctly refused (patch cronjobs: Forbidden), so David ran
# `kubectl -n boss patch cronjob boss-search-reindex -p
# '{"spec":{"suspend":true}}'` from his workstation at ~05:02Z. Every
# infra act should be a website-approved verb (David, 2026-10-18). This
# is that verb; the resume is the same verb with false. It runs on the
# forge over the ops_kubectl door (the admin kubeconfig David placed,
# infra/estate/ops-credentials.sh).
#
# THE SHAPE is design 17835005's, as set-longhorn-drain-policy's: the
# write's verb file declares requires_approval and names
# plan-a-chore-suspend as its plan_verb, so the runner renders --plan
# onto the request's approve step and hands the write sha256 of the
# SIGNED plan. The write re-renders and patches nothing unless today's
# plan hashes to it. Why the chore is being stopped rides the
# ops-request packet that asks for it, beside the plan it signs.
#
# THE BOUND is this script's, never a parameter:
#   * the CronJob is DECLARED: this checkout's infra/cluster/manifests,
#     rendered for every instance exactly as the converge renders them
#     (infra/cluster/render-instance.sh --all), holds a CronJob of that
#     name in that namespace. So an instance chore is admitted in every
#     namespace the instance roster renders it into (boss AND
#     boss-playground), a pipeline chore only in the namespace its file
#     names, and nothing the tree does not declare — no kube-system, no
#     hand-made CronJob;
#   * the declaration carries the label boss-chore=true — the set the
#     converge already treats as chores (it pins their image by that
#     selector). A CronJob the tree declares without it is refused;
#   * the LIVE object carries boss-chore=true too, and reads back as a
#     CronJob with a boolean spec.suspend;
#   * the converge does not own the field. The converge leaves
#     spec.suspend alone on every chore but one: no manifest declares the
#     field, so its client-side apply never resets it — EXCEPT
#     boss-break-glass-deposit, whose whole /spec the deposit
#     configuration writes on every converge
#     (infra/cluster/deposit-config-patch.jq, `add /spec`), which would
#     put suspend back to false on the next train without a word. That
#     chore is refused by name (CONVERGE_OWNS_SPEC below; the test
#     chore_suspend_sh.rs holds the list to the jq's CronJob);
#   * the value differs from the one that stands — a plan that changes
#     nothing is refused, so an applied plan re-renders to a refusal: at
#     most once.
#
# THE PLAN (stdout; sha256 on stderr as `plan-sha256:`) names the
# CronJob, the file that declares it, spec.suspend before and after, the
# schedule, startingDeadlineSeconds and concurrencyPolicy (a resume with
# no starting deadline starts the most recent missed run AT ONCE —
# Kubernetes' documented behaviour), the one patch and the read-back.
# The chore's last schedule time and the Jobs it is running now are
# printed on stderr, NOT in the signed bytes: they move with every run
# (boss-post-start-schema runs every minute, boss-views-catchup every
# five), and a plan that signed them could not be run before it went
# stale — the precedent is set-longhorn-drain-policy's volume states.
# The write prints them again, on its record, at the moment it patches.
#
# THE WRITE is one `kubectl patch cronjob --type=json` that TESTS
# /spec/suspend is still the planned value and replaces it — a
# compare-and-set on the one field.
#
# THE EFFECT IS READ BACK: the CronJob is read again and only when its
# spec.suspend is the new value is the line the verb file declares as
# `effect` printed; otherwise exit 1 naming what was read.
#
# NO EVIDENCE IS NOT A PASS: a read that could not look is CANNOT ANSWER
# and exit 1, never a plan. Exit 78 is a refusal.
set -uo pipefail

ME=suspend-a-chore
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TREE="$(cd "$HERE/../.." && pwd)"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"
# shellcheck source=infra/estate/ops-credentials.sh
. "$HERE/../estate/ops-credentials.sh"
# jq_doc_file: on jq-1.6 `jq -e` passes an input carrying no document
# (backlog d96e38ab), so every guard below asks that first.
# shellcheck source=infra/lib/jq.sh
. "$HERE/../lib/jq.sh"

# The chores whose spec.suspend the converge itself writes on every
# train, so a suspension would be undone in silence. Each with its
# writer. Space-separated names.
#   boss-break-glass-deposit — infra/cluster/deposit-config-patch.jq
#     replaces its whole /spec from the template (877ad378).
CONVERGE_OWNS_SPEC="boss-break-glass-deposit"
# A Kubernetes namespace is a DNS label; a CronJob's name is one of at
# most 52 characters (the controller appends an 11-character suffix to
# the Jobs it makes). The bound is the declaration, not these shapes.
NS_RE='^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'
NAME_RE='^[a-z0-9]([a-z0-9-]{0,50}[a-z0-9])?$'

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }
unread() { fail "CANNOT ANSWER — $* — an unread chore has no answer, and nothing was written"; }

MODE=write
case "${1-}" in
    --plan)
        MODE=plan
        shift
        [ $# -eq 3 ] || refuse "usage: chore-suspend.sh --plan <namespace> <cronjob> <true|false> — the plan takes the chore and the value and nothing else"
        ;;
    *)
        [ $# -eq 4 ] || refuse "usage: chore-suspend.sh <namespace> <cronjob> <true|false> <plan-sha256> — this patches only an APPROVED plan, and the hash is what the approval signed. Render one with --plan (the plan-a-chore-suspend verb)"
        [[ "$4" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters, got '$4'"
        APPROVED="$4"
        ;;
esac
NS="$1"
NAME="$2"
WANT="$3"
[[ "$NS" =~ $NS_RE ]] || refuse "the namespace must be a Kubernetes namespace name, got '$NS'"
[[ "$NAME" =~ $NAME_RE ]] || refuse "the CronJob must be a CronJob name (lowercase, digits, hyphens, at most 52), got '$NAME'"
case "$WANT" in
    true|false) ;;
    *) refuse "the value is spec.suspend's new value, true (suspend) or false (resume), got '$WANT'" ;;
esac
command -v jq >/dev/null 2>&1 || fail "jq is not on PATH — the CronJob cannot be read, and no evidence is not an answer"

WORK="$(mktemp -d)" || fail "cannot make a scratch directory"
trap 'rm -rf "$WORK"' EXIT

# --- the declared set ----------------------------------------------------------
# Rendered exactly as the converge renders it (cluster-deploy-runner.sh
# `render-instance.sh --all`), from THIS checkout, so the bound is what
# the converge applies and nothing a packet says.
if ! BOSS_CLUSTER_TREE="$TREE" bash "$TREE/infra/cluster/render-instance.sh" --all "$WORK/render" \
        > "$WORK/render.out" 2> "$WORK/render.err"; then
    unread "this checkout's manifests would not render (infra/cluster/render-instance.sh --all): $(tr '\n' ' ' < "$WORK/render.err" | cut -c1-600)"
fi
# declared_cronjobs — namespace<TAB>name<TAB>boss-chore label<TAB>file for
# every CronJob document in the render. awk, not a YAML library: the
# forge promises jq and no python (directive 26d61c97), and the shapes
# the tree writes are a top-level `kind:` and a `metadata:` block whose
# `labels:` is a block map or a flow map. A CronJob document whose name
# or namespace this cannot read prints `?` there, and that is CANNOT
# ANSWER below — a declaration that could not be read is no smaller set.
declared_cronjobs() {
    local f
    for f in "$WORK"/render/*/*.yaml; do
        [ -f "$f" ] || continue
        awk -v file="${f##*/}" '
            function clean(s) {
                sub(/[ \t]+#.*$/, "", s)
                gsub(/^[ \t]+|[ \t]+$/, "", s)
                q = substr(s, 1, 1)
                if (length(s) >= 2 && (q == "\"" || q == sq) && substr(s, length(s), 1) == q)
                    s = substr(s, 2, length(s) - 2)
                return s
            }
            function reset() { kind = ""; name = ""; ns = ""; chore = ""; meta = 0; labels = 0 }
            function flush() {
                if (kind == "CronJob")
                    printf "%s\t%s\t%s\t%s\n", (ns == "" ? "?" : ns), (name == "" ? "?" : name), (chore == "" ? "-" : chore), file
                reset()
            }
            function flow(s,   n, i, parts, k, v) {
                sub(/^[ \t]*\{/, "", s); sub(/\}[ \t]*$/, "", s)
                n = split(s, parts, ",")
                for (i = 1; i <= n; i++) {
                    k = parts[i]; v = parts[i]
                    sub(/:.*/, "", k); sub(/^[^:]*:/, "", v)
                    if (clean(k) == "boss-chore") chore = clean(v)
                }
            }
            BEGIN { sq = sprintf("%c", 39); reset() }
            /^---/ { flush(); next }
            /^[ \t]*#/ || /^[ \t]*$/ { next }
            /^kind:/ { v = $0; sub(/^kind:/, "", v); kind = clean(v); meta = 0; labels = 0; next }
            /^metadata:[ \t]*$/ { meta = 1; labels = 0; next }
            /^[^ \t]/ { meta = 0; labels = 0; next }
            meta && /^  [^ \t]/ {
                labels = 0
                k = $0; v = $0
                sub(/^  /, "", k); sub(/:.*/, "", k)
                sub(/^  [^:]*:/, "", v)
                if (k == "name") name = clean(v)
                else if (k == "namespace") ns = clean(v)
                else if (k == "labels") { if (clean(v) ~ /^\{/) flow(clean(v)); else labels = 1 }
                next
            }
            meta && labels && /^    [^ \t]/ {
                k = $0; v = $0
                sub(/^ +/, "", k); sub(/:.*/, "", k)
                sub(/^ +[^:]*:/, "", v)
                if (clean(k) == "boss-chore") chore = clean(v)
                next
            }
            END { flush() }
        ' "$f"
    done
}
declared_cronjobs | sort -u > "$WORK/declared"
[ -s "$WORK/declared" ] \
    || unread "the render of this checkout's manifests holds no CronJob at all — that is no declared set, not an empty one"
blind="$(awk -F'\t' '$1 == "?" || $2 == "?" { print $4 }' "$WORK/declared" | sort -u | tr '\n' ' ')"
[ -z "$blind" ] \
    || unread "a CronJob document in the render has no readable name or namespace (in ${blind% }), so which chores the tree declares is not known"
ROW="$(awk -F'\t' -v n="$NS" -v c="$NAME" '$1 == n && $2 == c' "$WORK/declared")"
if [ -z "$ROW" ]; then
    here="$(awk -F'\t' -v n="$NS" '$1 == n && $3 == "true" { printf "%s%s", sep, $2; sep = ", " }' "$WORK/declared")"
    refuse "no CronJob $NAME in namespace $NS is declared by this checkout's manifests, as the converge renders them for every instance — only a chore the tree declares can be suspended (the chores declared in $NS: ${here:-none})"
fi
DECL_FILE="$(printf '%s\n' "$ROW" | cut -f4)"
[ "$(printf '%s\n' "$ROW" | cut -f3)" = true ] \
    || refuse "CronJob $NS/$NAME is declared in infra/cluster/manifests/$DECL_FILE without the label boss-chore=true — it is not a chore, and this verb suspends only chores"
for owned in $CONVERGE_OWNS_SPEC; do
    [ "$owned" != "$NAME" ] \
        || refuse "CronJob $NAME's whole spec is written by the converge on every train (infra/cluster/deposit-config-patch.jq replaces /spec), so a suspension would be put back to false by the next converge without a word — suspend it by changing what that converge writes, not with this verb"
done

# The kubectl image, present and pinned (a bounded pull, named when it
# fails), before any read.
if ! ready="$(ops_image_ready 2> "$WORK/door.err")"; then
    fail "the kubectl door cannot run: $(tr '\n' ' ' < "$WORK/door.err")"
fi
echo "$ME: kubectl image $ready" >&2
K="$(ops_kubectl)"

# read_cronjob <file> — the live CronJob into $WORK/<file>, refused unless
# it reads back as that CronJob with a boolean spec.suspend.
read_cronjob() {
    # shellcheck disable=SC2086 # K is one line the door prints to be word-split
    if ! $K get cronjob "$NAME" -n "$NS" -o json --request-timeout=20s \
            > "$WORK/$1" 2> "$WORK/$1.err"; then
        unread "cronjob $NS/$NAME: $(tr '\n' ' ' < "$WORK/$1.err")"
    fi
    jq_doc_file "$WORK/$1" \
        && jq -e --arg n "$NAME" --arg s "$NS" '.kind == "CronJob" and .metadata.name == $n
                and .metadata.namespace == $s and (.spec.suspend | type == "boolean")' \
            "$WORK/$1" > /dev/null 2>&1 \
        || unread "cronjob $NS/$NAME did not read back as that CronJob with a boolean spec.suspend (its first 200 bytes: '$(head -c 200 "$WORK/$1" | tr '\n' ' ')')"
}

read_cronjob cronjob.json
[ "$(jq -r '.metadata.labels["boss-chore"] // "absent"' "$WORK/cronjob.json")" = true ] \
    || refuse "the live CronJob $NS/$NAME does not carry the label boss-chore=true (it reads '$(jq -r '.metadata.labels["boss-chore"] // "absent"' "$WORK/cronjob.json")'), though infra/cluster/manifests/$DECL_FILE declares it — the cluster and the tree disagree about what this object is, so nothing is written"
CUR="$(jq -r '.spec.suspend' "$WORK/cronjob.json")"
SCHEDULE="$(jq -r '.spec.schedule // "unread"' "$WORK/cronjob.json")"
DEADLINE="$(jq -r 'if (.spec.startingDeadlineSeconds | type) == "number" then "\(.spec.startingDeadlineSeconds)s" else "unset" end' "$WORK/cronjob.json")"
POLICY="$(jq -r '.spec.concurrencyPolicy // "Allow"' "$WORK/cronjob.json")"

# live_state <file> — the two facts that move with every run: when the
# chore was last scheduled, and the Jobs it is running now.
live_state() {
    LAST="$(jq -r '.status.lastScheduleTime // "never"' "$WORK/$1")"
    ACTIVE_N="$(jq -r '(.status.active // []) | length' "$WORK/$1")"
    ACTIVE="$(jq -r '[(.status.active // [])[] | .name // "unnamed"] | if length == 0 then "none" else join(", ") end' "$WORK/$1")"
}
live_state cronjob.json

[ "$WANT" != "$CUR" ] \
    || refuse "CronJob $NS/$NAME's spec.suspend is already $WANT — a plan that changes nothing is not a plan, and a second run of an applied one is refused here"

# --- the plan ------------------------------------------------------------------
{
    echo "plan: suspend-a-chore"
    echo "cronjob: $NS/$NAME"
    echo "declared: infra/cluster/manifests/$DECL_FILE, label boss-chore=true; the live object carries boss-chore=true"
    echo "spec.suspend: $CUR -> $WANT"
    echo "schedule: $SCHEDULE"
    echo "startingDeadlineSeconds: $DEADLINE"
    echo "concurrencyPolicy: $POLICY"
    echo "command: kubectl patch cronjob $NAME -n $NS --type=json — test that /spec/suspend is still $CUR, replace it with $WANT; the one field this writes."
    echo "read-back: cronjob $NS/$NAME is read again, and its spec.suspend must read $WANT."
    if [ "$WANT" = true ]; then
        echo "note: suspending stops the NEXT runs only — a Job this CronJob has already started keeps running, and the write names the ones it finds when it patches. The converge does not undo this (no manifest declares spec.suspend, so its apply leaves the field alone), so $NAME stays suspended until this verb runs with false."
    else
        if [ "$DEADLINE" = unset ]; then
            echo "note: resuming with no startingDeadlineSeconds starts the most recent run missed while suspended AT ONCE (Kubernetes' documented behaviour), then runs on the schedule above."
        else
            echo "note: resuming starts the most recent run missed while suspended at once only if it fell within the last $DEADLINE (startingDeadlineSeconds), then runs on the schedule above."
        fi
    fi
    echo "not signed: when $NAME was last scheduled and the Jobs it is running now move with every run, so they ride the plan verb's stderr and the write prints them again on its record as it patches."
} > "$WORK/plan"
HASH="$(sha256sum "$WORK/plan" | cut -d' ' -f1)"
echo "$ME: not signed (it moves with every run): last scheduled $LAST; active Jobs ($ACTIVE_N): $ACTIVE" >&2

if [ "$MODE" = plan ]; then
    cat "$WORK/plan"
    echo "plan-sha256: $HASH" >&2
    exit 0
fi

# --- the write -----------------------------------------------------------------
if [ "$HASH" != "$APPROVED" ]; then
    cat "$WORK/plan" >&2
    refuse "today's plan for $NS/$NAME (above) hashes to $HASH, not the approved plan $APPROVED — its value, schedule or declaration moved since the plan was signed (or this plan already ran). Nothing was patched; render and approve it again"
fi
cat "$WORK/plan"
echo
echo "$ME: plan $APPROVED still holds — setting spec.suspend of cronjob $NS/$NAME from $CUR to $WANT"
echo "$ME: at the write: last scheduled $LAST; active Jobs ($ACTIVE_N): $ACTIVE"

PATCH="[{\"op\":\"test\",\"path\":\"/spec/suspend\",\"value\":$CUR},{\"op\":\"replace\",\"path\":\"/spec/suspend\",\"value\":$WANT}]"
# shellcheck disable=SC2086 # K is one line the door prints to be word-split
if ! $K patch cronjob "$NAME" -n "$NS" --type=json -p "$PATCH" --request-timeout=20s \
        > "$WORK/patch.out" 2> "$WORK/patch.err"; then
    sed 's/^/    /' "$WORK/patch.err" >&2
    fail "kubectl patch cronjob $NS/$NAME did not succeed (its words above) — this run cannot say spec.suspend changed; read it again with plan-a-chore-suspend before anything else"
fi
sed 's/^/    /' "$WORK/patch.out"
echo "$ME: the API server accepted spec.suspend=$WANT on cronjob $NS/$NAME — reading it back"

# --- the read-back --------------------------------------------------------------
# shellcheck disable=SC2086
if ! $K get cronjob "$NAME" -n "$NS" -o json --request-timeout=20s > "$WORK/back.json" 2> "$WORK/back.err" \
    || ! jq_doc_file "$WORK/back.json"; then
    fail "the patch was accepted, and cronjob $NS/$NAME could not be read back ($(tr '\n' ' ' < "$WORK/back.err" | cut -c1-200)), so spec.suspend=$WANT is NOT proven. The patch stands — read it again with plan-a-chore-suspend"
fi
BACK="$(jq -r 'if (.spec.suspend | type) == "boolean" then (.spec.suspend | tostring) else "unreported" end' "$WORK/back.json")"
live_state back.json
if [ "$BACK" = "$WANT" ]; then
    # The literal words are the verb file's `effect`, spelled here as it
    # reads them (a_mutating_verb_declares_its_effect.rs).
    echo "$ME: cronjob $NS/$NAME reads back spec.suspend $BACK (was $CUR; schedule $SCHEDULE; active Jobs at the read-back: $ACTIVE_N)"
    exit 0
fi
fail "the patch was accepted, and cronjob $NS/$NAME reads back spec.suspend $BACK, not $WANT — NOT proven. Read it again with plan-a-chore-suspend"
