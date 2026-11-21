#!/usr/bin/env bash
# standby-banner.sh — what the standby shell prints before the prompt
# (design 0491591b, backlog 47601829).
#
# WHY. The standby (infra/cluster/manifests/boss-dev-standby.yaml) is the
# shell an operator reaches when the dev pod cannot be scheduled. On
# 2026-11-07 the dev pod sat Pending for about 80 minutes during the w-1
# drive window, and the reason ("0/5 nodes are available ...") was in
# the scheduler's events where nobody outside the LAN could read it. ssh
# cannot carry a reason with a refusal, so the reason is printed here: an
# operator who lands on the standby because the dev pod refused reads the
# cause in the first lines.
#
# WHAT IT READS, each bounded, and nothing else:
#   DEV POD   the pods labelled app=boss-dev: name, phase, node, and the
#             newest event recorded for each (kubectl, as the dev-session
#             ServiceAccount, whose Role reads pods and events in
#             boss-dev)
#   DOOR      the newest `door` comparison, the record the door alarm
#             reads (GET /api/estate/comparisons?scope=door&limit=1, the
#             read `boss orient` makes)
#   SESSIONS  the open work-sessions: how many, and the newest one
# A read that fails says so on its own line ("unavailable", with the
# error), and the banner still exits 0: a banner must never block the
# login it is printed for. If this file stops parsing, the standby's
# .profile skips it.
#
# Versioned in the tree and run from the standby's clone of main, so an
# edit rides a train and never rolls the pod. Pinned by
# crates/core/boss-testing/tests/standby_banner_sh.rs against a stub
# kubectl and boss-api.
#
# NOT EXECUTABLE, on purpose: it is not a door. Every executable in
# infra/dev/ is a door reached through /work/tools/bin and must source
# door-freshness.sh (a_door_knows_its_copy_is_stale.rs), and the first
# gate of this car went red on exactly that (gate b67bf233). This file is
# never on a PATH: the standby's .profile runs it as `bash <path>` after
# taking main, and its pin runs it the same way, so mode 644 states what
# it is: a file a caller reads, like door-freshness.sh and
# wt-target-dir.sh beside it, which their doors source.

NS=boss-dev
say() { printf '  %s\n' "$*"; }

# EVERY LINE THE BANNER PRINTS PASSES THROUGH clean (review run 75a760c9,
# finding 7). An event message and a work-session title are text other
# actors wrote, printed into a root terminal, where an ESC is a command
# rather than a character. [[:cntrl:]] under a UTF-8 locale is C0, DEL and
# the C1 range (an 8-bit CSI is U+009B); under plain C, should the image
# lack C.UTF-8, it is still C0 and DEL, which holds ESC. One filter on the
# whole output, so a read added later cannot forget it.
clean() { LC_ALL=C.UTF-8 sed 's/[[:cntrl:]]//g'; }

{
echo "BOSS standby shell on ${HOSTNAME:-this pod}, $(date -u +%Y-%m-%dT%H:%M:%SZ)"
say "This pod builds nothing. It reads the record and the cluster from inside the LAN."

# DEV POD. timeout bounds each kubectl call; kubectl takes NO flag that
# overrides its config. The standby has no kubeconfig file and reaches the
# cluster through client-go's in-cluster fallback, which is taken only
# when no override is set: --request-timeout=10s here made kubectl dial
# localhost:8080 instead, and the banner read "unavailable" on every login
# (backlog 41e90111, 2026-11-21).
pods=$(timeout 15 kubectl get pods -n "$NS" -l app=boss-dev \
    -o jsonpath='{range .items[*]}{.metadata.name} {.status.phase} {.spec.nodeName}{"\n"}{end}' 2>&1)
rc=$?
if [ "$rc" -ne 0 ]; then
    say "DEV POD: unavailable (kubectl exit $rc: $(printf '%s\n' "$pods" | tail -n 1))"
elif [ -z "$pods" ]; then
    say "DEV POD: NONE. No pod carries app=boss-dev; read the Deployment with kubectl get deploy -n $NS boss-dev"
else
    while read -r name phase node; do
        [ -n "$name" ] || continue
        say "DEV POD: $name $phase on ${node:-no node}"
        # Sorted by creation, so the tail is the newest event recorded. The
        # scheduler records through events.k8s.io and leaves lastTimestamp
        # null (14 of 14 in boss-dev, measured 2026-11-15 by review run
        # 75a760c9, finding 5), and kubectl leaves null-keyed rows in API
        # order, so the old sort was right only by a naming coincidence.
        events=$(timeout 15 kubectl get events -n "$NS" --field-selector "involvedObject.name=$name" \
            --sort-by=.metadata.creationTimestamp \
            -o jsonpath='{range .items[*]}{.reason}: {.message}{"\n"}{end}' 2>&1)
        erc=$?
        last=$(printf '%s\n' "$events" | tail -n 1)
        if [ "$erc" -ne 0 ]; then
            say "  last event: unavailable (kubectl exit $erc: $last)"
        elif [ -n "$last" ]; then
            say "  last event: $last"
        else
            say "  last event: none recorded"
        fi
    done <<< "$pods"
fi

# DOOR. The comparison the forge's observer posted and estate.compare
# judged. The banner never probes a door itself: that would be a second
# judgement beside the recorded one (crates/orchestrators/boss-cli/src/door.rs).
# boss-api writes the body to stdout and `HTTP:<code>` (or curl's error)
# to stderr, so stderr is kept for the line that says why a read failed.
err=$(mktemp) || err=/dev/null
trap '[ "$err" = /dev/null ] || rm -f "$err"' EXIT
if body=$(timeout 20 boss-api GET '/api/estate/comparisons?scope=door&limit=1' 2>"$err") && [ -n "$body" ]; then
    printf '%s\n' "$body" | jq -r '
        (.data[0].payload // null) as $c
        | if $c == null then "  DOOR: NOT WATCHED. No door comparison is recorded."
          else
            "  DOOR: observed \($c.observed_at // "at an unread time")",
            ($c.doors[]? | "    \(.id): " + ([.halves[]? | "\(.half) \(.target) "
                + (if .open then "open"
                   else "DARK since \(.dark_since // "an unread time")"
                        + (if .reason then " (\(.reason))" else "" end) end)] | join("; ")))
          end' 2>/dev/null || say "DOOR: unreadable (the answer was not the comparison's shape)"
else
    say "DOOR: unavailable ($(tail -n 1 "$err" 2>/dev/null))"
fi

# SESSIONS. Who was working when the dev pod went away.
if body=$(timeout 20 boss-api GET '/api/jobs?kind=work-session&status=open&limit=1' 2>"$err") && [ -n "$body" ]; then
    printf '%s\n' "$body" | jq -r '
        "  SESSIONS: \(.total // 0) open"
        + (if (.data | length) > 0
           then "; newest: \(.data[0].title), last active \(.data[0].metadata.last_active_at // "never")"
           else "" end)' 2>/dev/null || say "SESSIONS: unreadable (the answer was not a job list)"
else
    say "SESSIONS: unavailable ($(tail -n 1 "$err" 2>/dev/null))"
fi

say "Doors here: kubectl (namespace $NS), boss-api. The tree is /standby/boss."
} 2>&1 | clean
exit 0
