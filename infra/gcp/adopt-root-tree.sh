#!/usr/bin/env bash
# Initial adoption is the approved one-time old-converge handover. Bootstrap
# bytes come from authenticated forge main, never from /opt/boss. Later copies
# come only from the accepted root generation. This installs no live unit.
set -euo pipefail
umask 077
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
PREP=${BOSS_GCP_PREPARED_DIR:-/var/lib/boss/gcp-root-prepared}
TREE=${BOSS_GCP_ROOT_TREE:-/var/lib/boss/gcp-tree}
HELPER=${BOSS_GCP_TRUSTED_CREDENTIAL_HELPER:-/usr/local/libexec/boss/root-tree-delivery/infra/gcp/root-tree-credential.sh}
refuse() { echo "gcp-root-adoption: REFUSED: $*; old units unchanged" >&2; exit 78; }
[ "${1:-}" = bootstrap ] && [ "$#" = 2 ] || refuse arguments
deposit_user=$2
[[ "$deposit_user" =~ ^[a-z_][a-z0-9_-]*$ ]] && [ "$deposit_user" != root ] || refuse deposit-account
uid=$(id -u)
# The published preparation's path guard is reproduced here before the first
# bootstrap exists. Refuse writable or linked ancestors before any fetch.
# kernel_path e|m <path> — where the KERNEL reaches <path>, one component
# at a time: each name is asked of the kernel (`[ -L ]` is lstat(2)) on a
# prefix already resolved physically, a symlink's target from readlink is
# put back in front of what remains, and `..` climbs from the resolved
# prefix, never from the spelling — never `realpath`, whose -e/-m is
# GNU's and which a host may ship as one that collapses `..` as text
# first (backlog 9b74c84b, after restore-shared's 920f4fd5). `e` refuses
# a missing component, or a non-directory with more path after it; `m`
# spells a missing name as written and keeps walking, so a link AFTER a
# cancelled missing prefix (`gone/../link`) is still followed, as GNU -m
# follows it — the first text folded the missing prefix and appended the
# rest unresolved (review 5e3eb6f3). More than 40 links is a refusal in
# both modes: no answer exists, and a spelling would pass a guard. One
# text in every root verb that carries it, held to GNU's answers by
# gcp_root_paths_resolve_through_the_kernel.rs.
kernel_path() {
    local mode=$1 rest=$2 r c t more links=0
    [ -n "$rest" ] || return 1
    case $rest in
        /*) r= ;;
        *) r=$(pwd -P) || return 1; r=${r%/} ;;
    esac
    while [ -n "$rest" ]; do
        case $rest in
            */*) c=${rest%%/*}; rest=${rest#*/}; more=1 ;;
            *) c=$rest; rest=; more= ;;
        esac
        case $c in
            '' | .) continue ;;
            ..) r=${r%/*}; continue ;;
        esac
        if [ -L "$r/$c" ]; then
            links=$((links + 1))
            [ "$links" -le 40 ] || return 1
            t=$(readlink -- "$r/$c" && printf .) || return 1
            t=${t%.}; t=${t%$'\n'}
            [ -n "$t" ] || return 1
            case $t in /*) r= ;; esac
            rest=$t${more:+/$rest}
        elif [ "$mode" = m ]; then
            r=$r/$c
        elif [ -e "$r/$c" ] && { [ -z "$more" ] || [ -d "$r/$c" ]; }; then
            r=$r/$c
        else
            return 1
        fi
    done
    printf '%s\n' "${r:-/}"
}
safe() {
    local path=$1 owner bits child=
    [[ "$path" = /* ]] && [ "$(kernel_path m "$path")" = "$path" ] || refuse ambiguous-path
    while :; do
        [ ! -L "$path" ] || refuse linked-path
        if [ -e "$path" ]; then
            owner=$(stat -c %u "$path"); bits=$(stat -c %a "$path")
            [ "$owner" = 0 ] || [ "$owner" = "$uid" ] || refuse foreign-owner
            if [ "$((8#$bits & 0022))" != 0 ]; then
                [ "$owner" = 0 ] && [ "$((8#$bits & 01000))" != 0 ] && [ -n "$child" ] && [ -d "$child" ] || refuse writable-path
            fi
        fi
        [ "$path" != / ] || break
        child=$path; path=$(dirname -- "$path")
    done
}
safe "$PREP"
safe "$TREE"
. "$HERE/../lib/host-check.sh"
host_seam BOSS_GCP_PREPARED_DIR /var/lib/boss/gcp-root-prepared
host_seam BOSS_GCP_ROOT_TREE /var/lib/boss/gcp-tree
host_check boss-gcp gcp-root-adoption
. "$HERE/../lib/sor.sh"
. "$HERE/../lib/sor-reader.sh"
. "$HERE/../lib/secret-header.sh"
. "$HERE/../lib/jq.sh"
sor_require BOSS_FORGE_URL
sor_require BOSS_JOBS_URL
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
if [ -e "$PREP/bootstrap" ] || [ -L "$PREP/bootstrap" ]; then
    safe "$PREP/bootstrap"
    helper="$PREP/bootstrap/infra/gcp/root-tree.sh"
    safe "$helper"
    accepted=$(BOSS_GCP_ROOT_TREE="$TREE" bash "$helper" path) || refuse accepted-generation
    source=${BOSS_GCP_PREP_SOURCE:-$accepted/infra}
    [ "$source" = "$accepted/infra" ] || refuse generation-substitution
    safe "$PREP/bootstrap/deposit-user"
    old_user=$(cat "$PREP/bootstrap/deposit-user")
    [ "$old_user" = "$deposit_user" ] || refuse deposit-account-changed
else
    # First adoption, not rotation: the two human lifecycles must really have
    # ended. A bounded complete query and exact subject refuse stale/partial
    # answers. An unavailable record stops bootstrap, not an accepted root tree.
    machine_token_header MT "$BOSS_JOBS_URL" || refuse record-header
    reader=$(sor_reader_header automation:boss-gcp-converge)
    for pair in prepare-the-root-tree-deposit-key:root-tree-deposit-key rotate-a-credential:boss-gcp-root-tree-token; do
        kind=${pair%%:*}; subject=${pair#*:}
        curl -q --noproxy '*' -fsS --max-time 15 --max-redirs 0 --proto '=http,https' \
            ${MT:+-H "$MT"} -H "x-boss-user: $reader" \
            "$BOSS_JOBS_URL/api/jobs?kind=$kind&subject_id=$subject&limit=100&full=true" > "$work/lifecycle" \
            || refuse lifecycle-read
        [ "$(stat -c %s "$work/lifecycle")" -le 1048576 ] || refuse oversized-lifecycle
        jq_doc_file "$work/lifecycle" || refuse unfinished-human-lifecycle
        jq -e --arg kind "$kind" --arg subject "$subject" '
          .total > 0 and .total == (.data|length) and
          ([.data[] | select(.kind != $kind or .subject.id != $subject)] | length) == 0 and
          (.data | sort_by(.opened_at) | last | .status == "closed" and
            ([.steps[] | select(.spec_slug == "scope" or .spec_slug == "verify" or .spec_slug == "revoke")]
              | length == 3 and all(.status == "completed" and (.completed_by|type == "string"))))
        ' "$work/lifecycle" >/dev/null || refuse unfinished-human-lifecycle
    done
    # The helper already installed by the receiver is root's. No token value
    # is copied by an operator, placed in argv, or used through a user helper.
    safe "$HELPER"
    [ -f "$HELPER" ] && [ "$(stat -c %u "$HELPER")" = "$uid" ] || refuse credential-helper
    [[ "$BOSS_FORGE_URL" =~ ^https?://[a-zA-Z0-9.-]+(:[0-9]+)?$ ]] || refuse forge-origin
    scheme=${BOSS_FORGE_URL%%:*}; authority=${BOSS_FORGE_URL#*://}
    auth=$(printf 'protocol=%s\nhost=%s\npath=david/boss.git\n\n' "$scheme" "$authority" | bash "$HELPER" get) || refuse scoped-credential
    value=$(printf '%s\n' "$auth" | sed -n 's/^password=//p')
    [[ "$value" =~ ^[a-zA-Z0-9_-]+$ ]] || refuse scoped-credential
    secret_header FH "Authorization: token $value" || refuse forge-header
    unset auth value
    code=$(curl -q --noproxy '*' -sS --max-time 30 --max-redirs 0 --proto '=http,https' \
        -H "$FH" -o "$work/repository" -w '%{http_code}' "$BOSS_FORGE_URL/api/v1/repos/david/boss") || refuse forge-authentication
    [ "$code" = 200 ] && jq_doc_file "$work/repository" \
        && jq -e '.full_name == "david/boss"' "$work/repository" >/dev/null || refuse forge-identity
    clean_git() {
        env -i PATH=/usr/bin:/bin LC_ALL=C HOME=/nonexistent \
            GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 \
            BOSS_FORGE_URL="$BOSS_FORGE_URL" BOSS_SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}" \
            BOSS_GCP_ROOT_TREE_TOKEN="${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}" \
            git -c core.hooksPath=/dev/null "$@"
    }
    clean_git init --bare "$work/repo.git"
    # The bounded child gets the same empty Git environment as the init.
    timeout -k 5 300 env -i PATH=/usr/bin:/bin LC_ALL=C HOME=/nonexistent \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0 \
        BOSS_FORGE_URL="$BOSS_FORGE_URL" BOSS_SOR_ENV="${BOSS_SOR_ENV:-/etc/boss/sor.env}" \
        BOSS_GCP_ROOT_TREE_TOKEN="${BOSS_GCP_ROOT_TREE_TOKEN:-/etc/boss/root-tree/forge.token}" \
        git -c core.hooksPath=/dev/null -c credential.helper= -c "credential.helper=$HELPER" \
        -c credential.useHttpPath=true -c http.followRedirects=false -c fetch.fsckObjects=true -c transfer.fsckObjects=true \
        --git-dir="$work/repo.git" fetch --no-tags -- "$BOSS_FORGE_URL/david/boss.git" refs/heads/main || refuse root-fetch
    sha=$(clean_git --git-dir="$work/repo.git" rev-parse --verify 'FETCH_HEAD^{commit}')
    [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || refuse fetched-commit
    install -d -m 0700 "$work/source"
    clean_git --git-dir="$work/repo.git" archive "$sha" infra/gcp infra/lib infra/estate/estate.toml > "$work/source.tar"
    tar -xf "$work/source.tar" -C "$work/source"
    # Git archives do not carry directory ownership/mode policy. Normalize
    # only these newly fetched private bytes; never follow archived links.
    find "$work/source" -type d -exec chmod 0700 {} +
    find "$work/source" -type f -exec chmod go-w {} +
    source="$work/source/infra"
fi
BOSS_GCP_PREP_SOURCE="$source" BOSS_GCP_PREPARED_DIR="$PREP" BOSS_GCP_ROOT_TREE="$TREE" \
    bash "$source/gcp/prepare-root-tree.sh" bootstrap || refuse bootstrap-preparation
printf '%s\n' "$deposit_user" > "$PREP/bootstrap/deposit-user"
chmod 0600 "$PREP/bootstrap/deposit-user"
echo 'gcp-root-adoption: protected bootstrap prepared; no live units changed'
