#!/usr/bin/env bash
# no-github-path-outside-the-org.sh — every GitHub remote, repository
# slug, token slot and commit identity the estate's own files name
# belongs to the algedonic-dev organisation, where the GitHub App is
# installed. A personal account is not a path the estate takes.
#
# WHY THIS EXISTS (backlog d2b7c947, David 2026-09-30: "we have made a
# fundamental change to our posture and now are working professionally
# out of the algedonic-dev Github org via the Github App"). Until that
# day the public mirror's pull request was opened from a PUBLIC fork in
# David's personal account, with a personal access token he minted by
# hand, and the fork stayed declared as an off-site push target with a
# token slot of its own. Car 1 moved the publish to a branch inside the
# organisation's repository, opened as the App; car 2 retired every
# other personal-account path — the off-site target, its token slot,
# the recovery kit's copy of that token, the registry row — and this
# lint is what keeps the next one from arriving by a one-line edit. The
# reasoning is the credential broker's (CLAUDE.md §Doors, "the boundary
# is derivability"): an organisation's installation token is minted
# from root material the estate holds and revoked when its act is done;
# a personal token is placed by hand, lives until someone remembers it,
# and carries a person's whole account.
#
# THE RULE. In a tracked file under infra/, .forgejo/ or .github/:
#   * A GitHub REMOTE — `https://github.com/<owner>/<repo>` in any case
#     of scheme and host, with any user or token before the host, or its
#     ssh spellings (the `git` user at `github.com:<owner>/…`, or
#     `ssh://` to `github.com/<owner>/…`, with or without a user) — names
#     the organisation as its owner. Outside the organisation a URL is a
#     remote if it says so itself (ssh, a user before the host, a `.git`
#     segment) or its line uses it as one (a git verb, `url =`, a
#     `remote` key, a gh command); otherwise it is a LINK to read and
#     passes — `https://github.com/cli/cli` in a comment, a release
#     download (`/releases/…`, `/archive/…`) or a file (`/blob/`,
#     `/tree/`, `/raw/`). GitHub's own pages (`github.com/settings`,
#     `/apps`, …) name no owner and pass; `github.com/orgs/<owner>` and
#     `/organizations/<owner>` are judged by the owner they name.
#   * A REST path — `api.github.com/repos|orgs|users/<owner>`, or
#     `gh api repos/<owner>` with or without the leading slash — names
#     the organisation.
#   * A `gh` slug — `--repo <owner>/<repo>`, `-R <owner>/<repo>` on a
#     line that runs gh, or `GH_REPO=<owner>/<repo>` — and a pull request
#     head — `--head <owner>:<branch>` on a line that runs gh, never
#     `curl --head <url>` — name the organisation.
#   * No path has a `.` or `..` segment: git and curl follow one out of
#     the organisation while its first segment still reads as ours.
#   * Review 14cd7db9 (of car 2, landed #861) found six narrower spellings
#     that passed, each now read: a doubled slash after the host (R1), a
#     percent-encoded owner (R2, decoded before it is judged), a push
#     split across a `\` continuation (R3, the lines are joined), `gh api`
#     with flags before its path or behind a `gh_*` wrapper (R4), a
#     positional slug after `gh repo <verb>` (R5), and a URL DECLARED
#     under any key, or behind git's own flags (R6). It also found two
#     false refusals, now passing: `cp -R a/b` beside a comment naming gh,
#     and a port (`github.com:443/<org>/…`) read as the owner.
#   * Review d07c5c57 (of car 3, landed #868; backlog ef646681) found
#     three rarer spellings that passed, each now read: a GraphQL owner
#     string, `repository(owner:"<owner>"` or `owner:`/`-F owner=` in a
#     graphql command (M1); gh's `-R` glued to its value (M2); a quoted
#     URL alone on an array line (M3). And three false refusals, now
#     passing: a `gh repo clone` destination directory (F1, only clone's
#     first positional is a slug); declared third-party METADATA (F2);
#     and a flag's local-path value, `-D repos/cache`, read as a REST
#     path (F3). Three reviews of the folds that followed (e9619c13,
#     70e0d410, ab177632) narrowed F2 to what it can say on ONE line:
#   * METADATA is a URL declared under a `key:` mapping ON ITS OWN LINE
#     whose WHOLE name is a docs word — `homepage:`, `documentation:`,
#     `- sources: <url>`, `docs_link:` — and names no remote, url, uri,
#     push or target. Never an `=` or `:=` assignment, whatever it is
#     called. NOTHING IS CARRIED from a key above: a list item or a
#     bare array value has no key of its own and is judged as a remote,
#     so a docs LIST must put the key on each item's line (`- homepage:
#     <url>`) or be named in HISTORY below. Carrying the key down was
#     tried and leaked three times — an unreadable key, an inline value,
#     a dedent each let a later item inherit `sources:` — and, measured
#     2026-10-01 with the carry disabled, no file in the scanned tree
#     needed it (965 files, none refused).
#   * `--` does not hide clone's slug, and an empty `--input=` binds
#     nothing (review e9619c13).
#   * A GitHub TOKEN SLOT under /etc/boss-publish/ is one of the
#     organisation App's: `github-dr.token` (the DR copy's installation
#     token) or `github-app/<name>.token` whose name begins with the
#     organisation. The personal slot `github.token` was the fork's.
#   * A GitHub per-account no-reply address (`…@users.noreply.github.com`)
#     is an App's bot identity (`…[bot]@…`) or nothing: a person's
#     no-reply address is that person's GitHub account, and the publish
#     commits carry the publisher the verb file declares (David Auld
#     <david@algedonic.dev>, infra/ops/verbs/publish-github-pr.json),
#     never a handle. no-personal-address-in-infra.sh passes these as
#     "not a mailbox", which is right for its question; this is the
#     other question.
# A value built at run time (`$MIRROR_OWNER:$BRANCH`, `<owner>.token`)
# is not a literal and is not read: the literal it is built from is.
#
# WHAT IS NOT READ. Files outside those three directories: a test
# fixture names a personal account on purpose (the refusal tests of
# github-act.sh and of the broker's mint rule must name an owner that is
# refused, and a recorded packet or log keeps the head it recorded), and
# a person's record names their GitHub handle as an IDENTITY — the
# `github_username` on an employee row (boss-people operator_baseline.rs,
# the tenant seeds) says who someone is, not a path anything takes.
#
# HISTORY is the one allowance: an applied migration is never edited,
# so the one that declared the retired token (202609081230) keeps its
# text, and the migration that retires the row says so. Each entry must
# name a file that exists and must excuse a finding on every run
# (lib/allowlist.sh), so an entry cannot outlive what it excuses.
#
# THE FINDING names the file, the line, what shape it matched and the
# owner or slot it named — a GitHub owner is public, and the name is
# what the author needs to fix it.
#
# EXIT STATUS (house style, infra/lint/lib/git-answer.sh):
#   0  every GitHub path in those files is the organisation's
#   1  one or more is not — file:line, the shape, the owner or slot
#   3  the tree could not be listed — a fact about the MACHINE; never
#      `clean`
#
# USAGE
#   infra/lint/no-github-path-outside-the-org.sh
#   infra/lint/no-github-path-outside-the-org.sh --self-test
set -uo pipefail

NAME="no-github-path-outside-the-org"
LINT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=infra/lint/lib/scanned.sh
. "$LINT_DIR/lib/scanned.sh" || exit 3
# shellcheck source=infra/lint/lib/git-answer.sh
. "$LINT_DIR/lib/git-answer.sh" || exit 3
# shellcheck source=infra/lint/lib/allowlist.sh
. "$LINT_DIR/lib/allowlist.sh" || exit 3
cd "$LINT_DIR/../.." || exit 1

ORG="algedonic-dev"
SCANNED_DIRS=(infra/ .forgejo/ .github/)
HISTORY=(
    # Applied 2026-09-08 and never edited: it declared the personal token
    # the fork was pushed with. 20260930234959-the-personal-github-token-
    # is-retired.sql retires the row it made.
    infra/postgres/schema/202609081230-the-dauld-github-token-is-declared.sql
)

# The shapes. Two families, because the review of this lint (b0607d1d)
# found a bypass in each direction of case:
#   * CI_SHAPES are matched case-INSENSITIVELY. Git and GitHub read the
#     scheme and the host without case, so `HTTPS://GitHub.com/<owner>/…`
#     pushes exactly where the lower-case spelling does. A URL may carry
#     ANY userinfo before the host — `x-access-token:$T@`, `<user>:${TOKEN}@`
#     — which is the usual way a token push is written, and the shape of
#     the Forgejo push mirror that rewound main (forge_log_sh.rs); ssh may
#     name no user. A REST path on the api host may double its slashes.
#   * The gh shapes are read case-SENSITIVELY and only in a COMMAND that
#     runs gh — the line cut at `;`, `|`, `&`, `(`, `)` and backticks
#     outside quotes, from the first `gh` (or a `gh_*` wrapper, as
#     publish-github-pr.sh's `gh_t` and `gh_status` are) onward. `-R` is
#     gh's short `--repo`, while `cp -R a/b` is no slug even beside a
#     comment that mentions gh (review 14cd7db9); `--head` is a pull
#     request head only for gh, while `curl --head https://…` is an
#     ordinary probe. In such a command `repos|orgs|users/<owner>` is a
#     REST path wherever it stands — `gh api -i repos/…`, behind any
#     flags, is the form the publish verb's wrapper runs — and every
#     `<owner>/<repo>` word after `gh repo <verb>` is a positional slug
#     (`gh repo clone <slug>`). `GH_REPO=<owner>/<repo>` is gh's
#     environment and is read anywhere.
# Every owner and path is percent-DECODED before it is judged: git and
# GitHub decode `%64auld` to an owner that is not ours (review 14cd7db9,
# R2), and `%2e%2e` is a dot segment. A port after a scheme's host
# (`github.com:443/`) is the host's, not an owner.
PATH_CHARS='A-Za-z0-9._%-'
OWNER='[A-Za-z0-9%][A-Za-z0-9%-]*'
URL='((https?|ssh|git)://([^/@[:space:]"'"'"'`]+@)?|git@)(www\.)?github\.com(:[0-9]+)?[:/]+['"$PATH_CHARS"'][/'"$PATH_CHARS"']*#?'
API_HOST='api\.github\.com/+(repos|orgs|users)/+['"$PATH_CHARS"']+(/+['"$PATH_CHARS"']+)?'
TOKEN_SLOT='/etc/boss-publish/[A-Za-z0-9._/-]*\.token'
NOREPLY='[A-Za-z0-9._%+-]+(\[bot\])?@users\.noreply\.github\.com'
CI_SHAPES="$URL|$API_HOST|$TOKEN_SLOT|$NOREPLY"
GH_WORD='(^|[^A-Za-z0-9_.-])gh(_[A-Za-z0-9]+)? +'
GH_ENV='GH_REPO=["'"'"']?'"$OWNER"'/['"$PATH_CHARS"']+'
# `-R` may be glued to its value (`-R<owner>/<repo>`, `-R=<owner>/…`):
# gh's flag parser reads all three spellings (review d07c5c57, M2). The
# glued form must stand at a word's start, so `fix-Rust/x` is no slug.
GH_SLUG='(--repo[= ]+|(^|[[:space:]])-R[= ]*)["'"'"']?'"$OWNER"'/['"$PATH_CHARS"']+'
GH_HEAD='--head[= ]+["'"'"']?'"$OWNER"':'
GH_REST='(^|[[:space:]"'"'"'=])/*(repos|orgs|users)/+['"$PATH_CHARS"']+(/+['"$PATH_CHARS"']+)?'
GH_REPO_CMD='^gh(_[A-Za-z0-9]+)? +repo +[a-z-]+'
GH_POSITIONAL='(^|[[:space:]=])["'"'"']?'"$OWNER"'/['"$PATH_CHARS"']+'
# A flag whose value is a LOCAL path, never a REST one: `gh run download
# -D repos/cache` names a directory (review d07c5c57, F3). Its value is
# blanked before a REST path is looked for; every other flag's value is
# still read, because a boolean before the path (`gh api -i repos/…`)
# looks exactly like a flag taking one.
# The value binds as pflag binds it: `=` takes the rest of the word,
# even when that is EMPTY, and a space takes the next word — never both,
# or `--input= repos/<owner>/…` blanked the endpoint (review e9619c13, B4).
GH_LOCAL_FLAG='(^|[[:space:]])(-D|-O|--dir|--output|--input)(=["'"'"']?[^[:space:]"'"'"']*|[[:space:]]+["'"'"']?[^[:space:]"'"'"']+)'
# A GraphQL owner string (review d07c5c57, M1): `repository(owner:"<o>"`
# is read anywhere, escaped or not, because no other language spells it;
# a bare `owner: "<o>"` and the `-F owner=<o>` variable only in a
# command that runs gh's graphql. GraphQL strings take double quotes only.
GQL_REPO='repository[[:space:]]*\([[:space:]]*owner[[:space:]]*:[[:space:]]*\\?"'"$OWNER"
GQL_OWNER='(^|[^A-Za-z0-9_])owner[[:space:]]*:[[:space:]]*\\?"'"$OWNER"
GQL_VAR='(-[fF]|--(raw-)?field)[= ]+["'"'"']?owner='"$OWNER"
# Declared third-party METADATA is a page to read, not a remote: a
# `homepage:`, a `documentation` link, a `- sources: <url>` item (review
# d07c5c57, F2). Decided by the NAME of the key on the URL's own line —
# never one opened above it (review ab177632) — and a
# key that names remote, url, push or target is a remote whatever else it
# says (`homepage_url`, `docs_url`). A remote context on the line still
# wins. Review e9619c13 of the first fold: the docs word is the WHOLE
# key, optionally qualified as a link (`docs_link`, `homepage_url`) —
# never a suffix, since `publish_home` and `mirror_docs` are names a
# script pushes to — and only a `key:` MAPPING is metadata. An `=` or
# `:=` assignment (shell, TOML) is a variable something uses, whatever
# it is called (`export FORK_HOME=<url>` reopened review 14cd7db9's R6).
# `_uri` is a remote exactly as `_url` is (review 70e0d410: the split
# was arbitrary); `_link(s)` and `_page` name a page.
DOC_KEY='^(homepage|home|sources|documentation|docs|doc|readme|website|changelog|links?|references?)([_.-](url|uri|links?|page))?$'
REMOTE_KEY='remote|url|uri|push|target'
# A URL with no .git, no userinfo and no ssh is a REMOTE only where the
# line uses it as one: a git verb (behind any of git's own flags,
# `git --git-dir=… push`), a `url =`/`pushurl`/`insteadOf` setting, a
# declaration key named remote, or a gh command — or where the line
# DECLARES it, whatever the key is called (`"target": "<url>"`,
# `export X=<url>`: review 14cd7db9, R6), because a declared repository
# is one something will take. Anywhere else it is a link to read —
# `https://github.com/cli/cli` in a comment passes, as the header
# promises.
REMOTE_CONTEXT='git +((-c|--git-dir|--work-tree|--namespace) +[^ ]+ +|-[^ ]+ +)*(push|clone|fetch|pull|ls-remote|remote|submodule)|(^|[^a-z])(push)?url *=|insteadof|remote|(^|[^A-Za-z0-9_.-])gh(_[a-z0-9]+)? +(repo|pr|api)'
# The declaring prefix: a key (behind a YAML list dash or a shell
# declaration word), a list dash, or — review d07c5c57, M3 — a QUOTED
# value alone on its line, which is an array element whose key was
# opened on an earlier line (`"mirrors": [` then `"<url>",`). An
# unquoted URL alone on a line is prose and is not read as declared.
DECL_KEY='(-[[:space:]]+)?((export|local|readonly|declare)[[:space:]]+(-[A-Za-z]+[[:space:]]+)?)?["'"'"']?([A-Za-z0-9_.-]+)["'"'"']?[[:space:]]*(:=|=|:)[[:space:]]*\[?'
# A `key:` MAPPING, as opposed to an assignment: the colon is followed by
# anything but `=`. Decided from the prefix itself, never from which
# alternative a regex engine captured — GNU sed took `:` out of `:=` and
# read `homepage := <url>` as a mapping (review 70e0d410, C1).
MAP_PRE='^[[:space:]]*(-[[:space:]]+)?["'"'"']?[A-Za-z0-9_.-]+["'"'"']?[[:space:]]*:([^=]|$)'
DECL_PRE='^[[:space:]]*('"$DECL_KEY"'[[:space:]]*["'"'"']?|-[[:space:]]*["'"'"']?|\[?[[:space:]]*["'"'"'])$'
DECL_POST='^["'"'"']?[]}),[:space:]]*(#.*)?$'

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }

# dotted <path> — 0 iff a `/`-separated segment of <path> is `.` or `..`:
# git and curl follow one, so `<org>/../<owner>/<repo>` leaves the
# organisation while its first segment still reads as the organisation's
# (review b0607d1d, N1).
dotted() {
    case "/$1/" in */./*|*/../*) return 0 ;; esac
    return 1
}

# judge <match> <line> — prints `<shape>: <what it names>` for a match
# outside the organisation; prints nothing for one inside it or for a read.
judge() {
    local m="$1" line="$2" lm path owner o seg rest
    lm="$(lower "$m")"
    case "$lm" in
        gh-gql\ *)
            owner="${m#gh-gql }"
            placeholder "$owner" || [ "$(lower "$owner")" = "$ORG" ] || echo "a GraphQL owner outside $ORG (owner $owner)"
            return 0 ;;
        *@users.noreply.github.com)
            case "$m" in *'[bot]@'*) return 0 ;; esac
            echo "a personal GitHub no-reply identity (${m%@*}) — the publisher is a name and a company address, never a handle"
            return 0 ;;
        /etc/boss-publish/*)
            case "${lm#/etc/boss-publish/}" in
                github-dr.token|github-app/"$ORG"*.token) return 0 ;;
            esac
            echo "a GitHub token slot outside the organisation App's ($m) — a token the estate holds is an installation token minted for $ORG"
            return 0 ;;
        gh-slug\ *)
            rest="${m#gh-slug }"
            path="$(pct_decode "${rest#*|}")"
            owner="${path%%/*}"
            placeholder "$owner" || [ "$(lower "$owner")" = "$ORG" ] || echo "a gh slug outside $ORG (${rest%%|*}$owner/…)"
            return 0 ;;
        gh-head\ *)
            owner="$(pct_decode "${m#gh-head }")"
            ! placeholder "$owner" || return 0
            case "$(lower "$owner")" in
                http|https|"$ORG") return 0 ;;
            esac
            echo "a pull request head outside $ORG ($owner:…)"
            return 0 ;;
        gh-api\ *|api.github.com/*)
            # Anchored at the shape's own start, never `^.*`: a greedy
            # strip finds the LAST `repos/` and judges the wrong segment.
            path="$(printf '%s' "$lm" | sed -E 's#^(gh-api +|api\.github\.com/+)/*[a-z]+/+##')"
            path="$(lower "$(pct_decode "$path")")"
            owner="${path%%/*}"
            m="${m/#gh-api /gh api }"
            if dotted "$path"; then
                echo "a GitHub REST path with a dot segment ($m) — it is followed out of the organisation"
            elif ! placeholder "$owner" && [ "$owner" != "$ORG" ]; then
                echo "a GitHub REST path outside $ORG ($m)"
            fi
            return 0 ;;
    esac
    # A remote or a link: the path after the host, without an anchor. The
    # prefix is stripped ANCHORED, from the lower-cased copy and then by
    # length off the original: `^.*github.com/` stripped to the LAST
    # `github.com/` in the path, so `<owner>/github.com/<org>/<repo>` read
    # as the organisation's (review 14cd7db9). A scheme's port is the
    # host's; scp syntax (the git user's host, a colon, a path) has no
    # port, so there a number is the owner. Then any run of `:` and `/` — git follows
    # `github.com//<owner>` (R1) — and the percent-decode (R2).
    rest="$(printf '%s' "$lm" | sed -E 's#^((https?|ssh|git)://([^/@]*@)?|git@)(www\.)?github\.com##')"
    rest="${m:$((${#m} - ${#rest}))}"
    case "$lm" in git@*) ;; *) rest="$(printf '%s' "$rest" | sed -E 's/^:[0-9]+//')" ;; esac
    path="$(printf '%s' "$rest" | sed -E 's#^[:/]+##; s/#$//')"
    path="$(pct_decode "$path")"
    if dotted "$path"; then
        echo "a GitHub path with a dot segment ($m) — git follows it out of the organisation"
        return 0
    fi
    owner="${path%%/*}"
    o="$(lower "$owner")"
    local page=""
    case "$o" in
        orgs|organizations)
            page=1; rest="${path#*/}"; owner="${rest%%/*}"; o="$(lower "$owner")" ;;
        settings|apps|features|marketplace|login|sponsors|notifications|topics|about|pricing|enterprise|site|security|contact)
            return 0 ;;
    esac
    [ "$o" != "$ORG" ] || return 0
    ! placeholder "$owner" || return 0
    # Outside the organisation: a remote is refused, a link to read is not.
    # It is a remote if it says so itself — ssh or the git scheme, a user
    # before the host, a segment ending .git — or the line uses it as one.
    case "$lm" in
        ssh://*|git://*|git@*|*://*@*) echo "a GitHub remote outside $ORG (owner $owner)"; return 0 ;;
    esac
    for seg in ${path//\// }; do
        case "$(lower "$seg")" in *.git) echo "a GitHub remote outside $ORG (owner $owner)"; return 0 ;; esac
    done
    # A download or a file read is never a remote, whatever the line calls
    # it: `<owner>/<repo>/releases/…`, `/archive/…`, `/blob/…`, `/tree/…`,
    # `/raw/…`. A page that is only LIKE a read (`/pull`, `/issues`, an
    # anchor) is judged by its line below — `git push …/<repo>/pull` is a
    # push (review b0607d1d).
    case "$(lower "$path")" in
        */*/releases/*|*/*/archive/*|*/*/blob/*|*/*/tree/*|*/*/raw/*) return 0 ;;
    esac
    if grep -iqE -- "$REMOTE_CONTEXT" <<<"$line"; then
        echo "a GitHub remote outside $ORG (owner $owner)"
    elif [ -z "$page" ] && [ "${path#*/}" != "$path" ] && declares "$m" "$line" \
        && ! metadata "$m" "$line"; then
        echo "a GitHub remote outside $ORG (owner $owner) — declared as a value, which something will take"
    fi
}

# pct_decode <text> — <text> with every `%XX` decoded, repeatedly until
# nothing decodes: git and GitHub decode an owner before they route it
# (review 14cd7db9, R2), and a doubly-encoded `%2564` is judged by what
# it finally names. Only a `%` followed by two hex digits is decoded; one
# that is not stays, and an owner still holding a `%` is a printf
# placeholder (`%s/boss`), built at run time like `$OWNER` and not read
# (see placeholder) — no GitHub owner can contain one.
pct_decode() {
    local s="$1" prev i=0
    while [ "$i" -lt 4 ]; do
        case "$s" in *%[0-9A-Fa-f][0-9A-Fa-f]*) ;; *) break ;; esac
        prev="$s"
        s="$(printf '%s' "$s" | sed -E 's/\\/\\\\/g; s/%([0-9A-Fa-f]{2})/\\x\1/g')"
        s="$(printf '%b' "$s")"
        [ "$s" != "$prev" ] || break
        i=$((i + 1))
    done
    printf '%s' "$s"
}

# placeholder <owner> — 0 iff the decoded <owner> is no literal: it still
# holds a `%`, which no GitHub account name can.
placeholder() {
    case "$1" in *%*) return 0 ;; esac
    return 1
}

# declares <match> <line> — 0 iff <line> declares <match> as a value:
# `key = <url>`, `"key": "<url>",`, `export KEY=<url>`, a list item. Any
# key, not only remote and url (review 14cd7db9, R6: `"target":`).
declares() {
    local pre="${2%%"$1"*}" post="${2#*"$1"}"
    grep -E -- "$DECL_PRE" <<<"$pre" >/dev/null && grep -E -- "$DECL_POST" <<<"$post" >/dev/null
}

# metadata <match> <line> — 0 iff <line> declares <match> under a `key:`
# MAPPING on that same line (MAP_PRE; never an `=` or `:=` assignment)
# whose whole name is a docs word (DOC_KEY) and names nothing a remote
# is (REMOTE_KEY). The key is read off the same prefix declares judged.
# Nothing is carried from a line above (review ab177632): a list item or
# a bare array value has no key of its own, so it is no metadata.
metadata() {
    local pre="${2%%"$1"*}" k
    grep -E -- "$MAP_PRE" <<<"$pre" >/dev/null || return 1
    k="$(lower "$(sed -nE 's/^[[:space:]]*'"$DECL_KEY"'.*/\5/p' <<<"$pre")")"
    [ -n "$k" ] || return 1
    ! grep -qE -- "$REMOTE_KEY" <<<"$k" && grep -qE -- "$DOC_KEY" <<<"$k"
}

# joined_lines <file> — the file as `<n>:<text>`, a line ending in `\`
# joined to the next under the FIRST line's number, so a push split
# across a continuation is judged with its verb (review 14cd7db9, R3).
joined_lines() {
    awk '{ if (!open) start = NR
           if (sub(/\\$/, "")) { buf = buf $0 " "; open = 1; next }
           print start ":" buf $0; buf = ""; open = 0 }
         END { if (open) print start ":" buf }' "$1"
}

# gh_segments <text> — each command in <text> that runs gh, from the gh
# word on: the text cut at `;`, `|`, `&`, `(`, `)` and backticks outside
# quotes. A quote after a letter (`it's`) opens nothing. So `cp -R a/b`
# beside a comment that mentions gh is a command of its own (review
# 14cd7db9's first false refusal).
gh_segments() {
    printf '%s\n' "$1" | awk '
        function emit(s) {
            if (match(s, /(^|[^A-Za-z0-9_.-])gh(_[A-Za-z0-9]+)? +/)) {
                s = substr(s, RSTART); sub(/^[^g]/, "", s); print s
            }
        }
        { n = length($0); seg = ""; q = ""; p = ""
          for (i = 1; i <= n; i++) {
              c = substr($0, i, 1)
              if (q != "") { if (c == q) q = ""; seg = seg c }
              else if ((c == "\"" || c == "\047") && p !~ /[A-Za-z0-9]/) { q = c; seg = seg c }
              else if (index(";|&()`", c)) { emit(seg); seg = "" }
              else seg = seg c
              p = c
          }
          emit(seg) }'
}

# gh_matches <text> — the gh shapes in <text>, each tagged for judge:
# `gh-slug <how it was named>|<owner>/<repo>`, `gh-head <owner>`,
# `gh-api <path>`.
gh_matches() {
    local seg
    grep -oE -- "$GH_ENV" <<<"$1" | sed -E 's/^GH_REPO=["'"'"']?/gh-slug GH_REPO=|/'
    grep -oE -- "$GQL_REPO" <<<"$1" | sed -E 's/^.*"/gh-gql /'
    while IFS= read -r seg; do
        [ -n "$seg" ] || continue
        grep -oE -- "$GH_SLUG" <<<"$seg" | sed -E 's/^[[:space:]]*(--repo|-R)[= ]*["'"'"']?/gh-slug \1 |/'
        grep -oE -- "$GH_HEAD" <<<"$seg" | sed -E 's/^--head[= ]+["'"'"']?/gh-head /; s/:$//'
        sed -E "s/$GH_LOCAL_FLAG/\\1\\2 _/g" <<<"$seg" | grep -oiE -- "$GH_REST" | sed -E 's#^[^A-Za-z]*#gh-api #'
        if grep -E -- '(^|[[:space:]])graphql([[:space:]]|$)' <<<"$seg" >/dev/null; then
            grep -oE -- "$GQL_OWNER" <<<"$seg" | sed -E 's/^.*"/gh-gql /'
            grep -oE -- "$GQL_VAR" <<<"$seg" | sed -E 's/^.*owner=/gh-gql /'
        fi
        if grep -E -- "$GH_REPO_CMD" <<<"$seg" >/dev/null; then
            repo_positionals "$seg" | grep -oE -- "$GH_POSITIONAL" \
                | sed -E 's/^[[:space:]=]?["'"'"']?/gh-slug gh repo |/'
        fi
    done < <(gh_segments "$1")
}

# repo_positionals <gh repo segment> — the words after `gh repo <verb>`
# that are read as slugs: every one, except for `clone`, whose second
# positional is the local directory it clones INTO and whose words after
# `--` are git's flags — so only its first positional is read, skipping
# `-u <name>` (review d07c5c57, F1: `gh repo clone <org>/boss work/dir`
# read the destination as a slug).
repo_positionals() {
    local verb rest w skip="" words
    verb="$(sed -nE "s/^gh(_[A-Za-z0-9]+)? +repo +([a-z-]+).*/\\2/p" <<<"$1")"
    rest="$(sed -E "s/$GH_REPO_CMD//" <<<"$1")"
    [ "$verb" = clone ] || { printf '%s\n' "$rest"; return 0; }
    read -ra words <<<"$rest"
    # `--` ends clone's FLAGS, not its positionals: cobra strips it and
    # the next word is still the repository, so `gh repo clone --
    # <owner>/<repo>` clones it (review e9619c13, B3). Once a positional
    # is read, what follows `--` is git's and is not.
    local dashdash=""
    for w in "${words[@]}"; do
        if [ -n "$skip" ]; then skip=""; continue; fi
        if [ -n "$dashdash" ]; then printf ' %s\n' "$w"; return 0; fi
        case "$w" in
            -u|--upstream-remote-name) skip=1 ;;
            --) dashdash=1 ;;
            -*) ;;
            *) printf ' %s\n' "$w"; return 0 ;;
        esac
    done
}

# One file's findings as lines of `<line>: <finding>`; empty = clean.
# Each line that carries any shape is read whole, so a URL is judged with
# the words around it.
findings_in() { # file
    local hit line text m why
    joined_lines "$1" 2>/dev/null | grep -iE -- "$CI_SHAPES|GH_REPO=|$GH_WORD|repository" | while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        line=${hit%%:*}
        text=${hit#*:}
        {
            grep -oiE -- "$CI_SHAPES" <<<"$text"
            gh_matches "$text"
        } | while IFS= read -r m; do
            [ -n "$m" ] || continue
            why="$(judge "$m" "$text")"
            [ -z "$why" ] || printf '%s: %s\n' "$line" "$why"
        done
    done | sort -u -t: -k1,1n -k2
}

# --- self-test ---------------------------------------------------------
# Runs on every invocation: a pattern that stopped matching passes every
# file, and only a fixture it must refuse tells that from a clean tree.
# Every refused fixture is assembled from parts, so no line of THIS file
# has the shape it refuses: the lint scans itself (it lives under infra/).
# The later lines of each list are review b0607d1d's: every shape it
# found passing (B1) is refused, every legitimate line it found refused
# (B2) passes, and a dot segment (N1) is refused.
self_test() {
    local t
    t="$(mktemp -d)" || { echo "$NAME: cannot make a temp dir for the self-test" >&2; return 1; }
    # shellcheck disable=SC2064
    trap "rm -rf '$t'" RETURN

    local gh='github.com' GH='GitHub.com' p='someone' at='@' nr='users.noreply.github.com' slot='/etc/boss-publish'
    {
        printf 'a = "https://%s/%s/boss.git"\nb = https://%s/Algedonic-Dev/Boss-DR\n' "$gh" "$ORG" "$gh"
        printf 'c = git%s%s:%s/boss\nd = https://api.%s/repos/%s/boss/pulls\n' "$at" "$gh" "$ORG" "$gh" "$ORG"
        printf 'e = gh pr list --repo %s/boss --head %s:publish/x\n' "$ORG" "$ORG"
        printf 'f = %s/github-dr.token\ng = %s/github-app/%s-publish.token\n' "$slot" "$slot" "$ORG"
        printf 'h = curl -L https://%s/%s/tool/releases/download/v1/tool\n' "$gh" "$p"
        printf 'i = see https://%s/%s/tool#readme and https://%s/%s/tool/blob/main/x\n' "$gh" "$p" "$gh" "$p"
        printf 'j = https://%s/settings/apps\nk = 41898282+github-actions[bot]%s%s\n' "$gh" "$at" "$nr"
        printf 'l = --head "$OWNER:$BRANCH" --repo "$SLUG" %s/github-app/$owner.token\n' "$slot"
        printf 'm = git push https://%s/%s/boss.git main\nn = HTTPS://%s/%s/boss\n' "$GH" "$ORG" "$gh" "$ORG"
        printf 'o = git push https://x-access-token:%s%s%s/%s/boss.git\n' '$T' "$at" "$gh" "$ORG"
        printf 'p = git push ssh://%s/%s/boss.git\nq = gh pr list -R %s/boss\nr = GH_REPO=%s/boss gh pr list\n' "$gh" "$ORG" "$ORG" "$ORG"
        printf 's = gh api repos/%s/boss/pulls\nt = gh api /orgs/%s/installation\n' "$ORG" "$ORG"
        printf 'u = curl -sS --head https://example.org/x\nv = curl -sS -o /dev/null --head http://example.org:3000/\n'
        printf 'w = the GitHub CLI, https://%s/cli/cli, opens it\nx = https://%s/orgs/%s/people\n' "$gh" "$gh" "$p"
        printf 'y = cp -R some/dir elsewhere; rsync -r a/b c\n'
        printf 'z = URL="https://%s/%s/tool/releases/download/v1/tool.tgz"\n' "$gh" "$p"
        # Review 14cd7db9: the two fail-closed false refusals, and the
        # organisation's own spelling of each newly-read shape.
        printf '# hand it to %s pr create later; cp -R some/dir elsewhere\n' gh
        printf "cp -R some/dir elsewhere  # it's what gh does next\n"
        printf 'git clone https://%s:443/%s/boss.git\n' "$gh" "$ORG"
        printf 'gh api -i repos/%s/boss/pulls\ngh_t api "repos/%s/boss"\n' "$ORG" "$ORG"
        printf 'gh repo clone %s/boss\ngh repo view %s/boss --web\n' "$ORG" "$ORG"
        printf '"target": "https://%s/%s/boss",\n' "$gh" "$ORG"
        printf 'git push \\\n  https://%s/%s/boss main\n' "$gh" "$ORG"
        printf "printf 'git push https://%s/%%s/boss.git' \"\$owner\"\n" "$gh"
        # Review d07c5c57 (backlog ef646681): its three false refusals
        # pass — a clone's destination directory (F1), declared
        # third-party metadata under a docs-shaped key (F2), a flag's
        # local-path value inside a gh command (F3) — and the
        # organisation's spelling of each newly-read shape (M1-M3).
        printf 'gh repo clone %s/boss work/dir\ngh repo clone -u up %s/boss some/dir -- --depth 1\n' "$ORG" "$ORG"
        printf 'homepage: https://%s/%s/tool\n  - homepage: https://%s/%s/tool\n' "$gh" "$p" "$gh" "$p"
        printf '"documentation": "https://%s/%s/tool",\n' "$gh" "$p"
        printf 'gh run download 1 -D repos/cache\ngh release download v1 --dir=repos/cache\n'
        printf "gh api graphql -f query='{ repository(owner:\"%s\", name:\"boss\") { id } }'\n" "$ORG"
        printf 'gh api graphql -F owner=%s -f query=@q.graphql\ngh pr list -R%s/boss\n' "$ORG" "$ORG"
        printf '"mirrors": [\n  "https://%s/%s/boss"\n]\n' "$gh" "$ORG"
        # Review e9619c13: a docs word qualified as a link is still
        # metadata, and the organisation's spellings of B3 and B4.
        printf 'docs_link: https://%s/%s/tool\ngh repo clone -- %s/boss work/dir\n' "$gh" "$p" "$ORG"
        printf 'gh api --input= repos/%s/boss/git/refs\n' "$ORG"
    } >"$t/good.txt"
    [ -z "$(findings_in "$t/good.txt")" ] || {
        echo "$NAME: self-test FAILED — an organisation path, a read of a public page, or a line that is no GitHub path at all was refused:" >&2
        findings_in "$t/good.txt" >&2
        return 1
    }

    {
        printf 'remote = "https://%s/%s/boss-mirror.git"\n' "$gh" "$p"
        printf 'x = git%s%s:%s/boss-fork\n' "$at" "$gh" "$p"
        printf 'y = gh pr create --repo %s/boss --head %s:publish/x\n' "$p" "$p"
        printf 'token_file = "%s/github.token"\n' "$slot"
        printf 'z = https://api.%s/repos/%s/boss\n' "$gh" "$p"
        printf 'author = 123+%s%s%s\n' "$p" "$at" "$nr"
        printf 'git push https://%s/%s/boss-mirror.git main\n' "$GH" "$p"
        printf 'HTTPS://%s/%s/boss-mirror.git\n' "$gh" "$p"
        printf 'git push https://x-access-token:%s%s%s/%s/boss-mirror.git\n' '$T' "$at" "$gh" "$p"
        printf 'url = https://%s:%s%s%s/%s/boss-mirror.git\n' "$p" '${TOKEN}' "$at" "$gh" "$p"
        printf 'ssh://%s/%s/boss-mirror.git\n' "$gh" "$p"
        printf 'gh pr create -R %s/boss\n' "$p"
        printf 'GH_REPO=%s/boss gh pr list\n' "$p"
        printf 'gh api repos/%s/boss-mirror/pulls\n' "$p"
        printf 'gh api /repos/%s/boss\n' "$p"
        printf 'curl -X PUT https://%s/%s/boss-mirror.git#\n' "$gh" "$p"
        printf 'git push https://%s/%s/boss-mirror/pull main\n' "$gh" "$p"
        printf 'remote = "https://%s/%s/../cli/cli.git"\n' "$gh" "$ORG"
        # Review 14cd7db9, R1-R6, lines 19-33: a doubled slash after the
        # host (git follows it), a percent-encoded owner, a remote named
        # after the host's own path, flags or a wrapper between gh and
        # its REST path, a positional gh slug, a declaration key other
        # than remote/url, and git's own flags before its verb.
        printf 'remote = "https://%s//%s/boss-mirror.git"\n' "$gh" "$p"
        printf 'git ls-remote git%s%s:/%s/boss-mirror\n' "$at" "$gh" "$p"
        printf 'curl https://api.%s//repos//%s/boss\n' "$gh" "$p"
        printf 'remote = "https://%s/%%73omeone/boss-mirror.git"\n' "$gh"
        printf 'gh pr list --repo %%73omeone/boss\n'
        printf 'remote = "https://%s/%s/%s/%s/boss.git"\n' "$gh" "$p" "$gh" "$ORG"
        printf 'gh api -i repos/%s/boss/pulls\n' "$p"
        printf 'gh_t api "repos/%s/boss/pulls/1"\n' "$p"
        printf 'code="$(gh_status --method DELETE /repos/%s/boss)"\n' "$p"
        printf 'gh repo clone %s/boss\n' "$p"
        printf 'gh repo view --web %s/boss\n' "$p"
        printf '"target": "https://%s/%s/boss-mirror",\n' "$gh" "$p"
        printf 'export TARGET=https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'git --git-dir=/srv/x push https://%s/%s/boss-mirror main\n' "$gh" "$p"
        printf 'cd /srv && git -C x push https://%s/%s/boss-mirror main\n' "$gh" "$p"
        printf 'git push https://%s/%%2573omeone/boss-mirror.git main\n' "$gh"
        # Line 35 continues onto 36: the push is judged as one line.
        printf 'git push \\\n  https://%s/%s/boss-mirror main\n' "$gh" "$p"
        # Review d07c5c57 (backlog ef646681), lines 37-54. M1: a GraphQL
        # owner string — in repository(…) anywhere (37, and 38 escaped
        # inside a JSON body), as owner: in a graphql command (39), or as
        # its owner variable (40). M2: gh's -R glued to its value (41, 42).
        # M3: a URL alone on an array line under a key opened earlier (44,
        # 47; the key lines and closing brackets carry nothing). And the
        # edges of the three fixes, each still refused: a metadata-looking
        # key that names a url (49), a clone's FIRST positional (50).
        printf "gh api graphql -f query='{ repository(owner:\"%s\", name:\"boss\") { id } }'\n" "$p"
        printf '  "query": "{ repository(owner: \\"%s\\", name: \\"boss\\") { id } }"\n' "$p"
        printf "gh api graphql -f query='{ x(owner: \"%s\") { id } }'\n" "$p"
        printf 'gh api graphql -F owner=%s -f query=@q.graphql\n' "$p"
        printf 'gh pr create -R%s/boss\ngh pr list -R=%s/boss\n' "$p" "$p"
        printf '"mirrors": [\n  "https://%s/%s/boss-mirror"\n]\n' "$gh" "$p"
        printf 'remotes = [\n  "https://%s/%s/boss-mirror",\n]\n' "$gh" "$p"
        printf 'homepage_url: https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'gh repo clone %s/boss work/dir\n' "$p"
        # Review e9619c13 of this car, lines 51-59: what its first fold
        # opened. B1: a shell or TOML assignment is never metadata,
        # whatever its name ends in, and a docs word must be the WHOLE
        # key (51-54, 59). B3: `--` ends clone's flags, not its
        # positionals (55). B4: an empty `--input=` binds nothing, so the
        # endpoint after it is read (56, 57). B5: a docs-shaped key that
        # names a url is a remote — the REMOTE_KEY guard (58).
        printf 'publish_home=https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'export FORK_HOME=https://%s/%s/boss-fork\n' "$gh" "$p"
        printf 'MIRROR_LINK="https://%s/%s/boss-mirror"\n' "$gh" "$p"
        printf 'docs = "https://%s/%s/boss-mirror"\n' "$gh" "$p"
        printf 'gh repo clone -- %s/boss-mirror\n' "$p"
        printf 'gh api --input= repos/%s/boss-mirror/git/refs -f ref=x\n' "$p"
        printf 'gh api -D= /repos/%s/boss-mirror\n' "$p"
        printf 'docs_url: https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'mirror_docs: https://%s/%s/boss-mirror\n' "$gh" "$p"
        # Review 70e0d410 of the second fold, lines 60-63. C1: a `:=`
        # assignment is no mapping, spaced or glued (60-62). And `_uri`
        # is a remote like `_url` (63).
        printf 'homepage := https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'homepage:=https://%s/%s/boss-mirror\n' "$gh" "$p"
        printf 'docs := "https://%s/%s/boss-mirror"\n' "$gh" "$p"
        printf 'docs_uri: https://%s/%s/boss-mirror\n' "$gh" "$p"
        # Review ab177632 (line 65): no key is CARRIED to the lines below
        # it, so a bare list item is judged as a remote even under a
        # `sources:` key — the key goes on the item's own line.
        printf 'sources:\n  - https://%s/%s/tool\n' "$gh" "$p"
    } >"$t/bad.txt"
    local got lines want
    got="$(findings_in "$t/bad.txt")"
    lines="$(printf '%s\n' "$got" | cut -d: -f1 | sort -un | paste -sd, -)"
    want="$(printf '%s\n' $(seq 1 35) $(seq 37 42) 44 47 $(seq 49 63) 65 | paste -sd, -)"
    [ "$lines" = "$want" ] || {
        echo "$NAME: self-test FAILED — expected a finding on each of lines $want, got lines $lines:" >&2
        printf '%s\n' "$got" >&2
        return 1
    }
    [ "$(printf '%s\n' "$got" | grep -c '^3: ')" = 2 ] || {
        echo "$NAME: self-test FAILED — line 3 carries a slug AND a head outside the organisation; expected both named, got:" >&2
        printf '%s\n' "$got" >&2
        return 1
    }
    echo "$NAME: self-test ok — organisation remotes (any case, any userinfo, ssh), slugs (--repo, -R, GH_REPO), heads, REST paths (api host or gh api), token slots and bot identities pass, as do reads of public pages, curl --head, a cp -R beside a gh comment, a port, a clone's destination, project metadata under a docs key on its own line and a -D directory; 59 personal or dot-segment shapes are each refused, among them doubled slashes, percent-encoded owners, gh api behind flags or a wrapper, positional gh slugs, any declared key, a continued line, GraphQL owners, a glued -R, a bare array or list value, an assignment under any name, a clone after --, an endpoint after an empty --input= and a := assignment"
}

if [ "${1:-}" = "--self-test" ]; then self_test; exit $?; fi
self_test || exit 1

# --- the tree ----------------------------------------------------------
files="$(git_answer "$NAME" 0 ls-files -- "${SCANNED_DIRS[@]}")" || exit $?
allowlist_paths_exist "$NAME" "${HISTORY[@]}"

scanned=0
findings=0
used=""
while IFS= read -r file; do
    [ -n "$file" ] || continue
    [ -f "$file" ] || continue
    scanned=$((scanned + 1))
    hits="$(findings_in "$file")"
    [ -n "$hits" ] || continue
    if grep -qxF -- "$file" <<<"$(printf '%s\n' "${HISTORY[@]}")"; then
        used="${used:+$used
}$file"
        continue
    fi
    while IFS= read -r hit; do
        [ -n "$hit" ] || continue
        findings=$((findings + 1))
        echo "$NAME: $file:$hit" >&2
    done <<EOT
$hits
EOT
done <<EOT
$files
EOT

if [ "$findings" -gt 0 ]; then
    cat >&2 <<'MSG'

FAIL — the line(s) above name a GitHub path outside the algedonic-dev
organisation (backlog d2b7c947, David 2026-09-30: all GitHub work runs
through the organisation, as the GitHub App). A remote is
https://github.com/algedonic-dev/<repo>; a token is an installation
token the credential broker mints for the organisation App
(infra/forge/github-act.sh, publish-github-pr.sh); a commit is authored
as the publisher the verb file declares. A read of someone else's
public release or page is not a remote and passes.
MSG
    exit 1
fi

allowlist_entries_used "$NAME" "$used" "${HISTORY[@]}"
lint_scanned "$NAME" "$scanned" "file(s) under ${SCANNED_DIRS[*]} read"
echo "$NAME: ok — every GitHub path under ${SCANNED_DIRS[*]} is $ORG's"
exit 0
