#!/usr/bin/env bash
# publish-tenant — publish an instance's tenant main into its live
# registries, under a passkey-signed plan.
#
#   publish-tenant.sh --plan <instance> <take>
#   publish-tenant.sh <instance> <take> <plan-sha256>
#
# <take> is `none` or the comma list `boss tenant publish --take`
# accepts (classes,departments,…); the CLI's own parser judges the names
# (tenant_publish.rs TAKEABLE), so no list of them is typed here.
#
# WHY IT EXISTS (backlog 1610115d, folding the publish half of
# 2f6eb50d). A tenant reaches the live registries at a services start
# only ONCE per database (seed-tenant.sh reads the tenant_publishes
# stamp), and every door is insert-if-absent: the instance is the truth
# (design e187198f), and `--take <registry>` is the one overwrite. So a
# change that has landed on tenant main reaches production only through
# a `boss tenant publish` someone runs — and on 2026-09-25 (departments,
# 7edf0e97) and 2026-09-26 (classes, 0b032f13) David ran
# `boss tenant publish --take …` by hand against prod, with a dry-run
# and the operator between the file and the registry: no packet, no
# approval, no record of what the live rows were before. David,
# 2026-10-18: manually running any ops command is a BOSS coverage gap.
# This is design 17835005's shape (a rendered plan hash, signed with his
# passkey, single-use, verified before the argv is built), the
# merge-tenant-main / expand-dev-work-volume pair's shape, applied to
# the publish that follows a tenant merge.
#
# --plan RENDERS AND DOES NOT ACT. The plan names the tenant repo, the
# main sha and tree it publishes, the take, the `boss tenant check`
# verdict on that tree, the publish's own `--dry-run` (every write it
# would make), the sha256 of the CLI build that rendered it, and — the
# part a hand dry-run never had — THE LIVE REGISTRIES AS THEY STAND:
# `boss tenant export` through the machine door into a copy of the tree,
# compared with the tree ROW BY ROW (tenant-rows.py beside this script).
# Per file: the declared rows the same live, differing, or not live; a
# count of the live rows the tree does not declare (no publish writes
# them); the file's live digest. For a taken registry each row the take
# overwrites is printed field by field — a `-` line is what the tenant
# repo says, a `+` line what the instance holds now and the take
# overwrites. A file the export has no read of is named NOT READ, and a
# take of one is refused (exit 78). A tree the check does not PASS, or a
# take the CLI refuses, is exit 78 — not a plan. A live read that fails
# is exit 1: a plan without the live state would be the dry-run this
# verb exists to replace. The bytes carry only each CLI's stdout, no
# clock, no run id and no scratch path (each CLI runs from inside its
# directory with `.`), so two renders of one true state are
# byte-identical; the sha256 rides stderr as `plan-sha256:`.
#
# THE WRITE RE-RENDERS AND COMPARES, then publishes. Every observed fact
# — tenant main's sha, the tree, the CLI build, the check, the dry-run,
# and the live registries' rows and digests — is in the bytes, so a main
# that moved or a live row someone edited since the approval is a
# different hash and publishes nothing: execute compares the live
# registry to the planned one before it writes (the item's first
# acceptance line). The publish is the CLI's own (`boss tenant publish .
# --door <SoR> [--take …]`, signed as the seed identity, idempotent,
# refusing a directory that fails check). Then it READS BACK: a second
# export through the door into a fresh copy of the tree, compared again.
# The effect line names the declared rows that converged on the tree,
# those that still differ (a registry not taken keeps its live rows, by
# decision) and how many of those are in a taken registry, which the
# take did not converge; a row that was the tree's before the publish
# and is not after is FAILED — the publish moved the instance away from
# the tree it published, or something else wrote during it.
#
# THE BOUND IS BY CONSTRUCTION. The repo and ref are the instance's own
# `tenant_repo` / `tenant_ref` from infra/cluster/instances.toml, never
# a parameter, read from the forge's own repository on disk as its
# owner (cluster-deploy-lib.sh tenant_checkout — the converge's read,
# which needs no credential). The door is BOSS_JOBS_URL from
# /etc/boss/sor.env (infra/lib/sor.sh, no fallback), and it reaches ONE
# instance — the system of record's, whose namespace
# infra/estate/estate.toml spells in `sor_cluster_url` — so an instance
# in any other namespace is refused rather than published into the
# wrong registry.
#
# NOT THIS VERB, named so nobody mistakes it for done: the export of the
# instance BACK into the tenant repo as a branch (and its landing
# through merge-tenant-main), which closes the other direction of the
# drift found 2026-10-17 on seeds/agents.toml. This verb's read-back is
# that export, run into a scratch copy; committing it is the next car.
#
# Exit 78 is a refusal (the request was wrong or is no longer true);
# exit 1 is a step that genuinely failed.
set -uo pipefail

ME=publish-tenant
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLI="${BOSS_CLI:-boss}"
export GIT_TERMINAL_PROMPT=0

refuse() { echo "$ME: REFUSED — $*" >&2; exit 78; }
fail() { echo "$ME: FAILED — $*" >&2; exit 1; }

# shellcheck source=cluster-deploy-lib.sh
. "$HERE/cluster-deploy-lib.sh"
# shellcheck source=infra/lib/sor.sh
. "$HERE/../lib/sor.sh"

PLAN=0
if [ "${1-}" = "--plan" ]; then PLAN=1; shift; fi
if [ "$PLAN" -eq 1 ]; then
    [ $# -eq 2 ] || refuse "usage: $ME --plan <instance> <take>"
else
    [ $# -eq 3 ] || refuse "usage: $ME <instance> <take> <plan-sha256>"
    [[ "$3" =~ ^[0-9a-f]{64}$ ]] || refuse "the plan hash must be 64 hex characters"
fi
INSTANCE="$1"
TAKE="$2"
[[ "$INSTANCE" =~ ^[a-z][a-z0-9-]{0,30}$ ]] || refuse "'$INSTANCE' is not an instance name"
[[ "$TAKE" =~ ^[a-z]+(,[a-z]+){0,15}$ ]] \
    || refuse "'$TAKE' is not a take: \`none\`, or registry names joined by commas"
TAKE_ARGS=()
[ "$TAKE" = none ] || TAKE_ARGS=(--take "$TAKE")

sor_require BOSS_JOBS_URL
DOOR="$BOSS_JOBS_URL"

# --- the instance, and the one the door reaches -------------------------
rows=$(bash "$HERE/../cluster/render-instance.sh" --instances) \
    || fail "render-instance.sh --instances could not read infra/cluster/instances.toml"
row=$(awk -F'\t' -v n="$INSTANCE" '$1 == n' <<< "$rows")
[ -n "$row" ] || refuse "no instance [$INSTANCE] in infra/cluster/instances.toml"
NAMESPACE=$(cut -f2 <<< "$row")
TENANT_REPO=$(cut -f7 <<< "$row")
REF=$(cut -f8 <<< "$row")
[ -n "$TENANT_REPO" ] && [ -n "$REF" ] \
    || refuse "instance [$INSTANCE] is image-sourced — it has no tenant_repo, so there is no tenant main to publish"
ESTATE="$HERE/../estate/estate.toml"
sor_ns=$(sed -n -E 's#^sor_cluster_url = "https?://[^./]+\.([a-z0-9-]+)\.svc[^"]*"$#\1#p' "$ESTATE") \
    || fail "cannot read $ESTATE"
[ -n "$sor_ns" ] \
    || fail "$ESTATE spells no sor_cluster_url this script can read a namespace from — which instance the door reaches is unknown, and a guess is the wrong-target class"
[ "$NAMESPACE" = "$sor_ns" ] \
    || refuse "instance [$INSTANCE] lives in namespace $NAMESPACE, and the machine door ($DOOR) reaches the system of record's instance in $sor_ns — publishing would write into the wrong registries"

# --- the tools the plan is rendered with -----------------------------------
# The CLI build is in the plan's bytes (review c757cf5d F5): a converge
# between the signature and the execute that changes what
# `boss tenant publish` does is then a different plan, not the same hash.
CLI_PATH=$(command -v "$CLI") || fail "the boss CLI ($CLI) is not on PATH"
CLI_SHA=$(sha256sum "$CLI_PATH" | cut -d' ' -f1)
[[ "$CLI_SHA" =~ ^[0-9a-f]{64}$ ]] || fail "cannot hash the boss CLI at $CLI_PATH"
ROWS="$HERE/tenant-rows.py"
python3 -c 'import tomllib' 2>/dev/null \
    || fail "python3 has no tomllib (3.11+) — the tree and the live registries cannot be compared row by row, so no plan"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# --- tenant main, read from the forge's own repository -------------------
tenant_checkout "$TENANT_REPO" "$REF" "$WORK/src"
case $? in
    0) : ;;
    2) refuse "instance [$INSTANCE] names tenant_repo '$TENANT_REPO', which is not an owner/name" ;;
    *) fail "tenant $TENANT_REPO@$REF could not be read from the forge (above)" ;;
esac
MAIN=$(git -C "$WORK/src" rev-parse HEAD) || fail "cannot read the sha of $TENANT_REPO@$REF"
TREE=$(git -C "$WORK/src" rev-parse 'HEAD^{tree}') || fail "cannot read the tree of $TENANT_REPO@$MAIN"
# The tree as plain files: the publish and the export read a directory,
# and a .git beside the seeds would be diffed as if it were one.
mkdir "$WORK/tree"
git -C "$WORK/src" archive "$MAIN" | tar -x -C "$WORK/tree" \
    || fail "cannot unpack $TENANT_REPO@$MAIN"

# --- the check, and the publish's own dry-run ----------------------------
# ONLY A CLI's STDOUT IS PLAN BYTES. Its stderr is the tracing log, and
# the real CLI writes a timestamped INFO line there on every call that
# builds a machine client ("machine token appeared", boss-cli main.rs's
# subscriber, machine_token.rs) — nothing on the forge sets RUST_LOG.
# The first cut captured 2>&1, so no two renders of one state hashed the
# same and every approved publish was refused at the runner's re-render
# (review c757cf5d F1: two read-only prod exports wrote identical files
# and differed in that one line). Each stderr goes to a file under $WORK
# and is printed only when its call fails — kept, never in the hash.
check_rc=0
check=$(cd "$WORK/tree" && "$CLI" tenant check . 2> "$WORK/check.err") || check_rc=$?
case "$check_rc" in
    0) : ;;
    1) printf '%s\n' "$check" >&2; cat "$WORK/check.err" >&2
       refuse "boss tenant check does not PASS on $TENANT_REPO@$MAIN — a tree that does not pass is a refusal, not a plan" ;;
    *) printf '%s\n' "$check" >&2; cat "$WORK/check.err" >&2
       fail "boss tenant check exited $check_rc without a verdict — the tree was NOT checked" ;;
esac
dry_rc=0
dry=$(cd "$WORK/tree" && "$CLI" tenant publish . --door "$DOOR" --dry-run "${TAKE_ARGS[@]}" 2> "$WORK/dry.err") || dry_rc=$?
if [ "$dry_rc" -ne 0 ]; then
    printf '%s\n' "$dry" >&2; cat "$WORK/dry.err" >&2
    refuse "boss tenant publish --dry-run refused (exit $dry_rc) — the take or the tree is not one the publish would carry"
fi

# --- the files a take overwrites, named by the dry-run --------------------
# Each line of the dry-run names a file and, where one exists, the
# `--take <registry>` that overwrites it (tenant_publish.rs), so which
# file a taken registry is read from the CLI rather than typed here a
# second time. A taken registry the dry-run names no file for is refused:
# the plan could not show what it overwrites.
TAKEN_PAIRS=""
if [ "$TAKE" != none ]; then
    for reg in ${TAKE//,/ }; do
        files=$(grep -E -- "--take ${reg}([^a-z]|\$)" <<< "$dry" | awk '{print $1}' | sort -u)
        [ -n "$files" ] \
            || refuse "the dry-run names no file that --take $reg overwrites — the plan cannot show the live rows such a take would overwrite"
        while read -r f; do TAKEN_PAIRS+="$reg"$'\t'"$f"$'\n'; done <<< "$files"
    done
fi
TAKEN=$(cut -f2 <<< "$TAKEN_PAIRS" | sed '/^$/d' | sort -u | paste -sd, -)

# live_read DIR — export the live registries through the door into DIR
# (a fresh copy of the tree). Prints the export's report; sets READ to the
# files the export named (comma-joined) and LIVE_STATUS to every declared
# row that is not the same live (tenant-rows.py status). Exit 1 when the
# instance could not be read, or when the export named no file — never an
# empty comparison that reads as "the instance agrees".
#
# ROW BY ROW, NOT LINE BY LINE (review c757cf5d F2). The live side is the
# export's canonical rendering and the tree is hand-written, so the first
# cut's `git diff` of the two differed in all 14 files of the Algedonic
# tenant on comments and ordering alone — 210,105 bytes, over the
# runner's OPS_OUTPUT_CAP (102,400), which refuses to render such a plan.
# tenant-rows.py brings both sides to one normal form and matches rows by
# identity; its header says what it normalises and why.
#
# WHAT THE EXPORT READ (F3). The export's report names one line per file
# it read; a tenant file in none of them (seeds/departments.toml: the
# export has no departments read) has no live side here at all, and the
# report says NOT READ rather than comparing the tree with itself.
live_read() {
    local dir="$1" name out rc=0
    name=$(basename "$dir")
    cp -a "$WORK/tree" "$dir" || fail "cannot copy the tree for the live read"
    out=$(cd "$dir" && "$CLI" tenant export . --door "$DOOR" 2> "$WORK/$name.err") || rc=$?
    if [ "$rc" -ne 0 ]; then
        printf '%s\n' "$out" >&2; cat "$WORK/$name.err" >&2
        fail "boss tenant export through $DOOR exited $rc — the live registries were NOT read, and a plan without them is not a plan"
    fi
    printf '%s\n' "$out"
    READ=$(sed -n -E 's/^  (added|changed|unchanged|skipped) +([^ ]+) +[0-9]+ row\(s\)$/\2/p' <<< "$out" | sort -u | paste -sd, -)
    [ -n "$READ" ] \
        || fail "boss tenant export named no file it read (above) — which registries the live side holds is unknown, and a plan that guessed would be signed as if it knew"
    LIVE_STATUS=$(python3 -I "$ROWS" status "$WORK/tree" "$dir" "$READ") \
        || fail "tenant-rows.py could not compare the tree with the live export (above)"
}

# compare DIR — the row report of the tree against the export in DIR.
compare() {
    python3 -I "$ROWS" report "$WORK/tree" "$1" "$READ" "$TAKEN" \
        || fail "tenant-rows.py could not compare the tree with the live export (above)"
}

render() {
    echo "plan: publish-tenant"
    echo "instance: $INSTANCE (namespace $NAMESPACE)"
    echo "tenant repo: $TENANT_REPO"
    echo "publishes: refs/heads/$REF at $MAIN"
    echo "tree: $TREE"
    echo "door: $DOOR"
    echo "cli: boss, sha256 $CLI_SHA"
    if [ "$TAKE" = none ]; then
        echo "take: none — every door is insert-if-absent; a live row the tree differs from is kept and named"
    else
        echo "take: $TAKE — the live rows of these registries are overwritten with the tree's ($(awk -F'\t' 'NF == 2 { printf "%s%s: %s", (n++ ? ", " : ""), $1, $2 }' <<< "$TAKEN_PAIRS"))"
    fi
    echo
    echo "== boss tenant check on the tree =="
    printf '%s\n' "$check"
    echo
    echo "== every write the publish makes (boss tenant publish --dry-run) =="
    printf '%s\n' "$dry"
    echo
    echo "== the live registries against the tree, row by row (boss tenant export through the door) =="
    live_read "$WORK/live"
    # A take of a registry the export does not read would overwrite live
    # rows this plan neither shows nor guards (F3: departments, the
    # 2026-09-25 hand publish). Refused until the export reads it.
    while IFS=$'\t' read -r reg f; do
        [ -n "$f" ] || continue
        case ",$READ," in
            *,"$f",*) : ;;
            *) refuse "--take $reg overwrites $f, which boss tenant export does not read — the plan could neither show nor guard the live rows it would overwrite. Nothing was rendered; take it once the export reads it, or publish without taking it" ;;
        esac
    done <<< "$TAKEN_PAIRS"
    echo
    compare "$WORK/live"
    echo
    echo "== the write this plan authorises =="
    echo "boss tenant publish . --door $DOOR${TAKE_ARGS[*]:+ ${TAKE_ARGS[*]}}   (from $TENANT_REPO@$MAIN, signed as the seed identity)"
}

render > "$WORK/plan" || exit $?
# render ran in this shell (a redirected function is not a subshell), so
# LIVE_STATUS is the set of rows the plan's bytes show not the same live.
BEFORE="$LIVE_STATUS"
HASH=$(sha256sum "$WORK/plan" | cut -d' ' -f1) || fail "cannot hash the plan"

if [ "$PLAN" -eq 1 ]; then cat "$WORK/plan"; echo "plan-sha256: $HASH" >&2; exit 0; fi

# --- the write -----------------------------------------------------------
[ "$HASH" = "$3" ] || {
    cat "$WORK/plan" >&2
    refuse "the plan now hashes to $HASH, not the approved $3 — tenant main, the check or the live registries have moved since it was rendered (above: the plan as it stands). Nothing was published; render and approve it again"
}
echo "$ME: plan $HASH still holds — publishing $TENANT_REPO@$MAIN into $INSTANCE (take $TAKE)"
pub_rc=0
pub=$(cd "$WORK/tree" && "$CLI" tenant publish . --door "$DOOR" "${TAKE_ARGS[@]}" 2>&1) || pub_rc=$?
printf '%s\n' "$pub"
[ "$pub_rc" -eq 0 ] \
    || fail "boss tenant publish exited $pub_rc — doors before the failing one may have written (above); read the instance again with plan-a-tenant-publish before anything else"

# --- the read-back ---------------------------------------------------------
# Row by row, as the plan was. A declared row the same live before and not
# after is FAILED: the publish moved the instance away from the tree it
# published, or something else wrote during it. A row that was absent and
# is now present but differs (a door's default the tree does not state)
# is not a move. A taken row still not the tree's is named apart (review
# c757cf5d F4): it is what tells a take that landed from one that did not.
echo
echo "== read back: the live registries against the tree, after the publish =="
live_read "$WORK/after"
echo
compare "$WORK/after"
AFTER="$LIVE_STATUS"
rows() { cut -f1,2 | tr '\t' ' ' | sed '/^$/d' | sort -u; }
joined() { sed '/^$/d' | paste -sd';' - | sed 's/;/; /g'; }
moved=$(comm -13 <(rows <<< "$BEFORE") <(rows <<< "$AFTER"))
[ -z "$moved" ] || fail "after the publish these declared row(s) are not the tree's and were before: $(joined <<< "$moved") — the instance moved away from what was published"
converged=$(comm -23 <(rows <<< "$BEFORE") <(rows <<< "$AFTER"))
still=$(rows <<< "$AFTER")
taken_still=$(while read -r f r; do
    [ -n "$f" ] || continue
    case ",$TAKEN," in *,"$f",*) echo "$f $r" ;; esac
done <<< "$still")
count() { sed '/^$/d' | wc -l; }
n_conv=$(count <<< "$converged")
n_still=$(count <<< "$still")
n_ts=$(count <<< "$taken_still")
echo "$ME: $TENANT_REPO@$MAIN published into $INSTANCE (take $TAKE) — read back through the door: $n_conv declared row(s) converged on the tree${converged:+ ($(joined <<< "$converged"))}, $n_still still differ${still:+ ($(joined <<< "$still"))}; $n_ts of them in a taken registry${taken_still:+, which the take did not converge ($(joined <<< "$taken_still"))}"
