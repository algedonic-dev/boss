//! A child a test will signal starts with every signal at its default
//! action and none blocked — chosen here, never inherited from whatever
//! ran the suite.
//!
//! WHY THIS IS SHARED (backlog 5f6a2cde, after b43f68bb). GitHub's runner
//! hands its test processes an ignored or blocked INT or HUP: a
//! non-interactive bash gives an `&` job SIGINT ignored (POSIX), and
//! `nohup` gives its child SIGHUP ignored. A disposition is not the
//! environment: `env_clear` empties the one and passes the other through
//! the spawn untouched, and bash can neither trap nor reset a signal
//! that was ignored when it started. So a test that signals a script and
//! asserts its trap — or merely that it stopped — reads red on such a
//! runner for a reason the script did not cause, and green on the pod.
//! move_forgejo_data_sh.rs met it first and fixed it with a private
//! start; every test that signals a child now takes that start from here,
//! and `a_test_that_signals_a_child_starts_it_here.rs` refuses one that
//! does not.
//!
//! Python is the gate image's (infra/forge/boss-ci/required-tools.txt)
//! and needs no unsafe code here; it resets its own SIGPIPE and SIGINT
//! handlers with the rest, and `exec` keeps the pid, so a test that
//! signals the child's pid or its process group reaches the script.

use std::ffi::OsStr;
use std::process::Command;

/// Every signal at its default action and none blocked, then exec the
/// rest of argv.
pub const SIGNALS_AT_DEFAULT: &str = "import os, signal, sys
for s in signal.valid_signals():
    if s not in (signal.SIGKILL, signal.SIGSTOP):
        try:
            signal.signal(s, signal.SIG_DFL)
        except (OSError, ValueError):
            pass
signal.pthread_sigmask(signal.SIG_SETMASK, [])
os.execvp(sys.argv[1], sys.argv[1:])
";

/// A parent that IGNORES `argv[1]` (TERM, INT or HUP) and execs the
/// rest: what a step started under `nohup`, or a non-interactive bash's
/// `&` job, hands its children. A test reproduces the runner with it.
pub const HOST_IGNORES_THE_SIGNAL: &str = "import os, signal, sys
signal.signal(getattr(signal, 'SIG' + sys.argv[1]), signal.SIG_IGN)
os.execvp(sys.argv[2], sys.argv[2:])
";

/// A parent that BLOCKS `argv[1]` in the signal mask its children
/// inherit and execs the rest.
pub const HOST_BLOCKS_THE_SIGNAL: &str = "import os, signal, sys
signal.pthread_sigmask(signal.SIG_BLOCK, [getattr(signal, 'SIG' + sys.argv[1])])
os.execvp(sys.argv[2], sys.argv[2:])
";

/// The argv that starts what follows it with default dispositions, for a
/// test that assembles its own argv (behind a reproduced host, say).
pub const DEFAULT_SIGNALS_ARGV: [&str; 3] = ["python3", "-c", SIGNALS_AT_DEFAULT];

/// A `Command` that runs `program` with every signal at its default
/// action and none blocked. Add its arguments and environment as to
/// `Command::new(program)`; `env_clear` is safe, as long as the PATH it
/// sets still finds `python3`.
pub fn with_default_signals(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(DEFAULT_SIGNALS_ARGV[0]);
    cmd.args(&DEFAULT_SIGNALS_ARGV[1..]).arg(program);
    cmd
}

/// The argv of a host that ignores `signal` (`TERM`, `INT`, `HUP`) and
/// execs what follows it.
pub fn host_ignoring(signal: &str) -> [&str; 4] {
    ["python3", "-c", HOST_IGNORES_THE_SIGNAL, signal]
}

/// The argv of a host that blocks `signal` and execs what follows it.
pub fn host_blocking(signal: &str) -> [&str; 4] {
    ["python3", "-c", HOST_BLOCKS_THE_SIGNAL, signal]
}

#[cfg(test)]
mod tests {
    use super::{host_blocking, host_ignoring, with_default_signals};
    use std::process::Command;

    /// What a child reads of TERM's disposition and mask, from
    /// /proc/self/status: `SigIgn` and `SigBlk` with TERM's bit (15)
    /// picked out.
    const READ_TERM: &str = "ign=$(awk '/^SigIgn/{print $2}' /proc/self/status); \
         blk=$(awk '/^SigBlk/{print $2}' /proc/self/status); \
         printf '%s %s\\n' $(( (0x$ign >> 14) & 1 )) $(( (0x$blk >> 14) & 1 ))";

    fn term_state(mut cmd: Command) -> String {
        let out = cmd.output().expect("the child runs");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The control first: the reproduced hosts DO hand bash an ignored
    /// or blocked TERM, or the helper's answer below proves nothing.
    #[test]
    fn a_host_that_ignores_or_blocks_hands_it_on_and_the_helper_resets_both() {
        for (host, plain) in [
            (host_ignoring("TERM"), "1 0"),
            (host_blocking("TERM"), "0 1"),
        ] {
            let mut bare = Command::new(host[0]);
            bare.args(&host[1..]).args(["bash", "-c", READ_TERM]);
            assert_eq!(term_state(bare), plain, "{}", host[2]);

            let mut reset = Command::new(host[0]);
            reset.args(&host[1..]);
            let helped = with_default_signals("bash");
            reset.arg(helped.get_program()).args(helped.get_args());
            reset.args(["-c", READ_TERM]);
            assert_eq!(term_state(reset), "0 0", "{}", host[2]);
        }
    }
}
