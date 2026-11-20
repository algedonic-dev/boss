# git-owner.sh — run git in a checkout as the account that owns it.
#
# Source it, resolve the owner, then send every git call through as_owner:
#
#   . "$HERE/../lib/git-owner.sh"
#   git_owner_of "$TREE" \
#       || refuse "cannot resolve the owner of $TREE (stat says '${OWNER:-}', uid ${OWNER_UID:-?})"
#   head=$(as_owner "git -C '$TREE' rev-parse HEAD")
#
# WHY THE OWNER. An ops verb runs as root, and git refuses to READ a
# repository its caller does not own — since 2.35.2 that is "dubious
# ownership", and every command fails (ops-request c9877f75). Silencing
# it with safe.directory is the wrong repair: root's git then WRITES
# root-owned objects into the owner's clone, one edit away from breaking
# the owner's next pull. So the git runs as the account that owns the
# directory, read off the directory rather than hardcoded, so a rebuild
# that lands the checkout under another account does not silently start
# reading it as the wrong one.
#
# NOT DELETION AUTHORITY. A checkout read as its owner says what the
# OWNER says it holds: `git status` run as that account is its testimony,
# and a skip-worktree edit to a committed file is invisible to it (review
# a04e64e9 P1, 2026-11-15). A verb that derives what root may delete from
# a tree's files proves that tree instead — a generation of root's tree,
# infra/lib/root-generation.sh. Until that review this lib carried a
# git_owner_clean, a "refuse uncommitted declarations" check the two
# retire verbs used as their authority; they prove a generation now, it
# had no other caller, and it is gone rather than left for the next verb
# to mistake for proof.
#
# WHY ONE FILE (backlog 5ff118da). This was a function body copied into
# each verb that needed it: three when the packet was filed on
# 2026-10-18, nine on origin/main on 2026-11-14, seven of them byte-equal.
# CLAUDE.md §9a: one definition, not seven correct copies. A verb whose
# copy differs ON PURPOSE keeps it and says why on a line of its own,
# `# git-owner.sh not used: <reason>`; the_git_owner_bound_lives_once.rs
# holds every script under infra/forge and infra/gcp to one or the other.
#
# Every refusal stays the CALLER's: the exit code and the sentence are
# the verb's own, so these functions answer with a status.

# git_owner_of DIR — set OWNER, OWNER_UID and OWNER_HOME off the account
# that owns DIR. Returns 1, OWNER holding whatever stat said, when DIR
# has no owner this host can name: absent, UNKNOWN, or no such account.
git_owner_of() {
    OWNER="$(stat -c %U "$1" 2>/dev/null)"
    OWNER_UID="$(stat -c %u "$1" 2>/dev/null)"
    OWNER_HOME=""
    if [ -z "$OWNER" ] || [ "$OWNER" = "UNKNOWN" ] || ! id -u "$OWNER" >/dev/null 2>&1; then
        return 1
    fi
    OWNER_HOME="$(getent passwd "$OWNER" | cut -d: -f6)"
}

# as_owner COMMAND-STRING — run it under bash as $OWNER, keeping its exit
# status. Non-login, with the owner's HOME (where git finds the owner's
# global config and credential helper) and this PATH: a login profile's
# output would land in the caller's captured bytes. Skipped when this
# process already IS the owner, which is how the tests drive it.
as_owner() { # <command string>
    if [ "$(id -un)" = "$OWNER" ]; then
        bash -c "$1"
    else
        runuser -u "$OWNER" -- env HOME="${OWNER_HOME:-/}" PATH="$PATH" bash -c "$1"
    fi
}
