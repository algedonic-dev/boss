#!/usr/bin/env bash
# UserPromptSubmit → the heartbeat (design 511fa7d4 car 2b, backlog
# da925366): `last_active_at` and `prompt_count` merged onto the
# work-session packet through `boss job patch`. NEVER the prompt text —
# `.prompt` is not read here at all: the session's existence and rhythm
# are the record; what was said is the transcript's. The clock rule
# `work-session-ends-when-silent` reads `last_active_at` as movement.
#
# THEN THE WORKING WINDOW (backlog 37e3295c). On this event stdout is
# added to the model's context, so the ONE line printed is the window
# `boss session window` reads off this session's packet: open-ended, an
# `until` instant, ended, or closed. Twice in two days a session ended
# on an hour said in chat with urgent work ready; the end it honours now
# comes from the record, every prompt, and an extension is a write to
# the packet. A failed read prints nothing — never a guessed window.
# Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
session_dir
session_packet
[ -n "$packet" ] || bail "no work-session for session $(field .session_id) — SessionStart filed none"
need_boss
n=$(tr -d '[:space:]' < "$dir/prompts" 2>/dev/null || true)
case "${n:-empty}" in empty|*[!0-9]*) n=0 ;; esac
n=$((n + 1))
printf '%s\n' "$n" > "$dir/prompts"
jq -n --arg at "$(now_utc)" --argjson n "$n" \
  '{last_active_at: $at, prompt_count: $n}' > "$dir/heartbeat.json" 2>/dev/null \
  || bail "cannot write $dir/heartbeat.json"
if out=$(boss job patch "$packet" "$dir/heartbeat.json" 2>&1); then
  log "heartbeat $n on work-session $packet"
else
  log "heartbeat $n on $packet refused: ${out##*$'\n'}"
fi
# The verb's own line, and only when it answered as itself.
if window=$(boss session window "$packet" 2>/dev/null); then
  window=$(printf '%s\n' "$window" | sed -n '1{/^work-session /p}')
  [ -n "$window" ] && printf '%s\n' "$window"
else
  log "the window of $packet could not be read — nothing printed"
fi
exit 0
