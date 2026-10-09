#!/usr/bin/env bash
# gate-window-is-clean.sh <gate> [hours] < the gate-window answer
#
# The machine reader of a refusing flip's evidence. It judges ONE JSON
# document on stdin — the answer of
#   GET /api/events/gate-window?gate=<gate>&hours=<hours>
# — and exits 0 only when every half of the window is there and clean.
#
# WHY (design 21946380, decision 4; rows C and D of design b08725c2).
# A gate moves from `report` to `enforce` on a clean window read from the
# log AND from each live process, and "the flip refuses on any half that
# is missing, unreadable or short". Until row D's car (backlog b8e75382,
# 2026-10-06) the only program that read that answer was the hourly
# alarm, over two hours; the 72-hour answer was read by a person, off a
# document. `covers_requested_window: true` is one field of that
# document. A live tally's `overflow` — the key cap reached, so real
# refusals went uncounted (checklist F2 of review 1c2860f4) — is another,
# three screens down. This reads all of them, every time, the same way.
#
# HOW IT IS RUN. It has no reader of its own, so it is the same judge on
# every host, and the bytes judged can be the bytes recorded:
#
#   on the dev pod, before a refusing car is released:
#     boss-api GET '/api/events/gate-window?gate=policy-check&hours=72' > window.json
#     bash infra/ops/gate-window-is-clean.sh policy-check 72 < window.json
#   and window.json goes onto the flip's packet as the receipt, copied.
#
#   in a recorded probe, on the forge:
#     boss-sor-read '/api/events/gate-window?gate=policy-check&hours=72' \
#         | bash infra/ops/gate-window-is-clean.sh policy-check 72
#
# A reader that failed hands the pipe nothing, and nothing is refused
# (exit 2) — never passed: jq-1.6 exits 0 over no document at all
# (infra/lib/jq.sh), which is why the document is asked for first.
#
# WHAT CLEAN MEANS, each line a refusal when it does not hold:
#   * the answer is about <gate>, and its window (`now` - `from`) is at
#     least <hours> long — a one-hour read that covers its own request
#     is not a 72-hour window;
#   * `covers_requested_window` is true and `clean_since` is stated;
#   * the log half was read (`log`, no `log_error`), holds no dirty fact
#     (`log.dirty`: a would-refuse, a refusal or a tally overflow) and
#     names no gap (`log.not_clean`);
#   * the join names no gap (`not_clean`) and no `input_errors`;
#   * at least one service is required, and EVERY required service has a
#     coverage row with no `gap`, and a live row that was read (`error`
#     null, a `snapshot`) whose mode is recording (`report` or `enforce`,
#     no `mode_error`), whose `rows` are empty, whose `overflow` is 0 and
#     whose own `not_clean` is empty.
#   * for the machine gate, whose required services come from the
#     launcher's record: `roster_generations` names EXACTLY ONE launch
#     roster generation, and it is stated. The join already refuses a
#     window whose hours span a change of selection (its `not_clean`
#     names both generations and the instant — design 3cc6152a, backlog
#     14fe115c, `boss_core::gate_window::judged_generations`) and one in
#     which a required service's own running process stated anything
#     else, whatever another service stated after it (review 6858ef1d,
#     B1: that generation, or the absence of one, is then a second entry
#     of `roster_generations`); this line
#     is the same rule read off the same answer, so an answer from a
#     build that predates it is refused rather than read as "one". The
#     policy check consults no launch record and names none.
# A field that is ABSENT is refused like a dirty one: an answer from a
# build that does not carry it cannot be read as zero.
#
# WHAT IT DOES NOT SAY. It judges the instance whose answer it is
# handed. boss.yaml is rendered once per instance
# (infra/cluster/instances.toml), so a mode word flipped there is
# flipped on every instance, and each has a window of its own.
#
# Exit: 0 clean, one `GATE-WINDOW-CLEAN …` receipt line on stdout;
#       1 not clean, every reason on stderr, one per line;
#       2 unreadable (no document, not an object) or misused.
set -uo pipefail

me="gate-window-is-clean"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/jq.sh
. "$here/../lib/jq.sh" || { echo "$me: UNREADABLE — $here/../lib/jq.sh could not be sourced" >&2; exit 2; }

usage() {
    echo "$me: usage: $me <machine-gate|policy-check> [hours, default 72] < the answer of GET /api/events/gate-window?gate=<gate>&hours=<hours> — $*" >&2
    exit 2
}

[ $# -ge 1 ] && [ $# -le 2 ] || usage "got $# argument(s)"
gate="$1"
hours="${2:-72}"
case "$gate" in
    machine-gate|policy-check) ;;
    *) usage "'$gate' is not a gate" ;;
esac
case "$hours" in
    ''|*[!0-9]*|0*) usage "hours must be a whole number of at least 1, got '$hours'" ;;
esac

body="$(cat)"
jq_doc_text "$body" || {
    echo "$me: UNREADABLE — no JSON document on stdin. The read upstream of this pipe failed or answered nothing; no evidence is not a pass." >&2
    exit 2
}

# Every reason the window is not clean, one per line; empty when it is.
# Each test asks for the value it wants and refuses anything else, so an
# absent key, a null and a wrong type all land in the refusal.
reasons="$(printf '%s' "$body" | jq -r --arg gate "$gate" --argjson hours "$hours" '
    def secs: (sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601)? // null;
    def listed($name): if type == "array" then empty else "\($name) is missing or not a list" end;
    def texts: map(if type == "string" then . else tojson end) | join("; ");
    if type != "object" then "UNREADABLE" else
    . as $w
    | [
        (if $w.gate == $gate then empty
         else "the answer is about gate \($w.gate | tojson), not \($gate)" end),

        ((($w.from // "") | secs) as $from | (($w.now // "") | secs) as $now
         | if $from == null or $now == null then "from/now are missing or not instants: the window length cannot be read"
           elif ($now - $from) < ($hours * 3600) then
             "the window read is \((($now - $from) / 3600) | floor) hour(s), shorter than the \($hours) required"
           else empty end),

        (if $w.covers_requested_window == true then empty
         else "covers_requested_window is \($w.covers_requested_window | tojson), not true" end),

        (if ($w.clean_since | type) == "string" then empty
         else "clean_since is \($w.clean_since | tojson): no clean instant is stated" end),

        (if $w.log_error == null then empty else "the log half was not read: log_error \($w.log_error | tojson)" end),

        (if ($w.log | type) != "object" then "the log half is missing"
         else
           ($w.log.dirty | listed("log.dirty")),
           (if ($w.log.dirty | type) == "array" and ($w.log.dirty | length) > 0 then
              "log.dirty holds \($w.log.dirty | length) fact(s): \($w.log.dirty | map(.kind? // "?") | unique | join(", ")), newest at \($w.log.dirty | map(.at? // "") | max)"
            else empty end),
           ($w.log.not_clean | listed("log.not_clean")),
           (if ($w.log.not_clean | type) == "array" and ($w.log.not_clean | length) > 0 then
              "log.not_clean: \($w.log.not_clean | texts)" else empty end),
           ($w.log.coverage | listed("log.coverage"))
         end),

        ($w.not_clean | listed("not_clean")),
        (if ($w.not_clean | type) == "array" and ($w.not_clean | length) > 0 then
           "not_clean: \($w.not_clean | texts)" else empty end),

        ($w.input_errors | listed("input_errors")),
        (if ($w.input_errors | type) == "array" and ($w.input_errors | length) > 0 then
           "input_errors: \($w.input_errors | texts)" else empty end),

        ($w.live | listed("live")),

        (if $gate != "machine-gate" then empty
         elif ($w.roster_generations | type) != "array" then
           "roster_generations is missing or not a list: the answer does not say which launch roster its hours were judged under"
         elif ($w.roster_generations | length) != 1 then
           "roster_generations names \($w.roster_generations | length) launch roster generation(s), not exactly one: \($w.roster_generations | map("\(.generation? // "none stated") since \(.since? // "?")") | join("; "))"
         elif ($w.roster_generations[0].generation | type) != "string" then
           "roster_generations: the process starts state no generation (\($w.roster_generations[0].unstated? // "no reason given"))"
         else empty end),

        (if ($w.required_services | type) != "array" or ($w.required_services | length) == 0 then
           "required_services names no service: a window over nothing is not a window"
         else
           $w.required_services[] as $s
           | ([($w.log.coverage? // [])[]? | select(.service? == $s)] | first) as $c
           | ([($w.live | if type == "array" then . else [] end)[] | select(.service? == $s)] | first) as $l
           | (
               (if $c == null then "\($s): no log coverage row"
                elif $c.gap != null then "\($s): log coverage gap: \($c.gap | tojson)"
                else empty end),
               (if $l == null then "\($s): no live row — its tally was not read"
                else
                  (if $l.error != null then "\($s): the live read failed: \($l.error | tojson)" else empty end),
                  (if ($l.not_clean | type) == "array" and ($l.not_clean | length) == 0 then empty
                   else "\($s): live not_clean: \($l.not_clean | tojson)" end),
                  (if ($l.snapshot | type) != "object" then "\($s): no live snapshot"
                   else $l.snapshot as $t
                     | (if $t.mode == "report" or $t.mode == "enforce" then empty
                        else "\($s): mode is \($t.mode | tojson), which records nothing" end),
                       (if $t.mode_error? == null then empty
                        else "\($s): mode_error \($t.mode_error | tojson)" end),
                       (if ($t.rows | type) == "array" and ($t.rows | length) == 0 then empty
                        else "\($s): the live tally holds \(if ($t.rows | type) == "array" then ($t.rows | length) else "unreadable" end) row(s) (rows)" end),
                       (if $t.overflow == 0 then empty
                        else "\($s): overflow is \($t.overflow | tojson), not 0 — past the key cap refusals go uncounted" end),
                       (if ($t.not_clean | type) == "array" and ($t.not_clean | length) == 0 then empty
                        else "\($s): the tally reads not_clean: \($t.not_clean | tojson)" end)
                   end)
                end)
             )
         end)
      ]
    | .[]
    end
' 2>&1)" || {
    echo "$me: UNREADABLE — jq could not judge the answer: $reasons" >&2
    exit 2
}

if [ "$reasons" = "UNREADABLE" ]; then
    echo "$me: UNREADABLE — the answer is not one JSON object" >&2
    exit 2
fi

if [ -n "$reasons" ]; then
    echo "$me: NOT CLEAN — gate=$gate hours=$hours; the flip is refused on each line below (design 21946380, decision 4):" >&2
    printf '%s\n' "$reasons" | sed 's/^/  - /' >&2
    exit 1
fi

# The receipt line: every value copied out of the answer judged.
printf '%s' "$body" | jq -r --arg gate "$gate" --arg hours "$hours" '
    "GATE-WINDOW-CLEAN gate=\($gate) hours=\($hours) from=\(.from) now=\(.now) clean_since=\(.clean_since) services=\(.required_services | join(",")) modes=\([.live[] | "\(.service):\(.snapshot.mode)"] | join(","))"
    + (if $gate == "machine-gate" then " roster_generation=\(.roster_generations[0].generation)" else "" end)'
