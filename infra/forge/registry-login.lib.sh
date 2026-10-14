# registry-login.lib.sh — the converge's docker login for the forge
# registry, read ONE way. SOURCED, not run.
#
#   . "$(dirname "$0")/registry-login.lib.sh"     # after infra/lib/jq.sh
#   registry_login_read "$REGISTRY_PUSH_DIR/config.json" "$REG_HOST" \
#       || refuse "$REGISTRY_LOGIN_WHY"
#
# WHY A FILE (backlog 016aeea3, 2026-09-27). prune-registry-versions.sh
# was the one reader of this credential, and it read it inline. The
# forge data move proves the registry still serves a known manifest
# after Forgejo restarts on the new drive, and that read needs the same
# login from the same place — a second copy of the parse would be a fact
# living twice (CLAUDE.md §9a). Moved here out of the prune verbatim,
# refusal words included, rather than copied.
#
# WHOSE LOGIN. The converge's: the registry push login the forge
# converge deposits root-only from the credential broker's token
# (registry-token-deposit.sh, which reads back what it holds through
# this file too), at forge-defaults.sh's REGISTRY_PUSH_DIR/config.json.
# Until backlog 683d7fe9 moved the converge to root it was the checkout
# owner's own `docker login`, in his ~/.docker/config.json, and every
# reader derived that home from who owned the checkout.
#
# NEVER PRINTED. On success AUTH_B64 (base64 user:token), CRED_USER and
# TOKEN are set in the caller's shell and nothing is echoed; a caller
# puts the header in a curl config file (mode 600), never in argv, and
# scrubs these values from anything a tool says back.

# registry_login_read <docker-config> <registry-host> — set AUTH_B64,
# CRED_USER and TOKEN from the inline auth the config holds for the
# host. On any failure they are left empty, REGISTRY_LOGIN_WHY names
# what is wrong (never the value), and rc is 1.
registry_login_read() {
    local cfg="$1" host="$2" store userinfo
    AUTH_B64=""; CRED_USER=""; TOKEN=""; REGISTRY_LOGIN_WHY=""
    if [ ! -f "$cfg" ]; then
        REGISTRY_LOGIN_WHY="no docker config at $cfg — the converge's registry push login (deposited by the forge converge from the broker's Secret boss/forge-registry-push-token) is the credential this verb uses, and it is not there"
        return 1
    fi
    # The `[ -f ]` above proves the file exists; a zero-byte login file
    # is the input `jq -e` reads as valid JSON (d96e38ab).
    if ! jq_doc_file "$cfg" || ! jq -e . < "$cfg" > /dev/null 2>&1; then
        REGISTRY_LOGIN_WHY="$cfg is empty or not JSON, so the login for $host cannot be read"
        return 1
    fi
    AUTH_B64=$(jq -r --arg host "$host" '
        (.auths // {}) | to_entries[]
        | select((.key | sub("^https?://"; "") | sub("/.*$"; "")) == $host)
        | .value.auth // empty' "$cfg" 2>/dev/null | awk 'NR == 1')
    if [ -z "$AUTH_B64" ]; then
        store=$(jq -r --arg host "$host" '(.credHelpers // {})[$host] // .credsStore // empty' "$cfg" 2>/dev/null)
        if [ -n "$store" ]; then
            REGISTRY_LOGIN_WHY="$cfg keeps the login for $host in a credential store (credsStore/credHelpers: $store), not inline; this verb reads only an inline docker login"
            return 1
        fi
        REGISTRY_LOGIN_WHY="$cfg holds no auth for $host — the forge converge deposits the push login there from the broker's Secret boss/forge-registry-push-token (registry-token-deposit.sh); never a hand docker login"
        return 1
    fi
    userinfo=$(printf '%s' "$AUTH_B64" | base64 -d 2>/dev/null)
    CRED_USER="${userinfo%%:*}"
    TOKEN="${userinfo#*:}"
    if [ -z "$userinfo" ] || [ "$userinfo" = "$CRED_USER" ] || [ -z "$CRED_USER" ] || [ -z "$TOKEN" ]; then
        REGISTRY_LOGIN_WHY="the auth for $host in $cfg does not decode to user:token"
        return 1
    fi
    return 0
}
