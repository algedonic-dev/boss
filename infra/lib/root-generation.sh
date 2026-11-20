# root-generation.sh — is this tree a generation of root's tree, and is it
# still, byte for byte, the commit it says it is?
#
# Source it, then prove the tree before reading a byte of it:
#
#   . "$HERE/../lib/root-generation.sh"
#   root_generation_proved "$TREE" "$TMP" \
#       || refuse "$TREE is no declaration set for root (above)"
#   TREE="$GEN_TREE"      # physical; GEN_SHA is its commit, GEN_STORE the store
#
# WHO NEEDS IT. A verb that runs as root and DERIVES what it may delete
# from a tree: delete-orphan-object, retire-volume-claim and
# retire-access-object, each of which asks infra/cluster/undeclared-
# objects.sh --check whether a manifest declares the object. The tree is
# their authority, so it must be content somebody reviewed.
#
# WHY NOT A CLEAN CHECKOUT (backlog 1dde630c for delete-orphan-object;
# review a04e64e9 P1 for the two retire verbs, 2026-11-15). A checkout is
# owner-editable, and `git status` run as that owner is the owner's
# testimony, not proof: review a04e64e9 committed a manifest declaring the
# target, marked it skip-worktree, replaced its bytes with an unrelated
# ConfigMap, and both retire verbs then rendered a deletion-authorising
# plan from a tree git called clean. The derivation also reads more than
# the manifests (instances.toml, the renderer's RENDERS line, the gate
# runner, the dev and isolation declarations), any of which can be edited
# or replaced by a symlink root then follows.
#
# WHAT A GENERATION IS. Root's own tree (infra/forge/root-tree.sh, design
# c98c79aa): one root-owned export per commit main named, gen/<sha>,
# beside root's own bare store, repo.git, never written again. So the
# tree must BE such a generation — its name a commit, its `.boss-tree-sha`
# saying the same, the store beside it — every directory of the layout
# this uid's and writable by nobody else, and then it is PROVED to be
# that commit against the store: every tracked path matches its blob
# (content, mode and type, so a symlink or an exec bit is a difference)
# and nothing but `.boss-tree-sha` is present that the commit does not
# carry. The store is read with no account's config, no system config,
# no hook, and through an index in the caller's scratch, so nothing in the
# store is written, and nothing an account controls answers for it.
#
# NOT git-owner.sh. That lib drops to a checkout's owner so root can READ
# a clone it does not own; it proves nothing about the bytes, and is not
# deletion authority. Root's git in root's store needs no drop at all.
#
# The refusal stays the CALLER's — its exit code and its sentence — so
# this answers with a status and puts the evidence on stderr: reasons
# indented two, paths indented four.

# root_generation_proved TREE SCRATCH
#   0  TREE is a generation of root's tree, proved against its store;
#      sets GEN_TREE (TREE, physical), GEN_SHA (its commit), GEN_STORE
#   1  it is not, or it could not be proved; the reason on stderr
# SCRATCH is a directory of the caller's own run; the index goes there.
root_generation_proved() {
    local tree="$1" scratch="$2" sha gen_dir root_tree store d owner_uid me_uid extra
    GEN_TREE="" GEN_SHA="" GEN_STORE=""
    case "$tree" in
        *[[:space:]\'\"]*)
            echo "  the tree path \`$tree\` contains whitespace or a quote; name one without" >&2
            return 1
            ;;
    esac
    if [ ! -d "$tree" ]; then
        echo "  the tree $tree does not exist, so there is no declaration set to read" >&2
        return 1
    fi
    tree="$(cd "$tree" && pwd -P)" || { echo "  cannot resolve the tree $1" >&2; return 1; }
    sha="${tree##*/}"
    gen_dir="${tree%/*}"
    root_tree="${gen_dir%/*}"
    store="$root_tree/repo.git"
    if ! [[ "$sha" =~ ^[0-9a-f]{40,64}$ ]] || [ "${gen_dir##*/}" != gen ] || [ ! -d "$store" ]; then
        echo "  $tree is not a generation of root's tree: <tree>/gen/<sha> with <tree>/repo.git" >&2
        echo "  beside it (infra/forge/root-tree.sh). A checkout, however clean, is a tree its owner can" >&2
        echo "  edit between this check and the derivation's read, so it is no declaration set for root." >&2
        return 1
    fi
    if [ "$(cat -- "$tree/.boss-tree-sha" 2>/dev/null)" != "$sha" ]; then
        echo "  $tree/.boss-tree-sha does not say it is ${sha:0:12} — not a generation root-tree.sh made" >&2
        return 1
    fi
    me_uid="$(id -u)"
    for d in "$root_tree" "$gen_dir" "$tree" "$store"; do
        owner_uid="$(stat -c %u -- "$d" 2>/dev/null)"
        if [ "$owner_uid" != "$me_uid" ] || [ -L "$d" ]; then
            echo "  $d is owned by uid ${owner_uid:-unknown}, not this uid ($me_uid), or is a symlink — another account could rewrite the declaration set under this run" >&2
            return 1
        fi
        if [ -n "$(find "$d" -maxdepth 0 -perm /0022 2>/dev/null)" ]; then
            echo "  $d is writable by its group or by anyone ($(stat -c %A -- "$d")) — another account could rewrite the declaration set under this run" >&2
            return 1
        fi
    done
    if ! root_store_git "$store" "$tree" "$scratch" cat-file -e "$sha^{commit}" 2>"$scratch/git.err"; then
        echo "  the store $store does not hold the commit ${sha:0:12} this generation names, so nothing proves what it is: $(tr '\n' ' ' <"$scratch/git.err")" >&2
        return 1
    fi
    if ! root_store_git "$store" "$tree" "$scratch" read-tree "$sha" 2>"$scratch/git.err"; then
        echo "  cannot read the commit ${sha:0:12} out of $store: $(tr '\n' ' ' <"$scratch/git.err")" >&2
        return 1
    fi
    # Hashes every tracked file against its blob; a path that differs stays
    # unrefreshed and is listed below. Its own exit says only that some did.
    root_store_git "$store" "$tree" "$scratch" update-index -q --refresh >/dev/null 2>&1
    if ! root_store_git "$store" "$tree" "$scratch" diff-files --name-only >"$scratch/differs" 2>"$scratch/git.err"; then
        echo "  cannot compare $tree with its commit: $(tr '\n' ' ' <"$scratch/git.err")" >&2
        return 1
    fi
    if ! root_store_git "$store" "$tree" "$scratch" ls-files --others >"$scratch/others" 2>"$scratch/git.err"; then
        echo "  cannot list what $tree holds beyond its commit: $(tr '\n' ' ' <"$scratch/git.err")" >&2
        return 1
    fi
    # grep's 1 is "nothing else is there"; only a 2 is a read that failed.
    extra="$(grep -vxF .boss-tree-sha "$scratch/others")" || [ $? -eq 1 ] || {
        echo "  cannot read the list of what $tree holds beyond its commit" >&2
        return 1
    }
    if [ -s "$scratch/differs" ] || [ -n "$extra" ]; then
        echo "  $tree is not the commit ${sha:0:12} it says it is, so the tree that would prove" >&2
        echo "  this object undeclared is not one anybody reviewed. Differs from the commit:" >&2
        sed 's/^/    /' "$scratch/differs" >&2
        [ -z "$extra" ] || { echo "  Not in the commit at all:" >&2; printf '%s\n' "$extra" | sed 's/^/    /' >&2; }
        echo "  A proved generation must remain unchanged after export; these paths differ from its record." >&2
        return 1
    fi
    GEN_TREE="$tree" GEN_SHA="$sha" GEN_STORE="$store"
}

# root_store_git STORE TREE SCRATCH ARGS... — root's git in root's store:
# no account's config, no system config, no hook, and an index of this
# run's own, so nothing in the store is written.
root_store_git() {
    local store="$1" tree="$2" scratch="$3"
    shift 3
    GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_INDEX_FILE="$scratch/index" \
        git -c core.hooksPath=/dev/null --git-dir="$store" --work-tree="$tree" "$@"
}
