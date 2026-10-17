#!/usr/bin/env bash
# Read-only image reuse, shared by the shim and ordinary installer.
# The image CLI recomputes compiler inputs and native channel evidence. A
# retained observation is diagnostic evidence, never a cached authorization.
. "$(dirname "${BASH_SOURCE[0]}")/../lib/jq.sh"
cli_host_store() { printf '%s\n' "${BOSS_CLI_STORE:-/opt/boss-cli}"; }

cli_image_observation() { # STORE IMAGE_SHA SOURCE_HEAD REPOSITORY [ROOT_READER]
    local store="$1" image="$2" head="$3" repo="$4" reader="${5:-}" observation
    local -a args
    [[ "$image" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ && "$head" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || return 1
    [ -x "$store/$image/boss" ] && [ ! -L "$store/$image/boss" ] \
        && [ -f "$store/$image/boss-inputs.json" ] && [ ! -L "$store/$image/boss-inputs.json" ] || return 1
    command -v jq >/dev/null && command -v timeout >/dev/null || return 1
    mkdir -p "$store/compatibility" || return 1
    observation=$(mktemp "$store/compatibility/$head.$image.XXXXXX.json") || return 1
    args=(channels --compatible-source "$head" --repository "$repo")
    [ -z "$reader" ] || args+=(--root-tree-reader "$reader")
    if timeout -k 5 45 env BOSS_BUILD_COMMIT="$image" "$store/$image/boss" "${args[@]}" \
        > "$observation" 2> "$observation.stderr" \
        && jq_doc_file "$observation" && jq -e -s --arg image "$image" --arg head "$head" \
            'length == 1 and .[0].schema == 1 and .[0].binary_image == $image and .[0].compatible_source == $head' \
            "$observation" >/dev/null; then
        printf '%s\n' "$observation"
        return 0
    fi
    printf 'image CLI compatibility is unread or not yet qualified; whole observation: %s\n' "$observation" >&2
    return 1
}

cli_image_compatible() {
    local observation
    observation=$(cli_image_observation "$@") || return 1
    if jq_doc_file "$observation" && jq -e '.compatible == true and .compiler_inputs_equal == true and .delivery_evidence.arrived == true and .delivery_evidence.selection_ready == true' "$observation" >/dev/null; then
        printf '%s\n' "$observation"
        return 0
    fi
    printf 'image CLI compatibility is not yet qualified; whole observation: %s\n' "$observation" >&2
    return 1
}
