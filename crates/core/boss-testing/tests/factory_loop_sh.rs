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

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

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
        let mut cmd = Command::new(repo_root().join(SCRIPT));
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
    ] {
        assert!(
            never.contains(needle),
            "the Never section must carry `{needle}`: a capacity refusal is a queue, not a \
             refusal to file and stop on:\n{never}"
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
