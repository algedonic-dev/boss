#!/usr/bin/env bash
# Publisher for partial386: image evidence only; no login or launch binding.
set -euo pipefail
umask 077
export PATH=/usr/bin:/bin
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
SOURCE=$(cd "$HERE/../.." && pwd -P)
. "$HERE/../forge/forge-defaults.sh"
STORE="${BOSS_AGENT_IMAGE_STORE:-$CONVERGE_STATE/agent-images}"
MODE=${1:-}; SHA=${2:-}
[[ "$MODE" = publish || "$MODE" = status ]] && [[ "$SHA" =~ ^[0-9a-f]{40}$ ]] && [ "$#" = 2 ] || { echo 'agent-image: expected publish|status <full source sha>' >&2; exit 78; }
# The existing forge publisher is root on the forge, before any Docker effect.
. "$HERE/../lib/host-check.sh"
host_seam BOSS_AGENT_IMAGE_STORE "$CONVERGE_STATE/agent-images"
host_check forge agent-image
[ "$(id -u)" = 0 ] || { echo 'agent-image: root-owned publisher required' >&2; exit 78; }
/usr/bin/python3 -I "$HERE/image-witness.py" trusted-path "$SOURCE"
TARGET="$STORE/$SHA"
if [ "$MODE" = status ]; then
    exec /usr/bin/python3 -I "$HERE/image-witness.py" status "$TARGET" --attempt-store "$CONVERGE_STATE"
fi
/usr/bin/python3 -I "$HERE/image-witness.py" trusted-path "$REGISTRY_PUSH_DIR"
[ -f "$REGISTRY_PUSH_DIR/config.json" ] || { echo "agent-image: broker registry login absent" >&2; exit 75; }
/usr/bin/python3 -I "$HERE/image-witness.py" trusted-path "$REGISTRY_PUSH_DIR/config.json"
# Source identity is read from root's named Git view, never caller-supplied JSON.
GIT=(/usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 /usr/bin/git -c core.hooksPath=/dev/null -C "$SOURCE")
[ "$("${GIT[@]}" rev-parse HEAD)" = "$SHA" ] || { echo 'agent-image: source generation differs; nothing published' >&2; exit 78; }
[ -f "$HERE/../estate/registry-manifest.sh" ] || { echo 'agent-image: not yet: shared verified registry transport is not published' >&2; exit 75; }
. "$HERE/../estate/registry-manifest.sh"
forge_need REGISTRY_BASE
IMAGE_REPO="$REGISTRY_BASE/boss-agent-codex-exec"
install -d -m 0700 -- "$STORE"
[ ! -L "$STORE" ] && [ "$(realpath -e -- "$STORE")" = "$STORE" ] && [ "$(stat -c %u -- "$STORE")" = 0 ] && [ -z "$(find "$STORE" -maxdepth 0 -perm /022 -print)" ] || { echo 'agent-image: publication store is not private root-owned' >&2; exit 78; }
/usr/bin/python3 -I "$HERE/image-witness.py" trusted-path "$STORE"
[ ! -L "$STORE/.lock" ] || { echo 'agent-image: lock is linked' >&2; exit 78; }
[ ! -e "$STORE/.lock" ] || /usr/bin/python3 -I "$HERE/image-witness.py" trusted-path "$STORE/.lock"
exec 9>>"$STORE/.lock"
flock -x -w 30 9 || { echo 'agent-image: publication is still in use' >&2; exit 75; }
if [ -e "$TARGET" ] || [ -L "$TARGET" ]; then
    exec /usr/bin/python3 -I "$HERE/image-witness.py" status "$TARGET"
fi
/usr/bin/python3 -I "$HERE/image-witness.py" capacity "$STORE"
stage=$(mktemp -d "$STORE/.pending.$SHA.XXXXXXXX")
cleanup() { rm -f -- "$stage"/*/token.json "$stage"/*/token.hdr "$stage"/*/header-path; }
trap cleanup EXIT
. "$HERE/../lib/secret-header.sh"
install -d -m 0700 "$stage/context" "$stage/home" "$stage/toolchain" "$stage/agent"
raw="$stage/agent"
# Exactly tracked agent source, no checkout/home/docker config in the context.
"${GIT[@]}" archive --format=tar "$SHA" infra/agent > "$stage/context.tar"
tar -xf "$stage/context.tar" -C "$stage/context"
/usr/bin/python3 -I "$HERE/image-witness.py" inputs "$SOURCE" > "$stage/inputs"
mapfile -t inputs < "$stage/inputs"
VERSION=${inputs[0]}; TOOLCHAIN=${inputs[1]}
PLATFORM=amd64/linux; SCHEME=http; NAME=agent-image
say() { printf '%s: %s\n' "$NAME" "$*"; }
refuse() { printf '%s: %s — %s\n' "$NAME" "$1" "${2:-}" >&2; exit 1; }
refuse_http() {
    printf '%s: %s; HTTP %s, transport %s; original retained at %s\n' "$NAME" "$1" "$3" "$4" "$5" >&2
    exit 1
}
digest_of() { [ -s "$1" ] || return 1; local got; got=$(sha256sum "$1"); printf 'sha256:%s\n' "${got%% *}"; }
http_get() {
    local url=$1 body=$2 headers=$3 accept=${4:-} code rc=0 registry_url
    registry_url=$(/usr/bin/python3 -I "$HERE/image-witness.py" registry-url "$HERE" --url "$url" --scheme "$SCHEME" --repository "$IMAGE_REPO" --toolchain "$TOOLCHAIN") || return $?
    local args=(--silent --show-error --connect-timeout 10 --max-time 360 --max-filesize 2147483648 --dump-header "$headers" --output "$body" --write-out '%{http_code}')
    [ -z "$accept" ] || args+=(-H "Accept: $accept")
    [ -z "${TOKEN_HDR:-}" ] || args+=(-H "$TOKEN_HDR")
    code=$(/usr/bin/env -i PATH=/usr/bin:/bin HOME="$stage/home" /usr/bin/curl "${args[@]}" "$registry_url") || rc=$?
    printf '%s\n' "$code"; return "$rc"
}
fetch_image() {
    local repo=$1 tag=$2 out=$3 token_path member blob size
    registry_manifest "$repo" "$tag" "$out" no
    token_path=$(cat "$out/header-path")
    /usr/bin/python3 -I "$HERE/image-witness.py" descriptors "$out" > "$out/descriptors.tsv"
    while IFS=$'\t' read -r member blob size; do
        registry_blob "$repo" "$blob" "$size" "$out/$member" "$token_path"
    done < "$out/descriptors.tsv"
    /usr/bin/python3 -I "$HERE/image-witness.py" verify "$out"
    rm -f -- "${token_path#@}" "$out/header-path"
}
fetch_image "${TOOLCHAIN%:*}" "${TOOLCHAIN##*:}" "$stage/toolchain"
TOOLCHAIN_PIN="${TOOLCHAIN%:*}@$(cat "$stage/toolchain/digest")"
# All commands use the system socket and an empty environment. The broker's
# Docker consumer path rides push/pull only, never build or image execution.
native() {
    local phase=$1 login=$2; shift 2
    local rc=0
    local environment=(/usr/bin/env -i PATH=/usr/bin:/bin HOME="$stage/home")
    [ "$phase" != build ] || environment+=("DOCKER_BUILDKIT=1")
    [ "$login" = no ] || environment+=("DOCKER_CONFIG=$REGISTRY_PUSH_DIR")
    "${environment[@]}" /usr/bin/docker --host unix:///var/run/docker.sock "$@" > "$raw/$phase.stdout" 2> "$raw/$phase.stderr" || rc=$?
    /usr/bin/python3 -I "$HERE/image-witness.py" command "$raw" --phase "$phase" --exit-code "$rc" -- "$@"
    [ "$rc" = 0 ] || { cat "$raw/$phase.stdout" "$raw/$phase.stderr" >&2; refuse "native $phase failed exit $rc" "$stage"; }
}
/usr/bin/python3 -I "$HERE/image-witness.py" image-inputs "$SOURCE" --toolchain "$TOOLCHAIN_PIN" > "$stage/image-inputs.json"
/usr/bin/python3 -I "$HERE/image-witness.py" find-reuse "$STORE" --inputs-file "$stage/image-inputs.json" > "$stage/reuse.json"
if [ "$(cat "$stage/reuse.json")" = null ]; then
    native build no build --progress=plain --platform linux/amd64 --file "$stage/context/infra/agent/Dockerfile" --build-arg "TOOLCHAIN_IMAGE=$TOOLCHAIN_PIN" --build-arg "CODEX_VERSION=${VERSION#codex-cli }" --label "org.opencontainers.image.revision=$SHA" --tag "$IMAGE_REPO:$SHA" "$stage/context"
    native build-inspect no image inspect "$IMAGE_REPO:$SHA"
    native push yes push "$IMAGE_REPO:$SHA"
    fetch_image "$IMAGE_REPO" "$SHA" "$raw"
else
    cp -- "$stage/reuse.json" "$raw/reused-publication.json"
    reused_digest=$(/usr/bin/python3 -I - "$stage/reuse.json" <<'PYREUSE'
import json,sys
from pathlib import Path
print(json.loads(Path(sys.argv[1]).read_text())['image'].split('@')[1])
PYREUSE
    )
    fetch_image "$IMAGE_REPO" "$reused_digest" "$raw"
fi
cp -- "$stage/image-inputs.json" "$raw/image-inputs.json"
PIN="$IMAGE_REPO@$(cat "$raw/digest")"
native pull yes pull --platform linux/amd64 "$PIN"
native inspect no image inspect "$PIN"
PROBE=(run --rm --pull=never --network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 65534:1500 --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m)
native version no "${PROBE[@]}" --entrypoint /usr/local/bin/codex "$PIN" --version
native help no "${PROBE[@]}" --entrypoint /usr/local/bin/codex "$PIN" exec --help
native package-closure no "${PROBE[@]}" --entrypoint /bin/cat "$PIN" /usr/share/boss-agent/package-closure.json
native helper no "${PROBE[@]}" --entrypoint /usr/bin/sha256sum "$PIN" /usr/local/bin/boss-agent-io
cp -- "$stage/context/infra/agent/agent-io.py" "$raw/helper-source"
native collector-helper no "${PROBE[@]}" --entrypoint /usr/bin/sha256sum "$PIN" /usr/local/bin/boss-private-cache-collector
native collector-contract no "${PROBE[@]}" --entrypoint /usr/bin/sha256sum "$PIN" /usr/share/boss-agent/private-collector-contract.json
cp -- "$stage/context/infra/agent/private-collector.py" "$raw/collector-helper-source"
cp -- "$stage/context/infra/agent/private-collector-contract.json" "$raw/collector-contract-source"
/usr/bin/python3 -I "$HERE/image-witness.py" retire-layers "$raw" --generation "$SHA" --repository "$IMAGE_REPO" --version "$VERSION" --helper "$raw/helper-source"
# Toolchain staging also passed complete verification before the native build.
rm -f -- "$stage/toolchain"/layer-*.blob
/usr/bin/python3 -I "$HERE/image-witness.py" publish "$raw" --generation "$SHA" --repository "$IMAGE_REPO" --version "$VERSION" --helper "$raw/helper-source"
# The whole failed attempt stays private. Only a completely judged record is
# published under the immutable source name; no ready login binding is made.
mv -T -- "$stage" "$TARGET"
stage="$TARGET"
/usr/bin/python3 -I "$HERE/image-witness.py" status "$TARGET"
