//! `infra/dev/factory-loop.sh` — the factory's builder CLI, supervised
//! (backlog 628dc540, David's request 2026-10-28).
//!
//! On 2026-10-28 agent-codex stopped on a model-capacity limit twice
//! (about 06:42-19:20Z and 20:57-22:38Z) and nothing restarted it: the
//! interactive TUI waits at an error prompt and never exits. The loop
//! runs the CLI's non-interactive mode one turn at a time, relaunches a
//! failed or limited turn on the declared ladder (2, 5, 10, 20, then 30
//! minutes forever), resumes the session the first turn printed, and
//! writes a restart note on the factory's open work-session.
//!
//! Pinned against a fake CLI whose turns are scripted (STUB_TURNS, one
//! word per turn), a fake `sleep` that records what it was asked to wait
//! (FACTORY_SLEEP, so no case waits), and a fake `boss-api` door that
//! answers the session read and records each restart note's body. PATH
//! is the stubs, then the system's own tools.

use boss_testing::{create_dir, repo_root, scratch_dir, with_default_signals, write_exec};
use std::path::PathBuf;

const SCRIPT: &str = "infra/dev/factory-loop.sh";

const SESSION_ID: &str = "0199aaaa-bbbb-4ccc-8ddd-eeeeffff0000";

/// One turn per word of STUB_TURNS, counted in a file:
///   cap    — codex's own words for a limit, then exit 1;
///   capok  — the same words, then exit 0 (a limit a turn ended on);
///   fail   — exit 3 with nothing said;
///   ok     — a clean turn that MENTIONS builder capacity in its work;
///   relay  — a clean turn whose final message RELAYS the reason a
///            restart note gave (review 97bb7698 finding 4);
///   wsid   — a clean turn whose work prints a work-session id after
///            `session id: ` (finding 5);
///   load   — codex's own high-load error line, then exit 1;
///   noroll — what codex-cli 0.159.3 prints for a resume id it has no
///            rollout for (measured 2026-10-29, offline): one `Error:`
///            line, no header, exit 1;
///   big    — a clean turn whose work prints 60 lines (review 1c7ab966
///            N5: one long turn grew the log past its bound).
/// Every other turn prints the header codex prints, `session id: <uuid>`,
/// near its top.
const CLI_STUB: &str = r#"#!/usr/bin/env bash
echo "cli $*" >> "$STUB_LOG"
cat > /dev/null
n=$(( $(cat "$STUB_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$STUB_COUNT"
read -r -a turns <<< "$STUB_TURNS"
what=${turns[$((n - 1))]:-ok}
if [ "$what" = noroll ]; then
  echo "Error: thread/resume: thread/resume failed: no rollout found for thread id $STUB_SESSION_ID (code -32600)"
  exit 1
fi
echo "OpenAI Codex v0.159.3"
echo "--------"
echo "approval: never"
echo "session id: $STUB_SESSION_ID"
echo "--------"
case $what in
  cap) echo "ERROR: stream error: rate limit exceeded; flex capacity unavailable"; exit 1 ;;
  capok) echo "ERROR: You've hit your usage limit."; exit 0 ;;
  load) echo "ERROR: Codex is currently experiencing high load."; exit 1 ;;
  fail) exit 3 ;;
  ok) echo "factory turn done: three builder capacity slots in use"; exit 0 ;;
  relay) echo "turn done: resumed after the model rate or capacity limit in factory_restart_20261029T0100Z; car 788d2f06 parked"; exit 0 ;;
  wsid) echo "read my open work-session id: 243c9e91-4ab3-403c-a280-4dec3251096a"; exit 0 ;;
  big) for i in $(seq 1 60); do echo "work line $i: building, building, building"; done; exit 0 ;;
esac
"#;

/// The approval the loop runs every turn under (finding 8): spelled
/// here so each test can read a turn's command without it.
const POLICY: &str = r#" -c approval_policy="never""#;

const SLEEP_STUB: &str = r#"#!/usr/bin/env bash
echo "sleep $*" >> "$STUB_LOG"
"#;

/// The pod's door, its streams split as the real one splits them: the
/// body on stdout, `HTTP:<code>` on stderr, exit 1 on a non-2xx.
const BOSS_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $1 $2" >> "$STUB_LOG"
[ "${STUB_API_DOWN:-}" = 1 ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
case "$1" in
  GET) printf '{"data":[{"id":"sess-1"}],"total":1}\n'; echo "HTTP:200" >&2 ;;
  PATCH) { printf 'NOTE '; tr -d '\n' < "$3"; echo; } >> "$STUB_LOG"; echo "HTTP:200" >&2 ;;
  *) exit 9 ;;
esac
"#;

struct Fixture {
    root: PathBuf,
    stubs: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("factory-loop-{name}"));
        let stubs = root.join("stubs");
        create_dir(&stubs);
        write_exec(&stubs.join("codex"), CLI_STUB);
        write_exec(&stubs.join("fake-sleep"), SLEEP_STUB);
        write_exec(&stubs.join("boss-api"), BOSS_API_STUB);
        std::fs::write(root.join("prompt.md"), "one factory turn\n").expect("write prompt");
        Self { root, stubs }
    }

    fn log_path(&self) -> PathBuf {
        self.root.join("stub.log")
    }

    fn loop_log(&self) -> PathBuf {
        self.root.join("home/factory-loop.log")
    }

    fn run(&self, turns: &str, max_turns: usize, env: &[(&str, &str)]) -> (i32, String) {
        self.run_seat(&[], turns, max_turns, env)
    }

    /// Run the loop with `args` (a seat, or nothing).
    fn run_seat(
        &self,
        args: &[&str],
        turns: &str,
        max_turns: usize,
        env: &[(&str, &str)],
    ) -> (i32, String) {
        // The `term` turn signals this loop, so it starts with default
        // dispositions, not whatever the runner handed the suite
        // (backlog 5f6a2cde): an ignored TERM would read as the loop's.
        let mut cmd = with_default_signals(repo_root().join(SCRIPT));
        cmd.args(args);
        cmd.env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.stubs.display()))
            .env("HOME", self.root.join("home"))
            .env("BOSS_DOOR_FRESHNESS", "off")
            .env("FACTORY_CLI", self.stubs.join("codex"))
            .env("FACTORY_SLEEP", self.stubs.join("fake-sleep"))
            .env("FACTORY_PROMPT_FILE", self.root.join("prompt.md"))
            .env("FACTORY_LOG", self.loop_log())
            .env("FACTORY_MAX_TURNS", max_turns.to_string())
            .env("STUB_LOG", self.log_path())
            .env("STUB_COUNT", self.root.join("count"))
            .env("STUB_TURNS", turns)
            .env("STUB_SESSION_ID", SESSION_ID);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run factory-loop.sh");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    fn lines(&self, prefix: &str) -> Vec<String> {
        self.log()
            .lines()
            .filter(|l| l.starts_with(prefix))
            .map(str::to_string)
            .collect()
    }

    fn sleeps(&self) -> Vec<String> {
        self.lines("sleep ")
    }

    /// Each turn's CLI command, with the approval override taken out
    /// (`the_loop_runs_every_turn_under_an_approval_nobody_must_give`
    /// pins that it is there).
    fn turns(&self) -> Vec<String> {
        self.lines("cli ")
            .iter()
            .map(|l| l.replace(POLICY, ""))
            .collect()
    }

    fn patches(&self) -> Vec<serde_json::Value> {
        self.lines("NOTE ")
            .iter()
            .map(|l| serde_json::from_str(&l["NOTE ".len()..]).expect("a note is JSON"))
            .collect()
    }

    /// The restart notes: every PATCH whose one key is `factory_restart_*`.
    fn notes(&self) -> Vec<serde_json::Value> {
        self.patches()
            .into_iter()
            .filter(|n| {
                n.as_object()
                    .is_some_and(|o| o.keys().any(|k| k.starts_with("factory_restart_")))
            })
            .collect()
    }

    /// The turn heartbeats: every PATCH carrying `factory_turn`.
    fn heartbeats(&self) -> Vec<serde_json::Value> {
        self.patches()
            .into_iter()
            .filter(|n| n.get("factory_turn").is_some())
            .collect()
    }
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(repo_root().join(SCRIPT))
        .expect("infra/dev/factory-loop.sh")
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "{SCRIPT} must be executable");
}

/// 628dc540's own test: a CLI that exits on a capacity limit is
/// relaunched on the declared ladder — 2, 5, 10, 20, then 30 minutes,
/// capped there, forever — and every relaunch writes a restart note
/// naming the reason and the exit code.
#[test]
fn a_capacity_exit_is_relaunched_on_the_declared_backoff_and_noted() {
    let f = Fixture::new("ladder");
    let (rc, out) = f.run("cap cap cap cap cap cap ok", 7, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.sleeps(),
        ["120", "300", "600", "1200", "1800", "1800"].map(|s| format!("sleep {s}")),
        "{}",
        f.log()
    );
    let notes = f.notes();
    assert_eq!(notes.len(), 6, "{}", f.log());
    for (note, wait) in notes.iter().zip([120, 300, 600, 1200, 1800, 1800]) {
        let obj = note.as_object().expect("one key");
        assert_eq!(obj.len(), 1, "one new key per restart: {note}");
        let (key, body) = obj.iter().next().unwrap();
        assert!(
            key.starts_with("factory_restart_"),
            "the key the silent-session rule ignores: {key}"
        );
        assert_eq!(body["reason"], "model rate or capacity limit", "{note}");
        assert_eq!(body["exit_code"], 1, "{note}");
        assert_eq!(body["backoff_seconds"], wait, "{note}");
    }
    assert!(
        f.lines("boss-api PATCH ")
            .iter()
            .all(|l| l == "boss-api PATCH /api/jobs/sess-1/metadata"),
        "the note lands on the factory's open session: {}",
        f.log()
    );
    let loop_log = std::fs::read_to_string(f.loop_log()).expect("the loop's log");
    assert!(
        loop_log.contains("rate limit exceeded") && loop_log.contains("turn 7 ended cleanly"),
        "each turn's output reaches the log a pane tails: {loop_log}"
    );
}

/// After the first turn the loop RESUMES the session codex printed, so
/// a restart keeps the factory's context.
#[test]
fn a_relaunch_resumes_the_session_the_first_turn_printed() {
    let f = Fixture::new("resume");
    let (rc, out) = f.run("cap ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.turns(),
        [
            "cli exec -".to_string(),
            format!("cli exec resume {SESSION_ID} -")
        ],
        "{}",
        f.log()
    );
}

/// A limit the turn ENDED on is a failed turn even when the exit code is
/// clean; a clean turn that only mentions capacity in its work is not.
#[test]
fn a_limit_is_read_from_the_words_and_a_clean_turn_resets_the_ladder() {
    let f = Fixture::new("words");
    let (rc, out) = f.run("capok ok fail", 3, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.sleeps(),
        ["sleep 120", "sleep 60"],
        "limit, then a clean turn's pause; the third turn stops the run: {}",
        f.log()
    );
    let notes = f.notes();
    assert_eq!(notes.len(), 2, "{}", f.log());
    let body = |n: &serde_json::Value| n.as_object().unwrap().values().next().unwrap().clone();
    assert_eq!(body(&notes[0])["exit_code"], 0);
    assert_eq!(body(&notes[0])["reason"], "model rate or capacity limit");
    assert_eq!(body(&notes[1])["reason"], "exit 3");
    assert_eq!(
        body(&notes[1])["backoff_seconds"],
        120,
        "the clean turn between reset the ladder"
    );
}

/// The note is visibility, not a dependency: with the API dark the loop
/// says so and relaunches anyway.
#[test]
fn a_dark_api_never_stops_the_loop() {
    let f = Fixture::new("dark");
    let (rc, out) = f.run("cap ok", 2, &[("STUB_API_DOWN", "1")]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.sleeps(), ["sleep 120"], "{}", f.log());
    assert_eq!(f.lines("cli ").len(), 2, "{}", f.log());
    assert!(
        out.contains("restart note NOT written"),
        "a skipped note is said: {out}"
    );
}

/// No prompt, no turn: refused by name rather than driving the CLI with
/// nothing.
#[test]
fn a_missing_prompt_is_refused_by_name() {
    let f = Fixture::new("noprompt");
    let missing = f.root.join("absent.md");
    let (rc, out) = f.run(
        "ok",
        1,
        &[("FACTORY_PROMPT_FILE", missing.to_str().unwrap())],
    );
    assert_eq!(rc, 66, "{out}");
    assert!(out.contains("REFUSED"), "{out}");
    assert!(f.lines("cli ").is_empty(), "{}", f.log());
}

/// The shipped prompt exists and tells the factory the two things the
/// loop and the alarm depend on: write a handoff every turn, and read
/// the loop's restart notes. The handoff is ONE `handoff` key, replaced
/// each turn (backlog e18b0b44, design e17c81c9): a key per turn grew
/// session 2d013092 until the packet budget refused the handoff itself
/// (413 at 12:00Z and 13:41Z on 2026-11-16). The turns before are in the
/// session's `jobs.job.updated` events.
#[test]
fn the_shipped_factory_prompt_names_the_turn_note_and_the_restart_key() {
    let prompt = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    assert!(prompt.contains("`handoff`"), "{prompt}");
    assert!(
        !prompt.contains("handoff_<YYYYMMDD>_<HHMM>Z"),
        "a new handoff key every turn is the pile e18b0b44 removed: {prompt}"
    );
    assert!(prompt.contains("factory_restart_"), "{prompt}");
    assert!(
        prompt.contains("a-silent-factory-holding-claimed-work-is-alarmed"),
        "{prompt}"
    );
}

/// The factory reads its orders and its last handoff by which keys are
/// present and newer, never by the day its session opened (review
/// 2f225d2e of car a9e8e486, finding 1). Measured 2026-11-16 ~15:25Z:
/// the factory's own session 0ae2784a, opened 13:53Z that day, held 15
/// `conductor_order_add_*` and 22 `handoff_*` keys and no `order` or
/// `handoff`, so a fallback reserved for sessions "opened before
/// 2026-11-16" had its next turn read neither. And a writer that has not
/// loaded the new text still writes `conductor_order_add_*` after an
/// `order` exists (finding 2), so those newer than the `order` are read
/// too — an order nothing made the factory read is urgent_first.rs's
/// failure.
#[test]
fn the_factory_reads_orders_and_handoffs_by_key_not_by_date() {
    let raw = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    let prompt = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !prompt.contains("opened before 2026-"),
        "the fallback is keyed on a session's opening date: {prompt}"
    );
    for needle in [
        "`conductor_order_add_*`",
        "`david_order_*`",
        "newer than its `at`",
        "newest `handoff_*`",
        "absent or older",
    ] {
        assert!(
            prompt.contains(needle),
            "factory-drain.md no longer names `{needle}`: {prompt}"
        );
    }
}

// ---- review 97bb7698 (CHANGES at 3346b34d): each finding's RED leg,
// ---- now a car test (decided_2026_10_29_factory_watch_repair on 628dc540).

/// Finding 4: a clean turn (exit 0) whose final message relays the
/// reason a restart note gave was read as a limited turn — 120 s
/// instead of 60, the ladder climbing, and a note claiming a limit with
/// exit_code 0. Limit words count only on a non-zero exit or on the
/// CLI's own error line.
#[test]
fn a_clean_turn_that_relays_the_restart_reason_is_not_a_limit() {
    let f = Fixture::new("relay");
    let (rc, out) = f.run("relay ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.sleeps(), ["sleep 60"], "{}", f.log());
    assert!(f.notes().is_empty(), "no restart happened: {}", f.log());
}

/// The control beside finding 4: a clean turn pauses 60 s, no note.
#[test]
fn a_clean_turn_pauses_and_writes_no_note() {
    let f = Fixture::new("clean");
    let (rc, out) = f.run("ok ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.sleeps(), ["sleep 60"], "{}", f.log());
    assert!(f.notes().is_empty(), "{}", f.log());
}

/// Finding 4's other half: codex-cli 0.159.3 also says "Codex is
/// currently experiencing high load." (in the binary), and a note on it
/// must name the limit rather than read `exit 1`.
#[test]
fn the_clis_high_load_line_is_a_limit() {
    let f = Fixture::new("load");
    let (rc, out) = f.run("load ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    let notes = f.notes();
    assert_eq!(notes.len(), 1, "{}", f.log());
    let body = notes[0].as_object().unwrap().values().next().unwrap();
    assert_eq!(
        body["reason"],
        "model rate or capacity limit",
        "{}",
        f.log()
    );
}

/// Finding 5: the resume id was the LAST `session id: <uuid>` anywhere
/// in the turn, so a uuid in the turn's own work (a work-session id)
/// replaced the one codex's header named. Only the header counts.
#[test]
fn a_uuid_in_the_turns_work_is_not_taken_as_the_codex_session() {
    let f = Fixture::new("wsid");
    let (rc, out) = f.run("wsid ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.turns()[1],
        format!("cli exec resume {SESSION_ID} -"),
        "{}",
        f.log()
    );
}

/// Finding 5's other half: a resume id that fails twice is dropped, so a
/// bad id never costs 30-minute waits forever. Measured offline on
/// 2026-10-29: `codex exec resume <unknown id> -` prints one `Error:`
/// line and exits 1 at once, every time.
#[test]
fn a_resume_id_that_fails_twice_is_dropped() {
    let f = Fixture::new("noroll");
    let (rc, out) = f.run("ok noroll noroll ok", 4, &[]);
    assert_eq!(rc, 0, "{out}");
    let resume = format!("cli exec resume {SESSION_ID} -");
    assert_eq!(
        f.turns(),
        [
            "cli exec -".to_string(),
            resume.clone(),
            resume,
            "cli exec -".to_string()
        ],
        "{}",
        f.log()
    );
}

/// Finding 6: the log is bounded — rotated at 10 MB, five files kept.
#[test]
fn the_log_rotates_and_keeps_five_files() {
    let f = Fixture::new("rotate");
    let (rc, out) = f.run(
        "ok ok ok ok ok ok ok ok",
        8,
        &[("FACTORY_LOG_MAX_BYTES", "1")],
    );
    assert_eq!(rc, 0, "{out}");
    let log = f.loop_log();
    let rotated = |i: usize| std::path::PathBuf::from(format!("{}.{i}", log.display()));
    assert!(log.exists(), "the live log");
    for i in 1..=4 {
        assert!(rotated(i).exists(), "{} is kept", rotated(i).display());
    }
    assert!(!rotated(5).exists(), "five files, no sixth");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect(SCRIPT);
    assert!(
        script.contains("log_max=${FACTORY_LOG_MAX_BYTES:-10485760}")
            && script.contains("log_keep=${FACTORY_LOG_KEEP:-5}"),
        "the shipped bound is five files of 10 MB"
    );
}

/// Decision B1: the loop writes `last_active_at` on the factory's open
/// work-session at every turn start and end, so the six-hour age-out
/// (work-session-ends-when-silent) never closes a live factory. The
/// write rides with `factory_turn`, the key the silent-session rule
/// ignores, so the heartbeat never reads as the factory speaking.
#[test]
fn every_turn_start_and_end_stamps_the_session_heartbeat() {
    let f = Fixture::new("heartbeat");
    let (rc, out) = f.run("ok cap", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    let beats = f.heartbeats();
    let phases: Vec<&str> = beats
        .iter()
        .map(|b| b["factory_turn"]["phase"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(phases, ["start", "end", "start", "end"], "{}", f.log());
    for b in &beats {
        assert!(b["last_active_at"].as_str().is_some(), "{b}");
        assert_eq!(
            b.as_object().unwrap().len(),
            2,
            "last_active_at and factory_turn only: {b}"
        );
    }
}

/// Finding 8, measured 2026-10-29 offline (a dead local provider, a
/// scratch CODEX_HOME holding infra/dev/cli/codex-config.toml): under
/// the config's approval_policy = "on-request", `codex exec` 0.159.3
/// printed `approval: never` — exec answers no approval prompt. The loop
/// states that policy on every turn rather than inheriting it from an
/// exec default a later CLI may change, so no turn can wait on an
/// approval nobody is there to give.
#[test]
fn the_loop_runs_every_turn_under_an_approval_nobody_must_give() {
    let f = Fixture::new("policy");
    let (rc, out) = f.run("cap ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    let raw = f.lines("cli ");
    assert_eq!(raw.len(), 2, "{}", f.log());
    assert_eq!(raw[0], format!("cli exec{POLICY} -"), "{}", f.log());
    assert_eq!(
        raw[1],
        format!("cli exec resume{POLICY} {SESSION_ID} -"),
        "{}",
        f.log()
    );
}

/// Finding 7 and decision B1: the prompt holds the role (releases,
/// failover and orders are the conductor's), says what to do when no
/// session is open, and never writes the supervisor's keys.
#[test]
fn the_factory_prompt_holds_the_conductor_boundary_and_opens_a_missing_session() {
    let prompt = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    assert!(
        prompt.contains("Releases, failover and orders belong to the conductor"),
        "{prompt}"
    );
    assert!(
        prompt.contains("boss job file --kind work-session"),
        "{prompt}"
    );
    assert!(prompt.contains("factory_turn"), "{prompt}");
}

// ---- review 1c7ab966 (CHANGES at 66ca3e33): folds N3 and N5, and
// ---- decision B1 on the prompt (decided_2026_10_29_factory_watch_repair_2).

/// N3: a resume id was dropped after ANY two failed resumed turns, so in
/// a capacity outage (the case this loop exists for) the second limited
/// turn, about 7 minutes in, threw the session away and the factory came
/// back cold. Only a resume-specific error (an unknown or expired id)
/// counts toward the drop; a limit or a plain failure never does.
#[test]
fn a_limit_or_a_plain_failure_never_drops_the_resume_id() {
    let resume = format!("cli exec resume {SESSION_ID} -");
    for turns in ["ok cap cap cap ok", "ok fail fail fail ok"] {
        let f = Fixture::new(&format!("keep-{}", &turns[3..6]));
        let (rc, out) = f.run(turns, 5, &[]);
        assert_eq!(rc, 0, "{out}");
        let mut want = vec!["cli exec -".to_string()];
        want.extend(std::iter::repeat_n(resume.clone(), 4));
        assert_eq!(f.turns(), want, "{turns}: {}", f.log());
        assert!(
            !out.contains("dropping resume id"),
            "{turns}: nothing resume-specific failed: {out}"
        );
    }
}

/// N5: the log was rotated only at a turn's start, so one long turn grew
/// it past its bound. It rotates as it is written, mid-turn, whenever it
/// passes the bound.
#[test]
fn the_log_rotates_mid_turn_once_it_passes_its_bound() {
    let f = Fixture::new("rotate-mid");
    let (rc, out) = f.run("big", 1, &[("FACTORY_LOG_MAX_BYTES", "400")]);
    assert_eq!(rc, 0, "{out}");
    let log = f.loop_log();
    let rotated = |i: usize| std::path::PathBuf::from(format!("{}.{i}", log.display()));
    assert!(
        rotated(1).exists(),
        "one turn of 60 lines passed a 400-byte bound and was rotated mid-turn"
    );
    let kept: Vec<std::path::PathBuf> = std::iter::once(log.clone())
        .chain((1..=4).map(rotated))
        .filter(|p| p.exists())
        .collect();
    for p in &kept {
        let size = std::fs::metadata(p).expect("a kept log").len();
        assert!(
            size <= 700,
            "{} is {size} bytes: the bound plus at most a line or two",
            p.display()
        );
    }
    let newest = kept
        .iter()
        .take(2)
        .map(|p| std::fs::read_to_string(p).unwrap_or_default())
        .collect::<String>();
    assert!(
        newest.contains("work line 60: "),
        "the turn's newest output is kept: {newest}"
    );
}

/// A CAPACITY REFUSAL IS A QUEUE, NOT A STOP (backlog a3bb1743, measured
/// 2026-11-16 03:31Z). The factory ended every turn for hours with
/// "Capacity refusal remains unresolved... No retry attempted": it read
/// the claim door's 409 "resource claim refused" (pool exhausted, step
/// queued) as one of the refusals this prompt forbids retrying, while 2 of
/// 6 opus slots and 0 of 2 sol slots were held. The Never paragraph names
/// the capacity refusal apart from the policy one, and says what to do.
///
/// The TEST is the body's `queued` field, never the 409 text (backlog
/// dfc9884e, review of car b8584ecb): every reason the claim door refuses
/// other than capacity-exhausted (duration-unknown, packet-not-open,
/// capacity-unknown and more) answers the same "resource claim refused"
/// with no `queued`, and a robot matching the text would retry one every
/// turn and, under "never a backlog item", never file it. Likewise only a
/// scheduler's Insufficient cpu or memory (dev-build.py's CAPACITY_MISS)
/// is a worker queue; affinity or taints are a placement defect to file.
///
/// The needles alone are vocabulary (review b46744de, finding 2): a
/// prompt that kept the old text-keyed sentence and added them elsewhere
/// would pass. So the old sentence, and the closed count the review
/// refuted (finding 1), are refused outright.
#[test]
fn the_factory_prompt_names_a_capacity_refusal_a_queue_not_a_stop() {
    let raw = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    let prompt = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let never = prompt
        .split_once("## Never")
        .map(|(_, rest)| rest)
        .expect("factory-drain.md has its Never section");
    for needle in [
        "resource claim refused",
        "a capacity refusal is a queue",
        "other ready work",
        "next turn",
        "a `queued` field",
        "no `queued` field",
        "every other reason the claim door refuses",
        "CAPACITY_MISS",
        "a refusal to file",
    ] {
        assert!(
            never.contains(needle),
            "the Never section must carry `{needle}`: a capacity refusal is a queue, not a \
             refusal to file and stop on:\n{never}"
        );
    }
    for stale in [
        "When the claim door answers 409 \"resource claim refused\" because",
        "five declaration defects",
    ] {
        assert!(
            !never.contains(stale),
            "the Never section must not carry `{stale}`: the `queued` field is the test, and \
             the refusals without it are not a closed set:\n{never}"
        );
    }
}

/// Decision B1: the prompt exempted a turn carrying a dispatched run from
/// progress notes, and a bound worker held the alarm off while it said
/// nothing — the morning stall of 2026-10-28. Every factory turn writes a
/// progress note at least every 30 minutes, whatever it carries.
#[test]
fn every_long_factory_turn_writes_progress_whatever_it_carries() {
    let raw = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    let prompt = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !prompt.contains("no dispatched run of yours carries"),
        "no exemption for a turn carrying a dispatched run: {prompt}"
    );
    // One `progress` key, replaced (design e17c81c9 decision 2): the
    // same per-turn growth as the handoffs, at a smaller rate.
    assert!(
        prompt.contains("`progress`") && prompt.contains("at least every 30 minutes"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("progress_<YYYYMMDD>_<HHMM>Z"),
        "a new progress key every 30 minutes grows the session without bound: {prompt}"
    );
}

/// A process a turn starts dies with the turn (backlog 480894e2, triage
/// run 39a87f76). Measured 2026-11-19: three Codex review runs started a
/// worker admission or a baseline test, ended the turn as rule 6 told
/// them to, and the next turn's `write_stdin` answered `Unknown process
/// id`; the admissions' launch receipts stopped within 3 s of their
/// turn's end and never reached their own 300 s deadline. So rule 6 must
/// keep a worker-bound command inside the turn that started it, and hand
/// on only what the record holds.
///
/// The rule is true only while every turn is a NEW CLI process resuming
/// a conversation, so the same case pins that structure: a loop that
/// came to hold one process across turns would fail here, and the rule
/// would be re-read rather than outlive what it describes.
#[test]
fn a_worker_bound_command_ends_inside_the_turn_that_started_it() {
    let raw = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    let prompt = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let rule = prompt
        .split_once(" 6. ")
        .and_then(|(_, rest)| rest.split_once("## "))
        .map(|(rule, _)| rule)
        .expect("factory-drain.md has its rule 6");
    for needle in [
        "dies when the turn ends",
        "unknown to the next turn",
        "every turn is a new CLI process",
        "`boss dispatch <run> --started --local-root`",
        "`dev-build exec`",
        "a baseline or candidate test",
        "`infra/gate.sh --lint`",
        "runs to its final answer inside the turn that started it",
        "`progress`",
        "only what the record holds carries over",
        "`worker_session`",
        "never a native process handle",
    ] {
        assert!(
            rule.contains(needle),
            "rule 6 must carry `{needle}`: a worker-bound command left running at a turn's \
             end is killed with it:\n{rule}"
        );
    }
    assert!(
        !rule.contains("Do not wait inside a turn for something that takes longer than the turn needs; the next turn will read it."),
        "the unqualified sentence ended turns over live admissions on 2026-11-19:\n{rule}"
    );

    // The structure the rule describes: each codex turn is its own
    // process, the later one resuming the session the first printed.
    let f = Fixture::new("turn-is-a-process");
    let (rc, out) = f.run("ok ok", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.turns(),
        [
            "cli exec -".to_string(),
            format!("cli exec resume {SESSION_ID} -")
        ],
        "{}",
        f.log()
    );
    // And each claude seat turn is its own `claude -p --resume` process.
    let c = Fixture::claude("claude-turn-is-a-process");
    let (rc, out) = c.run_claude_seat("claude-b-factory", "claude-b@algedonic.dev", "ok ok", 2);
    assert_eq!(rc, 0, "{out}");
    let turns = c.turns();
    assert_eq!(turns.len(), 2, "one CLI process per turn: {}", c.log());
    assert!(
        turns[1].starts_with("cli -p ") && turns[1].contains(" --resume "),
        "the second turn is a fresh process resuming the conversation: {turns:?}"
    );
}

/// Design 40517105 D3/D4 (backlog 8a8264f4). Rule 6 keeps a worker-bound
/// command inside its turn (480894e2), but on the claude-b-factory seat
/// it was a dispatched SUBAGENT that turn 1's end stopped mid-build (fold
/// run 570fa370, 2026-11-18), and a subagent is no command the rule
/// lists. And a turn the loop's timeout or a pod roll ends still loses
/// what it ran, which since 141be047 leaves an `interrupted` or `lost`
/// receipt that nothing in the prompt reads: the next turn reads it
/// first, and runs again what it cannot prove.
#[test]
fn a_turn_waits_for_its_subagent_and_starts_by_reading_what_the_last_one_lost() {
    let raw = std::fs::read_to_string(repo_root().join("infra/dev/factory-drain.md"))
        .expect("infra/dev/factory-drain.md");
    let prompt = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let between = |start: &str, end: &str| -> String {
        prompt
            .split_once(start)
            .and_then(|(_, rest)| rest.split_once(end))
            .map(|(rule, _)| rule.to_string())
            .unwrap_or_else(|| panic!("factory-drain.md has `{start}` before `{end}`"))
    };
    let rule6 = between(" 6. ", "## ");
    for needle in [
        "A subagent this turn dispatches",
        "waits for its report",
        "fold run 570fa370",
    ] {
        assert!(
            rule6.contains(needle),
            "rule 6 must carry `{needle}`: a turn that ends over its subagent stops it:\n{rule6}"
        );
    }
    let rule3 = between(" 3. ", " 4. ");
    for needle in [
        "`dev-build receipt --run <run>`",
        "`observed`",
        "`active`",
        "`lost` or `interrupted`",
        "outcome unknown",
        "run again",
        "never inferred from a log tail",
    ] {
        assert!(
            rule3.contains(needle),
            "rule 3 must carry `{needle}`: a receipt the next turn never reads is a loss nobody sees:\n{rule3}"
        );
    }
}

/// Claude Code -p, FAITHFUL to what the review of car aab5f34e measured
/// (run ce8970ce, Claude Code 2.1.326, 2026-11-17): EVERY turn fires
/// SessionStart (`source` startup, or resume for `--resume`) before its
/// work and SessionEnd (`reason` "other") after it. The stub runs the
/// commands the project's own `.claude/settings.json` wires for those
/// two events, with CLAUDE_PROJECT_DIR set as Claude Code sets it, so
/// the REAL session-start.sh and session-end.sh run under the loop.
/// The stub that stood here fired no hook at all, and so the gate was
/// green on a seat whose every turn filed and closed its own packet.
///
/// One turn per word of STUB_TURNS:
///   ok     — a clean turn;
///   cap    — an API error naming an overloaded model, then exit 1;
///   noconv — what Claude Code 2.1.326 prints for a resume id it has no
///            conversation for (measured 2026-11-17 against a scratch
///            config home), exit 1;
///   term   — a clean turn during which the seat is stopped: SIGTERM to
///            the loop (the grandparent: the loop runs `timeout`, which
///            runs this), as a pod roll or a kill-session sends it.
const CLAUDE_STUB: &str = r#"#!/usr/bin/env bash
echo "cli $*" >> "$STUB_LOG"
cat > /dev/null
id='' source=startup
while [ $# -gt 0 ]; do
  case $1 in
    --session-id) id=$2 ;;
    --resume) id=$2 source=resume ;;
  esac
  shift
done
fire() {
  local cmd
  while IFS= read -r cmd; do
    [ -n "$cmd" ] || continue
    printf '%s' "$2" | bash -c "$cmd" 2>> "$STUB_LOG.hooks"
  done < <(jq -r --arg e "$1" '.hooks[$e][]?.hooks[]?.command' "$CLAUDE_PROJECT_DIR/.claude/settings.json")
}
fire SessionStart "$(jq -nc --arg s "$id" --arg src "$source" --arg cwd "$PWD" \
  '{session_id: $s, source: $src, cwd: $cwd, hook_event_name: "SessionStart"}')"
n=$(( $(cat "$STUB_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$STUB_COUNT"
read -r -a turns <<< "$STUB_TURNS"
code=0
case ${turns[$((n - 1))]:-ok} in
  cap) echo 'API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}'; code=1 ;;
  noconv) echo "No conversation found with session ID: $id"; code=1 ;;
  term) kill -TERM "$(cut -d' ' -f4 "/proc/$PPID/stat")"; echo "seat turn done"; ;;
  *) echo "seat turn done" ;;
esac
fire SessionEnd "$(jq -nc --arg s "$id" --arg cwd "$PWD" \
  '{session_id: $s, reason: "other", cwd: $cwd, hook_event_name: "SessionEnd"}')"
exit "$code"
"#;

/// The system of record the hooks and the loop write to, as state: a
/// directory per work-session under STUB_WORLD holding its metadata and
/// whether it is open. `boss job file` files one (answering the real
/// verb's confirmation line, which session-start.sh copies the id
/// from); `boss-api GET /api/jobs/<id>` answers its `active` step,
/// ready while open and completed once closed; the step PUT that
/// session-end.sh sends closes it (work-session.toml: `active` is the
/// terminal step); a list read answers the OPEN sessions whose metadata
/// carries the filter the query names. Every metadata PATCH is logged
/// as `NOTE <body>` (what the codex fixture logs) and as `ON <id>
/// <open|closed> <body>`, so a test reads which packet a write landed
/// on and whether it was open when it did.
const WORLD_BOSS_STUB: &str = r#"#!/usr/bin/env bash
echo "boss $*" >> "$STUB_LOG"
case "$1 $2" in
  'job file')
    meta=''
    while [ $# -gt 0 ]; do [ "$1" = --metadata ] && meta=$2; shift; done
    mkdir -p "$STUB_WORLD"
    n=$(( $(ls "$STUB_WORLD" | wc -l) + 1 ))
    id=$(printf '00000000-0000-4000-8000-%012d' "$n")
    mkdir -p "$STUB_WORLD/$id"
    cp "$meta" "$STUB_WORLD/$id/metadata.json"
    echo open > "$STUB_WORLD/$id/status"
    echo "FILED $id" >> "$STUB_LOG"
    echo "boss job: filed $id  \"Session\" — confirmed by reading it back"
    ;;
  'job patch') exit 0 ;;
  *) exit 1 ;;
esac
"#;

const WORLD_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $1 $2" >> "$STUB_LOG"
[ "${STUB_API_DOWN:-}" = 1 ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
path=${2%%\?*}
id=${path#/api/jobs/}
id=${id%%/*}
case "$1 $path" in
  'GET /api/jobs')
    q=${2#*metadata=}
    for d in $(ls -r "$STUB_WORLD" 2>/dev/null); do
      [ "$(cat "$STUB_WORLD/$d/status")" = open ] || continue
      m=$STUB_WORLD/$d/metadata.json
      a=$(jq -r '{actor} | tojson | @uri' "$m")
      as=$(jq -r '{actor, session_id} | tojson | @uri' "$m")
      [ "$q" = "$a" ] || [ "$q" = "$as" ] && echo "$d"
    done | jq -R . | jq -sc '{data: map({id: .}), total: length}'
    ;;
  'GET /api/jobs/'*)
    [ -d "$STUB_WORLD/$id" ] || { echo "HTTP:404" >&2; exit 1; }
    st=ready
    [ "$(cat "$STUB_WORLD/$id/status")" = closed ] && st=completed
    jq -nc --arg id "$id" --arg st "$st" \
      '{id: $id, kind: "work-session", steps: [{id: "st-opened", spec_slug: "opened", status: "completed"}, {id: "st-active", spec_slug: "active", status: $st}]}'
    ;;
  'PATCH /api/jobs/'*'/steps/'*'/metadata') ;;
  'PUT /api/jobs/'*'/steps/'*)
    echo closed > "$STUB_WORLD/$id/status"
    echo "CLOSED $id" >> "$STUB_LOG"
    ;;
  'PATCH /api/jobs/'*'/metadata')
    { printf 'NOTE '; tr -d '\n' < "$3"; echo; } >> "$STUB_LOG"
    { printf 'ON %s %s ' "$id" "$(cat "$STUB_WORLD/$id/status" 2>/dev/null || echo absent)"; tr -d '\n' < "$3"; echo; } >> "$STUB_LOG"
    [ -d "$STUB_WORLD/$id" ] && jq -s '.[0] + .[1]' "$STUB_WORLD/$id/metadata.json" "$3" > "$STUB_WORLD/$id/m.json" \
      && mv "$STUB_WORLD/$id/m.json" "$STUB_WORLD/$id/metadata.json"
    ;;
  *) exit 9 ;;
esac
echo "HTTP:200" >&2
"#;

impl Fixture {
    /// A fixture whose `claude` is the faithful stub above, firing the
    /// project's real hooks into a stateful system of record.
    fn claude(name: &str) -> Self {
        let f = Self::new(name);
        write_exec(&f.stubs.join("claude"), CLAUDE_STUB);
        write_exec(&f.stubs.join("boss"), WORLD_BOSS_STUB);
        write_exec(&f.stubs.join("boss-api"), WORLD_API_STUB);
        f
    }

    /// Run a claude seat as the pod runs it: the seat's actor signs the
    /// hooks, the hooks keep their state under this fixture, and the
    /// hooks are the project's, read out of `.claude/settings.json`.
    fn run_claude_seat(&self, seat: &str, actor: &str, turns: &str, max: usize) -> (i32, String) {
        let state = self.root.join("hook-state");
        let world = self.root.join("world");
        let cli = self.stubs.join("claude");
        self.run_seat(
            &[seat],
            turns,
            max,
            &[
                ("FACTORY_CLI", cli.to_str().unwrap()),
                ("BOSS_ACTOR", actor),
                ("BOSS_HOOK_STATE", state.to_str().unwrap()),
                ("STUB_WORLD", world.to_str().unwrap()),
                ("CLAUDE_PROJECT_DIR", repo_root().to_str().unwrap()),
            ],
        )
    }

    /// The work-sessions filed, in filing order.
    fn filed(&self) -> Vec<String> {
        self.lines("FILED ")
            .iter()
            .map(|l| l["FILED ".len()..].to_string())
            .collect()
    }

    /// The work-sessions closed, in closing order.
    fn closed(&self) -> Vec<String> {
        self.lines("CLOSED ")
            .iter()
            .map(|l| l["CLOSED ".len()..].to_string())
            .collect()
    }

    /// Every metadata write: the packet it landed on, whether that
    /// packet was open when it did, and the body.
    fn landed(&self) -> Vec<(String, String, serde_json::Value)> {
        self.lines("ON ")
            .iter()
            .map(|l| {
                let mut it = l["ON ".len()..].splitn(3, ' ');
                let id = it.next().unwrap_or_default().to_string();
                let state = it.next().unwrap_or_default().to_string();
                let body = serde_json::from_str(it.next().unwrap_or_default())
                    .unwrap_or_else(|e| panic!("a write's body is JSON ({e}): {l}"));
                (id, state, body)
            })
            .collect()
    }

    /// The turn heartbeats as (packet, open|closed, phase, turn).
    fn beats_landed(&self) -> Vec<(String, String, String, u64)> {
        self.landed()
            .into_iter()
            .filter_map(|(id, state, b)| {
                let t = b.get("factory_turn")?;
                Some((
                    id,
                    state,
                    t["phase"].as_str().unwrap_or_default().to_string(),
                    t["turn"].as_u64().unwrap_or_default(),
                ))
            })
            .collect()
    }
}

/// B1 of review ce8970ce, on car aab5f34e: Claude Code -p fires
/// SessionStart and SessionEnd on EVERY turn, so a seat whose turns end
/// its work-session files and closes one packet per turn, and the
/// loop's heartbeats and restart notes never find one open — the proof
/// probe could never pass. The seat's loop OWNS its work-session: ONE
/// packet for the conversation, open across every turn, carrying every
/// turn's start and end heartbeat and the failed turn's restart note,
/// and ended by the loop when the loop stops.
#[test]
fn a_claude_seat_keeps_one_open_work_session_across_turns_that_fire_the_real_hooks() {
    let f = Fixture::claude("claude-seat-owns-its-session");
    let (rc, out) = f.run_claude_seat("claude-b-factory", "claude-b@algedonic.dev", "ok cap ok", 3);
    assert_eq!(rc, 0, "{out}");
    let hooks = std::fs::read_to_string(f.root.join("stub.log.hooks")).unwrap_or_default();
    assert!(
        hooks.contains("boss-hook session-end"),
        "the stub fires the project's real SessionEnd hook every turn: {hooks}"
    );
    let filed = f.filed();
    assert_eq!(
        filed.len(),
        1,
        "ONE work-session for the seat's conversation, not one per turn:\n{}\n{hooks}",
        f.log()
    );
    let ws = &filed[0];
    let beats = f.beats_landed();
    let want: Vec<(String, String, String, u64)> = (1..=3)
        .flat_map(|t| {
            ["start", "end"]
                .into_iter()
                .map(move |p| (p.to_string(), t))
        })
        .map(|(p, t)| (ws.clone(), "open".to_string(), p, t))
        .collect();
    assert_eq!(
        beats,
        want,
        "every turn's start AND end heartbeat lands on the one OPEN packet:\n{}",
        f.log()
    );
    let notes: Vec<_> = f
        .landed()
        .into_iter()
        .filter(|(_, _, b)| {
            b.as_object()
                .is_some_and(|o| o.keys().any(|k| k.starts_with("factory_restart_")))
        })
        .collect();
    assert_eq!(notes.len(), 1, "{}", f.log());
    assert_eq!(
        (&notes[0].0, notes[0].1.as_str()),
        (ws, "open"),
        "the failed turn's restart note lands on the open packet: {notes:?}"
    );
    assert_eq!(
        f.closed(),
        std::slice::from_ref(ws),
        "no turn's end closes it; the loop ends it once, when it stops:\n{}",
        f.log()
    );
    let meta: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.root.join("world").join(ws).join("metadata.json"))
            .expect("the packet's metadata"),
    )
    .expect("JSON");
    assert_eq!(meta["actor"], "claude-b@algedonic.dev", "{meta}");
    assert_eq!(meta["source"], "seat", "{meta}");
    let turns = f.turns();
    let id = turns[0]
        .strip_prefix("cli -p --permission-mode auto --session-id ")
        .unwrap_or_else(|| panic!("the first turn names its conversation: {}", turns[0]));
    assert_eq!(meta["session_id"], id, "{meta}");
}

/// The claude seats of infra/dev/seats.tsv (backlog b513514c): the loop
/// takes a seat, reads its row, and drives Claude Code's non-interactive
/// mode the way it drives codex — under a conversation id the loop
/// chose, resumed every later turn, with the seat's pause and the
/// backoff ladder.
#[test]
fn a_claude_seat_resumes_its_own_conversation_and_beats_on_its_own_session() {
    let f = Fixture::claude("claude-seat");
    let (rc, out) = f.run_claude_seat("claude-b-factory", "claude-b@algedonic.dev", "ok cap ok", 3);
    assert_eq!(rc, 0, "{out}");
    let turns = f.turns();
    assert_eq!(turns.len(), 3, "{}", f.log());
    let id = turns[0]
        .strip_prefix("cli -p --permission-mode auto --session-id ")
        .unwrap_or_else(|| panic!("the first turn names its conversation: {}", turns[0]))
        .to_string();
    assert_eq!(id.len(), 36, "a uuid: {id}");
    for t in &turns[1..] {
        assert_eq!(t, &format!("cli -p --permission-mode auto --resume {id}"));
    }
    // The seat's row: pause 60.
    assert_eq!(f.sleeps(), ["sleep 60", "sleep 120"], "{}", f.log());
    let beats = f.heartbeats();
    assert!(
        beats.len() == 6
            && beats
                .iter()
                .all(|b| b["factory_turn"]["seat"] == "claude-b-factory"),
        "{beats:?}"
    );
    let notes = f.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    let note = notes[0].as_object().unwrap().values().next().unwrap();
    assert_eq!(note["reason"], "model rate or capacity limit", "{note}");
    assert_eq!(note["seat"], "claude-b-factory", "{note}");
}

/// A claude seat's id that the CLI says it has no conversation for is
/// dropped after two such turns, and the next turn names a new one. The
/// dropped conversation's work-session is ENDED by the loop as it drops
/// the id, and the new conversation gets its own.
#[test]
fn a_claude_seat_drops_an_id_its_cli_has_no_conversation_for() {
    let f = Fixture::claude("claude-seat-noconv");
    let (rc, out) = f.run_claude_seat(
        "conductor",
        "claude@algedonic.dev",
        "cap noconv noconv ok",
        4,
    );
    assert_eq!(rc, 0, "{out}");
    let turns = f.turns();
    assert_eq!(turns.len(), 4, "{}", f.log());
    let fresh = "cli -p --permission-mode auto --session-id ";
    assert!(turns[0].starts_with(fresh), "{turns:?}");
    let first = &turns[0][fresh.len()..];
    assert!(
        turns[1].ends_with(&format!("--resume {first}")),
        "{turns:?}"
    );
    assert!(
        turns[2].ends_with(&format!("--resume {first}")),
        "{turns:?}"
    );
    assert!(
        turns[3].starts_with(fresh) && !turns[3].ends_with(first),
        "after two turns failed on the id, a new conversation: {turns:?}"
    );
    let filed = f.filed();
    assert_eq!(
        filed.len(),
        2,
        "one work-session per conversation:\n{}",
        f.log()
    );
    assert_eq!(
        f.closed(),
        [filed[0].clone(), filed[1].clone()],
        "the dropped conversation's packet ends at the drop, the new one when the loop stops:\n{}",
        f.log()
    );
    let beats = f.beats_landed();
    assert!(
        beats
            .iter()
            .all(|(id, st, _, t)| st == "open" && *id == filed[usize::from(*t == 4)]),
        "turns 1-3 beat on the first conversation's packet and turn 4 on the second's, each open:\n{beats:?}"
    );
}

/// A seat stopped by a signal — a pod roll's SIGTERM, a kill-session —
/// ends its work-session on the way out, so the record never shows a
/// stopped seat as an open session for the six hours the silent-session
/// clock would take.
#[test]
fn a_stopped_claude_seat_ends_its_work_session() {
    let f = Fixture::claude("claude-seat-term");
    let (rc, out) = f.run_claude_seat(
        "claude-b-factory",
        "claude-b@algedonic.dev",
        "ok term ok",
        3,
    );
    assert_eq!(rc, 143, "a TERM ends the loop as TERM would: {out}");
    assert_eq!(f.turns().len(), 2, "no turn after the signal: {}", f.log());
    let filed = f.filed();
    assert_eq!(filed.len(), 1, "{}", f.log());
    assert_eq!(f.closed(), filed, "{}", f.log());
    assert!(
        std::fs::read_to_string(f.loop_log())
            .unwrap_or_default()
            .contains("stopped by SIGTERM"),
        "{}",
        f.log()
    );
}

/// A seat the list does not hold is refused before anything runs.
#[test]
fn an_unknown_seat_is_refused() {
    let f = Fixture::new("no-seat");
    let (rc, out) = f.run_seat(&["nobody"], "ok", 1, &[]);
    assert_eq!(rc, 64, "{out}");
    assert!(out.contains("no seat 'nobody'"), "{out}");
    assert!(f.turns().is_empty(), "{}", f.log());
}

// ---------------------------------------------------------------------
// A PULL SEAT (backlog 35f828cf, David's morning plan item 2,
// 2026-11-17): a seat whose row says `next` in its `pull` column takes
// its work through `boss dispatch --next` when its one slot is free,
// instead of waiting for a nomination or a pane nudge.
//
// THE SLOT IS READ OFF THE RECORD (review 0d65ef4c B1, 2026-11-20). The
// first build kept the slot as a run id in a file beside the log, and
// every way of losing that file — a failed write (C), a brief whose run
// line was worded otherwise (B), a restart under another log or after a
// kill (F) — pulled a second step while the first run's building was
// still open. These fakes keep the record the way the jobs API does: the
// station pull opens a run, the agent-run listing shows it until its
// building step ends, and the loop is judged on what it pulls.
// ---------------------------------------------------------------------

/// The run the fake station pull hands out, and lists as open.
const PULLED_RUN: &str = "0199bbbb-cccc-4ddd-8eee-ffff00001111";

/// One open agent-run as `GET /api/jobs?kind=agent-run&status=open`
/// lists it (slim steps): the login it signs as in `metadata.agent`, and
/// its `building` step's status and holder. The real verb places
/// `building` on the REGISTERED id (dispatch.rs nominate_building) while
/// the run's `agent` is the login, so the two differ, as they do here.
fn run_row(id: &str, building: &str, agent: &str, holder: Option<&str>) -> String {
    serde_json::json!({
        "id": id, "kind": "agent-run", "status": "open",
        "metadata": {"agent": agent},
        "steps": [
            {"spec_slug": "claimed", "status": "completed", "assignee_id": null},
            {"spec_slug": "building", "status": building, "assignee_id": holder},
        ],
    })
    .to_string()
}

/// A codex CLI that keeps each turn's stdin, so a test can read which
/// prompt the turn was handed: `$STUB_STDIN.<turn>`. The turn numbered
/// STUB_END_ON_TURN ends the pulled run's building step, as an executor
/// handing its step to the gate does.
const PULL_CLI_STUB: &str = r#"#!/usr/bin/env bash
n=$(( $(cat "$STUB_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$STUB_COUNT"
echo "cli $*" >> "$STUB_LOG"
cat > "$STUB_STDIN.$n"
if [ "$n" = "${STUB_END_ON_TURN:-}" ]; then
  sed -i 's/"building","status":"active"/"building","status":"completed"/' "$STUB_RUNS"
fi
echo "session id: $STUB_SESSION_ID"
echo "turn done"
"#;

/// `boss dispatch --next`, one answer per word of STUB_PULLS, counted
/// across processes in a file, its streams split as the real verb's
/// are — the brief alone on stdout, every status line on stderr:
///   brief    — a claimed step: its run opened on the record, and the
///              brief, naming it in THE RUN's own sentence;
///   reworded — the same, but the brief words its run line otherwise
///              (review 0d65ef4c scenario B);
///   empty    — no admissible work: exit 0, nothing on stdout;
///   refused  — the door's refusal: one line on stderr, exit 1.
/// It signs as the real verb does (identity.rs): `BOSS_ACTOR`, else the
/// actor file, each trimmed and blank meaning nothing — and with neither
/// it refuses the write. The run it opens is signed by that login and its
/// building placed on the registered id the login belongs to (review
/// f8cbaa0e scenario M: a loop started by hand signs as the actor file).
const BOSS_STUB: &str = r#"#!/usr/bin/env bash
echo "boss $*" >> "$STUB_LOG"
who=$(printf '%s' "${BOSS_ACTOR:-}" | tr -d '[:space:]')
[ -n "$who" ] || who=$(tr -d '[:space:]' < "$HOME/.config/boss/actor" 2>/dev/null)
if [ -z "$who" ]; then
  echo "Error: refusing to sign POST /api/stations/pull: nothing names the actor running this command" >&2
  exit 1
fi
echo "signed $who" >> "$STUB_LOG"
n=$(( $(cat "$STUB_PULL_COUNT" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$STUB_PULL_COUNT"
read -r -a pulls <<< "$STUB_PULLS"
what=${pulls[$((n - 1))]:-empty}
echo "boss station pull: $who at engineering, request r-$n" >&2
# STUB_BREAK_PROMPT=1: the turn prompt's place stops being writable
# after the loop checked it and before it writes the brief there.
if [ "${STUB_BREAK_PROMPT:-}" = 1 ]; then
  rm -f "$HOME/factory-loop.log.prompt"
  mkdir -p "$HOME/factory-loop.log.prompt"
fi
case $what in
  brief | reworded)
    jq -nc --arg id "$STUB_RUN" --arg who "$who" --arg holder "agent-${who%%@*}" \
      '{id: $id, kind: "agent-run", status: "open", metadata: {agent: $who},
        steps: [{spec_slug: "claimed", status: "completed", assignee_id: null},
                {spec_slug: "building", status: "active", assignee_id: $holder}]}' >> "$STUB_RUNS" ;;
esac
case $what in
  brief) printf '== THE PACKET ==\nbuild the pulled thing\n\n== THE RUN ==\n\nYour run is agent-run %s (profile `builder`, requested model opus-5-5).\n' "$STUB_RUN" ;;
  reworded) printf '== THE PACKET ==\nbuild the pulled thing\n\n== THE RUN ==\n\nThis brief opened agent-run %s for you.\n' "$STUB_RUN" ;;
  empty) ;;
  refused) echo "Error: boss dispatch --next: station selection refused: the native owner is unavailable" >&2; exit 1 ;;
esac
"#;

/// The door: the agents registry, the agent-runs, the work-session read
/// and the PATCHes. A listing with `status=open` is every line of
/// STUB_RUNS (one open run each); one with a `metadata=` filter and no
/// status is the control (review f8cbaa0e B3), every run of any status
/// — STUB_HISTORY's closed ones, then STUB_RUNS — signed by that
/// metadata's `agent`, newest first, cut to its `limit`.
///   STUB_AGENTS_DOWN=1 — the registry read fails;
///   STUB_RUNS_DOWN=1   — the run listing is refused (a 403);
///   STUB_RUNS_CUT=1    — the listing says one more run than it holds;
///   STUB_BLIND=1       — every agent-run read answers 200 with nothing
///                        in it, as a denied read scope or a wrong or
///                        dark instance answers (jobs.rs list_jobs);
///   STUB_HIDE_NEW=1    — every read leaves out the run the pull opens.
const PULL_API_STUB: &str = r#"#!/usr/bin/env bash
echo "boss-api $1 $2" >> "$STUB_LOG"
case "$1 $2" in
  "GET /api/agents")
    [ "${STUB_AGENTS_DOWN:-}" = 1 ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
    printf '{"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]},{"id":"agent-claude","aliases":["claude@algedonic.dev"]}]}\n'
    echo "HTTP:200" >&2 ;;
  "GET /api/jobs?kind=agent-run"*)
    [ "${STUB_RUNS_DOWN:-}" = 1 ] && { echo "HTTP:403" >&2; exit 1; }
    if [ "${STUB_BLIND:-}" = 1 ]; then
      printf '{"data":[],"total":0}\n'
      echo "HTTP:200" >&2
      exit 0
    fi
    hide=''
    [ "${STUB_HIDE_NEW:-}" = 1 ] && hide=$STUB_RUN
    case $2 in
      *status=open*)
        cat "$STUB_RUNS" 2>/dev/null |
          jq -s --arg hide "$hide" --argjson more "${STUB_RUNS_CUT:-0}" \
            '[.[] | select(.id != $hide)] | {data: ., total: (length + $more)}' ;;
      *metadata=*)
        enc=${2#*metadata=}
        enc=${enc%%&*}
        doc=$(printf '%b' "${enc//%/\\x}")
        limit=${2#*limit=}
        limit=${limit%%&*}
        cat "$STUB_HISTORY" "$STUB_RUNS" 2>/dev/null |
          jq -s --arg hide "$hide" --argjson doc "$doc" --argjson limit "$limit" \
            '[.[] | select(.id != $hide and .metadata.agent == $doc.agent)] | reverse
              | {data: .[:$limit], total: length}' ;;
      *) echo "HTTP:400" >&2; exit 1 ;;
    esac
    echo "HTTP:200" >&2 ;;
  GET*) printf '{"data":[{"id":"sess-1"}],"total":1}\n'; echo "HTTP:200" >&2 ;;
  PATCH*) { printf 'NOTE '; tr -d '\n' < "$3"; echo; } >> "$STUB_LOG"; echo "HTTP:200" >&2 ;;
  *) exit 9 ;;
esac
"#;

impl Fixture {
    /// A fixture whose seat list holds one codex seat, `puller`, with
    /// `pull` in its pull column and a 45 s pause.
    fn puller(name: &str, pull: &str) -> Self {
        let f = Self::new(name);
        write_exec(&f.stubs.join("codex"), PULL_CLI_STUB);
        write_exec(&f.stubs.join("boss"), BOSS_STUB);
        write_exec(&f.stubs.join("boss-api"), PULL_API_STUB);
        std::fs::write(
            f.root.join("seats.tsv"),
            format!("# seat cli actor prompt pause twin pull\npuller codex agent-codex unused 45 - {pull}\n"),
        )
        .expect("write seats");
        // The agent's past on the record: one closed run its login
        // signed, which the control read must see before a pull (review
        // f8cbaa0e B3). `fresh_agent` takes it away. Its id is a run id
        // as the record spells one, because the loop keeps it beside the
        // log and reads anything else there as '?' (review 2d18db68 N11).
        let mut past: serde_json::Value = serde_json::from_str(&run_row(
            "0199aaaa-0000-4000-8000-00000000a11d",
            "completed",
            "codex@algedonic.dev",
            Some("agent-codex"),
        ))
        .expect("a run row is JSON");
        past["status"] = "closed".into();
        std::fs::write(f.history(), format!("{past}\n")).expect("write history");
        f
    }

    fn runs(&self) -> PathBuf {
        self.root.join("runs.jsonl")
    }

    fn history(&self) -> PathBuf {
        self.root.join("history.jsonl")
    }

    /// An agent with no run of any status on the record.
    fn fresh_agent(&self) {
        std::fs::write(self.history(), "").expect("clear history");
    }

    /// The actor file `boss` reads when BOSS_ACTOR names nobody.
    fn actor_file(&self, id: &str) {
        let dir = self.root.join("home/.config/boss");
        create_dir(&dir);
        std::fs::write(dir.join("actor"), format!("{id}\n")).expect("write actor file");
    }

    /// Put one open agent-run on the fake record.
    fn seed_run(&self, row: &str) {
        let mut all = std::fs::read_to_string(self.runs()).unwrap_or_default();
        all.push_str(row);
        all.push('\n');
        std::fs::write(self.runs(), all).expect("seed run");
    }

    /// Run the `puller` seat for `passes` passes of the loop.
    fn pull(&self, pulls: &str, passes: usize, env: &[(&str, &str)]) -> (i32, String) {
        self.pull_as(&["puller"], pulls, passes, env)
    }

    /// Run the loop with `args` (the seat, or none) for `passes` passes.
    fn pull_as(
        &self,
        args: &[&str],
        pulls: &str,
        passes: usize,
        env: &[(&str, &str)],
    ) -> (i32, String) {
        let seats = self.root.join("seats.tsv");
        let stdin = self.root.join("stdin");
        let pull_count = self.root.join("pull-count");
        let runs = self.runs();
        let history = self.history();
        // The seat signs as its own login unless a case says otherwise
        // (a later entry of the same name wins; blank names nobody).
        let mut all: Vec<(&str, &str)> = vec![
            ("FACTORY_SEATS", seats.to_str().unwrap()),
            ("BOSS_ACTOR", "codex@algedonic.dev"),
            ("STUB_STDIN", stdin.to_str().unwrap()),
            ("STUB_PULL_COUNT", pull_count.to_str().unwrap()),
            ("STUB_PULLS", pulls),
            ("STUB_RUN", PULLED_RUN),
            ("STUB_RUNS", runs.to_str().unwrap()),
            ("STUB_HISTORY", history.to_str().unwrap()),
        ];
        all.extend_from_slice(env);
        self.run_seat(args, "ok", passes, &all)
    }

    /// The prompt turn `n` (from 1) was handed.
    fn stdin(&self, n: usize) -> String {
        std::fs::read_to_string(self.root.join(format!("stdin.{n}")))
            .unwrap_or_else(|_| panic!("turn {n} ran: {}", self.log()))
    }

    fn pulls(&self) -> Vec<String> {
        self.lines("boss ")
    }

    /// Each heartbeat's `factory_turn.pull.outcome`, in order.
    fn pull_outcomes(&self) -> Vec<String> {
        self.heartbeats()
            .iter()
            .filter_map(|b| {
                b["factory_turn"]["pull"]["outcome"]
                    .as_str()
                    .map(str::to_string)
            })
            .collect()
    }
}

/// The item's own case: a free slot pulls through the station door, the
/// turn is handed the seat's prompt with the pulled brief after it, and
/// while the record shows the pulled run's building open the next pass
/// finishes it through the seat's own prompt and pulls NOTHING more.
#[test]
fn a_pull_seat_hands_its_executor_the_step_its_station_pull_took() {
    let f = Fixture::puller("pull-hands", "next");
    let (rc, out) = f.pull("brief brief", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    let pulls = f.pulls();
    assert_eq!(
        pulls,
        ["boss dispatch --next"],
        "one pull, then a held slot: {}",
        f.log()
    );
    assert_eq!(f.turns().len(), 2, "{}", f.log());
    let first = f.stdin(1);
    assert!(
        first.starts_with("one factory turn\n"),
        "the seat's prompt first: {first}"
    );
    assert!(
        first.contains("build the pulled thing"),
        "then the pulled brief: {first}"
    );
    assert!(
        first.contains(&format!("Your run is agent-run {PULLED_RUN}")),
        "{first}"
    );
    assert_eq!(
        f.stdin(2),
        "one factory turn\n",
        "a held slot runs the seat's own prompt"
    );
    assert_eq!(f.sleeps(), ["sleep 45"], "{}", f.log());
    assert_eq!(
        f.pull_outcomes(),
        ["pulled", "pulled", "held", "held"],
        "each turn's two beats name the pass's pull outcome: {}",
        f.log()
    );
    for beat in [0, 2] {
        let pull = &f.heartbeats()[beat]["factory_turn"]["pull"];
        assert_eq!(pull["run"], PULLED_RUN, "the record names the run: {pull}");
    }
}

/// Restart recovery is the record's: a loop started again reads the run
/// its seat holds, finishes it, and pulls again only once its building
/// step has ended.
#[test]
fn a_restarted_pull_seat_reads_the_run_it_holds_off_the_record() {
    let f = Fixture::puller("pull-restart", "next");
    for _ in 0..3 {
        let (rc, out) = f.pull("brief empty", 1, &[("STUB_END_ON_TURN", "2")]);
        assert_eq!(rc, 0, "{out}");
    }
    assert_eq!(
        f.pulls(),
        ["boss dispatch --next", "boss dispatch --next"],
        "pulled, held across the restart, ended by its second turn, pulled: {}",
        f.log()
    );
    assert_eq!(
        f.turns().len(),
        2,
        "the second pull found nothing to run: {}",
        f.log()
    );
}

/// Review 0d65ef4c scenario C: the loop cannot write beside its log (a
/// full or read-only volume; a directory stands where the old slot file
/// was). Nothing the loop writes is the guard, so the run the record
/// shows open still holds the slot: one pull in three passes.
#[test]
fn a_seat_that_cannot_write_beside_its_log_still_pulls_once() {
    let f = Fixture::puller("pull-c", "next");
    create_dir(&f.root.join("home/factory-loop.log.slot"));
    let (rc, out) = f.pull("brief brief brief", 3, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "{}", f.log());
    assert_eq!(f.turns().len(), 3, "{}", f.log());
    assert_eq!(
        f.pull_outcomes(),
        ["pulled", "pulled", "held", "held", "held", "held"]
    );
}

/// Review 0d65ef4c scenario B: a brief that words its run line otherwise
/// than the loop expects. The run is read off the record, not the prose,
/// so the slot still holds it and the beat still names it.
#[test]
fn a_brief_that_words_its_run_otherwise_still_holds_the_slot() {
    let f = Fixture::puller("pull-b", "next");
    let (rc, out) = f.pull("reworded reworded reworded", 3, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "{}", f.log());
    assert_eq!(f.turns().len(), 3, "{}", f.log());
    let pull = &f.heartbeats()[0]["factory_turn"]["pull"];
    assert_eq!(pull["run"], PULLED_RUN, "{pull}");
}

/// Review 0d65ef4c scenario F: the loop is started again with nothing it
/// kept — another log, as after a kill -9 between the pull and any
/// write, or a restart under another FACTORY_LOG or HOME. The record
/// still shows the first run's building open, so nothing more is pulled.
#[test]
fn a_seat_started_again_with_nothing_kept_pulls_nothing_while_its_run_builds() {
    let f = Fixture::puller("pull-f", "next");
    let first = f.root.join("home/first.log");
    let second = f.root.join("home/second.log");
    let (rc, out) = f.pull(
        "brief brief",
        1,
        &[("FACTORY_LOG", first.to_str().unwrap())],
    );
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = f.pull(
        "brief brief",
        1,
        &[("FACTORY_LOG", second.to_str().unwrap())],
    );
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "{}", f.log());
    assert_eq!(f.turns().len(), 2, "{}", f.log());
    assert_eq!(f.stdin(2), "one factory turn\n", "{}", f.log());
}

/// Whose run holds the slot: any open run of the seat's own AGENT — its
/// building placed on the registered id, or signed by its login while
/// its building is still pending and placed on nobody — including one it
/// never pulled (its executor's own claim, N1). Another agent's open run
/// does not, nor does the agent's own run once its building has ended.
#[test]
fn the_slot_is_held_by_the_seats_own_agent_and_no_other() {
    let cases: [(&str, String, usize); 4] = [
        (
            "another agent's open building",
            run_row(
                "r-other",
                "active",
                "claude@algedonic.dev",
                Some("agent-claude"),
            ),
            1,
        ),
        (
            "the agent's registered id holds a building a coordinator opened",
            run_row(
                "r-held",
                "active",
                "claude@algedonic.dev",
                Some("agent-codex"),
            ),
            0,
        ),
        (
            "the agent's login opened a run whose building is pending, unplaced",
            run_row("r-pending", "pending", "codex@algedonic.dev", None),
            0,
        ),
        (
            "the agent's own run whose building has ended",
            run_row(
                "r-done",
                "completed",
                "codex@algedonic.dev",
                Some("agent-codex"),
            ),
            1,
        ),
    ];
    for (i, (case, row, want)) in cases.iter().enumerate() {
        let f = Fixture::puller(&format!("pull-whose-{i}"), "next");
        f.seed_run(row);
        let (rc, out) = f.pull("empty", 1, &[]);
        assert_eq!(rc, 0, "{case}: {out}");
        assert_eq!(f.pulls().len(), *want, "{case}: {}", f.log());
    }
}

/// No admissible work is not a turn: no model call, the seat's pause,
/// then the next pull — and nothing is noted, because nothing failed.
#[test]
fn a_pull_seat_with_no_admissible_work_runs_no_turn() {
    let f = Fixture::puller("pull-empty", "next");
    let (rc, out) = f.pull("empty empty", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 2, "{}", f.log());
    assert!(f.turns().is_empty(), "{}", f.log());
    assert_eq!(f.sleeps(), ["sleep 45"], "{}", f.log());
    assert!(f.notes().is_empty(), "{}", f.log());
    assert_eq!(f.pull_outcomes(), ["empty", "empty"], "{}", f.log());
}

/// A refused pull is the door's answer, kept whole and noted, on the
/// ladder a failed turn climbs — never a turn of the standing prompt
/// sent to find work some other way. The next pull is the same door.
#[test]
fn a_refused_station_pull_is_noted_backed_off_and_never_worked_around() {
    let f = Fixture::puller("pull-refused", "next");
    let (rc, out) = f.pull("refused refused brief", 3, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 3, "{}", f.log());
    assert_eq!(
        f.turns().len(),
        1,
        "only the pull that answered ran a turn: {}",
        f.log()
    );
    assert_eq!(f.sleeps(), ["sleep 120", "sleep 300"], "{}", f.log());
    let notes = f.notes();
    assert_eq!(notes.len(), 2, "{}", f.log());
    for n in &notes {
        let (_, note) = n.as_object().unwrap().iter().next().unwrap();
        let reason = note["reason"].as_str().unwrap_or_default();
        assert!(reason.starts_with("station pull refused"), "{note}");
        assert!(
            reason.contains("the native owner is unavailable"),
            "the door's own words: {note}"
        );
    }
    let log = std::fs::read_to_string(f.loop_log()).unwrap_or_default();
    assert!(
        log.contains("Error: boss dispatch --next: station selection refused"),
        "the refusal is kept whole in the log: {log}"
    );
    assert_eq!(
        f.pull_outcomes()[..2],
        ["refused", "refused"],
        "{}",
        f.log()
    );
}

/// A slot the record cannot judge is not judged free: a registry or run
/// listing that cannot be read, a denied listing, a listing cut short of
/// its total, or an actor no agent row names. Nothing is pulled and
/// nothing is run, because a second claim on a guess is the double claim
/// the slot exists to stop — and the seat says so on its work-session,
/// on the failure ladder, so a seat that cannot judge looks troubled
/// rather than idle (review 0d65ef4c N2).
#[test]
fn a_slot_the_record_cannot_judge_pulls_nothing_and_says_so() {
    let cases: [(&str, (&str, &str)); 4] = [
        ("the registry is dark", ("STUB_AGENTS_DOWN", "1")),
        ("the run listing is refused", ("STUB_RUNS_DOWN", "1")),
        ("the run listing is cut short", ("STUB_RUNS_CUT", "1")),
        (
            "no agent row names the actor",
            ("FACTORY_SESSION_ACTOR", "agent-nobody"),
        ),
    ];
    for (i, (case, env)) in cases.iter().enumerate() {
        let f = Fixture::puller(&format!("pull-unknown-{i}"), "next");
        let (rc, out) = f.pull("brief", 2, &[*env]);
        assert_eq!(rc, 0, "{case}: {out}");
        assert!(f.pulls().is_empty(), "{case}: {}", f.log());
        assert!(f.turns().is_empty(), "{case}: {}", f.log());
        assert_eq!(f.sleeps(), ["sleep 120"], "{case}: {}", f.log());
        assert_eq!(
            f.pull_outcomes(),
            ["unknown", "unknown"],
            "{case}: {}",
            f.log()
        );
        let notes = f.notes();
        assert_eq!(notes.len(), 2, "{case}: {}", f.log());
        let (_, note) = notes[0].as_object().unwrap().iter().next().unwrap();
        assert!(
            note["reason"]
                .as_str()
                .unwrap_or_default()
                .starts_with("seat slot not judged"),
            "{case}: {note}"
        );
    }
}

/// Each restart note's `reason`, in order.
fn note_reasons(f: &Fixture) -> Vec<String> {
    f.notes()
        .iter()
        .map(|n| {
            let (_, note) = n.as_object().unwrap().iter().next().unwrap();
            note["reason"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

/// Review f8cbaa0e B3, scenario L: the run listing answers 200 with
/// nothing in it — what a denied read scope answers (jobs.rs list_jobs:
/// "Denied -> return an empty collection") and what a wrong or dark
/// instance answers. An empty listing is not a free slot until a control
/// on the same door shows it can see the runs this seat signs; this one
/// cannot, so nothing is pulled, ever, and each pass says why.
#[test]
fn a_listing_that_cannot_see_the_seats_runs_pulls_nothing() {
    let f = Fixture::puller("pull-l", "next");
    let (rc, out) = f.pull("brief brief brief", 3, &[("STUB_BLIND", "1")]);
    assert_eq!(rc, 0, "{out}");
    assert!(
        f.pulls().is_empty(),
        "a blind read is no free slot: {}",
        f.log()
    );
    assert!(f.turns().is_empty(), "{}", f.log());
    assert_eq!(
        f.pull_outcomes(),
        ["unknown", "unknown", "unknown"],
        "{}",
        f.log()
    );
    assert_eq!(f.sleeps(), ["sleep 120", "sleep 300"], "{}", f.log());
    for reason in note_reasons(&f) {
        assert!(reason.starts_with("seat slot not judged"), "{reason}");
        assert!(
            reason.contains("codex@algedonic.dev"),
            "names the identity it could not see: {reason}"
        );
    }
}

/// The control's other face: an agent with no run of any status on the
/// record cannot show its read sees its runs, so it pulls nothing — its
/// first run is a hand's `boss dispatch`, never the loop's guess.
#[test]
fn an_agent_with_no_run_on_the_record_cannot_prove_its_read_and_pulls_nothing() {
    let f = Fixture::puller("pull-fresh", "next");
    f.fresh_agent();
    let (rc, out) = f.pull("brief", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.pulls().is_empty(), "{}", f.log());
    assert_eq!(f.pull_outcomes(), ["unknown", "unknown"], "{}", f.log());
}

/// Review f8cbaa0e B3's repair: a pull whose run the record does not
/// read back HOLDS the seat — no further pull, on the failure ladder —
/// until the record shows it, and that belief outlives a restart beside
/// the log, where it can only refuse a pull. The run read back while it
/// builds holds the slot as any run does; once it has ended, the record
/// shows a newer run than the one the pull was judged against, and the
/// seat pulls again.
#[test]
fn a_pull_whose_run_is_not_read_back_holds_the_seat_until_the_record_shows_it() {
    let f = Fixture::puller("pull-latch", "next");
    let hidden = [("STUB_HIDE_NEW", "1")];
    let (rc, out) = f.pull("brief brief brief brief brief", 3, &hidden);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(
        f.pulls().len(),
        1,
        "one pull, then the seat holds: {}",
        f.log()
    );
    assert_eq!(f.turns().len(), 1, "only the pulled step ran: {}", f.log());
    assert_eq!(
        f.pull_outcomes(),
        ["pulled", "pulled", "unknown", "unknown"],
        "{}",
        f.log()
    );
    for reason in note_reasons(&f) {
        assert!(
            reason.contains("not read back"),
            "the note names the unseen pull: {reason}"
        );
    }

    // A restart under the same log keeps the belief: still one pull.
    let (rc, out) = f.pull("brief brief", 1, &hidden);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "a restart does not forget: {}", f.log());

    // The record shows the run building: held, and the belief clears.
    let (rc, out) = f.pull("brief brief", 1, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "{}", f.log());
    assert_eq!(f.stdin(2), "one factory turn\n", "{}", f.log());

    // Its building ended: the record shows a newer run, so the seat pulls.
    let runs = std::fs::read_to_string(f.runs()).unwrap();
    std::fs::write(
        f.runs(),
        runs.replace(
            r#""building","status":"active""#,
            r#""building","status":"completed""#,
        ),
    )
    .unwrap();
    let (rc, out) = f.pull("empty", 1, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 2, "{}", f.log());
}

/// Review f8cbaa0e B3, scenario M: the loop is started by hand with
/// BOSS_ACTOR blank, so `boss` signs as the actor file. The loop names the
/// identity exactly as `boss` resolves it and hands it to the pull; an
/// identity that is not a name of the seat's own agent pulls nothing,
/// because the run it opened would be another agent's; and with nothing
/// naming one the door is never called.
#[test]
fn a_seat_pulls_only_as_the_identity_boss_signs_and_only_its_own_agent() {
    // The actor file names another agent: nothing is pulled.
    let other = Fixture::puller("pull-m-other", "next");
    other.actor_file("claude@algedonic.dev");
    let (rc, out) = other.pull("brief brief brief", 3, &[("BOSS_ACTOR", "")]);
    assert_eq!(rc, 0, "{out}");
    assert!(other.pulls().is_empty(), "{}", other.log());
    assert_eq!(
        other.pull_outcomes(),
        ["unknown", "unknown", "unknown"],
        "{}",
        other.log()
    );
    let reasons = note_reasons(&other);
    assert!(
        reasons[0].contains("claude@algedonic.dev") && reasons[0].contains("agent-codex"),
        "names the identity and the seat's agent: {reasons:?}"
    );

    // The actor file names the seat's own login: one pull, then held.
    let own = Fixture::puller("pull-m-own", "next");
    own.actor_file("codex@algedonic.dev");
    let (rc, out) = own.pull("brief brief brief", 3, &[("BOSS_ACTOR", "")]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(own.pulls().len(), 1, "{}", own.log());
    assert_eq!(own.lines("signed "), ["signed codex@algedonic.dev"]);

    // Nothing names an identity: the door is never called.
    let nobody = Fixture::puller("pull-m-nobody", "next");
    let (rc, out) = nobody.pull("brief", 2, &[("BOSS_ACTOR", " ")]);
    assert_eq!(rc, 0, "{out}");
    assert!(nobody.pulls().is_empty(), "{}", nobody.log());
    assert_eq!(
        nobody.pull_outcomes(),
        ["unknown", "unknown"],
        "{}",
        nobody.log()
    );
}

/// Review f8cbaa0e N7: a brief the loop cannot keep beside its log is the
/// loop's own trouble, not the station door's refusal. Nothing is
/// pulled (the door is never called) and the log does not blame it.
#[test]
fn a_brief_that_cannot_be_kept_is_not_called_the_doors_refusal() {
    let f = Fixture::puller("pull-n7", "next");
    create_dir(&f.root.join("home/factory-loop.log.pulled"));
    let (rc, out) = f.pull("brief", 1, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.pulls().is_empty(), "{}", f.log());
    assert_eq!(f.pull_outcomes(), ["unknown"], "{}", f.log());
    let log = std::fs::read_to_string(f.loop_log()).unwrap_or_default();
    assert!(!log.contains("REFUSED"), "{log}");
    assert!(!log.contains("No such file"), "{log}");
}

/// Review 2d18db68 N10 (backlog a569e6b2): the turn prompt is the third
/// place a pull writes beside its log, and the only one the pre-check
/// did not cover, so with it unwritable the pull claimed a step and its
/// turn was handed nothing — the claimed brief never reached an
/// executor. Unwritable before the pull, the door is never called.
#[test]
fn a_turn_prompt_that_cannot_be_written_pulls_nothing() {
    let f = Fixture::puller("pull-n10", "next");
    create_dir(&f.root.join("home/factory-loop.log.prompt"));
    let (rc, out) = f.pull("brief", 1, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.pulls().is_empty(), "{}", f.log());
    assert!(f.turns().is_empty(), "{}", f.log());
    assert_eq!(f.pull_outcomes(), ["unknown"], "{}", f.log());
}

/// Review 2d18db68 N10, the write itself: the turn prompt stops being
/// writable between the check and the write, after the station door
/// has claimed the step. The pass runs no turn on an empty prompt; it
/// says the step is claimed and where its brief is kept, and the run
/// holds the slot, so the next pass is that run's turn under the seat's
/// prompt and nothing more is pulled.
#[test]
fn a_claimed_brief_the_loop_cannot_write_is_named_and_never_run_empty() {
    let f = Fixture::puller("pull-n10-write", "next");
    let (rc, out) = f.pull("brief brief", 2, &[("STUB_BREAK_PROMPT", "1")]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls().len(), 1, "{}", f.log());
    let loop_log = std::fs::read_to_string(f.loop_log()).unwrap_or_default();
    assert_eq!(f.turns().len(), 1, "only the held pass ran: {loop_log}");
    assert_eq!(f.stdin(1), "one factory turn\n", "{}", f.log());
    let reasons = note_reasons(&f);
    assert!(
        reasons
            .iter()
            .any(|r| r.contains(PULLED_RUN) && r.contains("factory-loop.log.pulled")),
        "the note names the claimed run and the kept brief: {reasons:?}"
    );
}

/// Review 2d18db68 N11 (backlog a569e6b2, scenario U2): the kept belief
/// that a pull was not read back is a file beside the log, and one whose
/// contents are not a run id was compared as if it were one: it never
/// matched the control, so the hold dropped and the seat pulled again.
/// Anything there but a run id is '?', which only a run of the seat's
/// agent seen building clears.
#[test]
fn a_corrupt_unseen_file_keeps_the_seat_holding() {
    let f = Fixture::puller("pull-n11", "next");
    create_dir(&f.root.join("home"));
    std::fs::write(
        f.root.join("home/factory-loop.log.unseen"),
        "\u{1}garbled\n",
    )
    .expect("write unseen");
    let (rc, out) = f.pull("brief brief", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.pulls().is_empty(), "{}", f.log());
    assert_eq!(f.pull_outcomes(), ["unknown", "unknown"], "{}", f.log());
}

/// Review 0d65ef4c B2: a loop started with no seat IS the list's first
/// row — every column of it, the pull included — so a first row that
/// says `next` pulls, rather than the loop answering from defaults of
/// its own that the row could drift from. Its log keeps the bare name
/// option 6 has always written.
#[test]
fn a_loop_started_without_a_seat_is_the_first_row_of_its_list() {
    let f = Fixture::puller("pull-first-row", "next");
    let (rc, out) = f.pull_as(&[], "brief", 1, &[]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(f.pulls(), ["boss dispatch --next"], "{}", f.log());
    assert!(f.stdin(1).contains("build the pulled thing"), "{}", f.log());
    assert!(f.loop_log().exists(), "the bare log name: {}", f.log());
    let beat = &f.heartbeats()[0]["factory_turn"];
    assert_eq!(beat["seat"], "puller", "{beat}");

    let empty = Fixture::puller("pull-no-list", "next");
    std::fs::write(empty.root.join("seats.tsv"), "# no seats\n").unwrap();
    let (rc, out) = empty.pull_as(&[], "brief", 1, &[]);
    assert_eq!(rc, 64, "a list with no row is refused: {out}");
    assert!(empty.turns().is_empty(), "{}", empty.log());
}

/// A seat without `next` never pulls: today's standing-prompt turns,
/// exactly — the column's `-`.
#[test]
fn a_seat_that_does_not_pull_never_calls_the_station_door() {
    let f = Fixture::puller("pull-off", "-");
    let (rc, out) = f.pull("brief", 2, &[]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.pulls().is_empty(), "{}", f.log());
    assert_eq!(f.turns().len(), 2, "{}", f.log());
    assert_eq!(f.stdin(1), "one factory turn\n");
    assert!(f.pull_outcomes().is_empty(), "{}", f.log());
}

/// A pull value the loop does not define is refused before anything
/// runs, rather than read as on or off.
#[test]
fn a_pull_the_loop_does_not_define_is_refused() {
    let f = Fixture::puller("pull-bad", "maybe");
    let (rc, out) = f.pull("brief", 1, &[]);
    assert_eq!(rc, 64, "{out}");
    assert!(out.contains("pull 'maybe'"), "{out}");
    assert!(f.pulls().is_empty() && f.turns().is_empty(), "{}", f.log());
}

/// Every seat declares its pull, and none pulls yet: turning one on
/// changes who claims work unattended (trust boundary, mutating-verb),
/// so it is a reviewed car that edits the row AND this test. The message
/// names what that car accepts with it (review 2d18db68, backlog
/// a569e6b2): N1 and N6 below, and N12, the visibility assumption the
/// loop's header states.
#[test]
fn every_seat_declares_its_pull_and_none_pulls_until_a_reviewed_car_turns_it_on() {
    let seats = std::fs::read_to_string(repo_root().join("infra/dev/seats.tsv")).expect("seats");
    let rows: Vec<Vec<&str>> = seats
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert!(!rows.is_empty());
    for r in rows {
        assert_eq!(
            r.len(),
            7,
            "a seat row has seven columns, the last its pull: {r:?}"
        );
        assert_eq!(
            r[6], "-",
            "seat {} pulls before its activation was reviewed; the car that turns it on accepts, \
             by name: a held seat's turn runs the seat's standing prompt, not the held run's \
             brief (N1); a run of the same agent opened by any other process holds the slot \
             (N6); and the unseen hold assumes every agent-run is equally visible to the \
             seat's reads (N12, factory-loop.sh's header)",
            r[0]
        );
    }
}
