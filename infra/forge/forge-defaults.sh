# forge-defaults.sh — the defaults every forge verb used to carry alone.
#
# Source it after `set -u`, then name the image repos the verb reads:
#
#   . "$(dirname "$0")/forge-defaults.sh"
#   forge_need REGISTRY            # and/or CI_IMAGE_REPO, REGISTRY_BASE
#
# WHY ONE FILE. The audit of 2026-09-18 (§1 "forge script defaults",
# backlog 5222163e) counted the image repo the converge pushes in four
# verbs, the converge's stamp file in three, the hold file in two and
# the postgres target in four — each a literal, three of them pinned
# one-way by a test and two by nothing. A fact that lives twice gets an
# equality test; a fact that lives once needs none (CLAUDE.md §9a). This
# is the infra/files-root.sh pattern: the value is defined here, the
# verbs read it, and a verb that needs a different one sets the variable
# before sourcing.
#
# THE REGISTRY HOST comes from /etc/boss/sor.env through infra/lib/sor.sh
# (the one tree source is infra/estate/estate.toml). A verb that runs
# where that file is absent — a test, a workstation — names its image
# repo explicitly (BOSS_FORGE_REGISTRY, BOSS_CI_IMAGE_REPO,
# BOSS_FORGE_REGISTRY_BASE) or is refused by name when it asks; there is
# no literal to fall back on. The image repos are resolved on demand
# (`forge_need`) rather than at source time, so a verb that never
# touches the registry — converge-hold, the census — is never refused
# for want of it.
#
# EVERY NAME BELOW KEEPS THE OVERRIDE IT HAD: the tests and the units
# that set BOSS_FORGE_REGISTRY, REGISTRY, BOSS_FORGE_LAST_BUILT,
# BOSS_CONVERGE_HOLD, BOSS_REGISTRY_PUSH_DIR and the rest still win.

. "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib/sor.sh"

# The forge user whose repositories hold every image.
FORGE_OWNER="${BOSS_FORGE_OWNER:-david}"

# forge_need NAME... — resolve an image repo from its override, else
# from the env file's registry host; refuse by name when neither says.
#
#   REGISTRY       the image the converge pushes and the watchdog /
#                  rollback roll to. `REGISTRY` is the watchdog's and
#                  rollback-to's spelling of the override; the converge's,
#                  the sweep's and the prune's is BOSS_FORGE_REGISTRY.
#                  Either wins; both are set on return.
#   CI_IMAGE_REPO  the CI runner image (one tag per train, pruned by
#                  the sweep); override BOSS_CI_IMAGE_REPO.
#   REGISTRY_BASE  the owner's namespace on the registry — where the
#                  mirrored bases live and where build.sh pushes the
#                  runner; override BOSS_FORGE_REGISTRY_BASE (mirror) or
#                  BOSS_CI_REGISTRY (build.sh).
forge_need() {
    local n
    for n in "$@"; do
        case "$n" in
            REGISTRY)
                if [ -n "${BOSS_FORGE_REGISTRY:-}" ]; then
                    REGISTRY="$BOSS_FORGE_REGISTRY"
                elif [ -z "${REGISTRY:-}" ]; then
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    REGISTRY="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER/boss"
                fi
                BOSS_FORGE_REGISTRY="$REGISTRY" ;;
            CI_IMAGE_REPO)
                if [ -z "${BOSS_CI_IMAGE_REPO:-}" ]; then
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    BOSS_CI_IMAGE_REPO="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER/boss-ci"
                fi
                CI_IMAGE_REPO="$BOSS_CI_IMAGE_REPO" ;;
            REGISTRY_BASE)
                if [ -n "${BOSS_FORGE_REGISTRY_BASE:-}" ]; then
                    REGISTRY_BASE="$BOSS_FORGE_REGISTRY_BASE"
                elif [ -n "${BOSS_CI_REGISTRY:-}" ]; then
                    REGISTRY_BASE="$BOSS_CI_REGISTRY"
                else
                    sor_require BOSS_FORGE_REGISTRY_HOST
                    REGISTRY_BASE="$BOSS_FORGE_REGISTRY_HOST/$FORGE_OWNER"
                fi ;;
            *)
                echo "forge-defaults: no default named $n" >&2; exit 1 ;;
        esac
    done
}

# EVERYTHING THE CLUSTER CONVERGE KEEPS, under one directory root owns
# (backlog 683d7fe9, car 2; design c8502e17, question image-build, and
# position B of the forge sudo decision, David 2026-10-07). The converge
# ran as david and kept its stamps under his home, so the watchdog — root
# — read a file another account wrote, and every verb that wanted the
# stamp derived that home from the checkout's owner. It runs as root from
# root's tree now, and cluster-deploy-runner.service makes this directory
# (StateDirectory=boss/converge, 0700: every reader is root) before
# every start.
#
#   last-built         the last build the converge rolled and verified —
#                      what the watchdog and rollback-to roll to BY NAME
#   last-failed        a head whose BOOT failed: quarantined until main
#                      moves (and last-failed.<namespace> per instance)
#   last-build-failed  a head whose BUILD failed: retried, but the log
#                      says which attempt this is
#   requests           the ops-requests a `converge` asked for, answered
#                      when the run ends (converge-now.sh writes it)
#   tenants/           each repo-sourced instance's tenant checkout
#   alert-spool/       what the converge's alert door kept, replayed by
#                      the converge itself on every tick — the RUNNER's
#                      default, whoever starts it (review 1ba2873d, B2:
#                      started without its unit's environment, alert-lib.sh
#                      fell back to /var/tmp, which every account can write,
#                      and root replayed what was there with the machine
#                      token). The unit names the same path, because
#                      probe-account.sh reads the spools off the units; the
#                      two are held equal by
#                      the_converge_runs_as_root_from_roots_tree.rs.
#   run-tree.<pid>/    the view of root's tree each run executes from
CONVERGE_STATE="${BOSS_FORGE_CONVERGE_STATE:-/var/lib/boss/converge}"
STAMP_FILE="${BOSS_FORGE_LAST_BUILT:-$CONVERGE_STATE/last-built}"
FAILED_FILE="${BOSS_FORGE_LAST_FAILED:-$CONVERGE_STATE/last-failed}"
TENANTS_DIR="${BOSS_FORGE_TENANTS_DIR:-$CONVERGE_STATE/tenants}"
CONVERGE_ALERT_SPOOL="$CONVERGE_STATE/alert-spool"

# THE REGISTRY PUSH LOGIN: the docker config the forge converge's
# registry-token-deposit.sh writes from the broker-minted token (Secret
# boss/forge-registry-push-token; backlog 683d7fe9, car 1), root:root
# 0700 and 0600. The converge pushes with it, the sweeps verify a tag
# against the registry with it, and the registry prune reads its token.
# It replaced the login the operator once ran by hand into his own
# ~/.docker/config.json, which no registry row named and no rotation
# reached.
REGISTRY_PUSH_DIR="${BOSS_REGISTRY_PUSH_DIR:-/etc/boss/registry-push}"

# An operator's hand on the converge (converge-hold.sh). A FIXED path,
# not $HOME: the ops runner executes verbs as root with no HOME in its
# environment (2026-09-05: "HOME: unbound variable" on the first
# release-converge), and the converge reads the hold as david — two
# homes would be two files.
#
# UNDER /var/lib/boss, NOT /var/tmp (backlog d94d287e, 2026-09-28). The
# hold lived in /var/tmp because both accounts could write there — and
# so could every other account: /var/tmp is sticky and world-writable,
# so anyone on the forge could create the hold first (holding every
# converge) or plant a symlink the root verbs then read through. The
# hold is written by ROOT (the ops runner) and READ by the converge
# (root too, since backlog 683d7fe9), so it belongs in a directory root
# owns: install.sh makes
# /var/lib/boss root:root 0755 on every converge (converge-hold.sh
# prepare, which also carried a hold standing at the old path across,
# once). Survives a reboot, as /var/tmp did.
HOLD_FILE="${BOSS_CONVERGE_HOLD:-/var/lib/boss/converge-hold}"

# The instance database, as infra/cluster/manifests/boss.yaml declares
# it: the StatefulSet `postgres`, container `postgres`, POSTGRES_USER=
# boss. The namespace is each verb's argument; the database is the
# instance Secret's. tenant_census_sh.rs and switch_instance_database_
# sh.rs hold these words equal to the manifest (§9a).
PG_WORKLOAD="sts/postgres"
PG_CONTAINER="postgres"
PG_USER="boss"
