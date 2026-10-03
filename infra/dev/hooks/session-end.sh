#!/usr/bin/env bash
# SessionEnd → the work-session ends cleanly (design 511fa7d4 car 2b,
# backlog da925366): its `active` step is completed with
# `ended = clean` and the packet's metadata takes `ended_at` and Claude
# Code's own `end_reason`. Through `boss-api` rather than the boss
# shim, because SessionEnd hooks share a 1.5 s budget and the shim
# weighs its candidate binary (git, --version) before every verb;
# a few curls fit (a read, the step's key, its status, the packet's
# end), a shim round trip may not. A session this hook does
# not reach — the budget, a dark system of record, a lid closed with
# no hook fired at all — is ended by the clock rule
# `work-session-ends-when-silent` six hours after its last heartbeat,
# which is why the rule exists. Exit 0 always; see lib.sh.
. "$(dirname "$(readlink -f "$0")")/lib.sh"
read_payload
session_dir
session_packet
[ -n "$packet" ] || bail "no work-session for session $(field .session_id) — nothing to end"
door=$(command -v boss-api 2>/dev/null) || bail "boss-api is not on PATH — work-session $packet is left to the clock"
# PAST A STALE DOOR (backlog 584dc9da). Since 0b36dd65 boss-api
# REFUSES a write (exit 78) when its own copy is behind origin/main,
# and the pod's checkout was behind for most of 2026-09-19 — so a
# session ending then fell to the bail below, "left to the clock",
# at the one moment nobody reads the journal. The refusal guards the
# door's routing and flags from going stale; this write is the
# session closing its OWN packet, on the jobs port every copy of the
# door has routed to since it existed, and the clock rule that closes
# it otherwise knows less than this hook does. So the hook writes
# with the override — and first asks the door's own helper whether
# the override is doing anything, and says so on a line of its own,
# so how often a session ends past a stale door is a count in the
# journal, not a re-derivation, if this call is ever the wrong one.
# The judgement is local git and no fetch (door-freshness.sh).
freshness="$(dirname "$(readlink -f "$0")")/../door-freshness.sh"
if [ -r "$freshness" ]; then
  # shellcheck source=infra/dev/door-freshness.sh
  . "$freshness"
  if stale=$(door_is_stale "$door"); then
    log "work-session $packet is ended past a stale door (BOSS_DOOR_FRESHNESS=off): $stale"
  fi
fi
export BOSS_DOOR_FRESHNESS=off
job=$(boss-api GET "/api/jobs/$packet" 2>/dev/null) \
  || bail "cannot read work-session $packet — left to the clock"
# The open `active` step. TWO WRITES, THE KEY THROUGH THE STEP MERGE
# DOOR AND THEN THE STATUS ALONE (backlog e39a9d2a, Stage 2): this was
# one PUT carrying the step's whole read metadata back beside `ended`,
# because the step PUT replaces metadata wholesale. The merge door
# keeps every stored key without a read-back, and the PUT's end state
# refuses any metadata body. The merge goes first so the flip is not
# sent over a key that did not land.
step_id=$(printf '%s' "$job" | jq -r '[.steps[]? | select(.spec_slug == "active" and (.status == "ready" or .status == "active"))][0].id // empty' 2>/dev/null || true)
[ -n "$step_id" ] || bail "work-session $packet has no open active step — already ended"
printf '%s\n' '{"ended":"clean"}' > "$dir/ended-key.json" 2>/dev/null \
  || bail "cannot write $dir/ended-key.json"
printf '%s\n' '{"status":"completed"}' > "$dir/end.json" 2>/dev/null \
  || bail "cannot write $dir/end.json"
out=$(boss-api PATCH "/api/jobs/$packet/steps/$step_id/metadata" "$dir/ended-key.json" 2>&1) \
  || bail "ending work-session $packet refused at the merge door: ${out##*$'\n'}"
out=$(boss-api PUT "/api/jobs/$packet/steps/$step_id" "$dir/end.json" 2>&1) \
  || bail "ending work-session $packet refused: ${out##*$'\n'}"
jq -n --arg at "$(now_utc)" --arg reason "$(field .reason)" \
  '{ended_at: $at, end_reason: $reason}' > "$dir/ended.json" 2>/dev/null
boss-api PATCH "/api/jobs/$packet/metadata" "$dir/ended.json" >/dev/null 2>&1 \
  || log "ended_at on work-session $packet not written"
log "work-session $packet ended clean ($(field .reason))"
rm -rf "$dir"
exit 0
