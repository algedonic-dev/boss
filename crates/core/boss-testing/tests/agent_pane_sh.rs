//! `infra/dev/agent-pane.sh` — the one place the dev pod reads another
//! agent's tmux pane, pastes a labelled peer message into it, or starts
//! the supervised factory loop (backlog ccd985ef, decided 2026-10-29).
//!
//! THE CASE. On 2026-10-29 the interactive Codex factory (dev-session
//! option 2, tmux `codex`) ended its turn at about 04:01Z and sat idle at
//! its prompt for about ten hours. Silent alarm 7e947dbf fired at 06:10Z
//! and nothing could act on it: the restart loop covers only option 6,
//! and the conductor had no door — peer messages were tmux commands typed
//! by hand. `boss factory restart` and `boss peer send` both go through
//! this helper, so the read, the paste and the launch are defined once.
//!
//! Pinned against a stub `tmux` whose sessions live in a file (so a
//! launch adds one), whose panes are fixture files read in order (the
//! capture before, then the capture after), and which logs every call.
//! The pane shapes are the ones measured on the live `codex` pane on
//! 2026-10-29: the `› Ask Codex to do anything` input box is drawn at the
//! bottom whether or not the session is working, so the box alone is
//! never read as idle; a `Working (… esc to interrupt)` line above it is
//! busy; a box holding a draft is not empty; a pane drawing no box (a
//! dialog whose Enter confirms something) is never typed into.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/dev/agent-pane.sh";

const TMUX_STUB: &str = r#"#!/usr/bin/env bash
echo "tmux $*" >> "$STUB_LOG"
[ "${STUB_NO_SERVER:-}" = 1 ] && { echo "no server running on /tmp/tmux-0/default" >&2; exit 1; }
sessions=$STUB_DIR/sessions
case "$1" in
  list-sessions)
    while read -r s; do [ -n "$s" ] && echo "$s: 1 windows"; done < "$sessions"
    ;;
  has-session)
    t=${3#=}
    grep -qx -- "$t" "$sessions" && exit 0
    exit 1
    ;;
  list-panes)
    case "$*" in
      *pane_start_command*)
        echo "${STUB_PANE_DEAD:-0} ${STUB_START-$BOSS_DEV_ROOT/boss/infra/dev/factory-loop.sh}"
        ;;
      *) echo 4242 ;;
    esac
    ;;
  show-environment)
    if [ -n "${STUB_BOSS_ACTOR+x}" ]; then
      echo "BOSS_ACTOR=$STUB_BOSS_ACTOR"
    else
      echo "unknown variable: BOSS_ACTOR" >&2
      exit 1
    fi
    ;;
  capture-pane)
    [ "${STUB_PANE_FAILS:-}" = 1 ] && { echo "can't find pane" >&2; exit 1; }
    date +%s%N >> "$STUB_DIR/capture-times"
    n=$(( $(cat "$STUB_DIR/captures" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$STUB_DIR/captures"
    f=$STUB_DIR/pane.$n
    [ -f "$f" ] || f=$STUB_DIR/pane.1
    cat "$f"
    ;;
  display-message)
    case "$*" in
      *pane_width*) echo "${STUB_WIDTH-120}" ;;
      *pane_in_mode*)
        # The mode reads in order: mode.<n> when the fixture gave one,
        # else STUB_MODE, else a pane taking input.
        m=$(( $(cat "$STUB_DIR/modes" 2>/dev/null || echo 0) + 1 ))
        echo "$m" > "$STUB_DIR/modes"
        if [ -f "$STUB_DIR/mode.$m" ]; then cat "$STUB_DIR/mode.$m"; else echo "${STUB_MODE-0 0}"; fi
        ;;
    esac
    ;;
  load-buffer)
    cp -- "${@: -1}" "$STUB_DIR/buffer"
    ;;
  new-session)
    echo "codex-factory" >> "$sessions"
    echo "codex-factory 5151"
    ;;
esac
exit 0
"#;

/// The pod's `boss-api` door, as dev-session.sh reads the agents
/// registry through it: the body on stdout, `HTTP:<code>` on stderr.
const BOSS_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $*" >> "$STUB_LOG"
[ "$1 $2" = "GET /api/agents" ] || exit 9
printf '%s\n' '{"data":[{"aliases":["codex@algedonic.dev"],"id":"agent-codex"}],"total":1}'
echo "HTTP:200" >&2
"#;

/// The live `codex` pane at 2026-10-29 14:19Z, idle: its last turn
/// ended ("Worked for …"), and the input box holds only its placeholder.
const PANE_IDLE: &str = "\
• Build routing is blocked: 9e22f485 remains at triage.

  Recorded the blocker on both sessions.

  Worked for 1m 27s • 2:18 PM


› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss · Orient BOSS build handoff · Main [default]
  ⏎ for agents · ? for shortcuts
";

/// The same pane mid-turn: the box is still drawn, a Working line sits
/// above it.
const PANE_WORKING: &str = "\
• Ran git -C /work/boss status
  └ On branch main

• Working (12s • esc to interrupt)

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss · Main [default]
  ⏎ for agents · ? for shortcuts
";

/// Idle, but someone has typed into the box: a paste would append to
/// their draft.
const PANE_DRAFT: &str = "\
  Worked for 3m 02s • 9:40 AM

› ping me when it is ready for review

  GPT-6.1-Sol default · /work/boss · Main [default]
";

/// A dialog whose Enter confirms a choice (measured on the `shell`
/// session's Claude pane, 2026-10-29): no input box at all.
const PANE_DIALOG: &str = "\
   What do you want to do?

   ❯ 1. Stop and wait for limit to reset
     2. Wait here, then continue automatically shortly
     3. Switch to usage credits

   Enter to confirm · Esc to cancel
";

/// Review b6467d24's fixture A (B1, 2026-10-29): Codex swaps the default
/// "Working" header for one taken from the reasoning summary, and a
/// window left narrow by the last client cuts the hint off after `esc`.
/// Neither `Working (` nor `esc to interrupt` is drawn; the turn's
/// elapsed counter is.
const PANE_REASONING_CUT: &str = "\
• Ran cargo test -p boss-cli factory::
  └ running 11 tests

• Investigating the silent factory alarm on the gate (1m 05s • esc

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";

/// Review b6467d24's fixture B: queued messages push the Working line
/// more than eight lines above the composer.
const PANE_QUEUED: &str = "\
• Working (2m 10s • esc to interrupt)

  ↳ when this finishes, re-gate the car on the new head and park it with
    the probe carried over from the first gate, then
    tell me which train it boards
  ↳ also re-read the backlog item before you close it, the scope moved
    after the review, and the excludes line no longer
    says what the car leaves out
  ↳ and file the follow-up for the temp dir
    please
  ⌥ + ↑ edit

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";

/// The idle pane a second later with one cell redrawn: whatever moved,
/// a pane that does not hold still is not proven idle.
const PANE_IDLE_REDRAWN: &str = "\
• Build routing is blocked: 9e22f485 remains at triage.

  Recorded the blocker on both sessions.

  Worked for 1m 28s • 2:18 PM


› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss · Orient BOSS build handoff · Main [default]
  ⏎ for agents · ? for shortcuts
";

/// Review 1924c625's fixture A2 (B4, 2026-10-29): a reasoning-summary
/// header that fills the width is cut with an ellipsis BEFORE its
/// elapsed counter, so nothing in the plain capture ticks — the header
/// shimmer is colour only — and two captures 2 s apart are identical.
const PANE_HEADER_ELLIPSIS: &str = "\
• Ran cargo test -p boss-cli factory::
  └ running 11 tests

• Investigating the silent factory alarm on the gate runner…

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";

/// A2 with the ellipsis spelled as three dots.
const PANE_HEADER_DOTS: &str = "\
• Ran cargo test -p boss-cli factory::

• Investigating the silent factory alarm on the gate runner...

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";

/// Review 1924c625's fixture A3: the same header cut at the space before
/// its `(` — no ellipsis, no counter. Codex cuts there only in a pane
/// about sixty columns wide, so it is answered by the width floor.
const PANE_HEADER_CUT_AT_PAREN: &str = "\
• Ran cargo test -p boss-cli factory::

• Investigating the silent factory alarm on the gate runner

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";

/// Review 1924c625's fixture D: an approval dialog drawn indented, under
/// a history line that starts at column 0 with the prompt glyph.
const PANE_APPROVAL: &str = "\
› run the gate for the car

• I will run the gate.

  Would you like to run the following command?

  $ boss gate feat/x --wait

  › 1. Yes, proceed (y)
    2. Yes, and don't ask again for this command (p)
    3. No, and tell Codex what to do differently (esc)

  Press enter to confirm or esc to cancel
";

/// Review 1924c625's fixture K: a dialog with no composer, its choice
/// drawn at column 0 with the prompt glyph.
const PANE_SELECT: &str = "\
› earlier user message

  Would you like to make the following edits?

› 1. Yes, proceed (y)
  2. No (esc)

  Press enter to confirm or esc to cancel
";

/// What the pane shows after a wake: the message taken, a turn begun.
const PANE_AFTER: &str = "\
› [BOSS peer message]

• Working (1s • esc to interrupt)

› Ask Codex to do anything
";

struct Fixture {
    root: PathBuf,
    stubs: PathBuf,
}

impl Fixture {
    /// `sessions` running; `panes` are the captures, in order.
    fn new(name: &str, sessions: &[&str], panes: &[&str]) -> Self {
        let root = scratch_dir(&format!("agent-pane-{name}"));
        let stubs = root.join("stubs");
        create_dir(&stubs);
        write_exec(&stubs.join("tmux"), TMUX_STUB);
        write_exec(&stubs.join("boss-api"), BOSS_API_STUB);
        let mut list = sessions.join("\n");
        list.push('\n');
        std::fs::write(root.join("sessions"), list).expect("sessions");
        for (i, pane) in panes.iter().enumerate() {
            std::fs::write(root.join(format!("pane.{}", i + 1)), pane).expect("pane");
        }
        // The fixture `/work` dev-session.sh launches from: the codex CLI
        // on the PVC home's PATH, and the checkout the loop runs from.
        create_dir(&root.join("work/boss"));
        create_dir(&root.join("work/home/.local/bin"));
        write_exec(
            &root.join("work/home/.local/bin/codex"),
            "#!/usr/bin/env bash\n",
        );
        std::fs::write(root.join("message.txt"), "Read your orders.\n").expect("message");
        create_dir(&root.join("record"));
        Self { root, stubs }
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.stubs.display()))
            .env("HOME", self.root.join("work/home"))
            .env("BOSS_DEV_ROOT", self.root.join("work"))
            .env("BOSS_DOOR_FRESHNESS", "off")
            .env("AGENT_PANE_SETTLE", "0")
            .env("STUB_LOG", self.root.join("stub.log"))
            .env("STUB_DIR", &self.root);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run agent-pane.sh");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn factory(&self, env: &[(&str, &str)]) -> (i32, String, String) {
        self.run(
            &[
                "factory",
                "agent-codex",
                "claude@algedonic.dev",
                &self.message(),
                &self.record(),
            ],
            env,
        )
    }

    fn send(&self, actor: &str) -> (i32, String, String) {
        self.run(
            &[
                "send",
                actor,
                "claude@algedonic.dev",
                &self.message(),
                &self.record(),
            ],
            &[],
        )
    }

    fn message(&self) -> String {
        self.root.join("message.txt").display().to_string()
    }

    fn record(&self) -> String {
        self.root.join("record").display().to_string()
    }

    /// Replace the message with these exact bytes.
    fn with_message(self, bytes: &[u8]) -> Self {
        std::fs::write(self.root.join("message.txt"), bytes).expect("message");
        self
    }

    /// What tmux reports for `#{pane_in_mode} #{pane_input_off}`, read
    /// by read: the first answer, the second, and so on.
    fn with_modes(self, modes: &[&str]) -> Self {
        for (i, mode) in modes.iter().enumerate() {
            std::fs::write(
                self.root.join(format!("mode.{}", i + 1)),
                format!("{mode}\n"),
            )
            .expect("mode");
        }
        self
    }

    /// The stub's clock at each capture, in nanoseconds.
    fn capture_times(&self) -> Vec<u128> {
        std::fs::read_to_string(self.root.join("capture-times"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().parse().expect("a capture time"))
            .collect()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("stub.log")).unwrap_or_default()
    }

    fn recorded(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join("record").join(file)).ok()
    }

    /// Every tmux call that types, pastes or starts something.
    fn acts(&self) -> Vec<String> {
        self.log()
            .lines()
            .filter(|l| {
                ["load-buffer", "paste-buffer", "send-keys", "new-session"]
                    .iter()
                    .any(|a| l.starts_with(&format!("tmux {a}")))
            })
            .map(str::to_string)
            .collect()
    }

    fn never_killed(&self) {
        let log = self.log();
        for kill in [
            "kill-session",
            "kill-server",
            "kill-pane",
            "kill-window",
            "C-c",
        ] {
            assert!(
                !log.contains(kill),
                "nothing is ever killed ({kill}): {log}"
            );
        }
    }
}

/// `outcome=…` and the other `key=value` lines the helper prints.
fn field(out: &str, key: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

#[test]
fn the_helper_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(repo_root().join(SCRIPT)).expect(SCRIPT);
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} is executable"
    );
}

/// (a) The supervised loop runs: nothing is typed and nothing started,
/// even when an interactive session runs beside it.
#[test]
fn a_running_supervised_loop_is_left_alone() {
    for sessions in [vec!["codex-factory"], vec!["codex-factory", "codex"]] {
        let f = Fixture::new(
            &format!("supervised-{}", sessions.len()),
            &sessions,
            &[PANE_IDLE],
        );
        let (rc, out, err) = f.factory(&[]);
        assert_eq!(rc, 0, "{out}{err}");
        assert_eq!(
            field(&out, "outcome").as_deref(),
            Some("already_supervised")
        );
        assert_eq!(field(&out, "session").as_deref(), Some("codex-factory"));
        assert_eq!(field(&out, "pid").as_deref(), Some("4242"));
        assert!(f.acts().is_empty(), "{}", f.log());
        f.never_killed();
    }
}

/// (b) idle: the interactive pane at its empty prompt with no Working
/// line is woken by the peer sequence — load-buffer, paste-buffer,
/// Enter — with a capture before and after, and the labelled message is
/// what was pasted.
#[test]
fn an_idle_interactive_codex_is_woken_with_a_labelled_paste() {
    let f = Fixture::new("idle", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"));
    assert_eq!(field(&out, "session").as_deref(), Some("codex"));
    assert_eq!(field(&out, "pid").as_deref(), Some("4242"));
    let acts = f.acts();
    assert_eq!(acts.len(), 3, "{acts:?}");
    assert!(acts[0].starts_with("tmux load-buffer -b "), "{acts:?}");
    assert!(acts[1].starts_with("tmux paste-buffer "), "{acts:?}");
    assert!(acts[1].ends_with(" -t =codex:"), "{acts:?}");
    assert_eq!(acts[2], "tmux send-keys -t =codex: Enter", "{acts:?}");
    let log = f.log();
    let first_capture = log.find("tmux capture-pane").expect("a capture before");
    let last_capture = log.rfind("tmux capture-pane").expect("a capture after");
    assert!(
        first_capture < log.find("tmux load-buffer").unwrap(),
        "{log}"
    );
    assert!(last_capture > log.find("tmux send-keys").unwrap(), "{log}");
    assert_eq!(f.recorded("before.txt").as_deref(), Some(PANE_IDLE));
    assert_eq!(f.recorded("after.txt").as_deref(), Some(PANE_AFTER));
    let pasted = std::fs::read_to_string(f.root.join("buffer")).expect("the buffer");
    assert!(
        pasted.starts_with("[BOSS peer message from claude@algedonic.dev"),
        "labelled: {pasted}"
    );
    assert!(pasted.contains("boss factory restart"), "{pasted}");
    assert!(pasted.contains("Read your orders."), "{pasted}");
    assert_eq!(f.recorded("pasted.txt").as_deref(), Some(pasted.as_str()));
    f.never_killed();
}

/// (b) busy: a Working line above the box, a draft in the box, or no
/// box at all (a dialog) — nothing is typed, and the reason is said.
#[test]
fn a_busy_or_occupied_pane_is_never_typed_into() {
    for (name, pane, why) in [
        ("working", PANE_WORKING, "Working"),
        ("draft", PANE_DRAFT, "draft"),
        ("dialog", PANE_DIALOG, "no input box"),
    ] {
        let f = Fixture::new(&format!("busy-{name}"), &["codex"], &[pane]);
        let (rc, out, err) = f.factory(&[]);
        assert_eq!(rc, 0, "{name}: {out}{err}");
        assert_eq!(field(&out, "outcome").as_deref(), Some("busy"), "{name}");
        let reason = field(&out, "reason").unwrap_or_default();
        assert!(reason.contains(why), "{name}: {reason}");
        assert!(f.acts().is_empty(), "{name}: {}", f.log());
        assert_eq!(f.recorded("before.txt").as_deref(), Some(pane), "{name}");
        f.never_killed();
    }
}

/// B1 (review b6467d24): idle is PROVEN, never inferred from two strings
/// being absent. Its fixtures A and B were read idle and typed into; a
/// running turn's Enter submits at once and interrupts the model
/// (Codex 0.159.3: "Model interrupted to submit steer instructions").
/// A busy marker anywhere above the prompt reads busy: the turn's
/// elapsed counter in a header the hint was cut off from, and a Working
/// line queued messages pushed far above the box.
#[test]
fn a_running_turn_with_no_working_line_near_the_box_reads_busy() {
    for (name, pane) in [
        ("reasoning-cut", PANE_REASONING_CUT),
        ("queued", PANE_QUEUED),
    ] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(&format!("b1-{name}-{verb}"), &["codex"], &[pane]);
            let (rc, out, err) = if verb == "factory" {
                f.factory(&[])
            } else {
                f.send("agent-codex")
            };
            let want_rc = if verb == "factory" { 0 } else { 75 };
            assert_eq!(rc, want_rc, "{name} {verb}: {out}{err}");
            assert_eq!(
                field(&out, "outcome").as_deref(),
                Some("busy"),
                "{name} {verb}: {out}"
            );
            assert!(f.acts().is_empty(), "{name} {verb}: {}", f.log());
            f.never_killed();
        }
    }
}

/// B1: an idle-looking pane is captured TWICE, at least 1.5 s apart (a
/// running turn's elapsed counter ticks every second), and only two
/// byte-identical captures are idle. Both come before anything is typed.
#[test]
fn idle_is_two_identical_captures_at_least_a_second_and_a_half_apart() {
    let f = Fixture::new("b1-twice", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"), "{out}");
    let times = f.capture_times();
    assert_eq!(times.len(), 3, "before, again, after: {times:?}");
    assert!(
        times[1] - times[0] >= 1_500_000_000,
        "the two captures before the paste are at least 1.5 s apart: {times:?}"
    );
    let log = f.log();
    let second = log
        .match_indices("tmux capture-pane")
        .nth(1)
        .map(|(i, _)| i)
        .expect("a second capture");
    assert!(second < log.find("tmux load-buffer").unwrap(), "{log}");
    assert_eq!(f.recorded("recheck.txt").as_deref(), Some(PANE_IDLE));

    let f = Fixture::new("b1-moved", &["codex"], &[PANE_IDLE, PANE_IDLE_REDRAWN]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("busy"), "{out}");
    let reason = field(&out, "reason").unwrap_or_default();
    assert!(reason.contains("changed"), "{reason}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

/// B2 (review b6467d24): the message reaches the pane byte for byte, and
/// tmux 3.3a filters nothing — measured on a private server, an ESC[201~
/// in the buffer ended the bracketed paste and the ^C, Esc and CR after
/// it arrived as keystrokes. So any byte other than a printable
/// character, a newline or a tab is refused (66), named, BEFORE the pane
/// is even read — whatever the message's source: --why, a peer-send
/// file, or an alarm title that reached the wake.
#[test]
fn a_message_holding_a_control_byte_is_refused_before_anything_is_typed() {
    let cases: [(&str, &[u8]); 8] = [
        (
            "paste-end",
            b"Read your orders.\x1b[201~\x03\x1b do this instead\r\n",
        ),
        ("esc", b"Read\x1b your orders.\n"),
        ("etx", b"Read\x03 your orders.\n"),
        ("cr", b"Read your orders.\r\n"),
        ("nul", b"Read\x00 your orders.\n"),
        ("del", b"Read\x7f your orders.\n"),
        ("c1-csi", "Read\u{9b}201~ your orders.\n".as_bytes()),
        ("not-utf8", b"Read \xff your orders.\n"),
    ];
    for (name, bytes) in cases {
        for verb in ["factory", "send"] {
            let f = Fixture::new(&format!("b2-{name}-{verb}"), &["codex"], &[PANE_IDLE])
                .with_message(bytes);
            let (rc, out, err) = if verb == "factory" {
                f.factory(&[])
            } else {
                f.send("agent-codex")
            };
            assert_eq!(rc, 66, "{name} {verb}: {out}{err}");
            assert!(err.contains("REFUSED"), "{name} {verb}: {err}");
            assert!(err.contains("printable"), "{name} {verb}: {err}");
            assert!(field(&out, "outcome").is_none(), "{name} {verb}: {out}");
            assert!(
                !f.log().contains("tmux capture-pane"),
                "refused before the pane is read: {}",
                f.log()
            );
            assert!(f.acts().is_empty(), "{name} {verb}: {}", f.log());
            assert!(!f.root.join("buffer").exists(), "{name} {verb}");
        }
    }

    // A tab, a newline and non-ASCII text are printable: the control.
    let f = Fixture::new("b2-plain", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER])
        .with_message("Read your orders\t— alarm 7e947dbf (SILENT: agent-codex).\n".as_bytes());
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"), "{out}");
}

/// B2: the label is typed too, so a sender holding a control byte is
/// refused the same way.
#[test]
fn a_sender_holding_a_control_byte_is_refused() {
    let f = Fixture::new("b2-from", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (rc, out, err) = f.run(
        &[
            "send",
            "agent-codex",
            "claude\x1b[201~\x03",
            &f.message(),
            &f.record(),
        ],
        &[],
    );
    assert_eq!(rc, 66, "{out}{err}");
    assert!(err.contains("printable"), "{err}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

/// N4 (review b6467d24): `already_supervised` is a session named
/// `codex-factory` whose pane RUNS the loop — the command option 6
/// starts — never the name alone. Anything else holding that name is
/// refused by name, and nothing is typed or started.
#[test]
fn a_session_named_like_the_loop_that_does_not_run_it_is_refused() {
    for (name, start, dead) in [("other-command", "bash -l", "0"), ("dead-pane", "", "1")] {
        let f = Fixture::new(&format!("n4-{name}"), &["codex-factory"], &[PANE_IDLE]);
        let loop_cmd = format!("{}/work/boss/infra/dev/factory-loop.sh", f.root.display());
        let start = if start.is_empty() {
            loop_cmd.as_str()
        } else {
            start
        };
        let (rc, out, err) = f.factory(&[("STUB_START", start), ("STUB_PANE_DEAD", dead)]);
        assert_eq!(rc, 69, "{name}: {out}{err}");
        assert!(err.contains("factory-loop.sh"), "{name}: {err}");
        assert!(field(&out, "outcome").is_none(), "{name}: {out}");
        assert!(f.acts().is_empty(), "{name}: {}", f.log());
    }

    // tmux may quote the command it was given: still the loop.
    let f = Fixture::new("n4-quoted", &["codex-factory"], &[PANE_IDLE]);
    let quoted = format!(
        "\"{}/work/boss/infra/dev/factory-loop.sh\"",
        f.root.display()
    );
    let (rc, out, err) = f.factory(&[("STUB_START", quoted.as_str())]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(
        field(&out, "outcome").as_deref(),
        Some("already_supervised")
    );
}

/// N6 (review b6467d24): every outcome names the identity the target
/// session signs as, read from tmux's session environment (read-only),
/// so a factory that signs as nobody is on the record as such.
#[test]
fn every_outcome_names_the_sessions_boss_actor() {
    let f = Fixture::new("n6-supervised", &["codex-factory"], &[PANE_IDLE]);
    let (_, out, _) = f.factory(&[("STUB_BOSS_ACTOR", "codex@algedonic.dev")]);
    assert_eq!(
        field(&out, "boss_actor").as_deref(),
        Some("codex@algedonic.dev"),
        "{out}"
    );

    let f = Fixture::new("n6-woken", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (_, out, _) = f.factory(&[("STUB_BOSS_ACTOR", "")]);
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"), "{out}");
    assert_eq!(
        field(&out, "boss_actor").as_deref(),
        Some("(blank)"),
        "{out}"
    );

    let f = Fixture::new("n6-busy", &["codex"], &[PANE_WORKING]);
    let (_, out, _) = f.factory(&[]);
    assert_eq!(field(&out, "outcome").as_deref(), Some("busy"), "{out}");
    assert_eq!(
        field(&out, "boss_actor").as_deref(),
        Some("(unset)"),
        "{out}"
    );

    let f = Fixture::new("n6-started", &["dev", "shell"], &[]);
    let (_, out, _) = f.factory(&[("STUB_BOSS_ACTOR", "codex@algedonic.dev")]);
    assert_eq!(field(&out, "outcome").as_deref(), Some("started"), "{out}");
    assert_eq!(
        field(&out, "boss_actor").as_deref(),
        Some("codex@algedonic.dev"),
        "{out}"
    );
}

/// (c) No factory at all: the loop is started in a NEW session through
/// dev-session.sh's own option 6 — detached, signing as codex, running
/// infra/dev/factory-loop.sh — and the session and its pid are said.
#[test]
fn no_factory_starts_the_loop_through_option_six() {
    let f = Fixture::new("none", &["dev", "shell"], &[]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("started"));
    assert_eq!(field(&out, "session").as_deref(), Some("codex-factory"));
    assert_eq!(field(&out, "pid").as_deref(), Some("5151"));
    let acts = f.acts();
    assert_eq!(acts.len(), 1, "{acts:?}");
    let work = f.root.join("work");
    assert!(
        acts[0]
            .starts_with("tmux new-session -d -P -F #{session_name} #{pane_pid} -s codex-factory "),
        "{acts:?}"
    );
    assert!(
        acts[0].ends_with(&format!(
            " {}/boss/infra/dev/factory-loop.sh",
            work.display()
        )),
        "the command option 6 runs: {acts:?}"
    );
    assert!(
        acts[0].contains("-e BOSS_ACTOR=codex@algedonic.dev"),
        "it signs as codex, never as the pod's Claude: {acts:?}"
    );
    f.never_killed();
}

/// The refusals are named, and each does nothing.
#[test]
fn an_unknown_actor_is_refused_by_name() {
    let f = Fixture::new("unknown", &["codex"], &[PANE_IDLE]);
    let (rc, out, err) = f.run(
        &[
            "factory",
            "agent-nobody",
            "claude@algedonic.dev",
            &f.message(),
            &f.record(),
        ],
        &[],
    );
    assert_eq!(rc, 64, "{out}{err}");
    assert!(err.contains("agent-nobody"), "{err}");
    assert!(field(&out, "outcome").is_none(), "{out}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

#[test]
fn a_pod_with_no_tmux_server_is_refused() {
    let f = Fixture::new("noserver", &[], &[]);
    let (rc, out, err) = f.factory(&[("STUB_NO_SERVER", "1")]);
    assert_eq!(rc, 69, "{out}{err}");
    assert!(err.contains("no tmux server"), "{err}");
    assert!(field(&out, "outcome").is_none(), "{out}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

#[test]
fn an_unreadable_pane_is_refused() {
    let f = Fixture::new("unreadable", &["codex"], &[""]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 74, "an empty capture: {out}{err}");
    assert!(err.contains("could not read"), "{err}");
    assert!(f.acts().is_empty(), "{}", f.log());

    let f = Fixture::new("unreadable-fails", &["codex"], &[PANE_IDLE]);
    let (rc, out, err) = f.factory(&[("STUB_PANE_FAILS", "1")]);
    assert_eq!(rc, 74, "a failed capture: {out}{err}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

/// `boss peer send`: the same read and paste, as its own verb. A busy
/// pane is refused (75) with nothing typed; a session that runs only the
/// supervised loop has no pane to read, and says where its orders go.
#[test]
fn peer_send_pastes_only_into_an_idle_pane() {
    let f = Fixture::new("send-idle", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (rc, out, err) = f.send("agent-codex");
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("sent"));
    assert_eq!(f.acts().len(), 3, "{}", f.log());
    let pasted = std::fs::read_to_string(f.root.join("buffer")).expect("the buffer");
    assert!(pasted.contains("boss peer send"), "{pasted}");

    let f = Fixture::new("send-busy", &["codex"], &[PANE_WORKING]);
    let (rc, out, err) = f.send("agent-codex");
    assert_eq!(rc, 75, "{out}{err}");
    assert!(err.contains("not typed"), "{err}");
    assert!(f.acts().is_empty(), "{}", f.log());

    let f = Fixture::new("send-loop", &["codex-factory"], &[PANE_IDLE]);
    let (rc, out, err) = f.send("codex@algedonic.dev");
    assert_eq!(rc, 69, "{out}{err}");
    assert!(err.contains("work-session"), "{err}");
    assert!(f.acts().is_empty(), "{}", f.log());
}

/// The helper's text names no tmux verb that ends a process, and no key
/// that interrupts one.
#[test]
fn the_helper_never_kills_or_interrupts() {
    let text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    for word in [
        "kill-",
        "C-c",
        "kill ",
        "pkill",
        "send-keys -t \"$target\" Escape",
    ] {
        assert!(!text.contains(word), "{SCRIPT} must not say {word:?}");
    }
}

/// CLAUDE.md §9a: the session names live in dev-session.sh's menu and
/// here; the helper's must be the menu's.
#[test]
fn the_helpers_session_names_are_the_menus() {
    let menu = std::fs::read_to_string(repo_root().join("infra/dev/dev-session.sh")).expect("menu");
    // Option 6 is the seat list's first seat, `codex-factory`, whose twin
    // is `codex` (backlog b513514c moved it from a menu literal to a row).
    let seats = std::fs::read_to_string(repo_root().join("infra/dev/seats.tsv")).expect("seats");
    let first = seats
        .lines()
        .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .expect("a seat row");
    assert_eq!(
        (first[0], first[1], first[5]),
        ("codex-factory", "codex", "codex"),
        "option 6 is `codex-factory`, exclusive with `codex`"
    );
    assert!(
        menu.contains("start_seat codex-factory"),
        "option 6 starts its seat"
    );
    assert!(
        menu.contains("join_twin codex codex-factory codex"),
        "option 2"
    );
    let text = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    assert!(
        text.contains("interactive=codex supervised=codex-factory"),
        "{SCRIPT}"
    );
}

/// Run `verb` on fixture `f`: the factory, or a peer send to agent-codex.
fn run_verb(f: &Fixture, verb: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    if verb == "factory" {
        f.factory(env)
    } else {
        f.run(
            &[
                "send",
                "agent-codex",
                "claude@algedonic.dev",
                &f.message(),
                &f.record(),
            ],
            env,
        )
    }
}

/// Assert `verb` read busy and typed nothing: the factory records busy
/// (0), a peer send is refused (75).
fn assert_busy(f: &Fixture, verb: &str, ran: &(i32, String, String), why: &str, name: &str) {
    let (rc, out, err) = ran;
    let want_rc = if verb == "factory" { 0 } else { 75 };
    assert_eq!(*rc, want_rc, "{name} {verb}: {out}{err}");
    assert_eq!(
        field(out, "outcome").as_deref(),
        Some("busy"),
        "{name} {verb}: {out}{err}"
    );
    let reason = field(out, "reason").unwrap_or_default();
    assert!(reason.contains(why), "{name} {verb}: {reason}");
    assert!(f.acts().is_empty(), "{name} {verb}: {}", f.log());
    assert!(!f.root.join("buffer").exists(), "{name} {verb}");
    assert!(
        f.recorded("pasted.txt").is_none(),
        "nothing pasted, so no pasted.txt for the record: {name} {verb}"
    );
    f.never_killed();
}

/// B3 (review 1924c625, measured on a private tmux 3.3a server): a pane
/// in copy-mode — what C-b [ leaves behind when a human scrolls back to
/// read the factory, and the mode outlives a detach — still captures its
/// base screen, so it read idle; paste-buffer -p then found no
/// bracketed-paste mode on the copy-mode screen and sent the text raw,
/// every CR a keystroke, and the Enter went to copy-mode. The helper
/// said woken/sent for an act that did not happen. input_off is the
/// other way a paste is silently dropped. So the pane is busy unless
/// tmux reports `#{pane_in_mode} #{pane_input_off}` as exactly `0 0`.
#[test]
fn a_pane_in_copy_mode_or_with_its_input_off_reads_busy() {
    for (name, mode) in [
        ("copy-mode-input-off", "1 1"),
        ("copy-mode", "1 0"),
        ("input-off", "0 1"),
        ("unreported", ""),
        ("garbled", "0 0 0"),
    ] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(
                &format!("b3-{name}-{verb}"),
                &["codex"],
                &[PANE_IDLE, PANE_IDLE, PANE_AFTER],
            );
            let ran = run_verb(&f, verb, &[("STUB_MODE", mode)]);
            assert_busy(&f, verb, &ran, "pane_in_mode", name);
        }
    }
}

/// B3: the mode is read with EACH capture and again immediately before
/// load-buffer — a human can enter copy-mode inside the 2 s the idle
/// proof waits — and a pane that enters it at the last read is not
/// typed into.
#[test]
fn the_mode_is_read_at_each_capture_and_again_just_before_the_paste() {
    let f = Fixture::new("b3-order", &["codex"], &[PANE_IDLE, PANE_IDLE, PANE_AFTER]);
    let (rc, out, err) = f.factory(&[]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"), "{out}");
    let log = f.log();
    let calls: Vec<&str> = log.lines().collect();
    let mode = |l: &str| {
        l.starts_with("tmux display-message") && l.contains("#{pane_in_mode} #{pane_input_off}")
    };
    let load = calls
        .iter()
        .position(|l| l.starts_with("tmux load-buffer"))
        .expect("a paste");
    assert!(
        mode(calls[load - 1]),
        "the read just before load-buffer is the mode: {log}"
    );
    let captures: Vec<usize> = calls[..load]
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("tmux capture-pane"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(captures.len(), 2, "{log}");
    for c in captures {
        assert!(
            mode(calls[c + 1]),
            "the mode is read with the capture at {c}: {log}"
        );
    }

    for (name, modes) in [
        ("at-the-recheck", vec!["0 0", "1 0"]),
        ("just-before-the-paste", vec!["0 0", "0 0", "1 0"]),
    ] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(
                &format!("b3-{name}-{verb}"),
                &["codex"],
                &[PANE_IDLE, PANE_IDLE, PANE_AFTER],
            )
            .with_modes(&modes);
            let ran = run_verb(&f, verb, &[]);
            assert_busy(&f, verb, &ran, "pane_in_mode", name);
        }
    }
}

/// B4 (review 1924c625): the idle proof rests on a running turn's counter
/// being drawn, and a reasoning-summary header that fills the width is
/// cut with an ellipsis before it. A status line — the last line drawn
/// above the prompt — that ends in an ellipsis is busy.
#[test]
fn a_status_line_cut_with_an_ellipsis_reads_busy() {
    for (name, pane) in [("a2", PANE_HEADER_ELLIPSIS), ("a2-dots", PANE_HEADER_DOTS)] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(
                &format!("b4-{name}-{verb}"),
                &["codex"],
                &[pane, pane, PANE_AFTER],
            );
            let ran = run_verb(&f, verb, &[]);
            assert_busy(&f, verb, &ran, "ellipsis", name);
        }
    }
}

/// B4: below 80 columns Codex's status line need not reach its counter
/// (fixture A3 is cut at the space before the `(` in about sixty), so a
/// pane narrower than that is refused (69), named, before it is read.
#[test]
fn a_pane_under_eighty_columns_is_refused_before_it_is_read() {
    for (name, pane, width) in [
        ("a3-60", PANE_HEADER_CUT_AT_PAREN, "60"),
        ("a2-60", PANE_HEADER_ELLIPSIS, "60"),
        ("idle-60", PANE_IDLE, "60"),
        ("idle-79", PANE_IDLE, "79"),
        ("unreported", PANE_IDLE, ""),
    ] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(
                &format!("b4-{name}-{verb}"),
                &["codex"],
                &[pane, pane, PANE_AFTER],
            );
            let (rc, out, err) = run_verb(&f, verb, &[("STUB_WIDTH", width)]);
            assert_eq!(rc, 69, "{name} {verb}: {out}{err}");
            assert!(err.contains("REFUSED"), "{name} {verb}: {err}");
            assert!(err.contains("80 columns"), "{name} {verb}: {err}");
            assert!(field(&out, "outcome").is_none(), "{name} {verb}: {out}");
            assert!(
                !f.log().contains("tmux capture-pane"),
                "refused before the pane is read: {}",
                f.log()
            );
            assert!(f.acts().is_empty(), "{name} {verb}: {}", f.log());
        }
    }

    // Eighty columns is wide enough: the control.
    let f = Fixture::new(
        "b4-idle-80",
        &["codex"],
        &[PANE_IDLE, PANE_IDLE, PANE_AFTER],
    );
    let (rc, out, err) = f.factory(&[("STUB_WIDTH", "80")]);
    assert_eq!(rc, 0, "{out}{err}");
    assert_eq!(field(&out, "outcome").as_deref(), Some("woken"), "{out}");
}

/// N8 (review 1924c625): a dialog whose last column-0 prompt-glyph line
/// is a history message (fixture D) or its own choice (fixture K) read
/// busy — correctly — but the record called it a draft. It is named as
/// a dialog.
#[test]
fn a_dialog_is_named_as_a_dialog() {
    for (name, pane) in [("d-approval", PANE_APPROVAL), ("k-select", PANE_SELECT)] {
        for verb in ["factory", "send"] {
            let f = Fixture::new(&format!("n8-{name}-{verb}"), &["codex"], &[pane]);
            let ran = run_verb(&f, verb, &[]);
            assert_busy(&f, verb, &ran, "dialog", name);
            let reason = field(&ran.1, "reason").unwrap_or_default();
            assert!(!reason.contains("draft"), "{name} {verb}: {reason}");
        }
    }
    // A draft is still a draft.
    let f = Fixture::new("n8-draft", &["codex"], &[PANE_DRAFT]);
    let ran = run_verb(&f, "factory", &[]);
    assert_busy(&f, "factory", &ran, "draft", "draft");
}

/// B4, its other edge (measured by the conductor on the live `codex`
/// pane, 2026-10-29, read only: idle, two identical captures, mode 0 0,
/// width 134): Codex folds a COMPLETED tool call into one line that ends
/// in an ellipsis — `• Ran python3 - <<'PY' …`. Those lines sit in the
/// history of an idle pane, and one may be the last line above the box,
/// so the ellipsis rule reads only the running-status line; a folded
/// call is never one, or the door would never wake an idle Codex.
#[test]
fn folded_completed_calls_ending_in_an_ellipsis_do_not_read_busy() {
    const HISTORY: &str = "\
• Ran python3 - <<'PY' …

• Ran BOSS_ACTOR=codex@algedonic.dev boss-api GET /api/jobs/x > /scratch/…
  └ HTTP:200

  Recorded the blocker on the session.

  Worked for 1m 27s • 2:18 PM


› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";
    const LAST: &str = "\
  Recorded the blocker on the session.

• Ran BOSS_ACTOR=codex@algedonic.dev boss-api GET /api/jobs/x > /scratch/…

› Ask Codex to do anything

  GPT-6.1-Sol default · /work/boss
";
    for (name, pane) in [("history", HISTORY), ("last", LAST)] {
        let f = Fixture::new(
            &format!("b4-folded-{name}"),
            &["codex"],
            &[pane, pane, PANE_AFTER],
        );
        let (rc, out, err) = f.factory(&[("STUB_WIDTH", "134")]);
        assert_eq!(rc, 0, "{name}: {out}{err}");
        assert_eq!(
            field(&out, "outcome").as_deref(),
            Some("woken"),
            "{name}: {out}{err}"
        );
    }
}
