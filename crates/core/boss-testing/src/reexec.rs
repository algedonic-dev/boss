//! Re-running this test binary as a child for one exact test, timing
//! only the child's WORK and never its start (backlog 7e30ddb0).
//!
//! WHY. A test that needs a process of its own — a clean environment, a
//! signal, a FIFO that may block an `open` forever — re-runs its own
//! binary: `current_exe() --exact <test>`, with an env var that sends the
//! child down the test's inner branch. The child is the WHOLE debug test
//! binary (boss-cli's: 340 MB on the dev pod, 2026-10-21), so before the
//! inner branch runs a line the host must exec it, page it in and start
//! libtest. On a gate pod
//! running the full suite beside two other gates that start alone has
//! missed a 30 s window: gate 99ece855 (prove.rs) and gate 5fc86d87
//! (diagnostic_adapter.rs) went red on 2026-10-19 on cars that did not
//! touch either file, and neither reproduced in ten runs alone.
//!
//! THE MECHANISM. The child calls [`ready`] as the first line of its
//! inner branch; that creates a file the parent named for it in
//! [`READY_ENV`]. The parent waits for that file for as long as the
//! child is alive — start-up is judged by liveness, not by a clock — and
//! starts the work bound only when it appears. So the bound still
//! catches the hang the test was written for (a door blocked on a FIFO,
//! a fetch that never comes), and a slow host only makes the test slow.
//! [`START_CEILING`] exists so a child wedged before it could say ready
//! still ends the test with a sentence rather than at the gate's own
//! limit; it is a ceiling on a wedge, twenty times the window the reds
//! missed, not a judgement of the work.
//!
//! A file and not a line on stdout, because several of these children
//! write stdout to a file of the caller's or clear their whole
//! environment; the variable is set when the child is spawned, after
//! the caller's `env_clear`.
//!
//! `boss-testing/tests/a_re_exec_times_its_work_not_its_start.rs` refuses
//! a function that re-execs its test binary and builds a clock of its
//! own beside it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The variable naming the file the child creates when it is running.
pub const READY_ENV: &str = "BOSS_REEXEC_READY_FILE";

/// How long a child may take to reach [`ready`] before it is called
/// wedged. Measured 2026-10-21 on the dev pod, page cache warm: boss-cli's
/// debug test binary starts and filters to nothing in 5 ms, and runs the
/// whole prove.rs child in 68 ms; the two reds above took more than 30 s
/// on a gate pod. Ten minutes is a wedge on any host that finishes a
/// gate, so this never judges a slow one.
pub const START_CEILING: Duration = Duration::from_secs(600);

/// How often the parent looks for the ready file and the child's exit.
const POLL: Duration = Duration::from_millis(20);

/// Child side: say the child is running. The first line of the inner
/// branch of a test re-run through [`ReExec`] or armed with
/// [`Ready::arm`]. Outside such a child (no [`READY_ENV`]) it does
/// nothing, so a test may call it unconditionally.
pub fn ready() {
    if let Some(path) = std::env::var_os(READY_ENV) {
        std::fs::write(&path, b"ready\n").unwrap_or_else(|e| {
            panic!(
                "the re-exec'd child could not say ready at {}: {e}",
                Path::new(&path).display()
            )
        });
    }
}

/// One process-wide sequence, so two children of one test never share
/// a ready file.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The parent's half of [`ready`]: a file the child will create.
#[derive(Debug, Clone)]
pub struct Ready {
    path: PathBuf,
}

impl Default for Ready {
    fn default() -> Self {
        Self::new()
    }
}

impl Ready {
    /// A fresh ready file in this process's own scratch root.
    pub fn new() -> Self {
        let n = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self::at(crate::scratch_dir("reexec-ready").join(format!("ready-{n}")))
    }

    /// A ready file at `path` — for a fixture that keeps each child's
    /// files together and finds a child's file again by its label.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Name the file to the child. Call it last before spawning, after
    /// any `env_clear`. Whatever is at the path is removed first, so a
    /// ready file an earlier child of the same label left is not read as
    /// this one's.
    pub fn arm<'c>(&self, command: &'c mut Command) -> &'c mut Command {
        if let Err(e) = std::fs::remove_file(&self.path) {
            assert!(
                e.kind() == std::io::ErrorKind::NotFound,
                "a stale ready file {} cannot be removed: {e}",
                self.path.display()
            );
        }
        command.env(READY_ENV, &self.path)
    }

    /// Has the child said ready?
    pub fn reached(&self) -> bool {
        self.path.exists()
    }

    /// Wait, blocking, until the child says ready (`Ok`, with how long its
    /// start took) or ends without saying so (`Err`, its status). Panics,
    /// killing it, past [`START_CEILING`].
    pub fn wait(&self, child: &mut Child) -> Result<Duration, ExitStatus> {
        let spawned = Instant::now();
        loop {
            if self.reached() {
                return Ok(spawned.elapsed());
            }
            if let Some(status) = child.try_wait().unwrap_or_else(|e| panic!("try_wait: {e}")) {
                // It may have said ready in the moment before it ended.
                return if self.reached() {
                    Ok(spawned.elapsed())
                } else {
                    Err(status)
                };
            }
            if spawned.elapsed() > START_CEILING {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{}", self.wedged());
            }
            std::thread::sleep(POLL);
        }
    }

    /// [`Ready::wait`] for a tokio child, sleeping on the runtime rather
    /// than blocking it — a fixture server on the same runtime keeps
    /// answering while the child starts.
    pub async fn wait_async(
        &self,
        child: &mut tokio::process::Child,
    ) -> Result<Duration, ExitStatus> {
        let spawned = Instant::now();
        loop {
            if self.reached() {
                return Ok(spawned.elapsed());
            }
            if let Some(status) = child.try_wait().unwrap_or_else(|e| panic!("try_wait: {e}")) {
                return if self.reached() {
                    Ok(spawned.elapsed())
                } else {
                    Err(status)
                };
            }
            if spawned.elapsed() > START_CEILING {
                let _ = child.kill().await;
                panic!("{}", self.wedged());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn wedged(&self) -> String {
        format!(
            "the re-exec'd child did not reach boss_testing::reexec::ready() within {START_CEILING:?} \
             ({} never appeared) — a start this long is a wedge, not a slow host; killed",
            self.path.display()
        )
    }
}

/// The parent's whole re-exec: `current_exe() --exact <test> --nocapture`,
/// stdout and stderr piped and read while it runs (a full pipe would
/// otherwise stop the child and read as a hang).
pub struct ReExec {
    command: Command,
    ready: Ready,
}

impl ReExec {
    /// Re-run `test` — its full path, as `--exact` takes it.
    pub fn new(test: &str) -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|e| panic!("this test's binary: {e}"));
        let mut command = Command::new(exe);
        command
            .args(["--exact", test, "--nocapture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self {
            command,
            ready: Ready::new(),
        }
    }

    /// The command, for its environment, extra arguments or stdio.
    pub fn command(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Spawn it, with the ready file named last.
    pub fn spawn(mut self) -> Running {
        self.ready.arm(&mut self.command);
        let mut child = self
            .command
            .spawn()
            .unwrap_or_else(|e| panic!("spawning this test's binary: {e}"));
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            pipe.map(|mut p| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = p.read_to_end(&mut bytes);
                    bytes
                })
            })
        };
        let stdout = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let stderr = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        Running {
            child,
            ready: self.ready,
            stdout,
            stderr,
        }
    }
}

/// A spawned re-exec.
pub struct Running {
    child: Child,
    ready: Ready,
    stdout: Option<JoinHandle<Vec<u8>>>,
    stderr: Option<JoinHandle<Vec<u8>>>,
}

/// What a re-exec did. `status` is `None` when the work overran its
/// bound and the child was killed.
#[derive(Debug)]
pub struct Finished {
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
    /// From spawn to `ready()` — the host's share, never judged.
    pub start: Duration,
    /// From `ready()` to the end — the share the bound judged.
    pub work: Duration,
}

impl Finished {
    /// Killed at its work bound.
    pub fn overran(&self) -> bool {
        self.status.is_none()
    }

    /// Exited zero.
    pub fn succeeded(&self) -> bool {
        self.status.is_some_and(|s| s.success())
    }
}

impl Running {
    /// The child, to signal it or read its pid.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Wait for the child to say ready; see [`Ready::wait`].
    pub fn ready(&mut self) -> Result<Duration, ExitStatus> {
        self.ready.wait(&mut self.child)
    }

    /// The child itself, for a caller that drives it past `ready()` (a
    /// signal, a stop) and gave it file stdio rather than pipes.
    pub fn into_child(self) -> Child {
        self.child
    }

    /// Wait for the child's start (unbounded but for [`START_CEILING`]),
    /// then give its work `work`, killing it if that runs out. A child
    /// that exits zero without ever calling [`ready`] is a wiring error
    /// and panics: its work was never timed at all.
    pub fn finish(mut self, work: Duration) -> Finished {
        let started = self.ready.wait(&mut self.child);
        let start = match started {
            Ok(start) => start,
            Err(status) => {
                let (stdout, stderr) = self.drained();
                assert!(
                    !status.success(),
                    "the re-exec'd child exited zero without calling \
                     boss_testing::reexec::ready() — its inner branch must call it first, \
                     or its work is never timed:\n{stdout}\n{stderr}"
                );
                return Finished {
                    status: Some(status),
                    stdout,
                    stderr,
                    start: Duration::ZERO,
                    work: Duration::ZERO,
                };
            }
        };
        let from = Instant::now();
        let status = loop {
            if let Some(status) = self
                .child
                .try_wait()
                .unwrap_or_else(|e| panic!("try_wait: {e}"))
            {
                break Some(status);
            }
            if from.elapsed() > work {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break None;
            }
            std::thread::sleep(POLL);
        };
        let worked = from.elapsed();
        let (stdout, stderr) = self.drained();
        Finished {
            status,
            stdout,
            stderr,
            start,
            work: worked,
        }
    }

    fn drained(&mut self) -> (String, String) {
        let join = |h: Option<JoinHandle<Vec<u8>>>| {
            h.map(|h| String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned())
                .unwrap_or_default()
        };
        (join(self.stdout.take()), join(self.stderr.take()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD: &str = "BOSS_REEXEC_SELF_TEST";

    /// The inner branch every case below re-runs: it sleeps `start` ms
    /// BEFORE saying ready — the host's share — and then `work` ms.
    /// `WORKING` is printed BEFORE `ready()`, so a case that kills the
    /// child soon after ready still finds it: printed after, a host that
    /// descheduled the child between the two lines for longer than the
    /// work bound would red correct code (review 12a4ee82, N2).
    #[test]
    fn child() {
        let Ok(plan) = std::env::var(CHILD) else {
            return;
        };
        let (start, work) = plan.split_once(',').unwrap();
        std::thread::sleep(Duration::from_millis(start.parse().unwrap()));
        if work != "never-ready" {
            println!("WORKING");
            ready();
            std::thread::sleep(Duration::from_millis(work.parse().unwrap()));
        }
        println!("DONE");
    }

    fn run(plan: &str, work: Duration) -> Finished {
        let mut re = ReExec::new("reexec::tests::child");
        re.command().env(CHILD, plan);
        re.spawn().finish(work)
    }

    /// THE DEFECT, by effect: a child whose START alone outlasts the
    /// work bound — what a loaded gate pod does to a large debug binary —
    /// is not a hang. Its start is reported, never judged. (Four seconds
    /// against three, the pair boss-jobs' flow-snapshot test used first:
    /// the three seconds after `ready()` are the margin a loaded host
    /// gets to let the child exit.)
    #[test]
    fn a_slow_start_is_not_judged_by_the_work_bound() {
        let done = run("4000,0", Duration::from_secs(3));
        assert!(done.succeeded(), "{done:?}");
        assert!(done.start >= Duration::from_secs(4), "{done:?}");
        assert!(done.stdout.contains("DONE"), "{done:?}");
    }

    /// And the bound is still a bound: work that outlasts it after
    /// `ready()` is killed and said so.
    #[test]
    fn work_past_its_bound_is_killed_after_ready() {
        let done = run("0,30000", Duration::from_millis(500));
        assert!(done.overran(), "{done:?}");
        assert!(done.work < Duration::from_secs(20), "{done:?}");
        assert!(done.stdout.contains("WORKING"), "{done:?}");
        assert!(!done.stdout.contains("DONE"), "{done:?}");
    }

    /// A child that never calls `ready()` and exits zero is miswired —
    /// its work was never timed — and is refused loudly.
    #[test]
    fn a_child_that_never_says_ready_is_refused() {
        let caught = std::panic::catch_unwind(|| run("0,never-ready", Duration::from_secs(5)));
        let Err(panic) = caught else {
            panic!("a child that never said ready passed");
        };
        let said = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(said.contains("without calling"), "{said}");
    }
}
