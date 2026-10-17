#!/usr/bin/env bash
#
# install-cli-from-image — put the tree's `boss` CLI on this host, taken
# out of the cluster image built for the commit the host converged to.
#
# ONE INSTALLER FOR EVERY MANAGED HOST (backlog 9f00a805, consolidation
# H8, car 1, 2026-09-18). Written for boss-gcp under infra/gcp/, it
# lives here — beside roles.toml and node-roles.sh, what a host reads
# to know what it is for — because the forge needed the same thing.
# Measured on #448: infra/forge/*.sh is 32 scripts, 9,790 lines, the
# largest of them shell twins of CLI verbs (run-car-probe.sh for `boss
# prove --from-car`, tenant-census.sh for `boss tenant`, …), each with
# its own pin, for the one reason that the forge had no `boss` binary.
# Two callers now: infra/gcp/boss-gcp-converge.sh after its units, and
# infra/forge/install.sh for the `cluster-operator` role. The twins
# retire one verb at a time in the cars after this one.
#
# WHY THIS EXISTS (backlog 6f58e9a1; David chose option (b), 2026-09-15).
# boss-gcp's /usr/local/bin/boss had NO refresh path. Only a human
# `deploy-services.sh prod` — a full deploy of the second, older stack
# that 45641c91 retires — ever installed it, and boss-gcp-converge
# installed units only. Measured 2026-09-15 20:5xZ, ops-request 20ba7cdf:
# "REFUSED — /usr/local/bin/boss cannot say what it was built from
# (boss 0.1.0 names no commit)". That binary was older than built_from
# itself, so every host verb that shells to the CLI (publish-workflow
# today; any registry-writing verb tomorrow; the Drift tab's Approve
# through the same door) refused 78 by name, and the 17 adrift workflow
# kinds stayed unpublished — 33 fields, all tree-ahead.
#
# THE IMAGE IS THE BUILD. cluster-deploy-runner.sh builds
# `<registry>/david/boss:<short sha>` for every train that lands and
# copies every `boss`/`boss-*` release binary into it (infra/oss-
# quickstart/Dockerfile, stage 4). So "the CLI at converge_sha" already
# exists, built once, by the same pipeline that runs the cluster; this
# script takes /usr/local/bin/boss out of that image. The CLI on this
# host is then the tree's CLI BY CONSTRUCTION — the rule the pod's boss
# shim applies — and "which CLI does boss-gcp run" becomes a read on the
# converge packet (`cli_sha` beside `converge_sha`), not a question for
# a human with ssh.
#
# NO DOCKER. The first build of this step (car c142457c, held) shelled
# to `docker pull` / `docker create` / `docker cp`; measured 2026-09-16
# 00:30Z, ops-request 546c13fc: boss-gcp has no docker (docker.service
# not found), and David has said the host gets none. An OCI registry is
# HTTP + JSON + tar, so the pull is curl and tar, and no daemon, no
# login and no daemon.json are involved:
#
#   GET /v2/                       401 + Www-Authenticate: Bearer realm=
#                                  "<realm>",service="<service>"
#   GET <realm>?service=…&scope=repository:<repo>:pull
#                                  {"token": …} — no credentials: the
#                                  forge package david/boss is PUBLIC,
#                                  verified from the pod 2026-09-16
#   GET /v2/<repo>/manifests/<tag> an OCI index (one manifest per
#                                  platform + an attestation) or a bare
#                                  manifest; the platform's manifest is
#                                  fetched by digest and lists the layers
#   GET /v2/<repo>/blobs/<digest>  each layer, a gzip tar
#
# THE TAG IS THE SHORT SHA; THE FULL SHA IS THE ATTESTATION. The deploy
# runner tags with `git rev-parse --short` (cluster-deploy-runner.sh:
# "the short tag stays the image name, the full sha is the attestation")
# and the registry's tag list is 7-char shas. The first build pulled
# `<repo>:<full sha>` and refused anything shorter, so every pull would
# have been a 404 — measured 2026-09-16 00:30Z against tags/list. Now
# the tag is ${SHA:0:7}; the FULL sha is what this script is handed,
# what the generation directory is named, what the wrapper hands the
# binary as BOSS_BUILD_COMMIT, and what `boss --version` must print.
#
# LAYERS ARE READ THE WAY A RUNTIME READS THEM. A file's final state is
# the TOPMOST layer that mentions it: a later layer's copy replaces an
# earlier one, and a whiteout (`.wh.<name>` beside it, or an opaque
# `.wh..wh.opq` in it or any parent directory) deletes it. So the layers
# are walked from the top down and the first one that mentions
# usr/local/bin/boss decides: a regular member is extracted; a whiteout
# is a refusal ("the image deletes it"). Every blob is verified against
# its manifest digest (sha256, the whole file) BEFORE tar reads a byte
# of it, and the platform manifest is verified against the index the
# same way. A layer that is not gzip tar is refused by media type.
#
# THE SHA RIDES THE PATH, AND THE WRAPPER CARRIES IT TO THE BINARY. The
# image's binary is compiled with BOSS_CLI_BUILT_FROM=unknown and reads
# BOSS_BUILD_COMMIT at runtime (built_from.rs); copied out and run bare
# it prints `built from unknown`, which the floor check refuses. So:
#
#   <store>/<sha>/boss     the binary, one directory per generation
#   <store>/current        -> <sha>, flipped atomically (make-before-break,
#                          the previous generation stays as the revert)
#   <store>/boss           boss-cli-wrapper.sh: execs current/boss with
#                          BOSS_BUILD_COMMIT=<basename of current>
#   /usr/local/bin/boss    -> <store>/boss
#
# The sha is defined ONCE, as the generation directory's name; the
# wrapper reads it from there. No sidecar, no text with the sha baked
# in (CLAUDE.md §9a).
#
# A TRAIN WITHOUT A RUST CHANGE ships an image whose binaries are older
# than its tag — the Dockerfile says so — and that is fine here: the tag
# sha IS the tree sha, BOSS_BUILD_COMMIT is what built_from reports, and
# the floor check asks "was this built from a tree at or after the
# loader landed", which the tree sha answers truthfully.
#
# CONFIRMED, OR NOT INSTALLED. The staged binary is run with the sha in
# its environment and must print `built from <sha>`; then, after the
# flip, the host's own path (/usr/local/bin/boss --version) must print
# it again. Anything else is NOT CONFIRMED: the staging directory is
# removed, `current` stays where it was, the reason rides the packet
# (cli_result) and the exit is non-zero. Mostly sure is a feeling; the
# --version line is the artifact.
#
# EVERY REFUSAL NAMES THE URL AND THE HTTP CODE. Each response body is
# captured to a file and printed whole when the request is not what was
# expected (CLAUDE.md §Diagnosis: a digest or a tail throws away the
# only copy). The one failure that heals by itself is a tag the deploy
# runner has not built yet — it builds the image a few minutes after
# each train lands — which the next converge tick picks up. That one is
# NOT a refusal: it exits 75 with `cli_result` = `not yet: …`, so the
# caller can tell a wait from a fault. On the forge the converge that
# installs the CLI runs on the host that builds the image, ten minutes
# apart, so the first tick after every train lands in this state; a
# red there would be a red on every train. Both callers wait on it:
# infra/forge/install.sh, and since 2026-09-26 boss-gcp's converge,
# which until then redded and healed every half hour (backlog f15ff5f2)
# and now reds only on a wait older than its limit.
#
# THE CODE-BRANCH COUNTER RIDES THE SAME DOOR (backlog c4d60110). The
# daily codebase-metrics chore on boss-gcp filed
# `code_branches_on_kind: null` on 11 of 11 packets (2026-09-17..27):
# `boss-leaked-policy` was on no path it looked at, because nothing had
# ever put it on the host. The image already carries it — stage 4
# copies every `boss-*` release binary into /usr/local/bin, and on
# boss:c033e6a (measured 2026-09-27) the one layer that holds
# usr/local/bin/boss holds usr/local/bin/boss-leaked-policy too — so
# this script takes it out of the same verified layer walk into the
# same generation, as <store>/<sha>/boss-leaked-policy, and
# boss-codebase-metrics.service names <store>/current/boss-leaked-policy
# (a test holds the two equal). It is decided the way the CLI is — the
# topmost layer that mentions it, a whiteout meaning deleted — but it is
# NEVER the CLI's precondition: an image without it installs the CLI
# and records `cli_leaked_policy` = `absent: <why>`, and the chore then
# files `unrunnable:` naming the path, rather than a guess.
#
# IDEMPOTENT AND CHEAP WHEN NOTHING MOVED: a tick whose `current`
# already names the sha re-verifies through the wrapper and fetches
# nothing. Generations are pruned to KEEP; blobs never stay on disk.
#
# USAGE
#   install-cli-from-image.sh <full sha>
#
# ENV
#   BOSS_CLI_IMAGE_REPO   the image repository (default the converge's
#                         REGISTRY from infra/forge/forge-defaults.sh, off
#                         the registry host in /etc/boss/sor.env);
#                         "<host>/<name>"
#   BOSS_CLI_REGISTRY_SCHEME  http (default — the forge registry is plain
#                         HTTP on the LAN) or https
#   BOSS_CLI_PLATFORM     "<arch>/<os>" of the manifest to take from a
#                         multi-platform index (default from uname -m:
#                         amd64/linux on x86_64, arm64/linux on aarch64)
#   BOSS_CLI_STORE        the generation store (default /opt/boss-cli)
#   BOSS_CLI_LINK         the host path (default /usr/local/bin/boss)
#   BOSS_CLI_WRAPPER_SRC  the wrapper to install (default the
#                         boss-cli-wrapper.sh beside this script)
#   BOSS_CLI_KEEP         generations kept on disk (default 3)
#   BOSS_CLI_PULL_TIMEOUT seconds the WHOLE pull may take, every request
#                         together (default 360) — the converge unit's
#                         ceiling is 10min for the whole run, and a pull
#                         that hangs must be a named failure on the
#                         packet, not a unit timeout
#   BOSS_RUN_SUMMARY_FILE where cli_sha / cli_result / cli_action /
#                         cli_image land (infra/run-summary.sh)
#
# EXIT
#   0  the CLI at <sha> is confirmed on the host path
#   1  refused / not confirmed — cli_result on the packet says which
#   2  usage
#   75 not yet — the registry has no image for <sha>'s tag (the deploy
#      runner has not built it); nothing changed, the next tick retries
set -uo pipefail

NAME="install-cli-from-image"
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$SELF_DIR/cli-compatible.sh" || exit 1

# shellcheck source=infra/run-summary.sh
. "$SELF_DIR/../run-summary.sh"
# gen_atomic_link — the one definition of "flip a symlink without a
# window where it is absent" (infra/generation.sh). The store variables
# it sets default to the retired prod store and are not used here.
# shellcheck source=infra/generation.sh
. "$SELF_DIR/../generation.sh"
# shellcheck source=infra/lib/jq.sh
. "$SELF_DIR/../lib/jq.sh"

SHA="${1:-}"
case "$SHA" in
    *[!0-9a-f]*|"") echo "usage: $(basename "$0") <full sha>" >&2; exit 2 ;;
esac
[ "${#SHA}" -eq 40 ] || { echo "usage: $(basename "$0") <full sha> — got '${SHA}' (${#SHA} chars; the full commit is the attestation, the image tag is derived from it)" >&2; exit 2; }

if [ -z "${BOSS_CLI_IMAGE_REPO:-}" ]; then
    # shellcheck source=infra/forge/forge-defaults.sh
    . "$SELF_DIR/../forge/forge-defaults.sh"
    forge_need REGISTRY
    BOSS_CLI_IMAGE_REPO="$REGISTRY"
fi
IMAGE_REPO="$BOSS_CLI_IMAGE_REPO"
SCHEME="${BOSS_CLI_REGISTRY_SCHEME:-http}"
STORE="$(cli_host_store)"
LINK="${BOSS_CLI_LINK:-/usr/local/bin/boss}"
WRAPPER_SRC="${BOSS_CLI_WRAPPER_SRC:-$SELF_DIR/boss-cli-wrapper.sh}"
KEEP="${BOSS_CLI_KEEP:-3}"
PULL_TIMEOUT="${BOSS_CLI_PULL_TIMEOUT:-360}"
# The deploy runner's tag: the first seven characters of the merge's
# full sha (cluster-deploy-runner.sh, HEAD=${HEAD_FULL:0:7} — a fixed
# substring, never `git rev-parse --short`, whose length moves with the
# repository; a test pins both). The full sha is the attestation.
TAG="${SHA:0:7}"
IMAGE="$IMAGE_REPO:$TAG"
REGISTRY_HOST="${IMAGE_REPO%%/*}"
REPO_PATH="${IMAGE_REPO#*/}"
REGISTRY_URL="$SCHEME://$REGISTRY_HOST"
MEMBER="usr/local/bin/boss"
INPUTS_MEMBER="usr/local/bin/boss-inputs.json"
# The codebase-metrics chore's counter, installed beside the CLI when the
# image carries it (see THE CODE-BRANCH COUNTER above). Its basename is
# what lands in the generation; the unit's BOSS_LEAKED_POLICY_BIN is
# pinned to <STORE default>/current/<that basename>.
COUNTER_MEMBER="usr/local/bin/boss-leaked-policy"
COUNTER="${COUNTER_MEMBER##*/}"
CONTROL_MEMBER="opt/boss/dev-control.tar"
BOOTSTRAP_MEMBER="opt/boss/dev-control"
case "${BOSS_CLI_PLATFORM:-}" in
    "") case "$(uname -m 2>/dev/null)" in
            x86_64) PLATFORM="amd64/linux" ;;
            aarch64|arm64) PLATFORM="arm64/linux" ;;
            *) PLATFORM="amd64/linux" ;;
        esac ;;
    *) PLATFORM="$BOSS_CLI_PLATFORM" ;;
esac

say() { printf '%s: %s\n' "$NAME" "$*"; }

# Each fact on the packet as soon as it is true (run-summary.sh's rule):
# the sha this run is FOR, first, so a run that dies still says what it
# was trying to install.
run_summary_field cli_sha "$SHA"
run_summary_field cli_image "$IMAGE"

# One exit path for every verdict that is not "ok": the packet carries
# the verdict, the journal carries the paragraph, and `current` is
# whatever it was. A `not yet:` verdict exits 75 — the wait, not the
# fault — and every other exits 1.
refuse() { # <cli_result> <message...>
    local result="$1"; shift
    printf '%s: %s — %s\n    %s\n' "$NAME" "$(printf '%s' "${result%%:*}" | tr '[:lower:]' '[:upper:]')" "${result#*: }" "$*" >&2
    run_summary_field cli_result "$result"
    run_summary_note "$NAME: $result — $*"
    [ -n "${stage:-}" ] && rm -rf "$stage"
    case "$result" in "not yet:"*) exit 75 ;; esac
    exit 1
}

# `boss --version` prints `boss <crate> built from <sha>` (built_from.rs).
# Captured whole; first line taken in the shell — an external producer
# piped into an early-exiting reader is the SIGPIPE coin-flip.
version_of() { # <argv...> -> prints the first line, returns its status
    local line
    line="$("$@" 2>&1)"
    local rc=$?
    printf '%s\n' "${line%%$'\n'*}"
    return $rc
}

confirmed() { # <line> -> 0 iff it names this sha
    case "$1" in *"built from $SHA"*) return 0 ;; esac
    return 1
}

mkdir -p "$STORE" || refuse "failed: cannot create $STORE" "the generation store could not be created"

# --- one install at a time per store ----------------------------------------
# CONCURRENT INSTALLS TAKE TURNS (backlog 9e1f037a). Every run of one
# sha stages in the same `.staging-<sha>` and begins by removing it, so
# two runs at once delete and refill each other's pull mid-walk. On the
# dev pod that is the common case, not a corner: every builder's shim
# runs this before a write verb once a train lands (and re-runs it every
# 30 s while the image is late), beside the sidecar's hourly pass.
# Measured 2026-09-23 against the real registry, image boss:8c122d9 —
# the one the packet's "blob digest mismatch" named: alone it installs
# clean; two to four concurrent runs gave 12 refusals in 14, every one
# of them blaming the IMAGE ("layer 26 is '', not a gzip tar", "has no
# amd64/linux manifest", "arrived with no bytes"), and one walked past
# the layer that carries the binary — which is how the packet's refusal
# came to name layer 1 at all. So the whole run holds one lock on the
# store: the runs that waited find the generation the first installed
# and fetch nothing (`relinked`/`unchanged`), and the lock also covers
# `current`, the wrapper and the prune, which every sha shares. The fd
# is opened read-only — flock needs no write access, and the store is
# shared between accounts (the pod's dev container and its sidecar). A
# wait past the bound is `not yet` (75): the holder is pulling, and
# the callers that retry on 75 retry.
LOCK="$STORE/.install.lock"
LOCK_WAIT="${BOSS_CLI_LOCK_WAIT:-$PULL_TIMEOUT}"
command -v flock >/dev/null 2>&1 \
    || refuse "refused: no flock on this host" "two installs into $STORE at once destroy each other's staging, so this run takes $LOCK first, and flock (util-linux) is not on PATH"
[ -e "$LOCK" ] || : >>"$LOCK" 2>/dev/null
exec 9<"$LOCK" || refuse "failed: cannot open $LOCK" "the store's install lock could not be opened for reading"
if ! flock -w "$LOCK_WAIT" 9; then
    refuse "not yet: another install into $STORE held its lock for ${LOCK_WAIT}s" \
        "a concurrent run of this installer is still pulling into $STORE (BOSS_CLI_LOCK_WAIT=${LOCK_WAIT}s); nothing was touched. Retry: the waiter that gets the lock finds its generation, if the holder installed one, and fetches nothing."
fi

# --- the wrapper, current with the tree -------------------------------------
# Copied, never linked: /opt/boss is a checkout the converge moves, and
# a link into it would make `boss` change bytes mid-fast-forward.
[ -f "$WRAPPER_SRC" ] || refuse "failed: no wrapper at $WRAPPER_SRC" "boss-cli-wrapper.sh must ship beside this script"
if ! cmp -s "$WRAPPER_SRC" "$STORE/boss"; then
    tmp="$STORE/.boss.tmp.$$"
    cp "$WRAPPER_SRC" "$tmp" && chmod 0755 "$tmp" && mv -Tf "$tmp" "$STORE/boss" \
        || { rm -f "$tmp"; refuse "failed: cannot install the wrapper into $STORE" "copy failed"; }
    say "wrapper installed at $STORE/boss"
fi

# A later data-only source can use this actual image unchanged. Re-read every
# qualification; a previous local compatibility result is never authority.
# Keep cli_sha/image truthful and record compatible source separately.
previous_image=$(readlink "$STORE/current" 2>/dev/null || true)
if [ -n "$previous_image" ] && [ "$previous_image" != "$SHA" ] \
    && [ -r "$SELF_DIR/cli-compatible.sh" ]; then
    . "$SELF_DIR/cli-compatible.sh"
    source_repo="$(cd "$SELF_DIR/../.." && pwd -P)"
    source_reader=""
    [ ! -f "$source_repo/.boss-tree-sha" ] || source_reader="$source_repo/infra/forge/root-tree.sh"
    if compatibility_receipt=$(cli_image_compatible "$STORE" "$previous_image" "$SHA" "$source_repo" "$source_reader"); then
        if [ "$(readlink "$LINK" 2>/dev/null || true)" != "$STORE/boss" ]; then
            mkdir -p "$(dirname "$LINK")"
            gen_atomic_link "$STORE/boss" "$LINK" || refuse 'failed: cannot link compatible CLI' "$LINK"
        fi
        line="$(version_of "$LINK" --version)" || true
        case "$line" in
            *"built from $previous_image"*) ;;
            *) refuse 'unconfirmed: compatible image wrapper identity differs' "$line" ;;
        esac
        run_summary_field cli_sha "$previous_image"
        run_summary_field cli_image "$IMAGE_REPO:${previous_image:0:7}"
        run_summary_field cli_compatible_source "$SHA"
        run_summary_field cli_compatibility_receipt "$compatibility_receipt"
        run_summary_field cli_compatibility_sha256 "$(sha256sum "$compatibility_receipt" | cut -d' ' -f1)"
        run_summary_field cli_action compatible
        run_summary_field cli_result ok
        say "compatible source $SHA; actual image CLI remains $previous_image; whole native observation: $compatibility_receipt"
        exit 0
    fi
fi

# --- the registry, over HTTP ------------------------------------------------
# One request shape for everything, so a failure always has the same
# three facts beside it: the URL, the HTTP code, and the body in full.
# The whole pull shares ONE deadline (PULL_TIMEOUT from the first
# request), so twenty-four layers cannot each take the full allowance.
DEADLINE=0
TOKEN=""
# The pull token rides to curl as a 0600 header file, never in its argv,
# where every local user reads it in ps (backlog 5f3ad356): made with
# infra/lib/secret-header.sh in THIS shell once the token is read,
# because http_get runs inside $(…).
TOKEN_HDR=""
# shellcheck source=infra/lib/secret-header.sh
. "$SELF_DIR/../lib/secret-header.sh"
# shellcheck source=infra/estate/registry-manifest.sh
. "$SELF_DIR/registry-manifest.sh"
http_get() { # <url> <out-file> <headers-file> <accept|""> -> prints the http code; 0 iff curl ran
    local url="$1" out="$2" hdrs="$3" accept="${4:-}"
    local left=$((DEADLINE - SECONDS))
    [ "$left" -gt 0 ] || { printf '000\n'; echo "curl: the pull's ${PULL_TIMEOUT}s budget is spent before $url" >&2; return 28; }
    local -a args=(-sS -o "$out" -D "$hdrs" -w '%{http_code}' --max-time "$left" -L)
    [ -n "$accept" ] && args+=(-H "Accept: $accept")
    [ -n "$TOKEN_HDR" ] && args+=(-H "$TOKEN_HDR")
    curl "${args[@]}" "$url"
}

# Refuse with the request named and its body printed whole.
refuse_http() { # <what> <url> <code> <curl-rc> <body-file> <hint>
    local what="$1" url="$2" code="$3" rc="$4" body="$5" hint="$6"
    if [ "$rc" -ne 0 ]; then
        echo "$NAME: $what — curl exited $rc for $url (HTTP $code); curl's message is above." >&2
        [ "$rc" -eq 28 ] && echo "    (the whole pull is bounded by BOSS_CLI_PULL_TIMEOUT=${PULL_TIMEOUT}s)" >&2
        refuse "refused: $what — curl exit $rc for $url" "$hint"
    fi
    echo "$NAME: $what — $url answered HTTP $code; the body in full:" >&2
    sed 's/^/    body: /' "$body" >&2
    refuse "refused: $what — HTTP $code from $url" "$hint"
}

# sha256 of a file, as "sha256:<hex>" — the digest form the manifest uses.
#
# NO BYTES IS NOT A MISMATCH (backlog 112b1d87). This used to hash its
# argument unconditionally, and `sha256sum` on a file that is not there
# writes its complaint to stderr and nothing to stdout — so the caller
# got `sha256:`, an EMPTY digest, and compared it against the manifest.
# Measured 2026-09-20: a layer fetch answered HTTP 200 with rc 0 and
# produced no file (`layer.blob: No such file or directory`), and the
# run refused with "blob digest mismatch … the bytes fetched hash to
# sha256:". A mismatch reads as corruption or a tampered registry and
# sends its reader to the wrong investigation; the condition was in
# fact transient, and the identical call installed cleanly two minutes
# later.
#
# The check lives HERE rather than at the two call sites because both
# have the same shape one fetch apart, and a guard written twice is one
# a third caller will not know about (§9a). The existing HTTP check
# stays: it asks whether the fetch was reported to succeed, and this
# asks whether an artefact exists — the distinction between an answer
# and an effect.
digest_of() {
    if [ ! -e "$1" ]; then
        echo "$NAME: no file at $1 — the fetch reported success and created nothing" >&2
        return 3
    fi
    if [ ! -s "$1" ]; then
        echo "$NAME: $1 holds 0 bytes — the fetch reported success and wrote an empty body" >&2
        return 3
    fi
    printf 'sha256:%s\n' "$(sha256sum "$1" | cut -d' ' -f1)"
}

# --- the generation ---------------------------------------------------------
# What `current` names before this run touches anything: the revert
# target, and what every refusal below leaves in place.
prev="$(readlink "$STORE/current" 2>/dev/null || true)"
action=""
stage=""
# The counter's verdict: empty until a layer decides it, then
# `installed` or `absent: <why>`.
counter=""
compiler_inputs=""
control=""
bootstrap=""
if [ -x "$STORE/$SHA/boss" ]; then
    # Already on disk: a previous tick installed (and confirmed) it, or
    # `current` was moved away by hand. Re-link below; never re-pull.
    if [ "$(readlink "$STORE/current" 2>/dev/null)" = "$SHA" ]; then
        action="unchanged"
    else
        action="relinked"
    fi
else
    for tool in curl jq tar gzip sha256sum; do
        command -v "$tool" >/dev/null 2>&1 \
            || refuse "refused: no $tool on this host" "the CLI is taken out of the cluster image $IMAGE over HTTP with curl, jq, tar, gzip and sha256sum — no docker (boss-gcp has none, ops-request 546c13fc) — and $tool is not on PATH"
    done

    # Staging is not yet a generation. Every request's headers and body
    # land under pull/ (printed whole on a refusal, gone once the binary
    # is out); the generation keeps the binary and the manifest that
    # named its layers.
    stage="$STORE/.staging-$SHA"
    pull="$stage/pull"
    rm -rf "$stage"
    mkdir -p "$pull" || { stage=""; refuse "failed: cannot create $STORE/.staging-$SHA" "staging"; }
    DEADLINE=$((SECONDS + PULL_TIMEOUT))
    say "pulling $MEMBER out of $IMAGE for $SHA (platform $PLATFORM, timeout ${PULL_TIMEOUT}s)"

    registry_manifest "$IMAGE_REPO" "$TAG" "$pull" no
    TOKEN_HDR="$(cat "$pull/header-path")"
    nlayers="$(jq -r '.layers | length' "$pull/manifest.json" 2>/dev/null)"
    case "${nlayers:-empty}" in empty|0|*[!0-9]*) refuse "refused: the manifest of $IMAGE lists no layers" "manifest at $pull/manifest.json: $(head -c 400 "$pull/manifest.json")" ;; esac

    # 3. The layers, TOP DOWN. The first one that mentions the member
    #    decides — a whiteout in a higher layer means a lower layer's
    #    copy is deleted, so it must be seen first. Every blob is
    #    verified against its digest before tar reads it.
    found=""
    i=$((nlayers - 1))
    while [ "$i" -ge 0 ]; do
        ldigest="$(jq -r ".layers[$i].digest" "$pull/manifest.json")"
        ltype="$(jq -r ".layers[$i].mediaType" "$pull/manifest.json")"
        lsize="$(jq -r ".layers[$i].size" "$pull/manifest.json")"
        case "$ltype" in
            application/vnd.oci.image.layer.v1.tar+gzip|application/vnd.docker.image.rootfs.diff.tar.gzip) ;;
            *) refuse "refused: layer $i of $IMAGE is '$ltype', not a gzip tar" "this script reads gzip tar layers only (docker buildx writes those); a +zstd or unknown layer needs a different reader, not a guess. Nothing was installed." ;;
        esac
        burl="$REGISTRY_URL/v2/$REPO_PATH/blobs/$ldigest"
        blob="$pull/layer.blob"
        registry_blob "$IMAGE_REPO" "$ldigest" "$lsize" "$blob" "$TOKEN_HDR"
        # The member list, whole, then read in the shell: an early-exiting
        # grep on a 230 MB tar listing is the SIGPIPE coin-flip.
        if ! tar -tzf "$blob" >"$pull/layer.list" 2>"$pull/layer.err"; then
            refuse "refused: layer $i of $IMAGE is not a readable gzip tar" "tar said: $(tr '\n' ' ' <"$pull/layer.err")"
        fi
        # Optional complete compiler evidence from this same verified image.
        # Topmost mention wins, including whiteouts, just like the binary.
        if [ -z "$compiler_inputs" ]; then
            if grep -qx -- "$INPUTS_MEMBER" "$pull/layer.list"; then
                if [ "$(grep -cx -- "$INPUTS_MEMBER" "$pull/layer.list")" -eq 1 ] \
                    && tar -xzf "$blob" -C "$pull" -- "$INPUTS_MEMBER" 2>"$pull/inputs.err" \
                    && [ -f "$pull/$INPUTS_MEMBER" ] && [ ! -L "$pull/$INPUTS_MEMBER" ] \
                    && mv -- "$pull/$INPUTS_MEMBER" "$stage/boss-inputs.json"; then
                    compiler_inputs=staged
                else
                    compiler_inputs='unusable: image compiler receipt is not one regular member'
                fi
            else
                iwh="$(grep -m 1 -x -e "$(dirname "$INPUTS_MEMBER")/.wh.boss-inputs.json" \
                    -e "$(dirname "$INPUTS_MEMBER")/.wh..wh.opq" -e "usr/local/.wh.bin" \
                    -e "usr/local/.wh..wh.opq" -e "usr/.wh.local" -e "usr/.wh..wh.opq" \
                    -e ".wh.usr" -e ".wh..wh.opq" "$pull/layer.list")"
                [ -z "$iwh" ] || compiler_inputs="unusable: image deletes compiler receipt ($iwh)"
            fi
        fi
        # The counter, decided in the first layer that mentions it —
        # BEFORE the CLI's check below, which ends the walk and removes
        # the pull. A problem with it is recorded, never refused.
        # The optional controller is DATA beside this exact CLI image.
        # It is never unpacked or activated by this installer; only a
        # selector the operator already created makes the end of this
        # script run the bootstrap (dev isolation follows main, below).
        # Topmost mention wins, including directory whiteouts.
        if [ -z "$bootstrap" ]; then
            if grep -qx -- "$BOOTSTRAP_MEMBER" "$pull/layer.list"; then
                if python3 -I -c 'import sys,tarfile; archive=tarfile.open(sys.argv[1],"r:gz"); members=[m for m in archive if m.name==sys.argv[2]]; sys.exit(0 if len(members)==1 and members[0].isfile() else 1)' "$blob" "$BOOTSTRAP_MEMBER" \
                    && tar -xOzf "$blob" -- "$BOOTSTRAP_MEMBER" > "$stage/dev-control"; then
                    bootstrap="installed"
                    chmod 0755 "$stage/dev-control"
                else
                    refuse "refused: control bootstrap in layer $i is not a regular readable file" "no activation was attempted"
                fi
            else
                for deleted in opt/boss/.wh.dev-control opt/boss/.wh..wh.opq opt/.wh.boss opt/.wh..wh.opq .wh.opt .wh..wh.opq; do
                    if grep -qx -- "$deleted" "$pull/layer.list"; then
                        bootstrap="absent: whiteout $deleted in layer $i"
                        break
                    fi
                done
            fi
        fi
        if [ -z "$control" ]; then
            if grep -qx -- "$CONTROL_MEMBER" "$pull/layer.list"; then
                if python3 -I -c 'import sys,tarfile; archive=tarfile.open(sys.argv[1],"r:gz"); member=archive.getmember(sys.argv[2]); sys.exit(0 if member.isfile() else 1)' "$blob" "$CONTROL_MEMBER" \
                    && tar -xOzf "$blob" -- "$CONTROL_MEMBER" > "$stage/dev-control.tar"; then
                    control="installed"
                    chmod 0644 "$stage/dev-control.tar"
                else
                    refuse "refused: control artifact in layer $i is not a regular readable file" "no controller artifact was installed or activated"
                fi
            else
                for deleted in opt/boss/.wh.dev-control.tar opt/boss/.wh..wh.opq opt/.wh.boss opt/.wh..wh.opq .wh.opt .wh..wh.opq; do
                    if grep -qx -- "$deleted" "$pull/layer.list"; then
                        control="absent: whiteout $deleted in layer $i"
                        break
                    fi
                done
            fi
        fi
        if [ -z "$counter" ]; then
            if grep -qx -- "$COUNTER_MEMBER" "$pull/layer.list"; then
                if tar -xzf "$blob" -C "$pull" -- "$COUNTER_MEMBER" 2>"$pull/counter.err" \
                    && [ -f "$pull/$COUNTER_MEMBER" ] && [ ! -L "$pull/$COUNTER_MEMBER" ] \
                    && mv -f "$pull/$COUNTER_MEMBER" "$stage/$COUNTER" && chmod 0755 "$stage/$COUNTER"; then
                    counter="installed"
                    say "layer $i also carries $COUNTER_MEMBER; staged beside the CLI"
                else
                    counter="absent: layer $i of $IMAGE lists $COUNTER_MEMBER but no regular file could be staged from it ($(tr '\n' ' ' <"$pull/counter.err"))"
                fi
            else
                cwh="$(grep -m 1 -x -e "$(dirname "$COUNTER_MEMBER")/.wh.$COUNTER" \
                              -e "$(dirname "$COUNTER_MEMBER")/.wh..wh.opq" \
                              -e "usr/local/.wh.bin" -e "usr/local/.wh..wh.opq" \
                              -e "usr/.wh.local" -e "usr/.wh..wh.opq" \
                              -e ".wh.usr" -e ".wh..wh.opq" "$pull/layer.list")"
                [ -n "$cwh" ] && counter="absent: $IMAGE deletes $COUNTER_MEMBER (whiteout $cwh in layer $i)"
            fi
        fi
        if grep -qx -- "$MEMBER" "$pull/layer.list"; then
            found="$i"
            say "layer $i of $nlayers ($ldigest, $lsize bytes) carries $MEMBER; digest verified"
            tar -xzf "$blob" -C "$pull" -- "$MEMBER" 2>"$pull/layer.err" \
                || refuse "refused: tar could not extract $MEMBER from layer $i of $IMAGE" "tar said: $(tr '\n' ' ' <"$pull/layer.err")"
            [ -f "$pull/$MEMBER" ] && [ ! -L "$pull/$MEMBER" ] \
                || refuse "refused: layer $i of $IMAGE lists $MEMBER but tar extracted no regular file" "a symlink or directory at that path is not a CLI"
            mv -f "$pull/$MEMBER" "$stage/boss" && mv -f "$pull/manifest.json" "$stage/manifest.json" \
                || refuse "failed: cannot move the extracted binary into $stage" "rename failed"
            rm -rf "$pull"
            break
        fi
        # Not in this layer. A whiteout here for the member, or an opaque
        # whiteout on its directory or any parent, deletes every lower
        # layer's copy (OCI image-layer spec: whiteouts apply to lower
        # layers only — which is why a layer that carries the member is
        # decided above, before this check). The runtime would not see
        # the file, so neither may this host.
        wh="$(grep -m 1 -x -e "$(dirname "$MEMBER")/.wh.$(basename "$MEMBER")" \
                      -e "$(dirname "$MEMBER")/.wh..wh.opq" \
                      -e "usr/local/.wh.bin" -e "usr/local/.wh..wh.opq" \
                      -e "usr/.wh.local" -e "usr/.wh..wh.opq" \
                      -e ".wh.usr" -e ".wh..wh.opq" "$pull/layer.list")"
        if [ -n "$wh" ]; then
            refuse "refused: $IMAGE deletes $MEMBER (whiteout $wh in layer $i)" "a layer above every copy of the CLI removes it, so the image has no $MEMBER at runtime and this host installs none; nothing was installed."
        fi
        rm -f "$blob" "$pull/layer.list"
        i=$((i - 1))
    done
    [ -n "$found" ] || refuse "refused: no layer of $IMAGE carries $MEMBER" "$nlayers layers were listed and none has the CLI; the image is not one the Dockerfile's stage 4 built. Nothing was installed."
    # The walk stops at the CLI's layer, so a counter no layer at or
    # above it mentions is absent from this install — stage 4 copies both
    # in one COPY, so a lower layer carrying it alone is not this image.
    [ -n "$counter" ] || counter="absent: no layer of $IMAGE at or above the CLI's (layer $found) carries $COUNTER_MEMBER"
    chmod 0755 "$stage/boss"
    if [ "$compiler_inputs" = staged ]; then
        binary_hash=$(sha256sum "$stage/boss" | cut -d' ' -f1)
        if jq_doc_file "$stage/boss-inputs.json" && jq -e --arg h "$binary_hash" \
            '.schema == 1 and .reusable == true and .binaries.boss == $h and (.files | type == "object")' \
            "$stage/boss-inputs.json" >/dev/null; then
            chmod 0644 "$stage/boss-inputs.json"
            compiler_inputs='verified: whole image receipt matches actual CLI bytes'
        else
            mv -- "$stage/boss-inputs.json" "$stage/boss-inputs.invalid.json"
            compiler_inputs='unusable: compiler receipt is malformed, refused, or names other binary bytes'
        fi
    fi
    [ -n "$compiler_inputs" ] || compiler_inputs='unavailable: image carries no compiler receipt at or above the CLI layer'
    say "compiler inputs: $compiler_inputs"
    run_summary_field cli_compiler_inputs "$compiler_inputs"

    # CONFIRM THE STAGED BINARY BEFORE IT BECOMES A GENERATION: run it
    # the way the wrapper will, and read what it says it was built from.
    line="$(version_of env BOSS_BUILD_COMMIT="$SHA" "$stage/boss" --version)" || true
    if ! confirmed "$line"; then
        refuse "unconfirmed: the binary out of $IMAGE says '$line', not 'built from $SHA'" \
            "NOT CONFIRMED. With BOSS_BUILD_COMMIT=$SHA in its environment the extracted boss printed '$line'. A binary that does not name the commit it was installed for is not installed: the staging copy is removed and $STORE/current stays at ${prev:-none}."
    fi
    # A directory for this sha with no runnable boss in it (a run that
    # died mid-way) is not a generation; the confirmed staging replaces it.
    rm -rf "${STORE:?}/$SHA"
    mv -T "$stage" "$STORE/$SHA" || refuse "failed: cannot move $stage into place" "rename failed"
    stage=""
    action="installed"
fi

# D3 inputs are DATA from the same validated published archive, not
# caller environment or the currently running Pod. A legacy image may
# install its CLI without this preparation; it cannot supply D3 proof.
# Every new generation resolves once. Later ticks validate the retained
# binding without consulting mutable tags or rewriting its config.
control_config() {
    local generation="$STORE/$SHA" inputs worker postgres field ref repo tag digest temporary observations record
    local listing="$generation/control-members.txt"
    if ! tar -tf "$generation/dev-control.tar" > "$listing" 2>/dev/null || ! grep -Fxq 'infra/dev/control-images.json' "$listing"; then
        say 'dev-control config: unavailable — this generation has no published image inputs'
        run_summary_field cli_dev_control_config 'unavailable: generation has no published image inputs'
        return 0
    fi
    inputs="$generation/control-inputs.json"
    temporary="$(mktemp "$generation/.inputs.XXXXXX")" || refuse 'failed: cannot retain control inputs' "$generation"
    "$generation/dev-control" config-inputs "$SHA" --cli-store "$STORE" > "$temporary" || refuse 'refused: published control inputs failed validation' "$generation"
    jq_doc_file "$temporary" &&
    jq -e --arg sha "$SHA" '.commit == $sha and (.source_sha256 | type == "object") and (.worker_image | type == "string") and (.postgres_image | type == "string") and (.forge_url | type == "string")' "$temporary" >/dev/null || refuse 'refused: malformed published control inputs' "$generation"
    if [ -e "$generation/config.json" ] || [ -L "$generation/config.json" ]; then
        for record in "$generation/config.json" "$generation/config-source.json"; do
            [ -f "$record" ] && [ ! -L "$record" ] &&
            [ "$(stat -c %u "$record")" = "$(id -u)" ] &&
            [ "$(stat -c %a "$record")" = 600 ] &&
            [ "$(stat -c %s "$record")" -le 65536 ] \
                || refuse 'refused: unsafe immutable controller configuration' "$record must be private, owned, regular and bounded"
        done
        [ -f "$generation/config.json" ] && [ ! -L "$generation/config.json" ] &&
        [ -f "$generation/config-source.json" ] && [ ! -L "$generation/config-source.json" ] &&
        jq_doc_file "$generation/config.json" && jq_doc_file "$generation/config-source.json" && jq_doc_file "$temporary" &&
        jq -e -s --slurpfile inputs "$temporary" --arg p "$PLATFORM" '
          length == 2 and .[1].inputs == $inputs[0] and .[1].platform == $p and
          .[0] == (.[1].resolved + {forge_url:$inputs[0].forge_url}) and
          (.[0] | keys == ["forge_url","postgres_image","worker_image"])
        ' "$generation/config.json" "$generation/config-source.json" >/dev/null || refuse 'refused: immutable controller configuration binding changed' "$generation"
        rm -f "$temporary"
        run_summary_field cli_dev_control_config 'verified: immutable generation configuration'
        return 0
    fi
    if [ -e "$generation/config-source.json" ] || [ -L "$generation/config-source.json" ]; then
        refuse 'refused: incomplete immutable controller configuration' 'a retained binding without its config cannot be resolved again from mutable tags'
    fi
    mkdir -p "$generation/control-manifests" || refuse 'failed: cannot retain image observations' "$generation"
    observations="$(mktemp -d "$generation/control-manifests/attempt.XXXXXX")" || refuse 'failed: cannot retain image attempt' "$generation"
    mv -T "$temporary" "$observations/inputs.json" || refuse 'failed: cannot retain published control inputs' "$generation"
    inputs="$observations/inputs.json"
    [ "$DEADLINE" -ne 0 ] || DEADLINE=$((SECONDS + PULL_TIMEOUT))
    for field in worker_image postgres_image; do
        ref="$(jq -r --arg f "$field" '.[$f]' "$inputs")"
        repo="${ref%:*}"; tag="${ref##*:}"
        registry_manifest "$repo" "$tag" "$observations/$field" yes
        digest="$(cat "$observations/$field/digest")"
        if [ "$field" = worker_image ]; then worker="$repo@$digest"; else postgres="$repo@$digest"; fi
    done
    temporary="$(mktemp "$generation/.config-source.XXXXXX")" || refuse 'failed: cannot stage configuration binding' "$generation"
    jq -n --slurpfile inputs "$inputs" --arg p "$PLATFORM" --arg w "$worker" --arg d "$postgres" --arg observations "${observations#"$generation/"}" '{inputs:$inputs[0],platform:$p,observations:$observations,resolved:{worker_image:$w,postgres_image:$d}}' > "$temporary" || refuse 'failed: cannot write configuration binding' "$generation"
    mv -T "$temporary" "$generation/config-source.json" || refuse 'failed: cannot publish configuration binding' "$generation"
    temporary="$(mktemp "$generation/.config.XXXXXX")" || refuse 'failed: cannot stage configuration' "$generation"
    jq '.resolved + {forge_url:.inputs.forge_url}' "$generation/config-source.json" > "$temporary" || refuse 'failed: cannot write configuration' "$generation"
    mv -T "$temporary" "$generation/config.json" || refuse 'failed: cannot publish configuration' "$generation"
    say "dev-control config: resolved $PLATFORM images for immutable generation $SHA"
    run_summary_field cli_dev_control_config 'resolved: immutable generation configuration'
}
if [ -f "$STORE/$SHA/dev-control.tar" ] && [ ! -L "$STORE/$SHA/dev-control.tar" ]; then
    control_config
fi

# --- flip, link, and confirm through the host's own path --------------------
if [ "$prev" != "$SHA" ]; then
    gen_atomic_link "$SHA" "$STORE/current"
fi
# The host path is re-pointed only now, with a confirmed generation
# behind it: a host still carrying the old real-file binary keeps it
# until there is something better to run (make before break).
if [ "$(readlink "$LINK" 2>/dev/null || true)" != "$STORE/boss" ]; then
    mkdir -p "$(dirname "$LINK")"
    gen_atomic_link "$STORE/boss" "$LINK"
    say "$LINK -> $STORE/boss"
fi
line="$(version_of "$LINK" --version)" || true
if ! confirmed "$line"; then
    # The staged check passed and the wrapper does not agree: a wrapper
    # or link defect. Put `current` back where it was and say so.
    if [ -n "$prev" ] && [ "$prev" != "$SHA" ]; then
        gen_atomic_link "$prev" "$STORE/current"
    fi
    refuse "unconfirmed: $LINK --version says '$line', not 'built from $SHA'" \
        "NOT CONFIRMED through the host path. The generation at $STORE/$SHA answered correctly when run directly, so the wrapper ($STORE/boss) or the link ($LINK) is at fault; current is back at ${prev:-none}."
fi

# --- prune ------------------------------------------------------------------
# Newest KEEP generations by mtime stay; `current` is never pruned.
ls -1t "$STORE" 2>/dev/null | grep -x '[0-9a-f]\{40\}' | tail -n +"$((KEEP + 1))" \
    | while IFS= read -r old; do
        [ "$old" = "$SHA" ] && continue
        rm -rf "${STORE:?}/$old" && say "pruned generation ${old:0:8}"
    done

# The counter as the generation now holds it — which also answers for a
# tick that pulled nothing (`unchanged`/`relinked`).
if [ -x "$STORE/$SHA/$COUNTER" ]; then
    counter="installed"
elif [ -z "$counter" ]; then
    counter="absent: the generation $STORE/$SHA was installed without $COUNTER_MEMBER (before the counter rode this door, or from an image without it); the next sha's generation brings it"
fi
say "counter: $STORE/current/$COUNTER — $counter"
run_summary_field cli_leaked_policy "$counter"
if [ -f "$STORE/$SHA/dev-control.tar" ] && [ ! -L "$STORE/$SHA/dev-control.tar" ]; then
    control="installed"
else
    control="${control:-absent: this verified image generation carries no controller artifact}"
fi
run_summary_field cli_dev_control "$control"

# --- dev isolation follows main (design a68fc42a D6 and D1) -----------------
# David's answers, 2026-10-10 (backlog b3071bf4). The dev isolation store
# and its one selector live on the /work PVC, so a pod roll keeps them,
# and THIS pass keeps them current: where a selector already exists, the
# generation just confirmed re-activates through its OWN bootstrap,
# which validates its archive before running a byte of it. Without a
# selector nothing runs — activating is the operator's act, and every
# host but the dev pod has no store at all.
#
# A re-activation that fails leaves the selector REFUSED (D1): the doors
# and the CLI's child forwarding then refuse candidate execution with
# exit 75, naming the log below, while boss-api and the published CLI
# keep answering. Failing open would run candidate code beside the token
# until somebody noticed; the reclaim pass alarms on a refused selector
# every hour, so the refusal is never silent. The CLI install itself is
# still confirmed — this verdict rides beside it, not instead of it.
DEV_CONTROL="${BOSS_DEV_CONTROL_STORE:-/work/tools/dev-control}"
if [ -L "$DEV_CONTROL/enabled" ] || [ -e "$DEV_CONTROL/enabled" ]; then
    activation_out="$(mktemp "${TMPDIR:-/tmp}/install-cli-dev-control.XXXXXX")"
    activation_rc=0
    if [ -f "$STORE/$SHA/dev-control" ] && [ ! -L "$STORE/$SHA/dev-control" ]; then
        "$STORE/$SHA/dev-control" activate "$SHA" --cli-store "$STORE" --store "$DEV_CONTROL" --reactivate \
            >"$activation_out" 2>&1 || activation_rc=$?
    else
        echo "the generation $STORE/$SHA carries no dev-control bootstrap ($control), so nothing can re-establish the selector from it" >"$activation_out"
        activation_rc="no bootstrap"
    fi
    refusing="$DEV_CONTROL/.enabled-refusing.$$"
    # Rollback is the published deactivate door, never bare rm. It
    # holds the same lock as activation. Re-judge AFTER taking it:
    # a result observed earlier cannot resurrect a completed off or
    # report re-activation after follow-main returned inert.
    control_lock="$DEV_CONTROL/.activation.lock"
    if [ -f "$control_lock" ] && [ ! -L "$control_lock" ] &&
       exec 8<"$control_lock" && flock -w "$LOCK_WAIT" 8; then
        # Presence alone is not off intent (review cd351532): an
        # empty record left the old selector live while this pass
        # claimed it was absent. Match the published reader's owned,
        # private regular document and exact state. Finish an off
        # interrupted between recording intent and removing enabled,
        # under this same lock, before observing its completion.
        off_present=0
        off_valid=0
        if [ -e "$DEV_CONTROL/off.json" ] || [ -L "$DEV_CONTROL/off.json" ]; then
            off_present=1
            off_mode=$(stat -c %a "$DEV_CONTROL/off.json" 2>/dev/null || true)
            if [ -f "$DEV_CONTROL/off.json" ] && [ ! -L "$DEV_CONTROL/off.json" ] &&
               [ "$(stat -c %u "$DEV_CONTROL/off.json" 2>/dev/null)" = "$(id -u)" ] &&
               [ "${off_mode: -2}" = 00 ] &&
               jq_doc_file "$DEV_CONTROL/off.json" &&
               jq -e -s 'length == 1 and .[0] == {state:"off"}' "$DEV_CONTROL/off.json" >/dev/null 2>&1; then
                off_valid=1
            fi
            if [ "$off_valid" = 0 ]; then
                echo "invalid controller off intent at $DEV_CONTROL/off.json: expected an owned private regular document containing only state=off" >>"$activation_out"
            fi
        fi
        off_completed=0
        if [ "$off_valid" = 1 ]; then
            if rm -f -- "$DEV_CONTROL/enabled" &&
               [ ! -e "$DEV_CONTROL/enabled" ] && [ ! -L "$DEV_CONTROL/enabled" ]; then
                off_completed=1
            fi
        elif [ "$off_present" = 0 ] &&
             [ ! -e "$DEV_CONTROL/enabled" ] && [ ! -L "$DEV_CONTROL/enabled" ]; then
            off_completed=1
        fi
        if [ "$off_completed" = 1 ]; then
            if [ "$off_valid" = 1 ]; then
                activation="off: rollback completed after bootstrap (exit $activation_rc); selector observed absent"
            else
                activation="inert: selector observed absent after bootstrap (exit $activation_rc); no off intent recorded"
            fi
        elif [ "$activation_rc" = 0 ] && [ "$off_present" = 0 ]; then
            activation="re-activated at $SHA"
        elif ln -sfn refused "$refusing" && mv -Tf "$refusing" "$DEV_CONTROL/enabled"; then
            activation="REFUSED: re-activation at ${SHA:0:8} could not establish valid on/off state (bootstrap exit $activation_rc), so $DEV_CONTROL/enabled now refuses candidate execution; the bootstrap's whole output is in $DEV_CONTROL/activation.log"
        else
            rm -f "$refusing"
            activation="REFUSED BUT NOT WRITTEN: re-activation at ${SHA:0:8} could not establish valid on/off state (bootstrap exit $activation_rc) and the refused selector could not be written over $DEV_CONTROL/enabled; see $DEV_CONTROL/activation.log"
        fi
        flock -u 8
        exec 8<&-
    else
        activation="REFUSED BUT NOT WRITTEN: re-activation is unproven (bootstrap exit $activation_rc), because its transition lock could not be acquired; selector unchanged; see $DEV_CONTROL/activation.log"
    fi
    # The log keeps every pass's whole output (CLAUDE.md §Diagnosis: no
    # tail, no digest); one previous file is kept when it passes 1 MiB.
    if [ "$(stat -c %s "$DEV_CONTROL/activation.log" 2>/dev/null || echo 0)" -gt 1048576 ]; then
        mv -f "$DEV_CONTROL/activation.log" "$DEV_CONTROL/activation.log.1"
    fi
    {
        printf '%s %s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$SHA" "$activation"
        sed 's/^/    /' "$activation_out"
    } >> "$DEV_CONTROL/activation.log"
    say "dev-control: $activation"
    sed 's/^/    dev-control: /' "$activation_out"
    rm -f "$activation_out"
else
    activation="inert: no selector at $DEV_CONTROL/enabled — dev isolation was never activated here, and an install never activates it"
fi
run_summary_field cli_dev_control_activation "$activation"
run_summary_field cli_result ok
run_summary_field cli_action "$action"
say "CONFIRMED — $LINK is the tree's CLI at ${SHA:0:8} ($action, image $IMAGE): $line"
exit 0
