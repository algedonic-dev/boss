//! A STUB `kubectl` FOR THE ONE SECRET SHAPE THE FORGE'S RENDER USES —
//! one copy for every test that drives `infra/forge/credential-render.sh`
//! for real (backlog 4ce4ec55). It lived in `credential_render_sh.rs`
//! alone until `github_act_sh.rs` needed the real render too, to
//! reproduce two approved GitHub writes for one owner end to end; two
//! spellings of what the API server does would be CLAUDE.md §9a's pair.
//!
//! Point the script's `BOSS_RENDER_KUBECTL` at a file holding
//! [`SECRET_KUBECTL`] and `STUB_SECRETS` at a directory: a Secret is the
//! directory `$STUB_SECRETS/<namespace>/<name>`, and each key a file in
//! it.

/// The stub: `-n NS get secret NAME -o jsonpath=TEMPLATE`, each
/// `{.data.<key>}` (dots escaped `\.`) replaced by the base64 of the file
/// `$STUB_SECRETS/NS/NAME/<key>`, nothing when the file is absent, and
/// `{.metadata.resourceVersion}` by the object's version (`.rv`, 1 when
/// unset). And `-n NS patch secret NAME --type json -p PATCH`, the one
/// write `--expire` makes: a `test` of the resourceVersion that fails, as
/// the API server's does, when the object moved since it was read, then
/// each `add` of `/data/<key>` written back decoded; every patch is
/// appended to `$STUB_SECRETS/patches.log`. `$STUB_SECRETS/race` makes
/// the object move between the read and the patch.
pub const SECRET_KUBECTL: &str = r#"#!/usr/bin/env bash
set -euo pipefail
[ "$1" = -n ] && [ "$4" = secret ] || { echo "stub kubectl: unexpected $*" >&2; exit 2; }
dir="$STUB_SECRETS/$2/$5"
[ -d "$dir" ] || { echo "Error from server (NotFound): secrets \"$5\" not found" >&2; exit 1; }
rv="$(cat "$dir/.rv" 2>/dev/null || echo 1)"
if [ "$3" = patch ]; then
    [ "$6" = --type ] && [ "$7" = json ] && [ "$8" = -p ] || { echo "stub kubectl: unexpected $*" >&2; exit 2; }
    printf '%s\n' "$9" >> "$STUB_SECRETS/patches.log"
    [ -e "$STUB_SECRETS/race" ] && rv=$((rv + 1)) && echo "$rv" > "$dir/.rv"
    want="$(jq -r '.[] | select(.op == "test" and .path == "/metadata/resourceVersion") | .value' <<<"$9")"
    [ "$want" = "$rv" ] || { echo "The request is invalid: the server rejected our request due to an error in our request" >&2; exit 1; }
    while IFS=$'\t' read -r path value; do
        printf '%s' "$value" | base64 -d > "$dir/${path#/data/}"
    done < <(jq -r '.[] | select(.op == "add") | [.path, .value] | @tsv' <<<"$9")
    echo $((rv + 1)) > "$dir/.rv"
    echo "secret/$5 patched"
    exit 0
fi
[ "$3" = get ] && [ "$6" = -o ] || { echo "stub kubectl: unexpected $*" >&2; exit 2; }
tpl="${7#jsonpath=}" out=""
re='^([^{]*)\{\.(data|metadata)\.(([^}\\]|\\.)*)\}(.*)$'
while [[ $tpl =~ $re ]]; do
    out+="${BASH_REMATCH[1]}"
    key="${BASH_REMATCH[3]//\\./.}"
    tpl="${BASH_REMATCH[5]}"
    if [ "${BASH_REMATCH[2]}" = metadata ]; then
        [ "$key" = resourceVersion ] && out+="$rv"
    elif [ -f "$dir/$key" ]; then out+="$(base64 -w0 < "$dir/$key")"; fi
done
printf '%s%s' "$out" "$tpl"
"#;
