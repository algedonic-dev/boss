#!/usr/bin/env bash
# factory-loop.sh — the factory's builder CLI, supervised (backlog
# 628dc540, David's request 2026-10-28).
#
# THE CASE. On 2026-10-28 agent-codex, the factory, stopped on a
# temporary model-capacity limit at about 06:42Z and nothing restarted it
# until David did at about 19:20Z — the factory down about 12.5 h — and
# again from about 20:57Z to 22:38Z. It ran as the interactive TUI in the
# `codex` tmux session (dev-session.sh option 2), and on a capacity error
# that TUI waits at an error prompt and never exits, so even a
# restart-on-exit loop around it would never fire.
#
# WHAT THIS DOES INSTEAD. It runs the CLI's NON-INTERACTIVE mode one turn
# at a time, forever:
#   - each turn is `codex exec`, driven by the factory prompt
#     (infra/dev/factory-drain.md), and after the first turn it RESUMES
#     the same codex session (`codex exec resume <id> -`), so the factory
#     keeps its context across turns and across restarts. The id is read
#     off the turn's HEADER only — the line `session id: <uuid>` in its
#     first lines, as codex-cli 0.159.3 prints it (measured offline
#     2026-10-29) — never a uuid the turn's own work printed later (a
#     work-session id read as the codex session; review 97bb7698
#     finding 5). It may be pinned with FACTORY_CODEX_SESSION. A resume
#     id is dropped after two resumed turns in a row fail ON THE ID
#     ITSELF: measured, a resume of an id codex has no rollout for
#     prints one `Error: thread/resume: ... no rollout found ...` line
#     and exits 1 at once, every time, so a bad id kept would cost the
#     30-minute rung forever. A limit or any other failure never counts
#     toward the drop (review 1c7ab966 N3): in a capacity outage, the
#     case this file exists for, the old any-failure count dropped the
#     session on the second limited turn, about 7 minutes in, and the
#     factory came back cold. No id means a fresh turn, which is
#     correct, only colder;
#   - every turn runs with approval_policy "never" (finding 8). Measured
#     offline 2026-10-29 with a dead local provider and a scratch
#     CODEX_HOME holding infra/dev/cli/codex-config.toml: under that
#     config's "on-request", `codex exec` 0.159.3 printed
#     `approval: never` in its header — exec answers no approval prompt.
#     The loop states it rather than inherit an exec default a later CLI
#     may change, because a turn waiting on an approval nobody is there
#     to give is the stall this file exists to end;
#   - a turn that exits non-zero, or whose CLI says it hit a rate or
#     capacity limit, is a FAILED turn: the loop writes a restart note on
#     the factory's open work-session and waits 2, 5, 10, 20 minutes,
#     then 30 minutes every time after, and never gives up. A clean turn
#     resets the ladder. The limit words are read only on a non-zero exit
#     (the turn's last lines) or on the CLI's OWN error line (`ERROR: `
#     or `Error: `, as codex prints one): a clean turn whose final
#     message relays a restart note's reason is not a limit (finding 4);
#   - at every turn START and END the loop stamps `last_active_at` on the
#     factory's open work-session beside a `factory_turn` key (decided
#     2026-10-29 on 628dc540): work-session-ends-when-silent ends a
#     session six hours after its last `last_active_at`, and codex has no
#     prompt hook to write one, so without this a healthy factory's
#     session closed about 6 h after it opened. The `factory_turn` key
#     marks the write as the supervisor's, which the silent-session rule
#     ignores, so a loop relaunching a limited CLI never reads as the
#     factory speaking;
#   - every turn's output is tee'd into this pane and appended to the log
#     (FACTORY_LOG), so David can still watch it live and read it after.
#     The log is rotated whenever it passes 10 MB — at a turn's start and
#     AS a turn writes it (review 1c7ab966 N5: one 4 h turn could grow it
#     past the bound) — keeping five files, the log and .1 to .4
#     (finding 6: it grew on the /work PVC without bound).
#
# THE RESTART NOTE IS VISIBILITY, NOT A DEPENDENCY. It goes through the
# pod's boss-api door onto the newest open work-session whose `actor` is
# FACTORY_SESSION_ACTOR (agent-codex), under a NEW key per restart,
# `factory_restart_<YYYYMMDDTHHMMSSZ>`: the reason, the exit code, the
# wait and the log. When the API is dark the note is skipped with a line
# in the log and the loop goes on — a supervisor that needs the system it
# reports to is not a supervisor (CLAUDE.md, "an arm that needs the
# patient is not an arm"). The `factory_restart_` prefix is the one the
# rule a-silent-factory-holding-claimed-work-is-alarmed ignores, so these
# notes never read as the factory speaking; the handler's test holds the
# two equal.
#
# THE INTERACTIVE PATH STAYS. dev-session.sh keeps option 2 (`codex`, the
# TUI) and adds this as option 6 (`codex-factory`); the two refuse to
# start beside each other, because two CLIs signing as one agent would
# race each other for the same queue.
#
# The seams, unset on the pod: FACTORY_CLI (the CLI to run), FACTORY_SLEEP
# (what waits), FACTORY_MAX_TURNS (stop after n turns; 0 is forever),
# FACTORY_TURN_PAUSE (seconds between clean turns), FACTORY_TURN_TIMEOUT
# (seconds before a turn that never returns is killed and counted as
# failed), FACTORY_PROMPT_FILE, FACTORY_LOG, FACTORY_LOG_MAX_BYTES and
# FACTORY_LOG_KEEP.

set -u

here=${0%/*}
# This file's own age (backlog 0b36dd65): it runs from the main checkout.
# Warn and proceed — a factory stopped by a door is the defect this file
# exists to end.
if [ -r "$here/door-freshness.sh" ]; then
  # shellcheck source=door-freshness.sh
  . "$here/door-freshness.sh"
  if stale=$(door_is_stale "$0"); then
    echo "factory-loop: WARNING - $stale" >&2
  fi
fi

cli=${FACTORY_CLI:-codex}
prompt_file=${FACTORY_PROMPT_FILE:-$here/factory-drain.md}
log=${FACTORY_LOG:-$HOME/.local/state/boss/factory-loop.log}
actor=${FACTORY_SESSION_ACTOR:-agent-codex}
pause=${FACTORY_TURN_PAUSE:-60}
turn_timeout=${FACTORY_TURN_TIMEOUT:-14400}
sleep_cmd=${FACTORY_SLEEP:-sleep}
max_turns=${FACTORY_MAX_TURNS:-0}
session=${FACTORY_CODEX_SESSION:-}
log_max=${FACTORY_LOG_MAX_BYTES:-10485760}
log_keep=${FACTORY_LOG_KEEP:-5}

# The approval every turn runs under (see the header): exec has nobody
# to ask.
policy='approval_policy="never"'

# The waits after a failed turn, in seconds: 2, 5, 10, 20, then 30
# minutes for every failure after (628dc540's decision).
backoff=(120 300 600 1200 1800)

# A limit, as codex-cli 0.159.3 words one: read off the installed binary
# ("rate limit exceeded", "flex capacity unavailable", "server
# overloaded", "Quota exceeded", "usage limit", "too many requests",
# "Codex is currently experiencing high load."), matched case-blind on
# a failed turn's LAST lines or on the CLI's own error line — never on a
# clean turn's work, which may mention capacity or relay a restart note.
limit_re='rate[ _-]?limit|usage[ _-]?limit|capacity[ _-]?(limit|unavailable)|at capacity|model capacity|overloaded|too many requests|quota exceeded|currently experiencing high load'

# A resume that failed on the ID, read only off the CLI's own error line:
# "no rollout found for thread id" is codex-cli 0.159.3's measured words
# for an unknown id (2026-10-29, offline); the rest name an id the CLI
# no longer has (not found, unknown, expired).
resume_re='no rollout found|thread/resume failed|(thread|session|conversation)( id)? (not found|is unknown|has expired|expired)|unknown (thread|session|conversation)'

mkdir -p "${log%/*}"
turn_out=$log.turn
note_body=$log.note.json
beat_body=$log.beat.json

say() { printf 'factory-loop %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$log"; }

# Five files of 10 MB: the log, then .1 (newest) to .4 (oldest).
rotate_log() {
  local size i
  size=$(stat -c %s "$log" 2>/dev/null || echo 0)
  [ "$size" -ge "$log_max" ] || return 0
  i=$((log_keep - 1))
  rm -f "$log.$i"
  while [ "$i" -gt 1 ]; do
    [ -e "$log.$((i - 1))" ] && mv -f "$log.$((i - 1))" "$log.$i"
    i=$((i - 1))
  done
  mv -f "$log" "$log.1"
}

# Echo a turn's output to this pane and append it to the log, rotating
# whenever the log passes its bound mid-turn. The size is counted as
# lines are written (one stat at the start and after each rotation, never
# a fork per line). Each line is opened for append on its own, so a
# rotation's mv never leaves a writer on the old file.
log_turn() {
  local line size
  size=$(stat -c %s "$log" 2>/dev/null || echo 0)
  while IFS= read -r line || [ -n "$line" ]; do
    printf '%s\n' "$line"
    printf '%s\n' "$line" >>"$log"
    size=$((size + ${#line} + 1))
    if [ "$size" -ge "$log_max" ]; then
      rotate_log
      size=$(stat -c %s "$log" 2>/dev/null || echo 0)
    fi
  done
}

if [ ! -s "$prompt_file" ]; then
  say "REFUSED: the factory prompt $prompt_file is missing or empty - no turn can be driven"
  exit 66
fi

# The factory's open work-session, newest first.
open_session() {
  local q body
  q=$(jq -rn --arg a "$actor" '{actor: $a} | tojson | @uri')
  body=$(boss-api GET "/api/jobs?kind=work-session&status=open&order=newest&full=false&limit=5&metadata=$q" 2>/dev/null) || return 1
  jq -r '.data[0].id // empty' <<<"$body"
}

# The restart note: reason, exit code, wait, log. Best-effort.
note_restart() {
  local reason=$1 code=$2 wait=$3 sid stamp note_key
  stamp=$(date -u +%Y%m%dT%H%M%SZ)
  note_key=factory_restart_$stamp
  if ! sid=$(open_session) || [ -z "$sid" ]; then
    say "restart note NOT written: no open work-session for $actor could be read"
    return 0
  fi
  jq -n --arg k "$note_key" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg by "$actor" \
    --arg reason "$reason" --argjson code "$code" --argjson wait "$wait" --arg log "$log" \
    --arg cli "$cli" \
    '{($k): {at: $at, by: ($by + " factory-loop"), reason: $reason, exit_code: $code,
      backoff_seconds: $wait, cli: $cli, log: $log}}' >"$note_body"
  if boss-api PATCH "/api/jobs/$sid/metadata" "$note_body" >/dev/null 2>&1; then
    say "restart note $note_key written on work-session $sid"
  else
    say "restart note NOT written: PATCH /api/jobs/$sid/metadata refused or unreachable"
  fi
}

# The turn heartbeat: `last_active_at`, which the six-hour age-out reads,
# beside `factory_turn`, which marks it as the supervisor's. Best-effort,
# like the restart note.
heartbeat() {
  local phase=$1 sid now
  if ! sid=$(open_session) || [ -z "$sid" ]; then
    say "heartbeat ($phase) NOT written: no open work-session for $actor could be read"
    return 0
  fi
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -n --arg at "$now" --arg phase "$phase" --argjson turn "$turn" --arg by "$actor" \
    '{last_active_at: $at,
      factory_turn: {turn: $turn, phase: $phase, at: $at, by: ($by + " factory-loop")}}' \
    >"$beat_body"
  if ! boss-api PATCH "/api/jobs/$sid/metadata" "$beat_body" >/dev/null 2>&1; then
    say "heartbeat ($phase) NOT written: PATCH /api/jobs/$sid/metadata refused or unreachable"
  fi
}

turn=0
failures=0
resume_failures=0
while :; do
  turn=$((turn + 1))
  rotate_log
  resumed=$session
  if [ -n "$session" ]; then
    cmd=("$cli" exec resume -c "$policy" "$session" -)
  else
    cmd=("$cli" exec -c "$policy" -)
  fi
  heartbeat start
  say "turn $turn: ${cmd[*]} < $prompt_file"
  timeout --kill-after=60 "$turn_timeout" "${cmd[@]}" <"$prompt_file" 2>&1 | tee "$turn_out" | log_turn
  code=${PIPESTATUS[0]}
  heartbeat end

  # The header's own line, in the turn's first lines, first match only.
  seen=$(sed -n '1,40{/^session id: [0-9a-fA-F-]\{36\}$/{s/^session id: //p;q}}' "$turn_out")
  if [ -n "$seen" ] && [ "$seen" != "$session" ]; then
    session=$seen
    resume_failures=0
  fi

  cli_errors=$(grep -E '^(ERROR|Error): ' "$turn_out")
  last=$(tail -n 20 "$turn_out")
  reason=''
  if grep -Eiq "$limit_re" <<<"$cli_errors"; then
    reason='model rate or capacity limit'
  elif [ "$code" -ne 0 ] && grep -Eiq "$limit_re" <<<"$last"; then
    reason='model rate or capacity limit'
  elif [ "$code" -ne 0 ]; then
    reason="exit $code"
  fi

  if [ -z "$reason" ]; then
    failures=0
    resume_failures=0
    say "turn $turn ended cleanly; next turn in ${pause}s"
    wait_s=$pause
  else
    # Only a failure ON THE ID counts toward dropping it (N3): the CLI's
    # own error line names an unknown or expired id, and the turn was not
    # a limit. A limit or any other failure leaves the count alone.
    if [ -n "$resumed" ] && [ "$resumed" = "$session" ] &&
      [ "$reason" != 'model rate or capacity limit' ] &&
      grep -Eiq "$resume_re" <<<"$cli_errors"; then
      resume_failures=$((resume_failures + 1))
      if [ "$resume_failures" -ge 2 ]; then
        say "dropping resume id $session: $resume_failures resumed turns failed on the id itself (unknown or expired); the next turn starts fresh"
        session=''
        resume_failures=0
      fi
    fi
    i=$failures
    [ "$i" -ge "${#backoff[@]}" ] && i=$((${#backoff[@]} - 1))
    wait_s=${backoff[$i]}
    failures=$((failures + 1))
    say "turn $turn FAILED ($reason, exit $code); relaunching in ${wait_s}s (failure $failures in a row)"
    note_restart "$reason" "$code" "$wait_s"
  fi

  if [ "$max_turns" -gt 0 ] && [ "$turn" -ge "$max_turns" ]; then
    say "stopping after $turn turn(s) (FACTORY_MAX_TURNS)"
    exit 0
  fi
  "$sleep_cmd" "$wait_s"
done
