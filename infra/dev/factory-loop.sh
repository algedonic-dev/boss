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
# EVERY SEAT, NOT ONLY CODEX (backlog b513514c, 2026-11-17). On
# 2026-11-17 the factory stood idle 07:10Z-14:45Z (item 5627a4e1): this
# loop supervised Codex alone, while the conductor's interactive session
# ended its last background watch and Claude B sat at its prompt, and
# nothing woke either. So the loop now takes a SEAT, `factory-loop.sh
# <seat>`, a row of infra/dev/seats.tsv naming its CLI, the actor its
# work-session carries, its prompt and its pause. With no seat it is the
# file's first row, codex-factory, read off the row like any other, so
# option 6's command and agent-pane.sh's proof of it are unchanged.
# A `claude` seat runs Claude Code's non-interactive mode the same way:
# `claude -p --permission-mode auto`, the first turn under a session id
# the loop chooses (`--session-id`), every later turn `--resume` of it.
# Measured on Claude Code 2.1.326, 2026-11-17, against a scratch config
# home: an unknown resume id prints `No conversation found with session
# ID: <id>` and exits 1, and a home with no login prints `Not logged in ·
# Please run /login` and exits 1; both read as the CLI's own error line
# here, and only the first counts toward dropping the id.
#
# A CLAUDE SEAT OWNS ITS WORK-SESSION (review ce8970ce B1, 2026-11-17).
# Claude Code -p fires SessionStart AND SessionEnd (reason "other") on
# EVERY turn — measured on 2.1.326 by that review, with hooks that only
# copied their payloads — and the project's session-end hook completed
# the work-session's terminal `active` step at each one, so every turn
# filed and closed its own packet and no heartbeat here ever found one
# open. So the loop marks its turns BOSS_SEAT=<seat>, which
# session-end.sh honours by leaving the packet open, and runs the
# packet's life through the same two hooks, by hand: before a turn it
# FILES the conversation's packet when it has none open
# (session-start.sh, `source` seat), and it ENDS it (session-end.sh,
# unmarked) when the conversation ends — a dropped resume id, the turn
# bound, a signal. The packet is the one the hooks remember under the
# conversation id the loop chose, in the hooks' state directory, which
# the loop exports as BOSS_HOOK_STATE so it and every hook its turns
# fire read one place. Never another session of the same actor: tmux
# `dev`, the Remote Control door, signs as the conductor too.
#
# A PULL SEAT TAKES ITS WORK FROM ITS STATION (backlog 35f828cf, David's
# morning plan item 2, 2026-11-17). Every turn above runs a standing
# prompt that tells the executor to look for work, so a seat whose
# executor waits for a nomination or a tmux nudge does nothing until it
# gets one, and when the coordinator sleeps the factory sleeps. A seat
# whose row says `next` in its `pull` column has the loop do the looking,
# as machinery: before a turn it judges its ONE slot (a seat runs one
# CLI process, so it holds one step at a time) off the record, and
#   - a slot is HELD while any open agent-run of the seat's own agent has
#     its `building` step pending, ready or active — the agent read off
#     the agents registry, every name it goes by, and a run counted as
#     its own when its building is placed on one of them or the run is
#     signed by one (`metadata.agent`). The turn then runs the seat's own
#     prompt, which tells the executor to finish what it holds before it
#     claims anything;
#   - a FREE slot (no such run) pulls with `boss dispatch --next`, the
#     native station pull, which takes the first admissible step at the
#     seats the actor's agents row declares, in their order, and claims
#     it and opens its run as the seat's actor. Its stdout is the brief
#     and nothing else, so the turn is the seat's prompt with that brief
#     appended; the run it opened is read back off the record, and holds
#     the slot until its building ends;
#   - an empty answer is no admissible work: no turn, no model call, the
#     seat's pause, then another pull;
#   - a non-zero pull is the door's refusal, kept whole in the log and
#     written as a restart note, on the same ladder as a failed turn.
#     The next pull is the same door again (the native pull journal
#     replays or ends a request whose answer was lost); the loop never
#     falls back to the standing prompt to find work some other way;
#   - a slot the record cannot judge (the registry or the run listing
#     cannot be read, is refused, or is cut short of its total; or no
#     agent row names the actor) pulls nothing and runs nothing until it
#     can: a second claim on a guess is the double claim this column must
#     never make. Each such pass is a restart note on the failure ladder,
#     so a seat that cannot judge looks troubled, not idle.
# AN EMPTY LISTING IS NOT A FREE SLOT (review f8cbaa0e B3, 2026-11-20).
# The listing door answers a denied read scope with 200 and nothing in
# it (boss-jobs http/jobs.rs list_jobs, "Denied -> return an empty
# collection"), and so does a wrong or dark instance; judged free, that
# blind read pulled on every pass. So, before a pull:
#   - the loop names the identity the pull signs as exactly as `boss`
#     does (identity.rs: BOSS_ACTOR, else $BOSS_ACTOR_FILE or
#     $HOME/.config/boss/actor, trimmed, blank naming nobody), hands it
#     to the pull, and pulls nothing when nothing names one or when it
#     is not a name of the seat's own agent row (the run would be
#     another agent's — a loop started by hand signs as the actor file);
#   - an empty listing is trusted only beside a control on the same
#     door: the newest run of ANY status that identity signed must be
#     listed. An agent the record shows no run of cannot tell an empty
#     answer from a blind one, so its first run is a hand's dispatch;
#   - a pull whose run is not read back HOLDS the seat: nothing more is
#     pulled, on the failure ladder, until the record lists a run of the
#     seat's agent building, or a run that identity signed newer than the
#     one the pull was judged against. That belief is kept beside the
#     log ($log.unseen) so a restart keeps it too; it can only refuse a
#     pull, never permit one, and a file there the loop cannot read or
#     remove, or one that holds anything but a run id, holds the seat
#     until a run of its agent is seen building (review 2d18db68 N11).
# THE HOLD ASSUMES EVERY RUN IS EQUALLY VISIBLE (review 2d18db68 N12).
# Either way the unseen hold releases — a run of the agent seen
# building, a newer run its signer signed — can be met by a run other
# than the one it waits for if the two reads see different runs: another
# process signing under the same login, a listing blind where the
# control sees, a partial read scope. Not reachable on 2026-11-21: one
# primary answers both reads and every agent-run is owned by the same
# person, so whoever can read one run reads them all. A seat whose actor
# reads through a narrower role than that must not be turned to `next`
# until the hold names the run it waits for, not a run like it.
# THE SLOT IS THE RECORD, NOT A FILE (review 0d65ef4c B1, 2026-11-20).
# The first build kept the pulled run's id in a file beside the log and
# parsed it out of the brief's prose; a failed write, a brief that
# worded its run line otherwise, or a restart under another log or after
# a kill each lost it, and the next pass pulled a second step while the
# first run still built. Nothing the loop keeps is a guard now, so a
# restarted loop, or a second loop on the seat that starts after the
# first pulled, reads the same answer. The loop's own writes stay the
# supervisor's: the heartbeat's `factory_turn` names the pass's `pull`
# outcome and run; the executor's writes are its own.
# Every row is `-` until a reviewed car turns one on: this changes who
# claims work unattended (trust boundary, mutating-verb).
#
# The seams, unset on the pod: FACTORY_CLI (the CLI to run), FACTORY_SLEEP
# (what waits), FACTORY_MAX_TURNS (stop after n passes, each a turn but
# a pull seat's pass that runs none; 0 is forever),
# FACTORY_TURN_PAUSE (seconds between clean turns), FACTORY_TURN_TIMEOUT
# (seconds before a turn that never returns is killed and counted as
# failed), FACTORY_PROMPT_FILE, FACTORY_LOG, FACTORY_LOG_MAX_BYTES and
# FACTORY_LOG_KEEP, and FACTORY_SEATS (the seat list).

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

# The seat: its row of seats.tsv, or with none, the list's FIRST row,
# every column of it (review 0d65ef4c B2, 2026-11-20). The loop used to
# answer a bare start from defaults of its own, held equal to row 1 on
# three of seven columns, so a first row saying `next` would not have
# pulled; one row cannot drift from itself. `seat` stays empty for a
# bare start, so its log keeps the name option 6 has always written.
seat=${1:-}
seats=${FACTORY_SEATS:-$here/seats.tsv}
row=$(awk -v s="$seat" '$1 !~ /^#/ && NF > 0 && (s == "" || $1 == s) { print $1, $2, $3, $4, $5, $7; exit }' "$seats" 2>/dev/null)
if [ -z "$row" ]; then
  echo "factory-loop: REFUSED: no seat '${seat:-(the first row)}' in $seats - nothing is run" >&2
  exit 64
fi
read -r seat_name kind seat_actor seat_prompt seat_pause seat_pull <<<"$row"
seat_prompt=$here/../../$seat_prompt
# The pull column (35f828cf): `-` runs the standing prompt every turn,
# `next` pulls through the station door (see the header). Anything else
# is refused rather than guessed at.
case ${seat_pull:--} in
  - | next) seat_pull=${seat_pull:--} ;;
  *)
    echo "factory-loop: REFUSED: seat '$seat_name' names pull '$seat_pull'; a seat pulls with 'next' or not at all ('-')" >&2
    exit 64
    ;;
esac
case $kind in
  codex | claude) ;;
  *)
    echo "factory-loop: REFUSED: seat '$seat_name' names CLI '$kind'; a turn is defined only for codex and claude" >&2
    exit 64
    ;;
esac

cli=${FACTORY_CLI:-$kind}
prompt_file=${FACTORY_PROMPT_FILE:-$seat_prompt}
log=${FACTORY_LOG:-$HOME/.local/state/boss/factory-loop${seat:+-$seat}.log}
actor=${FACTORY_SESSION_ACTOR:-$seat_actor}
pause=${FACTORY_TURN_PAUSE:-$seat_pause}
turn_timeout=${FACTORY_TURN_TIMEOUT:-14400}
sleep_cmd=${FACTORY_SLEEP:-sleep}
max_turns=${FACTORY_MAX_TURNS:-0}
session=${FACTORY_CODEX_SESSION:-}
log_max=${FACTORY_LOG_MAX_BYTES:-10485760}
log_keep=${FACTORY_LOG_KEEP:-5}

# A claude seat's turns are marked as the seat's, and the hooks' state
# root is the loop's (see the header): lib.sh reads BOSS_HOOK_STATE
# first, so exporting the value it would default to makes the hooks
# and this loop read one directory by construction.
hooks=$here/hooks
if [ "$kind" = claude ]; then
  export BOSS_SEAT=$seat_name
  export BOSS_HOOK_STATE=${BOSS_HOOK_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/boss/sessions}
fi

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
resume_re='no rollout found|thread/resume failed|(thread|session|conversation)( id)? (not found|is unknown|has expired|expired)|unknown (thread|session|conversation)|no conversation found'

# The CLI's OWN error line, which a clean turn's work never starts with:
# codex prints `ERROR: ` or `Error: `; Claude Code -p prints `API Error: `
# for a refused model call and, measured on 2.1.326, the bare `No
# conversation found with session ID:` and `Not logged in` lines. A
# Claude limit is `Claude AI usage limit reached` or an API error
# naming rate_limit or overloaded, which limit_re reads with the words
# below.
case $kind in
  codex) err_re='^(ERROR|Error): ' ;;
  claude)
    err_re='^(API Error|Error|ERROR): |^No conversation found|^Not logged in'
    limit_re="$limit_re|limit reached"
    ;;
esac

mkdir -p "${log%/*}"
turn_out=$log.turn
note_body=$log.note.json
beat_body=$log.beat.json
pulled=$log.pulled
pull_err=$log.pull.err
turn_prompt=$log.prompt
unseen_file=$log.unseen

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

# The seat's open work-session. Codex's: the newest open one its actor
# carries. A claude seat's: the one the hooks remember for the
# conversation the loop chose, which own_session has checked open
# before the turn — never another session of the same actor (the `dev`
# door). Before that conversation has a packet there is none.
open_session() {
  local q body
  if [ "$kind" = claude ]; then
    [ -n "$session" ] || return 1
    tr -d '[:space:]' <"$BOSS_HOOK_STATE/$session/packet" 2>/dev/null
    return 0
  fi
  q=$(jq -rn --arg a "$actor" '{actor: $a} | tojson | @uri')
  body=$(boss-api GET "/api/jobs?kind=work-session&status=open&order=newest&full=false&limit=5&metadata=$q" 2>/dev/null) || return 1
  jq -r '.data[0].id // empty' <<<"$body"
}

# A claude seat's packet, OPEN before its turn (B1; see the header). The
# packet the hooks remember is judged off its own `active` step; one
# that has ended (the six-hour silent-session clock after a dark API,
# or a hand) is forgotten, and a conversation with none is given one
# through session-start.sh exactly as Claude Code would fire it. An
# answer that cannot be read judges nothing: the packet is kept, and a
# conversation the hook could not file for is filed on a later turn —
# Claude Code's own SessionStart files it meanwhile when it can.
own_session() {
  local dir=$BOSS_HOOK_STATE/$session packet job state
  packet=$(tr -d '[:space:]' <"$dir/packet" 2>/dev/null)
  if [ -n "$packet" ]; then
    job=$(boss-api GET "/api/jobs/$packet" 2>/dev/null) || return 0
    state=$(jq -r 'if (.steps | type) == "array" and (.steps | length) > 0
      then (if any(.steps[]; .spec_slug == "active" and (.status == "ready" or .status == "active"))
        then "open" else "ended" end)
      else "unknown" end' <<<"$job" 2>/dev/null)
    [ "$state" = ended ] || return 0
    say "work-session $packet of seat $seat_name has ended - the seat files a new one for conversation $session"
    rm -f "$dir/packet"
  fi
  jq -nc --arg s "$session" --arg cwd "$PWD" \
    '{session_id: $s, source: "seat", cwd: $cwd, hook_event_name: "SessionStart"}' |
    "$hooks/session-start.sh" >>"$log" 2>&1
  packet=$(tr -d '[:space:]' <"$dir/packet" 2>/dev/null)
  if [ -n "$packet" ]; then
    say "work-session $packet is seat $seat_name's, for conversation $session"
  else
    say "work-session NOT filed for seat $seat_name: the session-start hook's line above says why"
  fi
}

# The end of a claude seat's conversation ends its packet: session-end.sh
# with the seat's mark taken off, the one end that hook does not keep
# open. Best-effort, like every write here; a packet this cannot end is
# ended by the six-hour clock.
end_session() {
  local why=$1 packet
  [ "$kind" = claude ] && [ -n "$session" ] || return 0
  packet=$(tr -d '[:space:]' <"$BOSS_HOOK_STATE/$session/packet" 2>/dev/null)
  [ -n "$packet" ] || return 0
  jq -nc --arg s "$session" --arg r "$why" --arg cwd "$PWD" \
    '{session_id: $s, reason: $r, cwd: $cwd, hook_event_name: "SessionEnd"}' |
    env -u BOSS_SEAT "$hooks/session-end.sh" >>"$log" 2>&1
  say "work-session $packet of seat $seat_name: ended ($why)"
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
    --arg cli "$cli" --arg seat "$seat_name" \
    '{($k): {at: $at, by: ($by + " factory-loop"), seat: $seat, reason: $reason, exit_code: $code,
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
  # A pull seat's beat names the pass's pull outcome and run (35f828cf):
  # the supervisor's account of what it took, beside the executor's own.
  # It rides INSIDE `factory_turn`, so the write still changes only
  # `last_active_at` and that key, which a-silent-factory-holding-
  # claimed-work-is-alarmed ignores as the supervisor's; its handler's
  # pin reads `factory_turn: {` here, so the object stays spelled so and
  # the pull is added after it.
  jq -n --arg at "$now" --arg phase "$phase" --argjson turn "$turn" --arg by "$actor" \
    --arg seat "$seat_name" --arg pull "$pull_outcome" --arg run "$pull_run" \
    '{last_active_at: $at,
      factory_turn: {turn: $turn, phase: $phase, seat: $seat, at: $at, by: ($by + " factory-loop")}}
      | if $pull == "" then . else
          .factory_turn.pull = ({outcome: $pull} + (if $run == "" then {} else {run: $run} end)) end' \
    >"$beat_body"
  if ! boss-api PATCH "/api/jobs/$sid/metadata" "$beat_body" >/dev/null 2>&1; then
    say "heartbeat ($phase) NOT written: PATCH /api/jobs/$sid/metadata refused or unreachable"
  fi
}

turn=0
failures=0
resume_failures=0
pull_outcome=''
pull_run=''
signer=''
# A pull whose run was not read back (B3): the newest run of the signer
# the record showed before it, '?' when the kept belief names none the
# loop can compare, or empty when nothing is held against the record.
unseen=''

# The next wait on the failure ladder: sets wait_s and counts the
# failure.
climb() {
  local i=$failures
  [ "$i" -ge "${#backoff[@]}" ] && i=$((${#backoff[@]} - 1))
  wait_s=${backoff[$i]}
  failures=$((failures + 1))
}

# The identity the pull signs as, resolved as `boss` resolves it
# (crates/orchestrators/boss-cli/src/identity.rs, review f8cbaa0e
# scenario M): BOSS_ACTOR, else the actor file — $BOSS_ACTOR_FILE when it
# names one, else $HOME/.config/boss/actor — each trimmed, and a blank
# value names nobody. Sets signer, or slot_why and returns 1.
resolve_signer() {
  local file
  signer=$(jq -rn --arg v "${BOSS_ACTOR:-}" '$v | gsub("^\\s+|\\s+$"; "")' 2>/dev/null)
  if [ -z "$signer" ]; then
    file=$(jq -rn --arg v "${BOSS_ACTOR_FILE:-}" '$v | gsub("^\\s+|\\s+$"; "")' 2>/dev/null)
    file=${file:-$HOME/.config/boss/actor}
    signer=$(jq -Rrs 'gsub("^\\s+|\\s+$"; "")' 2>/dev/null <"$file")
  fi
  [ -n "$signer" ] && return 0
  slot_why="nothing names the identity the pull would sign as (BOSS_ACTOR is blank and ${file:-the actor file} names nobody)"
  return 1
}

# The seat's own open runs, off the record (review 0d65ef4c B1; see the
# header). Sets held_runs to the ids, space-separated, of every open
# agent-run of the seat's agent whose `building` step has not ended, and
# returns 0; or sets slot_why and returns 1 when the record cannot
# answer. The agent is the registry row naming the seat's actor, and
# every name it goes by: `boss dispatch` places `building` on the
# REGISTERED id and signs the run with the login, so either one names
# the run as the agent's. The signer must be one of those names
# (review f8cbaa0e scenario M): a pull signed as anyone else opens a run
# that is another agent's, which this slot would never read back. The
# listing is read whole or not at all: a page cut short of its `.total`
# may hold the very run that holds the slot.
own_runs() {
  local agents names body
  held_runs=''
  slot_why=''
  agents=$(boss-api GET /api/agents 2>/dev/null) || {
    slot_why='the agents registry could not be read'
    return 1
  }
  names=$(jq -c --arg a "$actor" '
    [.data[]? | select([.id] + (.aliases // []) | any(.[]; . == $a))
      | .id, (.aliases // [])[]] | unique' <<<"$agents" 2>/dev/null)
  case $names in
    '' | '[]')
      slot_why="no agent row of the registry names $actor"
      return 1
      ;;
  esac
  # Asked for a word, not an exit code: jq-1.6 exits 0 on an input with
  # no document (backlog d96e38ab), and silence here must refuse.
  if [ "$(jq -r --arg s "$signer" 'if any(.[]; . == $s) then "own" else "other" end' <<<"$names" 2>/dev/null)" != own ]; then
    slot_why="the pull would sign as $signer, which is not a name of the agent row of seat actor $actor ($names): its run would be another agent's"
    return 1
  fi
  body=$(boss-api GET '/api/jobs?kind=agent-run&status=open&limit=500&full=false' 2>/dev/null) || {
    slot_why='the open agent-runs could not be read (unreachable or refused)'
    return 1
  }
  held_runs=$(jq -r --argjson me "$names" '
    if (.data | type) != "array" or (.total | type) != "number" then error("not a listing")
    elif (.data | length) < .total then error("cut short of its total")
    else [.data[] | . as $run
      | select(any(.steps[]?; .spec_slug == "building"
          and (.status == "pending" or .status == "ready" or .status == "active")
          and ((.assignee_id as $h | any($me[]; . == $h))
            or ($run.metadata.agent as $g | any($me[]; . == $g)))))
      | .id] | join(" ") end' <<<"$body" 2>/dev/null) || {
    held_runs=''
    slot_why='the open agent-runs listing was not whole (cut short of its total, or not a listing)'
    return 1
  }
  return 0
}

# The control an empty listing is trusted beside (review f8cbaa0e B3):
# the same door, asked for the newest run of ANY status the signer
# signed. Sets control_run to its id and returns 0 only when the answer
# lists one signed by the signer; otherwise sets slot_why and returns 1,
# because a door that cannot show this identity's runs answers an empty
# listing whether a run holds the slot or not.
seen_runs() {
  local q body
  control_run=''
  q=$(jq -rn --arg a "$signer" '{agent: $a} | tojson | @uri')
  body=$(boss-api GET "/api/jobs?kind=agent-run&order=newest&limit=1&full=false&metadata=$q" 2>/dev/null) || {
    slot_why="the control read of runs signed by $signer could not be read (unreachable or refused)"
    return 1
  }
  control_run=$(jq -r --arg a "$signer" '
    if (.total | type) == "number" and .total > 0 and (.data | type) == "array"
      and (.data[0].metadata.agent? == $a)
    then .data[0].id // empty else empty end' <<<"$body" 2>/dev/null)
  [ -n "$control_run" ] && return 0
  slot_why="the record lists no run of any status signed by $signer, so an empty listing cannot be told from a read that cannot see its runs (a denied scope, or a wrong or dark instance, answers 200 with nothing in it)"
  return 1
}

# The kept belief that a pull's run was not read back (B3; see the
# header). Loaded from beside the log when the process holds none, so a
# restart keeps it; a file there that names no run is '?', which only a
# run of the agent seen building clears. Anything but a run id is '?' too
# (review 2d18db68 N11, backlog a569e6b2): a corrupt file was compared as
# a run id, never matched the control, and so released the hold.
load_unseen() {
  [ -z "$unseen" ] || return 0
  [ -e "$unseen_file" ] || [ -L "$unseen_file" ] || return 0
  unseen=$(tr -d '[:space:]' 2>/dev/null <"$unseen_file")
  [[ $unseen =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] || unseen='?'
}

clear_unseen() {
  [ -n "$unseen" ] || return 0
  unseen=''
  rm -f "$unseen_file" 2>/dev/null
  if [ -e "$unseen_file" ] || [ -L "$unseen_file" ]; then
    say "pass $turn: $1, but $unseen_file cannot be removed: the seat keeps holding until it can"
  else
    say "pass $turn: $1; the seat no longer holds for an unseen pull"
  fi
}

# Every pass that cannot judge its slot: no pull, no turn, the ladder
# and a restart note.
unjudged() {
  pull_outcome=unknown
  turn_from=''
  climb
  say "pass $turn: the slot is NOT judged - $slot_why; nothing is pulled and nothing is run until the record answers (next pass in ${wait_s}s, failure $failures in a row)"
  note_restart "seat slot not judged: $slot_why" null "$wait_s"
}

# A pull seat's pass, before its turn (35f828cf; see the header). Sets
# pull_outcome (held, pulled, empty, refused or unknown) and pull_run,
# and turn_from: the prompt this pass's turn is handed, or empty when
# the pass runs no turn (wait_s then says how long until the next).
pull_pass() {
  local rc why
  load_unseen
  if ! resolve_signer || ! own_runs; then
    unjudged
    return 0
  fi
  if [ -n "$held_runs" ]; then
    clear_unseen "the record lists run(s) $held_runs of the seat's agent building"
    pull_outcome=held
    pull_run=${held_runs%% *}
    say "pass $turn: the slot is held by run(s) $held_runs, whose building step is open: this turn finishes it under the seat's prompt, and nothing is pulled"
    return 0
  fi
  if ! seen_runs; then
    unjudged
    return 0
  fi
  if [ -n "$unseen" ]; then
    if [ "$unseen" != '?' ] && [ "$control_run" != "$unseen" ]; then
      clear_unseen "the record now lists run $control_run signed by $signer, newer than run $unseen the unseen pull was judged against"
    else
      if [ "$unseen" = '?' ]; then
        why="a run of its agent building ($unseen_file names no run to compare)"
      else
        why="a run of its agent building, or a run signed by $signer newer than $unseen"
      fi
      slot_why="a pull opened a run that was not read back; the seat holds until the record lists $why"
      unjudged
      return 0
    fi
  fi
  # The brief and the door's words are kept beside the log; a place the
  # loop cannot write is its own trouble, not the door's refusal (review
  # f8cbaa0e N7), and nothing is claimed before it is known. The turn's
  # prompt is the third place, checked with the other two (review
  # 2d18db68 N10): unchecked, a pull claimed a step whose turn was then
  # handed nothing.
  if ! { : >"$pulled" && : >"$pull_err" && : >"$turn_prompt"; } 2>/dev/null; then
    slot_why="the pulled brief cannot be kept beside the log ($pulled, $pull_err, $turn_prompt)"
    unjudged
    return 0
  fi
  BOSS_ACTOR=$signer boss dispatch --next >"$pulled" 2>"$pull_err"
  rc=$?
  log_turn <"$pull_err"
  if [ "$rc" -ne 0 ]; then
    # The door's own words: its error line, else its last line.
    why=$(sed -n '/^Error: /{s/^Error: //p;q}' "$pull_err")
    [ -n "$why" ] || why=$(sed '/^[[:space:]]*$/d' "$pull_err" | tail -n 1)
    pull_outcome=refused
    turn_from=''
    climb
    say "pass $turn: station pull REFUSED (exit $rc): ${why:-the door said nothing}; the same door again in ${wait_s}s (failure $failures in a row)"
    note_restart "station pull refused: ${why:-the door said nothing}" "$rc" "$wait_s"
    return 0
  fi
  failures=0
  if ! [ -s "$pulled" ]; then
    pull_outcome=empty
    turn_from=''
    wait_s=$pause
    say "pass $turn: no admissible work at the seat's stations; no turn, next pull in ${wait_s}s"
    return 0
  fi
  pull_outcome=pulled
  # The run this pull opened, read back off the record rather than out
  # of the brief's prose (review 0d65ef4c B, N3). Not read back, the
  # seat HOLDS (review f8cbaa0e B3): the turn is still handed the brief,
  # because its step is claimed, but nothing more is pulled until the
  # record shows a run of the agent — a read that missed this run would
  # miss it again next pass.
  if own_runs && [ -n "$held_runs" ]; then
    pull_run=${held_runs%% *}
    say "pass $turn: pulled run $pull_run through the station door; it holds the slot until its building step ends"
  else
    unseen=$control_run
    printf '%s\n' "$control_run" >"$unseen_file" 2>/dev/null ||
      say "pass $turn: the unseen pull could not be kept at $unseen_file: a restart of this loop forgets it"
    say "pass $turn: the run this pull opened was not read back (${slot_why:-the record lists no open run of $actor}); the turn is handed the brief because its step is claimed, and the seat HOLDS - nothing more is pulled until the record lists a run of its agent building, or a run signed by $signer newer than $control_run"
  fi
  # The write is checked too (N10): a place that stopped being writable
  # after the check, or a full volume, would hand the turn a prompt with
  # no brief in it. The step is claimed by then, so the pass runs no
  # turn and says what landed — the claim, its run when it was read back,
  # and the brief kept in $pulled — and the run holds the slot, so the
  # next pass is that run's turn under the seat's own prompt. The status
  # is read after, never through `if !`: bash 5.2 answers a negated group
  # whose own redirection fails as if it had succeeded (measured
  # 2026-11-21 on the pod's 5.2.15, `! { :; } >dir` takes no branch).
  {
    cat "$prompt_file" &&
      printf '\n## This turn'\''s step\n\nThe seat pulled this step from its station with `boss dispatch --next` (infra/dev/factory-loop.sh, backlog 35f828cf): it is claimed as you and it is this turn'\''s work. Build it under the brief below; the turn rules above still hold.\n\n' &&
      cat "$pulled"
  } 2>/dev/null >"$turn_prompt"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    turn_from=''
    climb
    why="the pulled step is claimed (run ${pull_run:-not read back}) but its turn prompt $turn_prompt could not be written; its brief is kept at $pulled, and the claimed run's turn runs under the seat's prompt"
    say "pass $turn: $why (next pass in ${wait_s}s, failure $failures in a row)"
    note_restart "seat turn prompt not written: $why" null "$wait_s"
    return 0
  fi
  turn_from=$turn_prompt
}

# The end of every pass: the bound, then the wait.
end_pass() {
  if [ "$max_turns" -gt 0 ] && [ "$turn" -ge "$max_turns" ]; then
    say "stopping after $turn turn(s) (FACTORY_MAX_TURNS)"
    end_session "seat loop stopped after $turn turn(s) (FACTORY_MAX_TURNS)"
    exit 0
  fi
  "$sleep_cmd" "$wait_s"
}

# A claude seat stopped by a signal — a pod roll's TERM, a tmux
# kill-session's HUP, a ^C — ends its conversation's packet on the way
# out, so the record never shows a stopped seat as an open session for
# the six hours the silent-session clock would take to say otherwise.
# Bash runs the trap once the turn in flight returns.
stop() {
  say "stopped by SIG$1"
  end_session "seat loop stopped by SIG$1"
  exit $((128 + $2))
}
if [ "$kind" = claude ]; then
  trap 'stop HUP 1' HUP
  trap 'stop INT 2' INT
  trap 'stop TERM 15' TERM
fi

while :; do
  turn=$((turn + 1))
  rotate_log
  turn_from=$prompt_file
  pull_outcome=''
  pull_run=''
  if [ "$seat_pull" = next ]; then
    pull_pass
    if [ -z "$turn_from" ]; then
      # A pass that runs no turn still says the supervisor is alive.
      heartbeat pull
      end_pass
      continue
    fi
  fi
  resumed=$session
  if [ "$kind" = claude ]; then
    # The loop names the conversation itself, so the id is known before
    # the turn prints anything: a fresh turn creates it, every later one
    # resumes it. `auto` is the permission mode the interactive seats
    # run under; -p has nobody to answer a prompt either.
    if [ -n "$session" ]; then
      cmd=("$cli" -p --permission-mode auto --resume "$session")
    else
      session=$(cat /proc/sys/kernel/random/uuid)
      cmd=("$cli" -p --permission-mode auto --session-id "$session")
    fi
  elif [ -n "$session" ]; then
    cmd=("$cli" exec resume -c "$policy" "$session" -)
  else
    cmd=("$cli" exec -c "$policy" -)
  fi
  [ "$kind" = claude ] && own_session
  heartbeat start
  say "turn $turn: ${cmd[*]} < $turn_from"
  timeout --kill-after=60 "$turn_timeout" "${cmd[@]}" <"$turn_from" 2>&1 | tee "$turn_out" | log_turn
  code=${PIPESTATUS[0]}
  heartbeat end

  # The header's own line, in the turn's first lines, first match only.
  # Codex's only: a claude seat's id is the one the loop chose.
  if [ "$kind" = codex ]; then
    seen=$(sed -n '1,40{/^session id: [0-9a-fA-F-]\{36\}$/{s/^session id: //p;q}}' "$turn_out")
    if [ -n "$seen" ] && [ "$seen" != "$session" ]; then
      session=$seen
      resume_failures=0
    fi
  fi

  cli_errors=$(grep -E "$err_re" "$turn_out")
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
        end_session "seat loop dropped conversation $session: its CLI no longer has it"
        session=''
        resume_failures=0
      fi
    fi
    climb
    say "turn $turn FAILED ($reason, exit $code); relaunching in ${wait_s}s (failure $failures in a row)"
    note_restart "$reason" "$code" "$wait_s"
  fi

  end_pass
done
