#!/usr/bin/env bash
#
# forge-account — make each non-admin forge account the tree declares
# (infra/forge/forge-accounts.json), grant it its ONE repository, and
# verify by effect that it is not a site admin and reaches nothing else.
#
#   forge-account.sh
#
# Run as root by forge-converge.sh on every tick, after protect-main and
# with the same forge header. Idempotent: an account already as declared
# is read and not written.
#
# WHY IT EXISTS (design 0af333ee, resolved by David 2026-11-13; backlog
# 70555ffd)
# ---------------------------------------------------------------------
# Every forge token the estate minted was a token of user david, and
# david is a Forgejo site admin: the registry's issuer root names its
# principal "user david (admin)", proved by every mint through
# /api/v1/admin/users/{user}/tokens, a route Forgejo serves only to a site
# admin. Forgejo does not narrow a site admin's token to the repositories
# it collaborates on, so david's read:repository token reads every
# repository on the instance and his write:repository token writes every
# one. read:repository is already the narrowest scope a Forgejo token has.
# Scope cannot narrow it further; only the principal can — and for a
# hosting business that will keep other people's repositories on this
# forge, that is the exposure to close.
#
# David's answers on the design:
#   accounts     two non-admin accounts. boss-reader, a READ collaborator
#                on david/boss only, holds the root-tree token. boss-writer,
#                a WRITE collaborator on david/boss only and never a site
#                admin, will hold the dev pod's, the conductor's and the
#                forge host checkout's. The registry push token
#                (write:package) stays on david.
#   create       by this converge, as root on the forge host, the way it
#                already makes and verifies boss-probe (probe-account.sh):
#                `forgejo admin user create` when absent — no password
#                login, must-change-password off, not admin — the
#                collaborator grant, and a line on the converge packet
#                stating is_admin false and the one repository grant. RED
#                if the account is an admin or holds any other repository.
#                No credential is placed by hand: a token for the account
#                is minted only by credential.rotate.forgejo with
#                forge_user set to it.
# This file is the `create` answer for every account the declaration
# lists. The first car declares boss-reader; boss-writer is a later car's
# row, and the writers move one rotation at a time after it.
#
# THE TWO HALVES, AND WHAT EACH IS ASKED THROUGH
# ----------------------------------------------
# THE ACCOUNT — inside the forge's container, as its `git` user:
#   docker --host unix:///var/run/docker.sock exec -u git forgejo \
#       forgejo admin user list | create …
# The system daemon, pinned on the argv, for forge-log.sh's reason: the
# converge builds on david's rootless daemon, and an inherited DOCKER_HOST
# would answer "no such container" about a container that is running.
# `list` is read by its HEADER (Username, IsAdmin), not by column number,
# and the line found is the only thing printed: the listing carries every
# user's address. `create` gets `--random-password` and its standard
# output goes to /dev/null, so the password Forgejo generates — and prints
# there — reaches no log, no packet and nobody: no password login means
# no one ever holds one. The same exec, as the same user, lists the
# forge's repository directories (`find /data/git/repositories …`) — the
# inventory NOTHING ELSE below is held to. Forgejo's own `prohibit_login` is NOT set,
# because Forgejo's API refuses a token whose user is prohibited from
# signing in (its API auth middleware answers 403 "This account is
# prohibited from signing in"), and a token is the account's only use.
# `--must-change-password=false`, because there is no one to change it.
# Never `--admin`. Then the listing is read again: an answer is not an
# effect.
#
# THE GRANT — the repository API, with the converge's forge header (the
# checkout's own credential, a token of the repository's owner; the same
# file protect-main reads, which never reaches an argv or this output):
#   PUT  /repos/{repo}/collaborators/{account}   {"permission": <declared>}
#   GET  /repos/{repo}/collaborators/{account}/permission
# written only when the read differs, and read back after.
#
# NOTHING ELSE — every repository the forge lists (/repos/search, paged to
# a short page) is asked what the account may do there. `none`
# passes. `read` passes only on a PUBLIC repository and only when the
# account is not a collaborator on it: a public repository answers `read`
# to every account, which is not a grant, and a collaboration on one is,
# so it is asked separately. Anything else — a write, an ownership, a
# team's reach through an organisation (the permission read is the
# EFFECTIVE one, teams included) — is RED, named. The declared repository
# must be IN the listing: a listing that cannot see it cannot see private
# repositories, and the absence of another grant in it proves nothing, so
# that is a cannot-answer, not a pass.
#
# Seeing it is not enough, though (review eb947a26 of car 0a95d470, B1).
# The listing is the CALLER's view, and nothing here authenticates the
# header's principal or its standing as a site admin — a token of
# repository scope cannot even ask who it is (/user wants read:user). A
# caller that administers david/boss and cannot see another private
# repository lists david/boss alone, and that partial view used to be
# recorded as "verified … no other reachable" while a write grant on the
# hidden repository stood. So the listing is held to the forge's OWN
# inventory — its repository directories, read inside the container, which
# do not depend on who asks — and "nothing else" is said only when the
# listing showed every repository the inventory holds. One it did not show
# is counted (never named: it is what the credential could not see) and is
# a cannot-answer. A RED found in what WAS seen stands either way.
#
# RED IS NAMED, NEVER REPAIRED BY GUESS
# -------------------------------------
# A site-admin account, or one holding another repository, is not
# demoted or stripped here: either is a change someone made by hand, and
# undoing it blind could undo something a person meant. It reds the
# converge every tick and leaves `forge_account_red` on the packet in the
# judge's shape (`RED forge-converge/forge-account refused: …`), which
# forge-converge.sh copies into `output`, so it is filed once as an item
# (infra/dispatcher/rules/file-backlog-items-on-forge-converge-red.toml).
# A wider grant on the declared repository IS put back, because the
# declaration says exactly what it should be.
#
# WHAT THIS DOES NOT DO
# ---------------------
#   * It mints no token and places none. The broker mints for the account
#     once a rule names it (broker-rotates-the-boss-gcp-root-tree-token).
#   * It does not mark the account `restricted` (no public-repository
#     reach) or a bot: neither is in the decision, and neither can be read
#     back without an admin-scoped credential this host does not hold.
#   * The grant needs the header's principal to administer the repository
#     — today david, its owner. When the checkout token moves to
#     boss-writer, which can write and cannot administer, this needs a
#     principal of its own for the grant (the item records it).
#
# EXIT
#   0  every declared account is present, not an admin, holds its one
#      grant as declared and nothing else — verified, or made so and then
#      verified
#   1  RED or FAILED: an account is an admin, holds another repository,
#      or could not be made or granted as declared — named, per account
#   2  the declaration is unreadable or declares what this must not make
#   4  cannot answer: docker, curl or the credential could not be had,
#      the forge's own repository inventory could not be read or holds no
#      declared repository, or the listing does not show every repository
#      that inventory holds — NOTHING was written on a guess
#
# ENV
#   BOSS_FORGE_URL               Forgejo's base (from /etc/boss/sor.env)
#   BOSS_FORGE_AUTH_HEADER_FILE  the header file (required for the grant)
#   BOSS_FORGE_ACCOUNTS          the declaration, default beside this file
#   BOSS_FORGE_ACCOUNT_DOCKER    the docker command words — the test seam
#   BOSS_FORGE_ACCOUNT_CURL      the curl command — the test seam
#   BOSS_FORGE_CONTAINER         the container, default forgejo
#   BOSS_RUN_SUMMARY_FILE        forge_accounts (and forge_account_red)
#                                land on the packet
# Tested against a stub docker and curl in
# crates/core/boss-testing/tests/forge_account_sh.rs.

set -uo pipefail
export LC_ALL=C

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=infra/forge/forge-defaults.sh
. "$HERE/forge-defaults.sh"
# shellcheck source=infra/run-summary.sh
. "$HERE/../run-summary.sh"

ME="forge-account"
DECL="${BOSS_FORGE_ACCOUNTS:-$HERE/forge-accounts.json}"
CURL="${BOSS_FORGE_ACCOUNT_CURL:-curl}"
read -r -a DOCKER <<<"${BOSS_FORGE_ACCOUNT_DOCKER:-docker}"
SYSTEM_DAEMON="unix:///var/run/docker.sock"
CONTAINER="${BOSS_FORGE_CONTAINER:-forgejo}"
PAGE=50
MAX_PAGES=40 # 2000 repositories; past that this says so rather than guess

say() { echo "$ME: $*" >&2; }

# --- the declaration, before any call -----------------------------------------
# jq_doc_file first (infra/lib/jq.sh, sourced by run-summary.sh): on
# jq-1.6 an empty file passes `jq -e` (backlog d96e38ab). The bounds are
# the decision's: a collaborator, read or write, never admin; never the
# site admin's own name; names and repositories in the forge's grammar,
# because both reach a URL path.
if ! jq_doc_file "$DECL" || ! jq -e --arg owner "$FORGE_OWNER" '
        type == "array" and length > 0 and all(.[];
            type == "object"
            and (.name | type == "string" and test("^[a-z][a-z0-9-]{0,38}$"))
            and .name != $owner and .name != "root" and .name != "admin"
            and (.email | type == "string" and test("^[a-z0-9.-]+@[a-z0-9.-]+[.]invalid$"))
            and (.repo | type == "string" and test("^[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*$"))
            and (.permission == "read" or .permission == "write"))' "$DECL" >/dev/null 2>&1; then
    say "REFUSED — $DECL is not a list of accounts, each a name (not $FORGE_OWNER), a .invalid address, one owner/name repository and a read or write permission"
    run_summary_field forge_accounts "refused: the declaration $DECL is unreadable or declares what this must not make"
    exit 2
fi

WORK="$(mktemp -d -t forge-account.XXXXXX)" || { say "mktemp failed"; exit 4; }
trap 'rm -rf "$WORK"' EXIT

# --- the container ------------------------------------------------------------
forgejo_user() { # <verb> [args…] — `forgejo admin user …` in the forge's container
    timeout -k 5 60 "${DOCKER[@]}" --host "$SYSTEM_DAEMON" \
        exec -u git "$CONTAINER" forgejo admin user "$@"
}

# admin_of <name> — sets ADMIN to the account's IsAdmin value and returns
# 0; 1 when it is not listed; 2 when the listing could not be had or read
# (DOCKER_WORDS says why). Called bare, never in $(…): the words a
# subshell sets die with it.
admin_of() {
    local name="$1" rc=0
    ADMIN=""
    if ! forgejo_user list >"$WORK/users" 2>"$WORK/docker.err"; then
        DOCKER_WORDS="$(head -c 300 "$WORK/docker.err" | tr '\n' ' ')"
        return 2
    fi
    awk -v want="$name" '
        !h { for (i = 1; i <= NF; i++) { if ($i == "Username") u = i; if ($i == "IsAdmin") a = i }
             if (u && a) h = 1; next }
        $u == want { print $a; found = 1 }
        END { if (!h) exit 3; if (!found) exit 1 }' "$WORK/users" >"$WORK/admin" || rc=$?
    case "$rc" in
        0) ADMIN="$(head -n 1 "$WORK/admin")"; return 0 ;;
        1) return 1 ;;
        *) DOCKER_WORDS="the listing carries no Username/IsAdmin header"; return 2 ;;
    esac
}

# inventory <declared repo> — every repository the forge HOLDS, read off
# its own repository directories inside the container (the container's
# path forge-log.sh reads david/boss.git under), lowercased owner/name one
# per line in $WORK/inventory; 4 cannot answer (WHY says why). Who asks
# does not change what is on the disk, which is the point: the API
# listing answers for its caller's view (review eb947a26, B1). Forgejo
# keeps a wiki beside its repository as <name>.wiki.git and reserves the
# .wiki suffix, so those are dropped. A root holding no declared
# repository is not this forge's — a wrong path answers with somebody
# else's directories, or none, instead of erroring.
REPO_ROOT="/data/git/repositories"
inventory() {
    local want
    if ! timeout -k 5 60 "${DOCKER[@]}" --host "$SYSTEM_DAEMON" exec -u git "$CONTAINER" \
        find "$REPO_ROOT" -mindepth 2 -maxdepth 2 -type d -name '*.git' >"$WORK/disk" 2>"$WORK/docker.err"; then
        WHY="the forge's own repository inventory could not be read: $(head -c 300 "$WORK/docker.err" | tr '\n' ' ')"
        return 4
    fi
    sed -n "s#^$REPO_ROOT/\([^/]*/[^/]*\)\.git\$#\1#p" "$WORK/disk" \
        | awk '!/\.wiki$/ { print tolower($0) }' | sort -u >"$WORK/inventory"
    want="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
    if ! grep -qxF "$want" "$WORK/inventory"; then
        WHY="the forge's repository inventory under $REPO_ROOT holds $(wc -l <"$WORK/inventory") repositories and not $1, so it is not this forge's inventory and proves nothing"
        return 4
    fi
}

# --- the API ------------------------------------------------------------------
AUTH="${BOSS_FORGE_AUTH_HEADER_FILE:-}"
API=""
if [ -n "$AUTH" ] && [ -s "$AUTH" ]; then
    sor_require BOSS_FORGE_URL
    API="${BOSS_FORGE_URL%/}/api/v1"
fi

# call <METHOD> <url> [body-file] — sets CODE, leaves the body in $WORK/out.
# A curl failure sets CURL_WORDS and returns non-zero.
call() {
    local method="$1" url="$2" body="${3:-}" rc=0
    local args=(-sS -m 20 -o "$WORK/out" -w '%{http_code}' -H "@$AUTH" -H 'Content-Type: application/json' -X "$method")
    [ -n "$body" ] && args+=(--data-binary "@$body")
    : >"$WORK/out"
    # -q FIRST (review 9b7494b3 B5, backlog 4f300192): $AUTH is the forge
    # token header file forge-converge.sh hands this as root, so a curlrc
    # in that HOME must not be read; anywhere but first -q is a no-op.
    # protect-main.sh's call(), handed the same file, has it too.
    CODE="$("$CURL" -q "${args[@]}" "$url" 2>"$WORK/err")" || rc=$?
    if [ "$rc" -ne 0 ]; then
        CURL_WORDS="curl exit $rc: $(head -c 300 "$WORK/err" | tr '\n' ' ')"
        return 1
    fi
    return 0
}
forge_words() { jq -r '.message // empty' "$WORK/out" 2>/dev/null | tr '\n' ' ' | cut -c1-200; }

# permission_on <repo> <name> — sets PERM to the effective permission; 4
# cannot answer (WHY says why). Called bare, like admin_of.
permission_on() {
    PERM=""
    call GET "$API/repos/$1/collaborators/$2/permission" || { WHY="reading $1: $CURL_WORDS"; return 4; }
    [ "$CODE" = 200 ] || { WHY="reading $2's permission on $1 answered HTTP $CODE ($(forge_words))"; return 4; }
    PERM="$(jq -r '.permission // empty' "$WORK/out")"
    [ -n "$PERM" ] || { WHY="$2's permission on $1 answered no permission"; return 4; }
}

# collaborates_on <repo> <name> — 0 a collaborator, 1 not, 4 cannot answer.
collaborates_on() {
    call GET "$API/repos/$1/collaborators/$2" || { WHY="reading $1: $CURL_WORDS"; return 4; }
    case "$CODE" in
        204) return 0 ;;
        404) return 1 ;;
        *) WHY="asking whether $2 collaborates on $1 answered HTTP $CODE ($(forge_words))"; return 4 ;;
    esac
}

# --- one account ----------------------------------------------------------------
# Each sets LINE (its packet line) and returns 0 verified, 1 red/failed,
# 4 cannot answer. A standing red also sets RED_LINE.
ensure_account() { # <name> <email> <repo> <permission>
    local name="$1" email="$2" repo="$3" want="$4" rc=0 created="" fixed="" have
    RED_LINE=""

    admin_of "$name" || rc=$?
    if [ "$rc" -eq 2 ]; then
        LINE="$name: cannot answer — the forge's accounts could not be listed: $DOCKER_WORDS"
        return 4
    fi
    if [ "$rc" -eq 1 ]; then
        # The password Forgejo generates is printed on stdout: it goes
        # nowhere. Only stderr is kept, and only on failure.
        if ! forgejo_user create --username "$name" --email "$email" \
            --random-password --must-change-password=false >/dev/null 2>"$WORK/create.err"; then
            LINE="$name: FAILED — forgejo admin user create refused: $(head -c 300 "$WORK/create.err" | tr '\n' ' ')"
            return 1
        fi
        rc=0
        admin_of "$name" || rc=$?
        case "$rc" in
            0) created=1 ;;
            1) LINE="$name: FAILED — forgejo admin user create answered, but the account is not listed"; return 1 ;;
            *) LINE="$name: cannot answer — created, but the accounts could not be listed again: $DOCKER_WORDS"; return 4 ;;
        esac
    fi
    if [ "$ADMIN" != "false" ]; then
        LINE="$name: RED — is_admin $ADMIN: a site admin's token reaches every repository on the forge; no grant was made"
        RED_LINE="RED forge-converge/forge-account refused: $name is a site admin (is_admin $ADMIN) — the converge never demotes an account; a person reads why it is one"
        return 1
    fi

    local made="${created:+created and }"
    if [ -z "$API" ]; then
        LINE="$name: ${created:+created; }is_admin false; cannot answer for the repository grant — no credential (BOSS_FORGE_AUTH_HEADER_FILE names no non-empty header file)"
        return 4
    fi

    # The one grant: written only when it differs, then read back.
    permission_on "$repo" "$name" || { LINE="$name: ${created:+created; }is_admin false; cannot answer — $WHY"; return 4; }
    have="$PERM"
    rc=0
    collaborates_on "$repo" "$name" || rc=$?
    [ "$rc" -ne 4 ] || { LINE="$name: ${created:+created; }is_admin false; cannot answer — $WHY"; return 4; }
    if [ "$have" != "$want" ] || [ "$rc" -ne 0 ]; then
        jq -nc --arg p "$want" '{permission: $p}' >"$WORK/grant.json"
        call PUT "$API/repos/$repo/collaborators/$name" "$WORK/grant.json" \
            || { LINE="$name: is_admin false; cannot answer — granting $repo: $CURL_WORDS"; return 4; }
        case "$CODE" in
            200|201|204) ;;
            *) LINE="$name: FAILED — granting $want on $repo answered HTTP $CODE ($(forge_words)); a 401/403 means the converge's forge credential cannot administer the repository"; return 1 ;;
        esac
        fixed=" (was ${have:-none})"
        permission_on "$repo" "$name" || { LINE="$name: FAILED — granted $want on $repo, but the read-back failed: $WHY"; return 1; }
        have="$PERM"
        if [ "$have" != "$want" ] || ! collaborates_on "$repo" "$name"; then
            LINE="$name: FAILED — granted $want on $repo, but it reads back as ${have:-none}"
            return 1
        fi
    fi

    # Nothing else: every repository the forge lists, page by page — held
    # to the forge's own inventory, read FIRST, so a repository made
    # between the two reads is only ever an extra one asked, and one
    # removed between them is a cannot-answer, never a pass.
    inventory "$repo" || { LINE="$name: is_admin false; cannot answer — $WHY"; return 4; }
    local page=1 rows seen=0 found="" others="" r private held unseen
    : >"$WORK/listed"
    while :; do
        if [ "$page" -gt "$MAX_PAGES" ]; then
            LINE="$name: cannot answer — the forge lists more than $((PAGE * MAX_PAGES)) repositories; not every one was asked"
            return 4
        fi
        call GET "$API/repos/search?limit=$PAGE&page=$page" \
            || { LINE="$name: is_admin false; cannot answer — listing repositories: $CURL_WORDS"; return 4; }
        [ "$CODE" = 200 ] || { LINE="$name: is_admin false; cannot answer — listing repositories answered HTTP $CODE ($(forge_words))"; return 4; }
        jq -r '.data[] | "\(.full_name) \(.private)"' "$WORK/out" >"$WORK/page" 2>/dev/null \
            || { LINE="$name: is_admin false; cannot answer — the repository listing is not {data: [...]}"; return 4; }
        rows="$(wc -l <"$WORK/page")"
        cat "$WORK/page" >>"$WORK/listed"
        while read -r r private; do
            seen=$((seen + 1))
            if [ "$r" = "$repo" ]; then found=1; continue; fi
            permission_on "$r" "$name" || { LINE="$name: is_admin false; cannot answer — $WHY"; return 4; }
            case "$PERM" in
                none) continue ;;
                read)
                    if [ "$private" = "false" ]; then
                        rc=0
                        collaborates_on "$r" "$name" || rc=$?
                        [ "$rc" -ne 4 ] || { LINE="$name: is_admin false; cannot answer — $WHY"; return 4; }
                        [ "$rc" -eq 0 ] || continue
                    fi ;;
            esac
            others="${others:+$others, }$r ($PERM)"
        done <"$WORK/page"
        [ "$rows" -lt "$PAGE" ] && break
        page=$((page + 1))
    done
    if [ -z "$found" ]; then
        LINE="$name: is_admin false; cannot answer — the forge's listing of $seen repositories does not show $repo, so it cannot see private repositories and proves nothing about the rest"
        return 4
    fi
    if [ -n "$others" ]; then
        LINE="$name: RED — is_admin false, but it reaches more than $repo: $others"
        RED_LINE="RED forge-converge/forge-account refused: $name reaches more than its one repository $repo: $others — the converge never removes a grant; a person reads why it was made"
        return 1
    fi
    # A RED above is true of what was seen, whatever was not. "Nothing
    # else" is a claim about EVERY repository, so it is made only when the
    # listing showed every one the inventory holds (review eb947a26, B1).
    # The unseen are counted, not named: their names are what the
    # credential could not see.
    held="$(wc -l <"$WORK/inventory")"
    awk '{ print tolower($1) }' "$WORK/listed" | sort -u >"$WORK/listed.lower"
    unseen="$(comm -23 "$WORK/inventory" "$WORK/listed.lower" | wc -l)"
    if [ "$unseen" -gt 0 ]; then
        LINE="$name: is_admin false; cannot answer — the forge holds $unseen repositories its listing does not show ($held in its inventory under $REPO_ROOT, $seen listed), so the converge's forge credential cannot see every repository and the absence of another grant proves nothing"
        return 4
    fi
    LINE="$name: ${made}verified — is_admin false; one repository grant: $repo $want$fixed; $seen repositories read, every one of the $held the forge holds, no other reachable beyond public read"
    return 0
}

# --- every declared account -----------------------------------------------------
lines="" reds="" worst=0
while IFS=$'\t' read -r name email repo want; do
    ensure_account "$name" "$email" "$repo" "$want"; rc=$?
    echo "$ME: $LINE"
    lines="${lines:+$lines
}$LINE"
    [ -z "$RED_LINE" ] || reds="${reds:+$reds
}$RED_LINE"
    if [ "$rc" -eq 1 ]; then worst=1
    elif [ "$rc" -eq 4 ] && [ "$worst" -eq 0 ]; then worst=4
    fi
done < <(jq -r '.[] | [.name, .email, .repo, .permission] | @tsv' "$DECL")

run_summary_field forge_accounts "$lines"
[ -z "$reds" ] || run_summary_field forge_account_red "$reds"
exit "$worst"
