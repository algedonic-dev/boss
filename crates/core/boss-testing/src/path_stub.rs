//! A STUB BIN DIRECTORY FIRST ON PATH, FOR A SHELL VERB UNDER TEST — one
//! copy for every test that runs a real script against stub binaries and
//! decides, as the fixture, which of its calls are ever answered
//! (backlog 59f4b65d).
//!
//! WHY. The wall-clock family (backlog 9bff920d) kept losing cases to a
//! stalled gate drive: an answered call raced the deadline under test,
//! and on a 280 s io.full stall (gate 3082de78) a budget of any size is
//! the wrong shape. Backlog dac8cf01 changed the shape twice, in
//! `every_converge_tick_observes_the_site.rs` (stub `kubectl`, `curl`,
//! `date`, `timeout`) and `a_step_close_has_a_deadline.rs` (stub
//! `curl`), and each file spelled the same fixture by hand: a stub
//! directory first on PATH, every call logged to `STUB_LOG`, the real
//! binary found again through a saved `STUB_REAL_PATH`, and the
//! answered-versus-never-answered decision. Two spellings of one
//! fixture, with more shell-verb tests of that family to come, is
//! CLAUDE.md §9a's pair; this module is the one spelling, and
//! `no_test_spells_the_handoff_itself` below holds it to that.
//!
//! WHAT IT OWNS, and the shape the two sites share:
//!
//! * [`PathStub::new`] makes `<dir>/bin`, and [`PathStub::wire`] puts it
//!   FIRST on a command's PATH, hands the PATH it displaced to the stubs
//!   as `STUB_REAL_PATH`, and names the call log `STUB_LOG`;
//!   [`PathStub::calls`] reads that log back, one line per call, argv as
//!   sent.
//! * Every stub [`PathStub::stub`] writes starts with [`PRELUDE`], the
//!   shell half: `stub_log` (log this call, and number it among this
//!   binary's calls), `stub_real` (the real binary of a name),
//!   `stub_never_answered` (is this a call the fixture never answers?)
//!   and `stub_hang` (never answer it — but only inside a bound).
//! * A call is NEVER ANSWERED when its argv contains `STUB_HANG`, or
//!   when `STUB_ANSWERED` is set and this binary has already been called
//!   that many times. Every other call is answered.
//! * The two delegations a deadline test needs, for a named binary:
//!   [`CURL_UNBOUNDED_WHEN_ANSWERED`] and
//!   [`TIMEOUT_UNBOUNDED_WHEN_ANSWERED`]. An answered call runs with its
//!   bound taken off, so no answer can race it; the never-answered call
//!   goes to the REAL binary with its bound as sent, where only that
//!   bound can end it. A never-answered call that carries no bound fails
//!   at once, in words, rather than hanging the test.
//! * [`PathStub::with_clock`]: a `date` whose `+%s` reads a fixture clock
//!   the stubs advance (`stub_clock_tick`), every other form the real
//!   `date`, so a deadline the script reads off its own clock is judged
//!   on the fixture's time, never the wall's.
//!
//! What a test's stubs ANSWER — the kubectl case table, the site's
//! pages, a stall on every answered call — stays in the test. Only the
//! wiring and the decision live here.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The shell every stub starts with (module note). `stub_log` keeps the
/// count of this binary's calls in a file beside the log rather than
/// counting log lines, because a stub's own notes (`stub_hang`'s) are
/// log lines too and must not shift which call is the never-answered
/// one.
pub const PRELUDE: &str = r#"#!/usr/bin/env bash
# boss_testing::path_stub — the shared half of this stub.
stub_name=${0##*/}
stub_log() {
  local count="$STUB_LOG.$stub_name.n"
  STUB_CALL=$(( $(cat "$count" 2>/dev/null || echo 0) + 1 ))
  printf '%s\n' "$STUB_CALL" > "$count"
  printf '%s %s\n' "$stub_name" "$*" >> "$STUB_LOG"
}
stub_real() { PATH="$STUB_REAL_PATH" command -v "$1"; }
stub_never_answered() {
  if [ -n "${STUB_HANG:-}" ] && [[ "$*" == *"$STUB_HANG"* ]]; then return 0; fi
  [ -n "${STUB_ANSWERED:-}" ] && [ "${STUB_CALL:-0}" -gt "$STUB_ANSWERED" ]
}
stub_hang() {
  if [ -z "${STUB_BOUNDED:-}" ]; then
    printf '%s HUNG WITH NOTHING BOUNDING IT\n' "$stub_name" >> "$STUB_LOG"
    printf 'stub %s: this read never answers, and nothing bounds it\n' "$stub_name" >&2
    exit 1
  fi
  printf '%s hangs; the bound ends it\n' "$stub_name" >> "$STUB_LOG"
  exec sleep 86400
}
stub_clock_tick() { printf '%s\n' "$(( $(cat "$STUB_CLOCK") + $1 ))" > "$STUB_CLOCK"; }
"#;

/// A stub `curl`: logs the call; an ANSWERED call goes to the real curl
/// without `--max-time` and `--connect-timeout`; the never-answered call
/// goes exactly as sent — or, carrying no `--max-time`, nowhere: it
/// would wait forever, so it fails (exit 99), naming why.
pub const CURL_UNBOUNDED_WHEN_ANSWERED: &str = r#"stub_log "$@"
real=$(stub_real curl)
if ! stub_never_answered "$@"; then
  args=() drop=0
  for a in "$@"; do
    if [ "$drop" = 1 ]; then drop=0; continue; fi
    case "$a" in --max-time | --connect-timeout) drop=1; continue ;; esac
    args+=("$a")
  done
  exec "$real" "${args[@]}"
fi
case " $* " in *" --max-time "*) exec "$real" "$@" ;; esac
echo 'stub curl: a call the fixture never answers carries no --max-time, so nothing would end it' >&2
exit 99
"#;

/// A stub `timeout`: logs the call; an ANSWERED call runs its command
/// unbounded (the duration and `-k`/`-s` options dropped); the
/// never-answered call goes to the REAL `timeout` with the script's own
/// arguments, and exports `STUB_BOUNDED` so a stub that `stub_hang`s
/// inside it knows a bound is around it.
pub const TIMEOUT_UNBOUNDED_WHEN_ANSWERED: &str = r#"stub_log "$@"
if stub_never_answered "$@"; then
  export STUB_BOUNDED="$*"
  exec "$(stub_real timeout)" "$@"
fi
while [ $# -gt 0 ]; do
  case "$1" in
    -k | -s) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
shift
exec "$@"
"#;

/// The stub `date` [`PathStub::with_clock`] writes: `+%s` answers the
/// fixture clock, every other form is the real `date`'s. It logs
/// nothing: reading the clock is not a call a test asserts on.
const DATE_ON_THE_FIXTURE_CLOCK: &str = r#"if [ "$*" = "+%s" ]; then cat "$STUB_CLOCK"; exit 0; fi
exec "$(stub_real date)" "$@"
"#;

/// A stub bin directory and its call log (module note).
#[derive(Debug, Clone)]
pub struct PathStub {
    dir: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    clock: Option<PathBuf>,
}

impl PathStub {
    /// `<dir>/bin`, created, for the stubs; `<dir>/calls` for their log.
    pub fn new(dir: &Path) -> Self {
        let bin = dir.join("bin");
        crate::create_dir(&bin);
        Self {
            dir: dir.to_path_buf(),
            bin,
            log: dir.join("calls"),
            clock: None,
        }
    }

    /// The same stubs with a fixture clock reading `start`, at
    /// `<dir>/clock`, and a stub `date` that answers `+%s` from it.
    pub fn with_clock(self, start: u64) -> Self {
        let clock = self.dir.join("clock");
        crate::write_file(&clock, &format!("{start}\n"));
        self.stub("date", DATE_ON_THE_FIXTURE_CLOCK);
        Self {
            clock: Some(clock),
            ..self
        }
    }

    /// The directory the stubs are in.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Write the stub `name`: [`PRELUDE`], then `body`. Returns its path.
    pub fn stub(&self, name: &str, body: &str) -> PathBuf {
        let path = self.bin.join(name);
        crate::write_exec(&path, &format!("{PRELUDE}{body}"));
        path
    }

    /// The stubs first on `cmd`'s PATH, the PATH they displaced handed to
    /// them as `STUB_REAL_PATH`, and the log as `STUB_LOG`. The selectors
    /// a stub reads (`STUB_HANG`, `STUB_ANSWERED`) and the bound marker
    /// (`STUB_BOUNDED`) are cleared, so the suite's own shell cannot
    /// decide a call for the test: set the selectors on `cmd` after this.
    pub fn wire<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let real_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{real_path}", self.bin.display()))
            .env("STUB_REAL_PATH", &real_path)
            .env("STUB_LOG", &self.log)
            .env_remove("STUB_HANG")
            .env_remove("STUB_ANSWERED")
            .env_remove("STUB_BOUNDED");
        if let Some(clock) = &self.clock {
            cmd.env("STUB_CLOCK", clock);
        }
        cmd
    }

    /// Every call the stubs logged, one line each, argv as sent; empty
    /// when none was made.
    pub fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision, run: with `STUB_ANSWERED=2` the first two calls of a
    /// binary are answered and the third is not; `STUB_HANG` selects by
    /// argv whatever the order; and each call is logged as sent.
    #[test]
    fn a_call_is_never_answered_by_order_or_by_argv() {
        let dir = crate::scratch_dir("path-stub-decision");
        let stubs = PathStub::new(&dir);
        stubs.stub(
            "probe",
            "stub_log \"$@\"\nif stub_never_answered \"$@\"; then echo never; else echo answered; fi\n",
        );
        let run = |args: &[&str], env: &[(&str, &str)]| {
            let mut cmd = Command::new(stubs.bin().join("probe"));
            stubs.wire(&mut cmd).args(args).envs(env.iter().copied());
            let out = cmd.output().expect("run the stub");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let by_order: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|a| run(&[a], &[("STUB_ANSWERED", "2")]))
            .collect();
        assert_eq!(by_order, ["answered", "answered", "never"]);
        assert_eq!(run(&["x", "hang-me"], &[("STUB_HANG", "hang-me")]), "never");
        assert_eq!(run(&["x"], &[("STUB_HANG", "hang-me")]), "answered");
        assert_eq!(
            stubs.calls(),
            "probe a\nprobe b\nprobe c\nprobe x hang-me\nprobe x\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The handoff and the bound marker are spelled here and in no test:
    /// a test that writes its own `STUB_REAL_PATH` delegation, or its own
    /// `STUB_BOUNDED` handshake, is the second copy this module exists to
    /// retire (backlog 59f4b65d; CLAUDE.md §9a).
    ///
    /// The selectors are READ here and nowhere else (backlog b4ffc59a). A
    /// test SETS `STUB_HANG` or `STUB_ANSWERED` on a wired command — that
    /// is this module's interface, `PathStub::wire` — but a stub whose own
    /// shell reads `$STUB_HANG` is deciding a call by hand:
    /// a_killed_sender_leaves_no_header_behind.rs did, with a meaning of
    /// its own, and passed this pin because it never spelled the handoff.
    ///
    /// Every `.rs` under crates/, not boss-testing's tests alone: the first
    /// scan read only that directory, so a hand copy in any other crate's
    /// tests passed it (backlog b4ffc59a).
    // tree-wide pin — it walks crates/, whose files no changed-file map
    // attributes to this crate, so every scoped gate runs it
    // (`tree_wide_pins` in infra/gate.sh; backlog b4ffc59a).
    #[test]
    fn no_test_spells_the_handoff_itself() {
        const OWNED: [&str; 6] = [
            "STUB_REAL_PATH",
            "STUB_BOUNDED",
            "$STUB_HANG",
            "${STUB_HANG",
            "$STUB_ANSWERED",
            "${STUB_ANSWERED",
        ];
        fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
            let entries = std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("read dir {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    rust_files(&path, out);
                } else if path.extension().is_some_and(|x| x == "rs")
                    && !path.ends_with("boss-testing/src/path_stub.rs")
                {
                    out.push(path);
                }
            }
        }
        let crates = crate::repo_root().join("crates");
        let mut files = Vec::new();
        rust_files(&crates, &mut files);
        // boss-testing/tests alone held ~600 files on 2026-11-11 and
        // crates/ 2305, so this floor fails a scan that has shrunk back to
        // the one directory as loudly as one that reads nothing.
        assert!(
            files.len() > 1500,
            "the scan read {} files under {} — a scan that reads nothing passes everything",
            files.len(),
            crates.display()
        );
        let spelled: Vec<String> = files
            .iter()
            .flat_map(|f| {
                let text = std::fs::read_to_string(f)
                    .unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
                OWNED
                    .iter()
                    .filter(|name| text.contains(*name))
                    .map(|name| format!("{} spells {name}", f.display()))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            spelled.is_empty(),
            "the PATH-stub handoff lives once, in boss_testing::path_stub — build the stub \
             through PathStub instead: {spelled:#?}"
        );
    }
}
