#!/bin/sh
# ops-credentials.sh — the ONE check of the credentials a
# cluster-operator host is declared to hold (design 1bc4b4ed; the
# boundary is design 835c0c9c: an admin kubeconfig and a talosconfig
# cannot be minted from anything the estate holds, so placing them is
# David's act, a scoped one is the credential broker's, and nothing in
# the estate's converge ever writes one). WHICH ones a host holds is
# declared per host in infra/estate/estate.toml (ops_credentials_declared).
#
# TWO READERS, ONE DEFINITION (CLAUDE.md §9a; backlog 714bc71f):
#   - install-cluster-operator.sh records it on the converge packet as
#     `ops_credentials` — a recorded not-ready, never a failure: no
#     converge can repair an absence only David can fill.
#   - observe-host.sh carries it on the host observation, where
#     `estate.compare` judges it against the host's DECLARED roles and
#     raises `ops_credentials_absent:<host>` for a cluster-operator that
#     lacks it. Until this reader existed the converge's field was read
#     by nobody, and the absence surfaced only as a twelve-hour red
#     every forge converge (car 78a88f65) with no alarm.
#
# SOURCED, POSIX sh: observe-host.sh runs under dash (#!/bin/sh), and
# infra/lint/a-sh-script-parses-under-sh.sh holds the shebang to it.
#
# ops_credentials_state <host-id>   (checks ONLY the set that host
#   declares — ops_credentials_declared below — or says `undeclared: …`)
#   Prints one line and returns 0, whatever it finds — it reports, the
#   caller decides:
#     present                                  both root:root 600
#     not ready: talosconfig:<why> kubeconfig:<why>
#                                              <why> is `absent`, or the
#                                              owner:group mode uid:gid
#                                              found
#     unmeasured: <dir> is not searchable by <user>, and the privileged
#                 door refused: <its own words>
#                                              this reader cannot tell —
#                                              neither directly nor
#                                              through the door below;
#                                              never "absent", which
#                                              would be a guess
#   The directory is $BOSS_OPS_DIR (default /etc/boss-ops), a knob only
#   so a test never reads /etc.
#
# AN UNSEARCHABLE DIRECTORY IS MEASURED THROUGH THE DOOR (backlog
# 6296dce1, 2026-09-29). The forge's observer runs as david, the
# directory is 0700 root, and so every forge host reading said
# `unmeasured` for a week: the `ops_credentials_absent` class was BLIND
# on the one host that owes the material, while the converge — root,
# reading this same function — recorded `present` each time. The same
# unit already reaches this directory through `sudo docker run --mount`
# (ops_kubectl, ops_talosctl below), so when the directory cannot be
# searched the check asks that door for exactly what it would have
# read itself: owner, group and mode of the two paths. The trust
# boundary is the command, because the door is root:
#   - `stat` and nothing else — never a byte of either file;
#   - the DIRECTORY is bind-mounted, read-only, because a missing file
#     must read `absent` (a file mount of it would be docker's refusal,
#     indistinguishable from the door's own), and `/ops/.` is stat'ed
#     first so a line for it proves the container saw inside: without
#     it the answer is the door's refusal, not two absences;
#   - `--network none`, and the image ops_kubectl already pulls;
#   - `sudo -n`, so a reader on a timer never waits at a prompt;
#   - bounded twice, because this runs inside the forge's OWN host
#     reading, the one the boarding check reads and the one a stalled
#     daemon (often a full disk's company) must not cost: `timeout -k 5`
#     host-side (BOSS_OPS_DOOR_TIMEOUT_S, default 30) escalates to KILL
#     when TERM is not enough, since `docker run` proxies the signal and
#     a CLI wedged on its daemon can outlive it; and `timeout 10` on stat
#     INSIDE the container, as ops_talosctl bounds talosctl, because
#     killing the host-side client leaves a container running (review
#     of 058b1ef1, M1). The directory's mode is never touched (design
#     835c0c9c).
# Names are the CONTAINER's reading of /etc/passwd, so the numbers ride
# beside them and are what is judged: uid 0 is root on both sides, and a
# file owned by any other host account is named by its uid, not by a
# name the image happens to give it (review of 058b1ef1, L2).
#
# EVERY STATUS HERE IS SWALLOWED ON PURPOSE. observe-host.sh runs this
# under `set -eu` (dash), which keeps -e inside a command substitution:
# a door that answers nonzero — stat's 1 for a missing credential, the
# very case this reading exists to report, or a sudo or docker refusal —
# would kill the function's subshell with nothing printed and take the
# whole host reading down with it, disk and all (review of 058b1ef1,
# H1). The words are the answer; `|| :` keeps them.
# The converge runs as root and never takes this branch, so one
# definition still answers both readers.
ops_credentials_state() {
    _oc_set="$(ops_credentials_declared "${1:-}")" || { echo "$_oc_set"; return 0; }
    _oc_dir="${BOSS_OPS_DIR:-/etc/boss-ops}"
    _oc_door=""
    if [ -d "$_oc_dir" ] && [ ! -x "$_oc_dir" ]; then
        _oc_door="$(timeout -k 5 "${BOSS_OPS_DOOR_TIMEOUT_S:-30}" sudo -n docker run --rm --network none \
            --mount "type=bind,src=$_oc_dir,dst=/ops,readonly" alpine/k8s:1.33.3 \
            timeout 10 stat -c '%n %U:%G %a %u:%g' /ops/. /ops/talosconfig /ops/kubeconfig 2>&1)" || :
        if [ -z "$(printf '%s\n' "$_oc_door" | sed -n '/^\/ops\/\. /p')" ]; then
            _oc_why="$(printf '%s\n' "$_oc_door" | awk 'NF { l = $0 } END { print l }')"
            echo "unmeasured: $_oc_dir is not searchable by $(id -un 2>/dev/null || id -u), and the privileged door refused: ${_oc_why:-no answer}"
            return 0
        fi
    fi
    _oc_missing=""
    for _oc_cred in $_oc_set; do
        _oc_f="$_oc_dir/$_oc_cred"
        if [ -n "$_oc_door" ]; then
            _oc_have="$(printf '%s\n' "$_oc_door" | sed -n "s|^/ops/$_oc_cred ||p")"
        elif [ -f "$_oc_f" ]; then
            _oc_have="$(stat -c '%U:%G %a %u:%g' "$_oc_f" 2>/dev/null)" || :
            # There but unstatable is not absent — say what was found.
            _oc_have="${_oc_have:-unstatable}"
        else
            _oc_have=""
        fi
        if [ -z "$_oc_have" ]; then
            _oc_missing="$_oc_missing $_oc_cred:absent"
        else
            case "$_oc_have" in
                *" 600 0:0") ;;
                *) _oc_missing="$_oc_missing $_oc_cred:$_oc_have" ;;
            esac
        fi
    done
    if [ -n "$_oc_missing" ]; then
        echo "not ready:$_oc_missing"
    else
        echo "present"
    fi
}

# WHICH CREDENTIALS A HOST IS DECLARED TO HOLD (design 835c0c9c,
# decision 3; backlog f371c749, 2026-09-29). The loop above checked the
# forge's set — a talosconfig and an admin kubeconfig — on every host,
# so boss-gcp read `talosconfig:absent kubeconfig:absent` for three days
# and its alarm told David to place both there by hand: a talosconfig on
# the public edge, which must never hold one, and a scoped kubeconfig
# the credential broker delivers (backlog 7336cb5f). The set is DATA
# now, the `[ops_credentials.<host>]` table of infra/estate/estate.toml,
# which estate.compare reads too (a test holds the two readings equal).
#
# ops_credentials_declared <host-id>
#   Prints the names the host's table declares, space-separated, status
#   0. Status 1, printing an `undeclared: …` state line instead, when no
#   host is named, the declaration ($BOSS_ESTATE_SOURCE — each caller
#   points it at the estate.toml beside itself) cannot be read, or it
#   holds no table for the host. Never an empty set: the loop above
#   would run zero times and answer `present`, the quiet wrong answer.
ops_credentials_declared() {
    _od_src="${BOSS_ESTATE_SOURCE:-}"
    case "${1:-}" in
        '') echo "undeclared: no host named — the check needs this host's estate id"; return 1 ;;
        *[!a-z0-9-]*) echo "undeclared: '$1' is not an estate node id"; return 1 ;;
    esac
    if [ -z "$_od_src" ] || [ ! -r "$_od_src" ]; then
        echo "undeclared: the estate declaration (BOSS_ESTATE_SOURCE=${_od_src:-unset}) is not readable, so $1's credential set is unknown"
        return 1
    fi
    _od_set="$(sed -n "/^\[ops_credentials\.$1\]\$/,/^\[/ s/^\([a-z][a-z0-9_-]*\) = .*/\1/p" "$_od_src" | tr '\n' ' ')"
    if [ -z "$_od_set" ]; then
        echo "undeclared: $_od_src declares no [ops_credentials.$1] table"
        return 1
    fi
    echo "${_od_set% }"
}

# THE ADMIN KUBECONFIG'S ONE PATH, AND HOW AN ACTING LOOP READS IT
# (backlog fb444bbb, car 1, 2026-09-28). Until this the forge held two
# admin kubeconfigs: this directory's, which the converge checks above,
# and /home/david/kc.yaml, which the watchdog's unit named and nothing
# checked — so a rotation here left the last safety net on the old
# credential. The watchdog and rollback-to now read only this one.
#
# ops_kubeconfig
#   Prints $BOSS_OPS_DIR/kubeconfig (default /etc/boss-ops/kubeconfig).
#
# ops_kubectl
#   Prints the kubectl an acting loop runs with it, one line to be
#   word-split. Reached ONLY through `sudo docker run`: the daemon mounts
#   the root:root 600 file as root, whoever the unit runs as. Two rules,
#   both about a reader that runs as david, who cannot search the 0700
#   directory and so cannot tell absent from unsearchable:
#   - `--mount`, never `-v`: an absent source is docker's refusal naming
#     the path, where `-v` would create it as a root-owned DIRECTORY in
#     /etc/boss-ops, in the way of the file David places next;
#   - no `[ -f ]` of the file in the caller: as david it answers "absent"
#     for a present credential and would switch the watchdog off every
#     tick. Docker's refusal is the signal; the caller must say it.
#
# ops_boss_image
#   deploy/boss's image, read through ops_kubectl — the read the
#   watchdog, rollback-to and the read-only check share. stdout the
#   image; stderr docker's or kubectl's own words; status theirs.
#   Bounded by --request-timeout: a half-dead API server that accepts
#   the connection and never answers held the watchdog's oneshot, and
#   the timer skipped every tick behind it (backlog cf321ffd).
#
# ops_image
#   The ONE image ops_kubectl and ops_talosctl run in: the forge
#   registry's mirror of alpine/k8s (mirror-base-images.sh puts it there,
#   and a test holds this tag to that list), never docker.io. WHY
#   (backlog cf321ffd). disk-floor-sweep.sh prunes the system daemon's
#   unused images older than 4h by CREATION time, so a months-old
#   alpine/k8s goes on nearly every hourly pass and the next tick pulls
#   again — from Docker Hub, until this, so a resolver or Hub stall
#   during an outage blinded the watchdog. The mirror is on the forge
#   itself, the host this runs on, reached with no DNS, and the system
#   daemon already pulls from it on every train (ci.yml's job images).
#   It IS a dependency the lever did not have: deploy/boss pulls
#   IfNotPresent, so a roll to the stamp usually needs no registry at
#   all, while a pull of this image needs Forgejo up. So the image is
#   also kept RESIDENT (ops_image_ready, below), and the pull is only
#   the first-time path, bounded and named when it fails (review of
#   8f50d314, findings L3 and M1). The host comes from /etc/boss/sor.env
#   (every caller's unit reads it; the scripts' forge-defaults.sh loads
#   it too); unset, it refuses by name and prints nothing, never a
#   Docker Hub fallback.
#
# ops_image_ready
#   Makes ops_image PRESENT and PINNED before an acting loop reads, and
#   prints one line saying which. PRESENT: `docker image inspect`, else
#   a pull bounded by BOSS_OPS_PULL_TIMEOUT_S (default 120) — a registry
#   that accepts the connection and stalls used to hold `docker run`'s
#   own pull until systemd killed the unit, which files nothing; now it
#   is a named refusal the caller reports as blind. PINNED: a stopped
#   container, boss-ops-image-pin, created from the image. The disk
#   sweep's `image prune -af` never removes an image a container
#   references, stopped or not, and nothing on the forge prunes
#   containers — so after the first pull the lever owes the registry
#   nothing. A pin it cannot make is said in the line ("NOT pinned")
#   and returns 0: the read can still go ahead, the caller decides.
#   Status 1, with the refusal and the hand fallback on stderr, when
#   the image cannot be made present.
#
# ops_image_fallback
#   The hand path when the mirror cannot serve: the upstream tag from
#   docker.io, retagged as the mirror name so every helper finds it. A
#   test holds the upstream name to mirror-base-images.sh's list.
ops_kubeconfig() {
    printf '%s\n' "${BOSS_OPS_DIR:-/etc/boss-ops}/kubeconfig"
}

ops_image() {
    if [ -z "${BOSS_FORGE_REGISTRY_HOST:-}" ]; then
        echo "ops-credentials: BOSS_FORGE_REGISTRY_HOST is not set, so there is no mirrored kubectl image to run (it comes from /etc/boss/sor.env, rendered from infra/estate/estate.toml; there is no Docker Hub fallback, backlog cf321ffd)" >&2
        return 1
    fi
    printf '%s/%s/alpine-k8s:1.33.3\n' "$BOSS_FORGE_REGISTRY_HOST" "${BOSS_FORGE_OWNER:-david}"
}

ops_image_fallback() {
    _of_img="$(ops_image 2>/dev/null)" || _of_img="<the mirror name: BOSS_FORGE_REGISTRY_HOST/david/alpine-k8s:1.33.3>"
    printf 'sudo docker pull docker.io/alpine/k8s:1.33.3 && sudo docker tag docker.io/alpine/k8s:1.33.3 %s\n' "$_of_img"
}

ops_image_ready() {
    _oir_img="$(ops_image)" || return 1
    _oir_pin=boss-ops-image-pin
    _oir_how=present
    if ! sudo -n docker image inspect "$_oir_img" >/dev/null 2>&1; then
        _oir_t="${BOSS_OPS_PULL_TIMEOUT_S:-120}"
        _oir_out="$(timeout "$_oir_t" sudo -n docker pull "$_oir_img" 2>&1)"
        _oir_rc=$?
        if [ "$_oir_rc" -ne 0 ]; then
            case "$_oir_rc" in 124) _oir_why="timed out after ${_oir_t}s" ;; *) _oir_why="exit $_oir_rc" ;; esac
            echo "ops-credentials: $_oir_img is not on this daemon and the pull from the forge registry failed ($_oir_why): $(printf '%s' "$_oir_out" | tail -3 | tr '\n' ' ')— by hand: $(ops_image_fallback)" >&2
            return 1
        fi
        _oir_how=pulled
    fi
    _oir_have="$(sudo -n docker container inspect -f '{{.Config.Image}}' "$_oir_pin" 2>/dev/null)"
    if [ "$_oir_have" != "$_oir_img" ]; then
        # A pin on an older tag is replaced; a create that loses a race
        # to another caller is fine if the pin it made is the right one.
        [ -z "$_oir_have" ] || sudo -n docker rm "$_oir_pin" >/dev/null 2>&1
        if ! _oir_out="$(sudo -n docker create --name "$_oir_pin" "$_oir_img" true 2>&1)"; then
            _oir_have="$(sudo -n docker container inspect -f '{{.Config.Image}}' "$_oir_pin" 2>/dev/null)"
            if [ "$_oir_have" != "$_oir_img" ]; then
                echo "$_oir_how $_oir_img; NOT pinned — the disk sweep's prune can remove it: $(printf '%s' "$_oir_out" | tr '\n' ' ')"
                return 0
            fi
        fi
    fi
    echo "$_oir_how $_oir_img, pinned by container $_oir_pin"
}

# With no image, both helpers print `false`: the caller word-splits the
# line and runs it, so an empty line would run its first argument and
# `docker run … kubectl` with no image would read `kubectl` as one to
# pull. `false` fails the read, and ops_image's refusal on stderr is
# the reason the caller records. `sudo -n`: a unit has no tty, so a
# password prompt already failed — now it fails saying so, as the disk
# sweep's does.
ops_kubectl() {
    _ok_img="$(ops_image)" || { echo false; return 0; }
    printf 'sudo -n docker run --rm --network host --mount type=bind,src=%s,dst=/kc,readonly %s kubectl --kubeconfig=/kc\n' "$(ops_kubeconfig)" "$_ok_img"
}

ops_boss_image() {
    $(ops_kubectl) get deploy boss -n boss --request-timeout=20s -o jsonpath='{.spec.template.spec.containers[0].image}'
}

# THE TALOSCONFIG, READ THE SAME WAY (backlog eeac3d56, 2026-09-29).
# The estate observer's free-space read left the cluster: it went through
# the kubelet as `get nodes/proxy`, a grant that also opens a WebSocket
# exec into any pod, and it now comes from the Talos API here, with the
# talosconfig David placed beside the kubeconfig. Same two rules as
# ops_kubectl — `sudo docker run`, `--mount` never `-v`, no `[ -f ]` in
# the caller — and ONE more: no second image and no second talosctl pin.
# The binary is the one install-cluster-operator.sh installs, pinned by
# sha to the version the cluster is within a minor of, mounted read-only
# into the image ops_kubectl already pulls (talosctl is a static Go
# binary). An absent binary is docker's refusal naming its path, exactly
# as an absent credential is.
#
# ops_talosctl
#   Prints the talosctl an observer runs, one line to be word-split,
#   bounded by `timeout` INSIDE the container (killing the host-side
#   sudo would leave the container running): BOSS_TALOS_TIMEOUT_S,
#   default 20. BOSS_TALOSCTL names the binary (default
#   /usr/local/bin/talosctl), a knob only so a test never reads /usr.
ops_talosctl() {
    _ot_img="$(ops_image)" || { echo false; return 0; }
    printf 'sudo -n docker run --rm --network host --mount type=bind,src=%s,dst=/tc,readonly --mount type=bind,src=%s,dst=/talosctl,readonly %s timeout %s /talosctl --talosconfig=/tc\n' \
        "${BOSS_OPS_DIR:-/etc/boss-ops}/talosconfig" "${BOSS_TALOSCTL:-/usr/local/bin/talosctl}" "$_ot_img" "${BOSS_TALOS_TIMEOUT_S:-20}"
}
