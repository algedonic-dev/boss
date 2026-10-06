#!/usr/bin/env bash
#
# publish-pr-state.sh — ASK GITHUB WHAT BECAME OF EACH PUBLISH PULL
# REQUEST, and write its answer onto the PR's own packet as `pr_state`.
# SOURCED, not run: it defines `publish_pr_states <read_by>`.
#
# One definition for both readers (CLAUDE.md 9a):
#
#   * publish-github-pr.sh --measure (the daily mirror-drift verb), where
#     this pass was born (backlog a5d4322c: the region called merged #239
#     open for 86 hours because nothing had ever asked GitHub);
#   * read-publish-checks.sh, filed every fifteen minutes by
#     reread-publish-pr-every-15-minutes (backlog 663589cd).
#
# WHY THE SECOND READER (backlog 663589cd, David 2026-09-26: "I closed
# the PR because it was red and we don't seem to have a way to correct
# red PRs ... Not sure why the alarm would stay going off this whole
# time"). Once a day was the only cadence this question had, so the
# publish region said "#244 has been open 46 hours (GitHub read it open
# at 2026-09-26T00:00:55Z)" for 22 hours after the PR was unmergeable,
# and a close or a merge was not seen for up to a day. The re-read rule
# already filed a forge verb against the same public API; it now asks
# this too, so a close shows within the quarter hour.
#
# WHAT IT ASKS, per publish packet whose open-pr recorded a pull request
# and whose `pr_state` does not already read closed:
#
#   * GET pulls/<n> — state, merged, merged_at, closed_at, the head sha;
#   * while OPEN, GET commits/<head>/check-runs — the verdict over every
#     check (`checks`: failure | running | success | none), the ones
#     that completed without passing named in `failing` and the ones
#     still running in `still_running`. The step's own `read-checks`
#     reading is frozen at the moment it completed; this is what the PR
#     says NOW, and it is what lets the region call a red PR red (#244's
#     reading recorded its Gate in_progress, and the Gate then failed);
#   * when CLOSED WITHOUT A MERGE, `unmerged_reason`, from what the
#     record already holds: superseded by a newer publish PR (the verb's
#     own `pr_superseded`), closed over checks judged real, closed with
#     checks failing, or — said, never implied — nothing on the record
#     says why. GitHub's pulls API carries no reason of its own.
#
# No credential: the mirror is public. A PR GitHub does not answer for
# is NAMED and left unread — the region then says "never read", not
# "open". A write the jobs API refuses is a failure.
#
# Inputs: BASE (the jobs API), BOSS_USER, MT_HDR (the caller's
# `machine_token_header MT_HDR …` — the machine token rides to curl as a
# 0600 file, never in its argv, backlog 5f3ad356; empty when this host
# holds none or BASE is not an estate host, infra/lib/secret-header.sh),
# GITHUB_API, MIRROR_SLUG, workdir; and the caller's say / fail.

publish_pr_states() {
    local read_by="$1" listed listed_total row pr_job pr_url pr_number head
    # A caller that never asked for the header would send the writes
    # below without the token whether or not its host holds one; say so
    # rather than be tallied, and later refused, unexplained.
    if [ -z "${MT_HDR+set}" ]; then
        fail "the caller made no machine_token_header MT_HDR call (infra/lib/secret-header.sh) — the pr-state writes would go out without the machine token"
    fi
    if ! curl -fsS -H "x-boss-user: $BOSS_USER" \
            "$BASE/api/jobs?kind=publish-to-github&limit=60&full=true" > "$workdir/prs-published" 2>"$workdir/prs-err"; then
        fail "jobs API unreachable at $BASE — $(cat "$workdir/prs-err")"
    fi
    # Everything the record holds that the reason is chosen from rides
    # the row, so the reason is read, not re-derived later.
    jq -c 'def step($slug): ((.steps // []) | map(select(.spec_slug == $slug)) | .[0]) // {};
        (if type == "object" and has("data") then .data else . end)
        | .[] | . as $j
        | (step("open-pr").metadata // {}) as $open
        | ($open.pr_url // "") as $url
        | select($url != "")
        | (($j.metadata.pr_state // {}) | if .pr_url == $url then . else {} end) as $was
        | select(($was.state // "") != "closed")
        | {id: $j.id, url: $url, snapshot: ($open.snapshot_commit // ""),
           superseded_by: ((($j.metadata.pr_superseded // {}) | select(.pr_url == $url) | .by_pr_url) // ""),
           verdict: (step("judge-checks").metadata.verdict // ""),
           real: ([(step("judge-checks").metadata.dispositions // [])[]
                   | select(.disposition == "real") | .rule] | join("; ")),
           conclusion: (step("read-checks").metadata.conclusion // ""),
           failing: (if ($was.failing // "") != "" then $was.failing
                     else (step("read-checks").metadata.failing // "") end)}' \
        "$workdir/prs-published" > "$workdir/prs-unsettled" 2>"$workdir/prs-err" \
        || fail "the jobs API answered something this verb cannot read as a packet list — $(head -c 200 "$workdir/prs-err" | tr '\n' ' ')"
    # A limit is not a filter: say when the page did not reach the tail.
    listed=$(jq -r '(if type == "object" and has("data") then .data else . end) | length' "$workdir/prs-published")
    listed_total=$(jq -r '.total? // empty' "$workdir/prs-published")
    case "${listed_total:-empty}" in
        empty|*[!0-9]*) ;;
        *) [ "$listed_total" -le "$listed" ] \
            || say "pr-state: read $listed of $listed_total publish packets — the $((listed_total - listed)) oldest were not asked about" ;;
    esac
    while IFS= read -r row; do
        pr_job=$(printf '%s' "$row" | jq -r '.id')
        pr_url=$(printf '%s' "$row" | jq -r '.url')
        pr_number="${pr_url##*/}"
        case "${pr_number:-empty}" in
            empty|*[!0-9]*)
                say "pr-state: ${pr_job:0:8} recorded '$pr_url', which is not a pull request url — its state stays never read"
                continue ;;
        esac
        if ! curl -fsS -H "accept: application/vnd.github+json" \
                "$GITHUB_API/repos/$MIRROR_SLUG/pulls/$pr_number" > "$workdir/prs-pr" 2>"$workdir/prs-err"; then
            say "pr-state: GitHub did not answer for $pr_url — $(head -c 200 "$workdir/prs-err" | tr '\n' ' '); its state stays never read"
            continue
        fi
        # The checks, only while the PR is open: a closed PR's checks
        # change nothing a person can do.
        echo '{}' > "$workdir/prs-checks"
        if [ "$(jq -r '.state // ""' "$workdir/prs-pr" 2>/dev/null)" = "open" ]; then
            head=$(jq -r --arg s "$(printf '%s' "$row" | jq -r '.snapshot')" '.head.sha // $s' "$workdir/prs-pr")
            case "${head:-empty}" in
                empty|*[!0-9a-f]*)
                    say "pr-state: $pr_url names no head sha — its checks stay unread" ;;
                *)
                    if curl -fsS -H "accept: application/vnd.github+json" \
                            "$GITHUB_API/repos/$MIRROR_SLUG/commits/$head/check-runs?per_page=100" \
                            > "$workdir/prs-runs" 2>"$workdir/prs-err" \
                        && jq -c --arg head "$head" '
                            [.check_runs[]?] as $r
                            | [$r[] | select(.status == "completed"
                                            and ((.conclusion // "none") | IN("success", "neutral", "skipped") | not))
                                    | "\(.name): \(.conclusion // "none")"] as $failing
                            | [$r[] | select(.status != "completed") | .name] as $running
                            | {head: $head,
                               checks: (if ($r | length) == 0 then "none"
                                        elif ($failing | length) > 0 then "failure"
                                        elif ($running | length) > 0 then "running"
                                        else "success" end),
                               failing: ($failing | join("; ")),
                               still_running: ($running | join(", "))}' \
                            "$workdir/prs-runs" > "$workdir/prs-checks" 2>"$workdir/prs-err"; then
                        :
                    else
                        echo '{}' > "$workdir/prs-checks"
                        say "pr-state: GitHub's check-runs for $pr_url (head ${head:0:12}) could not be read — $(head -c 200 "$workdir/prs-err" | tr '\n' ' '); its checks stay unread"
                    fi ;;
            esac
        fi
        if ! jq -c --arg url "$pr_url" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg by "$read_by" \
                --argjson row "$row" --slurpfile checks "$workdir/prs-checks" '
                select(.state == "open" or .state == "closed")
                | {pr_state: ({pr_url: $url, number, state, merged: (.merged == true),
                               merged_at, closed_at, read_at: $ts, read_by: $by}
                              + $checks[0]
                              + (if .state == "closed" and .merged != true then
                                   {unmerged_reason: (
                                      if $row.superseded_by != "" then "superseded by \($row.superseded_by)"
                                      elif $row.verdict == "real" then "closed over its red checks — judged real: \($row.real)"
                                      elif $row.failing != "" then "closed with checks failing: \($row.failing)"
                                      elif $row.conclusion != "" and $row.conclusion != "success" then "closed over a checks reading of \($row.conclusion)"
                                      else "closed on GitHub without a merge; nothing on the record says why" end)}
                                 else {} end))}' \
                "$workdir/prs-pr" > "$workdir/prs-state" 2>"$workdir/prs-err" || [ ! -s "$workdir/prs-state" ]; then
            say "pr-state: GitHub's answer for $pr_url carries no open/closed state — its state stays never read"
            continue
        fi
        if ! curl -fsS -X PATCH -H "content-type: application/json" -H "x-boss-user: $BOSS_USER" \
                ${MT_HDR:+-H "$MT_HDR"} \
                --data-binary @"$workdir/prs-state" \
                "$BASE/api/jobs/$pr_job/metadata" > /dev/null 2>"$workdir/prs-err"; then
            fail "annotating ${pr_job:0:8} with the state of $pr_url failed — $(head -c 300 "$workdir/prs-err" | tr '\n' ' ')"
        fi
        say "pr-state: $pr_url is $(jq -r '.pr_state | "\(.state), merged=\(.merged)"
            + (if .checks then ", checks \(.checks)" else "" end)
            + (if (.failing // "") != "" then " (failing: \(.failing))" else "" end)
            + (if .unmerged_reason then " — \(.unmerged_reason)" else "" end)' "$workdir/prs-state") — written onto ${pr_job:0:8}"
    done < "$workdir/prs-unsettled"
}
