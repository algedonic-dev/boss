#!/usr/bin/env bash
#
# mirror-base-images — copy the CI build's external base images into the
# forge registry so `build-image` resolves NO public DNS (backlog
# 7f408b99). The gcr.io / docker.io lookups the build depends on flaked
# repeatedly (#236, #250: `lookup gcr.io on 127.0.0.53:53: no such
# host`) and reddened trains; mirroring moves that DNS dependency off
# the every-train hot path and onto this occasional, retryable job.
# Trains then pull the base images from the forge's own registry
# (infra/estate/estate.toml `forge_registry`), which needs no external
# resolver.
#
# BOUNDED BY CONSTRUCTION, like disk-floor-sweep.sh (infra/ops/verbs/README.md):
# the set of images is a FIXED in-tree list below, never a packet-
# supplied name. The `mirror-base-images` ops-verb runs THIS script with
# no arguments; a packet cannot ask it to pull or push anything else.
# The forge tags produced here are the exact refs ci.yml's `image:` and
# infra/forge/boss-ci/Dockerfile's `FROM`s point at — one source of
# truth, pinned by infra/lint/the-build-pulls-only-mirrored-bases.sh.
#
# AUTH. The one proven registry-push path on the forge host is david's
# rootless docker with its ambient registry login (see
# cluster-deploy-runner.sh). This script runs under the ops-runner as
# ROOT, so it points the docker CLI at david's socket AND david's
# docker config explicitly — otherwise root's own (unauthenticated)
# config is read and the push 401s. Both are overridable; failure is
# LOUD (a non-zero exit naming the image and step), never silent, so the
# ops-request records exactly what broke.
set -euo pipefail

# The owner's namespace on the forge registry, from forge-defaults.sh
# off /etc/boss/sor.env (BOSS_FORGE_REGISTRY_BASE overrides).
. "$(dirname "$0")/forge-defaults.sh"
forge_need REGISTRY_BASE
export DOCKER_HOST="${BOSS_MIRROR_DOCKER_HOST:-unix:///run/user/1000/docker.sock}"
export DOCKER_CONFIG="${BOSS_MIRROR_DOCKER_CONFIG:-/home/david/.docker}"

# external source ref  |  forge repo:tag (under $REGISTRY_BASE)
# Keep in lockstep with ci.yml (the kaniko executor the job runs in) and
# infra/forge/boss-ci/Dockerfile (the FROM bases kaniko then pulls).
#
# The cluster's own runtime images joined the list on 2026-09-09. The
# pipeline stopped pulling publicly first, then the databases followed;
# these five were what was left running in the cluster straight off
# docker.io and gcr.io -- nats on the bus, caddy on the TLS front,
# alpine/k8s and lego on the certificate chain, and the cloud SDK the
# nightly backup ships with. A resolver stall or a rate limit on any of
# them is an outage diagnosed from the outside, which is what the
# mirror exists to prevent.
IMAGES="
gcr.io/kaniko-project/executor:v1.23.2-debug|kaniko-executor:v1.23.2-debug
docker.io/oven/bun:1.3-slim|bun:1.3-slim
docker.io/library/rust:1.96.1-slim-bookworm|rust:1.96.1-slim-bookworm
docker.io/library/postgres:16|postgres:16
docker.io/library/postgres:16-alpine|postgres:16-alpine
docker.io/library/nats:2.10-alpine|nats:2.10-alpine
docker.io/library/caddy:2.8-alpine|caddy:2.8-alpine
docker.io/alpine/k8s:1.33.3|alpine-k8s:1.33.3
docker.io/goacme/lego:v4.21.0|lego:v4.21.0
gcr.io/google.com/cloudsdktool/google-cloud-cli:alpine|google-cloud-cli:alpine
"

mappings() { printf '%s\n' "$IMAGES" | sed '/^[[:space:]]*$/d'; }

if [ "${1:-}" = "--check" ]; then
    # No docker, no network: validate the list is well-formed and print
    # the mappings. Lets a gate lint and a dry inspection verify the
    # source-of-truth list without touching the registry.
    rc=0
    while IFS='|' read -r ext dst; do
        if [ -z "$ext" ] || [ -z "$dst" ] || [ "$ext" = "$dst" ]; then
            echo "mirror-base-images: malformed mapping: '$ext' -> '$dst'" >&2
            rc=1
        fi
        echo "  $ext  ->  $REGISTRY_BASE/$dst"
    done < <(mappings)
    echo "mirror-base-images: --check ok ($(mappings | wc -l | tr -d ' ') mapping(s))"
    exit $rc
fi

# --missing: mirror only what the registry does not hold. The converge
# runs this before every image build (cluster-deploy-runner.sh), because
# a tag in the list above is a DECLARATION and the registry holding it is
# the fact: on 2026-09-13 the cluster image's `COPY --from=…/alpine-k8s:
# 1.33.3` named a tag listed here since 2026-09-09 that no one had ever
# mirrored, six converges failed on `not found`, and two merged trains
# sat unconverged for two hours until a human filed this verb. The
# presence check is `docker manifest inspect` against the forge tag with
# the same config the push uses; absent means pull, tag, push.
ONLY_MISSING=false
[ "${1:-}" = "--missing" ] && ONLY_MISSING=true

pull_with_retry() {
    # The pull is the only step exposed to public DNS — the very flake
    # this job exists to retire off the hot path. Retry it here so a
    # transient resolver miss during a mirror run does not need a human
    # to re-file the ops-request.
    local ref="$1" attempt=1 max=4
    while :; do
        if docker pull "$ref"; then return 0; fi
        if [ "$attempt" -ge "$max" ]; then
            echo "mirror-base-images: pull FAILED after ${max} attempts: $ref" >&2
            return 1
        fi
        echo "mirror-base-images: pull attempt ${attempt}/${max} failed for $ref — retrying" >&2
        attempt=$((attempt + 1)); sleep $((attempt * 5))
    done
}

# THE READ-BACK (backlog 1058e686, car E). `done` used to follow three
# exit codes — pull, tag, push — and nothing asked the registry what the
# tag now serves: a push that answered 0 over a tag still serving an
# older manifest printed done. So after each push, three reads:
#   1. the forge tag is, locally, the very image the pull brought (the
#      same image id as the source ref) — the tag did not name another;
#   2. the digest that PULLED image records for the forge repository
#      (RepoDigests; docker writes `<repo>@sha256:…` when a push lands) —
#      what this push sent, as docker computed it from the pulled bytes;
#   3. the REGISTRY, twice: the manifest it holds at that digest, and the
#      manifest its TAG serves — which must be the same manifest.
# The comparison is manifest to manifest in the forge's own registry,
# never the forge's digest against the SOURCE registry's: a push may
# re-encode a manifest (an OCI index pulled, one platform pushed), so the
# source's digest can differ from a faithful mirror's, and a read-back
# that failed every honest run would be read as noise. `--insecure`
# because the forge registry is plain HTTP on the LAN (the reason
# prune-registry-tags.lib.sh gives). No answer is CANNOT ANSWER and a
# different answer is FAILED. NEITHER STOPS THE LOOP (review of car E,
# run 7fe34bd7): the store a daemon keeps decides what RepoDigests holds
# — under the containerd image store it is the pulled INDEX digest, which
# a single-platform push never sends — so a read-back that exited on the
# first image would leave every later image unmirrored, and the mirror
# is the tool that repairs a missing base. Every image is pushed and read
# back; the run then exits 1 naming each one that did not read back, and
# prints `done`, its declared effect, only when all of them did.
read_back() { # <source ref> <forge ref>
    local ext="$1" forge="$2" repo="${2%:*}" src_id="" dst_id="" digest="" line="" by_digest="" by_tag=""
    if ! src_id=$(docker image inspect --format '{{.Id}}' "$ext" 2>&1) \
        || ! dst_id=$(docker image inspect --format '{{.Id}}' "$forge" 2>&1); then
        echo "mirror-base-images: CANNOT ANSWER — pushed $forge, but the local images could not be read back: $src_id $dst_id" >&2
        return 1
    fi
    if [ -z "$src_id" ] || [ "$src_id" != "$dst_id" ]; then
        echo "mirror-base-images: FAILED — $forge is local image ${dst_id:-none}, not the pulled $ext (${src_id:-none}); nothing vouches that the push sent what the pull brought" >&2
        return 1
    fi
    while IFS= read -r line; do
        case "$line" in "$repo@sha256:"*) digest="${line#"$repo"@}" ;; esac
    done < <(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$ext" 2>/dev/null || true)
    if ! [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
        echo "mirror-base-images: CANNOT ANSWER — pushed $forge, but the pulled $ext records no digest for $repo (RepoDigests), so there is nothing to compare the registry with" >&2
        return 1
    fi
    if ! by_digest=$(docker manifest inspect --insecure "$repo@$digest" 2>&1); then
        echo "mirror-base-images: CANNOT ANSWER — pushed $forge, but the registry gave no manifest at $repo@$digest, the digest the pulled image records: $by_digest" >&2
        return 1
    fi
    if ! by_tag=$(docker manifest inspect --insecure "$forge" 2>&1); then
        echo "mirror-base-images: CANNOT ANSWER — pushed $forge, but the registry gave no manifest for the tag: $by_tag" >&2
        return 1
    fi
    if [ -z "$by_tag" ] || [ "$by_tag" != "$by_digest" ]; then
        echo "mirror-base-images: FAILED — the push of $forge exited 0, but the registry's tag serves a different manifest than $repo@$digest, the digest the pulled $ext records — the tag was not moved to what was pushed" >&2
        return 1
    fi
    echo "mirror-base-images: read back $forge — the registry's tag serves $digest, the digest the pulled $ext records for it"
}

count=0
skipped=0
unread=()
while IFS='|' read -r ext dst; do
    forge="$REGISTRY_BASE/$dst"
    # `--insecure`: the forge registry is plain HTTP, and without it the
    # CLI asks over HTTPS, is refused, and reads every tag as absent — so
    # every converge re-pulled all ten public bases (review of car E).
    if $ONLY_MISSING && docker manifest inspect --insecure "$forge" >/dev/null 2>&1; then
        echo "mirror-base-images: $forge already in the registry — skipped"
        skipped=$((skipped + 1))
        continue
    fi
    echo "mirror-base-images: $ext  ->  $forge"
    pull_with_retry "$ext" || exit 1
    docker tag "$ext" "$forge" || { echo "mirror-base-images: tag FAILED: $ext -> $forge" >&2; exit 1; }
    docker push "$forge"      || { echo "mirror-base-images: push FAILED (registry auth? DOCKER_CONFIG=$DOCKER_CONFIG): $forge" >&2; exit 1; }
    count=$((count + 1))
    read_back "$ext" "$forge" || unread+=("$forge")
done < <(mappings)

# The done line is the verb's declared `effect`: it prints only when
# every pushed tag read back above.
if [ "${#unread[@]}" -gt 0 ]; then
    echo "mirror-base-images: FAILED — pushed ${count} image(s) to ${REGISTRY_BASE}, and ${#unread[@]} did not read back from the registry (each named above): ${unread[*]}" >&2
    exit 1
fi
if $ONLY_MISSING; then
    echo "mirror-base-images: done — ${count} image(s) mirrored to ${REGISTRY_BASE}, each read back from the registry at the digest its pulled source records (${skipped} already in the registry)"
else
    echo "mirror-base-images: done — ${count} image(s) mirrored to ${REGISTRY_BASE}, each read back from the registry at the digest its pulled source records"
fi
