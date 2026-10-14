#!/usr/bin/env bash
# The installer's existing anonymous public-registry transport, shared
# by CLI extraction and generation-owned dev image resolution (a68 D3).
# Caller supplies bounded http_get/refuse/digest_of and PLATFORM.
# shellcheck source=infra/lib/jq.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib/jq.sh"

# OCI documents are single objects, not streams. jq otherwise judges
# the last document, allowing an earlier conflicting platform to vanish.
registry_json_object() {
    jq_doc_file "$1" &&
    jq -e -s 'length == 1 and (.[0] | type == "object")' "$1" >/dev/null \
        || refuse 'refused: OCI response must contain exactly one JSON object' "$1"
}
registry_blob() {
    local image_repo="$1" digest="$2" size="$3" body="$4" TOKEN_HDR="${5:-}"
    local url="$SCHEME://${image_repo%%/*}/v2/${image_repo#*/}/blobs/$digest" code rc got actual
    [[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] && [[ "$size" =~ ^[1-9][0-9]*$ ]] \
        || refuse 'refused: blob descriptor invalid' "$image_repo"
    code="$(http_get "$url" "$body" "$body.hdr")"; rc=$?
    [ "$rc" -eq 0 ] && [ "$code" = 200 ] \
        || refuse_http 'blob could not be fetched' "$url" "$code" "$rc" "$body" 'the complete response is retained; no bytes are trusted'
    got="$(digest_of "$body")" \
        || refuse 'not yet: blob arrived with no bytes' "$url reported HTTP $code for a blob the manifest says is $size bytes; no bytes arrived to verify"
    actual="$(stat -c %s "$body")" || refuse 'refused: blob size could not be measured' "$body"
    [ "$got" = "$digest" ] \
        || refuse 'refused: blob digest mismatch' "$url named $digest; $actual fetched bytes hash to $got"
    [ "$actual" = "$size" ] \
        || refuse 'refused: blob size mismatch' "$url descriptor names $size bytes; fetched $actual bytes"
}

registry_manifest() {
    local IMAGE_REPO="$1" TAG="$2" pull="$3" check_platform="${4:-no}"
    local REGISTRY_URL="$SCHEME://${IMAGE_REPO%%/*}" REPO_PATH="${IMAGE_REPO#*/}"
    local IMAGE="$IMAGE_REPO:$TAG" TOKEN_HDR="" TOKEN="" url code rc auth realm service turl
    local accept murl kind arch os mdigest msize actual durl got cdigest csize
    mkdir -p "$pull" || refuse 'failed: cannot retain registry observations' "$pull"
    url="$REGISTRY_URL/v2/"
    code="$(http_get "$url" "$pull/ping.body" "$pull/ping.hdr")"; rc=$?
    case "$rc:$code" in
        0:200) say "$url answered 200 without a token; pulling anonymously" ;;
        0:401)
            auth="$(tr -d '\r' <"$pull/ping.hdr" | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *//p')"
            auth="${auth%%$'\n'*}"
            realm="$(printf '%s' "$auth" | sed -n 's/.*realm="\([^"]*\)".*/\1/p')"
            service="$(printf '%s' "$auth" | sed -n 's/.*service="\([^"]*\)".*/\1/p')"
            [ -n "$realm" ] || refuse_http "the registry's 401 names no token realm" "$url" "$code" 0 "$pull/ping.body" "Www-Authenticate was '$auth'"
            turl="$realm?service=$service&scope=repository:$REPO_PATH:pull"
            code="$(http_get "$turl" "$pull/token.json" "$pull/token.hdr")"; rc=$?
            [ "$rc" -eq 0 ] && [ "$code" = 200 ] || refuse_http "the anonymous pull token for $IMAGE_REPO could not be fetched" "$turl" "$code" "$rc" "$pull/token.json" 'public pull only; no login or new credential authority'
            TOKEN="$(jq -r '.token // .access_token // empty' "$pull/token.json")"
            [ -n "$TOKEN" ] || refuse_http 'the token endpoint answered 200 without a token' "$turl" "$code" 0 "$pull/token.json" 'expected a pull token'
            secret_header TOKEN_HDR "Authorization: Bearer $TOKEN" || refuse 'failed: pull header could not be written' 'no token is sent in argv'
            ;;
        *) refuse_http 'the registry did not answer' "$url" "$code" "$rc" "$pull/ping.body" 'nothing from this registry is trusted' ;;
    esac
    accept='application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json'
    murl="$REGISTRY_URL/v2/$REPO_PATH/manifests/$TAG"
    code="$(http_get "$murl" "$pull/tag.json" "$pull/tag.hdr" "$accept")"; rc=$?
    if [ "$rc" -eq 0 ] && [ "$code" = 404 ]; then
        echo "$NAME: no image $IMAGE yet — $murl answered HTTP 404; the body in full:" >&2
        sed 's/^/    body: /' "$pull/tag.json" >&2
        refuse "not yet: no image $IMAGE — HTTP 404 from $murl" 'nothing was linked; a later tick may retry'
    fi
    [ "$rc" -eq 0 ] && [ "$code" = 200 ] || refuse_http "no image $IMAGE" "$murl" "$code" "$rc" "$pull/tag.json" 'nothing from this image is trusted'
    registry_json_object "$pull/tag.json"
    kind="$(jq -r '.mediaType // empty' "$pull/tag.json")"
    if [ -z "$kind" ]; then
        kind="$(tr -d '\r' <"$pull/tag.hdr" | sed -n 's/^[Cc]ontent-[Tt]ype: *//p')"
        kind="${kind%%$'\n'*}"
    fi
    arch="${PLATFORM%%/*}"; os="${PLATFORM#*/}"
    case "$kind" in
        application/vnd.oci.image.index.v1+json|application/vnd.docker.distribution.manifest.list.v2+json)
            mdigest="$(jq -r --arg a "$arch" --arg o "$os" '[.manifests[] | select(.platform.architecture == $a and .platform.os == $o)] | if length == 1 then .[0].digest else empty end' "$pull/tag.json")"
            [ -n "$mdigest" ] || { sed 's/^/    index: /' "$pull/tag.json" >&2; refuse "refused: $IMAGE has no unique $PLATFORM manifest" 'the whole index is retained'; }
            # The index commits to both the digest and byte count (6316acae).
            msize="$(jq -r --arg a "$arch" --arg o "$os" '.manifests[] | select(.platform.architecture == $a and .platform.os == $o) | .size | select(type == "number" and . == floor and . > 0 and . <= 1048576)' "$pull/tag.json")"
            [[ "$msize" =~ ^[1-9][0-9]*$ ]] || refuse 'refused: manifest descriptor invalid' "$IMAGE requires a positive integer size no larger than 1048576 bytes; the whole index is retained"
            durl="$REGISTRY_URL/v2/$REPO_PATH/manifests/$mdigest"
            code="$(http_get "$durl" "$pull/manifest.json" "$pull/manifest.hdr" 'application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json')"; rc=$?
            [ "$rc" -eq 0 ] && [ "$code" = 200 ] || refuse_http "the $PLATFORM manifest of $IMAGE could not be fetched" "$durl" "$code" "$rc" "$pull/manifest.json" 'the index named it'
            got="$(digest_of "$pull/manifest.json")" || refuse "not yet: the $PLATFORM manifest of $IMAGE arrived with no bytes" "$durl"
            [ "$got" = "$mdigest" ] || refuse "refused: manifest digest mismatch for $IMAGE" "index named $mdigest; fetched bytes hash to $got"
            actual="$(stat -c %s "$pull/manifest.json")" || refuse 'refused: manifest size could not be measured' "$pull/manifest.json"
            [ "$actual" = "$msize" ] || refuse "refused: manifest size mismatch for $IMAGE" "index named $msize bytes; fetched $actual bytes; the whole response is retained"
            ;;
        application/vnd.oci.image.manifest.v1+json|application/vnd.docker.distribution.manifest.v2+json)
            mv -f "$pull/tag.json" "$pull/manifest.json"
            mdigest="$(digest_of "$pull/manifest.json")" || refuse "not yet: manifest of $IMAGE arrived with no bytes" "$murl"
            ;;
        *) refuse_http "the manifest of $IMAGE has an unknown media type '$kind'" "$murl" "$code" 0 "$pull/tag.json" 'only OCI and Docker v2 manifests/indexes are supported' ;;
    esac
    registry_json_object "$pull/manifest.json"
    if [ "$check_platform" = yes ]; then
        cdigest="$(jq -r '.config.digest // empty' "$pull/manifest.json")"
        csize="$(jq -r '.config.size // empty' "$pull/manifest.json")"
        [[ "$cdigest" =~ ^sha256:[0-9a-f]{64}$ ]] && [[ "$csize" =~ ^[1-9][0-9]*$ ]] && [ "$csize" -le 1048576 ] || refuse 'refused: image configuration descriptor invalid' "$IMAGE"
        registry_blob "$IMAGE_REPO" "$cdigest" "$csize" "$pull/image-config.json" "$TOKEN_HDR"
        registry_json_object "$pull/image-config.json"
        jq_doc_file "$pull/image-config.json" &&
        jq -e --arg a "$arch" --arg o "$os" '.architecture == $a and .os == $o' "$pull/image-config.json" >/dev/null || refuse 'refused: image configuration platform mismatch' "$IMAGE requires $PLATFORM"
    fi
    printf '%s\n' "$mdigest" > "$pull/digest"
    # Pull-token material is never part of generation evidence.
    rm -f "$pull/token.json" "$pull/token.hdr"
    if [ "$check_platform" = yes ]; then
        rm -f "$TOKEN_HDR"
    else
        printf '%s\n' "$TOKEN_HDR" > "$pull/header-path"
    fi
}
