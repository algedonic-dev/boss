#!/usr/bin/env bash
# a-manifest-sender-presents-the-machine-token.sh — shell written INSIDE
# a Kubernetes manifest that signs a request with `x-boss-user` stamps
# the estate machine token on it, through the tree's one reader; and the
# one manifest that does so today, the cluster estate observer, is RUN
# here across every state the token can be in, under every shell this
# box has, and never fails or sends anything different because of it.
#
# WHY THIS EXISTS (backlog 066a7613; urgent 2710c8fc; David's decision
# on design-doc bdc60b65, question `observer`, 2026-10-06). The machine
# door's gate refuses, once a port is set to `enforce`, any request with
# no estate token. The shell senders are held to the reader by a pin
# that walks SCRIPTS; the Rust ones by
# every_x_boss_user_sender_stamps_the_machine_token.rs. Neither can see
# a curl typed into a manifest's `args:` block scalar, and that is where
# the observer's four jobs-API routes live: 1,124 would-refuse facts
# from 207 peers in the 72 h to 2026-10-06T17:41Z, from the one pod that
# records what nodes exist and settles a gate runner that died with its
# node. Enforce the door with that pod bare and both go dark.
#
# WHAT IT HOLDS, in three parts.
#
#   A. THE PIN (every manifest). In infra/cluster/manifests/*.yaml and
#      infra/gate-runner/*.yaml, every command that carries `x-boss-user`
#      is sent through `api_curl`, and that file's `api_curl` hands curl
#      a header FILE made by `machine_token_header` — or the command
#      names the header variable itself — or the file is listed in
#      NOT_STAMPED below with why. A row whose file no longer needs it is
#      refused as stale.
#
#   B. THE GRANT'S CONDITIONS (the observer's manifest). David allowed
#      the Secret mount after two changes in the same car, and both are
#      read off the file: every image is either the boss image (whose
#      tag the converge rewrites) or a forge-mirror ref PINNED BY DIGEST
#      that infra/forge/mirror-base-images.sh's list carries; the reader
#      is COPIED from the boss image, from the path the Dockerfile puts
#      infra/lib/secret-header.sh at, and is not pasted into the
#      manifest; the Secret is mounted read-only and optional, on the
#      `observe` container only; and the init container that copies the
#      reader cannot fail the pod.
#
#   C. THE RUN (the observer's script, executed). The extraction is
#      a-cluster-node-reports-its-headroom.sh's, byte for byte. Under
#      stub `kubectl` and `curl`, with the TREE's reader and a fixture
#      token:
#        - a valid token: every jobs-API request carries the header, as
#          a 0600 FILE whose one line is the token header; the
#          dispatcher's /readyz read, which signs nothing, carries none;
#          the token is in no argv and no output; the header file is
#          gone when the script exits; every jobs-API send has a `-m`.
#        - every other state — no mount, an empty mount, a blank slot, a
#          line break, a carriage return, an oversized slot, an
#          unreadable slot, the mount a file, the slot a directory, the
#          slot a FIFO, a host the token may not go to, no reader, a
#          reader that does not parse, a reader with no
#          machine_token_header, a header file that cannot be written:
#          THE SAME REQUESTS, THE SAME EXIT STATUS, NO HEADER. A monitor
#          that fails because of a credential is an outage; one that
#          sends something different is a second system.
#        - and when the observation POST is refused, the script exits 1
#          with a token exactly as it does without one, and still
#          leaves no header file — the EXIT trap the reader chains must
#          not eat the status.
#      Under `bash`, under `sh` (dash on the hosts and in the gate
#      image), and under `busybox sh` when a busybox is on PATH — the
#      pod's /bin/sh is busybox ash, and "works under dash" is not
#      "works under ash". The closing line names the shells that ran.
#
# HERMETIC (infra/lint/a-lint-sources-a-header-lib-only-hermetic.sh, the
# 2026-10-01 incident): this lint never sources the reader into its own
# shell — the script under test does, in a child — and every child is
# handed its token directory, host list and sor.env by name. Nothing a
# host holds is read: not its token, not its address.
set -uo pipefail

NAME="a-manifest-sender-presents-the-machine-token"
cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
# shellcheck source=infra/lint/lib/scanned.sh
. infra/lint/lib/scanned.sh || exit 3

OBSERVER="infra/cluster/manifests/boss-estate-observe.yaml"
READER_SRC="infra/lib/secret-header.sh"
DOCKERFILE="infra/oss-quickstart/Dockerfile"
MIRROR="infra/forge/mirror-base-images.sh"
for f in "$OBSERVER" "$READER_SRC" "$DOCKERFILE" "$MIRROR" infra/estate/estate.toml; do
    [ -f "$f" ] || { echo "$NAME: missing $f" >&2; exit 1; }
done
command -v jq >/dev/null 2>&1 || { echo "$NAME: no jq — the observer is sh + jq, so this check cannot run without it" >&2; exit 1; }

fail() { echo "$NAME: FAIL — $*" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'chmod -R u+rwx "$tmp" 2>/dev/null; rm -rf "$tmp"' EXIT
# No token in reach of anything this lint starts, whatever the host holds.
mkdir -p "$tmp/no-machine-token"
export BOSS_MACHINE_TOKEN_DIR="$tmp/no-machine-token"
export BOSS_SOR_ENV="$tmp/no-sor.env"
unset BOSS_JOBS_URL BOSS_MACHINE_TOKEN_HOSTS RUNTIME_DIRECTORY BOSS_OBSERVE_READER

# Files that send x-boss-user from manifest-embedded shell WITHOUT the
# token, and why — `<path><TAB><reason>`. Empty, and meant to stay so: a
# new row is a caller the enforce flip refuses.
NOT_STAMPED=()

# ---------------------------------------------------------------------
# A. the pin
# ---------------------------------------------------------------------
# scan_senders FILE... — one line per finding, `<file>:<line>: <what>`.
# A comment line is prose; a command is its lines joined across trailing
# backslashes, so the header on line 3 of a curl is read with its line 1.
scan_senders() {
    local f
    for f in "$@"; do
        LC_ALL=C awk '
            function flush(    c) {
                c = cmd; cmd = ""
                if (c == "" || c !~ /[xX]-[bB][oO][sS][sS]-[uU][sS][eE][rR]/) return
                senders++
                if (c ~ /\$\{?MT_HDR/) return
                if (c ~ /(^|[^A-Za-z0-9_])api_curl[ \t]/) { via++; return }
                print FILENAME ":" start ": sends x-boss-user on a curl that is neither api_curl nor handed the machine-token header file"
            }
            {
                line = $0
                if (line ~ /^[ \t]*#/) next
                if (line ~ /^[ \t]*api_curl\(\)[ \t]*\{/) { infn = 1; fnstart = FNR }
                if (infn) {
                    if (line ~ /MT_HDR/) fn_names = 1
                    if (line ~ /curl[ \t]/ && line ~ /\$\{[A-Za-z_][A-Za-z0-9_]*:\+-H "\$[A-Za-z_][A-Za-z0-9_]*"\}/) fn_hands = 1
                    if (line ~ /^[ \t]*\}[ \t]*$/) infn = 0
                }
                if (line ~ /(^|[ \t;&|({])machine_token_header[ \t]+MT_HDR[ \t]/) called = 1
                if (cmd == "") start = FNR
                cmd = cmd " " line
                if (line !~ /\\[ \t]*$/) flush()
            }
            END {
                flush()
                if (via) {
                    if (!fnstart) print FILENAME ":1: sends x-boss-user through api_curl, but defines no api_curl() to stamp it"
                    else if (!fn_names || !fn_hands) print FILENAME ":" fnstart ": api_curl() does not hand curl the machine-token header file (${hdr:+-H \"$hdr\"}, from MT_HDR)"
                    if (!called) print FILENAME ":1: sends x-boss-user through api_curl, but never calls `machine_token_header MT_HDR <url>` — MT_HDR is always empty"
                }
            }
        ' "$f"
    done
}

# Self-test first: a scanner that cannot see the bad fixture passes any tree.
mkdir -p "$tmp/pin"
cat > "$tmp/pin/good.yaml" <<'EOF'
                - |
                  # curl -H "x-boss-user: prose" is not a sender
                  machine_token_header MT_HDR "$JOBS_API" || MT_HDR=""
                  api_curl() {
                    local hdr="$MT_HDR"
                    out=$(curl -m 60 ${hdr:+-H "$hdr"} "$@")
                  }
                  resp=$(api_curl -s -X POST \
                    -H "x-boss-user: $WHO" \
                    "$JOBS_API/api/x")
EOF
cat > "$tmp/pin/direct.yaml" <<'EOF'
                  machine_token_header MT_HDR "$JOBS_API" || MT_HDR=""
                  curl -s ${MT_HDR:+-H "$MT_HDR"} -H "x-boss-user: $WHO" "$JOBS_API/api/x"
EOF
cat > "$tmp/pin/bare.yaml" <<'EOF'
                  machine_token_header MT_HDR "$JOBS_API" || MT_HDR=""
                  api_curl() { :; }
                  curl -s -X PUT \
                    -H "X-Boss-User: $WHO" \
                    "$JOBS_API/api/x"
EOF
cat > "$tmp/pin/unstamped-fn.yaml" <<'EOF'
                  machine_token_header MT_HDR "$JOBS_API" || MT_HDR=""
                  api_curl() {
                    out=$(curl "$@")
                  }
                  api_curl -H "x-boss-user: $WHO" "$JOBS_API/api/x"
EOF
cat > "$tmp/pin/never-called.yaml" <<'EOF'
                  api_curl() {
                    local hdr="$MT_HDR"
                    out=$(curl ${hdr:+-H "$hdr"} "$@")
                  }
                  api_curl -H "x-boss-user: $WHO" "$JOBS_API/api/x"
EOF
got="$(scan_senders "$tmp/pin/good.yaml" "$tmp/pin/direct.yaml")"
[ -z "$got" ] || fail "the pin's self-test: the stamped fixtures were refused: $got"
got="$(scan_senders "$tmp/pin/bare.yaml" "$tmp/pin/unstamped-fn.yaml" "$tmp/pin/never-called.yaml" | sed "s|^$tmp/pin/||")"
want="bare.yaml:3: sends x-boss-user on a curl that is neither api_curl nor handed the machine-token header file
unstamped-fn.yaml:2: api_curl() does not hand curl the machine-token header file (\${hdr:+-H \"\$hdr\"}, from MT_HDR)
never-called.yaml:1: sends x-boss-user through api_curl, but never calls \`machine_token_header MT_HDR <url>\` — MT_HDR is always empty"
[ "$got" = "$want" ] || fail "the pin's self-test: a bare curl, an api_curl that hands curl nothing and a header nobody made must each be refused by name and line; got:
$got"

manifests=(infra/cluster/manifests/*.yaml infra/gate-runner/*.yaml)
[ -f "${manifests[0]}" ] || fail "no manifests found under infra/cluster/manifests"
findings="$(scan_senders "${manifests[@]}")"
unlisted=""; stale=""
while IFS= read -r line; do
    [ -n "$line" ] || continue
    file="${line%%:*}"; listed=""
    for row in ${NOT_STAMPED[@]+"${NOT_STAMPED[@]}"}; do [ "${row%%$'\t'*}" = "$file" ] && listed=1; done
    [ -n "$listed" ] || unlisted="$unlisted$line"$'\n'
done <<< "$findings"
for row in ${NOT_STAMPED[@]+"${NOT_STAMPED[@]}"}; do
    grep -q "^${row%%$'\t'*}:" <<< "$findings" || stale="$stale${row%%$'\t'*}"$'\n'
done
if [ -n "$unlisted" ]; then
    echo "$NAME: shell inside a manifest sends x-boss-user without the machine token:" >&2
    printf '%s' "$unlisted" | sed 's/^/  /' >&2
    cat >&2 <<'MSG'

  The machine door refuses a request with no estate token the day its
  port enforces, and no other pin reads a curl typed into a manifest.
  Send it the way infra/cluster/manifests/boss-estate-observe.yaml does:
  the reader copied out of the boss image by an init container, one
  `machine_token_header MT_HDR "$JOBS_API"` at the top level, and every
  request through an api_curl that hands curl ${hdr:+-H "$hdr"}. A new
  Secret mount is a grant — it needs a decision, not only a diff.
MSG
    exit 1
fi
[ -z "$stale" ] || fail "NOT_STAMPED lists file(s) that no longer send x-boss-user unstamped — delete the row(s): $(printf '%s' "$stale" | tr '\n' ' ')"

# ---------------------------------------------------------------------
# B. the grant's conditions, read off the observer's manifest
# ---------------------------------------------------------------------
REGISTRY=$(sed -n 's/^forge_registry = "\(.*\)"[[:space:]]*$/\1/p' infra/estate/estate.toml | sed -n 1p)
[ -n "$REGISTRY" ] || fail "infra/estate/estate.toml names no forge_registry"
FORGE_BASE="$REGISTRY/david"
mirrored="$(BOSS_FORGE_REGISTRY_BASE="$FORGE_BASE" bash "$MIRROR" --check 2>/dev/null \
    | awk -v base="$FORGE_BASE/" 'index($0, "  ->  ") { r = $NF; if (index(r, base) == 1) print substr(r, length(base) + 1) }')"
[ -n "$mirrored" ] || fail "$MIRROR --check listed no mirrored image — the list cannot be read"

# check_images FILE — every image: is the boss image or a mirrored ref
# pinned by digest. Prints findings.
check_images() {
    local n=0 ref rest tagged
    while IFS= read -r ref; do
        [ -n "$ref" ] || continue
        n=$((n + 1))
        case "$ref" in
            "$FORGE_BASE"/boss:*) ;;
            "$FORGE_BASE"/*@sha256:*)
                rest="${ref#"$FORGE_BASE"/}"; tagged="${rest%@*}"
                [[ "${rest##*@sha256:}" =~ ^[0-9a-f]{64}$ ]] \
                    || echo "$ref: the digest is not sha256 and 64 hex characters"
                grep -qxF -- "$tagged" <<< "$mirrored" \
                    || echo "$ref: $tagged is not a destination in $MIRROR's IMAGES list — nothing puts that image in the forge registry"
                ;;
            *) echo "$ref: not the boss image and not a forge-mirror ref pinned by digest (@sha256:…) — a tag is whatever its registry serves today, on a pod that holds the machine token and a cluster-wide node read" ;;
        esac
    done < <(awk '/^[[:space:]]*#/ { next }
                  match($0, /(^|[[:space:]{])image:[[:space:]]*/) {
                      line = substr($0, RSTART + RLENGTH); sub(/[,}].*$/, "", line)
                      sub(/[[:space:]]+#.*$/, "", line); gsub(/["\047]/, "", line); sub(/[[:space:]]+$/, "", line)
                      if (line != "") print line }' "$1")
    [ "$n" -gt 0 ] || echo "no image: key was read"
}
printf '%s\n' '          - image: alpine/k8s:1.33.3' "          - image: $FORGE_BASE/alpine-k8s:1.33.3" \
    "          - image: $FORGE_BASE/not-mirrored:1@sha256:$(printf '%064d' 0)" > "$tmp/bad-images.yaml"
[ "$(check_images "$tmp/bad-images.yaml" | wc -l)" -eq 3 ] \
    || fail "the image check's self-test: a public tag, a mirror tag with no digest and an unmirrored digest must each be refused"
out="$(check_images "$OBSERVER")"
[ -z "$out" ] || fail "$OBSERVER: $out"

# The manifest's CODE: comment lines dropped, so prose cannot satisfy or
# fail what follows.
code="$(grep -v '^[[:space:]]*#' "$OBSERVER")"
# Where the image carries the reader, from the Dockerfile's own COPY —
# the lib directory beside the chore helpers, never the lint copy.
in_image="$(awk '$1 == "COPY" && $2 == "'"$READER_SRC"'" && $3 ~ /^\/usr\/local\/bin\// { print $3; exit }' "$DOCKERFILE")"
[ -n "$in_image" ] || fail "$DOCKERFILE no longer COPYs $READER_SRC under /usr/local/bin — the init container would copy a file the image does not hold"
grep -qF -- "cp $in_image /reader/secret-header.sh" <<< "$code" \
    || fail "$OBSERVER: no init container copies $in_image (the image's copy of $READER_SRC) to /reader/secret-header.sh"
grep -qE 'cp /usr/local/bin/lib/secret-header\.sh /reader/secret-header\.sh \|\| ' <<< "$code" \
    || fail "$OBSERVER: the reader's copy can fail the init container — a non-zero init container holds the pod in Init and the observation never runs; a failed copy must say so and exit 0"
grep -qF -- '{name: reader, mountPath: /reader, readOnly: true}' <<< "$code" \
    || fail "$OBSERVER: the observe container does not mount the reader read-only at /reader"
grep -qE '(^|[^A-Za-z0-9_])(machine_token_header|secret_header|_secret_header_[a-z_]+)\(\)' <<< "$code" \
    && fail "$OBSERVER: defines the reader's functions inline — a pasted copy of $READER_SRC is the second definition that drifts; copy it from the boss image"
grep -qF -- 'secret: {secretName: boss-machine-token, optional: true, defaultMode: 0444}' <<< "$code" \
    || fail "$OBSERVER: Secret boss-machine-token is not mounted optional — a required Secret that is absent holds the pod in ContainerCreating"
[ "$(grep -cF -- '{name: machine-token, mountPath: /etc/boss/machine-token, readOnly: true}' <<< "$code")" -eq 1 ] \
    || fail "$OBSERVER: the machine token must be mounted exactly once, read-only, at /etc/boss/machine-token — on the observe container and no other"
# …and that one mount is BELOW the observe container's script, not on the
# init container, which runs before it and needs no token.
mount_line=$(grep -nF -- '{name: machine-token, mountPath: /etc/boss/machine-token' "$OBSERVER" | cut -d: -f1 | sed -n 1p)
script_line=$(grep -nF -- 'kubectl get nodes -o json' "$OBSERVER" | cut -d: -f1 | sed -n 1p)
[ -n "$mount_line" ] && [ -n "$script_line" ] && [ "$mount_line" -gt "$script_line" ] \
    || fail "$OBSERVER: the machine-token mount is not on the observe container (it sits above the observe script)"
grep -qE 'emptyDir: \{medium: Memory' <<< "$code" && grep -qF -- 'mountPath: /run/boss-secret-header' <<< "$code" \
    && awk '/- name: RUNTIME_DIRECTORY$/ { if ((getline nxt) > 0 && index(nxt, "value: /run/boss-secret-header")) ok = 1 } END { exit ok ? 0 : 1 }' <<< "$code" \
    || fail "$OBSERVER: the header file must live in a memory emptyDir the reader is pointed at (RUNTIME_DIRECTORY) — /tmp is the container's layer, which outlives a killed tick on the node's disk"

# ---------------------------------------------------------------------
# C. the run
# ---------------------------------------------------------------------
awk '
    { match($0, /^ */); ind = RLENGTH; body = substr($0, ind + 1) }
    ind == 14 && body == "args:"          { inargs = 1; next }
    inargs && ind == 16 && body == "- |"  { inblock = 1; inargs = 0; next }
    inblock {
        if ($0 ~ /^[ \t]*$/) { print ""; next }
        if (ind < 18) { inblock = 0; next }
        print substr($0, 19)
    }
' "$OBSERVER" >"$tmp/observe.sh"
[[ -s "$tmp/observe.sh" ]] || fail "could not extract the args: block scalar from $OBSERVER (indentation changed?)"
grep -q 'estate/observation' "$tmp/observe.sh" \
    || fail "the extracted block does not post an observation — the scraper found the wrong block"

# Fixtures: one node; one gate Job that died with its packet open, so the
# pass reads a packet, merges a verdict and flips a step; no dev pods and
# no evictions. That is every jobs-API route the observer has.
DEAD=610d715e-48f3-4f2f-9269-cffddcb84ca0
STEP=bfdc7ff5-0000-4000-8000-000000000002
cat >"$tmp/nodes.json" <<'JSON'
{"items":[{"metadata":{"name":"w-1","labels":{"boss.dev/purpose":"build"}},
  "status":{"addresses":[{"type":"InternalIP","address":"192.0.2.21"}],
            "capacity":{"cpu":"32","memory":"131497404Ki","ephemeral-storage":"974168604Ki"},
            "conditions":[{"type":"Ready","status":"True"}]}}]}
JSON
cat >"$tmp/gate-jobs.json" <<JSON
{"items":[{"metadata":{"name":"gate-docs-a-probe-shape-f-x8c5q","labels":{"app":"gate-runner","boss.dev/packet":"$DEAD"}},
  "status":{"failed":1,"conditions":[{"type":"Failed","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit","lastTransitionTime":"2026-09-11T19:20:30Z"}]}}]}
JSON
cat >"$tmp/packet-$DEAD.json" <<JSON
{"id":"$DEAD","kind":"gate-run","status":"open","steps":[{"id":"$STEP","kind":"gate-verdict","status":"ready","metadata":{}}]}
JSON

mkdir -p "$tmp/bin"
cat >"$tmp/bin/kubectl" <<'STUB'
#!/usr/bin/env bash
case "${1:-} ${2:-}" in
    "get nodes") cat "$FIXTURES/nodes.json" ;;
    "get jobs") cat "$FIXTURES/gate-jobs.json" ;;
    "get pods" | "get events") printf '{"items":[]}\n' ;;
    *) echo "kubectl stub: unexpected args: $*" >&2; exit 99 ;;
esac
STUB
# curl: one line per request — method, url, what it was stamped with,
# whether it was bounded, body — and every argv, whole, in a second file.
# `stamped` means: `-H @FILE`, FILE mode 600, its content exactly the one
# header line the fixture token makes.
cat >"$tmp/bin/curl" <<'STUB'
#!/usr/bin/env bash
method=GET; url=""; body=""; prev=""; want_code=""; stamp=bare; bound=unbounded
printf '%s\n' "$*" >>"$ARGV"
for a in "$@"; do
    case "$prev" in
        -X) method="$a" ;;
        --data-binary) if [[ "$a" == @* ]]; then body="$(tr '\n' ' ' <"${a#@}")"; else body="${a//$'\n'/ }"; fi ;;
        -w) want_code=1 ;;
        -m) bound=bounded ;;
        -H) if [[ "$a" == @* ]]; then
                f="${a#@}"
                if [[ ! -f "$f" ]]; then stamp="header-file-missing"
                elif [[ "$(stat -c %a "$f")" != 600 ]]; then stamp="header-file-mode-$(stat -c %a "$f")"
                elif ! cmp -s "$f" "$EXPECT_HEADER_FILE"; then stamp="header-file-wrong-content"
                else stamp=stamped; fi
            fi ;;
    esac
    [[ "$a" == http* ]] && url="$a"
    prev="$a"
done
printf '%s\t%s\t%s\t%s\t%s\n' "$method" "$url" "$stamp" "$bound" "$body" >>"$LOG"
case "$method $url" in
    "GET "*/api/dispatcher/readyz) printf '{"ready":true,"dead_letters":0,"dead_letters_unrecorded":0,"last_dead_letter_unix":null,"last_unrecorded_dead_letter_unix":null}' ;;
    "POST "*/api/estate/observation)
        if [[ -n "${POST_REFUSED:-}" ]]; then printf '{"error":"refused"}\n500'; else printf '{"recorded":true}\n202'; fi ;;
    "GET "*scope=*) printf '{"data":[],"total":0}' ;;
    "GET "*/api/jobs\?*) printf '{"data":[],"total":0}' ;;
    "GET "*/api/jobs/*) cat "$FIXTURES/packet-${url##*/api/jobs/}.json" ;;
    "PATCH "*/steps/*/metadata) [[ -n "$want_code" ]] && printf '204' ;;
    "PUT "*/steps/*) [[ -n "$want_code" ]] && printf '204' ;;
    *) printf '{"error":"stub has no answer for %s %s"}' "$method" "$url"; [[ -n "$want_code" ]] && printf '\n500' ;;
esac
exit 0
STUB
chmod +x "$tmp/bin/kubectl" "$tmp/bin/curl"

# Short on purpose: a fixture, and no-secrets.sh reads 32+ characters
# after such a name as a credential.
TOKEN="fixture-not-a-credential"
JOBS="http://jobs.stub.test:7900"
# The one line a stamped request's header file must hold — written to a
# file, as the reader writes it, and compared by the stub byte for byte.
printf 'x-boss-machine-token: %s\n' "$TOKEN" >"$tmp/expect-header"

# The reader fixtures a pod could find at /reader.
printf 'if then ((( this is not shell\n' >"$tmp/reader-garbage.sh"
printf 'secret_header() { eval "$1="; }\n' >"$tmp/reader-old.sh"

# token_dir STATE — make the machine-token directory for one state under
# $tmp/tok-STATE and print its path.
token_dir() {
    local d="$tmp/tok-$1"
    rm -rf "$d" 2>/dev/null
    case "$1" in
        absent) ;;
        empty) mkdir -p "$d" ;;
        blank) mkdir -p "$d"; printf '\n   \n' >"$d/current" ;;
        linebreak) mkdir -p "$d"; printf 'abc\ndef\n' >"$d/current" ;;
        carriage) mkdir -p "$d"; printf 'abc\rdef\n' >"$d/current" ;;
        oversized) mkdir -p "$d"; head -c 5000 /dev/zero | tr '\0' 'a' >"$d/current" ;;
        unreadable) mkdir -p "$d"; printf '%s\n' "$TOKEN" >"$d/current"; chmod 000 "$d/current" ;;
        mount-is-a-file) printf '%s\n' "$TOKEN" >"$d" ;;
        slot-is-a-directory) mkdir -p "$d/current" ;;
        slot-is-a-fifo) mkdir -p "$d"; mkfifo "$d/current" ;;
        *) mkdir -p "$d"; printf '%s\n' "$TOKEN" >"$d/current" ;;
    esac
    printf '%s' "$d"
}

# run SHELLTAG STATE [POST_REFUSED] — run the observer once. Leaves
# $tmp/r-<tag>-<state>[-refused]/{log,argv,out,rc,hdr,tmp}.
run() {
    local tag="$1" state="$2" refused="${3:-}" r tok reader hosts=".stub.test" tmpdir runtime
    r="$tmp/r-$tag-$state${refused:+-refused}"
    mkdir -p "$r/work" "$r/hdr" "$r/tmp"
    : >"$r/log"; : >"$r/argv"
    tok="$(token_dir "$state")"
    reader="$PWD/$READER_SRC"; tmpdir="$r/tmp"; runtime="$r/hdr"
    case "$state" in
        off-host) hosts="elsewhere.test" ;;
        no-reader) reader="$tmp/there-is-no-reader.sh" ;;
        garbage-reader) reader="$tmp/reader-garbage.sh" ;;
        old-reader) reader="$tmp/reader-old.sh" ;;
        # No runtime directory, and TMPDIR a regular file: the header
        # directory cannot be made, as root or not.
        header-unwritable) runtime=""; : >"$r/not-a-directory"; tmpdir="$r/not-a-directory" ;;
    esac
    # shellcheck disable=SC2086  # the shell is a command line: `busybox sh`
    ( cd "$r" && env LOG="$r/log" ARGV="$r/argv" FIXTURES="$tmp" PATH="$tmp/path-$tag:$tmp/bin:$PATH" \
        EXPECT_HEADER_FILE="$tmp/expect-header" POST_REFUSED="$refused" \
        JOBS_API="$JOBS" DISPATCHER_API="http://dispatcher.stub.test:7950" \
        BOSS_OBSERVE_WORK="$r/work" BOSS_OBSERVE_READER="$reader" \
        BOSS_MACHINE_TOKEN_DIR="$tok" BOSS_MACHINE_TOKEN_HOSTS="$hosts" BOSS_SOR_ENV="$tmp/no-sor.env" \
        TMPDIR="$tmpdir" ${runtime:+RUNTIME_DIRECTORY="$runtime"} \
        ${SHELLS[$tag]} "${OBS_SCRIPT:-$tmp/observe.sh}" >"$r/out" 2>&1 </dev/null
      echo $? >"$r/rc" )
    printf '%s' "$r"
}

# requests DIR — what was sent, with the one field that differs by clock
# removed: method, url, body.
requests() { cut -f1,2,5 "$1/log" | sed -E 's/"(observed_at|at)":[[:space:]]*"[^"]*"//g'; }
leftovers() { find "$1/hdr" "$1/tmp" -mindepth 1 2>/dev/null | sed -n '1,5p'; }

# The shells. `sh` is dash on the hosts and in the gate image; busybox is
# the pod's own, when this box has one. Each gets a PATH directory: empty,
# except busybox's, where `sh` and the applets the reader and the script
# call are busybox's too, as they are in the pod.
declare -A SHELLS=([bash]="bash")
order=(bash)
mkdir -p "$tmp/path-bash"
if command -v sh >/dev/null 2>&1; then
    SHELLS[sh]="sh"; order+=(sh); mkdir -p "$tmp/path-sh"
fi
if command -v busybox >/dev/null 2>&1 && busybox sh -c 'exit 0' 2>/dev/null; then
    SHELLS[busybox]="busybox sh"; order+=(busybox); mkdir -p "$tmp/path-busybox"
    applets="$(busybox --list 2>/dev/null)"
    for applet in sh sed wc tr mktemp rm chmod cat head tail date mkdir cp sleep; do
        grep -qx "$applet" <<< "$applets" || continue
        printf '#!/bin/sh\nexec busybox %s "$@"\n' "$applet" >"$tmp/path-busybox/$applet"
        chmod +x "$tmp/path-busybox/$applet"
    done
fi

STATES=(absent empty blank linebreak carriage oversized unreadable mount-is-a-file
        slot-is-a-directory slot-is-a-fifo off-host no-reader garbage-reader old-reader header-unwritable)
runs=0
for tag in "${order[@]}"; do
    sh_name="${SHELLS[$tag]}"

    # The reference: no reader at all — the pod as it was before this car.
    base="$(run "$tag" no-reader)"; runs=$((runs + 1))
    [ "$(cat "$base/rc")" = 0 ] || { cat "$base/out" >&2; fail "[$sh_name] the observer exited $(cat "$base/rc") with no reader — the fixtures or the script are broken"; }
    n_jobs=$(grep -c "	$JOBS/" "$base/log")
    [ "$n_jobs" -ge 6 ] || { cat "$base/log" "$base/out" >&2; fail "[$sh_name] the pass made $n_jobs jobs-API request(s); the fixtures should drive at least six (observations read, observation, packet, merge, flip, agent-runs)"; }
    for route in "GET	$JOBS/api/estate/observations?scope=talos-nodefs" "POST	$JOBS/api/estate/observation	" \
                 "GET	$JOBS/api/jobs/$DEAD	" "PATCH	$JOBS/api/jobs/$DEAD/steps/$STEP/metadata	" \
                 "PUT	$JOBS/api/jobs/$DEAD/steps/$STEP	" "GET	$JOBS/api/jobs?kind=agent-run"; do
        grep -qF -- "$route" "$base/log" || { cat "$base/log" >&2; fail "[$sh_name] the pass did not send ${route%%	*} ${route#*	} — a route this check never drives is a route it never judges"; }
    done
    grep -q "^GET	http://dispatcher.stub.test:7950/api/dispatcher/readyz	" "$base/log" \
        || fail "[$sh_name] the dispatcher's readyz was not read"
    unbounded=$(grep "	$JOBS/" "$base/log" | grep -c "	unbounded	")
    [ "$unbounded" -eq 0 ] || { cat "$base/log" >&2; fail "[$sh_name] $unbounded jobs-API send(s) carry no -m: a connect that is accepted and never answered holds the tick open, and concurrencyPolicy Forbid skips every tick behind it"; }

    # A valid token: every jobs-API request stamped, nothing else touched.
    ok="$(run "$tag" valid)"; runs=$((runs + 1))
    [ "$(cat "$ok/rc")" = 0 ] || { cat "$ok/out" >&2; fail "[$sh_name] the observer exited $(cat "$ok/rc") with a valid token mounted"; }
    [ "$(requests "$ok")" = "$(requests "$base")" ] \
        || { diff <(requests "$base") <(requests "$ok") >&2; fail "[$sh_name] a valid token changed WHAT the observer sends — the token is a header, not a behaviour"; }
    bad=$(grep "	$JOBS/" "$ok/log" | grep -vc "	stamped	")
    [ "$bad" -eq 0 ] || { cut -f1-4 "$ok/log" >&2; cat "$ok/out" >&2; fail "[$sh_name] $bad jobs-API request(s) went out without the machine-token header as a 0600 file holding the one header line — the machine door refuses each the day it enforces"; }
    [ "$(grep "	http://dispatcher.stub.test" "$ok/log" | grep -c "	bare	")" -ge 1 ] \
        || { cut -f1-4 "$ok/log" >&2; fail "[$sh_name] the dispatcher's readyz read was stamped — it signs nothing and is sent no token; only a \$JOBS_API request carries it"; }
    grep -qF -- "$TOKEN" "$ok/argv" && fail "[$sh_name] the token is in a curl argv — a command line is world-readable while the process runs"
    grep -qF -- "$TOKEN" "$ok/out" && fail "[$sh_name] the token is in the observer's output — pod logs are kept and read"
    [ -z "$(leftovers "$ok")" ] || fail "[$sh_name] a header file outlived the pass: $(leftovers "$ok")"
    grep -q 'machine token: presented' "$ok/out" || { cat "$ok/out" >&2; fail "[$sh_name] a stamped pass does not say so — the first stamped run is read off the pod's log"; }

    # api_curl aimed at a host that is not \$JOBS_API: no call does that
    # today, so one is appended to the script. The header was judged for
    # one host and goes to no other.
    { cat "$tmp/observe.sh"; printf '%s\n' 'api_curl -s "http://elsewhere.example.test/api/x" >/dev/null || true'; } >"$tmp/observe-elsewhere.sh"
    el="$(OBS_SCRIPT="$tmp/observe-elsewhere.sh" run "$tag" valid-elsewhere)"; runs=$((runs + 1))
    [ "$(grep "	http://elsewhere.example.test/api/x	" "$el/log" | grep -c "	bare	")" -ge 1 ] \
        || { cut -f1-4 "$el/log" >&2; fail "[$sh_name] api_curl sent the machine-token header to a host that is not \$JOBS_API"; }
    [ "$(grep "	$JOBS/" "$el/log" | grep -c "	stamped	")" -eq "$n_jobs" ] \
        || { cut -f1-4 "$el/log" >&2; fail "[$sh_name] the jobs-API requests of that same pass were not all stamped"; }

    # Every other state: the same requests, the same exit, no header.
    # Run together (each has its own directories), judged in order.
    for state in "${STATES[@]}"; do run "$tag" "$state" >/dev/null & done
    wait
    for state in "${STATES[@]}"; do
        r="$tmp/r-$tag-$state"; runs=$((runs + 1))
        [ -s "$r/rc" ] || fail "[$sh_name] token state '$state': the run left no exit status"
        [ "$(cat "$r/rc")" = "$(cat "$base/rc")" ] \
            || { cat "$r/out" >&2; fail "[$sh_name] token state '$state': the observer exited $(cat "$r/rc"), and $(cat "$base/rc") without a reader — it must never fail because of the token"; }
        [ "$(requests "$r")" = "$(requests "$base")" ] \
            || { diff <(requests "$base") <(requests "$r") >&2; cat "$r/out" >&2; fail "[$sh_name] token state '$state': the observer sent different requests than it does with no reader"; }
        # Root reads a mode-000 file, so there the slot is simply a token.
        if [ "$state" = unreadable ] && [ "$(id -u)" = 0 ]; then
            continue
        fi
        stamped=$(grep -vc "	bare	" "$r/log")
        [ "$stamped" -eq 0 ] || { cut -f1-4 "$r/log" >&2; fail "[$sh_name] token state '$state': $stamped request(s) carried a header — nothing may be sent for a token that is not one, or to a host that is not the estate's"; }
        grep -q 'machine token: not presented' "$r/out" || { cat "$r/out" >&2; fail "[$sh_name] token state '$state': a bare pass does not say it went out bare"; }
        [ -z "$(leftovers "$r")" ] || fail "[$sh_name] token state '$state': a header file outlived the pass: $(leftovers "$r")"
    done

    # A refused observation: exit 1 with a token exactly as without one,
    # and the EXIT trap still removes the header file.
    b="$(run "$tag" no-reader refused)"; t="$(run "$tag" valid refused)"; runs=$((runs + 2))
    [ "$(cat "$b/rc")" = 1 ] || { cat "$b/out" >&2; fail "[$sh_name] a refused observation exited $(cat "$b/rc") without a reader — it must fail loudly"; }
    [ "$(cat "$t/rc")" = "$(cat "$b/rc")" ] \
        || { cat "$t/out" >&2; fail "[$sh_name] a refused observation exited $(cat "$t/rc") with a token and $(cat "$b/rc") without — the reader's EXIT trap changed the observer's exit status"; }
    [ "$(requests "$t")" = "$(requests "$b")" ] || fail "[$sh_name] a refused observation sent different requests with a token"
    [ -z "$(leftovers "$t")" ] || fail "[$sh_name] a refused observation left the header file behind: $(leftovers "$t")"
done

ran="$(printf '%s, ' "${order[@]}")"; ran="${ran%, }"
case " ${order[*]} " in
    *" busybox "*) ash="busybox ash, the pod's shell, among them" ;;
    *) ash="NOT under busybox ash — this box has no busybox; the pod's /bin/sh is one" ;;
esac
lint_scanned "$NAME" "${#manifests[@]}" "manifest(s) for an x-boss-user sender, and $runs run(s) of the observer's script under: $ran ($ash)"
echo "$NAME: ok — no manifest sends x-boss-user bare; the observer's images are the boss image and a mirrored digest, its reader is the image's copy, its token mount is read-only, optional and the observe container's alone; and under each shell a valid token stamps every jobs-API request as a 0600 header file while ${#STATES[@]} other token states send the same requests with the same exit status and no header"
exit 0
