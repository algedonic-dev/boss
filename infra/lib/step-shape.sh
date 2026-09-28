# step-shape.sh — the server's step shape hash, in jq. Source it:
#
#   . "$(dirname "$0")/../lib/step-shape.sh"
#   shape=$(shape_hash_of "$step_json")
#
# WHAT IT IS. A presence stamp (a passkey sign-off) records
# `boss_core::job::step_shape_hash(title, metadata)` of the step it
# signed: sha256(title || NUL || canonical(metadata)), where canonical
# sorts object keys, writes each as `"key":value,` inside braces — the
# key JSON-ENCODED, since the security review of 2026-09-24: written
# raw, `{"zz":1,"zzz":2}` and `{"zz:1,zzz":2}` canonicalised alike and
# one shape's stamp verified on the other — each array element as
# `value,` inside brackets, and a scalar as its JSON text. Recomputing
# it over the step as it stands NOW is what makes "the content on the
# step is the content that was signed" a check a verb performs rather
# than a belief it holds.
#
# ONE COPY FOR EVERY VERB THAT ACTS ON A PASSKEY (CLAUDE.md §9a). It
# lived inside infra/ops/ops-runner.sh until 2026-09-27, when the
# public-mirror publish (infra/forge/publish-github-pr.sh) became the
# second verb that must refuse to act on an approval its passkey did not
# sign (backlog 02b65d81). A second copy in that script would be two
# definitions of one fact, so it moved here, and both source it.
#
# PINNED EQUAL TO THE RUST FUNCTION by the happy paths that bind their
# stamps with it: ops_runner_approval_sh.rs and publish_github_pr_sh.rs
# each pass only if this agrees. A disagreement refuses — jq 1.6
# escapes DEL, and prints 1.0 as 1, which serde_json does not — so the
# failure mode is a refused approval, never an accepted one.
#
# POSIX sh, because ops-runner.sh is `#!/bin/sh` (dash on the hosts) and
# publish-github-pr.sh is bash: nothing here may be a bashism. Needs jq
# and sha256sum. Writes nothing: the NUL separator travels in the pipe,
# never in a shell variable that could not hold it.

SHAPE_JQ='def canon:
    if type == "object" then
        "{" + ([keys[] as $k | ($k | tojson) + ":" + (.[$k] | canon) + ","] | join("")) + "}"
    elif type == "array" then
        "[" + (map(canon + ",") | join("")) + "]"
    else tojson end;'

# shape_hash_of <step json> — the server's step_shape_hash of that step,
# 64 lowercase hex characters.
shape_hash_of() {
    {
        printf '%s' "$1" | jq -j '.title // ""'
        printf '\000'
        printf '%s' "$1" | jq -j "$SHAPE_JQ .metadata | canon"
    } | sha256sum | cut -c1-64
}
