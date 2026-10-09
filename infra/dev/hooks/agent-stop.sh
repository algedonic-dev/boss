#!/usr/bin/env bash
# PostToolUse on the Agent tool → `boss dispatch <run> --report`
# (design 511fa7d4 car 2b, backlog da925366). The run is the one
# agent-start.sh remembered under this call's tool_use_id; a call it
# never recorded (no packet in the prompt, or the door was unreachable
# at start) is not reported — there is no run to report on.
#
# The summary is the Agent tool's own result — `tool_response.content`
# text blocks, joined — copied, not retyped, and a response with no text
# is not reported at all (below); the tokens are the
# response's `usage` split when it carries one (the only shape the rate
# card prices). `--report` is car 3's half of the run
# (feat/agent-controls-station-per-role-model-and-budgeted-claim); on a
# CLI without it the verb refuses the flag and this hook logs that,
# which is the record saying the report did not land. Exit 0 always;
# see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
[ "$(field .tool_name)" = Agent ] || exit 0
session_dir
tool_use=$(field .tool_use_id)
[ -n "$tool_use" ] || bail "the payload names no tool_use_id"
case "$tool_use" in */*|.|..) bail "tool_use_id $tool_use is not a filename" ;; esac
[ -s "$dir/runs/$tool_use" ] || bail "call $tool_use opened no run — nothing to report"
run=$(tr -d '[:space:]' < "$dir/runs/$tool_use")
need_boss
summary=$(printf '%s' "$payload" | jq -r '
  .tool_response
  | if type == "string" then .
    elif type == "object" then ([.content[]? | select(.type == "text") | .text] | join("\n"))
    else "" end' 2>/dev/null || true)
# NO TEXT IS NOT A HANDBACK (backlog b5a3a174, cause named by ec97dbeb).
# A BACKGROUND Agent call returns at launch — a status and an agent id,
# no text, no usage — and this hook used to report that as the run's
# handback under the summary "(the Agent tool returned no text)". The
# pair it put on the packet, a minute into the run, is what the run then
# LANDED on: `reported` completed with the placeholder as its summary,
# an agent_runs row holding no count, and the agent's own report — the
# real usage — refused on that row. 21 of the 56 runs of 2026-10-08.
# So the hook reports what the tool handed back, or nothing. The run it
# remembered stays remembered; the agent's own `boss dispatch --report`
# is the report, and a run that never sends one ends `unreported`.
case "$summary" in
  *[![:space:]]*) ;;
  *) bail "call $tool_use handed back no text, so run $run is not reported from here — a launch is not a handback; the agent's own boss dispatch --report is" ;;
esac
tokens=$(printf '%s' "$payload" | jq -r '
  .tool_response.usage
  | select(type == "object")
  | select((.input_tokens | type) == "number" and (.output_tokens | type) == "number")
  | "\(.input_tokens),\(.output_tokens)"' 2>/dev/null || true)
if [ -n "$tokens" ]; then
  out=$(boss dispatch "$run" --report --summary "$summary" --tokens "$tokens" 2>&1)
else
  out=$(boss dispatch "$run" --report --summary "$summary" 2>&1)
fi
status=$?
[ "$status" -eq 0 ] || bail "boss dispatch --report on run $run refused: ${out##*$'\n'}"
rm -f "$dir/runs/$tool_use"
log "run $run reported (${#summary} bytes of summary${tokens:+, tokens $tokens})"
exit 0
