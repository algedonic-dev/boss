#!/usr/bin/env bash
# dev-session.sh — the dev pod's login selector (backlog e2d63c28,
# David 2026-10-01).
#
# An interactive ssh login used to exec straight into the one durable
# tmux session, `dev` — the Remote Control / drain session the boot
# starts running /work/dev-claude.sh. It now offers four sessions:
#
#   1) Claude + Claude Code   tmux `dev`     /work/dev-claude.sh
#   2) GPT + Codex            tmux `codex`   codex
#   3) Gemini + Code Assist   tmux `gemini`  gemini
#   4) Shell                  tmux `shell`   bash -l
#
# Each JOINS its session when it runs and CREATES it when it does not
# (`tmux new-session -A`). Enter, or no answer within the timeout, is 1,
# so anything that relied on a login landing in `dev` still lands there.
#
# A LOGIN CAN START THE DRAIN (review a27c860d F2, kept by design).
# With `dev` absent, option 1 creates it running /work/dev-claude.sh —
# what the boot runs — so while /work/home/.config/boss/drain-on-boot
# exists, an idle login (Enter, or the timeout) restarts claude with the
# drain prompt and appends its `dev session:` lines to postStart.log.
# Before the selector it got a plain bash in `dev`. Quitting claude no
# longer stops the drain past the next login; removing the switch does.
# Shell (4) is the way to a bare shell, e.g. to run `claude` once to log
# in.
#
# THE ROOT LOGIN PATH WAITS FOR A REVIEW (review a27c860d F4). Every
# interactive ssh login to the pod runs this file as root, and an edit
# that parses but exits closes every such login. While the menu lived in
# boss-dev.yaml it drew that manifest's hold; here it would ride a train
# on the gate alone. So, in the words the gate's hold reads off a file's
# own text (boss-cli mutating_verb.rs, backlog 275d75af): every change
# to this file is a credentials-area car and waits for an adversarial
# review.
#
# WHY IT LIVES HERE AND NOT IN THE MANIFEST. boss-dev.yaml's
# /work/dev-session.sh only execs this copy (falling back to today's
# `tmux new-session -A -s dev` when it is missing or does not parse), so
# an edit to the menu rides a train like any door in infra/dev/ and
# never rolls the pod — the roll restarts the operator's session.
# Pinned by crates/core/boss-testing/tests/dev_session_sh.rs.
#
# NO CREDENTIAL IS PLACED HERE. A missing CLI is reported with the
# command that installs it, and offered; each CLI's login (`codex
# login`, gemini's first-run auth) is David's, once, inside its session,
# and lives on the PVC home — the rule /work/home/.claude/.credentials.json
# already follows.
#
# BOSS_DEV_ROOT stands in for /work, BOSS_DEV_SESSION_TIMEOUT for the
# 10 s, and BOSS_DEV_SESSION_ASSUME_TTY=1 for a terminal: the test
# harness's seams, unset on the pod.

root=${BOSS_DEV_ROOT:-/work}
timeout=${BOSS_DEV_SESSION_TIMEOUT:-10}

# The session environment, as the pod's .bashrc and dev-claude.sh set it.
export HOME=$root/home
export PATH=$root/tools/bin:$root/home/.local/bin:/usr/local/cargo/bin:$PATH
export RUSTUP_HOME=/usr/local/rustup
export CARGO_TARGET_DIR=/scratch/target
cd "$root/boss" || true

# No terminal, or already inside tmux: no menu. This is the old
# dev-session.sh exactly — the .profile execs this only for interactive
# tty logins outside tmux, and a shell inside any session (the `shell`
# session's own login bash included) must never be offered it again.
if [ -n "${TMUX:-}" ] || { [ ! -t 0 ] && [ "${BOSS_DEV_SESSION_ASSUME_TTY:-}" != 1 ]; }; then
  exec tmux new-session -A -s dev
fi

# AND THIS FILE'S OWN AGE (backlog 0b36dd65): it runs from the main
# checkout, which the reclaim sidecar fast-forwards only hourly, so a
# menu edit can be on origin/main and not yet here. Warn and proceed —
# a login stopped by a door is worse than a stale menu. Exec'd by its
# real path, so $0's directory is this file's; no readlink fork.
here=${0%/*}
if [ -r "$here/door-freshness.sh" ]; then
  # shellcheck source=door-freshness.sh
  . "$here/door-freshness.sh"
  if stale=$(door_is_stale "$0"); then
    echo "dev-session: WARNING - $stale" >&2
  fi
fi

# jq_doc_text, from the tree's one definition. Missing, the registry
# read below fails closed: an agent session starts unsigned.
# shellcheck source=../lib/jq.sh
. "$here/../lib/jq.sh" 2>/dev/null || true

running() { tmux has-session -t "=$1" 2>/dev/null; }

status() { if running "$1"; then echo running; else echo 'not running'; fi; }

# The CLI an option's session needs to START (joining needs only tmux).
cli_of() {
  case $1 in
    dev) echo claude ;;
    codex) echo codex ;;
    gemini) echo gemini ;;
  esac
}

install_hint() {
  case $1 in
    claude) echo "curl -fsSL https://claude.ai/install.sh | HOME=$HOME bash" ;;
    codex) echo "npm i -g --prefix $HOME/.local @openai/codex" ;;
    gemini) echo "npm i -g --prefix $HOME/.local @google/gemini-cli" ;;
  esac
}

run_install() {
  case $1 in
    claude) curl -fsSL https://claude.ai/install.sh | bash ;;
    codex) npm i -g --prefix "$HOME/.local" @openai/codex ;;
    gemini) npm i -g --prefix "$HOME/.local" @google/gemini-cli ;;
  esac
}

# Say the CLI is missing and how to install it, offer to run that, and
# answer whether it is on PATH afterwards. No answer is no.
offer_install() {
  local cli=$1 answer=''
  printf '\n%s is not installed on this pod. Install it with:\n  %s\n' "$cli" "$(install_hint "$cli")"
  printf 'Install it now? [y/N] '
  read -r -t "$timeout" answer || echo
  case $answer in
    y | Y | yes) ;;
    *)
      echo "not installed."
      return 1
      ;;
  esac
  if run_install "$cli"; then
    hash -r
    command -v "$cli" >/dev/null 2>&1 && return 0
  fi
  echo "the install did not install $cli - run the command above by hand to see why."
  return 1
}

# WHO AN AGENT SESSION SIGNS AS (David 2026-10-01). The pod names
# Claude container-wide — BOSS_ACTOR=claude@algedonic.dev, and the actor
# file under $HOME — so a codex or gemini session would sign the audit
# log as Claude. Each signs as its OWN registered alias, or as nobody:
# BOSS_ACTOR blank and the actor file pointed at /dev/null, so every
# boss write is refused naming the fix and every read is marked
# operator:unidentified. Git authorship is its own either way.
#
# IN THE SESSION ENV, NOT THE FIRST PROCESS'S (review 23f1c6fd B1,
# reproduced on real tmux). The tmux server is started by the boot with
# the container env, so its GLOBAL env holds BOSS_ACTOR=claude, and
# every pane is the global env plus the session's. An `env -u` on the
# first command left every later window (prefix-c, a split) signing as
# Claude. So every identity value is a `-e` on new-session, which beats
# the global one in every pane — BOSS_ACTOR included, set BLANK when
# unsigned: identity.rs, boss-api, hooks/lib.sh, open-page-audits.ts
# and record-agent-runs.sh all read blank as no answer and fall through
# to BOSS_ACTOR_FILE, which names nothing.
#
# ASSERTED, NOT AUTHENTICATED (review 23f1c6fd N2). This is a provenance
# DEFAULT, not a boundary: any session can export BOSS_ACTOR itself, a
# session made by hand outside this menu inherits the server's Claude,
# and one created before this existed keeps what it was created with.
# It makes the honest attribution the one a session gets by doing
# nothing; it does not make another attribution impossible.
#
# Sets the caller's `flags`; `check` reads the registry (on create),
# `unsigned` does not (on join, where tmux ignores -e, and a session
# gone in between is then created unsigned, never Claude's).
agent_identity() {
  local alias=$1 who=$2 mode=$3 body actor=''
  if [ "$mode" = check ]; then
    # The door's EXIT STATUS is its verdict (1 on a non-2xx; its
    # HTTP:<code> line goes to stderr — review 23f1c6fd B2), and the
    # alias must be one of an agent's `aliases`, exactly: a substring
    # of the whole body would count an address in any field.
    # jq_doc_text first: on jq-1.6 `jq -e` over NO document exits 0, so
    # an empty answer would read as registered (backlog d96e38ab).
    if body=$(timeout 15 boss-api GET /api/agents 2>/dev/null) && jq_doc_text "$body" &&
      printf '%s' "$body" | jq -e 'has("data")' >/dev/null 2>&1; then
      if printf '%s' "$body" | jq -e --arg a "$alias" \
        'any(.data[]?; any(.aliases[]?; . == $a))' >/dev/null 2>&1; then
        actor=$alias
      else
        echo "$alias is not a registered agent (GET /api/agents)."
      fi
    else
      echo "could not read the agents registry (GET /api/agents) to confirm $alias."
    fi
    if [ -z "$actor" ]; then
      echo "The session starts with BOSS_ACTOR blank: boss writes are refused naming the fix, reads are marked operator:unidentified."
      echo "Register it: an agents-registry row whose aliases hold $alias (POST /api/agents/batch?mode=insert-if-absent)."
    fi
  fi
  flags=(-e BOSS_ACTOR_FILE=/dev/null
    -e "GIT_AUTHOR_NAME=$who" -e "GIT_AUTHOR_EMAIL=$alias"
    -e "GIT_COMMITTER_NAME=$who" -e "GIT_COMMITTER_EMAIL=$alias"
    -e "BOSS_ACTOR=$actor")
}

# A CLI's versioned config (infra/dev/cli/), placed on the PVC home only
# when it has none — never over a file David has edited; a difference
# is said, not settled. A difference means a versioned LINE missing
# from the placed file: codex appends its own keys on first run
# (`[tui] screen_reader_detection_done = true`, review 23f1c6fd N3), and
# a note that fires on every create is one nobody reads.
place_config() {
  local src=$root/boss/$1 dest=$HOME/$2 line have
  [ -f "$src" ] || return 0
  if [ ! -e "$dest" ]; then
    mkdir -p "${dest%/*}" && cp "$src" "$dest" && echo "placed $dest from the versioned $1"
    return 0
  fi
  have=$'\n'$(<"$dest")$'\n'
  while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    if [[ $have != *$'\n'"$line"$'\n'* ]]; then
      echo "note: $dest differs from the versioned $1 (it lacks: $line) - kept as it is"
      return 0
    fi
  done <"$src"
}

# Join or create session $1 running the rest of argv. Returns only when
# its CLI is missing and was not installed.
join() {
  local name=$1 cli mode=check
  shift
  local -a flags=() cmd=("$@")
  if running "$name"; then
    mode=unsigned
  else
    cli=$(cli_of "$name")
    if [ -n "$cli" ] && ! command -v "$cli" >/dev/null 2>&1; then
      offer_install "$cli" || return 1
    fi
  fi
  case $name in
    codex)
      agent_identity codex@algedonic.dev 'Codex (engineering)' "$mode"
      [ "$mode" = check ] && place_config infra/dev/cli/codex-config.toml .codex/config.toml
      ;;
    gemini)
      agent_identity gemini@algedonic.dev 'Gemini (engineering)' "$mode"
      [ "$mode" = check ] && place_config infra/dev/cli/gemini-settings.json .gemini/settings.json
      ;;
  esac
  exec tmux new-session -A -s "$name" -c "$root/boss" "${flags[@]}" "${cmd[@]}"
}

join_dev() {
  if [ -x "$root/dev-claude.sh" ]; then
    join dev "$root/dev-claude.sh"
  fi
  # No claude (or no dev-claude.sh): `dev` as a plain shell, which is
  # what a login made of a missing `dev` before the selector. Option 1
  # never strands a login at the menu.
  exec tmux new-session -A -s dev -c "$root/boss"
}

while :; do
  printf '\nBOSS dev pod - which session? (Enter or %ss = 1)\n' "$timeout"
  printf '  1) %-22s %-8s %s\n' 'Claude + Claude Code' dev "$(status dev)"
  printf '  2) %-22s %-8s %s\n' 'GPT + Codex' codex "$(status codex)"
  printf '  3) %-22s %-8s %s\n' 'Gemini + Code Assist' gemini "$(status gemini)"
  printf '  4) %-22s %-8s %s\n' 'Shell' shell "$(status shell)"
  printf '> '
  choice=''
  if ! read -r -t "$timeout" choice && [ -z "$choice" ]; then
    printf '\nno answer - 1 (Claude + Claude Code)\n'
  fi
  case $choice in
    '' | 1) join_dev ;;
    2) join codex codex ;;
    3) join gemini gemini ;;
    4) join shell bash -l ;;
    *) echo "no option '$choice'" ;;
  esac
done
