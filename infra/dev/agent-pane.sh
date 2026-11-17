#!/usr/bin/env bash
# agent-pane.sh — the one place the dev pod reads another agent's tmux
# pane, pastes a labelled peer message into it, or starts the supervised
# factory loop (backlog ccd985ef, decided 2026-10-29).
#
#   agent-pane.sh factory <actor> <from> <message-file> <record-dir>
#   agent-pane.sh send    <actor> <from> <message-file> <record-dir>
#
# THE CASE. On 2026-10-29 the interactive Codex factory (dev-session.sh
# option 2, tmux `codex`) ended its turn at about 04:01Z and sat idle at
# its prompt for about ten hours: an interactive CLI acts only while it
# answers a prompt. Silent alarm 7e947dbf fired at 06:10Z, and nothing
# could act on it — the restart loop (factory-loop.sh) runs only under
# option 6, and the conductor had no door. Peer messages between agents
# were tmux commands typed by hand, each time a little differently.
#
# `factory` — what `boss factory restart` runs — does exactly ONE of:
#   (a) tmux `codex-factory` runs the supervised loop: nothing. It
#       prints outcome=already_supervised. Never a second loop. The
#       session's NAME is not the proof (N4, review b6467d24): its first
#       pane must be alive and started with option 6's own command,
#       $BOSS_DEV_ROOT/boss/infra/dev/factory-loop.sh. A session holding
#       the name and running anything else is refused (69) — it is not
#       supervised, and its name blocks option 6. A loop started outside
#       tmux is not seen at all: factory-loop.sh holds no lock.
#   (b) tmux `codex` runs (the interactive TUI): never killed, never
#       interrupted. When its pane is IDLE at an empty prompt it is woken
#       with the labelled message (outcome=woken); otherwise nothing is
#       typed (outcome=busy, with the reason).
#   (c) neither: option 6 is started DETACHED through dev-session.sh's
#       own `--detached 6`, so the session, its identity and its command
#       are the menu's, defined once (outcome=started, with its pid).
# `send` — what `boss peer send` runs — is (b)'s read and paste alone:
# outcome=sent, or a refusal (75) with nothing typed.
#
# Every outcome also prints `boss_actor=`: the BOSS_ACTOR in the target
# session's tmux environment (`show-environment`, read-only), `(blank)`
# or `(unset)` — so a factory that signs as nobody, and so cannot write
# to the session the wake points it at, is on the record as such (N6,
# review b6467d24).
#
# IDLE IS PROVEN OFF THE PANE, NEVER INFERRED. Measured on the live
# `codex` pane on 2026-10-29: the input box, `› Ask Codex to do anything`
# when empty, is drawn at the bottom whether or not a turn is running, so
# the box alone is never idle. A box holding text is somebody's draft —
# a paste would append to it; a pane drawing no box at all may be a
# dialog whose Enter confirms a choice (measured on the `shell` session's
# Claude, the same day: "Enter to confirm"). Both are busy.
#
# The first version read idle whenever two strings (`Working (`, `esc to
# interrupt`) were absent from the eight lines above the box, and review
# b6467d24 typed into two running turns that way: Codex swaps "Working"
# for a header taken from the reasoning summary, and a narrow window cuts
# the hint off after `esc` (its fixture A); queued `↳` messages push the
# Working line out of eight lines (fixture B). In Codex 0.159.3 an Enter
# during a turn submits at once and interrupts the model ("Model
# interrupted to submit steer instructions"). An absent marker is no
# evidence. So idle now takes all three of:
#   - the LAST line starting with the prompt glyph is exactly the empty
#     box (trailing padding aside);
#   - no busy marker anywhere in the capture above it: `Working (`,
#     `esc to interrupt`, a turn's elapsed counter `(1m 05s •`, a queued
#     `↳` message;
#   - a SECOND capture, $recheck s later (at least 1.5 s: a running
#     turn's counter ticks every second), byte-identical to the first;
#   - the running-status line — the last line drawn above the box — not
#     cut off with an ellipsis (B4, below);
#   - tmux reporting the pane as taking input as typed (B3, below).
#
# THE PANE MUST TAKE INPUT AS TYPED (B3, review 1924c625). Measured on a
# private tmux 3.3a server: a pane in copy-mode — what C-b [ leaves when
# a human scrolls back to read the factory, and the mode outlives a
# detach — still captures its BASE screen, so it read idle; paste-buffer
# -p then looked for bracketed-paste mode on the copy-mode screen, found
# none, and sent the text raw (every CR a keystroke: the label line
# submitted a turn), and the Enter went to copy-mode. The helper printed
# woken. input_off is the other way a paste is dropped in silence. So
# the pane is busy unless `#{pane_in_mode} #{pane_input_off}` reads
# exactly `0 0`, read with each capture and again immediately before
# load-buffer.
#
# THE COUNTER MUST BE DRAWN (B4, review 1924c625). The 1.5 s proof rests
# on a running turn's counter ticking, and Codex cuts its status line at
# the pane's width: a reasoning-summary header that fills it loses the
# `(1m 05s • esc …)` after it — cut with an ellipsis (fixture A2), or, in
# a pane about sixty columns wide, at the space before the `(` (A3) —
# and the header's shimmer is colour, which a plain capture never sees.
# So the status line ending in an ellipsis is busy, and a pane under 80
# columns is refused (69) before it is read: a phone or a narrow client
# leaves the window that size after it detaches. ONLY the status line:
# Codex folds a COMPLETED call into one line ending in an ellipsis too
# (`• Ran python3 - <<'PY' …`, measured by the conductor on the live
# idle pane, 2026-10-29, width 134), and such a line is never a running
# status, or the door would never wake an idle Codex.
# Reading busy for idle costs one recorded `busy`; reading idle for busy
# types into someone's work, so every doubt reads busy. Only an actor
# whose empty prompt has been MEASURED is in the table below; any other
# is refused by name.
#
# THE PASTE is the sequence agents already used by hand — load-buffer,
# paste-buffer, Enter — with a capture before and after, kept in the
# record directory with the exact text pasted (before.txt, after.txt,
# pasted.txt) so the caller can put what was seen on the record. The
# paste is bracketed (`-p`, when the application asked for it, as the
# codex TUI does), so a multi-line message arrives as one input rather
# than one Enter per line; `-d` drops the buffer after. That holds only
# while the pane is in no tmux mode, which is why B3's read comes last,
# just before load-buffer: in copy-mode `-p` brackets nothing.
#
# ONLY TEXT IS TYPED (B2, review b6467d24). tmux pastes the buffer byte
# for byte and 3.3a filters nothing: measured on a private server, an
# ESC[201~ inside the buffer ended the bracketed paste early, and the
# ^C, Esc and CR after it reached the application as keystrokes —
# interrupt, interrupt, submit. The text comes from --why, a peer-send
# file and every open alarm's title, which any actor who can file a
# backlog-item writes. So the message and the sender, and then the whole
# labelled text, must hold nothing but printable characters (valid
# UTF-8, no C0 or C1 control, no DEL), newlines and tabs, or it is
# refused (66) before the pane is even read.
#
# THE RACE THAT STAYS OPEN (N1, review b6467d24). Between the second
# capture and the paste there is a window, milliseconds wide, that
# nothing here can close: tmux has no compare-and-paste. A key a human
# types into the pane inside it is merged into the paste and submitted
# with its Enter, and a turn that starts inside it takes that Enter as a
# steer. It is kept short — nothing is read between the judgement and
# the paste but the pane's pid — and it is named here rather than
# hidden. The capture after, on the record, shows what the paste met.
#
# NOTHING IS EVER KILLED OR INTERRUPTED: no tmux verb here ends a process
# and the only key sent is the one Enter after the paste (pinned by the
# test, and by the text check above for the keys a message could carry).
#
# Refusals, each named on stderr, each before anything is typed or
# started: 64 a usage error or an actor with no pane defined here; 66 an
# empty message, or one or a sender holding a byte that is not text; 69
# no tmux server (this is not the dev pod, the one machine that hosts
# the factory), for `send` no interactive pane, or a pane under 80
# columns (B4);
# 74 a pane that cannot be read; 75 `send` into a busy pane. 70 is an act
# that failed part-way — what was done is printed.
#
# THE ROOT LOGIN PATH'S NEIGHBOUR. This file types into another agent's
# process and starts one, so, in the words the gate's hold reads off a
# file's own text (boss-cli mutating_verb.rs, backlog 275d75af): every
# change to this file is a credentials-area car and waits for an
# adversarial review.
#
# Pinned by crates/core/boss-testing/tests/agent_pane_sh.rs. The seams,
# unset on the pod: AGENT_PANE_SETTLE (seconds between the Enter and the
# capture after; 2), and BOSS_DEV_ROOT, passed through to dev-session.sh.

set -u

here=${0%/*}
# This file's own age (backlog 0b36dd65): it runs from the main checkout.
if [ -r "$here/door-freshness.sh" ]; then
  # shellcheck source=door-freshness.sh
  . "$here/door-freshness.sh"
  if stale=$(door_is_stale "$0"); then
    echo "agent-pane: WARNING - $stale" >&2
  fi
fi

settle=${AGENT_PANE_SETTLE:-2}
# Seconds between the two captures that prove idle. No seam: the proof
# is the gap (B1, review b6467d24), and the test measures it.
recheck=2

refuse() {
  local code=$1
  shift
  echo "agent-pane: REFUSED - $* - nothing typed, nothing started" >&2
  exit "$code"
}

usage() {
  refuse 64 "usage: agent-pane.sh factory|send <actor> <from> <message-file> <record-dir>"
}

[ $# -eq 5 ] || usage
verb=$1 actor=$2 from=$3 message=$4 record=$5
case $verb in factory | send) ;; *) usage ;; esac

# The actors whose panes this file can read: the tmux sessions
# dev-session.sh's menu runs them in (the test holds these to the menu's)
# and the empty prompt measured on the live pane. Only these.
case $actor in
  agent-codex | codex@algedonic.dev)
    interactive=codex supervised=codex-factory
    empty_prompt='› Ask Codex to do anything'
    ;;
  *)
    refuse 64 "no pane is defined for actor '$actor' in infra/dev/agent-pane.sh (only agent-codex's empty prompt has been measured)"
    ;;
esac

[ -s "$message" ] || refuse 66 "the message file '$message' is missing or empty"
mkdir -p "$record" || refuse 66 "cannot make the record directory '$record'"

# Exit 0 when file $1 holds only text a pane may be typed: valid UTF-8,
# printable characters, newlines and tabs. Otherwise print what it holds
# and where, and exit 1. python3 because a shell cannot see a NUL or
# decode UTF-8; without it nothing can be checked, so nothing is typed.
plain_py='
import sys
b = open(sys.argv[1], "rb").read()
try:
    t = b.decode("utf-8")
except UnicodeDecodeError as e:
    print("byte 0x%02x at offset %d, which is not UTF-8" % (b[e.start], e.start))
    sys.exit(1)
for i, c in enumerate(t):
    o = ord(c)
    if c not in "\t\n" and (o < 0x20 or 0x7f <= o <= 0x9f):
        print("control character U+%04X at byte offset %d" % (o, len(t[:i].encode("utf-8"))))
        sys.exit(1)
'
plain_text() { # <file> <what it is>
  local found
  command -v python3 >/dev/null 2>&1 ||
    refuse 66 "cannot check $2 for control bytes here (python3 is not installed)"
  found=$(python3 -I -c "$plain_py" "$1" 2>&1) ||
    refuse 66 "$2 holds a $found - only printable characters, newlines and tabs are typed into a pane (a control byte is a keystroke: ESC[201~ ends the paste, ^C and Esc interrupt, CR submits)"
}
plain_text "$message" "the message"
printf '%s' "$from" >"$record/from.txt" || refuse 66 "cannot write to the record directory '$record'"
plain_text "$record/from.txt" "the sender '$(printf '%s' "$from" | tr -c '[:print:]' '?')'"
rm -f "$record/from.txt"

command -v tmux >/dev/null 2>&1 ||
  refuse 69 "no tmux server here (tmux is not installed): the factory runs only on the dev pod"
tmux list-sessions >/dev/null 2>&1 ||
  refuse 69 "no tmux server here: the factory runs only on the dev pod"

running() { tmux has-session -t "=$1" 2>/dev/null; }

pane_pid() { tmux list-panes -t "=$1:" -F '#{pane_pid}' 2>/dev/null | sed -n 1p; }

# Option 6's command, as dev-session.sh's start_factory gives it to tmux.
loop_cmd=${BOSS_DEV_ROOT:-/work}/boss/infra/dev/factory-loop.sh

# What tmux `$supervised`'s first pane is: `loop` when it is alive and
# was started with $loop_cmd, else a description of what it is.
supervised_pane() {
  local got dead cmd
  got=$(tmux list-panes -t "=$supervised:" -F '#{pane_dead} #{pane_start_command}' 2>/dev/null | sed -n 1p)
  dead=${got%% *}
  cmd=${got#* }
  # tmux may quote the command it was given.
  cmd=${cmd#\"}
  cmd=${cmd%\"}
  if [ "$dead" = 0 ] && [ "$cmd" = "$loop_cmd" ]; then
    echo loop
  elif [ "$dead" = 1 ]; then
    echo "a dead pane"
  elif [ -z "$got" ]; then
    echo "a pane tmux would not describe"
  else
    printf "a pane running '%s'\n" "$(printf '%s' "$cmd" | tr -c '[:print:]' '?')"
  fi
}

# The BOSS_ACTOR in session $1's tmux environment: its value, `(blank)`,
# `(unset)`, or `(unreadable)`.
session_actor() {
  local line
  line=$(tmux show-environment -t "=$1" BOSS_ACTOR 2>&1)
  case $line in
    BOSS_ACTOR=) echo '(blank)' ;;
    BOSS_ACTOR=*) printf '%s\n' "${line#BOSS_ACTOR=}" | sed -n 1p ;;
    -BOSS_ACTOR | *'unknown variable'*) echo '(unset)' ;;
    *) echo '(unreadable)' ;;
  esac
}

# Capture the pane into $1, or refuse: a capture that fails or holds no
# text is no reading, and never an idle one.
capture() {
  if ! tmux capture-pane -p -t "$target" >"$1" 2>/dev/null; then
    refuse 74 "could not read tmux \`$interactive\`'s pane (capture-pane failed)"
  fi
  grep -q '[^[:space:]]' "$1" ||
    refuse 74 "could not read tmux \`$interactive\`'s pane (the capture is empty)"
}

# What a running turn draws above the box, anywhere above it: the
# Working header, the interrupt hint, the turn's elapsed counter — which
# survives a reasoning-summary header and a hint cut off after `esc`
# (`(1m 05s • esc`) — and a queued message.
busy_marker='Working( \(|$)|esc to interrupt|\(([0-9]+h )?([0-9]+m )?[0-9]+s( •|$)|↳'
# A dialog's own words (N8): an approval or a choice waiting on Enter.
dialog_marker='enter to confirm|esc to cancel|would you like to'
# A completed call Codex folded into one line (B4's other edge).
folded_call='^[[:space:]]*(• )?Ran '

# `idle`, or `busy <reason>`, read off ONE capture file. Idle here is
# necessary, not sufficient: the caller also requires a second, identical
# capture.
judge() {
  local file=$1 n line status
  n=$(grep -n '^› ' "$file" | tail -n 1 | cut -d: -f1)
  if [ -z "$n" ]; then
    echo "busy no input box is drawn (a dialog or another screen holds the pane)"
    return
  fi
  line=$(sed -n "${n}p" "$file" | sed 's/[[:space:]]*$//')
  if [ "$line" != "$empty_prompt" ]; then
    # N8 (review 1924c625): a dialog's last column-0 glyph line is a
    # history message (fixture D) or its own choice (K), not a draft.
    if grep -Eiq "$dialog_marker" "$file" || grep -Eq '^› [0-9]+\. ' <<<"$line"; then
      echo "busy a dialog is drawn (it asks to confirm or to choose), not the empty prompt: an Enter would answer it"
    else
      echo "busy its input box holds a draft, not the empty prompt"
    fi
    return
  fi
  [ "$n" -gt 1 ] || { echo idle; return; }
  if head -n "$((n - 1))" "$file" | grep -E "$busy_marker" >/dev/null; then
    echo "busy a running turn's marker (Working, its elapsed counter, the interrupt hint or a queued message) is drawn above its prompt"
    return
  fi
  # B4: the running-status line is the last line drawn above the box. A
  # folded completed call (`• Ran … …`) is never one.
  status=$(head -n "$((n - 1))" "$file" | grep '[^[:space:]]' | tail -n 1 | sed 's/[[:space:]]*$//')
  if grep -Eq '(…|\.\.\.)$' <<<"$status" && ! grep -Eq "$folded_call" <<<"$status"; then
    echo "busy its status line ends in an ellipsis: the width cut it before the turn's counter, so the pane cannot prove it is still"
    return
  fi
  echo idle
}

# B3: `idle` when tmux reports the pane taking input as typed — in no
# mode, input on — else `busy <reason>`. Anything but exactly `0 0`,
# an empty or unreadable answer included, is busy.
mode_judge() {
  local mode
  mode=$(tmux display-message -p -t "$target" '#{pane_in_mode} #{pane_input_off}' 2>/dev/null | sed -n 1p)
  if [ "$mode" = "0 0" ]; then
    echo idle
  else
    echo "busy the pane is not taking input as typed: tmux reports pane_in_mode pane_input_off as '$(printf '%s' "$mode" | tr -c '[:print:]' '?')', not '0 0' (copy-mode or input off: a paste would go unbracketed and its Enter would not reach the application)"
  fi
}

# B4: refuse (69) a pane narrower than 80 columns, or one whose width
# tmux will not report, before it is read.
width_floor() {
  local width
  width=$(tmux display-message -p -t "$target" '#{pane_width}' 2>/dev/null | sed -n 1p)
  case ${width:-empty} in
    empty | *[!0-9]*)
      refuse 69 "tmux would not report the width of \`$interactive\`'s pane: a pane under 80 columns cuts the status line before the turn's counter, so it cannot be proven idle"
      ;;
  esac
  [ "$width" -ge 80 ] ||
    refuse 69 "tmux \`$interactive\`'s pane is $width columns wide, under 80 columns: Codex cuts its status line before the turn's counter there, so the pane cannot be proven idle (widen the window, or reattach a wider client)"
}

# Print the busy outcome for `$1` (`busy <reason>`) and exit: 0 for the
# factory, which records it; 75 for a send, which is refused.
busy_exit() {
  echo "outcome=busy"
  echo "session=$interactive"
  echo "pid=$pid"
  echo "reason=${1#busy }"
  echo "boss_actor=$(session_actor "$interactive")"
  if [ "$verb" = send ]; then
    echo "agent-pane: not typed - tmux \`$interactive\` is busy: ${1#busy }" >&2
    exit 75
  fi
  exit 0
}

# The labelled message, the sequence, the capture after. Prints what it
# did; a tmux step that fails is exit 70, naming the step.
paste() {
  local buf=boss-peer-$$
  {
    printf '[BOSS peer message from %s at %s, sent by boss %s]\n\n' \
      "$from" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1"
    cat "$message"
  } >"$record/pasted.txt"
  # The whole text as it will be typed, label included.
  plain_text "$record/pasted.txt" "the labelled message"
  # B3: the last read before anything is typed.
  local mode
  mode=$(mode_judge)
  if [ "$mode" != idle ]; then
    rm -f "$record/pasted.txt"
    busy_exit "$mode"
  fi
  tmux load-buffer -b "$buf" "$record/pasted.txt" ||
    { echo "outcome=paste_failed"; echo "reason=load-buffer failed; nothing was pasted"; exit 70; }
  tmux paste-buffer -p -d -b "$buf" -t "$target" ||
    { echo "outcome=paste_failed"; echo "reason=paste-buffer failed; nothing was pasted"; exit 70; }
  tmux send-keys -t "$target" Enter ||
    { echo "outcome=paste_failed"; echo "reason=the message was pasted, and the Enter after it failed"; exit 70; }
  [ "$settle" = 0 ] || sleep "$settle"
  tmux capture-pane -p -t "$target" >"$record/after.txt" 2>/dev/null ||
    echo "agent-pane: the capture after the paste failed - the paste itself was sent" >&2
}

target="=$interactive:"

if [ "$verb" = factory ]; then
  # (a) The supervised loop runs: nothing more.
  if running "$supervised"; then
    what=$(supervised_pane)
    [ "$what" = loop ] ||
      refuse 69 "tmux \`$supervised\` runs, but it is $what, not $loop_cmd: it is not the supervised loop, and its name blocks option 6"
    echo "outcome=already_supervised"
    echo "session=$supervised"
    echo "pid=$(pane_pid "$supervised")"
    echo "boss_actor=$(session_actor "$supervised")"
    exit 0
  fi
  # (c) No factory at all: option 6, detached, through the menu's own
  # definition.
  if ! running "$interactive"; then
    launched=$(bash "$here/dev-session.sh" --detached 6)
    rc=$?
    printf '%s\n' "$launched" | grep -v "^$supervised [0-9]*$" >&2
    pid=$(printf '%s\n' "$launched" | sed -n "s/^$supervised \([0-9][0-9]*\)$/\1/p" | sed -n 1p)
    if [ "$rc" -ne 0 ] || [ -z "$pid" ] || ! running "$supervised"; then
      echo "outcome=start_failed"
      echo "session=$supervised"
      echo "reason=dev-session.sh --detached 6 exited $rc and tmux \`$supervised\` is $(running "$supervised" && echo running || echo 'not running')"
      exit 70
    fi
    echo "outcome=started"
    echo "session=$supervised"
    echo "pid=$pid"
    echo "boss_actor=$(session_actor "$supervised")"
    exit 0
  fi
fi

# (b), and `send`: the interactive pane.
if ! running "$interactive"; then
  if running "$supervised"; then
    refuse 69 "$actor runs as the supervised loop (tmux \`$supervised\`), which reads no pane: its orders go on its work-session"
  fi
  refuse 69 "$actor has no interactive pane (no tmux \`$interactive\`)"
fi
width_floor
capture "$record/before.txt"
state=$(mode_judge)
[ "$state" != idle ] || state=$(judge "$record/before.txt")
if [ "$state" = idle ]; then
  # The proof's second half: the same bytes $recheck s later.
  sleep "$recheck"
  capture "$record/recheck.txt"
  state=$(mode_judge)
  if [ "$state" != idle ]; then
    :
  elif ! cmp -s "$record/before.txt" "$record/recheck.txt"; then
    state="busy the pane changed between two captures ${recheck} s apart (a running turn redraws; idle holds still)"
  else
    state=$(judge "$record/recheck.txt")
  fi
fi
pid=$(pane_pid "$interactive")
[ "$state" = idle ] || busy_exit "$state"
if [ "$verb" = factory ]; then
  paste "factory restart"
  echo "outcome=woken"
else
  paste "peer send"
  echo "outcome=sent"
fi
echo "session=$interactive"
echo "pid=$pid"
echo "boss_actor=$(session_actor "$interactive")"
exit 0
