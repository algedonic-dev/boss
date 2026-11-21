#!/usr/bin/env bash
# dev-session.sh — the dev pod's login selector (backlog e2d63c28,
# David 2026-10-01).
#
# An interactive ssh login used to exec straight into the one durable
# tmux session, `dev` — the Remote Control / drain session the boot
# starts running /work/dev-claude.sh. It now offers six sessions:
#
#   1) Claude + Claude Code   tmux `dev`     /work/dev-claude.sh
#   2) GPT + Codex            tmux `codex`   codex
#   3) Gemini + Code Assist   tmux `gemini`  gemini
#   4) Shell                  tmux `shell`   bash -l
#   5) Claude B + Claude Code tmux `claude-b` claude, own login home
#      ($HOME/.claude-b), signing as claude-b@algedonic.dev
#   6) GPT + Codex, factory   tmux `codex-factory`
#      infra/dev/factory-loop.sh: the same codex identity, the CLI's
#      non-interactive mode run one turn at a time and relaunched on a
#      model limit (backlog 628dc540). Option 2 stays the interactive
#      TUI; the two never start beside each other, because two CLIs
#      signing as one agent would race for one queue.
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
#
# `--detached 6` (backlog ccd985ef, decided 2026-10-29) starts option 6
# — the factory loop, with its session name, identity and command
# exactly as the menu's 6 has them, through the one `start_factory`
# below — in a DETACHED session, prints `<session> <pane pid>`, and
# exits. It shows no menu, needs no terminal, never joins (a running
# loop or TUI is refused), and never offers an install. Its caller is
# infra/dev/agent-pane.sh, for `boss factory
# restart`: the conductor starting a factory that is not running at all,
# so a stall is not left waiting for David at the console.
#
# `--detached 2` and `--detached 5` (backlog 2a2d11b2, David
# 2026-11-10) start the interactive Codex and Claude B the same way, and
# for the same reason: after the pod rolled at ~01:40Z that night both
# waited for an interactive login, and seven of Claude B's packets and
# one of Codex's sat stranded behind them until David typed. Each goes
# through the menu's own definition (`start_codex`, `start_claude_b`
# below) and so through join()'s identity check, config placing and
# Claude B's login-home preparation; detached, a running session is
# refused, never joined, and the codex TUI is never started beside the
# factory loop. Its caller is `boss session start <codex|claude-b>`,
# which records what it saw and did on that agent's work-session.
# Option 1 (`dev`, the drain the boot owns), 3 and 4 stay menu-only.
#
# `--boot` (backlog b513514c, David's morning plan 2026-11-17, item 3)
# starts EVERY seat of infra/dev/seats.tsv — each executor CLI the pod
# runs supervised by factory-loop.sh — that is not running, detached,
# and prints one `seat <name>:` line for each: started, with its
# `<session> <pane pid>`, or not started and why. The boot runs it
# (boss-dev.yaml's /work/dev-factory.sh, behind the drain switch), so a
# pod roll brings back the conductor's loop and Claude B's as well as
# Codex's; on 2026-11-17 the factory stood idle 7.5 h (item 5627a4e1)
# with only Codex supervised. A seat starts exactly as `--detached`
# starts an option: through join(), so its identity, config and login
# home are the menu's, a running seat is never joined, and a seat whose
# `twin` (the interactive session on the same agent) is running is
# refused. Before it starts, the seat's CLI must be installed and say it
# is logged in — its own answer (`codex login status`, `claude auth
# status`, measured exiting 1 with no login on Claude Code 2.1.326) — so
# a seat that cannot run a turn is named in the log rather than looping
# on a refusal. It exits 0 whatever each seat did: the boot reads the
# lines, and no seat's refusal stops another's start. Option 6 is the
# list's first seat, started by the same start_seat. Review ce8970ce
# added three lines: a seat with a file under
# $HOME/.config/boss/seat-off/ is `off` (N3: a seat stopped on purpose
# otherwise comes back on every roll); `--boot --except <seat>` skips
# that seat (N2: the boot decides once whether the drain is in tmux
# `dev` or the conductor's seat); and a seat started with its identity
# unconfirmed said `started UNSIGNED` (N4).
#
# A --boot SEAT NEVER STARTS UNIDENTIFIED (backlog 4be9ef95, measured
# 2026-11-18). N4 named the unsigned start and still made it: at the
# 13:19Z boot the agents registry could not be read, codex-factory
# started with BOSS_ACTOR blank, and its loop ran 3h40m unable to
# heartbeat or open a work-session, so the conductor's orders to it went
# unread until David approved a restart by hand. A seat has no person at
# its terminal to read why it cannot write, so join() refuses to start a
# --boot seat whose BOSS_ACTOR would be blank. A registry that could not
# be READ (the API is often still starting at a roll) is read again on
# a later pass of the same boot, for up to BOSS_DEV_BOOT_IDENTITY_WAIT
# seconds (default 150, BOSS_DEV_BOOT_IDENTITY_PAUSE = 15 between
# passes: inside the 300 s dev-factory.sh gives the whole boot); a
# registry that ANSWERED without the alias is not, since no pass changes
# that answer. Either way the seat is named `not started - identity
# unconfirmed`, and the next `dev-session.sh --boot` starts it once the
# registry confirms it. The menu and --detached are unchanged: they say
# the session starts unsigned, to a person or to the verb that called.

root=${BOSS_DEV_ROOT:-/work}
timeout=${BOSS_DEV_SESSION_TIMEOUT:-10}

detached=''
if [ "${1:-}" = --detached ]; then
  case ${2:-} in
    2 | 5 | 6) detached=$2 ;;
    *)
      echo "dev-session: --detached takes only 2, 5 or 6 (the codex TUI, Claude B, the factory loop), not '${2:-}' - nothing started" >&2
      exit 64
      ;;
  esac
fi
boot_except=''
if [ "${1:-}" = --boot ]; then
  detached=boot
  # `--boot --except <seat>`: the boot's ONE decision about the drain
  # (review ce8970ce N2). When the boot left the drain in tmux `dev`,
  # the conductor's seat must not start beside it, even if the checkout
  # was fast-forwarded between the two reads (boss-dev.yaml).
  if [ "${2:-}" = --except ]; then
    boot_except=${3:-}
  fi
fi

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
if [ -z "$detached" ] && { [ -n "${TMUX:-}" ] || { [ ! -t 0 ] && [ "${BOSS_DEV_SESSION_ASSUME_TTY:-}" != 1 ]; }; }; then
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
# read below fails closed: an agent session starts unsigned, and a
# --boot seat does not start.
# shellcheck source=../lib/jq.sh
. "$here/../lib/jq.sh" 2>/dev/null || true

running() { tmux has-session -t "=$1" 2>/dev/null; }

status() { if running "$1"; then echo running; else echo 'not running'; fi; }

# The seats (infra/dev/seats.tsv, beside this file): `seat_field <seat>
# <n>` prints column n of the seat's row, and fails when no row names it.
# Plain bash: a login runs this with nothing else assumed on PATH.
seats=$here/seats.tsv
# A seat stopped on purpose stays stopped across rolls only with its
# own switch (review ce8970ce N3): a file here named after the seat,
# on the PVC beside drain-on-boot, touched and removed by hand, no
# deploy. Every other stop lasts only until the next boot.
seat_off=$HOME/.config/boss/seat-off
seat_rows() {
  local line
  local -a f
  [ -r "$seats" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    read -r -a f <<<"$line"
    [ "${#f[@]}" -gt 0 ] || continue
    case ${f[0]} in '#'*) continue ;; esac
    echo "${f[*]}"
  done <"$seats"
}
seat_field() {
  local row
  local -a f
  while IFS= read -r row; do
    read -r -a f <<<"$row"
    if [ "${f[0]}" = "$1" ]; then
      echo "${f[$2 - 1]:-}"
      return 0
    fi
  done < <(seat_rows)
  return 1
}
seat_names() {
  local row
  while IFS= read -r row; do echo "${row%% *}"; done < <(seat_rows)
}

# The interactive session a seat stands for — its identity, config and
# login home are that session's — or the name itself.
twin_of() {
  local twin
  twin=$(seat_field "$1" 6) || twin=-
  if [ "$twin" = - ]; then echo "$1"; else echo "$twin"; fi
}

# The CLI an option's session needs to START (joining needs only tmux).
cli_of() {
  case $1 in
    dev | claude-b) echo claude ;;
    codex) echo codex ;;
    gemini) echo gemini ;;
    *) seat_field "$1" 2 ;;
  esac
}

# -q FIRST on the installer's curl (backlog 2b1bf9b4): this menu runs as
# root with HOME=/work/home, which group 1500 may create a .curlrc in,
# and what that curl fetches is piped into bash as root. Only as curl's
# first argument does -q skip the curlrc; the_doors_read_no_curlrc.rs
# holds every curl under infra/dev to it.
install_hint() {
  case $1 in
    claude) echo "curl -q -fsSL https://claude.ai/install.sh | HOME=$HOME bash" ;;
    codex) echo "npm i -g --prefix $HOME/.local @openai/codex" ;;
    gemini) echo "npm i -g --prefix $HOME/.local @google/gemini-cli" ;;
  esac
}

run_install() {
  case $1 in
    claude) curl -q -fsSL https://claude.ai/install.sh | bash ;;
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
# gone in between is then created unsigned, never Claude's). Returns 0
# when the alias is confirmed or nothing was asked, 1 when the registry
# answered without it, 2 when the registry could not be read — the two
# a --boot seat tells apart, since only the second is worth another
# pass (backlog 4be9ef95).
agent_identity() {
  local alias=$1 who=$2 mode=$3 body actor='' unconfirmed=0
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
        unconfirmed=1
      fi
    else
      echo "could not read the agents registry (GET /api/agents) to confirm $alias."
      unconfirmed=2
    fi
    if [ -z "$actor" ]; then
      # Not said of a --boot seat: join() does not start it.
      [ "$detached" = boot ] ||
        echo "The session starts with BOSS_ACTOR blank: boss writes are refused naming the fix, reads are marked operator:unidentified."
      echo "Register it: an agents-registry row whose aliases hold $alias (POST /api/agents/batch?mode=insert-if-absent)."
    fi
  fi
  flags=(-e BOSS_ACTOR_FILE=/dev/null
    -e "GIT_AUTHOR_NAME=$who" -e "GIT_AUTHOR_EMAIL=$alias"
    -e "GIT_COMMITTER_NAME=$who" -e "GIT_COMMITTER_EMAIL=$alias"
    -e "BOSS_ACTOR=$actor")
  return "$unconfirmed"
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

# A SECOND CLAUDE ACCOUNT's login home (David 2026-10-17: practise
# scaling to two accounts). Prepared when missing: option 1's settings
# copied, its memory (projects), skills and plugins LINKED so both
# accounts work from one record. Never a credential: the login is the
# person's own act, `/login` on first launch, and it lives only here.
claude_b_home() {
  local a=$HOME/.claude b=$HOME/.claude-b part
  mkdir -p "$b" || return 0
  [ -e "$b/settings.json" ] || { [ -f "$a/settings.json" ] && cp "$a/settings.json" "$b/settings.json"; }
  for part in projects skills plugins; do
    [ -e "$b/$part" ] || [ -L "$b/$part" ] || /usr/bin/ln -s "$a/$part" "$b/$part"
  done
}

# Join or create session $1 running the rest of argv. Returns only when
# its CLI is missing and was not installed.
join() {
  local name=$1 cli mode=check identity=0 flag
  shift
  local -a flags=() cmd=("$@")
  if running "$name"; then
    if [ -n "$detached" ]; then
      echo "dev-session: tmux \`$name\` is already running - --detached never joins, nothing started"
      return 1
    fi
    mode=unsigned
  else
    cli=$(cli_of "$name")
    if [ -n "$cli" ] && ! command -v "$cli" >/dev/null 2>&1; then
      if [ -n "$detached" ]; then
        echo "dev-session: $cli is not installed on this pod - nothing started. Install it with: $(install_hint "$cli")"
        return 1
      fi
      offer_install "$cli" || return 1
    fi
  fi
  case $(twin_of "$name") in
    codex)
      agent_identity codex@algedonic.dev 'Codex (engineering)' "$mode" || identity=$?
      [ "$mode" = check ] && place_config infra/dev/cli/codex-config.toml .codex/config.toml
      ;;
    claude-b)
      agent_identity claude-b@algedonic.dev 'Claude (engineering, account B)' "$mode" || identity=$?
      [ "$mode" = check ] && claude_b_home
      flags+=(-e "CLAUDE_CONFIG_DIR=$HOME/.claude-b")
      ;;
    gemini)
      agent_identity gemini@algedonic.dev 'Gemini (engineering)' "$mode" || identity=$?
      [ "$mode" = check ] && place_config infra/dev/cli/gemini-settings.json .gemini/settings.json
      ;;
  esac
  # A --boot seat never starts with BOSS_ACTOR blank (backlog 4be9ef95):
  # read off the flag it would start with, so no path into tmux below
  # can carry one. 75 (try again) when the registry could not be read,
  # 77 when it answered without the alias; boot_seat() reads which.
  if [ "$detached" = boot ]; then
    for flag in "${flags[@]}"; do
      if [ "$flag" = BOSS_ACTOR= ]; then
        [ "$identity" -eq 2 ] && return 75
        return 77
      fi
    done
  fi
  if [ -n "$detached" ]; then
    exec tmux new-session -d -P -F '#{session_name} #{pane_pid}' -s "$name" -c "$root/boss" "${flags[@]}" "${cmd[@]}"
  fi
  exec tmux new-session -A -s "$name" -c "$root/boss" "${flags[@]}" "${cmd[@]}"
}

# An interactive session and its seat sign as ONE agent — the codex TUI
# and the factory loop (backlog 628dc540), Claude B and its loop
# (b513514c) — so neither is CREATED while the other runs: two CLIs on
# one identity race for one queue. Joining a running one is unchanged.
# Returns 1, back to the menu, when the other holds the identity.
join_twin() {
  local name=$1 other=$2
  shift 2
  if ! running "$name" && running "$other"; then
    echo "tmux \`$other\` is running and signs as the same agent - quit it before starting \`$name\`."
    # A seat comes back on every roll (review ce8970ce N3): name the
    # switch that keeps it off.
    if seat_field "$other" 1 >/dev/null; then
      echo "\`$other\` is a supervised seat: stop it (tmux kill-session -t $other) and touch $seat_off/$other so the next pod roll does not start it again."
    fi
    return 1
  fi
  join "$name" "$@"
}

# A seat of seats.tsv: its loop, never beside its twin. The list's first
# seat is the loop's default, so its command is the bare path option 6
# has always had (agent-pane.sh proves a factory by it); every other
# seat names itself.
start_seat() {
  local seat=$1 twin first
  twin=$(seat_field "$seat" 6) || {
    echo "dev-session: no seat '$seat' in $seats - nothing started"
    return 1
  }
  first=$(seat_names)
  first=${first%%$'\n'*}
  local -a cmd=("$root/boss/infra/dev/factory-loop.sh")
  [ "$seat" = "$first" ] || cmd+=("$seat")
  if [ "$twin" = - ]; then
    join "$seat" "${cmd[@]}"
  else
    join_twin "$seat" "$twin" "${cmd[@]}"
  fi
}

# Option 6, the factory loop: defined ONCE, read by the menu and by
# `--detached 6` (backlog ccd985ef), so the conductor's restart and
# David's login start the same thing — and by `--boot`, as the seat it
# is (b513514c).
start_factory() {
  start_seat codex-factory
}

# Options 2 and 5, likewise defined once for the menu and `--detached`
# (backlog 2a2d11b2). Claude B's interactive session is refused beside
# its seat, as the codex TUI is beside the factory: one identity, one
# queue.
start_codex() {
  join_twin codex codex-factory codex
}

start_claude_b() {
  join_twin claude-b claude-b-factory claude
}

# Whether a CLI says it is logged in, in the seat's own login home.
logged_in() {
  local seat=$1 cli=$2
  case $cli in
    codex) timeout -k 5 30 codex login status >/dev/null 2>&1 ;;
    claude)
      if [ "$(twin_of "$seat")" = claude-b ]; then
        CLAUDE_CONFIG_DIR=$HOME/.claude-b timeout -k 5 30 claude auth status >/dev/null 2>&1
      else
        timeout -k 5 30 claude auth status >/dev/null 2>&1
      fi
      ;;
    *) return 1 ;;
  esac
}

# --boot's bound on waiting for the agents registry (backlog 4be9ef95):
# whole seconds from the first pass, and the pause between passes.
boot_identity_wait=${BOSS_DEV_BOOT_IDENTITY_WAIT:-150}
case $boot_identity_wait in '' | *[!0-9]*) boot_identity_wait=150 ;; esac
boot_identity_pause=${BOSS_DEV_BOOT_IDENTITY_PAUSE:-15}
case $boot_identity_pause in '' | *[!0-9.]*) boot_identity_pause=15 ;; esac

# --boot: one seat, in a subshell (a start execs tmux), one line for
# its outcome. Returns 75 when the seat waits for another pass: the
# registry could not be read and `$3` (the last pass) is empty. Said
# once, on the first pass; a later pass speaks only when it decides.
boot_seat() {
  local seat=$1 pass=$2 last=$3 cli out rc
  cli=$(cli_of "$seat")
  if [ "$seat" = "$boot_except" ]; then
    echo "seat $seat: not started - the boot left the drain in tmux dev (--except $seat), and two drains on one agent race for one queue"
    return 0
  fi
  if [ -e "$seat_off/$seat" ]; then
    echo "seat $seat: off - $seat_off/$seat exists; remove it and the next boot starts this seat"
    return 0
  fi
  if running "$seat"; then
    echo "seat $seat: already running - nothing started"
    return 0
  fi
  if ! command -v "$cli" >/dev/null 2>&1; then
    echo "seat $seat: not started - $cli is not installed on this pod. Install it with: $(install_hint "$cli")"
    return 0
  fi
  if ! logged_in "$seat" "$cli"; then
    echo "seat $seat: not started - $cli is not logged in for this seat; log in once in its interactive session ($(twin_of "$seat")) and the next boot starts it"
    return 0
  fi
  out=$( (start_seat "$seat") </dev/null 2>&1)
  rc=$?
  case $rc in
    0)
      if [ "$pass" -gt 1 ]; then
        echo "seat $seat: started ${out:-(tmux printed no pane)} (pass $pass: the agents registry answered)"
      else
        echo "seat $seat: started ${out:-(tmux printed no pane)}"
      fi
      ;;
    75)
      if [ -n "$last" ]; then
        echo "seat $seat: not started - identity unconfirmed (a seat never starts with an empty BOSS_ACTOR): ${out//$'\n'/ | } The registry did not answer in ${boot_identity_wait}s of passes; the next dev-session.sh --boot starts this seat once it does."
        return 0
      fi
      [ "$pass" -gt 1 ] ||
        echo "seat $seat: waiting - identity unconfirmed: ${out//$'\n'/ | } The boot reads the registry again for up to ${boot_identity_wait}s before it names this seat not started."
      return 75
      ;;
    77)
      echo "seat $seat: not started - identity unconfirmed (a seat never starts with an empty BOSS_ACTOR): ${out//$'\n'/ | } The next dev-session.sh --boot starts this seat once the registry holds its alias."
      ;;
    *) echo "seat $seat: not started (exit $rc): $out" ;;
  esac
  return 0
}

# --boot: every seat, then again for each seat that waits on the
# registry, until none waits or the bound is spent; the pass that starts
# at or past the bound is the last, and names what it could not start.
boot_seats() {
  local seat pass=1 last deadline
  local -a todo again
  if ! [ -r "$seats" ]; then
    echo "seats: no seat list at $seats - nothing started"
    return 0
  fi
  mapfile -t todo < <(seat_names)
  deadline=$((SECONDS + boot_identity_wait))
  while [ "${#todo[@]}" -gt 0 ]; do
    last=''
    [ "$SECONDS" -lt "$deadline" ] || last=1
    again=()
    for seat in "${todo[@]}"; do
      boot_seat "$seat" "$pass" "$last" || again+=("$seat")
    done
    [ "${#again[@]}" -gt 0 ] || break
    sleep "$boot_identity_pause"
    todo=("${again[@]}")
    pass=$((pass + 1))
  done
  return 0
}

# Each start execs tmux on success, so a return is a refusal.
case $detached in
  2) start_codex ;;
  5) start_claude_b ;;
  6) start_factory ;;
  boot)
    boot_seats
    exit 0
    ;;
esac
[ -z "$detached" ] || exit 1

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
  printf '  5) %-22s %-8s %s\n' 'Claude B + Claude Code' claude-b "$(status claude-b)"
  printf '  6) %-22s %-8s %s\n' 'GPT + Codex, factory' codex-factory "$(status codex-factory)"
  printf '> '
  choice=''
  if ! read -r -t "$timeout" choice && [ -z "$choice" ]; then
    printf '\nno answer - 1 (Claude + Claude Code)\n'
  fi
  case $choice in
    '' | 1) join_dev ;;
    2) start_codex ;;
    3) join gemini gemini ;;
    4) join shell bash -l ;;
    5) start_claude_b ;;
    6) start_factory ;;
    *) echo "no option '$choice'" ;;
  esac
done
