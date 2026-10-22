//! `boss prove <car>` — proof is a receipt, not a claim.
//!
//! WHY THIS EXISTS. The `gate` step is trustworthy because a machine
//! writes its receipt and `boss park` copies it verbatim; nobody types
//! a verdict. The `proven` step had no such thing. Its one required
//! field, `verified`, is free prose, so "proven in prod" meant only
//! that somebody wrote a sentence saying so.
//!
//! That is not a hypothetical weakness. On 2026-08-28 a change was
//! reported done on the strength of an HTTP 204 — the API accepted a
//! write, so the write was called proof — and the behaviour it claimed
//! had never been observed. Twice. David's question was the right one:
//! how is there evidence a fix is in prod before coming back to me?
//! The answer was that there wasn't, because nothing required any.
//!
//! WORSE, THE HAND-ROLLED CHECKS WERE THEMSELVES WRONG. The same day, a
//! verification loop used `grep -c ... || echo 0`; on no match that
//! prints `0` from grep AND `0` from the fallback, and the resulting
//! "0\n0" compared unequal to "0", so the loop reported success for a
//! file that had not changed. A check written fresh per change is a
//! second thing that can be broken, and it is broken silently, in the
//! direction of saying yes.
//!
//! SO THE VERB RUNS THE PROBE ITSELF. It does not accept output pasted
//! in; it executes the command, captures exit status and both streams
//! verbatim, and REFUSES to record anything unless the probe exits zero
//! and — unless the caller explicitly downgrades to `--exit-only` —
//! unless the expected string is actually present. What lands on the
//! step is what happened, not what was hoped.
//!
//! A NUMERIC EXPECTATION IS COMPARED AS A WHOLE TOKEN. `--expect "1"`
//! once accepted a probe that printed `10`, because the comparison was
//! a plain substring test (421b3032): the claim was true and the proof
//! checked nothing, which is the one failure this verb exists to
//! prevent. Numbers now have to stand alone, and recording a bare
//! number warns and names the shape that cannot lie — assert inside the
//! probe and print a unique token.
//!
//! AND THE PROOF STAYS RE-RUNNABLE. The command is recorded alongside
//! its output, so `boss prove <car> --recheck` re-executes it later and
//! says whether the claim still holds. A proof that has silently
//! decayed — a ConfigMap preview reverted by the next converge, exactly
//! the failure a hand-pushed step-plugin bundle used to warn about — becomes findable
//! instead of being a sentence in a closed packet that nobody rereads.
//!
//! WHAT IT DELIBERATELY DOES NOT DO. It does not judge whether the
//! probe is a good probe. `--probe 'true' --exit-only` will pass, and
//! it will pass legibly: the recorded proof shows a caller who asserted
//! nothing, which is a thing a reader can see and challenge. The prose
//! in `verified` stays human, because what a change MEANS is judgement.
//! Only the evidence under it is mechanised.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use boss_jobs::probe::{CAR_CONVERGED_AT_VAR, CAR_MERGE_REF_VAR};
use serde_json::{Value, json};

/// The step this verb fills. Job step titles come from the registry, so
/// a rename there is a rename here — pinned the same way `boss park`
/// pins its three: refuse rather than guess which step was meant.
const PROVEN: &str = "Proven in prod";

/// Streams are recorded verbatim up to this much. Long enough for a
/// real probe's output, short enough that a runaway `find /` does not
/// push a megabyte into job metadata.
const MAX_STREAM: usize = 4000;

fn clip(s: &str) -> String {
    let s = s.trim_end();
    if s.len() <= MAX_STREAM {
        return s.to_string();
    }
    let cut = s
        .char_indices()
        .map(|(i, _)| i)
        .nth(MAX_STREAM)
        .unwrap_or(s.len());
    format!("{}\n… [{} more bytes]", &s[..cut], s.len() - cut)
}

/// What running a probe produced. Captured, never typed.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    /// The commands bash could not resolve while the probe ran — the
    /// forge's fd-9 finding, now this door's too (46f67333). Empty means
    /// the probe RAN; anything here means it did not, whatever the exit
    /// and the streams say. Sorted and deduplicated, as the runner's
    /// `sort -u` leaves it.
    pub missing_tools: Vec<String>,
    /// How long the run took, under what bound, and which reads its time
    /// went to ([`Timing`]); `None` for an outcome no probe run produced.
    pub timing: Option<Timing>,
}

/// WHERE A RUN'S TIME WENT (backlog a32af300). Car 5159b6b6's probe was
/// killed at 60 s three hours running and its attempt said only "killed
/// at 60s timeout" beside an empty stdout: the record could not tell a
/// slow system of record from a slow transport or the probe's own `jq`,
/// so it was re-derived from the pod (the record: one read, 25-122 s).
/// Measured by this process, never reported by the probe's text: the
/// wall clock from launch to exit, the bound the run had, and each read
/// the reader door forwarded ([`crate::probe_reader::ReadTiming`]) —
/// empty where no door opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Timing {
    pub elapsed_ms: u64,
    /// `None`: no timeout at this door (the hand door, no reader door).
    pub bound_s: Option<u64>,
    pub reads: Vec<crate::probe_reader::ReadTiming>,
}

/// THE UNRUNNABLE PRELUDE (backlog 46f67333). Every probe's shell gets
/// this ahead of the probe's own text: fd 9 opened on a not-found
/// channel before the probe can redirect anything, and a
/// `command_not_found_handle` that writes every unresolved command to
/// it — so a probe that pipes its own stderr into a `grep -q` still
/// cannot hide which tool was missing (f9304366). The handler ALSO
/// prints bash's usual message, so nothing is taken away; if fd 9
/// cannot be opened the probe still runs, with an empty channel.
///
/// ONE DEFINITION, ONE DOOR. Until backlog 9f00a805 (car 2, 2026-09-18)
/// this text lived in the forge's shell twin, infra/forge/run-car-
/// probe.sh, between markers this crate `include_str!`d and lifted at
/// build time, and the hand door ran the forge's prelude so the two
/// could not disagree. The twin is retired — the arrival rule's
/// ops-request runs `boss prove <car> --from-car --unattended` on the
/// forge — so the prelude, the judge, the verdict sentences and the
/// attempt record are written here once and nowhere else.
///
/// ITS FIRST LINE PUTS `probe-bin` ON PATH, INSIDE THE PROBE'S OWN SHELL
/// (review 717a7f7c N1 of car f5d53af7). The door names the directory in
/// [`PROBE_BIN_ENV`] and this line moves it onto PATH and unsets the name,
/// so the lookup it changes is the probe's — never the one that finds
/// `timeout`, `runuser`, `setpriv` and `bash` (see [`Shell::command`]).
/// No empty PATH entry is made when PATH is unset: an empty one is the
/// working directory.
pub(crate) const PRELUDE: &str = r#"[ -z "${BOSS_PROBE_BIN:-}" ] || export PATH="$BOSS_PROBE_BIN${PATH:+:$PATH}"; unset BOSS_PROBE_BIN
{ exec 9>>"${BOSS_PROBE_NOTFOUND:-/dev/null}"; } 2>/dev/null || exec 9>/dev/null
command_not_found_handle() {
    printf "%s: command not found\n" "$1" >&2
    printf "%s\n" "$1" >&9
    return 127
}
"#;

/// The channel, read as the forge's runner read it: `sort -u`, blank
/// lines dropped. Pure, so the record's `missing_tools` is testable
/// without a shell.
pub(crate) fn missing_tools(channel: &str) -> Vec<String> {
    let mut tools: Vec<String> = channel
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    tools.sort_unstable();
    tools.dedup();
    tools
}

/// The seconds `timeout` waits after its TERM before the KILL — the
/// forge runner's `timeout -k 5`, kept.
const KILL_AFTER_SECS: u64 = 5;

/// WHERE, AS WHOM AND UNDER WHAT A PROBE'S SHELL RUNS. The hand door
/// ([`Shell::here`]) is the operator's own shell: this user, this
/// directory (or the recorded one under `--recheck`), the environment
/// as it is, and no timeout of its own — a probe is bounded there only
/// while a reader door stands open for it ([`Shell::bound`]). The unattended door ([`Shell::unattended`]) is
/// what the forge's shell twin did and what the ops-request now asks
/// `boss prove --unattended` to do: drop from root to the probe user
/// with no way back up ([`NO_NEW_PRIVS`]),
/// run in the converged checkout, under a timeout, with exactly the
/// environment a recorded probe is promised — the system of record's
/// address, the read-only reader's identity and port table, the car's
/// own converged instant ([`Shell::with_car_instant`]), and
/// `probe-bin` first on PATH — and WITHOUT the actor this verb writes
/// as, which is a write credential handed to program text a builder
/// wrote if it leaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shell {
    pub cwd: Option<std::path::PathBuf>,
    /// The user to run as, when this process is root — the forge's
    /// `runuser -u boss-probe`. A process that is not root runs the probe
    /// as itself, the twin's by-hand path: a person who is already the
    /// probe user.
    pub user: Option<String>,
    /// Seconds before the probe is killed; `None` at the hand door.
    /// What the shell ASKS for: [`Shell::bound`] is what a run gets.
    pub timeout_secs: Option<u64>,
    /// Set on the probe's environment, after `strip`.
    pub env: Vec<(String, String)>,
    /// Removed from the probe's environment.
    pub strip: Vec<String>,
    /// Prepended to the probe's PATH, colon-separated — by the probe's
    /// own shell ([`PRELUDE`]), never on the launch's.
    pub path_prefix: Option<std::path::PathBuf>,
    /// `Some`: the probe's environment starts EMPTY and takes only these
    /// names from this process's own, before `strip` and `env` — the
    /// unattended door's [`UNATTENDED_INHERITS`]. `None`: everything this
    /// process holds, as the hand door's operator shell always has.
    pub inherit: Option<&'static [&'static str]>,
    /// Where a reader door for this probe would be opened FROM. Naming
    /// it opens nothing: `execute_with` asks `probe_reader::open`, and a
    /// host that holds no reader credential gets no door and reads as
    /// it did before the door existed (review 0bd6a9c2, B1).
    pub reader: Option<crate::probe_reader::Config>,
}

/// THE WORDS AFTER `runuser -u <account> --` AT A DOOR THAT DROPS TO THE
/// PROBE ACCOUNT (backlog dda26693, review acab446c F8b). runuser hands
/// the account its uid and nothing to stop it gaining more: every setuid
/// binary on the host was a step up from inside car text. With
/// no_new_privs set, an exec never grants privilege the process did not
/// already hold (setuid, setgid and file capabilities are ignored), and
/// the flag is inherited by everything the probe starts. verify-tick
/// (infra/forge/probe-account.sh) asks the account to run these same
/// words and read the flag back before it writes the marker, so a host
/// whose setpriv cannot set it runs no probe at all — the environment's
/// refusal, never the car's NOT PROVEN. Held equal to the script by
/// `the_launch_words_are_the_ones_the_tick_asks_the_account_to_run`.
pub(crate) const NO_NEW_PRIVS: [&str; 2] = ["setpriv", "--no-new-privs"];

/// THE ONLY NAMES THE UNATTENDED PROBE TAKES FROM THIS PROCESS'S OWN
/// ENVIRONMENT (backlog dda26693, review acab446c F8b, the environment
/// half). Everything else it is handed, the door sets by name: the
/// system of record's address, the reader's identity, port table and
/// door, the car's instant, the not-found channel and the empty token
/// directory.
///
/// WHY AN ALLOW-LIST. The probe inherited the forge's ops runner whole,
/// minus a strip list of the names somebody had thought of. Read from
/// the files that set them, that runner holds every address in
/// /etc/boss/sor.env (the forge's, its journal's, the registry's, the
/// mirror's, the ML API's), the host's id (`HOST_ID`), the request's
/// (`OPS_REQUEST_ID`), the probe account's own drop-in names
/// (`BOSS_PROBE_USER`, `BOSS_PROBE_DIR`, the marker's path, the view's
/// source), the reader's override (`BOSS_PROBE_READER_ACTOR`) and
/// systemd's (`RUNTIME_DIRECTORY`, the runner's 0700 directory where a
/// verb's token header lives, `INVOCATION_ID`, `JOURNAL_STREAM`). None
/// of it is a probe's to read, and a name added to that unit tomorrow
/// would have reached car text the day it landed.
///
/// WHAT PROBES READ, MEASURED BEFORE CHOOSING (2026-10-10, 1,584
/// ship-a-change cars, 1,582 with a recorded probe, 18 of them open): no
/// probe reads a runner name except as text inside a quoted grep or a
/// `${NAME:-default}` it supplies itself; the names they read bare are
/// the door's (`BOSS_CAR_CONVERGED_AT` in 311, `BOSS_JOBS_URL`,
/// `BOSS_SOR_USER`, `BOSS_SOR_DOOR`), and the tools in `probe-bin` read
/// only the door's four. `HOME`, `USER`, `LOGNAME` and `SHELL` are the
/// account's own, set by `runuser` after this list is applied — so not
/// listed here: off the forge, where nothing drops to an account, an
/// operator's HOME would hand the probe that operator's actor file.
pub(crate) const UNATTENDED_INHERITS: [&str; 4] = ["PATH", "LANG", "LC_ALL", "TZ"];

/// THE GIT CONFIGURATION A HOST-FREE RECHECK READS: the checkout's own,
/// and nothing of this host's or this operator's (backlog 51587416).
/// [`Shell::host_free`] sets both after the allow-list, so a value the
/// operator exported cannot stand in. `safe.directory` lives only in
/// the scopes these close, so a checkout this user does not own is
/// refused by git there, measured 2026-10-18 on the pod (uid 65534
/// reading a root-owned repository, HOME unset: "dubious ownership",
/// exit 128), and with HOME unset and both closed a checkout this user
/// owns reads as before (`git show`, `git grep`, `git rev-parse`).
pub(crate) const HOST_FREE_GIT: [(&str, &str); 2] = [
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
];

/// The name the door hands `path_prefix` to the probe's shell under; the
/// [`PRELUDE`]'s first line moves it onto PATH and unsets it. Held equal
/// to that line by `the_probe_bin_prefix_does_not_resolve_the_launch_words`,
/// which needs the two to agree to find its tool.
const PROBE_BIN_ENV: &str = "BOSS_PROBE_BIN";

/// THE CAR'S OWN CONVERGENCE INSTANT, read off the car: the `merge_ref`
/// the conductor wrote when its train landed. `None` when the car has
/// not merged, or when what it carries is not an object name — which is
/// also the guard on what this verb hands to `git`, since a car's
/// metadata is data from the system of record, not a literal here.
pub(crate) fn car_merge_ref(car: &Value) -> Option<&str> {
    car.pointer("/metadata/merge_ref")
        .and_then(Value::as_str)
        .and_then(resolvable_merge_ref)
}

/// An abbreviated or full object name, and nothing else.
fn resolvable_merge_ref(raw: &str) -> Option<&str> {
    let r = raw.trim();
    ((7..=40).contains(&r.len()) && r.chars().all(|c| c.is_ascii_hexdigit())).then_some(r)
}

/// The commit time of `merge_ref` in the checkout at `dir`, in epoch
/// seconds — an epoch because that is the only form that compares
/// honestly against the system of record's UTC timestamps
/// (`boss_jobs::probe::GIT_TIME_STRING_EVIDENCE`). `None` when the
/// merge is not in this checkout: the car's change has not converged
/// here, and a probe that gets no instant says NOT YET rather than
/// inventing one.
///
/// Read in `shell`'s own environment (backlog 51587416), so an instant a
/// host-free recheck hands its probe comes from the repository that probe
/// reads, not from one an operator's `GIT_DIR` names.
fn converged_at(shell: &Shell, dir: &Path, merge_ref: &str) -> Option<String> {
    let argv: Vec<String> = [
        "git",
        // The unattended door runs as root in the probe user's
        // checkout, and git refuses a repository owned by somebody
        // else ("dubious ownership") by answering nonzero rather
        // than erroring loudly — which here would silently hand
        // over NO instant and starve the probe the other way. This
        // one command is a read; say so rather than find out.
        "-c",
        &format!("safe.directory={}", dir.display()),
        "-C",
        &dir.display().to_string(),
        "show",
        "-s",
        "--format=%ct",
        merge_ref,
        "--",
    ]
    .map(str::to_string)
    .to_vec();
    let o = shell
        .command(&argv)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let ct = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !ct.is_empty() && ct.chars().all(|c| c.is_ascii_digit())).then_some(ct)
}

/// The top level of the checkout the operator stands in, when its HEAD
/// holds the car's merge — the witness that a git-object probe recorded
/// on another host reads the same objects here (backlog da937eb3). A
/// directory outside any repository, a repository git refuses, a
/// checkout behind the merge, or a car with no merge on record each
/// answer their own [`NoCheckout`], and the recheck keeps the
/// recorded-host refusal and says which (backlog 70cb74d8).
///
/// WITNESSED IN THE PROBE'S OWN ENVIRONMENT (backlog 51587416): `shell`
/// is the one the probe will run in, [`Shell::host_free`], so a
/// `GIT_DIR` or `GIT_CONFIG_*` of the operator's cannot make the witness
/// read one repository and the probe another — and a checkout git
/// refuses there as of dubious ownership, which it does once this host's
/// global `safe.directory` is out of reach, is refused HERE, as the
/// recorded-host refusal, rather than as a probe that exits 128 and
/// reads as decay.
fn checkout_holding(merge_ref: Option<&str>, shell: &Shell) -> Result<String, NoCheckout> {
    let merge_ref = merge_ref.ok_or(NoCheckout::NoMerge)?;
    let git = |args: &[&str]| {
        let argv: Vec<String> = std::iter::once("git")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        let mut cmd = shell.command(&argv);
        cmd.stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| NoCheckout::GitUnrunnable {
                error: e.to_string(),
            })
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    let root = String::from_utf8_lossy(&top.stdout).trim().to_string();
    if !top.status.success() || root.is_empty() {
        let dir = shell
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .map_or_else(|| ".".to_string(), |d| d.display().to_string());
        return Err(rev_parse_refused(
            dir,
            top.status.code(),
            &String::from_utf8_lossy(&top.stderr),
        ));
    }
    let held = git(&[
        "-C",
        &root,
        "merge-base",
        "--is-ancestor",
        merge_ref,
        "HEAD",
    ])?;
    if held.status.success() {
        return Ok(root);
    }
    Err(NoCheckout::NotHeld {
        root,
        merge: merge_ref.to_string(),
        exit: held.status.code(),
        stderr: first_line(&String::from_utf8_lossy(&held.stderr)),
    })
}

/// WHY A GIT-OBJECT PROBE WAS NOT PLACED IN A CHECKOUT HERE (backlog
/// 70cb74d8). The recheck answered every one of these with the same
/// CANNOT RE-RUN HERE, whose last sentence listed them all — "this one
/// runs something else, or reads through the shell, or there is no such
/// checkout here" — so an operator could not tell a probe to rewrite
/// from a directory to change (reviews 8d6976fa, da88bbff and 1042cf88
/// of car afd61aa2). Each variant carries what decided it: the place
/// the text left the grammar, or the directory git was asked about, its
/// exit and the first line it printed on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoCheckout {
    /// The probe's text is outside the git-object grammar; the checkout
    /// was never read.
    Grammar(boss_jobs::probe::Refused),
    /// The car records no merge commit, so no checkout can be shown to
    /// hold it.
    NoMerge,
    /// `git` could not be started.
    GitUnrunnable { error: String },
    /// Git refused the repository at `dir` as one this user does not own
    /// ("detected dubious ownership", exit 128). The witness runs in the
    /// host-free shell, which reads no system or global git configuration
    /// (51587416), so a `safe.directory` there does not reach it —
    /// measured 2026-10-18 on the pod as uid 65534 reading a root-owned
    /// checkout, and again 2026-10-19 by this car.
    DubiousOwnership {
        dir: String,
        exit: Option<i32>,
        stderr: String,
    },
    /// `dir` is in no checkout git will read: outside any repository, or
    /// in one with no work tree.
    NotInACheckout {
        dir: String,
        exit: Option<i32>,
        stderr: String,
    },
    /// The checkout at `root` does not hold the car's merge: its HEAD is
    /// behind it (exit 1), or the commit is not in it at all (exit 128).
    NotHeld {
        root: String,
        merge: String,
        exit: Option<i32>,
        stderr: String,
    },
}

/// `git rev-parse --show-toplevel` failed in `dir`: dubious ownership
/// when git's message says so, and otherwise no checkout. The message is
/// git's own (2.39 on the pod, measured 2026-10-19: "fatal: detected
/// dubious ownership in repository at '<dir>'", exit 128), and a git
/// that words it differently lands in [`NoCheckout::NotInACheckout`]
/// with its stderr quoted — named less precisely, never silently.
fn rev_parse_refused(dir: String, exit: Option<i32>, stderr: &str) -> NoCheckout {
    let stderr = first_line(stderr);
    if stderr.contains("dubious ownership") {
        NoCheckout::DubiousOwnership { dir, exit, stderr }
    } else {
        NoCheckout::NotInACheckout { dir, exit, stderr }
    }
}

/// The first non-empty line of what a command printed, trimmed.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// `exit 128`, or how it ended without one, then `: <stderr>` when git
/// printed anything.
fn git_said(exit: Option<i32>, stderr: &str) -> String {
    let ended = exit.map_or_else(|| "killed by a signal".to_string(), |n| format!("exit {n}"));
    if stderr.is_empty() {
        ended
    } else {
        format!("{ended}: {stderr}")
    }
}

impl std::fmt::Display for NoCheckout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grammar(r) => write!(f, "its text is outside the git-object grammar — {r}"),
            Self::NoMerge => write!(
                f,
                "the car records no merge commit, so no checkout can be shown to hold it"
            ),
            Self::GitUnrunnable { error } => write!(f, "git could not be started: {error}"),
            Self::DubiousOwnership { dir, exit, stderr } => write!(
                f,
                "git refuses {dir} as a repository this user does not own ({}). The recheck \
                 reads no system or global git configuration (51587416), so a safe.directory \
                 there does not reach it: run it from a checkout this user owns",
                git_said(*exit, stderr)
            ),
            Self::NotInACheckout { dir, exit, stderr } => write!(
                f,
                "{dir} is in no checkout git will read (`git rev-parse --show-toplevel`, {}): \
                 run it from one whose HEAD holds the car",
                git_said(*exit, stderr)
            ),
            Self::NotHeld {
                root,
                merge,
                exit,
                stderr,
            } => write!(
                f,
                "the checkout at {root} does not hold the car's merge {merge} (`git merge-base \
                 --is-ancestor`, {}): bring it up to that commit, or run it from one that holds it",
                git_said(*exit, stderr)
            ),
        }
    }
}

impl Shell {
    /// THE FIXED CUTOFF A RECORDED PROBE COMPARES AGAINST (backlog
    /// a92571a6, measured 2026-09-19). A probe asking "did my
    /// qualifying event happen after my change landed?" had nothing
    /// fixed to ask it of, so the idiom that grew was `git log -1
    /// --format=%ct HEAD` — the converged checkout's CURRENT head,
    /// which advances with every train. The car's change converged
    /// ONCE; that instant is what the question means, it is derivable
    /// from the `merge_ref` already on the car, and handing it over is
    /// what makes the correct comparison available rather than merely
    /// documented.
    ///
    /// Both names are STRIPPED first, so a value left in the runner's
    /// environment can never stand in for one this door resolved.
    pub(crate) fn with_car_instant(mut self, merge_ref: Option<&str>) -> Self {
        self.strip.push(CAR_MERGE_REF_VAR.into());
        self.strip.push(CAR_CONVERGED_AT_VAR.into());
        let Some(r) = merge_ref.and_then(resolvable_merge_ref) else {
            return self;
        };
        self.env.push((CAR_MERGE_REF_VAR.into(), r.to_string()));
        let dir = self.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
        if let Some(at) = converged_at(&self, &dir, r) {
            self.env.push((CAR_CONVERGED_AT_VAR.into(), at));
        }
        self
    }

    /// The hand door's shell: as the operator, in `cwd` when given. No
    /// machine token reaches it — not by a strip here, which removed the
    /// dev pod's directory name and so fell back to boss-core's default,
    /// but by `execute_with`, which points every probe at an empty one
    /// (`door_env::NoTokenInReach`; review ef2da426 F1, backlog 844b936e).
    pub(crate) fn here(cwd: Option<&Path>) -> Self {
        Self {
            cwd: cwd.map(Path::to_path_buf),
            user: None,
            timeout_secs: None,
            env: Vec::new(),
            strip: Vec::new(),
            path_prefix: None,
            inherit: None,
            reader: None,
        }
    }

    /// WHO A `boss` VERB IN THIS PROBE SIGNS AS — the `actor` an attempt
    /// records beside `host` (backlog 371b0443).
    pub(crate) fn probe_actor(&self) -> String {
        self.probe_actor_with(crate::identity::caller)
    }

    /// [`Self::probe_actor`], with this process's own resolution passed
    /// in so a test can name it. Read off how the shell is BUILT, the
    /// CLI's own order (identity.rs): an actor the door sets by name
    /// wins; a door that builds the environment from an allow-list
    /// ([`UNATTENDED_INHERITS`]) hands on neither actor name, and the
    /// account it drops to has `/nonexistent` for a home
    /// (infra/forge/probe-account.sh), so no actor file either —
    /// [`crate::identity::UNIDENTIFIED`]; and the hand door's probe
    /// inherits this process whole, so it is whoever this process is.
    pub(crate) fn probe_actor_with(
        &self,
        this_process: impl FnOnce() -> Option<crate::identity::Caller>,
    ) -> String {
        let set = self
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == crate::identity::ACTOR_ENV)
            .map(|(_, v)| v.clone());
        crate::identity::resolve_from(set, None)
            .or_else(|| self.inherit.is_none().then(this_process).flatten())
            .map_or_else(|| crate::identity::UNIDENTIFIED.to_string(), |c| c.id)
    }

    /// THE SHELL A PROBE PLACED BY ITS TEXT ALONE RUNS IN (backlog
    /// 51587416). `--recheck` re-runs a probe recorded on another host
    /// when its text reads only the system of record (c89a661e) or only
    /// git objects at a named revision (da937eb3), and both judgements
    /// are of the TEXT. The shell it ran in was still this one's
    /// [`Shell::here`] — the operator's whole environment — so a
    /// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_CONFIG_COUNT`/`KEY_n`/`VALUE_n`, a
    /// global gitconfig, or an exported function named `git` or
    /// `boss-sor-read` changed what clean text read (measured on the pod
    /// 2026-10-18: a recheck that answered HOLDS answered NOT YET with
    /// `git` exported as a function).
    ///
    /// So the probe starts from the unattended door's own allow-list,
    /// [`UNATTENDED_INHERITS`] — ONE list, so the two doors that run a
    /// probe whose text was judged anywhere cannot drift apart — and git
    /// reads only the checkout's own configuration ([`HOST_FREE_GIT`]).
    /// Everything a probe is legitimately handed is set by name after
    /// the list and so still reaches it: the system of record's address
    /// and reader ([`Shell::with_probe_reader`]), the car's instant
    /// ([`Shell::with_car_instant`]), the empty token directory and the
    /// not-found channel (`execute_gated`). HOME is not on the list, as
    /// it is not at the unattended door: here nothing drops to an
    /// account, so it would be the operator's, and with it their actor
    /// file and their global gitconfig.
    pub(crate) fn host_free(mut self) -> Self {
        self.inherit = Some(&UNATTENDED_INHERITS);
        self.env.extend(
            HOST_FREE_GIT
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        self
    }

    /// The argv the shell runs — `timeout`, `runuser`, [`NO_NEW_PRIVS`]
    /// and `bash -c` in the forge runner's order — with the prelude ahead of the probe's
    /// text, under the bound this run actually has: [`Self::bound`],
    /// once it is known whether a door opened. Pure, so the test pins
    /// the words rather than a run.
    fn command_line_under(&self, probe: &str, as_root: bool, bound: Option<u64>) -> Vec<String> {
        let mut argv = self.command_prefix(as_root, bound);
        argv.extend(["bash", "-c", &format!("{PRELUDE}\n{probe}")].map(str::to_string));
        argv
    }

    fn command_prefix(&self, as_root: bool, bound: Option<u64>) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(t) = bound {
            argv.extend(
                [
                    "timeout",
                    "-k",
                    &KILL_AFTER_SECS.to_string(),
                    &t.to_string(),
                ]
                .map(str::to_string),
            );
        }
        if let (Some(u), true) = (&self.user, as_root) {
            argv.extend(["runuser", "-u", u, "--"].map(str::to_string));
            argv.extend(NO_NEW_PRIVS.map(str::to_string));
        }
        argv
    }

    /// The seconds this run is given. A door that is OPEN holds a
    /// credential for one recorded command and lives
    /// [`crate::probe_reader::MAX_LIFETIME_SECS`], so the probe behind it
    /// lives no longer; with no door the shell's own answer
    /// stands, which at the hand door is no timeout at all.
    ///
    /// The first cut clamped every shell that NAMED a reader, and every
    /// shell names one, so the hand door lost its "no timeout" without a
    /// word while its doc comment still promised it (review 0bd6a9c2,
    /// F2). The cap now follows the door, and [`Self::clamp_notice`]
    /// says so when it changes what was asked.
    fn bound(&self, door_open: bool) -> Option<u64> {
        if door_open {
            Some(
                self.timeout_secs
                    .unwrap_or(crate::probe_reader::MAX_LIFETIME_SECS)
                    .clamp(1, crate::probe_reader::MAX_LIFETIME_SECS),
            )
        } else {
            self.timeout_secs
        }
    }

    /// The sentence owed when an open door shortens what the shell
    /// asked for; `None` when it changes nothing.
    fn clamp_notice(&self, door_open: bool) -> Option<String> {
        let bound = self.bound(door_open)?;
        if !door_open || self.timeout_secs == Some(bound) {
            return None;
        }
        let asked = self
            .timeout_secs
            .map_or("no timeout".to_string(), |t| format!("{t}s"));
        Some(format!(
            "a reader door is open for this probe, so it is bounded at {bound}s where this \
             door asked for {asked}: the door holds a reader credential for one recorded \
             command and lives no longer (design b35c22b4)"
        ))
    }

    /// Shared by execution and its provenance reads: the same directory,
    /// environment and effective user, without any broader git trust.
    ///
    /// PATH HERE IS THIS PROCESS'S OWN, so the launch words resolve where
    /// verify-tick resolved them, on the runner's PATH. `path_prefix`
    /// rides as [`PROBE_BIN_ENV`] for the [`PRELUDE`] to put on the
    /// probe's PATH inside `bash -c`: set on this Command's PATH, as it
    /// was until review 717a7f7c N1, it was the lookup for `timeout`
    /// too, and through it for `runuser`, `setpriv` and `bash` — a file
    /// of one of those names in the converged tree's `probe-bin` ran as
    /// root, ahead of the drop to the account.
    fn command(&self, argv: &[String]) -> std::process::Command {
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        if let Some(names) = self.inherit {
            cmd.env_clear();
            for name in names {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
        }
        for name in &self.strip {
            cmd.env_remove(name);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        match &self.path_prefix {
            Some(prefix) => cmd.env(PROBE_BIN_ENV, prefix),
            None => cmd.env_remove(PROBE_BIN_ENV),
        };
        cmd
    }

    fn read(
        &self,
        program: &str,
        args: &[&str],
        no_token: &crate::door_env::NoTokenInReach,
    ) -> Result<std::process::Output, String> {
        // A provenance read opens no door, so it is bounded as asked.
        let mut argv =
            self.command_prefix(self.user.is_some() && running_as_root(), self.timeout_secs);
        argv.push(program.into());
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        let mut cmd = self.command(&argv);
        // Local metadata needs no credential. Keep the same empty-token
        // boundary as execution, including after caller-set environment.
        no_token.token_only(&mut cmd);
        cmd.stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("{program} {}: {e}", args.join(" ")))
    }
}

/// Is this process root? Asked of `id -u`, the way the forge runner
/// asked it, so no libc binding rides in for one question.
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// Run `probe` in `shell` and capture everything it did.
///
/// `bash`, not `sh`, because the prelude's handler is a bash facility
/// and because the forge ran the same text under `bash -c` — a probe
/// means one thing at both doors. On a bash too old for the handler
/// (macOS's 3.2) the channel stays empty and the verdict falls back to
/// what it was: `command not found` on stderr, read as a red.
pub(crate) fn execute_with(probe: &str, shell: &Shell) -> Result<Outcome> {
    execute_given(probe, shell, crate::door_env::NoTokenInReach::new())
}

/// THE PROBE DID NOT RUN, AND THE HOST IS WHY (backlog 2e1f609e, review
/// 595d8a89 N2 of car 844b936e). A probe runs only beside an empty token
/// directory this process made; when the directory cannot be made — the
/// temp filesystem full or unwritable — the probe is not run, which is
/// fail-closed and right.
///
/// WHAT WAS WRONG WITH IT (corrected by review 19ecf37f). That error left
/// this verb by `?` as an ordinary failure, exit 1, and recorded nothing.
/// Exit 1 never made a car red — nothing reads an ops-request's exit as a
/// car's colour; the shed reads only `proof_attempt` — so review
/// 595d8a89's N2 overstated it. The real gap was the SILENCE: the arrival
/// run left no `proof_attempt`, recheck-failing-probes-hourly re-runs only
/// cars that carry one (`Scope::Failing` in jobs_run_car_probes.rs), and
/// such a car sat as probe-pending with no last attempt for ~72 hours,
/// until the shed's stale band, with the host fault shown nowhere. The
/// gate's disk floor is the shape to copy: a refusal of the environment,
/// said as one, on the record, never a verdict on the branch (CLAUDE.md
/// §Diagnosis, "An infrastructure refusal is not a consist failure").
///
/// So it is its own error type, every door exits [`REFUSED_EXIT`] on it
/// ("nothing ran and nothing was judged"), and the doors that record
/// attempts record this refusal on the car ([`refusal_patch`]): as a NOT
/// YET attempt naming the host and the cause when there is no judgement
/// to keep, and beside a failed attempt, never over it (review 2d0043ad).
/// Which door does what is [`on_environment_refusal`].
#[derive(Debug)]
pub(crate) struct EnvironmentRefusal {
    cause: String,
    executed: Option<Outcome>,
}

impl EnvironmentRefusal {
    pub(crate) fn executed(&self) -> Option<&Outcome> {
        self.executed.as_ref()
    }

    fn before_execution(cause: String) -> Self {
        Self {
            cause,
            executed: None,
        }
    }
}

impl std::fmt::Display for EnvironmentRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if ran_without_a_read(&self.cause) {
            "the probe's result was discarded"
        } else {
            "the probe did not run"
        };
        write!(
            f,
            "ENVIRONMENT REFUSAL — {what} and nothing about the claim was \
             judged: {}. This host could not give the probe the environment it is promised; \
             repair the named cause and run it again.",
            self.cause
        )
    }
}

/// How the cause of the ONE refusal that follows a run begins. Every
/// other environment refusal is made before the probe starts, and its
/// record says "did not run", which is true of it. This one is found
/// after: the probe ran, and the reader door refused a read it asked
/// for. The cause is stored on the car as written, so the record says
/// which of the two it was in its own first words, and the two fixed
/// sentences around it ([`EnvironmentRefusal`]'s and
/// [`environment_why`]) follow it rather than claim a run did not
/// happen.
pub(crate) const RAN_WITHOUT_A_READ: &str = "the probe ran without a read it asked for";

fn ran_without_a_read(cause: &str) -> bool {
    cause.starts_with(RAN_WITHOUT_A_READ)
}

/// A PROBE THAT RAN WITHOUT A READ IT ASKED FOR JUDGED NOTHING (review
/// 991bb439, N1). The door serves a port only where that port's own gate
/// names the credential a reader; anywhere else it answers 502, which
/// `boss-sor-read`'s `curl -f` turns into exit 22. Until this, that was
/// the probe's verdict: "A probe that fails is evidence AGAINST the
/// claim" — a FAILED proof on correct code, for every car whose probe
/// reads a service that does not mount the reader directory yet or whose
/// rotation has moved on. The cause is the host's, exactly as a missing
/// credential's is, so it is the host's refusal: nothing judged, exit
/// [`REFUSED_EXIT`], the car a NOT YET the hourly recheck runs again,
/// with the port and what its gate answered on the record.
///
/// WHATEVER THE PROBE EXITED, a pass included. A probe whose text
/// swallows the failed read (`|| true`, a pipe into `jq` with no
/// pipefail) and prints its marker has asserted something about a page
/// it never saw — the absence assertion against a narrower world this
/// door exists to prevent (61085a9e). Discarding a pass is the strict
/// side of the rule; nothing here can turn a not-proven into a proven.
///
/// What it does NOT cover: an upstream that is simply DARK is still the
/// probe's own failed read, as it was on the LAN before the door
/// existed — whether it went dark after the door vouched for it or was
/// dark the first time the probe read it. The door remembers only a
/// port where something ANSWERED and did not name the credential a
/// reader (`probe_reader::Unvouched`; review cd3f6a99, N1 — the first
/// cut remembered a connect error too, so a car that took its own
/// service down read NOT YET every hour instead of NOT PROVEN).
fn unserved_read_refusal(unserved: &[String], exit: i32) -> Option<String> {
    if unserved.is_empty() {
        return None;
    }
    Some(format!(
        "{RAN_WITHOUT_A_READ}: the reader door refused {} port(s) no gate vouches for — {} — \
         so what the probe concluded (exit {exit}) is not a judgement of the claim",
        unserved.len(),
        unserved.join("; ")
    ))
}

/// WHOM THE READER DOOR'S SOCKET IS HANDED TO: the user this door drops
/// to, and only when this process is root — the one uid for which
/// "drops to" is an act. Anyone else runs the probe as themselves
/// ([`Shell::command_prefix`] adds no `runuser`), so the socket stays
/// theirs; naming a recipient there would ask a non-root process to
/// chown, which succeeds for itself and is a refusal for anybody else —
/// a door that named a user would stop proving by hand (review
/// 991bb439, N4: nothing held this line).
fn door_recipient(shell: &Shell, as_root: bool) -> Option<&str> {
    shell.user.as_deref().filter(|_| as_root)
}

impl std::error::Error for EnvironmentRefusal {}

/// The refusal's cause, when this error is one; any other error back
/// unchanged, for the door to propagate as before.
pub(crate) fn environment_refusal(e: anyhow::Error) -> Result<String> {
    match e.downcast_ref::<EnvironmentRefusal>() {
        Some(r) => Ok(r.cause.clone()),
        None => Err(e),
    }
}

/// A refusal a door reached BEFORE `execute_with`, in the shape
/// `execute_with` gives its own — so the door's one refusal arm records
/// and exits for both, and neither can be routed differently.
fn not_run(cause: String) -> Result<Outcome> {
    Err(anyhow::Error::new(EnvironmentRefusal::before_execution(
        cause,
    )))
}

/// The four doors that run a car's probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Door {
    /// `boss prove <car> --from-car --unattended`: the arrival run and
    /// the hourly recheck's, filed as a `run-car-probe` ops-request.
    Unattended,
    /// `boss prove <car> --from-car` or `--probe` by hand: records a
    /// proof or an attempt on the car.
    Hand,
    /// `boss prove <car> --recheck`: re-runs a recorded probe, writes
    /// nothing on any outcome.
    Recheck,
    /// `boss prove <car> --disproved`: records a disproof only on a
    /// probe judged false.
    Disproved,
}

/// What a door does when the host refuses the probe's environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OnRefusal {
    /// Record the refusal on the car ([`refusal_patch`]).
    pub record_attempt: bool,
    /// The process's exit.
    pub exit: i32,
}

/// EVERY DOOR EXITS [`REFUSED_EXIT`]; the two that record attempts
/// record this one. `--recheck` writes nothing on any outcome and
/// `--disproved` records only a disproof, so neither starts writing here.
pub(crate) fn on_environment_refusal(door: Door) -> OnRefusal {
    let record_attempt = matches!(door, Door::Unattended | Door::Hand);
    OnRefusal {
        record_attempt,
        exit: REFUSED_EXIT,
    }
}

/// The `proof_attempt.why` of a refusal: a NOT YET, so the shed never
/// draws it red and the hourly recheck runs it again, naming the host
/// and the cause so the fault is visible where the car is.
pub(crate) fn environment_why(host: &str, cause: &str) -> String {
    format!(
        "NOT YET: {} — {host} refused its environment ({cause}). \
         ENVIRONMENT REFUSAL, not a verdict against the change; repair the named cause, \
         and recheck-failing-probes-hourly runs it again.",
        if ran_without_a_read(cause) {
            "the probe's result was discarded"
        } else {
            "the probe did not run"
        }
    )
}

/// The attempt a refusal records: the doors' own shape
/// ([`attempt_json`]), so the yard and the streak read it like any other
/// not-yet, with `exit` null because nothing exited and `environment`
/// set so a reader can tell the host's refusal from a probe's own 75.
pub(crate) fn environment_attempt_json(
    probe: &str,
    expect: Option<&str>,
    host: &str,
    actor: &str,
    at: &str,
    cause: &str,
    prior: Option<&Value>,
) -> Value {
    let nothing = Outcome {
        exit: 0,
        stdout: String::new(),
        stderr: String::new(),
        missing_tools: Vec::new(),
        timing: None,
    };
    let why = environment_why(host, cause);
    let mut a = attempt_json(probe, expect, &nothing, host, actor, at, &why, prior);
    a["exit"] = Value::Null;
    a["environment"] = json!(true);
    a
}

/// The car key that holds the latest environment refusal, beside
/// `proof_attempt` rather than inside it.
pub(crate) const PROOF_ENVIRONMENT_REFUSAL: &str = "proof_environment_refusal";

/// WHAT A REFUSAL WRITES ON THE CAR — the metadata PATCH, as a pure
/// function of the car (backlog 2e1f609e, review 2d0043ad F2).
///
/// A refusal judged nothing, so it must not overwrite a judgement. The
/// hourly recheck re-runs exactly the cars whose last attempt FAILED, and
/// writing the refusal over that attempt turned every failing car in the
/// shed into a calm NOT YET within the hour of a full /tmp on the forge
/// (measured by the review through `shed_place`). So:
///
/// - no prior attempt, or a prior NOT YET: the refusal IS the attempt (a
///   not-yet, the streak carried), so the recheck picks the car up and
///   the shed shows why — the ~72 silent hours review 19ecf37f found;
/// - a prior that judged (failed, unrunnable): `proof_attempt` is left
///   exactly as it was. The car still carries an attempt, so the recheck
///   still selects it, and it still reads failing.
///
/// Either way the refusal itself lands under
/// [`PROOF_ENVIRONMENT_REFUSAL`] as `{at, host, cause}`, so the host
/// fault is on the car whichever case it was.
pub(crate) fn refusal_patch(
    car: &Value,
    probe: &str,
    expect: Option<&str>,
    host: &str,
    actor: &str,
    at: &str,
    cause: &str,
) -> Value {
    let prior = car
        .pointer("/metadata/proof_attempt")
        .filter(|p| !p.is_null());
    let mut patch = json!({
        PROOF_ENVIRONMENT_REFUSAL: {"at": at, "host": host, "cause": cause},
    });
    if prior.is_none_or(boss_jobs::car::attempt_said_not_yet) {
        patch["proof_attempt"] =
            environment_attempt_json(probe, expect, host, actor, at, cause, prior);
    }
    patch
}

/// Hand `patch` to `write` when `door` records refusals and this is not
/// a dry run: `Ok(true)` when written, `Ok(false)` when this door writes
/// nothing. The decision lives here, not at the door, so the test that
/// drives this with a capturing writer pins both the decision and the
/// handing-over (review 2d0043ad: an `if false` at the door left every
/// refusal test green); each door's call is pinned by `door_sites`.
pub(crate) async fn record_refusal<F, Fut>(
    door: Door,
    dry: bool,
    patch: &Value,
    write: F,
) -> Result<bool>
where
    F: FnOnce(Value) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    if !on_environment_refusal(door).record_attempt || dry {
        return Ok(false);
    }
    write(patch.clone()).await?;
    Ok(true)
}

/// Leave the process as `door` does on a refusal, with the sentence on
/// stderr. Called after any record the door makes.
pub(crate) fn exit_refused(door: Door, cause: &str) -> ! {
    eprintln!(
        "boss prove: REFUSED — {}",
        EnvironmentRefusal::before_execution(cause.to_string())
    );
    std::process::exit(on_environment_refusal(door).exit)
}

/// The forge's unattended door, by the two dispatcher rules that file its
/// `run-car-probe` ops-requests (`infra/dispatcher/rules/`): the run on a
/// train's arrival, and the hourly re-run of every car whose last attempt
/// did not prove it. Held equal to those files by a test.
pub(crate) const ARRIVAL_PROBE_RULE: &str = "run-car-probes-on-train-arrived";
pub(crate) const HOURLY_PROBE_RULE: &str = "recheck-failing-probes-hourly";

/// A RECORD READ WITH NO READER CREDENTIAL IS REFUSED AT A HAND DOOR,
/// BEFORE IT RUNS (design 00227628, decided by David 2026-10-09, option
/// 3; item 64576122, parent 37742794 R3a).
///
/// MEASURED. The 24 h machine-gate window read 2026-10-09 18:35Z (builder
/// run f76adc85) put two tokenless reads at 05:53:31Z on this door: a
/// hand `boss prove` on the dev pod, 60 ms before car d6e1979f's `proven`
/// was completed. The pod holds no reader credential by design (b35c22b4)
/// and a pin forbids it a copy, so [`probe_reader::open`] finds none,
/// opens no door, and every `boss-sor-read` the probe runs goes out with
/// no token — which the machine gate refuses once it enforces. Under
/// enforce that is a 401, `curl -f` makes it exit 22, and the door would
/// record a NOT PROVEN (or, at `--disproved`, a disproof) about correct
/// code: a verdict on the host, written as a verdict on the change.
///
/// THE DECISION. Those reads are named refused, not routed elsewhere —
/// the alternatives were a reader slot in the dev container (it widens
/// what the pod holds) and a read through the pod's stamping door as the
/// operator (against review 177b4976 N2). A probe that reads the record
/// is proven by the forge's unattended door ([`ARRIVAL_PROBE_RULE`],
/// [`HOURLY_PROBE_RULE`]), which needs no hand. So this door says so
/// before anything is sent, rather than letting the gate say it after.
///
/// WHICH PROBES: one that RUNS a known record client, in command position
/// ([`boss_jobs::probe::record_read_client`]) — a mention, a grep for its
/// name, or a `git show` of its file reads only the tree and still runs.
/// The original reader-only check missed `boss-api` (0b4705f8); naming
/// an identity does not put a reader credential in this environment.
/// WHICH HOSTS: one whose credential path holds nothing at all
/// ([`probe_reader::holds_no_credential`], the reading `open` uses). A
/// host that holds one gets the door and reads as the reader; a broken
/// deposit is `open`'s environment refusal, unchanged. The unattended
/// door is never refused here: it is the door this sentence names.
///
/// `None` when the probe may run.
///
/// [`probe_reader::open`]: crate::probe_reader::open
/// [`probe_reader::holds_no_credential`]: crate::probe_reader::holds_no_credential
pub(crate) fn record_read_refusal(probe: &str, door: Door, no_credential: bool) -> Option<String> {
    if door == Door::Unattended || !no_credential {
        return None;
    }
    let reader = boss_jobs::probe::record_read_client(probe)?;
    let what_here = match door {
        Door::Hand => {
            "This door records nothing for it: once the car's train has arrived, the forge \
             records the proof (or the attempt) on the car by itself."
        }
        Door::Recheck => {
            "--recheck re-runs it only on a host that holds the credential; the forge's \
             hourly door re-runs it while the car is not proven."
        }
        Door::Disproved => {
            "A disproof of it is recorded only on a host that holds the credential; the \
             forge records a probe that fails as the car's NOT PROVEN attempt by itself."
        }
        Door::Unattended => "",
    };
    Some(format!(
        "REFUSED BEFORE ANYTHING WAS SENT — this probe reads the system of record through \
         `{reader}`, and this host holds no reader credential, so each read would go out \
         carrying no token, which the machine gate refuses under enforce. Decided on design \
         00227628 (David, 2026-10-09): a probe that reads the record is proven by the forge's \
         unattended door — `{ARRIVAL_PROBE_RULE}` runs it when the car's train arrives, and \
         `{HOURLY_PROBE_RULE}` runs it again every hour while its last attempt has not proven \
         it. {what_here} Nothing ran and nothing is recorded, and no stated reason overrides \
         this. A probe that reads only the tree (`git show HEAD:<path>`) still runs here."
    ))
}

/// [`record_read_refusal`] asked of a door's own shell, as an error the
/// door returns before `execute_with` — the one call each hand door makes
/// between building its shell and running the probe. A shell that names
/// no reader can open no door, so it counts as holding none.
pub(crate) fn refuse_a_record_read_without_a_reader(
    probe: &str,
    shell: &Shell,
    door: Door,
) -> Result<()> {
    let no_credential = shell
        .reader
        .as_ref()
        .is_none_or(crate::probe_reader::holds_no_credential);
    match record_read_refusal(probe, door, no_credential) {
        None => Ok(()),
        Some(why) => bail!(
            "{why}\n  (the reader credential this host would use: {})",
            shell.reader.as_ref().map_or_else(
                || "none named".to_string(),
                |c| c.credential.display().to_string()
            )
        ),
    }
}

/// The file whose presence says "the probe account verified on this host,
/// this runner tick" — named by the drop-in `infra/forge/probe-account.sh`
/// writes for the ops runner, and written by that script's `verify-tick`
/// into the unit's runtime directory before each tick.
pub(crate) const PROBE_VERIFIED_FILE_ENV: &str = "BOSS_PROBE_VERIFIED_FILE";

/// THE VERIFIED ACCOUNT, OR NO PROBE (decided by David on design-doc
/// c98c79aa, question `fallback`, 2026-10-06; backlog 703358ce). Where a
/// host names the marker, a probe runs only when the marker exists and
/// names the user this door is about to drop to. Absent, unreadable or
/// naming anyone else, the probe is NOT RUN — an environment refusal, so
/// the car records did-not-run with the cause and the hourly recheck
/// retries it; never NOT PROVEN, and never a run as some other user.
/// It has to be here, before `runuser`: `runuser -u <absent>` exits 1,
/// and exit 1 is what a probe that ran and failed exits.
///
/// `None` when no marker is named. At the unattended door that case never
/// reaches here: [`unattended_ground`] refuses a host that names no marker
/// (or no account, or no view) first, through [`unnamed_probe_host_refusal`].
/// What is left is a hand run of a shell that names a user, which runs as
/// whoever runs it.
pub(crate) fn probe_account_refusal(user: &str, marker: Option<&Path>) -> Option<String> {
    let marker = marker?;
    let said = std::fs::read_to_string(marker).ok();
    if said.as_deref().map(str::trim) == Some(user) {
        return None;
    }
    let why = match tick_reason(marker) {
        Some(reason) => format!(
            "this tick's verify-tick said: {reason} — the host's converge is what repairs it"
        ),
        None => "verify-tick left no reason beside the marker (it did not run this tick, or \
                 predates the reason) — the host's converge packet says what it found under \
                 probe_account, and that converge is what repairs it"
            .to_string(),
    };
    Some(format!(
        "the probe account {user} is not verified on this host for this runner tick ({} {}); \
         no probe runs until it is — {why}",
        marker.display(),
        match said {
            Some(other) => format!("names {:?}", other.trim()),
            None => "is absent".to_string(),
        }
    ))
}

/// Beside the marker, the file `verify-tick` leaves when it writes none:
/// `<marker>.why`, one line (backlog dda26693, review acab446c F3). Held
/// equal to the script's `local why=` line by a test.
pub(crate) const PROBE_TICK_WHY_SUFFIX: &str = ".why";

/// The most of it a refusal carries. The file is root's own in the
/// runner's 0700 runtime directory, so this is a bound on a sentence,
/// not a defence: nothing is read whole on the door's path.
pub(crate) const PROBE_TICK_WHY_MAX: usize = 1024;

/// THE TICK'S OWN REASON (backlog dda26693, review acab446c F3). A tick
/// that refused used to say why only on the runner's journal, while this
/// refusal sent the car's reader to the converge packet — written every
/// ten minutes, and `verified` while ticks failed for a cause of their
/// own (the 30 s fetch, a runuser bound under load). `None` when there is
/// no reason file, or nothing in it.
fn tick_reason(marker: &Path) -> Option<String> {
    use std::io::Read;
    let mut name = marker.as_os_str().to_os_string();
    name.push(PROBE_TICK_WHY_SUFFIX);
    let mut bytes = Vec::new();
    std::fs::File::open(PathBuf::from(name))
        .ok()?
        .take(PROBE_TICK_WHY_MAX as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}

/// THE HOST NAMES ALL THREE, OR NO PROBE (decision c98c79aa, question
/// `fallback`; backlog 703358ce, 2026-10-09). The unattended door runs
/// car text only as the account the host names (`BOSS_PROBE_USER`), in
/// the view it names (`BOSS_PROBE_DIR`), on a tick whose marker it names
/// (`BOSS_PROBE_VERIFIED_FILE`) — all three set by the one drop-in
/// infra/forge/probe-account.sh writes on the forge's converge. Until
/// this, an unset account was filled with `david` and an unset view with
/// `/home/david/boss`, and a host naming no marker "behaved as it did":
/// so a forge whose drop-in was never written, or was lost, ran car text
/// as the checkout's owner — who reaches root on that host. Measured
/// unreachable on the forge when this landed (every proof since
/// 2026-10-07 records user boss-probe, uid 997), which is why it can be a
/// refusal now: the transition the default covered is over. Any one
/// missing is an environment refusal, named, so the car records
/// did-not-run and the hourly recheck retries it once the converge has
/// written the drop-in.
pub(crate) fn unnamed_probe_host_refusal(
    user: Option<&str>,
    dir: Option<&Path>,
    marker: Option<&Path>,
) -> Option<String> {
    let unset: Vec<&str> = [
        (user.is_none(), "BOSS_PROBE_USER"),
        (dir.is_none(), "BOSS_PROBE_DIR"),
        (marker.is_none(), PROBE_VERIFIED_FILE_ENV),
    ]
    .into_iter()
    .filter_map(|(missing, name)| missing.then_some(name))
    .collect();
    if unset.is_empty() {
        return None;
    }
    Some(format!(
        "this host does not name the probe account's {} — no probe runs here until the \
         drop-in infra/forge/probe-account.sh writes on this host's converge names it, and \
         never as the checkout's owner or as root",
        unset.join(", ")
    ))
}

/// [`execute_with`] with the empty token directory's making handed in,
/// so the failure to make it can be pinned without filling a disk.
fn execute_given(
    probe: &str,
    shell: &Shell,
    no_token: Result<crate::door_env::NoTokenInReach>,
) -> Result<Outcome> {
    execute_gated(probe, shell, no_token, probe_account_marker().as_deref())
}

/// The marker this host names, if it names one — the ONE read of the
/// environment both askers share: the unattended door before it touches
/// the probe's directory ([`unattended_ground`]) and every door before
/// `runuser` ([`execute_given`]).
fn probe_account_marker() -> Option<PathBuf> {
    std::env::var_os(PROBE_VERIFIED_FILE_ENV).map(PathBuf::from)
}

/// WHAT THE UNATTENDED DOOR HOLDS BEFORE THE PROBE RUNS: the shell with
/// the car's own instant, and the tree observed where the probe will
/// run — or the account's refusal, and then NEITHER.
///
/// THE ACCOUNT IS ASKED FIRST (backlog dda26693, review acab446c F4).
/// Both of the others run programs in the probe's directory: the instant
/// is root's own `git` there with `safe.directory` set, and the
/// observation is `pwd`, `id` and `git` through `runuser` as the named
/// user. Until this, `run_unattended` did both and only then reached
/// [`probe_account_refusal`] inside `execute_with`, so on a tick whose
/// verify had refused the account or its view, root's git still opened
/// that view and three programs still ran as the unverified account. No
/// car text ran, which is the claim that matters, but "no marker, nothing
/// runs" was not literally true of this door. Now it is: a refusal
/// returns before either, and the caller records it as the environment's
/// the same way `execute_with` would have.
///
/// AND THE HOST IS ASKED BEFORE THE ACCOUNT: a host that names no
/// account, view or marker refuses here ([`unnamed_probe_host_refusal`]),
/// so the shell [`Shell::unattended`] builds without them — no user, no
/// directory — never reaches `execute_with`, where no user would mean
/// this process's own: root, at the forge's ops runner.
///
/// AND NO FILE BEFORE EITHER (backlog dda26693, review 37f44583 F1): the
/// reader half of the shell — the port table root reads out of the probe's
/// directory, and `base`, the system of record this verb resolved — is
/// added here, after the answer, so a refusal returns before root opens
/// anything there, not only before it runs anything.
pub(crate) fn unattended_ground(
    shell: Shell,
    merge_ref: Option<&str>,
    base: &str,
) -> (Shell, ProbeObservation, Option<String>) {
    let marker = probe_account_marker();
    let refused = unnamed_probe_host_refusal(
        shell.user.as_deref(),
        shell.cwd.as_deref(),
        marker.as_deref(),
    )
    .or_else(|| {
        shell
            .user
            .as_deref()
            .and_then(|user| probe_account_refusal(user, marker.as_deref()))
    });
    if refused.is_some() {
        return (shell, ProbeObservation::default(), refused);
    }
    let tree = shell.cwd.clone();
    let shell = shell
        .with_probe_reader(tree.as_deref(), base)
        .with_car_instant(merge_ref);
    let tree = probe_tree(&shell);
    (shell, tree, None)
}

/// [`execute_given`] with the probe account's marker handed in, so the
/// stop can be pinned without this process's environment.
fn execute_gated(
    probe: &str,
    shell: &Shell,
    no_token: Result<crate::door_env::NoTokenInReach>,
    marker: Option<&Path>,
) -> Result<Outcome> {
    // Car-written text runs below: no machine token in reach of it, at
    // EITHER door, and not by the shell's own `strip` — removing the
    // directory's name falls back to boss-core's default, where a host
    // that mounts the Secret there keeps the token (backlog 844b936e).
    // Here, rather than in each constructor, so no Shell can be built
    // that runs a probe with it. Made before anything else, so a
    // failure to make it runs nothing — and is said as the host's
    // refusal, not the car's failure (backlog 2e1f609e).
    let no_token = no_token
        .map_err(|e| anyhow::Error::new(EnvironmentRefusal::before_execution(format!("{e:#}"))))?;
    let as_root = shell.user.is_some() && running_as_root();
    // THE ACCOUNT FIRST, before the reader door, the channel, the argv and
    // `runuser`: a door that drops to a user runs nothing unless that user
    // verified this tick — and opens no reader door for it either, so an
    // unverified account is never handed a socket (decision c98c79aa).
    if let Some(cause) = shell
        .user
        .as_deref()
        .and_then(|user| probe_account_refusal(user, marker))
    {
        return Err(anyhow::Error::new(EnvironmentRefusal::before_execution(
            cause,
        )));
    }
    // THE READER DOOR, IF THIS HOST HOLDS A READER CREDENTIAL (design
    // b35c22b4; review 0bd6a9c2, B1). `None` is a host with no
    // credential file: no door, and everything below is the read path
    // as it was — `BOSS_SOR_USER` on the LAN, which an enforcing gate
    // refuses 401 by itself and `curl -f` makes loud. An ERROR is a
    // credential that is there and cannot be used or is not accepted as
    // a reader: the probe does not run, and that is said as the host's
    // refusal. Never the quiet fallback — a probe that silently read
    // without the door it was meant to have is how an absence assertion
    // passes against a narrower world (61085a9e).
    let reader = match &shell.reader {
        Some(config) => {
            crate::probe_reader::open(config, door_recipient(shell, as_root)).map_err(|error| {
                anyhow::Error::new(EnvironmentRefusal::before_execution(format!("{error:#}")))
            })?
        }
        None => None,
    };
    let bound = shell.bound(reader.is_some());
    let clamped = shell.clamp_notice(reader.is_some());
    if let Some(notice) = &clamped {
        eprintln!("boss prove: {notice}.");
    }
    // The channel: a file of this process's own, named so two operators
    // (or two rechecks) on one box never share it — the uid and pid by
    // `own_temp_path` (307df975), the clock within one process. It is
    // created empty and removed after the read; the prelude opens it
    // for append. As
    // on the forge, the channel must not be able to take the probe down
    // with it: a temp dir that refuses the file leaves the env unset,
    // the prelude falls back to /dev/null, and the probe still runs —
    // with an empty channel, which reads as today's verdict. When the
    // probe runs as another user the file is opened to everyone (the
    // forge's `chmod 666`): it holds command names and nothing else,
    // and a channel the probe cannot append to is a channel that never
    // names the tool.
    let channel = crate::own_temp::own_temp_path(&format!(
        "boss-prove-notfound-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let channel = std::fs::write(&channel, "").is_ok().then_some(channel);
    #[cfg(unix)]
    if let (Some(c), true) = (&channel, as_root) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(c, std::fs::Permissions::from_mode(0o666));
    }
    let argv = shell.command_line_under(probe, as_root, bound);
    let mut cmd = shell.command(&argv);
    // Neither name reaches the probe from anywhere but the line below:
    // not from this process's own environment, and not from the shell's
    // `env`. A `BOSS_SOR_DOOR` left standing there would send every
    // `boss-sor-read` of a probe that was given NO door to a socket
    // somebody else chose (review 991bb439, N4).
    cmd.env_remove(crate::probe_reader::CREDENTIAL_ENV)
        .env_remove(crate::probe_reader::DOOR_ENV);
    if let Some(reader) = &reader {
        cmd.env(crate::probe_reader::DOOR_ENV, reader.socket());
    }
    // Admission is independent of the optional diagnostic channel.
    // Its presence also keeps nested session doors out of a deliberately
    // tokenless probe; an unavailable channel must not erase that fact.
    cmd.env(
        "BOSS_PROBE_NOTFOUND",
        channel.as_deref().unwrap_or_else(|| Path::new("")),
    );
    // LAST, so nothing the shell set can name a token; the address its
    // door resolved stays, because reading the record is the probe's job.
    no_token.token_only(&mut cmd);
    let began = std::time::Instant::now();
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("could not run the probe ({}): {e}", argv[0]));
    let elapsed_ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
    let caught = channel
        .as_deref()
        .map(|c| {
            let caught = std::fs::read_to_string(c).unwrap_or_default();
            let _ = std::fs::remove_file(c);
            caught
        })
        .unwrap_or_default();
    let out = out?;
    // A signalled probe reports no code; -1 is recorded rather than
    // silently becoming 0, because "killed" must not read as "passed".
    let exit = out.status.code().unwrap_or(-1);
    // Asked while the door still stands, and before anything is made of
    // what the probe said.
    let unserved = reader.as_ref().map(|r| r.unserved()).unwrap_or_default();
    // Likewise the door's read log, while it stands: a read the kill
    // left unanswered is still in it, in flight (backlog a32af300).
    let timing = Timing {
        elapsed_ms,
        bound_s: bound,
        reads: reader.as_ref().map(|r| r.reads()).unwrap_or_default(),
    };
    if let Some(cause) = unserved_read_refusal(&unserved, exit) {
        return Err(anyhow::Error::new(EnvironmentRefusal {
            cause,
            executed: Some(Outcome {
                exit,
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                missing_tools: missing_tools(&caught),
                timing: Some(timing),
            }),
        }));
    }
    let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // `timeout` exits 124 for a probe it had to stop; the record says
    // so where the probe's own last words are, as the forge's did.
    if let (Some(t), TIMEOUT_EXIT) = (bound, exit) {
        stderr.push_str(&format!("\n[boss prove: killed at {t}s timeout]\n"));
        // ...and which read it was waiting on, where the reader door
        // took one: the sentence car 5159b6b6's attempt was missing.
        for read in timing.reads.iter().filter(|read| read.status.is_none()) {
            stderr.push_str(&format!(
                "[boss prove: still waiting on {} after {}.{:03}s]\n",
                read.path,
                read.ms / 1000,
                read.ms % 1000
            ));
        }
        // The record says whose bound it was when it was not the one
        // this door asked for.
        if let Some(notice) = &clamped {
            stderr.push_str(&format!("[boss prove: {notice}]\n"));
        }
    }
    Ok(Outcome {
        exit,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr,
        missing_tools: missing_tools(&caught),
        timing: Some(timing),
    })
}

/// What `timeout(1)` exits when the command it ran did not finish.
const TIMEOUT_EXIT: i32 = 124;

/// Is this expectation a bare number?
///
/// `inf` and `nan` parse as floats and are not what anyone means by a
/// numeric expectation, so a digit is required as well.
pub(crate) fn is_bare_number(want: &str) -> bool {
    want.chars().any(|c| c.is_ascii_digit()) && want.parse::<f64>().is_ok()
}

/// Could this character be part of the same number token?
fn joins_a_token(c: char) -> bool {
    c.is_alphanumeric() || c == '.' || c == '_'
}

/// Did the probe print what was expected?
///
/// A token expectation is a substring test, which is what every good
/// probe relies on (`claim:ok` inside a longer line). A NUMERIC one is
/// a whole-token test, because a substring test accepted `10` as proof
/// of `1` (421b3032) — the claim was true and the proof checked
/// nothing. One definition, used by the record path and by `--recheck`.
pub(crate) fn observed(printed: &str, want: &str) -> bool {
    if !is_bare_number(want) {
        return printed.contains(want);
    }
    printed.match_indices(want).any(|(i, _)| {
        let before = printed[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !joins_a_token(c));
        let after = printed[i + want.len()..]
            .chars()
            .next()
            .is_none_or(|c| !joins_a_token(c));
        before && after
    })
}

/// A bare number is a weak expectation even when it matches, so say so
/// at record time and name the shape that cannot lie. Returns the
/// warning text, or `None` when the expectation is already a token.
pub(crate) fn bare_number_warning(want: &str) -> Option<String> {
    is_bare_number(want).then(|| {
        format!(
            "boss prove: WEAK EXPECTATION — --expect {want:?} is a bare number, so the proof \
             rests on the probe's output being read correctly by a human later.\n  \
             It is compared as a whole token (a substring test would accept 10 as proof of 1), \
             but a number still says nothing about WHICH number was meant.\n  \
             The shape that cannot lie asserts inside the probe and prints a unique token:\n    \
             $ test $({want} …) -eq {want} && echo claim:ok    --expect 'claim:ok'"
        )
    })
}

/// Decide whether an outcome is evidence, or refuse and say why.
///
/// This is the whole gate, and it is deliberately only two rules: the
/// probe must have exited zero, and — unless the caller downgraded to
/// `exit_only` — the expected string must actually appear in what it
/// printed (see `observed` for what "appear" means). Both refusals
/// quote the streams, because a refusal that hides the output makes
/// the reader go hunting for it.
pub(crate) fn judge(o: &Outcome, expect: Option<&str>) -> Result<()> {
    if o.exit != 0 {
        bail!(
            "the probe exited {}, so it is not proof of anything.\n\
             \n  stdout: {}\n  stderr: {}\n\n\
             A probe that fails is evidence AGAINST the claim. Fix the change, \
             or fix the probe if the probe is what is wrong — but nothing is \
             recorded either way. If the change LANDED and the claim is \
             measured false, that is a terminal of its own: `boss prove <car> \
             --disproved --superseded-by <remedy>` records the failing run and \
             closes the car (08664157).",
            o.exit,
            if o.stdout.trim().is_empty() {
                "(empty)"
            } else {
                o.stdout.trim()
            },
            if o.stderr.trim().is_empty() {
                "(empty)"
            } else {
                o.stderr.trim()
            },
        );
    }
    if let Some(want) = expect {
        // Both streams count: plenty of real probes report on stderr.
        if !observed(&o.stdout, want) && !observed(&o.stderr, want) {
            bail!(
                "the probe exited 0 but never printed {want:?}, so it did not \
                 observe what was claimed.{}\n\
                 \n  stdout: {}\n  stderr: {}\n\n\
                 An exit code alone is a weak assertion — `echo hi` exits 0 too. \
                 Either the change is not in prod, or the probe is looking in the \
                 wrong place.",
                if is_bare_number(want) {
                    format!(
                        "\n  {want:?} is a number, so it is compared as a whole token: a \
                         substring test would accept 10 as proof of 1, which is how a proof \
                         passes for the wrong reason (421b3032). If the number IS in the \
                         output as part of a longer one, assert it inside the probe instead: \
                         `test $(…) -eq {want} && echo claim:ok`."
                    )
                } else {
                    String::new()
                },
                if o.stdout.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stdout.trim()
                },
                if o.stderr.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stderr.trim()
                },
            );
        }
    }
    Ok(())
}

/// WHAT FAILED, NOT WHAT MIGHT HAVE — the verdict half of 4fccc595.
///
/// `judge` says the probe exited nonzero and quotes the streams. That
/// was not enough twice over on car a0ab90a5: the recorded verdict
/// offered two possibilities ("the claim is not holding, or the probe is
/// wrong"), and the evidence under it was an empty string and a
/// rewritten exit code. A reader had to re-derive what the system
/// already held, which is the defect class, not the incident.
///
/// So this names ONE thing when the record supports naming one, and
/// nothing when it does not. Order matters: the self-contradictory shape
/// is checked FIRST because a probe carrying it never had a verdict to
/// give — its nonzero exit says nothing about production either way, and
/// the measured probe carried both findings at once. A crashed numeric
/// test comes next, for the same reason and one more: its exit code is
/// whatever the `||` branch behind it chose — 75 on both measured cars
/// — so the code is not just uninformative, it points the wrong way.
pub(crate) fn failure_diagnosis(probe: &str, o: &Outcome) -> Option<String> {
    if o.exit == 0 {
        return None;
    }
    if let Some(evidence) = quoting_was_mangled(probe, o) {
        return Some(format!(
            "THE PROBE'S QUOTING WAS MANGLED, so exit {exit} is not a verdict on the claim \
             — the shell never ran the probe that was written. {evidence}\n  \
             A stored probe carrying `\\\"` was escaped on the way in: the backslash-quotes \
             reach the tool as literal characters, so the words of a pattern become \
             filenames and the guard arm meant for \"nothing to judge yet\" catches the \
             wreckage and exits {exit}. Re-store the probe with SINGLE quotes, or pass it \
             through `--park-probe-file`, where no word expansion happens at all. Then \
             re-run it: this says nothing about whether the change is in production \
             (302bc2f2 — two cars sat unprovable for days this way, hourly rechecked, \
             while both claims were already true on main).",
            exit = o.exit,
        ));
    }
    if boss_jobs::probe::asserts_its_own_negation(probe) {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                "\n  AND THE NUMBER THAT WOULD PROVE IT IS GONE: a bare `|| exit {n}` replaced \
                 jq's own status with {n}, so 4 (produced nothing — the claim HELD) and 5 \
                 (called `error` — the claim FAILED) read identically. Keep it: \
                 `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE PROBE IS SELF-CONTRADICTORY, so this exit code is not a verdict on the \
             claim. It runs `jq -e` over a filter whose success branch is `empty`.\
             \n  {evidence}{rewritten}\n  \
             Nothing here says whether the change is in production. Fix the probe and \
             re-run — and if the car is parked, re-park it so the arrival rule does not \
             run this text again.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        ));
    }
    if let Some(line) = crashed_comparing_a_non_number(&o.stderr) {
        return Some(format!(
            "THE PROBE CRASHED comparing a non-number (jq printed null?), so exit {exit} is \
             not a verdict on the claim — it is whichever `||` branch caught the crash. \
             bash's `[` said:\n  {line}\n  \
             Guard the value before the numeric test — `// empty` in the jq filter, or \
             `case \"$n\" in ''|*[!0-9]*) echo \"not yet: no number\"; exit 75;; esac` — so a \
             missing value says not-yet BY NAME instead of crashing into the not-yet branch. \
             Fix the probe and re-park; a recheck re-runs a crash forever (68081368).",
            exit = o.exit,
        ));
    }
    if let Some(line) = quoting_mangled_its_arguments(&o.stderr) {
        return Some(format!(
            "THE PROBE'S QUOTING WAS MANGLED, so exit {exit} is not a verdict on the claim \
             — a tool was handed a filename with a quote in it, which means the argument \
             list was split somewhere the author did not intend. The tool said:\n  {line}\n  \
             This is the backtick trap's cousin: prose damaged between authoring and \
             storage. SINGLE-quote the probe when you pass it, or use the flag's `-file` \
             twin, where no word expansion happens at all. Fix the probe and re-park — a \
             recheck re-runs damaged text forever, and every run reports the probe's own \
             not-yet sentence, which reads exactly like an honest wait (302bc2f2).",
            exit = o.exit,
        ));
    }
    if o.stdout.trim().is_empty() && o.stderr.trim().is_empty() {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                " A bare `|| exit {n}` in the probe is the usual cause: it replaces the \
                 failing command's status with {n} and prints nothing, so the one number \
                 that named the cause is gone."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE FAILURE CANNOT BE READ: the probe exited {exit} and printed NOTHING on \
             either stream, so this is not a verdict on the claim — it is a missing \
             record.{rewritten}\n  \
             Make each outcome speak: print a named token on success, and on failure echo \
             what failed with the status that caused it before exiting. A probe whose \
             failure is silent costs its next reader the whole diagnosis (4fccc595: 18 \
             hours, on a probe that could not pass).",
            exit = o.exit,
        ));
    }
    None
}

/// WHAT A TOOL SAYS WHEN THE PROBE'S OWN QUOTING REACHED IT LITERALLY
/// (backlog 302bc2f2). A probe stored with backslash-quotes —
/// `grep -c \"boss step complete\"` — hands grep the pattern `"boss`
/// and then the FILENAMES `step` and `complete"`. grep warns about
/// each, the substitution comes back non-numeric, and the probe's own
/// guard arm catches it and exits 75. The record then says "not yet"
/// in the probe's own words, which is the one answer nobody re-reads.
///
/// Measured 2026-09-20: two shed cars sat unprovable for days this
/// way, hourly rechecked, while BOTH claims were already true on main.
///
/// THE SIGNAL IS DECIDABLE AND THE OBVIOUS ONE IS NOT. "No such file
/// or directory" alone is a false positive — a probe that greps a file
/// production has not written yet is an HONEST not-yet, and several in
/// the shed are exactly that. Two signals distinguish them, and either
/// is enough:
///
/// 1. The probe text carries a backslash-quote AND a tool reported a
///    lookup failure. A correctly quoted probe cannot produce both.
/// 2. The probe's own message arrives WRAPPED IN QUOTE CHARACTERS. A
///    working `echo 'not yet: …'` never emits them; seeing them means
///    the echo's quotes were literal, so every other quote in that
///    probe was too.
const LOOKUP_FAILURE_MARKERS: [&str; 2] = ["No such file or directory", "cannot open"];

/// A literal backslash followed by a quote, as it appears in a stored
/// probe whose quoting was escaped on the way in.
const ESCAPED_QUOTE: &str = "\\\"";

/// Did the shell read this probe as something other than what it says?
/// `None` when the probe ran as written — including every honest
/// not-yet. See [`LOOKUP_FAILURE_MARKERS`] for why the two signals are
/// what they are.
pub(crate) fn quoting_was_mangled(probe: &str, o: &Outcome) -> Option<String> {
    let lookup_failed = o
        .stderr
        .lines()
        .chain(o.stdout.lines())
        .find(|l| LOOKUP_FAILURE_MARKERS.iter().any(|m| l.contains(m)))
        .map(str::trim);
    let said_in_quotes = what_it_said(o).filter(|l| l.starts_with('"'));

    let evidence = match (lookup_failed, &said_in_quotes) {
        (Some(line), _) if probe.contains(ESCAPED_QUOTE) => {
            format!("a tool could not find what it was handed:\n  {line}")
        }
        (_, Some(line)) => format!(
            "the probe's own message arrived wrapped in quote characters, which a working \
             `echo` never emits:\n  {line}"
        ),
        _ => return None,
    };
    Some(evidence)
}

/// WHAT BASH SAYS WHEN `[` OR `((` IS HANDED A NON-NUMBER (backlog
/// 68081368). Cars dd1d872d and 2e4d3bce each recorded the not-yet
/// sentence over a stderr of `bash: line 10: [: null: integer expression
/// expected`: jq printed null, `[ "$n" -ge 1 ]` exited 2, and the `||`
/// branch meant for "no such packet yet" exited 75. Nothing was judged.
///
/// Until backlog 9f00a805 (car 2) the forge's shell twin carried this
/// list a second time, as a grep pattern, pinned equal by a test; the
/// twin is retired and this is the one copy.
pub(crate) const NUMERIC_CRASH_MARKERS: [&str; 3] = [
    "integer expression expected",
    "unary operator expected",
    "syntax error: invalid arithmetic operator",
];

/// The first stderr line that carries one of [`NUMERIC_CRASH_MARKERS`],
/// trimmed — the line the verdict quotes. `None` when the probe crashed
/// on nothing of the kind, so the other diagnoses get their turn.
pub(crate) fn crashed_comparing_a_non_number(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| NUMERIC_CRASH_MARKERS.iter().any(|m| line.contains(m)))
}

/// WHAT A TOOL SAYS WHEN THE PROBE'S QUOTING WAS MANGLED (backlog
/// 302bc2f2). Car c4c1ac17's stored probe carried backslash-escaped
/// quotes, so `grep -c \"boss step complete\"` ran with `\"boss` as the
/// pattern and `step` and `complete"` as FILENAMES. grep warned about
/// each, exited 2, and the probe's own `case` arm turned that into exit
/// 75 — the one answer nobody re-reads. The claim was already true on
/// main; the car sat unproven for days over its own text, and the
/// hourly recheck re-ran it forever, each run printing the same
/// reassuring sentence.
///
/// THE RULE IS NARROW ON PURPOSE, and the narrowness is the whole
/// design. "No such file or directory" ALONE is not evidence of damage:
/// a probe that greps a file which has not landed on main yet says
/// exactly that, and it is an honest not-yet. What is never honest is a
/// missing operand whose NAME CARRIES A DOUBLE QUOTE — no probe reads a
/// file called `complete"`, so the quote is the residue of an extra
/// escaping layer between authoring and storage.
///
/// WHY NOT REFUSE THE TEXT AT THE DOOR INSTEAD. That was the first
/// proposal and it is wrong, measured: of 254 stored probes, 71 carry a
/// backslash-quote and nearly all are CORRECT — `grep -q "name =
/// \"folded_into\""` is how sh writes a literal quote inside a quoted
/// string. Restricting to a backslash-quote in unquoted context still
/// flags two correct probes, both `case` patterns where `*\"x\"*` is the
/// idiomatic way to match a literal quote. The text cannot tell the
/// damage from the idiom; the RUN can, because only the damaged one
/// makes a tool report a filename with a quote in it.
///
/// It fails toward NotProven with the cause attached, never toward a
/// green, so the worst a false positive does is make a car look
/// troubled — which is the direction CLAUDE.md asks this to fail.
pub(crate) const MISSING_OPERAND_MARKERS: [&str; 3] =
    ["No such file or directory", "cannot open", "can't open"];

/// The first stderr line that names a missing operand whose name
/// carries a double quote — the line the verdict quotes. `None` when
/// nothing on stderr shows that shape, so an honestly-missing file
/// keeps its not-yet.
pub(crate) fn quoting_mangled_its_arguments(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.contains('"') && MISSING_OPERAND_MARKERS.iter().any(|m| line.contains(m)))
}

/// [`judge`], with the diagnosis attached when the record supports one.
/// Every path that runs a probe goes through here rather than `judge`,
/// so a refusal the operator reads and a refusal `--recheck` records
/// cannot say different things.
pub(crate) fn judge_probe(probe: &str, o: &Outcome, expect: Option<&str>) -> Result<()> {
    judge(o, expect).map_err(|e| match failure_diagnosis(probe, o) {
        Some(d) => anyhow::anyhow!("{e}\n\n{d}"),
        None => e,
    })
}

/// THE EXIT CODE THAT MEANS "NOT YET" — the probe's, and now this
/// verb's. 75 is EX_TEMPFAIL, the code a recorded probe exits with
/// when the world is not ready to judge the claim ("not yet: no
/// disk-report request carrying for_sweep yet"). This verb exits the
/// same number for the same reason — under `--unattended` that IS the
/// ops-request's `exit_code`, the verdict a reader of the packet sees
/// first — so a script wrapping either door tells the three answers
/// apart the same way.
pub(crate) const NOT_YET_EXIT: i32 = 75;

/// THE EXIT CODE THAT MEANS "COULD NOT RUN HERE" — the third code
/// (46f67333). 3 when fd 9 caught a tool, 75 when the probe ran and
/// said not yet, 1 when it ran and did not prove: three different
/// things to do about it, so three numbers for the ops-request to
/// carry. The forge's shell twin exited the same three until it was
/// retired (9f00a805 car 2); `the_unattended_door_exits_one_of_three_codes`
/// pins them here.
pub(crate) const UNRUNNABLE_EXIT: i32 = 3;

/// THE EXIT CODE THAT MEANS "REFUSED" at the unattended door — the
/// forge runner's `refuse`, kept: the car is not a merged ship-a-change
/// car with a recorded probe and an open `proven` step, so nothing ran
/// and nothing was judged. Distinct from 1 (ran, not proven) so a
/// reader of the ops-request does not go looking for a probe run that
/// never happened. The host refusing the probe's environment — no empty
/// token directory could be made ([`EnvironmentRefusal`], backlog
/// 2e1f609e) — exits the same code for the same reason: nothing ran.
pub(crate) const REFUSED_EXIT: i32 = 2;

/// THE THREE-WAY READING (backlog 726562de). Two doors run a car's
/// probe, and until 2026-09-14 they read exit 75 in opposite
/// directions. The forge runner learned the not-yet protocol on
/// 2026-09-13 (feat/a-probe-can-say-not-yet): a clean exit 75 is NOT
/// YET — the probe ran, found nothing to judge yet, and said so — so it
/// stamps `proof_attempt{not_yet:true}`, leaves `proven` ready and lets
/// the daily recheck run it again. This door was not taught: `judge`
/// has two rules, and exit 75 broke the first, so `boss prove <car>
/// --from-car` on the SAME run printed "A probe that fails is evidence
/// AGAINST the claim". One record, two verdicts, depending on which
/// door happened to run it — found by the pin that could not make the
/// wording equal because one side had no wording at all.
///
/// So the verdict is an enum with three arms rather than a `Result`
/// with two, and every path that runs a probe reads it here. Not-yet
/// is what a probe SAYS, not merely a number it exits with: it takes
/// exit 75 AND no [`failure_diagnosis`] — the same order the forge's
/// verdict block uses, where a crashed numeric test (68081368) and a
/// silent exit (4fccc595) are checked before the code is read, because
/// in both the 75 is whichever `||` branch caught a probe that never
/// judged anything.
///
/// UNRUNNABLE IS THE FOURTH ARM, and the first one read after the two
/// rules (46f67333). The forge's verdict block checks fd 9's channel
/// before anything else — a probe that never resolved its tool has no
/// exit code worth reading and no streams worth diagnosing — and, like
/// the forge, this door checks it only on a run the two rules refused:
/// a probe that lost a tool on a `||` branch and still printed its token
/// proved the claim at both doors.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// Exit 0 and the expectation printed — [`judge`]'s two rules.
    Proven,
    /// Exit 75 with something to say and nothing to diagnose. `said` is
    /// the probe's own not-yet line ([`what_it_said`]).
    NotYet { said: String },
    /// The not-found channel named a tool: the probe DID NOT RUN, so
    /// nothing was judged. `missing` is [`Outcome::missing_tools`].
    Unrunnable { missing: Vec<String> },
    /// Everything else: [`judge_probe`]'s refusal, diagnosis attached.
    NotProven(anyhow::Error),
}

/// Read one run four ways. See [`Verdict`].
pub(crate) fn verdict(probe: &str, o: &Outcome, expect: Option<&str>) -> Verdict {
    let refused = match judge_probe(probe, o, expect) {
        Ok(()) => return Verdict::Proven,
        Err(e) => e,
    };
    if !o.missing_tools.is_empty() {
        return Verdict::Unrunnable {
            missing: o.missing_tools.clone(),
        };
    }
    if o.exit == NOT_YET_EXIT
        && failure_diagnosis(probe, o).is_none()
        && let Some(said) = what_it_said(o)
    {
        return Verdict::NotYet { said };
    }
    Verdict::NotProven(refused)
}

/// WHAT THE PROBE SAID, in one line: the first non-empty line of
/// stderr, else of stdout, trimmed and cut as the forge cuts it (300
/// characters). stderr first because that is where a tool puts its
/// diagnosis — jq's `error(…)`, curl's message, bash's command-not-
/// found. One definition, so the reason clause both doors quote is the
/// same line.
pub(crate) fn what_it_said(o: &Outcome) -> Option<String> {
    o.stderr
        .lines()
        .chain(o.stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
}

/// THE NOT-YET SENTENCE — `proof_attempt.why` when the probe said not
/// yet, and what the operator reads. The lead word, the reason clause,
/// and the disclaimer that this is not a verdict against the change
/// are load-bearing text a reader of the car acts on; both doors
/// compose it here, so it cannot depend on which door ran the probe.
pub(crate) fn not_yet_why(host: &str, said: &str) -> String {
    format!(
        "NOT YET: the probe ran on {host} and said the claim cannot be judged until \
         something happens — {said}. Not a verdict against the change; \
         recheck-failing-probes-hourly runs it again."
    )
}

/// THE DID-NOT-RUN SENTENCE — `proof_attempt.why` when the channel
/// named a tool, and what the operator reads. The lead word, the `on
/// <host>: <tools> not found` clause a reader acts on, and the
/// disclaimer that this says nothing about the change are the same at
/// both doors. The middle sentence is the door's own — the unattended
/// door names the forge host's vantage (the probe user and the
/// converged checkout), the hand door names the machine the operator
/// is standing at — because what to do instead differs by door
/// (`boss_jobs::probe`).
pub(crate) fn unrunnable_why(host: &str, missing: &[String]) -> String {
    format!(
        "THE PROBE DID NOT RUN on {host}: {list} not found. It ran HERE, as you, with this \
         machine's PATH, and bash could not resolve the tool, so nothing about the claim was \
         judged — the same finding the unattended door records from its fd-9 channel. This \
         says nothing about whether the change works; re-probe from a vantage this host has, \
         or record the car as event-bound.",
        list = missing.join(", ")
    )
}

/// [`unrunnable_why`] as the unattended door says it: the forge host,
/// the user the probe ran as, the checkout it ran in — and where the
/// tool the builder reached for actually lives (f9304366: two kubectl
/// probes, correct from the pod, impossible on the forge).
pub(crate) fn unrunnable_why_unattended(
    host: &str,
    missing: &[String],
    user: &str,
    dir: &Path,
) -> String {
    format!(
        "THE PROBE DID NOT RUN on {host}: {list} not found. A recorded probe runs on the \
         forge host as {user} in {dir}, with this host's tools — not on the dev pod where it \
         was written, which is where cluster tools like kubectl live. This says nothing about \
         whether the change works; re-probe from a vantage this host has, or record the car \
         as event-bound.",
        list = missing.join(", "),
        dir = dir.display(),
    )
}

/// THE ATTEMPT RECORD — a run that settled nothing, written on the CAR
/// (`metadata.proof_attempt`, via the merge-PATCH), never on the step,
/// which stays open. One shape from both doors, because `--recheck`,
/// `boss orient` and the yard's shed (`apps/web/src/it/yard/yard.ts`,
/// `proofAttempt`) read it and must not learn which door wrote it;
/// `the_attempt_record_carries_the_keys_the_yard_reads` names the keys.
/// `unrunnable` and `missing_tools` are the not-found channel's
/// finding, from whichever door ran the prelude (46f67333). `expect` is
/// `null` under `--exit-only`, as `proof_json` records it.
///
/// `prior` is the car's CURRENT `proof_attempt`, which this record
/// replaces: a not-yet carries its streak forward from it
/// (`boss_jobs::car::carried_not_yet_streak`, backlog adef5ddf), because
/// the replace is the only moment the previous answer is still in hand.
/// Anything else carries no streak — `null` and `0`, never an absent
/// key, so the shape the yard reads is one shape.
///
/// `actor` is who a `boss` verb in the probe signed as
/// ([`Shell::probe_actor`]), beside `host`: car f4cdf451's probe read
/// FAILING because the forge ran as nobody what the builder rehearsed
/// as itself, and the attempt named the host and not the actor (backlog
/// 371b0443).
#[allow(clippy::too_many_arguments)]
pub(crate) fn attempt_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    actor: &str,
    at: &str,
    why: &str,
    prior: Option<&Value>,
) -> Value {
    let not_yet = why.starts_with("NOT YET");
    let (since, runs) = if not_yet {
        let (s, n) = boss_jobs::car::carried_not_yet_streak(prior, probe, at);
        (json!(s), n)
    } else {
        (Value::Null, 0)
    };
    json!({
        (boss_jobs::car::NOT_YET_SINCE): since,
        (boss_jobs::car::NOT_YET_RUNS): runs,
        "at": at,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        "actor": actor,
        "probe": probe,
        "expect": expect,
        "why": why,
        "unrunnable": !o.missing_tools.is_empty(),
        // The flag is the sentence's: a record whose `not_yet` and `why`
        // disagree cannot be written from here.
        "not_yet": not_yet,
        "missing_tools": o.missing_tools,
        // Where the run's time went (backlog a32af300): null and empty
        // for an outcome no run produced, never an absent key.
        "elapsed_ms": o.timing.as_ref().map(|t| t.elapsed_ms),
        "bound_s": o.timing.as_ref().and_then(|t| t.bound_s),
        "reads": o.timing.as_ref().map_or_else(Vec::new, |t| {
            t.reads
                .iter()
                .map(|read| {
                    json!({
                        "path": read.path,
                        "ms": read.ms,
                        "status": read.status,
                        "in_flight": read.status.is_none(),
                    })
                })
                .collect()
        }),
    })
}

/// THE DECLARED WAIT, OBSERVED (backlog b461341d). A not-yet from a car
/// that declared what it waits on (`boss_jobs::car::WAITS_ON`) with a
/// `seen` check runs that check here, with the same shell and on the
/// same host as the probe, and stamps the attempt with what it found:
/// `waits_on_exit` (the check's exit, `null` when it could not run) and
/// `waits_on_seen_at` (when the event was first seen, `null` when it was
/// not). The shed then reads "seen, and the probe STILL said not yet" as
/// ours to read at once. A successful negative reading requires exit 1
/// and JSON stdout containing `"observation":"unseen"`; a legacy nonzero
/// outcome or execution error is unread, not a negative observation.
///
/// A check that cannot run — refused by the door's own admission rule,
/// or naming a tool this host lacks — is NOT a sighting: it leaves the
/// wait unread. The qualified exit is null, while any actual process exit
/// and its whole stdout/stderr remain separate from missing-tool,
/// admission and execution diagnoses. Anything but a not-yet, or
/// a car with no `seen` check, is returned untouched.
fn with_wait_observed(
    attempt: Value,
    car: &Value,
    shell: &Shell,
    prior: Option<&Value>,
    at: &str,
) -> Value {
    if attempt.get("not_yet").and_then(Value::as_bool) != Some(true) {
        return attempt;
    }
    let Some(seen) = car
        .get("metadata")
        .and_then(boss_jobs::car::waits_on)
        .and_then(|w| w.seen)
    else {
        return attempt;
    };
    let stdout;
    let stderr;
    let mut observation = Value::Null;
    let mut raw_exit = None;
    let mut missing_tools = Vec::new();
    let mut refusal = Value::Null;
    let mut execution_error = Value::Null;
    let (exit, coverage) = match admit(&seen, true).refusal {
        Some(r) => {
            eprintln!("boss prove: the declared wait's seen check is refused — {r}");
            stdout = None;
            stderr = None;
            refusal = json!({"rule": r.rule, "text": r.text});
            (None, None)
        }
        None => match execute_with(&seen, shell) {
            Ok(o) if o.missing_tools.is_empty() => {
                stdout = Some(o.stdout.clone());
                stderr = Some(o.stderr.clone());
                raw_exit = Some(o.exit);
                let reading = serde_json::from_str::<Value>(&o.stdout).ok();
                observation = reading
                    .as_ref()
                    .and_then(|reading| reading.get("observation").cloned())
                    .filter(|reading| reading.as_str() == Some("unseen"))
                    .unwrap_or(Value::Null);
                // Copy the executed observer's structured reading, never a
                // caller's pass flag. The shared reader validates its bounds.
                let coverage = reading
                    .and_then(|reading| reading.get("coverage").cloned())
                    .and_then(|reading| match reading {
                        Value::Object(mut fields) => {
                            fields.insert("source".into(), json!(seen));
                            Some(Value::Object(fields))
                        }
                        _ => None,
                    });
                (Some(o.exit), coverage)
            }
            Ok(o) => {
                stdout = Some(o.stdout.clone());
                stderr = Some(o.stderr.clone());
                raw_exit = Some(o.exit);
                missing_tools = o.missing_tools.clone();
                eprintln!(
                    "boss prove: the declared wait's seen check did not run — missing {}",
                    o.missing_tools.join(", ")
                );
                (None, None)
            }
            Err(e) => {
                let executed = e
                    .downcast_ref::<EnvironmentRefusal>()
                    .and_then(|r| r.executed.as_ref());
                stdout = executed.map(|o| o.stdout.clone());
                stderr = executed.map(|o| o.stderr.clone());
                raw_exit = executed.map(|o| o.exit);
                if let Some(outcome) = executed {
                    missing_tools = outcome.missing_tools.clone();
                }
                execution_error = json!(format!("{e:#}"));
                if executed.is_some() {
                    eprintln!(
                        "boss prove: the declared wait's seen check ran but its result was refused — {e}"
                    );
                } else {
                    eprintln!("boss prove: the declared wait's seen check did not run — {e}");
                }
                (None, None)
            }
        },
    };
    let observed = stamp_wait(attempt, prior, exit, at, &seen);
    match observed {
        Value::Object(mut fields) => {
            fields.insert("waits_on_coverage".into(), coverage.unwrap_or(Value::Null));
            // Preserve whole execution evidence before any display reduction.
            fields.insert("waits_on_stdout".into(), json!(stdout));
            fields.insert("waits_on_stderr".into(), json!(stderr));
            fields.insert("waits_on_raw_exit".into(), json!(raw_exit));
            fields.insert("waits_on_missing_tools".into(), json!(missing_tools));
            fields.insert("waits_on_refusal".into(), refusal);
            fields.insert("waits_on_error".into(), execution_error);
            fields.insert("waits_on_observation".into(), observation);
            Value::Object(fields)
        }
        other => other,
    }
}

/// The pure half of [`with_wait_observed`]: exit 0 is a sighting,
/// dated from the first run in an unbroken line of them.
fn stamp_wait(
    attempt: Value,
    prior: Option<&Value>,
    exit: Option<i32>,
    at: &str,
    observer: &str,
) -> Value {
    let same_observer =
        prior.filter(|p| p.get("waits_on_probe").and_then(Value::as_str) == Some(observer));
    let seen_at = boss_jobs::car::carried_seen_at(same_observer, exit == Some(0), at);
    match attempt {
        Value::Object(mut m) => {
            m.insert("waits_on_probe".into(), json!(observer));
            m.insert("waits_on_exit".into(), json!(exit));
            m.insert(boss_jobs::car::WAITS_ON_SEEN_AT.into(), json!(seen_at));
            Value::Object(m)
        }
        other => other,
    }
}

/// WHICH PROBES THIS DOOR WILL RUN (backlog 23b2dffa).
///
/// TWO DOORS RUN A CAR'S RECORDED PROBE AND ONLY ONE CHECKED IT. `boss
/// gate --park-probe` refuses a probe that names a tool the forge host
/// lacks, and one that reads the system of record unidentified. This
/// verb — the HAND door onto the same text, and the one an operator
/// reaches for while moving fast — applied neither, and the only trace
/// of the rule here was a comment describing the gate's refusal. One
/// rule, two enforcement sites, one of which is prose about the other,
/// is CLAUDE.md §9a in its behavioural form, so the predicates are now
/// `boss_jobs::probe` and both doors call them.
///
/// WHY IT REFUSES RATHER THAN WARNS. The operator IS present, and may
/// have a reason the check cannot see — which is an argument for a
/// warning until you notice `--recheck`: it re-runs this recorded probe
/// later, when nobody is watching, so a probe admitted by hand BECOMES
/// an unattended one. A refusal with a stated override keeps both — the
/// operator gets through, and the next reader learns it was overridden
/// and why.
///
/// AND WHY ONLY ONE OF THE TWO RULES REFUSES HERE. They are not the same
/// kind of rule. "Can this text run where it is going" is host-relative:
/// `host-absent-tools.txt` is a measurement of the FORGE, because a
/// `--park-probe` runs there, while this verb runs the probe HERE, in
/// front of the operator, one second later — and a pod-local `kubectl`
/// proof is the established shape for a claim only the cluster can show
/// (memory: proof probes from the pod). Refusing that would block
/// exactly the case where a human must prove what the forge cannot, so a
/// hand `--probe` is not judged against another machine's tools: the
/// question the manifest predicts, this verb measures. "Can its answer
/// be trusted" is host-independent, and refuses everywhere.
///
/// `--from-car` is the one path where both apply, because that text has
/// a second destination: the arrival rule runs it on the forge. There
/// the mismatch is NAMED and not refused — proving it by hand here is
/// the right move; what needs fixing is the probe the car recorded.
///
/// AND WHY THE TWO SHAPE CHECKS ONLY WARN (backlog 4fccc595). They are
/// host-independent like the unidentified read, so they are checked on
/// every probe at every door a person is standing at — but they are said,
/// not refused, and the reason is the DIRECTION OF THE LIE. An
/// unidentified read fails OPEN: the probe passes, the absence assertion
/// is green against a world it was never allowed to see, and a car closes
/// on a proof of nothing. A `jq -e` whose success branch is `empty`, and
/// a bare `|| exit n`, fail CLOSED: `judge` records nothing on a nonzero
/// exit, so the worst they can do is strand a car and misdescribe why —
/// which is exactly what they did for 18 hours, and which a line of text
/// in front of the operator fixes. Both detectors are also deliberately
/// coarse text scans (`boss_jobs::probe` says where they are wrong), and
/// a refusal cannot be spent that cheaply: a false refusal blocks a
/// correct probe, while a false warning costs a line someone ignores.
///
/// The SAME two warnings are said by `boss gate --park-probe`
/// (`ParkIntent::probe_warnings`), because that is the door where the
/// measured probe was admitted and the one moment the shape is cheap to
/// fix. Neither is said at the two UNATTENDED doors — the arrival rule
/// and `--unattended` — because nothing there reads a warning, and a
/// check nobody reads is a check that is not running (CLAUDE.md
/// §Diagnosis). What those two got instead is a VERDICT that names one
/// thing: [`failure_diagnosis`], and the `why` on the attempt record.
pub(crate) const OVERRIDE_FLAG: &str = "--probe-anyway";

pub(crate) use boss_jobs::probe::{GIT_TIME_RULE, UNIDENTIFIED_RULE, override_record};

/// Why a probe must not run: the rule that refused it — recorded when
/// an operator overrides it, so `--recheck` and every later reader find
/// the argument against the rule it was actually made against — and
/// the wording this door says. Two rules refuse here since c0ac92b8;
/// until then the override always recorded [`UNIDENTIFIED_RULE`]
/// because it was the only one.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub rule: &'static str,
    pub text: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// What this door makes of a probe: a reason not to run it, things the
/// operator should know, or neither.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    /// Why the probe must not run, unless the operator overrides it.
    pub refusal: Option<Refusal>,
    /// Things worth saying that do not stop the probe. A list because
    /// they are independent findings and a probe can trip more than one
    /// — the measured 18-hour probe (4fccc595) tripped two, and showing
    /// the operator only the first would have hidden the other.
    pub warnings: Vec<String>,
}

/// Judge a probe at this door. `from_car` says the text came from the
/// car's `proof_probe`, which means the forge will run it too.
pub(crate) fn admit(probe: &str, from_car: bool) -> Admission {
    let unidentified = boss_jobs::probe::reads_the_sor_unidentified(probe).map(|client| {
        format!(
            "THE PROBE READS THE SYSTEM OF RECORD WITH `{client}` AND NO IDENTITY, so it \
             reads as operator:unidentified — and an unidentified reader is answered with a \
             NARROWER WORLD, silently. What this verb would record is a proof of nothing.\
             \n\n{evidence}\n\n\
             Read it as a named reader instead — `boss-api GET /api/...` where that door \
             has a reader credential, `{reader} /api/...` on the forge, a curl that sends an identity \
             header, or a gateway session (POST /api/auth/guest, then curl -b). The DOOR \
             has to do the read; `jq` or `python3` fed its stdout is a parser and is not \
             refused (5dc5159d). On a host with no reader credential, every known record \
             client is refused BEFORE execution at the manual proof doors, even with a \
             stated identity: design 00227628 routes such proof to the forge's \
             `{hourly}` door instead. This guidance does not authorize an operator token \
             or a reader deposit in the dev pod.\n\n\
             If this read IS identified in a way a text check cannot see, say so and it \
             runs: {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof, because \
             `--recheck` re-runs this probe later when nobody is watching.",
            evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            reader = boss_jobs::probe::SOR_READER,
            hourly = HOURLY_PROBE_RULE,
        )
    });
    // The git-date rule (c0ac92b8) fails open the same way — a string
    // compare of mixed-offset timestamps can pass for an event that
    // never happened — so it refuses at this door as at the gate, with
    // the same evidence and the same rewrite.
    let git_time = boss_jobs::probe::reads_git_time_with_an_offset(probe).map(|token| {
        format!(
            "THE PROBE READS A GIT DATE WITH `{token}`, which carries the committer's UTC \
             offset — and a probe that compares that string against the system of record's \
             UTC timestamps lies in BOTH directions. What this verb could record is a proof \
             of nothing.\n\n{evidence}\n\n\
             This refuses the TOKEN, not the compare: whether the string reaches a \
             `[ ... \\> ... ]` would take a shell parser to know honestly, and the fix is the \
             same either way — `git log -1 --format=%ct` on one side, `date -u -d \"$ts\" +%s` \
             on the other after the empty guard, `-gt` between them. `boss gate --park-probe` \
             refuses this text too, so a car carrying it needs re-parking.\n\n\
             If the date is only printed and never compared, say so and it runs: \
             {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof.",
            evidence = boss_jobs::probe::GIT_TIME_STRING_EVIDENCE,
        )
    });
    // THE CALLER'S OWN QUEUE (backlog 371b0443) is refused only for a
    // probe typed HERE: it is recorded and re-run later by somebody else
    // (`--recheck`, the forge), who reads a different queue. A car's
    // recorded probe has landed and cannot be re-parked, so it is named
    // below instead, like the `cd` — and refusing it at the unattended
    // door would trade a FAILING the attempt now explains (it names the
    // actor) for a refusal on an ops-request nobody reads.
    let own_queue = boss_jobs::probe::reads_the_callers_own_queue(probe);
    let queue = own_queue.filter(|_| !from_car).map(|marker| {
        format!(
            "THE PROBE READS `{marker}` — the queue of whoever runs it, so what this verb \
             would record is a fact about YOU, and `--recheck` and the forge re-run it as \
             somebody else (the forge as {unidentified}).\n\n{evidence}\n\n\
             `boss gate --park-probe` refuses this text too. If the claim really is about \
             one named actor's queue and you are that actor, say so and it runs: \
             {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof.",
            unidentified = crate::identity::UNIDENTIFIED,
            evidence = boss_jobs::probe::CALLERS_OWN_QUEUE_EVIDENCE,
        )
    });
    let refusal = unidentified
        .map(|text| Refusal {
            rule: UNIDENTIFIED_RULE,
            text,
        })
        .or_else(|| {
            git_time.map(|text| Refusal {
                rule: GIT_TIME_RULE,
                text,
            })
        })
        .or_else(|| {
            queue.map(|text| Refusal {
                rule: boss_jobs::probe::CALLERS_OWN_QUEUE_RULE,
                text,
            })
        });
    let mut warnings: Vec<String> = Vec::new();
    if from_car && let Some(marker) = own_queue {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe reads `{marker}`, the queue of \
             whoever runs it: here it answers for you, and the forge runs it as {unidentified} \
             (backlog 371b0443). `boss gate --park-probe` refuses this text; prove the car \
             by hand with `--probe` and a probe every reader answers the same.",
            unidentified = crate::identity::UNIDENTIFIED,
        ));
    }
    if from_car && let Some(tool) = boss_jobs::probe::needs_absent_tool(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe invokes `{tool}`, which the \
             forge host does not have (infra/forge/host-absent-tools.txt). It runs HERE, \
             so proving by hand is fine and is the point; but the arrival rule could not \
             have run it, and any later re-run on the forge will report `unrunnable` \
             rather than a verdict (f9304366). Re-park the car with a probe the forge \
             can run, or record it as --park-proof-event."
        ));
    }
    if from_car && let Some(verb) = boss_jobs::probe::changes_directory(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe runs `{verb}`, and on the forge \
             the door has already placed it in the converged checkout; a pod path there \
             is `No such file or directory` and an exit 1 on the car (4bb6797c). It runs \
             HERE, so proving by hand is fine; `boss gate --park-probe` refuses this text, \
             so re-park the car with the `{verb}` dropped — `git show HEAD:<path>` reads \
             the converged tree from where the door puts it."
        ));
    }
    if from_car && let Some(var) = boss_jobs::probe::names_an_actor(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe assigns `{var}`, which would name \
             an actor for the `boss` verbs it runs on the forge, and a probe proves, it \
             does not act (infra/forge/host-absent-tools.txt, the `boss` block). Here it \
             runs as you, so proving by hand is fine; `boss gate --park-probe` refuses this \
             text, so re-park the car with the assignment dropped."
        ));
    }
    warnings.extend(shape_warnings(probe).map(|w| format!("boss prove: {w}")));
    Admission { refusal, warnings }
}

/// [`admit`], and then the probe judged against the CLAIM it is about to
/// be recorded as proof of (backlog 490acf06): `verified` naming an
/// endpoint answer or a latency over a probe that reads only the tree.
/// Refused for a probe typed here, as the gate refuses it, with the
/// override every other rule here takes. A car's recorded probe has
/// landed and cannot be re-parked, so it is named instead — the same
/// split as the caller's own queue above — and the reader is told the
/// claim stays NOT OBSERVED whatever this proof records.
pub(crate) fn admit_against_claim(probe: &str, verified: &str, from_car: bool) -> Admission {
    let admission = admit(probe, from_car);
    let Some(words) = boss_jobs::probe::a_tree_read_under_a_production_claim(probe, verified)
    else {
        return admission;
    };
    if from_car {
        let note = format!(
            "boss prove: NOTE — this car's claim names a production behaviour (`{words}`) and \
             its recorded probe reads only the converged tree, so what it records is a fact \
             about the source; the claim itself stays NOT OBSERVED (backlog 490acf06). \
             `boss gate --park-probe` refuses this pair; prove the car by hand with `--probe` \
             reading the answer through the reader door, the bound asserted inside it."
        );
        return Admission {
            refusal: admission.refusal,
            warnings: admission.warnings.into_iter().chain([note]).collect(),
        };
    }
    let claim = Refusal {
        rule: boss_jobs::probe::PRODUCTION_CLAIM_RULE,
        text: format!(
            "THE CLAIM NAMES A PRODUCTION BEHAVIOUR (`{words}`) AND THE PROBE READS ONLY THE \
             CONVERGED TREE — what this verb would record is a fact about the source, filed \
             under a sentence about production that nothing observed.\n\n{evidence}\n\n\
             `boss gate --park-probe` refuses this pair too. If the tree read really does \
             observe the claim, say so and it runs: {OVERRIDE_FLAG} '<reason>'. The reason \
             is recorded in the proof.",
            evidence = boss_jobs::probe::PRODUCTION_CLAIM_EVIDENCE,
        ),
    };
    Admission {
        refusal: admission.refusal.or(Some(claim)),
        warnings: admission.warnings,
    }
}

/// What the UNATTENDED door refuses: everything [`admit`] refuses, and
/// the gate-log-alone clearance that [`admit`] only warns of (review
/// 4ce5ca85 N1, backlog ce72aea9).
///
/// The hand door warns because a person is standing there to say
/// whether the probe asserts an absence (which the one-fact-per-caller
/// gap turns green) or a presence (which it does not). Here nobody is,
/// and the dispatcher's refusal before filing (`ship_refusal` in
/// `jobs_run_car_probes.rs`) does not cover every request this door
/// executes: one filed by hand, by another rule, or by a dispatcher
/// still on the old build runs all the same, and its PROVEN is recorded.
/// Car 176d5c9d was recorded PROVEN here at 2026-10-10T04:00Z on such a
/// probe, the third after 033c5c2f and 099f8a9f. No override: there is
/// no one to state a reason, and the hand door is the way round.
pub(crate) fn unattended_refusal(car_id: &str, probe: &str) -> Option<Refusal> {
    admit(probe, true).refusal.or_else(|| {
        let kind = boss_jobs::probe::clears_a_caller_from_the_gate_log_alone(probe)?;
        Some(Refusal {
            rule: boss_jobs::probe::GATE_LOG_ALONE_RULE,
            text: format!(
                "THE PROBE {heading} — {reading}, and this door has nobody to ask whether \
                 that is the claim.\n\n{evidence}\n\n\
                 The car has landed, so it is not re-parked: prove it by hand with a probe \
                 that also reads the live rows with their coverage —\n  {by_hand}\n\n\
                 If the claim is a PRESENCE — the gate DID state this caller, which the log \
                 answers correctly — run this same probe by hand, where it warns rather than \
                 refuses and a person reads which claim it makes:\n  {from_car}",
                heading = boss_jobs::probe::gate_log_alone_heading(kind),
                reading = boss_jobs::probe::gate_log_alone_reading(kind),
                evidence = boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE,
                by_hand = boss_jobs::probe::hand_proof_command(car_id),
                from_car = boss_jobs::probe::from_car_command(car_id),
            ),
        })
    })
}

/// THE EIGHT SHAPE WARNINGS, in the wording every door can use (the
/// prefix is the door's). All read the probe TEXT, like the two rules
/// above them, and all are host-independent — so they are said at every
/// door a human is standing at, `--from-car` or not. The third
/// (0df3af1c) joined the first two for the same reason they exist: `[`
/// exits 2 on a non-integer, so the shape fails CLOSED, and the one
/// place its stderr has a reader is the terminal it is typed at. The
/// fourth (a92571a6) is the same argument again: a cutoff dated from a
/// moving HEAD answers 75 forever, never a false green.
///
/// THE FIFTH AND SIXTH (e7cf78c6) ARE WARNINGS ON A DIFFERENT ARGUMENT,
/// and the difference is worth keeping in sight: a counted page and a
/// grep for a bare name can both record a FALSE GREEN, which is the
/// direction that made `reads_git_time_with_an_offset` a refusal. What
/// keeps them here is decidability, not harmlessness. `limit=` is
/// correct in most of the probes that carry it (16 of 49 measured pair
/// it with `.total` already, and a page that is never counted is fine),
/// and a grep for a mention is a legitimate claim the text cannot be
/// told apart from the defect — so a refusal would fire on correct
/// probes and earn a routine override, which is read by nobody
/// (CLAUDE.md §Diagnosis). Both texts therefore SAY that they can fail
/// open, because a warning is only worth what its reader does with it.
///
/// THE SEVENTH SAID HERE (ce72aea9) FAILS OPEN TOO, and stays a warning
/// on the same argument: a clean claim counted over the machine gate's
/// logged facts with the live tally never read. The gate writes one
/// would-refuse fact per caller key per process, so "none since the
/// cutoff" is true of a caller already tallied — but a probe asserting
/// that a fact EXISTS has the same text and is correct. That argument
/// holds only where a person stands: `boss gate --park-probe` refuses
/// it since 5605c2b5, because the unattended door refuses it and a car
/// parked with it could never be proven by the machine.
pub(crate) fn shape_warnings(probe: &str) -> impl Iterator<Item = String> {
    let inverted = boss_jobs::probe::asserts_its_own_negation(probe).then(|| {
        format!(
            "THIS PROBE MAY ASSERT ITS OWN NEGATION — it runs `jq -e` over a filter whose \
             success branch is `empty`. `jq -e` exits 4 when the filter produces no output, \
             and `empty` is no output, so the claim HOLDING is what makes jq exit nonzero.\
             \n  {evidence}\n  \
             This is a warning, not a refusal: the shape fails CLOSED, so it can strand a \
             car but never record a proof of nothing. Check it if the filter is doing \
             something else.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        )
    });
    let rewritten = boss_jobs::probe::rewrites_its_exit_status(probe).map(|n| {
        format!(
            "THIS PROBE DISCARDS THE STATUS THAT WOULD EXPLAIN ITS FAILURE — a bare \
             `|| exit {n}` replaces the failing command's exit code with {n} and prints \
             nothing, so jq's 4 (the filter produced nothing) and 5 (the filter called \
             `error`), or curl's 7 (could not connect) and 22 (the server said no), all \
             arrive as {n}. That is the reduction CLAUDE.md §Diagnosis names: it suppresses \
             OUTPUT, not work, and it is paid for by whoever is next in front of the \
             failure.\n  \
             Keep the evidence first: `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
        )
    });
    let unguarded = boss_jobs::probe::compares_an_unguarded_number(probe).map(|var| {
        format!(
            "THIS PROBE COMPARES `${var}` AS A NUMBER WITHOUT CHECKING THAT IT IS ONE. When \
             the query matches nothing, `jq -r` prints the literal `null` — four characters, \
             not an empty string, so a `${{{var}:-9999}}` default does not fill it — and `[` \
             answers `integer expression expected` on stderr, exits 2, and takes the else \
             branch. Measured 2026-09-14 (0df3af1c): three cars sat UNPROVEN on exactly that, \
             each behind a stderr nobody reads, until an operator rewrote the probe by hand.\
             \n  \
             Guard on either side of the pipe, so a missing number says NOT YET instead:\n    \
             in jq:        `… | first | .field // empty`   (or `first | select(. != null)`)\n    \
             in the shell: `case \"${var}\" in ''|*[!0-9]*) echo 'not yet: <what has not happened>'; exit 75;; esac`\n  \
             This is a warning, not a refusal: `[` fails closed on the string, so the shape \
             strands a car but never records a proof of nothing."
        )
    });
    let moving = boss_jobs::probe::compares_against_a_moving_head(probe).map(|cmd| {
        format!(
            "THIS PROBE DATES ITS CUTOFF FROM A TARGET THAT MOVES — `{cmd}` reads the \
             converged checkout's CURRENT head, which advances with every train (about 28 a \
             day), so the claim quietly becomes 'this change worked more recently than any \
             other change landed'. The car's change converged ONCE; an unrelated train \
             landing afterwards is not evidence against it.\n  {evidence}\n  \
             Date it from the car's own merge, which is fixed and handed to every recorded \
             probe as `${var}` — absent only when the car has not converged here, which is \
             what the guard reports:\n{recipe}\n  \
             This is a warning, not a refusal: the shape fails CLOSED — a starved probe \
             answers 75 (not yet), never a false green — but a car whose qualifying event \
             is rarer than a train starves by construction, and a daily one is effectively \
             unprovable.",
            evidence = boss_jobs::probe::MOVING_HEAD_EVIDENCE,
            var = CAR_CONVERGED_AT_VAR,
            recipe = boss_jobs::probe::CAR_INSTANT_RECIPE,
        )
    });
    let truncated = boss_jobs::probe::counts_a_page_it_may_not_have_read(probe).map(|token| {
        format!(
            "THIS PROBE COUNTS A PAGE IT MAY NOT HAVE READ — `{token}` asks the server for a \
             PAGE, and a page answers in the same shape the whole list does: \
             `{{\"data\":[…],\"total\":345}}` is a correct reply to a request for 300, and \
             nothing in it says 45 rows were left behind. A count taken over that page is a \
             count of a set the probe did not see.\n  {evidence}\n  \
             UNLIKE THE THREE WARNINGS ABOVE, THIS SHAPE CAN FAIL OPEN: a probe that counts \
             a page to assert an ABSENCE — no row since the cutoff matches the bad shape — \
             records a false green when the row it wanted was in the tail. It is a warning \
             and not a refusal only because `limit=` is right far more often than it is \
             wrong and this is a coarse text scan, so read this one rather than skim it.",
            evidence = boss_jobs::probe::TRUNCATED_PAGE_EVIDENCE,
        )
    });
    let mention = boss_jobs::probe::greps_a_name_where_a_definition_is_meant(probe).map(|name| {
        format!(
            "THIS PROBE COUNTS A NAME WHERE A DEFINITION LOOKS LIKE WHAT IT MEANS — `{name}` \
             matches every line that MENTIONS it, and the doc comment above a call site is \
             such a line. The count is nonzero whether or not the thing was ever defined.\n  \
             {evidence}\n  \
             This is a warning, not a refusal, because the text cannot say which you meant: \
             counting a mention is a legitimate claim (a call site exists, a literal is still \
             in the config, a name was not removed). But when a definition WAS meant it fails \
             OPEN — absent reads as present — so it is worth the ten seconds to check.",
            evidence = boss_jobs::probe::MENTION_NOT_DEFINITION_EVIDENCE,
        )
    });
    let log_alone = boss_jobs::probe::clears_a_caller_from_the_gate_log_alone(probe).map(|kind| {
        format!(
            "THIS PROBE {heading} — {reading}.\n  {evidence}\n  \
             THIS SHAPE CAN FAIL OPEN: 'nothing since the cutoff' passes for a caller that \
             presented nothing. It is a warning and not a refusal only because the text \
             cannot tell that absence claim from a PRESENCE claim — the gate did state this \
             caller — which the reading answers correctly; if yours counts to conclude \
             there are none, add the live reading with its coverage before this gate starts.",
            heading = boss_jobs::probe::gate_log_alone_heading(kind),
            reading = boss_jobs::probe::gate_log_alone_reading(kind),
            evidence = boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE,
        )
    });
    // A PINNED BLOB (2770eba8) is refused at park, where re-parking is
    // free. Here it is said and run: it fails toward RED, never toward a
    // false green, and a landed car whose pins still hold is honestly
    // proven by it once.
    let pinned = boss_jobs::probe::pins_file_bytes(probe).map(|tool| {
        format!(
            "THIS PROBE PINS A FILE'S BYTES — it reads a content id with `{tool}` and \
             compares it to a literal, so it proves which bytes were checked out, not that \
             the change works, and the next edit to that file turns it red on correct code.\
             \n  {evidence}\n  \
             This is a warning here and a refusal at `boss gate --park-probe`: the shape \
             fails CLOSED, and a landed car cannot be re-parked.",
            evidence = boss_jobs::probe::PINNED_BYTES_EVIDENCE,
        )
    });
    inverted
        .into_iter()
        .chain(rewritten)
        .chain(unguarded)
        .chain(moving)
        .chain(truncated)
        .chain(mention)
        .chain(log_alone)
        .chain(pinned)
}

/// THE NINTH SHAPE, AND THE ONE THE TEXT CANNOT SHOW (backlog
/// 8ac42ee5): a grep over a file whose every match is a COMMENT. It
/// lives outside [`shape_warnings`] because it needs the tree — `read`
/// answers a path at the revision `at` names — so each door that has a
/// tree hands it one: `boss gate --park-probe` reads the car's own tip,
/// where the comment that defeats a removal probe is written by the same
/// car, and the hand door of `boss prove` reads HEAD where the probe
/// runs, where a NOT YET that will never clear is otherwise
/// indistinguishable from one that will.
pub(crate) fn prose_only_warning(
    probe: &str,
    at: &str,
    read: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let found = boss_jobs::probe::a_grep_only_prose_answers(probe, read)?;
    let shown: Vec<String> = found
        .lines
        .iter()
        .take(3)
        .map(|(n, text)| format!("    {}:{n}: {text}", found.path))
        .collect();
    let more = found.lines.len().saturating_sub(shown.len());
    let more = if more > 0 {
        format!("\n    … and {more} more, all comments")
    } else {
        String::new()
    };
    Some(format!(
        "THIS PROBE'S GREP IS ANSWERED ONLY BY PROSE — `{pattern}` matches {path} at {at} on \
         {n} line(s), and every one is a comment:\n{lines}{more}\n  \
         Whatever the probe concludes from that count is a conclusion about a sentence. An \
         ABSENCE assertion over it cannot pass while the comment stands — and the car that \
         removes a thing is usually the one that writes the comment saying so — while a \
         PRESENCE assertion passes on the mention with nothing behind it.\n  {evidence}\n  \
         This is a warning, not a refusal: counting a mention can be the claim, and the \
         comment test is a line-prefix scan.",
        pattern = found.pattern,
        path = found.path,
        n = found.lines.len(),
        lines = shown.join("\n"),
        evidence = boss_jobs::probe::PROSE_ONLY_EVIDENCE,
    ))
}

/// A reader of `<rev>:<path>` in the repository at `dir`, for
/// [`prose_only_warning`]: `None` for anything git will not show, which
/// the detector reads as nothing to judge.
pub(crate) fn git_show_reader(dir: &Path, rev: &str) -> impl Fn(&str) -> Option<String> + use<> {
    let (dir, rev) = (dir.to_path_buf(), rev.to_string());
    move |path: &str| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["show", &format!("{rev}:{path}")])
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// The override, resolved once: `None` when the flag was not given,
/// refusing a flag given with no reason — an override with no stated
/// reason is the silent yes the whole verb exists to end.
pub(crate) fn override_reason(given: Option<&str>) -> Result<Option<&str>> {
    match given.map(str::trim) {
        Some("") => bail!(
            "{OVERRIDE_FLAG} needs a reason: it is recorded in the proof, where the next \
             reader — or `--recheck`, unattended — finds out why a refused probe was run."
        ),
        other => Ok(other),
    }
}

/// WHERE A PROBE'S TREE IS OBSERVED: the directory the probe will
/// actually run in, which is the shell's own `cwd` — the converged
/// checkout at the unattended door, the recorded one under `--recheck`,
/// and this process's directory at the hand door. Reading it anywhere
/// else would answer about a tree the probe never opens, which is the
/// class of mistake this whole area exists to refuse.
#[derive(Debug, Default)]
pub(crate) struct ProbeObservation {
    tree: crate::freshness::TreeObservation,
    cwd: Option<String>,
    user: Option<String>,
    uid: Option<String>,
    unreadable: Vec<String>,
}

impl ProbeObservation {
    /// Raw execution context for a separate read observation, not a proof.
    pub(crate) fn evidence(&self) -> Value {
        let mut context = crate::freshness::tree_metadata(&self.tree);
        context["cwd"] = json!(self.cwd);
        context["user"] = json!(self.user);
        context["uid"] = json!(self.uid);
        context["unreadable"] = json!(self.unreadable);
        context
    }
}

pub(crate) fn probe_tree(shell: &Shell) -> ProbeObservation {
    let no_token = match crate::door_env::NoTokenInReach::new() {
        Ok(no_token) => no_token,
        Err(e) => {
            return ProbeObservation {
                unreadable: vec![format!("could not isolate provenance reads: {e:#}")],
                ..Default::default()
            };
        }
    };
    let mut unreadable = Vec::new();
    let mut line = |program: &str, args: &[&str]| {
        let result = shell.read(program, args, &no_token).and_then(|out| {
            let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !value.is_empty() {
                Ok(value)
            } else {
                Err(format!(
                    "{program} {} exited {:?}: {}",
                    args.join(" "),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        });
        match result {
            Ok(value) => Some(value),
            Err(why) => {
                unreadable.push(why);
                None
            }
        }
    };
    let cwd = line("pwd", &["-P"]);
    let user = line("id", &["-un"]);
    let uid = line("id", &["-u"]);
    let tree = crate::freshness::observe_tree_with(|args| shell.read("git", args, &no_token));
    ProbeObservation {
        tree,
        cwd,
        user,
        uid,
        unreadable,
    }
}

/// The proof record. Serialised once, stored verbatim, re-read by
/// `--recheck` — so its field names are a contract, not a detail.
/// `overridden` is the one optional key: present only when the
/// operator ran a probe this door refused, absent otherwise, so its
/// presence means something to whoever reads the proof back. The
/// unattended door never writes it: it has no override, and a probe it
/// refuses is refused on the ops-request, exit 2.
///
/// `observed` is passed explicitly rather than left to each door to remember,
/// because a door that forgets it records a proof that cannot say what
/// it read — the defect this closed (backlog 6f581de6). See
/// [`crate::freshness::tree_metadata`] for what the two keys mean.
pub(crate) fn proof_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
    observed: &ProbeObservation,
    overridden: Option<&Value>,
) -> Value {
    let mut p = json!({
        "probe": probe,
        "expect": expect,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        // WHERE IT RAN, not just what ran. A command means the same
        // thing twice only if the host and the directory are the same
        // both times, and 3 of 10 rechecks false-failed for exactly
        // this: probes opening `git rev-parse HEAD` re-run outside a
        // repository, and probes authored on the workstation as
        // `ssh boss-gcp ...` re-run ON boss-gcp, where that name does
        // not resolve. Both are free to record — the verb already
        // knows them (66fd64c6).
        "cwd": observed.cwd,
        "user": observed.user,
        "uid": observed.uid,
        "at": at,
    });
    // WHAT IT READ, beside where it ran. `cwd` names a directory whose
    // contents change under it; these name the revision that answered.
    for (k, v) in crate::freshness::tree_metadata(&observed.tree)
        .as_object()
        .into_iter()
        .flatten()
    {
        p[k] = v.clone();
    }
    if !observed.unreadable.is_empty() {
        p["context_unreadable"] = json!(observed.unreadable);
    }
    if let Some(o) = overridden {
        p["overridden"] = o.clone();
    }
    p
}

/// Can the recorded probe be re-run HERE, meaning the same thing?
///
/// The distinction this draws is the whole point of the packet: a probe
/// that cannot be re-run is not the same fact as a claim that stopped
/// being true, and `--recheck` used to render them identically — as
/// NO LONGER HOLDS. An instrument that cries wolf 30% of the time gets
/// ignored, and then decay stops being detected at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rerunnable {
    /// Same host, and the directory is available.
    Here,
    /// Recorded somewhere else. Re-running here tests a different thing.
    /// `checkout` says why a checkout here did not place it instead —
    /// `None` only from [`rerunnable`], which never asks (70cb74d8).
    WrongHost {
        recorded: String,
        here: String,
        checkout: Option<NoCheckout>,
    },
    /// The directory the probe assumed is gone; `checkout` as above.
    MissingDir {
        cwd: String,
        checkout: Option<NoCheckout>,
    },
    /// The probe reads only the system of record through the sanctioned
    /// reader, so it means the same thing on any host, in any directory
    /// (backlog c89a661e).
    Anywhere,
    /// The probe reads only git objects at literally named revisions
    /// ([`boss_jobs::probe::reads_only_git_objects`]), and `root` is the
    /// top level of a checkout here whose HEAD holds the car's merge —
    /// so it re-runs there and means the same thing (backlog da937eb3).
    AnyCheckout { root: String },
}

/// ABSENT CONTEXT IS NOT A MISMATCH. Proofs recorded before this change
/// carry no `cwd`, and the oldest carry no `host`. Refusing on missing
/// data would break every proof already on the board, so an unrecorded
/// field means "cannot check", and the recheck proceeds exactly as it
/// did before.
pub(crate) fn rerunnable(
    recorded_host: Option<&str>,
    here: &str,
    cwd: Option<&str>,
    dir_exists: bool,
) -> Rerunnable {
    if let Some(rec) = recorded_host.filter(|h| !h.is_empty() && *h != "unknown")
        && rec != here
    {
        return Rerunnable::WrongHost {
            recorded: rec.to_string(),
            here: here.to_string(),
            checkout: None,
        };
    }
    if let Some(dir) = cwd.filter(|c| !c.is_empty())
        && !dir_exists
    {
        return Rerunnable::MissingDir {
            cwd: dir.to_string(),
            checkout: None,
        };
    }
    Rerunnable::Here
}

/// Where `--recheck` may run THIS probe: anywhere, when its text reads
/// nothing but the system of record ([`boss_jobs::probe::reads_only_the_sor`]),
/// and otherwise by the recorded context, as [`rerunnable`] judges it.
/// The recheck's own shell already carries the reader and its env
/// (`with_probe_reader`, 18fee481) — the recorded-host rule was the
/// only thing keeping a closed car's host-independent claim from being
/// re-judged on the pod (c89a661e).
///
/// And in a checkout here, when its text reads nothing but git objects
/// at literally named revisions
/// ([`boss_jobs::probe::reads_only_git_objects`]) and the operator's
/// checkout holds the car's merge: 12 of 12 W41 rechecks were refused
/// for exactly such probes, 11 recorded on the workstation and one on a
/// dev-pod name that no longer exists (da937eb3). The checkout is read
/// only when the recorded context would otherwise refuse, and a refusal
/// carries why neither placed it (backlog 70cb74d8).
pub(crate) fn recheck_placement(
    probe: &str,
    recorded_host: Option<&str>,
    here: &str,
    cwd: Option<&str>,
    dir_exists: bool,
    checkout_holding_the_car: impl FnOnce() -> Result<String, NoCheckout>,
) -> Rerunnable {
    if boss_jobs::probe::reads_only_the_sor(probe) {
        return Rerunnable::Anywhere;
    }
    let placed = rerunnable(recorded_host, here, cwd, dir_exists);
    if !matches!(
        placed,
        Rerunnable::WrongHost { .. } | Rerunnable::MissingDir { .. }
    ) {
        return placed;
    }
    let why = match boss_jobs::probe::git_objects_only(probe) {
        Err(refused) => NoCheckout::Grammar(refused),
        Ok(()) => match checkout_holding_the_car() {
            Ok(root) => return Rerunnable::AnyCheckout { root },
            Err(why) => why,
        },
    };
    match placed {
        Rerunnable::WrongHost { recorded, here, .. } => Rerunnable::WrongHost {
            recorded,
            here,
            checkout: Some(why),
        },
        Rerunnable::MissingDir { cwd, .. } => Rerunnable::MissingDir {
            cwd,
            checkout: Some(why),
        },
        other => other,
    }
}

/// THE CANNOT RE-RUN HERE TEXT, for a placement that refuses; `None` for
/// one that runs. Its last paragraph names the one reason a checkout here
/// did not place the probe instead (backlog 70cb74d8), where it used to
/// list every reason it might have been.
pub(crate) fn cannot_rerun_here(placement: &Rerunnable, probe: &str) -> Option<String> {
    let because = |checkout: &Option<NoCheckout>| match checkout {
        Some(why) => format!("It was not re-run in a checkout here: {why}."),
        None => "This one runs something else, or reads through the shell, or there is no \
                 such checkout here."
            .to_string(),
    };
    match placement {
        Rerunnable::WrongHost {
            recorded,
            here,
            checkout,
        } => Some(format!(
            "CANNOT RE-RUN HERE — this proof was recorded on {recorded}, and you are on \
             {here}.\n  $ {probe}\n\n\
             The claim has NOT been tested either way, which is a different fact from it \
             having decayed. Reporting the two identically is how this instrument \
             false-failed 3 times in 10 (66fd64c6) — probes reference paths, hostnames and \
             services that exist on the host they were written for, so re-running one \
             elsewhere tests something else and usually fails.\n  \
             Re-run it on {recorded}, or, while the car is still open, re-prove it here \
             with a probe that belongs to this host. A probe that reads ONLY the system \
             of record through `{reader}` re-runs from any host on its own (c89a661e), \
             and one that reads only git objects at a named revision (`git show \
             HEAD:<path>`, `git grep <pattern> HEAD`) re-runs from any checkout whose HEAD \
             holds the car's merge — run this from one (da937eb3). That second class \
             is a small grammar: literal words only, with no variable, `$(…)`, `case`, \
             `if` or loop.\n  {}",
            because(checkout),
            reader = boss_jobs::probe::SOR_READER,
        )),
        Rerunnable::MissingDir { cwd, checkout } => Some(format!(
            "CANNOT RE-RUN HERE — this proof was recorded in {cwd}, which does not exist \
             on this machine.\n  $ {probe}\n\n\
             A probe that opens `git rev-parse HEAD` means nothing outside a repository. \
             The claim has not been tested either way.{}",
            checkout
                .as_ref()
                .map(|why| format!("\n  It was not re-run in a checkout here: {why}."))
                .unwrap_or_default()
        )),
        Rerunnable::Here | Rerunnable::Anywhere | Rerunnable::AnyCheckout { .. } => None,
    }
}

/// THE SHELL A `--recheck` RUNS ITS PROBE IN, by where it was placed.
/// A host-free probe runs where the operator stands: the recorded
/// directory belongs to the host that recorded it (c89a661e). A
/// git-object probe runs at the top of the checkout that holds the car,
/// where `HEAD:` and a pathspec mean what they meant on the recording
/// host (da937eb3). Both of those placements stand on the probe's TEXT
/// alone, so both run [`Shell::host_free`] (backlog 51587416).
///
/// A same-host recheck keeps the hand door's shell, the operator's whole
/// environment: it re-runs where and as the proof was recorded, by hand,
/// with that environment, and a probe recorded so may need it — an
/// in-cluster `kubectl` finds its API server through
/// `KUBERNETES_SERVICE_HOST`. Its claim never rested on the text.
fn recheck_shell(placement: &Rerunnable, recorded_cwd: Option<&str>) -> Shell {
    match placement {
        Rerunnable::AnyCheckout { root } => Shell::here(Some(Path::new(root))).host_free(),
        Rerunnable::Anywhere => Shell::here(None).host_free(),
        _ => Shell::here(recorded_cwd.filter(|d| !d.is_empty()).map(Path::new)),
    }
}

/// The matching core `find_car` and `boss job` share: a row matches by
/// exact `metadata.branch`, or by an id prefix of at least 8
/// characters. Pure and message-free so each verb can refuse in its
/// own vocabulary — `find_car`'s "no ship-a-change car" would be a lie
/// coming from `boss job get`, which sees every kind.
pub(crate) fn matching_jobs<'a>(rows: &'a [Value], given: &str) -> Vec<&'a Value> {
    rows.iter()
        .filter(|c| {
            let by_branch = c
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                == Some(given);
            let by_id = c
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|i| i.starts_with(given) && given.len() >= 8);
            by_branch || by_id
        })
        .collect()
}

/// Which cars a lookup may legitimately resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Eligible {
    /// Recording a proof WRITES to the car, so a car the pipeline has
    /// finished with is not a candidate: it cannot take a new proof.
    Live,
    /// `--recheck` re-runs a probe already recorded on the car and
    /// writes nothing, so a finished car is a legitimate target — which
    /// is the whole reason [`crate::sor_client::all_cars_at`] pages over closed
    /// cars instead of filtering `status=open` at the API.
    AnyStatus,
}

/// The car's `status` as the jobs API spelled it, or `"?"` when the row
/// carries none. For DISPLAY only — whether a car is live is
/// `boss_jobs::car::is_open`'s single answer, never a second reading of
/// this string.
fn status_of(car: &Value) -> &str {
    car.get("status").and_then(Value::as_str).unwrap_or("?")
}

/// One `id8  status  title` line per candidate, so a refusal that lists
/// cars is answerable from the refusal.
///
/// The id is shortened by [`crate::train::id8`] rather than a second
/// `[..8]` of its own: the bare slice panicked on any row whose id the
/// reader could not find, which is the listing for a malformed row
/// crashing instead of printing it.
fn listed(rows: &[&Value]) -> String {
    rows.iter().fold(String::new(), |mut out, c| {
        out.push_str(&format!(
            "\n  {}  {}  {}",
            crate::train::id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
            status_of(c),
            c.get("title").and_then(Value::as_str).unwrap_or("?")
        ));
        out
    })
}

/// Find the one car for `given` — a branch name, or an id prefix.
///
/// Refuses on ambiguity rather than picking. `boss park` learned this
/// the expensive way: it took the LAST match from a list the API
/// returns newest-first, and quietly parked two cars against a stale
/// receipt. Choosing among candidates is how that happens, so this
/// does not choose.
///
/// It does DISCARD candidates that cannot take a proof at all — which
/// is not choosing between live cars, and is what backlog 0a58827d asked
/// for. Measured live 2026-09-10 05:02 UTC: `boss prove
/// feat/arrival-runs-the-probe-rerail` refused with "2 open cars match"
/// over one live car and one closed, abandoned twin ("gated, then
/// changed"). There was one candidate, so the refusal was wrong to fire,
/// and the word "open" sent the reader to investigate a dead car — two
/// queries spent learning what the message had already asserted falsely.
///
/// Whether a car is live is `boss_jobs::car::is_open`, the one definition
/// `boss park`, `boss rerail` and the auto-park handler already decide
/// on (CLAUDE.md §9a) — this verb does not carry a second reading of
/// `status`. What the CALLER's mode decides is what a finished car means
/// here:
///
/// - [`Eligible::Live`] — recording a proof is a WRITE, so a finished
///   car is discarded outright. With nothing left the refusal says the
///   matches are finished, rather than sending the reader after a typo.
/// - [`Eligible::AnyStatus`] — `--recheck` writes nothing, so a finished
///   car is a legitimate target; a live match is still PREFERRED when
///   the name has one, so a recheck resolves where it used to.
///
/// Either way every listed candidate is labelled with its status, so a
/// refusal that must name a finished car stays answerable without a
/// second query — the per-row label replaces the one collective "open"
/// / "closed" word, which could only describe a homogeneous set.
pub(crate) fn find_car<'a>(
    cars: &'a [Value],
    given: &str,
    eligible: Eligible,
) -> Result<&'a Value> {
    let matched = matching_jobs(cars, given);
    let live: Vec<&Value> = matched
        .iter()
        .copied()
        .filter(|c| boss_jobs::car::is_open(c))
        .collect();
    let candidates: Vec<&Value> = match eligible {
        Eligible::Live => live,
        Eligible::AnyStatus if !live.is_empty() => live,
        Eligible::AnyStatus => matched.clone(),
    };

    match candidates.len() {
        1 => Ok(candidates[0]),
        // Nothing live, but the name DID match — say so, rather than
        // sending the operator to hunt a typo that is not there.
        0 if !matched.is_empty() => bail!(
            "no car for {given:?} that can take a proof — every match is finished:{}\n\n\
             A closed or cancelled car is done, and recording a new proof on one would be \
             writing to a terminal. Use --recheck to re-run the probe already recorded on \
             it, or name the car that carries this work now.",
            listed(&matched)
        ),
        0 => bail!(
            "no ship-a-change car for {given:?}. Give the car's branch exactly \
             as it was parked, or at least 8 characters of its id."
        ),
        n => bail!(
            "{n} cars match {given:?} — say which:{}",
            listed(&candidates)
        ),
    }
}

/// The car's `proven` step, refusing unless it is actually reachable.
///
/// `proven` is gated on `job.metadata.merged = "true"`, so a step still
/// pending means the change has not merged. Recording proof there would
/// be recording that unshipped code works in production.
pub(crate) fn proven_step(car: &Value, replace: bool) -> Result<&Value> {
    let step = car
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the car has no step titled {PROVEN:?}. The ship-a-change workflow was \
                 renamed or re-versioned; this verb fills a step by title and will not \
                 guess which one was meant."
            )
        })?;

    match step.get("status").and_then(Value::as_str) {
        // --replace WITH NOTHING TO REPLACE (backlog 251dba77, measured
        // 2026-09-11). The flag's write path appends to `reproof` in JOB
        // metadata and never touches the step, because a COMPLETED step
        // is frozen — so given an open step it recorded a second-class
        // copy of a proof nobody had yet, printed "re-proven — recorded
        // as reproof #1. The original proof is untouched on the step"
        // (two assertions, both false), and left the step `ready`. The
        // car did not advance and nothing said so.
        Some(open @ ("ready" | "active")) if replace && recorded_proof_on(step).is_none() => bail!(
            "--replace has nothing to replace: the `{PROVEN}` step is {open} and carries no \
             recorded proof, so there is no first proof to keep beside a second one.\n\n\
             Run the same command WITHOUT --replace. That is the path that records a FIRST \
             proof: it writes the proof on the step and completes it, which is what makes \
             the car advance.\n\n\
             --replace is for a step already COMPLETED, whose proof is frozen and must not \
             be erased — a proof that used to hold and no longer does is evidence, not a \
             mistake (2b30eff4)."
        ),
        Some("ready") | Some("active") => Ok(step),
        // A PROOF THAT STOPPED HOLDING IS EVIDENCE, NOT A MISTAKE.
        // Car 932aa956's probe observed a real production refusal and
        // stopped holding within the hour, because the condition it
        // keyed on was transient. `--recheck` reported NO LONGER HOLDS
        // correctly and was wrong about the cause, and there was no way
        // to put a better probe under the same claim short of an
        // operator editing job metadata by hand (2b30eff4).
        Some("completed") if replace => Ok(step),
        Some("completed") => bail!(
            "this car is already proven. To check whether the proof STILL holds, \
             re-run with --recheck; it re-executes the recorded probe and changes \
             nothing. To put a BETTER probe under the same claim — because the first \
             one was transient rather than wrong — re-run with --replace, which keeps \
             the original proof and records the new one beside it."
        ),
        Some("pending") => bail!(
            "the `{PROVEN}` step is still pending, which means the car has not merged \
             — its predicate is `steps.review.done AND job.metadata.merged = \"true\"`. \
             A change that has not shipped cannot be proven in production."
        ),
        other => bail!("the `{PROVEN}` step is {other:?}, which this verb does not fill"),
    }
}

/// The proof already recorded on this step, if there is one.
///
/// The step's own `proof` field — required by the workflow when the step
/// completes, so a completed step always has one — read here to answer
/// the only question `--replace` needs: is there anything to replace?
pub(crate) fn recorded_proof_on(step: &Value) -> Option<&str> {
    step.get("metadata")
        .and_then(|m| m.get("proof"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// Which record a re-runnable probe was read from. `--recheck` reports
/// differently on each: a PROOF that fails now has decayed ("NO LONGER
/// HOLDS"), while an ATTEMPT was never a proof, so the same failure is
/// "still not proven" and the same success is "now provable" — never
/// "still HOLDS" about a claim nobody ever proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The step's `proof`, or the newest `reproof` entry.
    Proof,
    /// The car's `proof_attempt`: a run that settled nothing, with
    /// `not_yet` as the writer stamped it.
    Attempt { not_yet: bool },
}

/// A recorded proof, as much of it as the writer stored.
#[derive(Debug)]
pub(crate) struct Recorded {
    pub probe: String,
    pub expect: Option<String>,
    /// `None` on proofs written before the context was recorded.
    pub host: Option<String>,
    pub cwd: Option<String>,
    /// The override this proof was admitted under, if the operator ran
    /// a probe the door refused. Read back so `--recheck` — the
    /// unattended re-run — says so instead of reporting a clean HOLDS
    /// on a probe somebody had to argue past.
    pub overridden: Option<Value>,
    pub source: Source,
}

/// Read back a recorded proof so `--recheck` can re-run it.
pub(crate) fn recorded_probe_for(car: &Value, step: &Value) -> Result<Recorded> {
    // A REPLACEMENT SUPERSEDES THE ORIGINAL, and is read in preference
    // to it — the same precedence `regate_receipt` has over a stale
    // gate receipt. The original stays on the step; this is which proof
    // `--recheck` should be re-running, not which one happened.
    if let Some(last) = car
        .get("metadata")
        .and_then(|m| m.get("reproof"))
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        && let Some(p) = last.get("proof")
    {
        return read_proof(p);
    }
    if recorded_proof_on(step).is_some() {
        return recorded_probe(step);
    }
    // A NOT-YET CAR HAS NO PROOF TO RE-RUN, AND ONE PROBE THAT RAN
    // (726562de). Its `proven` step is still ready, so the step reader
    // below would refuse with "nothing to re-run" — but "not yet" is a
    // request to run it again, and the car holds exactly what ran in
    // `proof_attempt` (the forge's record or this door's, one shape).
    // Read in preference to nothing, never in preference to a proof.
    if let Some(attempt) = car.get("metadata").and_then(|m| m.get("proof_attempt"))
        && attempt.get("probe").and_then(Value::as_str).is_some()
    {
        let mut rec = read_proof(attempt)?;
        rec.source = Source::Attempt {
            not_yet: attempt.get("not_yet").and_then(Value::as_bool) == Some(true),
        };
        return Ok(rec);
    }
    recorded_probe(step)
}

/// Is `value` one of the variants `field_type` declares, for the field
/// named `name` on this step?
///
/// Reads the step's OWN field spec — the one that arrived with the car
/// we already fetched, from the workflow version this packet is pinned
/// to. Deliberately not a constant in this file: the vocabulary already
/// lives in `infra/platform/workflows/` and in the registry
/// default, and a third copy here would be the drift CLAUDE.md 9a is
/// about. Validating against the packet's own spec also means a
/// workflow version that adds a variant needs no CLI release.
///
/// `Ok(())` when the field is absent or free-text: this verb is not the
/// place to invent a contract the protocol did not state.
pub(crate) fn check_enum_field(step: &Value, name: &str, value: &str) -> Result<()> {
    let Some(ft) = step
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|f| f.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|f| f.get("field_type"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if !ft.contains('|') {
        return Ok(());
    }
    let allowed: Vec<&str> = ft.split('|').map(str::trim).collect();
    if allowed.contains(&value) {
        return Ok(());
    }
    bail!(
        "--{name} {value:?} is not one of {}. This is checked BEFORE the probe runs: \
         the probe would have executed against production, passed, and then been \
         thrown away when the API refused the record — which is how two proofs were \
         silently lost on 2026-09-02.",
        allowed.join(" | ")
    )
}

pub(crate) fn recorded_probe(step: &Value) -> Result<Recorded> {
    let md = step.get("metadata");
    let raw = md.and_then(|m| m.get("proof")).ok_or_else(|| {
        anyhow::anyhow!(
            "this step carries no `proof`, so there is nothing to re-run. It was \
                 completed before proof was mechanised, or filled by hand — its \
                 `verified` prose is a claim with no probe under it."
        )
    })?;
    read_proof(raw)
}

/// Parse one recorded proof, however it was stored.
fn read_proof(raw: &Value) -> Result<Recorded> {
    // Stored as a JSON string (verbatim, like the gate receipt) or, if a
    // future writer stores it structurally, as an object. Read both.
    let v: Value = match raw.as_str() {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| anyhow::anyhow!("the recorded proof is not readable JSON: {e}"))?,
        None => raw.clone(),
    };
    let probe = v
        .get("probe")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the recorded proof has no `probe` to re-run"))?
        .to_string();
    let expect = v.get("expect").and_then(Value::as_str).map(str::to_string);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Recorded {
        probe,
        expect,
        host: text("host"),
        cwd: text("cwd"),
        overridden: v.get("overridden").cloned(),
        source: Source::Proof,
    })
}

/// The probe a car recorded at park time or through --record-probe, read as the
/// pair `boss prove` would have been given by hand.
///
/// WHERE IT RUNS. Two callers run this text on two different machines:
/// `--from-car` runs it wherever the operator is standing, and the
/// arrival rule runs it on the FORGE HOST as the low-privilege account
/// boss-probe, in a read-only view of the converged checkout, with that
/// host's tools. The forge has no kubeconfig, so a
/// probe can pass here and be unrunnable there — measured 2026-09-09
/// (f9304366), which is why `boss gate --park-probe` refuses a probe
/// naming a tool in infra/forge/host-absent-tools.txt.
///
/// THE CAR CARRIES ITS PROBE (28ac45ab): `boss gate --park-probe/
/// --park-expect` stamps the intent, the auto-park handler copies it
/// onto the car under `boss_jobs::car::PROOF_*`. --record-probe can add
/// a command when its original event becomes observable, preserving that
/// event and the previous command in a distinct registration. This is the read
/// side — `--from-car` here, and the arrival rule's runner. The keys
/// are one definition in core so the three cannot drift.
///
/// Refuses, naming the door, when the car recorded nothing — and says
/// so differently for an EVENT-BOUND car, whose builder decided a
/// machine cannot prove it: running "no probe" as a probe would be the
/// silent yes this verb exists to end.
pub(crate) fn car_probe(car: &Value) -> Result<(String, String)> {
    let md = |k: &str| {
        car.get("metadata")
            .and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if md(boss_jobs::car::PROOF_PROBE).is_none()
        && let Some(event) = md(boss_jobs::car::PROOF_EVENT)
    {
        bail!(
            "this car is EVENT-BOUND, not probed: {event}\n\n\
             Its builder recorded the event that proves it instead of a command, so there \
             is nothing for --from-car to run. Prove it by hand when the event fires: \
             {}.",
            boss_jobs::probe::hand_proof_command("<car>")
        );
    }
    let Some(probe) = md(boss_jobs::car::PROOF_PROBE) else {
        bail!(
            "this car recorded no `{}`, so --from-car has nothing to run. The probe is \
             written at park time — `boss gate --park-probe '<cmd>' --park-expect '<string>'` \
             — by the builder who knows what the change does. Give one now with --probe \
             and --expect instead.",
            boss_jobs::car::PROOF_PROBE
        );
    };
    let Some(expect) = md(boss_jobs::car::PROOF_EXPECT) else {
        bail!(
            "this car recorded a `{}` but no `{}` — a probe asserting nothing is `echo hi`. \
             The gate verb refuses that pair, so the car was edited by hand; give --expect \
             here or fix the car's metadata.",
            boss_jobs::car::PROOF_PROBE,
            boss_jobs::car::PROOF_EXPECT
        );
    };
    Ok((probe, expect))
}

/// What a car-carried proof MEANS, when the operator did not say: the
/// car's own summary — the claim the change makes, which is exactly
/// what the probe checks in production. `verified` stays required in
/// the record; it just has a source the builder already wrote.
pub(crate) fn car_verified(car: &Value, given: Option<String>) -> Result<String> {
    if let Some(v) = given {
        return Ok(v);
    }
    if let Some(meaning) = car
        .pointer("/metadata/proof_probe_verified")
        .and_then(Value::as_str)
        && !meaning.trim().is_empty()
    {
        return Ok(meaning.to_string());
    }
    car.get("metadata")
        .and_then(|m| m.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--verified is required: the car has no summary to fall back on, so say \
                 what the probe means, in prose, for a reader"
            )
        })
}

// A registration is intent, never a verdict (dee64538). Each write has
// its own top-level key so concurrent metadata merges conserve both records;
// the active pair is just the projection the existing forge door reads.
fn probe_registration_patch(
    car: &Value,
    probe: &str,
    expect: &str,
    meaning: &str,
    actor: &str,
    at: &str,
    registration: &str,
) -> Result<Value> {
    if car.get("kind").and_then(Value::as_str) != Some("ship-a-change")
        || car.get("status").and_then(Value::as_str) != Some("open")
        || car.pointer("/metadata/merged").and_then(Value::as_str) != Some("true")
    {
        bail!("--record-probe requires an open landed car whose proof is still owed");
    }
    proven_step(car, false)?;
    if [probe, expect, meaning, actor, registration]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("--record-probe requires --probe, --expect and --verified, with a named actor");
    }
    if let Some(refusal) = unattended_refusal(
        car.get("id").and_then(Value::as_str).unwrap_or_default(),
        probe,
    ) {
        bail!("{refusal}");
    }
    // The registered probe proves `meaning` unattended, where nobody can
    // argue an override, so the tree-read-under-a-production-claim pair
    // is refused outright here (backlog 490acf06).
    if let Some(words) = boss_jobs::probe::a_tree_read_under_a_production_claim(probe, meaning) {
        bail!(
            "--verified claims a production behaviour (`{words}`) and --probe reads only the \
             converged tree (`{rule}`).\n\n{evidence}",
            rule = boss_jobs::probe::PRODUCTION_CLAIM_RULE,
            evidence = boss_jobs::probe::PRODUCTION_CLAIM_EVIDENCE,
        );
    }
    if let Some(tool) = boss_jobs::probe::needs_absent_tool(probe) {
        bail!("recorded probe needs {tool}, which the forge does not have");
    }
    if boss_jobs::probe::changes_directory(probe).is_some()
        || boss_jobs::probe::names_an_actor(probe).is_some()
    {
        bail!("a recorded probe must use the forge's existing directory and read-only identity");
    }
    let md = car.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let mut patch = json!({
        boss_jobs::car::PROOF_PROBE: probe,
        boss_jobs::car::PROOF_EXPECT: expect,
        "proof_probe_verified": meaning,
    });
    patch[format!("proof_probe_registration_{registration}")] = json!({
        "probe":probe,"expect":expect,"verified":meaning,"by":actor,"at":at,
        "event":md.get(boss_jobs::car::PROOF_EVENT),
        "previous":{"probe":md.get(boss_jobs::car::PROOF_PROBE),"expect":md.get(boss_jobs::car::PROOF_EXPECT),"verified":md.get("proof_probe_verified")},
    });
    Ok(patch)
}

pub(crate) async fn record_probe(
    car_ref: &str,
    probe: &str,
    expect: &str,
    meaning: &str,
    dry: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let cars = crate::sor_client::all_cars_at(&http, &base).await?;
    let car = find_car(&cars, car_ref, Eligible::Live)?;
    let id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("car has no id"))?;
    let path = format!("/api/jobs/{id}/metadata");
    let actor = crate::identity::sign(&reqwest::Method::PATCH, &path)?;
    let patch = probe_registration_patch(
        car,
        probe,
        expect,
        meaning,
        &actor,
        &now.to_rfc3339(),
        &uuid::Uuid::new_v4().to_string(),
    )?;
    if dry {
        println!(
            "boss prove: DRY — would register this probe; nothing runs and no proof is recorded\n{patch}"
        );
        return Ok(());
    }
    crate::sor_client::api_at(
        &http,
        &base,
        reqwest::Method::PATCH,
        &path,
        Some(patch.clone()),
    )
    .await?;
    let after = crate::sor_client::api_at(
        &http,
        &base,
        reqwest::Method::GET,
        &format!("/api/jobs/{id}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("registered probe readback is absent"))?;
    let actual = after
        .get("metadata")
        .ok_or_else(|| anyhow::anyhow!("registered probe metadata readback is absent"))?;
    let expected = patch
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("registered probe patch is not an object"))?;
    if expected
        .iter()
        .any(|(key, value)| actual.get(key) != Some(value))
    {
        bail!("registered probe readback differs; no execution or proof was claimed");
    }
    if after.pointer("/metadata/proof_event") != car.pointer("/metadata/proof_event") {
        bail!("original event changed during registration; no proof was claimed");
    }
    println!(
        "boss prove: recorded probe on {id}; nothing ran and the proof stays owed\n  run: boss ops forge run-car-probe {id} --wait"
    );
    Ok(())
}

/// The host a verb is running on. Shared with `boss car open`, which
/// records it on the car at build start — the same question, one
/// definition (CLAUDE.md §9a).
pub(crate) fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The evidence the `proven` step carries, and when it was recorded.
///
/// `now` is the instant the probe ran under, not a fresh one taken at
/// write time: the proof and its stamp describe a single event, and a
/// slow API call should not drag the timestamp away from the probe.
fn proven_metadata(
    verified: &str,
    proof: &str,
    method: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut md = json!({
        "verified": verified,
        // VERBATIM, as a string, exactly like the gate receipt: what the
        // machine saw, not a summary of it.
        "proof": proof,
        "completed_at": crate::sor_client::stamp(now),
    });
    if let Some(m) = method {
        md["method"] = json!(m);
    }
    md
}

/// The two writes that complete `proven` with a proof, in order: the
/// proof's keys through the step's MERGE door, then a status-only flip.
/// Pure, so the shape is pinned without a socket, and shared by the
/// attended and the unattended door so neither can drift alone.
///
/// Why two writes and not the one PUT this used to be (e39a9d2a, the
/// car after the gate's refused verdicts, 2026-09-24): the step PUT
/// REPLACES `metadata` wholesale, and the registry materializes keys
/// onto every step at admission (`metadata_defaults`, `authority_role`,
/// `station`, `audience`, `claimable`), so a fresh `{verified, proof,
/// completed_at}` body deleted every stored key it did not name, on
/// every proof. The item's last car makes the PUT refuse such a body
/// outright; this verb would then fail to record the proofs it exists
/// to record. The merge door lands the keys against the row as it
/// stands, in one transaction. It goes FIRST because `verified` and
/// `proof` are required at done and the flip is where that is judged.
fn proven_writes(car_id: &str, step_id: &str, md: &Value) -> Vec<(reqwest::Method, String, Value)> {
    vec![
        (
            reqwest::Method::PATCH,
            format!("/api/jobs/{car_id}/steps/{step_id}/metadata"),
            md.clone(),
        ),
        (
            reqwest::Method::PUT,
            format!("/api/jobs/{car_id}/steps/{step_id}"),
            json!({"status": "completed"}),
        ),
    ]
}

// ---------------------------------------------------------------------
// THE UNATTENDED DOOR — `boss prove <car> --from-car --unattended`
// ---------------------------------------------------------------------
//
// What the forge's ops-runner runs for a `run-car-probe` ops-request
// (infra/ops/verbs/run-car-probe.json), filed by the dispatcher rule
// run-car-probes-on-train-arrived for every car aboard an arrived
// train that recorded a probe at park time, and again by
// recheck-failing-probes-hourly for a car whose last run settled
// nothing. Until backlog 9f00a805 (consolidation H8, car 2) the verb
// ran infra/forge/run-car-probe.sh — 482 lines of shell re-implementing
// this file's judge, verdict and records, because the forge had no
// `boss` binary; car 1 installs the tree's CLI there from the converged
// image (infra/estate/install-cli-from-image.sh), so the twin is
// retired and the ops-request runs this door instead. Measured before
// choosing it: of the four candidate twins this was the only one with
// a CLI verb to retire INTO (tenant-census.sh, retire-example-
// reference-rows.sh and prune-registry-versions.sh have none), and the
// gaps it had were these, each closed here:
//
//   - the probe runs as ANOTHER USER (root drops to $BOSS_PROBE_USER)
//     in the converged checkout ($BOSS_PROBE_DIR) under a timeout
//     ($BOSS_PROBE_TIMEOUT, default the reader door's lifetime,
//     `probe_reader::MAX_LIFETIME_SECS`) — `Shell::unattended`. On the
//     forge both are named by the drop-in infra/forge/probe-account.sh
//     writes: the account boss-probe and its read-only view,
//     /var/lib/boss/probe-view (backlog 703358ce). There is no default:
//     the twin's, david and /home/david/boss, were retired on 2026-10-09,
//     and a host with no drop-in runs no probe (`unattended_ground`);
//   - the probe's environment is exactly what a recorded probe is
//     promised: BOSS_JOBS_URL, a READ-ONLY reader identity as
//     BOSS_SOR_USER (backlog 61085a9e — never this verb's own write
//     actor, which is stripped), the reader's port table as
//     BOSS_SOR_PORTS (design 28d2bed9, read as data from the checkout's
//     infra/forge/sor-ports.env), infra/forge/probe-bin first on
//     PATH so `boss-sor-read` is the cheap thing to type, and the
//     car's OWN converged instant (backlog a92571a6 — the fixed cutoff
//     a dated claim compares against, since the checkout's HEAD moves
//     with every train) — every piece read out of the checkout only
//     after the account has answered (`unattended_ground`, review
//     37f44583 F1), and the probe launched with no_new_privs set
//     (`NO_NEW_PRIVS`, review acab446c F8b) — and NOTHING ELSE of the
//     ops runner's environment but PATH, LANG, LC_ALL and TZ
//     (`UNATTENDED_INHERITS`, the same finding's environment half);
//   - every outcome lands on the car: PROVEN completes `proven` with
//     the proof record (and `proven_by`, which the yard reads); every
//     other verdict — NOT YET, NOT RUN, NOT PROVEN — stamps
//     `proof_attempt` and leaves `proven` ready, where the hand door
//     prints NOT PROVEN to the operator and records nothing;
//   - the exit code IS the verdict the ops-request carries: 0 proven,
//     1 not proven, 3 did not run, 75 not yet, 2 refused;
//   - a refusal changes nothing and names why (exit 2); a car already
//     proven is "nothing to run", exit 0, because the daily recheck
//     re-files for a car whose attempt has since been settled by hand.

/// THE READER A RECORDED PROBE READS THE SYSTEM OF RECORD AS (backlog
/// 61085a9e). The probe is program text a builder wrote, run here as
/// the probe user; the privilege matches the job — read the system of
/// record, change nothing. `audit-readonly` is what core policy
/// (boss-policy-client::defaults) grants Read at Scope::All on every
/// shipped resource and NO other action anywhere: verified by effect on
/// the live deployment, a PATCH and a step PUT under it both answered
/// 403 while every list read matched the operator's. The role is
/// NAMED in `boss_core::roles` and DEFINED by the policy defaults, and
/// `the_probes_reader_role_can_read_everything_and_write_nothing` holds
/// them equal (CLAUDE.md §9a); this crate reads the name from core
/// (`identity::READER_ROLE`) rather than spelling it again — until
/// backlog d843abf2 (2026-09-19) it was a literal here, the second
/// spelling the unidentified reader in identity.rs would have needed a
/// third of. The id is the one the credentials door already names for
/// this reader (`boss_jobs::credentials`), and since design b35c22b4 it
/// is read from core too: every machine gate writes this same id over a
/// probe-reader caller's own, so the door and the gate cannot disagree
/// about who a probe is.
pub(crate) const READER_ACTOR: &str = boss_core::roles::PROBE_READER_ACTOR;

/// The `x-boss-user` header a recorded probe's reader sends —
/// `boss-sor-read` puts it on the wire verbatim. One shape with the
/// CLI's own unidentified read (identity.rs), because they are the
/// same identity: a reader nobody-in-particular is, with the
/// platform's own read role.
pub(crate) use crate::identity::reader_header;

/// The reader's port table, from `infra/forge/sor-ports.env` in the
/// checkout the probe runs in: `name=port` lines, `#` comments and
/// blanks dropped, one space between entries — the shape
/// `boss-sor-read` parses from `BOSS_SOR_PORTS`. Read as DATA, never
/// sourced (this verb runs as root at that door). An absent file is an
/// empty table, which the reader treats as absent: jobs only.
pub(crate) fn sor_ports_table(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// THE ACCOUNT THE UNATTENDED DOOR DROPS TO, if the host names one:
/// `BOSS_PROBE_USER` from the ops runner's drop-in
/// (infra/forge/probe-account.sh). `None` is a host that names none, and
/// there the door runs nothing ([`unnamed_probe_host_refusal`]) — the
/// twin's default, the checkout's owner, was retired on 2026-10-09
/// (backlog 703358ce). One reading, so the host check of the reader
/// socket's hand-over (`probe_door_check`) hands to the account this
/// door would.
pub(crate) fn probe_user() -> Option<String> {
    std::env::var("BOSS_PROBE_USER")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Where the twin's tools live, relative to the checkout the probe runs
/// in: the sanctioned reader on PATH, and the port table it reads.
const PROBE_BIN: &str = "infra/forge/probe-bin";
pub(crate) const SOR_PORTS_ENV: &str = "infra/forge/sor-ports.env";

impl Shell {
    /// The unattended door's shell, from the same environment the
    /// twin read (`BOSS_PROBE_USER`, `BOSS_PROBE_DIR`,
    /// `BOSS_PROBE_TIMEOUT`). An account or directory the host does not
    /// name stays `None` — never the checkout's owner or its checkout —
    /// and [`unattended_ground`] refuses such a shell before anything
    /// runs.
    ///
    /// ENVIRONMENT ONLY, NO FILE (backlog dda26693, review 37f44583 F1).
    /// This used to end in [`Shell::with_probe_reader`], which opens the
    /// port table inside `BOSS_PROBE_DIR` as root, and the door built it
    /// as the argument to [`unattended_ground`] — so on the tick whose
    /// verify had refused the view, root still opened a path in it,
    /// following a link and reading it whole. The reader half is now
    /// added by `unattended_ground`, after the account has answered.
    pub(crate) fn unattended() -> Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let user = probe_user();
        let dir = var("BOSS_PROBE_DIR").map(std::path::PathBuf::from);
        let timeout = match var("BOSS_PROBE_TIMEOUT") {
            Some(t) => t.parse::<u64>().map_err(|_| {
                anyhow::anyhow!("BOSS_PROBE_TIMEOUT={t:?} is not a number of seconds")
            })?,
            // The door's own lifetime: one bound, never a second 60
            // (backlog a32af300).
            None => crate::probe_reader::MAX_LIFETIME_SECS,
        };
        Ok(Self {
            path_prefix: None,
            reader: None,
            cwd: dir,
            user,
            timeout_secs: Some(timeout),
            env: Vec::new(),
            // Nothing of the ops runner's own beyond the allow-list
            // (review acab446c F8b, the environment half).
            inherit: Some(&UNATTENDED_INHERITS),
            // This verb's own write credential never reaches the
            // probe's text; `boss_jobs::probe::names_an_actor` refuses
            // a probe that sets one, and this is the other half. Nor
            // does the machine token, which `execute_with` keeps out of
            // reach at both doors (`door_env::NoTokenInReach`; review
            // ef2da426 F1, backlog 844b936e).
            strip: [crate::identity::ACTOR_ENV, crate::identity::ACTOR_FILE_ENV]
                .into_iter()
                .map(str::to_string)
                .collect(),
        })
    }

    /// THE ENVIRONMENT A RECORDED PROBE IS PROMISED, BUILT ONCE FOR
    /// BOTH DOORS (backlog 18fee481, measured 2026-09-22). `tree` is the
    /// checkout the reader and its port table are read out of — the
    /// converged checkout at the unattended door, the operator's own
    /// worktree at the hand door — and `base` is the system of record
    /// the verb resolved.
    ///
    /// WHY THE HAND DOOR GETS IT TOO. `--from-car` exists so an operator
    /// can rehearse locally what the forge will run, and its own help
    /// says so; it ran that text with the caller's PATH and nothing
    /// else, so `boss prove <car> --from-car` on the dev pod answered
    /// `boss-sor-read not found` for every car whose probe reads the
    /// system of record — which is most of them. The refusal was
    /// correct (did-not-run, not failed) and the defect was upstream of
    /// it: two doors onto one text, in two environments, and the flag
    /// meant to make them agree was the one that did not. Nothing has to
    /// be installed for this — the reader is IN THE TREE beside its
    /// route table. Measured that hour: the shed sat at 8 of 12 cars
    /// open past 24 h, oldest 108 h, with the forge's hourly recheck as
    /// its only worker.
    ///
    /// The privilege is unchanged and server-enforced: the identity is
    /// the READ-SCOPED reader, at both doors, and never
    /// `operator:unidentified` — an unidentified read is answered with a
    /// NARROWER WORLD silently, which is a green absence assertion
    /// against a page the probe was never allowed to see (61085a9e). It
    /// does not come from the tree, so a caller standing outside a
    /// worktree is still handed it; what such a caller loses is the
    /// reader on PATH, and a probe that needs it then says `not found`,
    /// which is the honest did-not-run.
    ///
    /// What this does NOT make the same is the rest of the hand door:
    /// the probe still runs HERE, as the operator, with their own
    /// `BOSS_ACTOR` and no timeout — unless this host holds a reader
    /// credential, in which case the door that opens bounds the probe
    /// at its own 60 s and says so (`Shell::bound`, review 0bd6a9c2 F2) — `admit` already says so, and a
    /// pod-local proof is the established shape for a claim only the
    /// cluster can show.
    pub(crate) fn with_probe_reader(mut self, tree: Option<&Path>, base: &str) -> Self {
        let reader = std::env::var("BOSS_PROBE_READER_ACTOR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| READER_ACTOR.into());
        // One open of the file: the text and whose file it was, so the
        // door judges the owner of what was READ (review 991bb439, N3).
        let (ports, ports_from) = tree
            .and_then(|t| crate::probe_reader::read_ports_file(&t.join(SOR_PORTS_ENV)))
            .map(|(text, source)| (sor_ports_table(&text), Some(source)))
            .unwrap_or_default();
        self.reader = Some(crate::probe_reader::Config {
            ports_from,
            credential: std::env::var_os(crate::probe_reader::CREDENTIAL_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(crate::probe_reader::DEFAULT_CREDENTIAL)),
            base: base.to_string(),
            ports: ports.clone(),
        });
        self.path_prefix = tree.map(|t| t.join(PROBE_BIN));
        self.env.push(("BOSS_JOBS_URL".into(), base.to_string()));
        self.env
            .push(("BOSS_SOR_USER".into(), reader_header(&reader)));
        self.env.push(("BOSS_SOR_PORTS".into(), ports));
        self
    }
}

/// The proven step, by slug first and title second — the twin's read,
/// and the one the arrival handler uses to decide a car is probe-able.
fn proven_step_unattended(car: &Value) -> Option<&Value> {
    let steps = car.get("steps").and_then(Value::as_array)?;
    steps
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("proven"))
        .or_else(|| {
            steps
                .iter()
                .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
        })
}

/// What the unattended door found before running anything. Pure over
/// the car, so the refusals are pinned without a socket.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// Run this probe, expect this token, complete this step id with
    /// this `verified` prose.
    Run {
        probe: String,
        expect: String,
        step_id: String,
        verified: String,
    },
    /// A car that closed LANDED with its proof OWED (backlog b9005734):
    /// run this probe, and pay the debt on the car itself — its `proven`
    /// was skipped by the landing close, so there is no step to complete.
    Owed {
        probe: String,
        expect: String,
        verified: String,
    },
    /// `proven` is already completed — nothing to run, and not a red.
    AlreadyProven,
    /// Not a car this door will run: the reason, for the packet.
    Refused(String),
}

/// The twin's admission, in order: kind, merged, probe and expect
/// (with the event-bound car named as such), a `proven` step, its
/// status. Every refusal changes nothing.
pub(crate) fn admit_unattended(car: &Value) -> Admitted {
    let short = crate::train::id8(car.get("id").and_then(Value::as_str).unwrap_or("?"));
    let kind = car.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "ship-a-change" {
        return Admitted::Refused(format!("{short} is a {kind}, not a ship-a-change car"));
    }
    let merged = car
        .get("metadata")
        .and_then(|m| m.get("merged"))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();
    if merged != "true" {
        return Admitted::Refused(format!(
            "{short} has not merged (metadata.merged={merged:?}) — a probe against production \
             proves nothing about unshipped code"
        ));
    }
    let (probe, expect) = match car_probe(car) {
        Ok(pair) => pair,
        Err(e) => return Admitted::Refused(format!("{short}: {e}")),
    };
    let Some(step) = proven_step_unattended(car) else {
        return Admitted::Refused(format!("{short} has no proven step"));
    };
    // `verified` is the car's own claim (the summary), the same
    // default the hand door's --from-car uses; a car without one is
    // still proven, by the probe it recorded.
    let verified = || {
        car_verified(car, None)
            .unwrap_or_else(|_| "proven by the probe the car recorded at park time".into())
    };
    let md = car.get("metadata");
    // THE MARKER IS NOT ENOUGH (review 9daaa7a9 F2). Any UPDATE_JOB
    // holder could PATCH `proof_owed`, and nothing clears it when a car
    // marked owed is later disproved, abandoned or unlanded — each of
    // which skips `proven` exactly as the landing close does. So an owed
    // proof is run, and a paid one stands, only for a car that is CLOSED,
    // still LANDED by the one definition (`is_landed`, which reads
    // `unlanded` where the bare `merged` string above cannot), and
    // neither disproved nor abandoned — each mark read as `is_landed`
    // reads its own, string or bool (backlog 82946cd9).
    let fallen = skipped_cause(car);
    let owed = md.is_some_and(boss_jobs::car::owes_proof) && fallen.is_none();
    let paid = md
        .and_then(|m| m.get(boss_jobs::car::PROOF_OWED))
        .and_then(Value::as_str)
        == Some("paid")
        && fallen.is_none();
    match step.get("status").and_then(Value::as_str).unwrap_or("") {
        "ready" | "active" => {}
        "completed" => return Admitted::AlreadyProven,
        // A PAID debt is proven, not abandoned (review 9daaa7a9 F1): a
        // repeat request — filed by hand, re-filed, or the hourly settle
        // of an unsettled one — answers as the step path's repeat does,
        // nothing to run, exit 0, never REFUSED with the wrong reason.
        // Only while the car still stands: paid and LATER disproved is
        // not "already proven" (backlog 82946cd9).
        "skipped" if paid => return Admitted::AlreadyProven,
        // THE OBLIGATION'S DOOR (b9005734, car 2). The landing close
        // skipped `proven`, and the marker — not the step — is what is
        // owed, on a car that still stands landed (see `owed` above).
        "skipped" if owed => {
            return Admitted::Owed {
                probe,
                expect,
                verified: verified(),
            };
        }
        // A skipped step is refused with ITS cause, never a guessed one:
        // "skipped = abandoned" told an unlanded or disproved car it was
        // abandoned (backlog 82946cd9).
        "skipped" => {
            let cause = fallen.unwrap_or(
                "it owes no proof: proof_owed is not \"true\", so the step was skipped \
                 without a debt (abandoned, or closed without one)",
            );
            return Admitted::Refused(format!(
                "{short}'s proven step is \"skipped\" and nothing stands to run — {cause}"
            ));
        }
        other => {
            return Admitted::Refused(format!(
                "{short}'s proven step is {other:?} (pending = not merged)"
            ));
        }
    }
    let Some(step_id) = step.get("id").and_then(Value::as_str) else {
        return Admitted::Refused(format!("{short}'s proven step has no id"));
    };
    Admitted::Run {
        probe,
        expect,
        step_id: step_id.to_string(),
        verified: verified(),
    }
}

/// Why a car no longer STANDS to have its owed proof run or its payment
/// honoured, by its own cause, or `None` when it does: closed, landed,
/// neither disproved nor abandoned. Each mark is read by
/// [`boss_jobs::car::is_marked`], the reading `is_landed` uses.
fn skipped_cause(car: &Value) -> Option<&'static str> {
    use boss_jobs::car::{is_landed, is_marked};
    if is_marked(car, "disproved") {
        Some("the car is disproved: its claim was judged false after it landed")
    } else if is_marked(car, "abandoned") {
        Some("the car is abandoned")
    } else if !is_landed(car) {
        Some("the car is unlanded: its change was taken back off main")
    } else if car.get("status").and_then(Value::as_str) != Some("closed") {
        Some("the car is not closed, so its skipped step is not the landing's")
    } else {
        None
    }
}

/// The car metadata key an owed proof is paid under — spelled once, in
/// core, beside the server rule that reserves it (backlog 82946cd9).
pub(crate) use boss_jobs::car::OWED_PROOF;

/// PURE: the one metadata PATCH that pays an owed proof — the proof on
/// the car and the marker turned to `paid`, in ONE write, so no reader
/// can see a proof recorded while the debt still reads open, or the
/// reverse. `paid` (not a deleted key) because the obligation's reader
/// asks `metadata_has=proof_owed`: a paid car stays findable as one
/// that owed and was paid, and `owes_proof` answers false for it.
///
/// THE SERVER ADMITS IT ONLY FROM THE CREDENTIALED RUNNER ON A PROBE
/// HOST (backlog 82946cd9, `boss_jobs::car::payment_refusal`): both job
/// doors refuse this patch, 409, from any caller the credential door did
/// not resolve to `runner:ops` on a host the run-car-probe verb runs on.
/// So `run_unattended` sends it — and only it — presenting this host's
/// runner credential ([`runner_credential`]). A credential it cannot
/// read is said on stderr and the write goes without it: the server's
/// 409 then refuses the payment loudly, never a payment anyone could
/// have typed.
pub(crate) fn owed_paid_patch(
    verified: &str,
    proof: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut paid = proven_metadata(verified, proof, None, now);
    paid["proven_by"] = json!(PROVEN_BY);
    json!({
        (boss_jobs::car::PROOF_OWED): "paid",
        (OWED_PROOF): paid,
    })
}

/// Where this host's ops-runner credential is read: the variable and
/// default infra/ops/ops-runner.sh reads, held equal to that line by
/// `the_payment_reads_the_runners_own_credential_file` (§9a).
pub(crate) const RUNNER_CREDENTIAL_FILE_ENV: &str = "BOSS_RUNNER_CREDENTIAL_FILE";
pub(crate) const RUNNER_CREDENTIAL_FILE_DEFAULT: &str = "/etc/boss/ops-runner.credential";

/// PURE: the header a read of the runner credential file makes — the
/// value as ops-runner.sh's `$(cat …)` reads it, trailing newlines
/// dropped, marked sensitive so no Debug print carries it — or why not.
/// A reason never quotes the value.
pub(crate) fn runner_credential_header(
    read: std::io::Result<String>,
) -> std::result::Result<reqwest::header::HeaderValue, String> {
    let raw = read.map_err(|e| format!("reading it failed: {e}"))?;
    let value = raw.trim_end_matches('\n');
    if value.is_empty() {
        return Err("it is empty".into());
    }
    let mut header = reqwest::header::HeaderValue::from_str(value)
        .map_err(|_| "it is not one header value".to_string())?;
    header.set_sensitive(true);
    Ok(header)
}

/// THIS HOST'S RUNNER CREDENTIAL, for the owed payment alone (backlog
/// 82946cd9). The verb runs as a child of ops-runner.sh, as the runner's
/// own account, so it reads the file the runner reads and presents what
/// the runner presents: no one new can present it, because reading that
/// root-only file is already all presenting it takes. It is read only
/// after the probe has run (`run_unattended`), so the builder-authored
/// probe never runs beside a process that holds it, and it rides only
/// the payment — no other write this door makes is reserved to the
/// runner. The error names the file and why, never the value.
fn runner_credential() -> std::result::Result<reqwest::header::HeaderValue, String> {
    let file = std::env::var(RUNNER_CREDENTIAL_FILE_ENV)
        .ok()
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| RUNNER_CREDENTIAL_FILE_DEFAULT.to_string());
    runner_credential_header(std::fs::read_to_string(&file))
        .map_err(|why| format!("the runner credential at {file} was NOT presented ({why})"))
}

/// The one line the ops-request's output leads its verdict with: the
/// ALL-CAPS word a reader of the packet scans for, then the car, then
/// why. The twin printed the same four words for two weeks of packets,
/// and `the_unattended_verdict_line_leads_with_its_word` pins them.
pub(crate) fn unattended_verdict_line(
    short: &str,
    verdict: &Verdict,
    expect: &str,
    why: &str,
) -> String {
    match verdict {
        Verdict::Proven => format!("PROVEN {short} — exit 0 and printed {expect:?}"),
        Verdict::NotYet { .. } => format!("NOT YET {short} — {why}"),
        Verdict::Unrunnable { .. } => format!("NOT RUN {short} — {why}"),
        Verdict::NotProven(_) => format!("NOT PROVEN {short} — {why}"),
    }
}

/// The exit code the ops-request carries for a verdict — see
/// [`NOT_YET_EXIT`], [`UNRUNNABLE_EXIT`].
pub(crate) fn unattended_exit(verdict: &Verdict) -> i32 {
    match verdict {
        Verdict::Proven => 0,
        Verdict::NotYet { .. } => NOT_YET_EXIT,
        Verdict::Unrunnable { .. } => UNRUNNABLE_EXIT,
        Verdict::NotProven(_) => 1,
    }
}

/// Who completed `proven` at this door — what the yard's inspection
/// shed shows as the stamp's `by`, and the verb's name so a reader of
/// the step finds the door that wrote it.
pub(crate) const PROVEN_BY: &str = "run-car-probe";

/// Run a car's recorded probe the way the forge's ops-runner asks for
/// it, and write the verdict on the car — see the header of this
/// section. `car_id` is the full uuid the verb's argument pattern
/// admitted; the car is re-read from the system of record, never
/// trusted from the packet that asked.
pub(crate) async fn run_unattended(car_id: &str, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let car = crate::sor_client::api(
        &http,
        reqwest::Method::GET,
        &format!("/api/jobs/{car_id}"),
        None,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("GET /api/jobs/{car_id} answered no body"))?;
    let short = crate::train::id8(car_id);
    // `step_id` is `None` for an OWED car: closed, its `proven` skipped,
    // so a green pays the debt on the car rather than completing a step.
    let (probe, expect, step_id, verified) = match admit_unattended(&car) {
        Admitted::Run {
            probe,
            expect,
            step_id,
            verified,
        } => (probe, expect, Some(step_id), verified),
        Admitted::Owed {
            probe,
            expect,
            verified,
        } => (probe, expect, None, verified),
        Admitted::AlreadyProven => {
            println!("boss prove: {short} is already proven — nothing to run");
            return Ok(());
        }
        Admitted::Refused(why) => {
            eprintln!("boss prove: REFUSED — {why}");
            std::process::exit(REFUSED_EXIT);
        }
    };
    // The gate's rule on a recorded probe, at this door too: a probe
    // that reads the system of record unidentified is refused — it
    // would pass an absence assertion against a narrowed world — and
    // there is no operator here to override. A caller cleared from the
    // gate log alone is refused here too, where the hand door only warns
    // ([`unattended_refusal`], review 4ce5ca85 N1).
    if let Some(r) = unattended_refusal(car_id, &probe) {
        eprintln!("boss prove: REFUSED — {r}");
        std::process::exit(REFUSED_EXIT);
    }

    println!("boss prove: {short}  $ {probe}");
    // THE TREE THIS PROBE READS, observed where it will run — the
    // converged checkout. Here the standing is nearly always
    // `unreadable`, because the forge's clone need not carry a
    // remote-tracking ref, and `tree_head` is the fact that matters:
    // production's own revision at the moment the claim was judged.
    // Observed only once the account has answered: `unverified` is its
    // refusal, and then nothing was run or opened in that directory at
    // all — the reader's port table included (review 37f44583 F1).
    let (shell, tree, unverified) =
        unattended_ground(Shell::unattended()?, car_merge_ref(&car), &base);
    let at = now.to_rfc3339();
    let here = host();
    let actor = shell.probe_actor();
    let o = match unverified.map_or_else(|| execute_with(&probe, &shell), not_run) {
        Ok(o) => o,
        // The host refused the probe's environment: recorded on the car
        // as a NOT YET naming the host and the cause, so the hourly
        // recheck retries it and the shed shows it — never red, and
        // never the ~72 silent hours an unrecorded arrival run cost
        // (backlog 2e1f609e, review 19ecf37f).
        Err(e) => {
            let cause = environment_refusal(e)?;
            let path = format!("/api/jobs/{car_id}/metadata");
            let http = &http;
            let wrote = record_refusal(
                Door::Unattended,
                false,
                &refusal_patch(&car, &probe, Some(&expect), &here, &actor, &at, &cause),
                |patch| async move {
                    crate::sor_client::api(http, reqwest::Method::PATCH, &path, Some(patch))
                        .await
                        .map(|_| ())
                },
            )
            .await;
            if let Err(w) = wrote {
                eprintln!(
                    "boss prove: recording the environment refusal on {short} failed too — {w}"
                );
            }
            exit_refused(Door::Unattended, &cause)
        }
    };
    let verdict = verdict(&probe, &o, Some(&expect));
    if let Verdict::Proven = verdict {
        let proof = proof_json(&probe, Some(&expect), &o, &here, &at, &tree, None);
        let proof = serde_json::to_string(&proof)?;
        let artifact_id = car
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("the resolved proof car has no identity"))?;
        let machine_frame = machine_proof_frame(artifact_id, &proof)?;
        let writes = match &step_id {
            Some(step_id) => {
                let mut md = proven_metadata(&verified, &proof, None, now);
                md["proven_by"] = json!(PROVEN_BY);
                proven_writes(car_id, step_id, &md)
            }
            None => vec![(
                reqwest::Method::PATCH,
                format!("/api/jobs/{car_id}/metadata"),
                owed_paid_patch(&verified, &proof, now),
            )],
        };
        // Only the owed payment presents the runner credential, read
        // here, after the probe has exited ([`runner_credential`]).
        let runner = match &step_id {
            Some(_) => None,
            None => match runner_credential() {
                Ok(credential) => Some(credential),
                Err(why) => {
                    eprintln!("boss prove: {why}; the jobs API refuses the payment without it");
                    None
                }
            },
        };
        for (method, path, body) in writes {
            let wrote = match &runner {
                Some(credential) => {
                    crate::sor_client::api_presenting_runner(
                        &http,
                        method,
                        &path,
                        Some(body),
                        credential,
                    )
                    .await
                }
                None => crate::sor_client::api(&http, method, &path, Some(body)).await,
            };
            wrote.map_err(|e| {
                anyhow::anyhow!(
                    "the probe passed but recording it on {short} failed — {e}; re-file \
                         the ops-request or run boss prove {short} --from-car"
                )
            })?;
        }
        println!(
            "boss prove: {}",
            unattended_verdict_line(&short, &verdict, &expect, "")
        );
        let shown = o.stdout.trim();
        if !shown.is_empty() {
            println!("{shown}");
        }
        // The protected ops output carries the executor's exact observation,
        // not a caller's later copy of a car's mutable metadata (6b8e4e5a).
        println!("{machine_frame}");
        return Ok(());
    }
    let why = match &verdict {
        Verdict::NotYet { said } => not_yet_why(&here, said),
        Verdict::Unrunnable { missing } => unrunnable_why_unattended(
            &here,
            missing,
            shell.user.as_deref().unwrap_or("?"),
            shell.cwd.as_deref().unwrap_or(Path::new("?")),
        ),
        Verdict::NotProven(e) => e.to_string(),
        Verdict::Proven => unreachable!("handled above"),
    };
    let prior = car.pointer("/metadata/proof_attempt");
    let attempt = attempt_json(&probe, Some(&expect), &o, &here, &actor, &at, &why, prior);
    let attempt = with_wait_observed(attempt, &car, &shell, prior, &at);
    crate::sor_client::api(
        &http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{car_id}/metadata"),
        Some(json!({"proof_attempt": attempt})),
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "probe exited {} and recording the attempt on {short} failed too — {e}",
            o.exit
        )
    })?;
    println!(
        "boss prove: {}",
        unattended_verdict_line(&short, &verdict, &expect, &why)
    );
    println!(
        "boss prove: proof_attempt recorded, {}",
        if step_id.is_some() {
            "proven stays ready"
        } else {
            "the proof stays owed"
        }
    );
    println!(
        "  stdout: {}\n  stderr: {}",
        if o.stdout.trim().is_empty() {
            "(empty)"
        } else {
            o.stdout.trim()
        },
        if o.stderr.trim().is_empty() {
            "(empty)"
        } else {
            o.stderr.trim()
        }
    );
    std::process::exit(unattended_exit(&verdict))
}

fn machine_proof_frame(car_id: &str, proof: &str) -> Result<String> {
    let frame = boss_jobs::move_diagnosis::proof::FRAME;
    let value: Value = serde_json::from_str(proof)?;
    if value.get("exit").and_then(Value::as_i64) != Some(0)
        || ["stdout", "stderr"].iter().any(|key| {
            value
                .get(key)
                .and_then(Value::as_str)
                .is_none_or(|s| s.contains(frame))
        })
    {
        bail!("the probe cannot author the executor's machine proof frame");
    }
    let envelope = boss_jobs::move_diagnosis::proof::Envelope {
        schema: 1,
        artifact: uuid::Uuid::parse_str(car_id)?,
        proof_json: proof.to_string(),
    };
    Ok(format!("{frame}{}", serde_json::to_string(&envelope)?))
}

/// Run a probe against production and record it on the car — or refuse.
pub(crate) async fn run(
    car_ref: &str,
    probe: Option<String>,
    expect: Option<String>,
    exit_only: bool,
    verified: Option<String>,
    method: Option<String>,
    recheck: bool,
    replace: bool,
    dry: bool,
    from_car: bool,
    // The stated override for a probe this door refuses — see [`admit`].
    probe_anyway: Option<String>,
    // The operator's now, taken once at the CLI entry point and passed
    // in — the same shape `train::run` uses, so nothing down here reads
    // the wall clock on its own.
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let overriding = override_reason(probe_anyway.as_deref())?;
    let http = crate::sor_client::machine_client()?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    let cars = crate::sor_client::all_cars_at(&http, &base).await?;
    // THE TREE THE PROBE'S READER COMES OUT OF (backlog 18fee481): the
    // worktree the operator is standing in, which carries
    // `infra/forge/probe-bin` by construction. Outside one there is no
    // reader to put on PATH — say so here rather than let the probe
    // report `boss-sor-read not found` as if the tool were missing from
    // the machine, which is the reading that sent a session looking for
    // an install that was never needed.
    let tree = crate::brief::repo_root().ok();
    if tree.is_none() {
        eprintln!(
            "boss prove: NOTE — this is not a checkout, so {PROBE_BIN} is not on the \
             probe's PATH and a probe that reads the system of record will report \
             `{reader}: command not found`. Run this from one.",
            reader = boss_jobs::probe::SOR_READER,
        );
    }
    // `--recheck` RE-RUNS a recorded probe and writes nothing, so a
    // finished car is a legitimate target for it and for nothing else.
    // Every other path records a proof, which a closed or cancelled car
    // cannot take — so those are not candidates, and naming them as
    // "open" is what sent an operator after a dead twin (0a58827d).
    let eligible = if recheck {
        Eligible::AnyStatus
    } else {
        Eligible::Live
    };
    let car = find_car(&cars, car_ref, eligible)?;
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("car has no id"))?
        .to_string();
    let short = &car_id[..8.min(car_id.len())];

    // --recheck reads the proof already on the step and re-runs it. It
    // is read-only on purpose: a decayed proof is a finding to act on,
    // not something to quietly overwrite.
    if recheck {
        let step = car
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
            .ok_or_else(|| anyhow::anyhow!("the car has no step titled {PROVEN:?}"))?;
        let rec = recorded_probe_for(car, step)?;
        let (probe, expect) = (rec.probe, rec.expect);
        let here = host();
        let dir_exists = rec.cwd.as_deref().is_none_or(|d| Path::new(d).is_dir());
        let placement = recheck_placement(
            &probe,
            rec.host.as_deref(),
            &here,
            rec.cwd.as_deref(),
            dir_exists,
            || checkout_holding(car_merge_ref(car), &Shell::here(None).host_free()),
        );
        if let Some(refusal) = cannot_rerun_here(&placement, &probe) {
            bail!("{refusal}");
        }
        match &placement {
            Rerunnable::Anywhere => println!(
                "boss prove: this probe reads only the system of record through `{}`, so it \
                 re-runs here whatever host or directory recorded it",
                boss_jobs::probe::SOR_READER
            ),
            Rerunnable::AnyCheckout { root } => println!(
                "boss prove: this probe reads only git objects at a named revision, so it \
                 re-runs in {root}, whose HEAD holds the car, whatever host or directory \
                 recorded it"
            ),
            Rerunnable::Here | Rerunnable::WrongHost { .. } | Rerunnable::MissingDir { .. } => {}
        }
        // THE RULES APPLY TO A RE-RUN TOO, and this is the path that
        // most needs them: `--recheck` is the unattended re-run, so a
        // recorded probe that reads the system of record unidentified
        // would report a confident HOLDS against a narrowed world. The
        // refusal says so instead, and names `--replace` — which exists
        // to put a better probe under a claim already proven.
        if let Some(r) = admit(&probe, true).refusal {
            match overriding {
                None => bail!(
                    "CANNOT RE-CHECK THIS PROBE — {r}\n\n\
                     A recorded proof is not evidence of itself: re-prove the car with \
                     `--replace` and a probe that reads as a named reader, which leaves the \
                     original on the step where a reader can see what used to be claimed."
                ),
                Some(reason) => println!(
                    "boss prove: re-checking a probe this door refuses, on your stated \
                     reason — {reason}"
                ),
            }
        }
        if let Some(o) = &rec.overridden {
            println!(
                "boss prove: this proof was recorded OVER A REFUSAL — {}",
                serde_json::to_string(o).unwrap_or_else(|_| "(unreadable)".into())
            );
        }
        if let Source::Attempt { not_yet } = rec.source {
            println!(
                "boss prove: {short} is not proven — re-running the probe of its last \
                 attempt, which said {}",
                if not_yet { "NOT YET" } else { "NOT PROVEN" }
            );
        }
        println!("boss prove: re-running the recorded probe for {short}\n  $ {probe}");
        // The recorded cwd, and the car's own converged instant read
        // there — a re-run compares against the same fixed cutoff the
        // unattended door hands over, or the claim means a different
        // thing at each door (a92571a6). Where, and in what environment,
        // the placement decides (`recheck_shell`).
        let shell = recheck_shell(&placement, rec.cwd.as_deref())
            .with_probe_reader(tree.as_deref(), &base)
            .with_car_instant(car_merge_ref(car));
        // A record read on a host with no reader credential is refused
        // before anything is sent (design 00227628).
        refuse_a_record_read_without_a_reader(&probe, &shell, Door::Recheck)?;
        // WHICH TREE THIS RE-RUN READS (backlog 6f581de6). A recheck
        // records nothing, so there is no immutable fact to refuse —
        // but its HOLDS / NO LONGER HOLDS is acted on by a human, and
        // acting on a verdict taken off an unnamed tree is the same
        // defect one step removed. So it is stated, never refused.
        println!(
            "{}",
            crate::freshness::unrecorded_tree_note(
                &probe_tree(&shell).tree,
                crate::freshness::freshness_silenced(),
            )
        );
        // A recheck writes nothing on any outcome, a refusal included.
        let o = match execute_with(&probe, &shell) {
            Ok(o) => o,
            Err(e) => exit_refused(Door::Recheck, &environment_refusal(e)?),
        };
        // THREE READINGS, and which record they are read against
        // decides the sentence: a PROOF that fails now has decayed; an
        // ATTEMPT was never a proof. --recheck writes nothing on any of
        // them — `proven` stays exactly as it was (ready, for a not-yet
        // car), and a not-yet exits 75 so a script can tell.
        return match (verdict(&probe, &o, expect.as_deref()), rec.source) {
            (Verdict::Proven, Source::Proof) => {
                println!("boss prove: HOLDS — {short} is still true in production");
                Ok(())
            }
            (Verdict::Proven, Source::Attempt { .. }) => {
                println!(
                    "boss prove: NOW PROVABLE — {short}'s last attempt did not settle the \
                     claim and the same probe passes now. Nothing is recorded by --recheck; \
                     run boss prove {short} --from-car (or --probe/--expect) to record it \
                     and complete `{PROVEN}`."
                );
                Ok(())
            }
            (Verdict::NotYet { said }, _) => {
                println!("boss prove: {}", not_yet_why(&here, &said));
                println!("boss prove: --recheck records nothing; `{PROVEN}` stays as it is");
                std::process::exit(NOT_YET_EXIT)
            }
            // DID NOT RUN (46f67333): not decay, not "still not proven" —
            // the claim was not tested either way, which is the fact
            // 66fd64c6 taught this path to keep separate. Exit 3, as the
            // forge does.
            (Verdict::Unrunnable { missing }, _) => {
                println!("boss prove: {}", unrunnable_why(&here, &missing));
                println!("boss prove: --recheck records nothing; `{PROVEN}` stays as it is");
                std::process::exit(UNRUNNABLE_EXIT)
            }
            (Verdict::NotProven(e), Source::Proof) => bail!(
                "NO LONGER HOLDS — {short} was proven once and is not true now.\n\n{e}\n\n\
                 A proof can decay honestly: a step-plugin ConfigMap preview survives \
                 exactly until the next converge, and a car that never landed leaves \
                 prod looking fixed until it isn't."
            ),
            (Verdict::NotProven(e), Source::Attempt { .. }) => bail!(
                "STILL NOT PROVEN — {short}'s recorded probe does not pass now either.\n\n{e}"
            ),
        };
    }

    // --from-car: the probe the builder recorded at park time, and the
    // car's summary as the default prose. The clap-level conflicts
    // keep --probe/--expect/--exit-only off this path.
    let (probe, expect, verified) = if from_car {
        let (p, e) = car_probe(car)?;
        println!("boss prove: {short} runs its recorded probe");
        (Some(p), Some(e), Some(car_verified(car, verified)?))
    } else {
        (probe, expect, verified)
    };
    let probe = probe.ok_or_else(|| {
        anyhow::anyhow!(
            "--probe is required: proof is a command that ran, not a sentence \
             (or --from-car, to run the probe the car recorded at park time)"
        )
    })?;
    let verified = verified.ok_or_else(|| {
        anyhow::anyhow!("--verified is required: say what the probe means, in prose, for a reader")
    })?;
    if expect.is_none() && !exit_only {
        bail!(
            "give --expect '<string the probe must print>', or pass --exit-only to assert \
             on the exit code alone.\n\n\
             Exit codes are a fine assertion when the command IS the test (`grep -q`, \
             `test -f`), and a weak one otherwise — `echo hi` exits 0. Downgrading is \
             allowed, but it is recorded in the proof so a reader can see what was \
             actually asserted."
        );
    }

    // Refuse before running anything: a car that has not merged should
    // cost a line of output, not a probe against production.
    let target = proven_step(car, replace)?;
    // ...and a --method the protocol does not accept is the same class
    // of refusal, for the same reason. It used to be caught only by the
    // API when the finished proof was recorded, so the probe ran, the
    // evidence was good, and it was discarded on a 400.
    if let Some(m) = method.as_deref() {
        check_enum_field(target, "method", m)?;
    }

    // A bare number is legal but weak, so say so BEFORE the probe runs
    // — the reader is looking at the output right then, which is the
    // only moment the better shape is cheap to adopt.
    if let Some(w) = expect.as_deref().and_then(bare_number_warning) {
        eprintln!("{w}");
    }

    // THE SAME RULES THE GATE APPLIES, AT THIS DOOR (23b2dffa), before
    // the probe runs and before anything is recorded.
    let admission = admit_against_claim(&probe, &verified, from_car);
    for w in &admission.warnings {
        eprintln!("{w}");
    }
    let overridden = match (&admission.refusal, overriding) {
        (Some(r), None) => bail!("{r}"),
        (Some(r), Some(reason)) => {
            println!(
                "boss prove: running a probe this door refuses, on your stated reason — \
                 {reason}\n  It is recorded in the proof as `overridden`, so a later reader \
                 (and `--recheck`) sees the claim was argued past rather than clean."
            );
            Some(override_record(r.rule, reason))
        }
        (None, Some(_)) => {
            eprintln!(
                "boss prove: {OVERRIDE_FLAG} was given but nothing refused this probe — \
                 nothing is recorded as overridden."
            );
            None
        }
        (None, None) => None,
    };

    // WHICH TREE THIS PROBE IS ABOUT TO READ (backlog a09bd894), and a
    // refusal when it is not the one the forge reads. The first half of
    // this fix (18fee481) gave `--from-car` the forge's probe
    // ENVIRONMENT; this is the other half, because the two doors then
    // agreed about the environment and disagreed about the TREE — a
    // recorded probe reads it with `git show HEAD:<path>`, and on the
    // pod HEAD is whatever the checkout last fast-forwarded to, which
    // under load is trains behind (nine, measured 2026-09-22).
    //
    // Only `--from-car`: that flag exists so an operator can rehearse
    // what the forge will run, so it is the one door that PROMISES the
    // forge's reading. A hand-written `--probe` is the operator's own
    // text about their own tree, and refusing it would be this verb
    // deciding what their probe meant.
    let shell = Shell::here(None)
        .with_probe_reader(tree.as_deref(), &base)
        .with_car_instant(car_merge_ref(car));
    // A record read on a host with no reader credential is refused before
    // anything is sent, `--dry` included — a dry run still runs the probe
    // (design 00227628).
    refuse_a_record_read_without_a_reader(&probe, &shell, Door::Hand)?;
    // OBSERVED ONCE, USED TWICE: the guard below judges it, and the
    // proof records it (backlog 6f581de6). Two readings could disagree
    // — a train lands between them — and a proof stamped with a tree
    // the guard did not judge is the same gap in a new place.
    let obs = probe_tree(&shell);
    // The shape only the tree can show, read where the probe will read
    // it (8ac42ee5) — said BEFORE the run, so a NOT YET that follows is
    // read beside the reason it will never clear.
    let dir = shell.cwd.as_deref().unwrap_or_else(|| Path::new("."));
    if let Some(w) = prose_only_warning(&probe, "HEAD", git_show_reader(dir, "HEAD")) {
        eprintln!("boss prove: {w}");
    }
    if from_car {
        match crate::freshness::stale_tree_guard(
            &obs.tree,
            // A `--dry` run records nothing, so it is the rehearsal the
            // refusal leaves open rather than a write to refuse.
            !dry,
            crate::freshness::freshness_silenced(),
        ) {
            crate::freshness::BaseGuard::Note(n) => println!("{n}"),
            crate::freshness::BaseGuard::Refuse(why) => bail!("{why}"),
        }
    }

    println!("boss prove: {short}  $ {probe}");
    let at = now.to_rfc3339();
    let actor = shell.probe_actor();
    let o = match execute_with(&probe, &shell) {
        Ok(o) => o,
        // Recorded on the car as the unattended door records it
        // (`refusal_patch`; backlog 2e1f609e, reviews 19ecf37f and
        // 2d0043ad); a --dry run records nothing. A failed write is said
        // and still exits 2: nothing ran, whatever the record did.
        Err(e) => {
            let cause = environment_refusal(e)?;
            let path = format!("/api/jobs/{car_id}/metadata");
            let http = &http;
            let wrote = record_refusal(
                Door::Hand,
                dry,
                &refusal_patch(car, &probe, expect.as_deref(), &host(), &actor, &at, &cause),
                |patch| async move {
                    crate::sor_client::api(http, reqwest::Method::PATCH, &path, Some(patch))
                        .await
                        .map(|_| ())
                },
            )
            .await;
            match wrote {
                Ok(true) => println!("boss prove: the refusal is recorded on {short}"),
                Ok(false) => {}
                Err(w) => eprintln!(
                    "boss prove: recording the environment refusal on {short} failed too — {w}"
                ),
            }
            exit_refused(Door::Hand, &cause)
        }
    };
    match verdict(&probe, &o, expect.as_deref()) {
        Verdict::Proven => {}
        Verdict::NotProven(e) => return Err(e),
        // DID NOT RUN (46f67333): the channel named a tool this machine
        // lacks, so nothing about the claim was judged — not a verdict
        // against it, and not a proof. What lands is the forge's record
        // from this door: `proof_attempt{unrunnable:true, missing_tools}`
        // on the car, `proven` untouched, exit 3 as the forge exits.
        Verdict::Unrunnable { missing } => {
            let here = host();
            let why = unrunnable_why(&here, &missing);
            println!("boss prove: {why}");
            let step_status = target.get("status").and_then(Value::as_str).unwrap_or("?");
            if dry {
                println!(
                    "boss prove: DRY — would record this as proof_attempt on {short}; \
                     `{PROVEN}` stays {step_status}"
                );
                std::process::exit(UNRUNNABLE_EXIT);
            }
            let prior = car.pointer("/metadata/proof_attempt");
            let attempt = attempt_json(
                &probe,
                expect.as_deref(),
                &o,
                &here,
                &actor,
                &at,
                &why,
                prior,
            );
            crate::sor_client::api(
                &http,
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car_id}/metadata"),
                Some(json!({"proof_attempt": attempt})),
            )
            .await?;
            println!(
                "boss prove: proof_attempt recorded on {short}; `{PROVEN}` stays {step_status}"
            );
            std::process::exit(UNRUNNABLE_EXIT);
        }
        // NOT YET (726562de): the probe ran and said the claim cannot be
        // judged until something happens. Not a verdict against the
        // change, so nothing is refused — and not a proof, so nothing
        // completes. What lands is the forge's record, from this door:
        // `proof_attempt{not_yet:true}` on the car, `proven` untouched,
        // and the hourly recheck (recheck-failing-probes-hourly picks up
        // any car carrying an attempt) runs it again. Exit 75, as the
        // probe did and as the forge does.
        Verdict::NotYet { said } => {
            let here = host();
            let why = not_yet_why(&here, &said);
            println!("boss prove: {why}");
            let step_status = target.get("status").and_then(Value::as_str).unwrap_or("?");
            if dry {
                println!(
                    "boss prove: DRY — would record this as proof_attempt on {short}; \
                     `{PROVEN}` stays {step_status}"
                );
                std::process::exit(NOT_YET_EXIT);
            }
            let prior = car.pointer("/metadata/proof_attempt");
            let attempt = attempt_json(
                &probe,
                expect.as_deref(),
                &o,
                &here,
                &actor,
                &at,
                &why,
                prior,
            );
            let attempt = with_wait_observed(attempt, car, &shell, prior, &at);
            crate::sor_client::api(
                &http,
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car_id}/metadata"),
                Some(json!({"proof_attempt": attempt})),
            )
            .await?;
            println!(
                "boss prove: proof_attempt recorded on {short}; `{PROVEN}` stays {step_status}"
            );
            std::process::exit(NOT_YET_EXIT);
        }
    }

    let proof = proof_json(
        &probe,
        expect.as_deref(),
        &o,
        &host(),
        &at,
        &obs,
        overridden.as_ref(),
    );
    let shown = o.stdout.trim();
    println!(
        "boss prove: probe exited 0{}\n  {}",
        match &expect {
            Some(w) => format!(" and printed {w:?}"),
            None => " (--exit-only, no output asserted)".into(),
        },
        if shown.is_empty() {
            "(no output)"
        } else {
            shown
        }
    );

    if dry {
        println!("boss prove: DRY — would record this proof on {short} and complete `{PROVEN}`");
        return Ok(());
    }

    // Re-read the step id from the car we already fetched.
    let sid = proven_step(car, replace)?
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the `{PROVEN}` step has no id"))?
        .to_string();

    let md = proven_metadata(
        &verified,
        &serde_json::to_string(&proof)?,
        method.as_deref(),
        now,
    );

    if replace {
        // THE STEP IS FROZEN, SO THE NEW PROOF LANDS BESIDE IT. A
        // completed step cannot be rewritten — which is right, because
        // the original proof is evidence about the system and erasing
        // it would destroy the record of what used to hold. So the
        // replacement appends to `reproof` in JOB metadata, the same
        // door `regate_receipt` uses for a receipt whose branch moved,
        // and `--recheck` reads the newest entry in preference.
        let mut history: Vec<Value> = car
            .get("metadata")
            .and_then(|m| m.get("reproof"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        history.push(json!({
            "proof": serde_json::to_string(&proof)?,
            "verified": verified,
            "recorded_at": crate::sor_client::stamp(now),
        }));
        crate::sor_client::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{car_id}/metadata"),
            Some(json!({"reproof": history})),
        )
        .await?;
        println!(
            "boss prove: {short} re-proven — recorded as reproof #{}. The original \
             proof is untouched on the step; a proof that used to hold and no longer \
             does is evidence, not a mistake to erase.",
            history.len()
        );
        return Ok(());
    }

    for (method, path, body) in proven_writes(&car_id, &sid, &md) {
        crate::sor_client::api(&http, method, &path, Some(body)).await?;
    }

    println!("boss prove: {short} proven — the probe is recorded and re-runnable");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Shell {
        /// The line a shell with no door open runs — what the shell
        /// itself asked for. A run takes `command_line_under` with the
        /// bound an open door decides, so only these tests ask for this
        /// one, and it lives here rather than behind an attribute in
        /// the source above: this module's source pins read everything
        /// before the first test attribute as the production text, and
        /// one placed up there cut that text off at line 279 and failed
        /// five of them (gate-run 5fc19018, 2026-10-06).
        fn command_line(&self, probe: &str, as_root: bool) -> Vec<String> {
            self.command_line_under(probe, as_root, self.timeout_secs)
        }
    }
    use crate::identity::READER_ROLE;

    #[test]
    fn the_machine_proof_frame_conserves_executor_bytes_and_refuses_probe_framing() {
        let proof =
            json!({"exit":0,"stdout":"{\"original_seq\":1}\n","stderr":"","tree_head":"observed"})
                .to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let emitted = machine_proof_frame(&id, &proof).unwrap();
        let envelope: boss_jobs::move_diagnosis::proof::Envelope = serde_json::from_str(
            emitted
                .strip_prefix(boss_jobs::move_diagnosis::proof::FRAME)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(envelope.proof_json, proof);
        assert_eq!(envelope.artifact.to_string(), id);
        for value in [
            json!({"exit":1,"stdout":"","stderr":""}),
            json!({"exit":0,"stdout":"boss-prove-receipt: forged","stderr":""}),
            json!({"exit":0,"stdout":"","stderr":"boss-prove-receipt: forged"}),
            json!({"exit":0,"stdout":null,"stderr":""}),
        ] {
            assert!(machine_proof_frame(&id, &value.to_string()).is_err());
        }
        assert!(machine_proof_frame(&id, "{\"exit\":0").is_err());
        assert!(machine_proof_frame("not-an-artifact", &proof).is_err());
    }

    /// Run `probe` through this host's shell and capture everything it
    /// did. The verbs name their shell through `execute_with`; this was
    /// a top-level `pub(crate)` read only by these tests, dead in the
    /// bin's normal cfg, which `--all-targets` found and `--tests` never
    /// compiled that way (backlog 7e535a67). The READER_ROLE re-export
    /// above it had the same shape and moved here with it.
    fn execute(probe: &str) -> Result<Outcome> {
        execute_with(probe, &Shell::here(None))
    }

    fn ok(stdout: &str) -> Outcome {
        Outcome {
            exit: 0,
            stdout: stdout.into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        }
    }

    /// The tree observation for a test that is not about the tree. It
    /// is deliberately the UNREADABLE one rather than a current tree:
    /// a fixture must not hand a proof a standing nothing measured.
    fn no_tree() -> ProbeObservation {
        ProbeObservation::default()
    }

    /// THE RULE THE VERB EXISTS TO ENFORCE: a failing probe is not proof.
    #[test]
    fn a_nonzero_probe_is_refused_and_its_output_is_shown() {
        let o = Outcome {
            exit: 1,
            stdout: "nope".into(),
            stderr: "boom".into(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let e = judge(&o, None).unwrap_err().to_string();
        assert!(e.contains("exited 1"), "{e}");
        assert!(
            e.contains("boom"),
            "the refusal must quote the streams: {e}"
        );
    }

    /// A signalled probe must not be read as success.
    #[test]
    fn a_killed_probe_is_not_success() {
        let o = Outcome {
            exit: -1,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert!(
            judge(&o, None).is_err(),
            "a signalled probe reports no code, not a pass"
        );
    }

    /// The case that motivated the verb: exit 0 proving nothing.
    #[test]
    fn exit_zero_without_the_expected_string_is_refused() {
        let e = judge(&ok("hi"), Some("MARKER")).unwrap_err().to_string();
        assert!(e.contains("never printed"), "{e}");
        assert!(
            e.contains("echo hi"),
            "the refusal should name the weakness: {e}"
        );
    }

    #[test]
    fn the_expected_string_may_arrive_on_stderr() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: "MARKER present".into(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert!(
            judge(&o, Some("MARKER")).is_ok(),
            "real probes report on stderr too"
        );
    }

    #[test]
    fn a_matching_probe_passes() {
        assert!(judge(&ok("MARKER present"), Some("MARKER")).is_ok());
    }

    /// THE BUG (421b3032): `--expect "1"` accepted a probe that printed
    /// `10`, because the comparison was a substring test. The claim was
    /// true; the proof did not check what was meant. A numeric
    /// expectation must appear as a WHOLE token or it is not evidence.
    #[test]
    fn a_number_does_not_match_a_longer_number() {
        for printed in ["10", "21", "100", "0.15", "v1.2.3", "step_1a"] {
            let e = judge(&ok(printed), Some("1")).unwrap_err().to_string();
            assert!(
                e.contains("never printed"),
                "expect \"1\" must not match {printed:?}: {e}"
            );
            assert!(
                e.contains("whole"),
                "the refusal must say the number is compared as a whole token: {e}"
            );
        }
    }

    /// ...and it still matches when it really is the number printed.
    #[test]
    fn a_number_matches_when_it_stands_alone() {
        for printed in ["1", "1 rule", "count=1", "rules: 1", "1\n", "(1)"] {
            assert!(
                judge(&ok(printed), Some("1")).is_ok(),
                "expect \"1\" must match {printed:?}"
            );
        }
    }

    /// Only NUMERIC expectations tighten. A token expectation is still
    /// a substring test, which is what every good probe relies on.
    #[test]
    fn a_non_numeric_expectation_is_still_a_substring() {
        assert!(judge(&ok("prefix-claim:ok-suffix"), Some("claim:ok")).is_ok());
        assert!(judge(&ok("unprovenX"), Some("unproven")).is_ok());
    }

    /// The stronger rider: a bare number is a weak expectation even
    /// when it matches, so recording one warns and names the shape
    /// that cannot lie.
    #[test]
    fn a_bare_number_expectation_warns_at_record_time() {
        let w = bare_number_warning("1").expect("a bare number must warn");
        assert!(w.contains("WEAK"), "the warning must be loud: {w}");
        assert!(
            w.contains("-eq 1 && echo claim:ok"),
            "the warning must name the shape that cannot lie: {w}"
        );
        assert!(bare_number_warning("0.15").is_some());
        assert!(
            bare_number_warning("claim:ok").is_none(),
            "a token expectation is the good shape and must not warn"
        );
        assert!(bare_number_warning("1 rule").is_none());
    }

    /// The probe is actually EXECUTED, not parsed — this is what makes
    /// the recorded output evidence rather than transcription.
    #[test]
    fn execute_captures_what_really_happened() {
        let o = execute("printf hello; printf oops >&2; exit 3").unwrap();
        assert_eq!(o.exit, 3);
        assert_eq!(o.stdout, "hello");
        assert_eq!(o.stderr, "oops");
    }

    /// THE CAR CARRIES ITS PROBE (28ac45ab). `--from-car` reads the
    /// pair the auto-park handler copied from the gate's park intent,
    /// under the one set of keys core defines.
    #[test]
    fn from_car_reads_the_probe_the_car_recorded() {
        let car = serde_json::json!({
            "id": "c1", "metadata": {
                "summary": "The yard shows the probe count. And why.",
                "proof_probe": "curl -s http://sor/api/yard | grep -c unproven",
                "proof_expect": "unproven"
            }
        });
        let (p, e) = car_probe(&car).unwrap();
        assert_eq!(p, "curl -s http://sor/api/yard | grep -c unproven");
        assert_eq!(e, "unproven");
        // The prose defaults to the car's own claim, and an explicit
        // --verified still wins.
        assert_eq!(
            car_verified(&car, None).unwrap(),
            "The yard shows the probe count. And why."
        );
        assert_eq!(car_verified(&car, Some("seen".into())).unwrap(), "seen");
    }

    #[test]
    fn from_car_runs_a_recorded_probe_without_erasing_its_event() {
        let car = json!({"metadata": {
            "proof_event": "the next actual native fence observation",
            "proof_probe": "boss-sor-read /api/jobs/known | jq -e '.status == \"closed\"'",
            "proof_expect": "true"
        }});
        let original = car.clone();
        let (probe, expect) = car_probe(&car).unwrap();
        assert!(probe.starts_with("boss-sor-read"));
        assert_eq!(expect, "true");
        assert_eq!(car, original);
    }

    #[test]
    fn record_probe_is_a_distinct_nonexecuting_cli_door() {
        use clap::Parser;
        let parsed = crate::Cli::try_parse_from([
            "boss",
            "prove",
            "c0ffee12",
            "--record-probe",
            "--probe",
            "boss-sor-read /api/jobs/known",
            "--expect",
            "closed",
            "--verified",
            "Observe the original event after it occurs",
        ]);
        assert!(parsed.is_ok());
        for option in [
            "--from-car",
            "--unattended",
            "--recheck",
            "--replace",
            "--exit-only",
            "--disproved",
        ] {
            assert!(
                crate::Cli::try_parse_from([
                    "boss",
                    "prove",
                    "c0ffee12",
                    "--record-probe",
                    "--probe",
                    "true",
                    "--expect",
                    "token",
                    "--verified",
                    "meaning",
                    option,
                ])
                .is_err(),
                "conflicting proof operation {option}"
            );
        }
    }

    #[test]
    fn registering_a_probe_retains_prior_intent_and_never_records_a_verdict() {
        let car = json!({"id":"c0ffee12", "kind":"ship-a-change", "status":"open",
            "metadata":{"merged":"true", "proof_event":"actual native event", "proof_probe":"old", "proof_expect":"old-expect"},
            "steps":[{"title":PROVEN,"status":"ready"}]});
        let original = car.clone();
        let patch = probe_registration_patch(
            &car,
            "boss-sor-read /api/jobs/known",
            "closed",
            "the observed event",
            "actor",
            "time",
            "registration-1",
        )
        .unwrap();
        assert_eq!(car, original);
        assert!(patch.get("proof_event").is_none());
        assert!(patch.get("proof_attempt").is_none());
        assert!(patch.get("status").is_none());
        assert_eq!(
            patch["proof_probe_registration_registration-1"]["previous"]["probe"],
            "old"
        );
        assert_eq!(
            patch["proof_probe_registration_registration-1"]["event"],
            "actual native event"
        );
        assert_eq!(patch["proof_probe"], "boss-sor-read /api/jobs/known");
        for state in ["pending", "completed", "skipped"] {
            let mut bad = car.clone();
            bad["steps"][0]["status"] = json!(state);
            assert!(
                probe_registration_patch(&bad, "true", "token", "meaning", "actor", "time", "id")
                    .is_err()
            );
        }
        let mut unlanded = car;
        unlanded["metadata"]["merged"] = json!("false");
        assert!(
            probe_registration_patch(&unlanded, "true", "token", "meaning", "actor", "time", "id")
                .is_err()
        );
    }

    /// No probe recorded → a refusal that names the park-time door; an
    /// event-bound car → a refusal that quotes the event, so nobody
    /// runs "nothing" and records a yes.
    #[test]
    fn from_car_refuses_an_unprobed_or_event_bound_car() {
        let bare = serde_json::json!({"id": "c1", "metadata": {"summary": "x"}});
        let e = car_probe(&bare).unwrap_err().to_string();
        assert!(e.contains("--park-probe"), "{e}");

        let event = serde_json::json!({"id": "c1", "metadata": {
            "proof_event": "event-bound — the next yard-button cancel"
        }});
        let e = car_probe(&event).unwrap_err().to_string();
        assert!(e.contains("EVENT-BOUND"), "{e}");
        assert!(e.contains("yard-button cancel"), "quotes the event: {e}");

        // A probe with no expectation is refused here too — the gate
        // refuses the pair, so this is a hand-edited car.
        let half = serde_json::json!({"id": "c1", "metadata": {"proof_probe": "true"}});
        let e = car_probe(&half).unwrap_err().to_string();
        assert!(e.contains("echo hi"), "{e}");

        // And with no summary there is no default prose to fall back on.
        let e = car_verified(&half, None).unwrap_err().to_string();
        assert!(e.contains("--verified is required"), "{e}");
    }

    /// Guards the bug class that made a hand-rolled check lie on
    /// 2026-08-28: `grep -c || echo 0` prints "0\n0" on no match. Under
    /// this verb the same probe is refused, because grep's exit is 1.
    #[test]
    fn the_grep_c_fallback_that_lied_is_now_refused() {
        let o = execute("printf '' | grep -c MARKER || echo 0").unwrap();
        assert_eq!(
            o.stdout.trim(),
            "0\n0".trim_matches('"'),
            "reproduces the double zero"
        );
        // The honest form is what the verb pushes callers toward:
        let honest = execute("printf '' | grep -q MARKER").unwrap();
        assert!(
            judge(&honest, None).is_err(),
            "no match must refuse, not pass"
        );
    }

    #[test]
    fn a_proof_round_trips_through_the_recorded_form() {
        let o = ok("MARKER present");
        let p = proof_json(
            "grep -q MARKER f",
            Some("MARKER"),
            &o,
            "h",
            "2026-08-28T00:00:00Z",
            &no_tree(),
            None,
        );
        let step = json!({"metadata": {"proof": serde_json::to_string(&p).unwrap()}});
        let rec = recorded_probe(&step).unwrap();
        assert_eq!(rec.probe, "grep -q MARKER f");
        assert_eq!(rec.expect.as_deref(), Some("MARKER"));
    }

    /// A RECORDED PROOF SAYS WHICH TREE ANSWERED IT (backlog 6f581de6).
    /// `host` and `cwd` say WHERE it ran; a directory's contents change
    /// under it, and a recorded probe reads the tree with `git show
    /// HEAD:<path>` — so without the sha the artifact is incomplete in
    /// the one dimension this verb exists to close. Provenance is the
    /// first of the five properties, and the standing rides beside the
    /// sha because a later reader cannot re-derive it: `origin/main`
    /// has moved a hundred times by the time anyone reads the proof.
    #[test]
    fn a_recorded_proof_names_the_tree_it_read() {
        use crate::freshness::{Base, TreeObservation};
        let o = ok("MARKER present");
        let tree = TreeObservation {
            standing: Base::Behind,
            head: "1c63ca24ffff".into(),
            main_head: "de960a2affff".into(),
            behind_by: 9,
            unreadable: None,
        };
        let p = proof_json(
            "grep -q MARKER f",
            Some("MARKER"),
            &o,
            "h",
            "2026-09-22T00:00:00Z",
            &ProbeObservation {
                tree,
                ..Default::default()
            },
            None,
        );
        assert_eq!(p["tree_head"], json!("1c63ca24ffff"));
        assert_eq!(p["tree_standing"], json!("behind"));
        // BESIDE the existing keys, not instead of them: these eight
        // are what a live proof carried when this was measured, and
        // `--recheck` reads three of them back.
        for k in [
            "at", "cwd", "exit", "expect", "host", "probe", "stderr", "stdout",
        ] {
            assert!(p.get(k).is_some(), "{k} is a contract, not a detail: {p}");
        }
    }

    #[test]
    fn a_proof_records_the_directory_its_shell_actually_used() {
        let dir = boss_testing::scratch::scratch_dir("proof-executed-cwd");
        let shell = Shell::here(Some(&dir));
        let probe = "pwd -P";
        let outcome = execute_with(probe, &shell).unwrap();
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let proof = proof_json(
            probe,
            None,
            &outcome,
            "private-host",
            "now",
            &probe_tree(&shell),
            None,
        );
        assert_eq!(
            proof["cwd"].as_str(),
            Some(outcome.stdout.trim()),
            "{proof}"
        );
        assert_eq!(
            proof["tree_head"],
            Value::Null,
            "a non-repository must not invent a revision: {proof}"
        );
        assert_eq!(proof["tree_standing"], "unreadable");
    }

    #[test]
    fn a_proof_observes_the_tree_and_identity_as_the_probe_user() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let root = boss_testing::scratch::scratch_dir("proof-executed-user");
        #[cfg(unix)]
        // mode-bits-ok: a fixture directory nobody traverses and creates its own checkout in
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        let dir = root.join("user-checkout");
        let dropping = running_as_root();
        let git = |args: &[&str]| {
            let mut command = if dropping {
                let mut command = std::process::Command::new("runuser");
                command.args(["-u", "nobody", "--", "git"]);
                command
            } else {
                std::process::Command::new("git")
            };
            let output = command
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "private fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "private fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main", dir.to_str().unwrap()]);
        git(&[
            "-C",
            dir.to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "fixture",
        ]);
        let head = git(&["-C", dir.to_str().unwrap(), "rev-parse", "HEAD"]);
        git(&[
            "-C",
            dir.to_str().unwrap(),
            "update-ref",
            "refs/remotes/origin/main",
            &head,
        ]);
        let shell = Shell {
            user: Some("nobody".into()),
            ..Shell::here(Some(&dir))
        };
        let probe = "pwd -P; id -un; id -u; git rev-parse HEAD";
        let outcome = execute_with(probe, &shell).unwrap();
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let lines: Vec<_> = outcome.stdout.lines().collect();
        assert_eq!(lines.len(), 4, "{outcome:?}");
        assert_eq!(lines[3], head);
        if dropping {
            assert_eq!(lines[1], "nobody");
        }
        let tree = probe_tree(&shell);
        println!(
            "actual execution identity={} uid={} head={} observed tree={tree:?}",
            lines[1], lines[2], head
        );
        let proof = proof_json(probe, None, &outcome, "private-host", "now", &tree, None);
        assert_eq!(proof["tree_head"].as_str(), Some(head.as_str()), "{proof}");
        assert_eq!(proof["cwd"].as_str(), Some(lines[0]), "{proof}");
        assert_eq!(proof["user"].as_str(), Some(lines[1]), "{proof}");
        assert_eq!(proof["uid"].as_str(), Some(lines[2]), "{proof}");
        // A missing remote ref is a standing gap, not loss of the
        // checkout identity git already answered for this same user.
        git(&[
            "-C",
            dir.to_str().unwrap(),
            "update-ref",
            "-d",
            "refs/remotes/origin/main",
        ]);
        let without_main = probe_tree(&shell);
        let proof = proof_json(
            probe,
            None,
            &outcome,
            "private-host",
            "now",
            &without_main,
            None,
        );
        assert_eq!(proof["tree_head"].as_str(), Some(head.as_str()), "{proof}");
        assert_eq!(proof["tree_standing"], "unreadable");
    }

    #[test]
    fn an_unavailable_execution_directory_is_not_replaced_by_the_parent() {
        let dir = boss_testing::scratch::scratch_dir("proof-unavailable-cwd").join("absent");
        let observed = probe_tree(&Shell::here(Some(&dir)));
        let proof = proof_json(
            "true",
            None,
            &ok(""),
            "private-host",
            "now",
            &observed,
            None,
        );
        assert_eq!(proof["cwd"], Value::Null, "{proof}");
        assert_eq!(proof["user"], Value::Null, "{proof}");
        assert_eq!(proof["uid"], Value::Null, "{proof}");
        assert_eq!(proof["tree_head"], Value::Null, "{proof}");
        assert_eq!(
            proof["context_unreadable"].as_array().unwrap().len(),
            3,
            "{proof}"
        );
        assert!(execute_with("true", &Shell::here(Some(&dir))).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_symbolic_execution_directory_is_recorded_as_its_physical_directory() {
        let root = boss_testing::scratch::scratch_dir("proof-symlink-cwd");
        let dir = root.join("real");
        let alias = root.join("alias");
        std::fs::create_dir(&dir).unwrap();
        std::os::unix::fs::symlink(&dir, &alias).unwrap();
        let shell = Shell::here(Some(&alias));
        let observed = probe_tree(&shell);
        let outcome = execute_with("pwd -P", &shell).unwrap();
        assert_eq!(outcome.exit, 0, "{outcome:?}");
        let proof = proof_json(
            "pwd -P",
            None,
            &outcome,
            "private-host",
            "now",
            &observed,
            None,
        );
        assert_eq!(
            proof["cwd"].as_str(),
            Some(outcome.stdout.trim()),
            "{proof}"
        );
        assert_eq!(proof["cwd"], dir.to_str().unwrap());
    }

    /// THE 932aa956 / 3f846cc5 CASE. A probe authored on the workstation
    /// as `ssh boss-gcp ...` re-runs ON boss-gcp, where that name does
    /// not resolve. The claim still held; the instrument was wrong.
    #[test]
    fn a_proof_recorded_elsewhere_cannot_be_rechecked_here() {
        assert_eq!(
            rerunnable(Some("mac-studio"), "boss-gcp", None, true),
            Rerunnable::WrongHost {
                recorded: "mac-studio".into(),
                here: "boss-gcp".into(),
                checkout: None,
            }
        );
    }

    /// THE 64d5e3c7 CASE. The probe opened `git rev-parse --short HEAD`
    /// and was re-run outside any repository.
    #[test]
    fn a_proof_whose_directory_is_gone_cannot_be_rechecked() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/var/lib/boss-train/repo"), false),
            Rerunnable::MissingDir {
                cwd: "/var/lib/boss-train/repo".into(),
                checkout: None,
            }
        );
    }

    /// AND THE ONE THAT MUST NOT BREAK. Every proof already on the board
    /// was recorded without a `cwd`, and the oldest without a `host`.
    /// Refusing on absent context would turn a 30% false-alarm rate into
    /// a 100% one.
    #[test]
    fn absent_context_is_not_a_mismatch() {
        assert_eq!(rerunnable(None, "anywhere", None, true), Rerunnable::Here);
        assert_eq!(
            rerunnable(Some(""), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("unknown"), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("h"), "h", Some(""), false),
            Rerunnable::Here
        );
    }

    /// THE c89a661e CASE. Car 2f5b6df0's attempt was recorded on the
    /// forge in its converged checkout; its probe reads only the system
    /// of record through the sanctioned reader, so the dev pod — another
    /// host, no such directory — can re-run it and mean the same thing.
    /// A probe that reads the tree still cannot.
    #[test]
    fn a_probe_that_reads_only_the_sor_rechecks_from_any_host() {
        let sor_only = "n=$(boss-sor-read '/api/jobs?kind=x' | jq -r '.total // empty'); echo x:ok";
        assert_eq!(
            recheck_placement(
                sor_only,
                Some("david-asus-minipc"),
                "boss-dev-0",
                Some("/home/david/boss"),
                false,
                no_checkout,
            ),
            Rerunnable::Anywhere
        );
        assert_eq!(
            recheck_placement(
                "ssh boss-gcp true && echo ok",
                Some("david-asus-minipc"),
                "boss-dev-0",
                None,
                true,
                || Ok("/work/boss".into()),
            ),
            Rerunnable::WrongHost {
                recorded: "david-asus-minipc".into(),
                here: "boss-dev-0".into(),
                checkout: Some(NoCheckout::Grammar(boss_jobs::probe::Refused::Command {
                    command: "ssh boss-gcp true".into()
                })),
            }
        );
    }

    fn no_checkout() -> Result<String, NoCheckout> {
        Err(NoCheckout::NoMerge)
    }

    /// THE da937eb3 CASE. 12 of 12 sampled W41 rechecks answered CANNOT
    /// RE-RUN HERE, recorded on `david-asus-minipc` or on a dev-pod name
    /// that no longer exists, for probes that read only git objects at
    /// `HEAD`. Such a probe re-runs in any checkout here whose HEAD holds
    /// the car — in that checkout's top level, never the recorded cwd —
    /// and without one it keeps the recorded-host refusal.
    #[test]
    fn a_probe_that_reads_only_git_objects_rechecks_in_a_checkout_that_holds_the_car() {
        let tree_only = "git show HEAD:crates/x.rs | grep -F 'pub fn x(' > /dev/null && echo ok";
        assert_eq!(
            recheck_placement(
                tree_only,
                Some("boss-dev-7c64f98bb7-6trb6"),
                "boss-dev-549f4b77f6-trdbp",
                Some("/scratch/gone"),
                false,
                || Ok("/work/boss".into()),
            ),
            Rerunnable::AnyCheckout {
                root: "/work/boss".into()
            }
        );
        assert_eq!(
            recheck_placement(
                tree_only,
                Some("david-asus-minipc"),
                "boss-dev-0",
                None,
                true,
                no_checkout,
            ),
            Rerunnable::WrongHost {
                recorded: "david-asus-minipc".into(),
                here: "boss-dev-0".into(),
                checkout: Some(NoCheckout::NoMerge),
            }
        );
        // The same host keeps re-running where it recorded, and never
        // pays for the checkout read.
        assert_eq!(
            recheck_placement(tree_only, Some("h"), "h", Some("/tmp"), true, || {
                panic!("a same-host recheck needs no other checkout")
            }),
            Rerunnable::Here
        );
    }

    /// A REFUSED RECHECK SAYS WHICH CAUSE APPLIED (backlog 70cb74d8).
    /// The text is judged first, and a text outside the grammar never
    /// reads the checkout; a text inside it carries the witness's own
    /// answer — on a recorded-host refusal and a missing-directory one
    /// alike.
    #[test]
    fn a_refused_recheck_carries_why_no_checkout_placed_it() {
        let refused = recheck_placement(
            "git show HEAD:x | grep -q \"$HOME\"",
            Some("david-asus-minipc"),
            "boss-dev-0",
            None,
            true,
            || panic!("a text outside the grammar never reads the checkout"),
        );
        assert_eq!(
            refused,
            Rerunnable::WrongHost {
                recorded: "david-asus-minipc".into(),
                here: "boss-dev-0".into(),
                checkout: Some(NoCheckout::Grammar(boss_jobs::probe::Refused::Shell {
                    at: 26,
                    near: "\"$HOME\"".into(),
                })),
            }
        );
        let dubious = rev_parse_refused(
            "/work/boss".into(),
            Some(128),
            "fatal: detected dubious ownership in repository at '/work/boss'\n",
        );
        assert_eq!(
            recheck_placement(
                "git show HEAD:x | grep -q y",
                Some("h"),
                "h",
                Some("/scratch/gone"),
                false,
                || Err(dubious.clone()),
            ),
            Rerunnable::MissingDir {
                cwd: "/scratch/gone".into(),
                checkout: Some(dubious),
            }
        );
    }

    /// THE 51587416 CAUSE, by git's own words. A checkout this user does
    /// not own is refused by git in the host-free shell, which reads no
    /// global `safe.directory`. Measured on the pod 2026-10-19 with git
    /// 2.39.5, uid 65534 reading a root-owned worktree under
    /// `GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null`, exit 128 and
    /// this stderr (no CAP_CHOWN here, so a test cannot plant the owner;
    /// the message is the fixture). Any other rev-parse failure is no
    /// checkout, and its own first line is kept.
    #[test]
    fn a_checkout_this_user_does_not_own_is_named_dubious_ownership() {
        let stderr = "fatal: detected dubious ownership in repository at '/work/boss/x'\n\
                      To add an exception for this directory, call:\n\n\
                      \tgit config --global --add safe.directory /work/boss/x\n";
        assert_eq!(
            rev_parse_refused("/work/boss/x".into(), Some(128), stderr),
            NoCheckout::DubiousOwnership {
                dir: "/work/boss/x".into(),
                exit: Some(128),
                stderr: "fatal: detected dubious ownership in repository at '/work/boss/x'".into(),
            }
        );
        assert_eq!(
            rev_parse_refused(
                "/srv/x".into(),
                Some(128),
                "fatal: not a git repository (or any of the parent directories): .git\n"
            ),
            NoCheckout::NotInACheckout {
                dir: "/srv/x".into(),
                exit: Some(128),
                stderr: "fatal: not a git repository (or any of the parent directories): .git"
                    .into(),
            }
        );
    }

    /// THE WITNESS NAMES EACH WAY IT REFUSES, run against real git in
    /// the host-free shell the recheck uses: no merge on record, a
    /// directory in no repository, a merge the checkout does not have,
    /// and a merge its HEAD is not descended from — and the checkout
    /// that holds the car is still placed.
    #[test]
    fn the_witness_names_each_way_a_checkout_is_refused() {
        let dir = boss_testing::scratch::scratch_dir("prove-no-checkout");
        let repo = dir.join("checkout");
        let bare = dir.join("nowhere");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .expect("git runs");
            assert!(o.status.success(), "git {args:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let at = repo.display().to_string();
        git(&["-C", &at, "init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "one").unwrap();
        git(&["-C", &at, "add", "f"]);
        git(&["-C", &at, "commit", "-q", "-m", "one"]);
        let first = git(&["-C", &at, "rev-parse", "HEAD"]);
        git(&["-C", &at, "checkout", "-q", "--orphan", "other"]);
        git(&["-C", &at, "commit", "-q", "-m", "unrelated"]);
        let top = git(&["-C", &at, "rev-parse", "--show-toplevel"]);
        let shell = Shell::here(Some(&repo)).host_free();

        assert_eq!(checkout_holding(None, &shell), Err(NoCheckout::NoMerge));

        // Outside any repository: the ceiling stops git climbing out of
        // the scratch root into whatever holds it.
        let mut outside = Shell::here(Some(&bare)).host_free();
        outside
            .env
            .push(("GIT_CEILING_DIRECTORIES".into(), dir.display().to_string()));
        match checkout_holding(Some(&first), &outside) {
            Err(NoCheckout::NotInACheckout { dir, exit, stderr }) => {
                assert_eq!(dir, bare.display().to_string());
                assert_eq!(exit, Some(128));
                assert!(stderr.contains("not a git repository"), "{stderr}");
            }
            other => panic!("{other:?}"),
        }

        // A merge this checkout does not have at all.
        let absent = "0123456789abcdef0123456789abcdef01234567";
        match checkout_holding(Some(absent), &shell) {
            Err(NoCheckout::NotHeld {
                root,
                merge,
                exit,
                stderr,
            }) => {
                assert_eq!((root.as_str(), merge.as_str()), (top.as_str(), absent));
                assert_eq!(exit, Some(128));
                assert!(stderr.contains(absent), "{stderr}");
            }
            other => panic!("{other:?}"),
        }

        // A merge it has, which its HEAD does not descend from.
        assert_eq!(
            checkout_holding(Some(&first), &shell),
            Err(NoCheckout::NotHeld {
                root: top.clone(),
                merge: first.clone(),
                exit: Some(1),
                stderr: String::new(),
            })
        );

        // And the checkout that holds it is placed.
        git(&["-C", &at, "checkout", "-q", "main"]);
        assert_eq!(checkout_holding(Some(&first), &shell), Ok(top));
    }

    /// THE TEXT NAMES THE CAUSE THAT APPLIED, with its evidence, where it
    /// used to list three it might have been (backlog 70cb74d8).
    #[test]
    fn the_cannot_rerun_here_text_names_the_cause_that_applied() {
        let probe = "git show HEAD:x | grep -q y";
        let wrong_host = |checkout| Rerunnable::WrongHost {
            recorded: "david-asus-minipc".into(),
            here: "boss-dev-0".into(),
            checkout,
        };
        let text = |p: &Rerunnable| cannot_rerun_here(p, probe).expect("a refusal");
        let generic = "or there is no such checkout here";

        let dubious = text(&wrong_host(Some(rev_parse_refused(
            "/work/boss/x".into(),
            Some(128),
            "fatal: detected dubious ownership in repository at '/work/boss/x'\n",
        ))));
        assert!(dubious.starts_with("CANNOT RE-RUN HERE"), "{dubious}");
        assert!(
            dubious.contains(
                "It was not re-run in a checkout here: git refuses /work/boss/x as a repository \
                 this user does not own (exit 128: fatal: detected dubious ownership"
            ),
            "{dubious}"
        );
        assert!(!dubious.contains(generic), "{dubious}");

        let grammar = text(&wrong_host(Some(NoCheckout::Grammar(
            boss_jobs::probe::Refused::Command {
                command: "ssh boss-gcp true".into(),
            },
        ))));
        assert!(grammar.contains("`ssh boss-gcp true`"), "{grammar}");

        let behind = text(&wrong_host(Some(NoCheckout::NotHeld {
            root: "/work/boss".into(),
            merge: "abc1234".into(),
            exit: Some(1),
            stderr: String::new(),
        })));
        assert!(
            behind.contains("/work/boss does not hold the car's merge abc1234")
                && behind.contains("--is-ancestor`, exit 1)"),
            "{behind}"
        );

        let outside = text(&Rerunnable::MissingDir {
            cwd: "/scratch/gone".into(),
            checkout: Some(NoCheckout::NotInACheckout {
                dir: "/srv".into(),
                exit: Some(128),
                stderr: "fatal: not a git repository".into(),
            }),
        });
        assert!(
            outside.contains("/srv is in no checkout git will read")
                && outside.contains("exit 128: fatal: not a git repository"),
            "{outside}"
        );

        // From `rerunnable` alone nothing was asked, and the old sentence
        // stands; a placement that runs has no refusal text.
        assert!(text(&wrong_host(None)).contains(generic));
        assert_eq!(cannot_rerun_here(&Rerunnable::Here, probe), None);
        assert_eq!(cannot_rerun_here(&Rerunnable::Anywhere, probe), None);
    }

    #[test]
    fn the_same_host_and_a_live_directory_re_runs() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/tmp"), true),
            Rerunnable::Here
        );
    }

    /// The context is recorded so it can be read back — a field that is
    /// written but not recoverable is not a contract.
    #[test]
    fn the_recorded_context_round_trips() {
        let o = Outcome {
            exit: 0,
            stdout: "ok".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let proof = proof_json(
            "echo ok",
            None,
            &o,
            "somehost",
            "2026-08-29T00:00:00Z",
            &probe_tree(&Shell::here(None)),
            None,
        );
        let step = json!({"metadata": {"proof": proof.to_string()}});
        let rec = recorded_probe(&step).expect("readable");
        assert_eq!(rec.host.as_deref(), Some("somehost"));
        assert!(
            rec.cwd.is_some_and(|c| !c.is_empty()),
            "cwd must be recorded"
        );
    }

    #[test]
    fn a_step_proven_by_hand_has_nothing_to_recheck() {
        let step = json!({"metadata": {"verified": "trust me"}});
        let e = recorded_probe(&step).unwrap_err().to_string();
        assert!(e.contains("a claim with no probe under it"), "{e}");
    }

    fn car(id: &str, branch: &str, proven_status: &str) -> Value {
        json!({
            "id": id,
            "title": "a car",
            "metadata": {"branch": branch},
            "steps": [{"id": "s1", "title": PROVEN, "status": proven_status, "metadata": {}}],
        })
    }

    /// An unmerged car cannot be proven in production.
    #[test]
    fn a_pending_proven_step_is_refused_because_it_has_not_merged() {
        let c = car("11111111-a", "feat/x", "pending");
        let e = proven_step(&c, false).unwrap_err().to_string();
        assert!(e.contains("has not merged"), "{e}");
    }

    #[test]
    fn a_ready_proven_step_is_fillable() {
        assert!(proven_step(&car("11111111-a", "feat/x", "ready"), false).is_ok());
    }

    /// A car at a named status — the shape the jobs API returns, which
    /// `car()` predates.
    fn car_at(id: &str, branch: &str, status: &str) -> Value {
        let mut c = car(id, branch, "ready");
        c["status"] = json!(status);
        c
    }

    /// Ambiguity is refused rather than resolved — the failure mode that
    /// cost two cars when `boss park` picked from a list instead.
    #[test]
    fn two_matching_cars_are_refused_not_chosen() {
        let cars = vec![
            car("11111111-aaa", "feat/x", "ready"),
            car("11111111-bbb", "feat/x", "ready"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
    }

    /// THE DEFECT (backlog 0a58827d). `boss prove` refused with "2 open
    /// cars match" when only one was open: the other had been abandoned
    /// ("gated, then changed"). A closed car cannot take a new proof, so
    /// it was never a candidate — and with the dead twin gone the case
    /// resolves to one car and needs no refusal at all. The refusal sent
    /// an operator to investigate a dead car.
    #[test]
    fn a_closed_twin_is_not_a_candidate_for_a_new_proof() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/arrival-runs-the-probe-rerail", "open"),
            car_at(
                "538775dd-bbb",
                "feat/arrival-runs-the-probe-rerail",
                "closed",
            ),
        ];
        let got = find_car(&cars, "feat/arrival-runs-the-probe-rerail", Eligible::Live)
            .expect("one live car is not ambiguous");
        assert_eq!(got.get("id").and_then(Value::as_str), Some("ef4eff3c-aaa"));
    }

    /// A cancelled car is as finished as a closed one — answered by
    /// `boss_jobs::car::is_open`, so this verb carries no second reading
    /// of `status` to drift from it.
    #[test]
    fn a_cancelled_twin_is_not_a_candidate_either() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "open"),
            car_at("538775dd-bbb", "feat/x", "cancelled"),
        ];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// And when the ONLY match is finished, the refusal says so. The
    /// bare "no open car — check your spelling" would send the operator
    /// hunting a typo that is not there.
    #[test]
    fn the_only_match_being_closed_is_said_out_loud() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("538775dd"), "names the car: {e}");
        assert!(e.contains("closed"), "names its status: {e}");
        assert!(
            e.contains("--recheck"),
            "names what a closed car can do: {e}"
        );
    }

    /// `--recheck` re-runs a probe already recorded on the car, which is
    /// a read — so a closed car IS its candidate, and that is why the
    /// reader pages over closed cars at all.
    #[test]
    fn a_recheck_can_still_name_a_closed_car() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        assert!(find_car(&cars, "feat/x", Eligible::AnyStatus).is_ok());
    }

    /// A listing is not homogeneous, so one collective word cannot
    /// describe it: two finished matches can be finished DIFFERENTLY.
    /// Each row carries its own status, which is what makes "say which"
    /// answerable without a second query — and is why the refusal no
    /// longer prefixes the count with a single "open" / "closed".
    #[test]
    fn a_refusal_labels_each_candidates_status() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "cancelled"),
            car_at("538775dd-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(
            !e.contains("cars match"),
            "nothing is a candidate, so nothing is ambiguous: {e}"
        );
        assert!(e.contains("ef4eff3c  cancelled"), "{e}");
        assert!(e.contains("538775dd  closed"), "{e}");
    }

    /// A row whose status is absent is NOT assumed finished. Dropping a
    /// candidate the code cannot see the state of would be guessing in
    /// the direction that loses cars.
    #[test]
    fn a_car_with_no_status_stays_a_candidate() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// The live match is PREFERRED for a recheck too — the behaviour
    /// that landed before this change and must not regress. A recheck
    /// admits a finished car, but when the name also matches a live one
    /// that is the one meant, so the abandoned twin does not turn a
    /// resolvable recheck into an ambiguity.
    #[test]
    fn a_recheck_prefers_the_live_car_over_an_abandoned_twin() {
        let mut abandoned = car("22222222-bbb", "feat/x", "ready");
        abandoned["status"] = json!("closed");
        abandoned["metadata"]["skip_reason"] =
            json!("gate receipt is for fb973bd0 but the branch boards 0ec4521f");
        let cars = vec![car("11111111-aaa", "feat/x", "ready"), abandoned];
        let found = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .expect("the one live car is the candidate");
        assert_eq!(
            found.get("id").and_then(Value::as_str),
            Some("11111111-aaa")
        );
    }

    /// And the refusal is not regressed: two cars that are genuinely
    /// open still refuse, and still name both so "say which" is
    /// answerable.
    #[test]
    fn two_genuinely_open_cars_still_refuse_and_list_both() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "open"),
            car_at("22222222-bbb", "feat/x", "open"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  open"), "{e}");
        assert!(e.contains("22222222  open"), "{e}");
    }

    /// And when the ambiguity is among finished cars only — which only a
    /// recheck can reach — no row is called open.
    #[test]
    fn an_ambiguity_among_closed_cars_is_not_called_open() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "closed"),
            car_at("22222222-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  closed"), "{e}");
        assert!(!e.contains("open"), "{e}");
    }

    #[test]
    fn a_car_is_found_by_branch_or_by_id_prefix() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
        assert!(find_car(&cars, "abcdef12", Eligible::Live).is_ok());
        // Too short to be an id, and not a branch: refused, not guessed.
        assert!(find_car(&cars, "abc", Eligible::Live).is_err());
    }

    #[test]
    fn a_long_stream_is_clipped_rather_than_pushed_into_metadata() {
        let clipped = clip(&"x".repeat(MAX_STREAM + 500));
        assert!(clipped.len() < MAX_STREAM + 100);
        assert!(clipped.contains("more bytes"));
    }

    #[test]
    fn a_recorded_proof_says_when_it_was_taken() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09.847213Z")
            .unwrap()
            .into();
        let md = proven_metadata("v", "{\"exit\":0}", Some("api"), now);

        assert_eq!(
            md["completed_at"], "2026-08-29T04:15:09Z",
            "proof with no timestamp cannot be aged, so a --recheck cannot \
             tell a proof taken minutes ago from one taken in June"
        );
        // The stamp must match the conductor's format byte for byte —
        // proof lag is this minus `review.completed_at`.
        assert_eq!(md["completed_at"], json!(crate::sor_client::stamp(now)));
        // And it does not disturb what the step already carried.
        assert_eq!(md["verified"], json!("v"));
        assert_eq!(md["proof"], json!("{\"exit\":0}"));
        assert_eq!(md["method"], json!("api"));
    }

    /// The refusal happens BEFORE the probe runs, and it reads the
    /// step's own declared vocabulary rather than a copy in this file
    /// (9a: it already lives in workflows.toml and the registry
    /// default). 2026-09-02: two proof probes ran green against
    /// production and were thrown away when the API refused a
    /// free-prose --method, because nothing checked it locally.
    #[test]
    fn a_bad_method_is_refused_against_the_steps_own_vocabulary() {
        let step = json!({"fields": [
            {"name": "method", "field_type": "browser|api|log", "required": false}
        ]});
        assert!(check_enum_field(&step, "method", "api").is_ok());
        let e = check_enum_field(&step, "method", "ran it live")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("browser | api | log"),
            "names the vocabulary: {e}"
        );
        assert!(
            e.contains("BEFORE the probe runs"),
            "says why it refuses early: {e}"
        );
    }

    /// A field the step does not declare, or one that is free text, is
    /// not this verb's business to constrain — it must not invent a
    /// contract the protocol never stated.
    #[test]
    fn an_undeclared_or_free_text_field_is_not_constrained() {
        let free = json!({"fields": [{"name": "note", "field_type": "string"}]});
        assert!(check_enum_field(&free, "note", "anything at all").is_ok());
        assert!(check_enum_field(&json!({}), "method", "whatever").is_ok());
    }

    /// A PROOF IS MERGED, THEN THE STEP FLIPS WITH A STATUS-ONLY BODY
    /// (backlog e39a9d2a, the car after the gate's refused verdicts).
    /// The `proven` step carries keys the registry materialized at
    /// admission (`authority_role`, `station`, `audience`, `claimable`,
    /// `metadata_defaults`); a PUT whose `metadata` is a fresh
    /// `{verified, proof, completed_at}` replaced them wholesale on
    /// every proof. The merge goes first because `verified` and `proof`
    /// are required at done, and the flip is where that is judged.
    #[test]
    fn a_proof_merges_its_keys_then_flips_the_status_alone() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T01:00:00Z")
            .unwrap()
            .into();
        let md = proven_metadata("v", "{\"exit\":0}", None, now);
        let writes = proven_writes("car-1", "s-proven", &md);
        assert_eq!(writes.len(), 2, "one merge, one flip: {writes:?}");

        let (method, path, body) = &writes[0];
        assert_eq!(*method, reqwest::Method::PATCH);
        assert_eq!(path, "/api/jobs/car-1/steps/s-proven/metadata");
        assert_eq!(body, &md, "the merge carries exactly the proof's keys");

        let (method, path, body) = &writes[1];
        assert_eq!(*method, reqwest::Method::PUT);
        assert_eq!(path, "/api/jobs/car-1/steps/s-proven");
        assert_eq!(
            body,
            &json!({"status": "completed"}),
            "the flip carries no metadata, so it can drop no stored key"
        );
    }

    /// Both doors that complete `proven` — the attended `boss prove` and
    /// the forge's `--unattended` — write through [`proven_writes`], so
    /// neither can drift back to a metadata-carrying PUT on its own. The
    /// text half of the pin above: a later edit that builds the body by
    /// hand again fails here by name.
    #[test]
    fn no_proof_completion_puts_metadata() {
        let src = include_str!("prove.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first piece");
        let banned = ["\"completed\", \"metadata\"", "\"completed\",\"metadata\""];
        for b in banned {
            assert!(
                !production.contains(b),
                "a proof completion PUTs a metadata body ({b}) — that replaces the \
                 step's stored keys wholesale; write through proven_writes (e39a9d2a)"
            );
        }
        assert_eq!(
            production.matches("proven_writes(").count(),
            3,
            "the definition and its two callers (attended, unattended)"
        );
    }

    #[test]
    fn an_omitted_method_stays_omitted() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09Z")
            .unwrap()
            .into();
        let md = proven_metadata("", "{}", None, now);
        assert!(md.get("method").is_none(), "an absent method is not `null`");
        assert!(md["completed_at"].is_string());
    }

    /// A COMPLETED STEP IS CLOSED TO A NEW PROOF, AND OPEN TO A BETTER
    /// ONE. Car 932aa956's probe observed a real production refusal and
    /// stopped holding within the hour because the condition was
    /// transient — `--recheck` said NO LONGER HOLDS, correctly, and was
    /// wrong about the cause. Without --replace the only route to a
    /// durable probe was an operator editing job metadata by hand.
    #[test]
    fn a_proven_step_reopens_only_for_a_replacement() {
        let done = car("11111111-a", "feat/x", "completed");
        assert!(
            proven_step(&done, false).is_err(),
            "an ordinary prove must not overwrite a recorded proof"
        );
        assert!(
            proven_step(&done, true).is_ok(),
            "--replace is how a transient proof gets a better probe"
        );
    }

    /// ...and the refusal now says how, rather than only saying no.
    #[test]
    fn the_refusal_names_replace_as_the_way_forward() {
        let e = proven_step(&car("11111111-a", "feat/x", "completed"), false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--recheck"), "{e}");
        assert!(e.contains("--replace"), "{e}");
    }

    /// A REPLACEMENT SUPERSEDES THE ORIGINAL FOR RE-RUNNING, and the
    /// original is still on the step — the same precedence
    /// `regate_receipt` has over a stale gate receipt. Which proof
    /// `--recheck` should re-run is a different question from which one
    /// happened, and both answers are kept.
    #[test]
    fn recheck_re_runs_the_newest_replacement() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let first = proof_json(
            "old-probe",
            None,
            &o,
            "h",
            "2026-08-29T00:00:00Z",
            &no_tree(),
            None,
        );
        let better = proof_json(
            "better-probe",
            None,
            &o,
            "h",
            "2026-08-30T00:00:00Z",
            &no_tree(),
            None,
        );
        let step = json!({"metadata": {"proof": first.to_string()}});
        let with_reproof = json!({"metadata": {"reproof": [
            {"proof": better.to_string(), "recorded_at": "2026-08-30T00:00:00Z"}
        ]}});
        let plain = json!({"metadata": {}});

        assert_eq!(
            recorded_probe_for(&plain, &step).expect("readable").probe,
            "old-probe",
            "with no replacement, the step's own proof is what re-runs"
        );
        assert_eq!(
            recorded_probe_for(&with_reproof, &step)
                .expect("readable")
                .probe,
            "better-probe",
            "a replacement is what --recheck should be re-running"
        );
    }

    /// THE FALSE NEGATIVE THAT GREW WITH THE PIPELINE'S AGE.
    ///
    /// The car read was one `?kind=ship-a-change&limit=200`. Once more
    /// than 200 closed cars had accumulated, a legitimately open,
    /// unproven car sorting after them fell off that single page, and
    /// `find_car` reported "no open ship-a-change car" for a car that
    /// plainly existed. Paging on `total` must reach it — and must keep
    /// reading closed cars too, because `--recheck` proves a CLOSED one,
    /// which is why the fix is not a `status=open` filter.
    ///
    /// The stub honours `limit`/`offset` and reports the true `total`,
    /// so the reader is exercised across page boundaries with no
    /// `BOSS_JOBS_URL` anywhere in the environment.
    /// A page with no `total` is refused, never read as the whole
    /// population (backlog 10776b6c). The read counted a missing total
    /// as 0, so one page of rows already "covered" it and the loop
    /// stopped there — a car past page one was reported as no car, the
    /// false negative the paging above exists to prevent.
    #[tokio::test]
    async fn a_car_page_without_a_total_is_refused_not_read_as_all() {
        let (base, stub) = crate::sor_client::stub::one_request(
            r#"{"data":[{"id":"00000000-0000-0000-0000-0000000000cc","status":"closed"}]}"#,
        )
        .await;
        let http = crate::sor_client::machine_client().unwrap();
        let why = crate::sor_client::all_cars_at(&http, &base)
            .await
            .expect_err("a page without its total cannot say the read is complete")
            .to_string();
        stub.abort();
        assert!(why.contains("total"), "{why}");
    }

    #[tokio::test]
    async fn a_car_past_the_first_page_is_still_reachable() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Enough closed cars to overflow more than one page, then one
        // OPEN car dead last — only a full paging read finds it.
        let needle = "fix/needle-in-the-haystack";
        let mut all: Vec<Value> = (0..1100)
            .map(|i| {
                json!({
                    "id": format!("{i:08}-0000-0000-0000-0000000000cc"),
                    "status": "closed",
                    "title": format!("closed car {i}"),
                    "metadata": {"branch": format!("fix/closed-{i}")},
                })
            })
            .collect();
        all.push(json!({
            "id": "ffffffff-0000-0000-0000-0000000000ff",
            "status": "open",
            "title": "the open one",
            "metadata": {"branch": needle},
        }));
        let total = all.len();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = all.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let param = |k: &str| -> Option<usize> {
                    target
                        .split(['?', '&'])
                        .find_map(|p| p.strip_prefix(&format!("{k}=")))
                        .and_then(|v| v.parse().ok())
                };
                let limit = param("limit").unwrap_or(100).max(1);
                let offset = param("offset").unwrap_or(0);
                let page: Vec<Value> = served.iter().skip(offset).take(limit).cloned().collect();
                let body = json!({"data": page, "total": served.len()}).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let http = crate::sor_client::machine_client().unwrap();
        let cars = crate::sor_client::all_cars_at(&http, &base)
            .await
            .expect("paging read succeeds");
        assert_eq!(
            cars.len(),
            total,
            "every page must be read, not just the first {}",
            cars.len()
        );
        let car = find_car(&cars, needle, Eligible::Live)
            .expect("the open car past page 1 must be found");
        assert_eq!(
            car.get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str),
            Some(needle)
        );
    }

    // ------------------------------------------------------------------
    // ONE RULE SET, BOTH DOORS (backlog 23b2dffa). `boss gate
    // --park-probe` refuses a probe naming a tool the forge lacks, and
    // one reading the system of record unidentified. This verb — the
    // HAND door onto the same recorded probe — applied neither, and
    // said so only in a comment describing the gate's refusal.
    // ------------------------------------------------------------------

    /// THE RULE THAT HOLDS ON EVERY HOST. An unidentified read of the
    /// system of record is answered with a NARROWER WORLD, silently, so
    /// an absence assertion passes falsely — measured twice in one hour
    /// on 2026-09-10 (61085a9e), once on a probe that would have
    /// recorded a green proof of nothing. That is true wherever the
    /// probe runs, so the hand verb refuses it too, and names the way
    /// out rather than leaving the operator to argue with a wall.
    #[test]
    fn a_probe_that_reads_the_sor_unidentified_is_refused_by_the_hand_verb() {
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=pr-train&status=open | grep -q b9302f90",
            "curl -sf http://boss-jobs-internal.boss.svc.cluster.local:7900/api/yard/status | grep -q trains",
            "wget -qO- http://10.20.0.34:7900/api/stations/loading-dock/queue | grep -q x",
        ] {
            let a = admit(probe, false);
            let r = a
                .refusal
                .unwrap_or_else(|| panic!("admitted: {probe}"))
                .text;
            assert!(r.contains("unidentified"), "{r}");
            assert!(
                r.contains(OVERRIDE_FLAG),
                "the refusal must name its override: {r}"
            );
        }
    }

    /// The same refusal on the path that runs the probe the CAR
    /// recorded: a car's `proof_probe` can be written by hand with a
    /// metadata PATCH, so it never had to meet a gate.
    #[test]
    fn a_car_carried_probe_that_reads_the_sor_unidentified_is_refused_too() {
        let a = admit(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | grep -q dock_depth",
            true,
        );
        assert!(
            a.refusal.is_some(),
            "a car-carried probe gets the same rule"
        );
    }

    /// A GIT DATE READ WITH ITS OFFSET IS REFUSED HERE TOO (c0ac92b8):
    /// the string compare it feeds lies in both directions, so this
    /// door — hand-run or `--from-car` — says what the gate says, from
    /// the one evidence text, and names its OWN rule id so an override
    /// is recorded against the rule that refused, not the other one.
    #[test]
    fn a_probe_that_reads_a_git_date_with_an_offset_is_refused_by_the_hand_verb() {
        let probe = "since=$(git log -1 --format=%cI HEAD); \
                     last=$(boss-api GET /api/audit | jq -r '.data[0].at'); \
                     [ \"$last\" \\> \"$since\" ] && echo retire:after-landing";
        for from_car in [false, true] {
            let a = admit(probe, from_car);
            let r = a
                .refusal
                .unwrap_or_else(|| panic!("admitted (from_car={from_car}): {probe}"));
            assert_eq!(r.rule, boss_jobs::probe::GIT_TIME_RULE);
            assert!(r.text.contains("`%cI`"), "{}", r.text);
            assert!(r.text.contains("--format=%ct"), "{}", r.text);
            assert!(
                r.text.contains(boss_jobs::probe::GIT_TIME_STRING_EVIDENCE),
                "{}",
                r.text
            );
            assert!(r.text.contains(OVERRIDE_FLAG), "{}", r.text);
        }
        // And the unidentified read still records ITS rule.
        let a = admit(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | grep -q x",
            false,
        );
        assert_eq!(a.refusal.map(|r| r.rule), Some(UNIDENTIFIED_RULE));
    }

    /// AND THE RULE THAT DOES NOT. `host-absent-tools.txt` says what the
    /// FORGE lacks, because a `--park-probe` runs there. This verb runs
    /// the probe HERE, in front of the operator, one second later — and
    /// a pod-local `kubectl` proof is the established shape for a claim
    /// only the cluster can show, so refusing it would block exactly the
    /// case where a human has to prove what the forge cannot.
    #[test]
    fn the_hand_verb_does_not_judge_a_hand_probe_against_the_forges_tools() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            false,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    }

    /// But a probe that came from the CAR has a second destination — the
    /// arrival rule runs that same text on the forge — so the mismatch
    /// is named. Named, not refused: proving it by hand here is the
    /// right move, and the car's recorded probe is what needs fixing.
    #[test]
    fn a_car_carried_probe_the_forge_cannot_run_is_named_but_not_refused() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            true,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a
            .warnings
            .first()
            .expect("the forge cannot run this car's probe");
        assert!(w.contains("kubectl"), "{w}");
        assert!(w.contains("forge"), "{w}");
    }

    /// A car-carried probe that ASSIGNS AN ACTOR is named at this door
    /// too (8a1fcd22): the forge will run that text, and an actor is the
    /// one thing that would let its `boss` verbs write. Named, not
    /// refused, for the same reason as the absent tool above — the hand
    /// run here is the operator's, already signed as them; what needs
    /// fixing is the probe the car recorded, which `boss gate` refuses.
    #[test]
    fn a_car_carried_probe_that_names_an_actor_is_named_but_not_refused() {
        let a = admit(
            "BOSS_ACTOR=emp-david boss job close 1234 && echo closed",
            true,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a.warnings.first().expect("the actor assignment is named");
        assert!(w.contains("BOSS_ACTOR"), "{w}");
        assert!(w.contains("does not act"), "{w}");
        let hand = admit(
            "BOSS_ACTOR=emp-david boss job close 1234 && echo closed",
            false,
        );
        assert!(hand.warnings.is_empty(), "{:?}", hand.warnings);
    }

    /// A car-carried probe that `cd`s is named here too (4bb6797c): on
    /// this pod `cd /work/boss` works, so the hand run is fine and is
    /// not refused — but the forge has no such path, so the recorded
    /// text is what needs re-parking, and `boss gate` refuses it. A
    /// probe typed by hand (`--probe`) is the operator's own shell and
    /// is not this rule's business.
    #[test]
    fn a_car_carried_probe_that_changes_directory_is_named_but_not_refused() {
        let probe = "cd /work/boss && git show HEAD:x | grep -q y && echo ok";
        let a = admit(probe, true);
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a.warnings.first().expect("the cd is named");
        assert!(w.contains("`cd`"), "{w}");
        assert!(w.contains("converged checkout"), "{w}");
        assert!(admit(probe, false).warnings.is_empty());
    }

    /// A PROBE THAT READS THE CALLER'S OWN QUEUE (backlog 371b0443). A
    /// probe typed at this door is recorded and re-run later by somebody
    /// else, so it is refused like the gate refuses it, with the rule an
    /// override records. A car's recorded probe has already landed, so
    /// it is named rather than refused — the same split as the `cd`
    /// rule above — and the attempt record says who ran it.
    #[test]
    fn a_probe_that_reads_the_callers_own_queue_is_refused_by_hand_and_named_from_a_car() {
        let probe = "boss orient | grep -A3 'MY WORK' | grep -c \"urgent first, then oldest\"";
        let hand = admit(probe, false);
        let r = hand.refusal.expect("refused at the hand door");
        assert_eq!(r.rule, boss_jobs::probe::CALLERS_OWN_QUEUE_RULE);
        assert!(r.text.contains("MY WORK"), "{r}");
        assert!(r.text.contains(OVERRIDE_FLAG), "{r}");
        assert!(
            r.text
                .contains(boss_jobs::probe::CALLERS_OWN_QUEUE_EVIDENCE),
            "{r}"
        );
        let car = admit(probe, true);
        assert!(car.refusal.is_none(), "{:?}", car.refusal);
        let w = car
            .warnings
            .iter()
            .find(|w| w.contains("MY WORK"))
            .expect("the queue read is named");
        assert!(w.contains("boss gate --park-probe"), "{w}");
    }

    /// A TREE READ UNDER A PRODUCTION CLAIM (backlog 490acf06). Typed at
    /// this door with `--verified` naming an endpoint answer or a latency,
    /// a probe that only reads the tree is refused like the gate refuses
    /// it, with the rule an override records. A car's recorded probe has
    /// landed and cannot be re-parked, so it is named rather than
    /// refused. A source claim over the same probe, and an observing
    /// probe under the production claim, are both clean.
    #[test]
    fn a_tree_probe_under_a_production_claim_is_refused_by_hand_and_named_from_a_car() {
        let probe = "git show HEAD:crates/core/boss-events/src/history_snapshot.rs | \
                     grep -c 'sqlx::query_scalar(RELEVANT_COUNT_SQL)'";
        let claim = "GET /api/stations/flow with the default basis answers in seconds.";
        let hand = admit_against_claim(probe, claim, false);
        let r = hand.refusal.expect("refused at the hand door");
        assert_eq!(r.rule, boss_jobs::probe::PRODUCTION_CLAIM_RULE);
        assert!(r.text.contains("/api/stations/flow"), "{r}");
        assert!(r.text.contains(OVERRIDE_FLAG), "{r}");
        assert!(
            r.text.contains(boss_jobs::probe::PRODUCTION_CLAIM_EVIDENCE),
            "{r}"
        );
        let car = admit_against_claim(probe, claim, true);
        assert!(car.refusal.is_none(), "{:?}", car.refusal);
        let w = car
            .warnings
            .iter()
            .find(|w| w.contains("/api/stations/flow"))
            .expect("the unobserved claim is named");
        assert!(w.contains("NOT OBSERVED"), "{w}");
        let source = admit_against_claim(probe, "history_snapshot.rs counts first.", false);
        assert!(source.refusal.is_none(), "{:?}", source.refusal);
        let observed = admit_against_claim(
            "boss-sor-read /api/stations/flow | jq -e '.stations | length > 0' >/dev/null \
             && echo flow:ok",
            claim,
            false,
        );
        assert!(
            observed
                .refusal
                .as_ref()
                .is_none_or(|r| r.rule != boss_jobs::probe::PRODUCTION_CLAIM_RULE),
            "{:?}",
            observed.refusal
        );
        // An earlier rule's refusal is kept: the claim rule only speaks
        // when nothing before it did.
        let own_queue = admit_against_claim(
            "boss orient | grep -A3 'MY WORK' | grep -c ordered",
            "MY WORK answers in 20 s",
            false,
        );
        assert_eq!(
            own_queue.refusal.map(|r| r.rule),
            Some(boss_jobs::probe::CALLERS_OWN_QUEUE_RULE)
        );
    }

    /// The registration door records a probe a landed car will be proven
    /// by, unattended, so it refuses the tree-read-under-production-claim
    /// pair outright: nobody is standing there to override it.
    #[test]
    fn registering_a_tree_probe_under_a_production_claim_is_refused() {
        let car = json!({"id":"c0ffee12", "kind":"ship-a-change", "status":"open",
            "metadata":{"merged":"true"},
            "steps":[{"title":PROVEN,"status":"ready"}]});
        let probe =
            "git show HEAD:crates/core/boss-jobs/src/http/yard.rs | grep -c 'fn list_every('";
        let e = probe_registration_patch(
            &car,
            probe,
            "1",
            "GET /api/yard/status lists every open pr-train",
            "actor",
            "time",
            "registration-1",
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("/api/yard/status"), "{e}");
        assert!(e.contains(boss_jobs::probe::PRODUCTION_CLAIM_RULE), "{e}");
        assert!(
            probe_registration_patch(
                &car,
                probe,
                "1",
                "yard.rs pages the open trains with list_every",
                "actor",
                "time",
                "registration-1",
            )
            .is_ok()
        );
    }

    /// A named read is not this rule's business, and a MENTION is not a
    /// read — the shared command-position scan is what keeps both legal.
    #[test]
    fn named_reads_and_mere_mentions_are_admitted() {
        for probe in [
            "boss-sor-read /api/yard/status | grep -q dock_depth",
            "boss-api GET /api/jobs?kind=pr-train | grep -q arrived",
            "grep -c BOSS_JOBS_URL infra/ops/ops-runner.sh",
            "test -n \"$BOSS_JOBS_URL\" && echo claim-ok",
        ] {
            let a = admit(probe, true);
            assert!(a.refusal.is_none(), "{probe}: {:?}", a.refusal);
        }
    }

    // -----------------------------------------------------------------
    // THE PROBE THAT COULD NOT PASS (backlog 4fccc595)
    // -----------------------------------------------------------------

    /// jq is a declared required tool of the CI image
    /// (infra/forge/boss-ci/required-tools.txt), and this section is
    /// about jq's own exit codes — so its absence is a failure, never a
    /// skip. A suite that passes by skipping is the check-nobody-reads
    /// defect with extra steps.
    fn require_jq() {
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v jq >/dev/null")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            ok,
            "jq is missing and this test would otherwise pass by skipping — \
             jq is declared in infra/forge/boss-ci/required-tools.txt"
        );
    }

    /// The shape off car a0ab90a5, reduced to one claim so the test needs
    /// no network: `if <claim> then empty else error(…) end` under
    /// `jq -e`. The claim HOLDS in this fixture.
    const INVERTED_FILTER: &str = "printf '%s' '{\"category\":\"platform\"}' | \
         jq -e 'if (.category==\"platform\") then empty else error(\"category=\\(.category)\") end'";

    /// THE ANCHOR (4fccc595). Eighteen hours were spent on a probe that
    /// reported failure precisely when its claim held, and the record
    /// said "not holding, or the probe is wrong" over an empty output and
    /// a rewritten exit code. Three things must now be true of that
    /// exact shape: the door NAMES it, the recorded attempt carries jq's
    /// OWN exit status, and the verdict names ONE thing.
    #[test]
    fn the_inverted_jq_e_probe_is_named_and_its_real_exit_is_recorded() {
        require_jq();

        // 1. The door says so, before anything runs — and warns rather
        //    than refuses, because this shape cannot record a false
        //    proof (see `admit`).
        let a = admit(INVERTED_FILTER, false);
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a
            .warnings
            .iter()
            .find(|w| w.contains("jq -e"))
            .unwrap_or_else(|| panic!("the inverted shape is not named: {:?}", a.warnings));
        assert!(w.contains("empty"), "{w}");
        assert!(w.contains("4"), "the warning must name jq's exit 4: {w}");

        // 2. THE CLAIM HOLDS AND THE PROBE FAILS, with jq's own 4 —
        //    the number that identifies the defect. Recorded verbatim.
        let o = execute(INVERTED_FILTER).unwrap();
        assert_eq!(
            o.exit, 4,
            "jq -e exits 4 when its filter produces no output; `empty` produces none"
        );
        let p = proof_json(
            INVERTED_FILTER,
            Some("claim:ok"),
            &o,
            "h",
            "now",
            &no_tree(),
            None,
        );
        assert_eq!(p["exit"], 4, "the proof record carries the REAL status");
        assert!(
            p.get("stderr").is_some(),
            "stderr is recorded as its own field"
        );

        // 3. And the verdict names the self-contradiction instead of
        //    offering the reader two possibilities.
        let e = judge_probe(INVERTED_FILTER, &o, Some("claim:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("exited 4"), "{e}");
        assert!(
            e.contains("SELF-CONTRADICTORY"),
            "the verdict must name what failed: {e}"
        );
        assert!(
            e.contains("4fccc595"),
            "and carry the measured evidence: {e}"
        );
    }

    /// THE OTHER HALF OF THE SAME PROBE: jq's stderr, which exists the
    /// moment the claim actually fails. `error(…)` prints to stderr and
    /// exits 5 — a DIFFERENT number from the 4 above, which is the whole
    /// reason a rewritten exit code costs a diagnosis.
    #[test]
    fn jq_names_the_failing_row_on_stderr_and_exits_five() {
        require_jq();
        let failing = "printf '%s' '{\"category\":\"a sentence\"}' | \
             jq -e 'if (.category==\"platform\") then empty else \
             error(\"maintenance-backup: category=\\(.category)\") end'";
        let o = execute(failing).unwrap();
        assert_eq!(o.exit, 5, "jq exits 5 when the filter calls error()");
        assert!(
            o.stderr.contains("maintenance-backup: category=a sentence"),
            "jq's own diagnosis, captured: {:?}",
            o.stderr
        );
        let p = proof_json(failing, Some("claim:ok"), &o, "h", "now", &no_tree(), None);
        assert!(
            p["stderr"].as_str().unwrap().contains("maintenance-backup"),
            "and recorded: {p}"
        );
    }

    /// `|| exit 1` IS WHY THE NUMBER WAS GONE. The same filter with the
    /// trailing bare exit records a 1, and 4 and 5 — two different
    /// causes — become the same digit. The diagnosis says so, naming the
    /// reduction rather than leaving a reader to find it.
    #[test]
    fn a_bare_exit_rewrite_is_named_as_the_reason_the_status_is_gone() {
        require_jq();
        let rewritten = format!("{INVERTED_FILTER} || exit 1");
        let o = execute(&rewritten).unwrap();
        assert_eq!(o.exit, 1, "jq's 4 is gone — this is the measured record");
        let d = failure_diagnosis(&rewritten, &o).expect("a failure this shape is diagnosable");
        assert!(d.contains("SELF-CONTRADICTORY"), "{d}");
        assert!(
            d.contains("|| exit 1"),
            "the reduction that erased jq's status must be named: {d}"
        );
        let a = admit(&rewritten, false);
        assert!(
            a.warnings.iter().any(|w| w.contains("|| exit")),
            "the door names it too: {:?}",
            a.warnings
        );
    }

    /// A FAILURE THAT CANNOT BE READ is its own finding. Exit code, both
    /// streams empty, nothing to go on — the verdict must say THAT,
    /// rather than asserting which of several causes it was.
    #[test]
    fn a_failure_with_no_output_at_all_is_named_as_unreadable() {
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let d = failure_diagnosis("some-command --quiet", &o).expect("silence is diagnosable");
        assert!(
            d.contains("printed NOTHING"),
            "the reader must be told the record is empty, not left to infer it: {d}"
        );
        assert!(
            d.contains("not a verdict on the claim"),
            "an unreadable failure is not evidence against the claim: {d}"
        );
    }

    /// And an honest failure — a probe that ran, disagreed, and SAID so —
    /// gets no invented diagnosis. A verdict that speculates on every
    /// failure is noise, and noise is what gets ignored.
    #[test]
    fn a_probe_that_explains_its_own_failure_is_left_alone() {
        let o = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS for maintenance-backup (jq exit 5)".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert!(
            failure_diagnosis(
                "boss-sor-read /api/x | grep -q y || { echo x; exit 1; }",
                &o
            )
            .is_none(),
            "the probe already named what failed"
        );
        // Nor on a pass: `judge_probe` adds nothing when there is
        // nothing to explain.
        assert!(judge_probe("true", &ok("claim:ok"), Some("claim:ok")).is_ok());
    }

    // -----------------------------------------------------------------
    // A PROBE THAT CRASHED IS NOT A PROBE THAT SAID NOT-YET (68081368)
    // -----------------------------------------------------------------

    /// THE MEASURED RECORD, twice (cars dd1d872d and 2e4d3bce,
    /// 2026-09-14): a probe read a count with jq, jq printed `null`,
    /// bash's `[` refused to compare it and exited 2, and the `||`
    /// branch behind it said "not yet" and exited 75. The verdict
    /// recorded "said the claim cannot be judged until something
    /// happens" — the not-yet sentence, over stderr that held the crash.
    /// The reader had to open stderr to learn the probe had never
    /// judged anything.
    const CRASHED_STDERR: &str = "bash: line 10: [: null: integer expression expected\n";
    const NULL_COUNT_PROBE: &str = "n=$(boss-sor-read /api/jobs?kind=x | jq -r .total); \
         if [ \"$n\" -ge 1 ]; then echo x:ok; else echo 'not yet: none'; exit 75; fi";

    fn crashed(exit: i32, stderr: &str) -> Outcome {
        Outcome {
            exit,
            stdout: "not yet: none\n".into(),
            stderr: stderr.into(),
            missing_tools: Vec::new(),
            timing: None,
        }
    }

    #[test]
    fn a_probe_that_crashed_comparing_a_non_number_has_the_crash_named() {
        let o = crashed(75, CRASHED_STDERR);
        let d = failure_diagnosis(NULL_COUNT_PROBE, &o).expect("a crash is diagnosable");
        assert!(
            d.contains("THE PROBE CRASHED"),
            "the crash comes first: {d}"
        );
        assert!(
            d.contains("[: null: integer expression expected"),
            "the stderr line is quoted, not pointed at: {d}"
        );
        assert!(
            d.contains("// empty") && d.contains("*[!0-9]*"),
            "and the guard to add is named: {d}"
        );
        assert!(
            !d.contains("cannot be judged"),
            "a crash is not the not-yet sentence: {d}"
        );
        // The door's refusal carries it too — `judge_probe` is the one
        // path every probe run goes through.
        let e = judge_probe(NULL_COUNT_PROBE, &o, Some("x:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
    }

    /// THE MEASURED c4c1ac17 STDERR (backlog 302bc2f2). Its stored probe
    /// carried backslash-escaped quotes, so under sh `\"boss` was the
    /// PATTERN and `step` and `complete"` were FILENAMES. grep warned
    /// about both and exited 2, the probe's own `case` arm caught the
    /// non-number and exited 75, and the runner read 75 as an honest
    /// wait. `recheck-failing-probes-hourly` then re-ran a probe that
    /// could never pass, hourly, each run producing the same reassuring
    /// sentence. Nothing was ever judged about the claim — which was, as
    /// it happens, already true on main.
    const MANGLED_ARGS_STDERR: &str = "\
ugrep: warning: step: No such file or directory\n\
ugrep: warning: complete\": No such file or directory\n";

    #[test]
    fn a_probe_whose_quoting_was_mangled_is_not_read_as_an_honest_wait() {
        // Exit 75 is the probe's own not-yet code, and that is the whole
        // trap: this must NOT come back as NotYet.
        let o = crashed(75, MANGLED_ARGS_STDERR);
        let d = failure_diagnosis(NULL_COUNT_PROBE, &o)
            .expect("a mangled argument list is diagnosable");
        assert!(d.contains("QUOTING"), "the cause is named: {d}");
        assert!(
            d.contains("complete\": No such file or directory"),
            "the stderr line is quoted, not pointed at: {d}"
        );
        assert!(!d.contains("not yet"), "a broken probe is not a wait: {d}");
        // The verdict is the thing that mattered: 75 with a diagnosis is
        // NotProven, so the shed renders it troubled instead of waiting.
        assert!(
            matches!(
                verdict(NULL_COUNT_PROBE, &o, Some("x:ok")),
                Verdict::NotProven(_)
            ),
            "exit 75 with mangled arguments must not read as NotYet"
        );
    }

    /// Every phrasing a tool uses for a missing operand is the same
    /// finding, not only the one measured — a fourth is a line in
    /// `MISSING_OPERAND_MARKERS`, and this walks the list so the list is
    /// what gets tested, at every exit code a `||` branch could turn it
    /// into. The quote in the name is what makes each one damage rather
    /// than a wait, so each marker is checked both ways.
    #[test]
    fn every_missing_operand_marker_is_damage_only_when_the_name_carries_a_quote() {
        for marker in MISSING_OPERAND_MARKERS {
            for exit in [1, 2, 75] {
                let mangled = format!("grep: complete\": {marker}\n");
                let d = failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &mangled))
                    .unwrap_or_else(|| panic!("{marker:?} at exit {exit} is mangled quoting"));
                assert!(d.contains("QUOTING"), "{marker:?}: {d}");
                let honest = format!("grep: newfile.txt: {marker}\n");
                assert!(
                    failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &honest)).is_none(),
                    "{marker:?} without a quote in the name is an honest wait"
                );
            }
        }
    }

    /// The rule is narrow ON PURPOSE. A probe that greps a file which is
    /// not on main yet is an HONEST not-yet, and its stderr says "No such
    /// file or directory" too. What is never honest is a missing file
    /// whose NAME carries a double quote: no probe greps such a file, so
    /// the quote is the residue of an extra escaping layer.
    #[test]
    fn a_plainly_missing_file_is_still_an_honest_wait() {
        let o = crashed(75, "grep: newfile.txt: No such file or directory\n");
        assert!(
            failure_diagnosis(NULL_COUNT_PROBE, &o).is_none(),
            "a file that has simply not landed yet is not a mangled probe"
        );
        assert!(
            matches!(
                verdict(NULL_COUNT_PROBE, &o, Some("x:ok")),
                Verdict::NotYet { .. }
            ),
            "and it still reads as the honest wait it is"
        );
    }

    /// Every message bash's `[` and `((` print for a non-number is a
    /// crash, not only the one measured. A fourth one is a line in
    /// `NUMERIC_CRASH_MARKERS`, and this test walks the list so the
    /// list is what gets tested — at every exit code a `||` branch
    /// could turn the crash into.
    #[test]
    fn every_numeric_crash_marker_is_named_whatever_the_exit_code() {
        for marker in NUMERIC_CRASH_MARKERS {
            let stderr = format!("bash: line 3: [: : {marker}\n");
            for exit in [1, 2, 75] {
                let d = failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &stderr))
                    .unwrap_or_else(|| panic!("{marker:?} at exit {exit} is a crash"));
                assert!(d.contains("THE PROBE CRASHED"), "{marker}: {d}");
                assert!(d.contains(marker), "the line is quoted: {d}");
            }
        }
    }

    /// A CLEAN NOT-YET IS LEFT ALONE BY THE DIAGNOSER. Exit 75, a "not
    /// yet:" line, an empty stderr — the probe said what it meant, and
    /// `failure_diagnosis` invents nothing for it; `verdict` reads it as
    /// NOT YET precisely BECAUSE there is no diagnosis (726562de), the
    /// same order the forge's verdict block uses. Nor does an ordinary
    /// failure — a curl refusal on stderr — read as a crash.
    #[test]
    fn a_clean_exit_75_gets_no_crash_diagnosis() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: "curl: (7) Failed to connect\n".into(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
    }

    // -----------------------------------------------------------------
    // --replace WITH NOTHING TO REPLACE (backlog 251dba77)
    // -----------------------------------------------------------------

    /// THE MEASURED DEFECT. `boss prove <car> --replace` on a car whose
    /// `proven` step had never been proven printed "re-proven — recorded
    /// as reproof #1. The original proof is untouched on the step" —
    /// two assertions, both false — and left the step `ready`, so the
    /// car never advanced and nothing said so.
    #[test]
    fn replace_refuses_when_there_is_no_proof_to_replace() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"authority_role": "platform-admin", "procedure": "boss prove"}
        }]});
        let e = proven_step(&car, true).unwrap_err().to_string();
        assert!(
            e.contains("no recorded proof"),
            "it must say what is missing: {e}"
        );
        assert!(e.contains("ready"), "and that the step has not moved: {e}");
        assert!(
            e.contains("WITHOUT --replace"),
            "and name the plain form, which is what records a FIRST proof: {e}"
        );
        // The plain form is unaffected: this is the path that records it.
        assert!(proven_step(&car, false).is_ok());
    }

    /// AND THE FLAG STILL DOES WHAT IT WAS BUILT FOR (2b30eff4): a
    /// completed step's proof is frozen, and `--replace` is how a better
    /// probe lands beside it without erasing what used to hold.
    #[test]
    fn replace_still_works_on_a_step_whose_proof_is_frozen() {
        let proven = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "completed",
            "metadata": {"verified": "it held", "proof": "{\"exit\":0}"}
        }]});
        assert!(
            proven_step(&proven, true).is_ok(),
            "a completed step is exactly what --replace is for"
        );
        assert!(
            proven_step(&proven, false)
                .unwrap_err()
                .to_string()
                .contains("--replace"),
            "and the plain form still points at it"
        );
    }

    /// A step that is OPEN but already carries a proof is the one
    /// non-obvious case: the plain form would overwrite the recorded
    /// proof, which is the erasure 2b30eff4 exists to prevent — so
    /// `--replace` is accepted there and appends instead.
    #[test]
    fn replace_is_accepted_on_an_open_step_that_does_carry_a_proof() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"proof": "{\"exit\":0,\"probe\":\"true\"}"}
        }]});
        assert!(proven_step(&car, true).is_ok());
    }

    /// AN OVERRIDE NOBODY CAN SEE IS THE SAME DEFECT AGAIN. The escape
    /// hatch is recorded IN the proof — the one record `--recheck` and
    /// every later reader already open — and absent entirely when it was
    /// not used, so its presence means something.
    #[test]
    fn an_overridden_refusal_is_recorded_in_the_proof() {
        let ov = override_record(
            UNIDENTIFIED_RULE,
            "the identity header comes from my shell profile",
        );
        let p = proof_json(
            "curl $BOSS_JOBS_URL/api/x",
            Some("x"),
            &ok("x"),
            "h",
            "now",
            &no_tree(),
            Some(&ov),
        );
        assert_eq!(p["overridden"]["rule"], UNIDENTIFIED_RULE);
        assert_eq!(
            p["overridden"]["reason"],
            "the identity header comes from my shell profile"
        );
        let plain = proof_json("true", Some("x"), &ok("x"), "h", "now", &no_tree(), None);
        assert!(
            plain.get("overridden").is_none(),
            "no override, no key — a reader must not read one to learn nothing"
        );
    }

    // -----------------------------------------------------------------
    // THE HAND DOOR CAN SAY NOT YET (backlog 726562de)
    // -----------------------------------------------------------------

    /// THE MEASURED ASYMMETRY. The forge runner records a clean exit 75
    /// as NOT YET — the probe said the claim cannot be judged until
    /// something happens — leaves `proven` ready and re-runs it daily.
    /// `boss prove <car> --from-car` on the SAME run read "A probe that
    /// fails is evidence AGAINST the claim": the opposite verdict for
    /// one record, depending on which door ran it. Exit 75 is a third
    /// answer, and this door now gives it.
    #[test]
    fn an_exit_75_with_a_not_yet_line_is_not_yet_not_evidence_against() {
        let o = Outcome {
            exit: NOT_YET_EXIT,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let Verdict::NotYet { said } = verdict(NULL_COUNT_PROBE, &o, Some("sweep-measured:ok"))
        else {
            panic!("a clean exit 75 is NOT YET, not a refusal");
        };
        assert_eq!(
            said,
            "not yet: no disk-report request carrying for_sweep yet"
        );
        let why = not_yet_why("pod", &said);
        assert!(why.starts_with("NOT YET"), "{why}");
        assert!(
            why.contains(&said),
            "the probe's own line is the reason: {why}"
        );
        assert!(
            !why.contains("evidence AGAINST") && !why.contains("not proof of anything"),
            "not-yet is not a verdict against the change: {why}"
        );
        assert_eq!(
            NOT_YET_EXIT, 75,
            "the verb exits as the probe did, so scripts can tell"
        );
    }

    /// A CRASH THAT EXITED 75 IS THE CRASH (68081368), at this door as
    /// at the forge: the code is whichever `||` branch caught it, and
    /// re-running a crash daily judges nothing forever.
    #[test]
    fn an_exit_75_whose_stderr_shows_a_crash_is_the_crash_not_not_yet() {
        let o = crashed(75, CRASHED_STDERR);
        let Verdict::NotProven(e) = verdict(NULL_COUNT_PROBE, &o, Some("x:ok")) else {
            panic!("a crashed numeric test is not not-yet");
        };
        let e = e.to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
        assert!(!e.contains("cannot be judged"), "{e}");
    }

    /// AND AN EXIT 75 THAT SAID NOTHING CANNOT BE READ — the forge's
    /// `why` checks the silent nonzero exit before it reads the code,
    /// and this door keeps the same order: not-yet is what a probe SAYS,
    /// not a number it exits with.
    #[test]
    fn an_exit_75_that_said_nothing_cannot_be_read() {
        let o = Outcome {
            exit: 75,
            stdout: String::new(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let Verdict::NotProven(e) = verdict("some-command --quiet", &o, Some("x:ok")) else {
            panic!("silence is a missing record, not a not-yet");
        };
        assert!(e.to_string().contains("THE FAILURE CANNOT BE READ"), "{e}");
    }

    /// The other two readings are unchanged by the third.
    #[test]
    fn the_other_two_readings_are_unchanged() {
        assert!(matches!(
            verdict("true", &ok("x:ok"), Some("x:ok")),
            Verdict::Proven
        ));
        let red = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS (jq exit 5)\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let Verdict::NotProven(e) = verdict("true", &red, Some("x:ok")) else {
            panic!("exit 1 is still a refusal");
        };
        assert!(e.to_string().contains("evidence AGAINST"), "{e}");
    }

    /// WHAT THE PROBE SAID, read as the forge reads it: the first
    /// non-empty line of stderr, else of stdout — stderr first because
    /// that is where a tool puts its diagnosis.
    #[test]
    fn what_it_said_reads_stderr_first_then_stdout() {
        let o = Outcome {
            exit: 75,
            stdout: "\n  not yet: from stdout\nmore\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("not yet: from stdout"));
        let o = Outcome {
            exit: 75,
            stdout: "not yet: from stdout\n".into(),
            stderr: "\ncurl: (7) refused\n".into(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("curl: (7) refused"));
        let o = Outcome {
            exit: 75,
            stdout: "  \n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        assert_eq!(what_it_said(&o), None);
    }

    // -----------------------------------------------------------------
    // THE UNRUNNABLE PRELUDE, AT THIS DOOR (backlog 46f67333)
    // -----------------------------------------------------------------

    /// THE MEASURED ASYMMETRY. The forge runner runs every probe behind
    /// a `command_not_found_handle` writing to fd 9 and records DID NOT
    /// RUN with the tool named; this door ran `sh -c` and read the same
    /// run as CANNOT BE READ or NOT PROVEN — a verdict about the
    /// operator's machine recorded as a verdict about the claim. Now the
    /// probe runs behind the forge's own prelude here too, so a tool the
    /// PATH lacks is a finding on the record, not a red.
    /// THE MANGLED-QUOTING CASE (backlog 302bc2f2, measured 2026-09-20).
    /// Two shed cars carried a probe stored with LITERAL backslash-quotes,
    /// so `grep -c \"boss step complete\"` made `step` and `complete\"`
    /// FILENAMES. grep warned, the count came back non-numeric, the
    /// `case` arm caught it, and the probe exited 75 with a sentence that
    /// read exactly like an honest wait — hourly, for days, while both
    /// claims were already TRUE on main.
    #[test]
    fn a_probe_whose_quoting_was_mangled_is_not_a_wait() {
        let probe = concat!(
            r#"n=$(echo hi | grep -c \"pub enum StepAction\" || true); "#,
            r#"case ${n:-empty} in empty|*[!0-9]*) echo \"not yet: cannot read the file\"; "#,
            "exit 75;; esac; echo ok"
        );
        let o = execute(probe).unwrap();
        assert_eq!(o.exit, NOT_YET_EXIT, "the case arm catches it: {o:?}");

        let d = failure_diagnosis(probe, &o)
            .expect("a probe the shell could not read as written has no verdict to give");
        assert!(d.starts_with("THE PROBE'S QUOTING WAS MANGLED"), "{d}");
        assert!(
            d.contains("--park-probe-file") || d.contains("single-quote"),
            "the diagnosis names the repair: {d}"
        );

        // …and because a diagnosis exists, the not-yet arm cannot claim it.
        assert!(
            !matches!(verdict(probe, &o, Some("ok")), Verdict::NotYet { .. }),
            "a mangled probe read as an honest wait is the whole defect: {o:?}"
        );
    }

    /// THE SHED, AS IT ACTUALLY READ (backlog 302bc2f2, 2026-09-20).
    /// These are the not-yet lines the thirteen cars awaiting proof
    /// really recorded. Eleven are honest waits and ELEVEN OF THEM
    /// carry a `\"` somewhere in their probe — inside a jq filter,
    /// where it belongs — which is exactly why "the probe text contains
    /// an escape" is the wrong detector and was my first, wrong count.
    /// Not one of them may be diagnosed.
    #[test]
    fn the_honest_waits_the_shed_really_recorded_are_not_diagnosed() {
        for said in [
            "not yet: the newest run ed96b6e3 was dispatched before this car converged",
            "not yet: no publish-github-pr request answered with an exit",
            "not yet: no tenant.published in the audit tail; none lands until a publish",
            "not yet: no build step nominated to the executor since convergence",
            "not yet: no backlog-item filed by the rule carrying design 0e07ce64",
            "not yet: no tag-release ops-request has been answered",
            "not yet: no green after a red at the same head opened since convergence",
            "not yet: no answered sweep-archive-branches request opened after convergence",
            "not yet: no real prune since convergence",
            "not yet: no sponsorship polled since convergence",
            "not yet: no sponsorship packet opened since the change converged",
        ] {
            // A jq filter's own escaped quotes, which are correct and
            // must not by themselves condemn the probe.
            let probe = format!(
                r#"row=$(printf '%s' "$body" | jq -r ".data[]|select(.k==\"x\")"); echo '{said}'; exit 75"#
            );
            let o = execute(&probe).unwrap();
            assert_eq!(o.exit, NOT_YET_EXIT, "{said}");
            assert!(
                quoting_was_mangled(&probe, &o).is_none(),
                "an honest wait was condemned: {said}\n{o:?}"
            );
            assert!(
                matches!(verdict(&probe, &o, Some("token")), Verdict::NotYet { .. }),
                "an honest wait must stay a wait: {said}"
            );
        }
    }

    /// The guard on the guard: a CORRECTLY quoted probe that says not-yet
    /// keeps saying not-yet. Every honest wait in the shed looks like
    /// this, and 9 of the 13 measured cars were exactly this.
    #[test]
    fn a_correctly_quoted_not_yet_is_still_a_wait() {
        let probe = "echo 'not yet: no tag-release ops-request has been answered'; exit 75";
        let o = execute(probe).unwrap();
        assert_eq!(o.exit, NOT_YET_EXIT);
        assert!(
            failure_diagnosis(probe, &o).is_none(),
            "a clean probe must not be diagnosed: {o:?}"
        );
        let Verdict::NotYet { said } = verdict(probe, &o, Some("token")) else {
            panic!("an honest wait stays a wait: {o:?}")
        };
        assert!(said.contains("no tag-release"), "{said}");
    }

    #[test]
    fn a_probe_naming_a_tool_the_path_lacks_did_not_run() {
        let stub = boss_testing::scratch::scratch_dir("prove-stub-path");
        let probe = format!(
            "export PATH={}; kubectl get pods -A && echo pods:ok",
            stub.display()
        );
        let o = execute(&probe).unwrap();
        assert_eq!(
            o.missing_tools,
            vec!["kubectl".to_string()],
            "the channel names the tool bash could not resolve: {o:?}"
        );
        assert!(
            o.stderr.contains("kubectl: command not found"),
            "bash's own message still reaches stderr: {}",
            o.stderr
        );
        let Verdict::Unrunnable { missing } = verdict(&probe, &o, Some("pods:ok")) else {
            panic!("a missing tool is DID NOT RUN, not a verdict on the claim: {o:?}")
        };
        assert_eq!(missing, vec!["kubectl".to_string()]);
        let why = unrunnable_why("pod-7", &missing);
        assert!(
            why.starts_with("THE PROBE DID NOT RUN on pod-7: kubectl not found."),
            "{why}"
        );
        assert!(
            !why.contains("CANNOT BE READ") && !why.contains("NOT PROVEN"),
            "the two verdicts about the claim must not appear: {why}"
        );
        let a = attempt_json(&probe, Some("pods:ok"), &o, "pod-7", "a", "now", &why, None);
        assert_eq!(a["unrunnable"], true);
        assert_eq!(a["missing_tools"], json!(["kubectl"]));
        assert_eq!(a["not_yet"], false);
    }

    /// A probe whose tools are present is judged exactly as before: the
    /// channel is empty, and the two rules decide.
    #[test]
    fn a_probe_whose_tools_are_present_is_judged_as_today() {
        let green = execute("echo pods:ok").unwrap();
        assert!(green.missing_tools.is_empty(), "{green:?}");
        assert!(matches!(
            verdict("echo pods:ok", &green, Some("pods:ok")),
            Verdict::Proven
        ));
        let red = execute("echo CLAIM FAILS; exit 1").unwrap();
        assert!(red.missing_tools.is_empty(), "{red:?}");
        let Verdict::NotProven(e) = verdict("echo CLAIM FAILS; exit 1", &red, Some("pods:ok"))
        else {
            panic!("a false claim is still NOT PROVEN")
        };
        assert!(
            !e.to_string().contains("DID NOT RUN"),
            "a false claim must not look unrunnable: {e}"
        );
        // A NOT-YET probe is still read as not-yet, prelude and all.
        let ny = execute("echo 'not yet: nothing filed'; exit 75").unwrap();
        assert!(matches!(
            verdict("true", &ny, Some("x:ok")),
            Verdict::NotYet { .. }
        ));
    }

    /// The channel is read the way the runner reads it — `sort -u` —
    /// so a tool a probe fails to find three times is named once, and
    /// the empty lines a `>>` append can leave are not tools.
    #[test]
    fn missing_tools_is_the_channel_sorted_and_deduplicated() {
        assert_eq!(
            missing_tools("kubectl\nboss\nkubectl\n\nboss\n"),
            vec!["boss".to_string(), "kubectl".to_string()]
        );
        assert!(missing_tools("").is_empty());
        assert!(missing_tools("\n\n").is_empty());
    }

    /// `--recheck` ON A NOT-YET CAR RE-RUNS THE ATTEMPT'S PROBE. The
    /// step is still `ready` and carries no `proof`, so the old reader
    /// refused with "nothing to re-run" — but the car holds exactly what
    /// ran, in `proof_attempt`, and re-running it is what "not yet"
    /// asks for. The record's provenance rides along so the report says
    /// "now provable", never "still HOLDS", about a claim never proven.
    #[test]
    fn a_recheck_of_a_not_yet_car_re_runs_the_attempts_probe() {
        let car = json!({"metadata": {"proof_attempt": {
            "at": "2026-09-14T00:00:25Z", "exit": 75, "not_yet": true, "unrunnable": false,
            "probe": "n=$(boss-sor-read /api/x | jq -r '.total // empty'); echo x:ok",
            "expect": "x:ok", "host": "david-asus-minipc",
            "why": "NOT YET: the probe ran on david-asus-minipc and said …",
            "stdout": "not yet: none", "stderr": "", "missing_tools": []
        }}});
        let step = json!({"title": PROVEN, "status": "ready", "metadata": {}});
        let rec = recorded_probe_for(&car, &step).expect("the attempt is what re-runs");
        assert!(rec.probe.starts_with("n=$(boss-sor-read"), "{}", rec.probe);
        assert_eq!(rec.expect.as_deref(), Some("x:ok"));
        assert_eq!(rec.host.as_deref(), Some("david-asus-minipc"));
        assert!(
            rec.cwd.is_none(),
            "the forge's attempt records no cwd; absent is not a mismatch"
        );
        assert_eq!(rec.source, Source::Attempt { not_yet: true });

        // A step that DOES carry a proof still wins — an attempt is the
        // record of a run that settled nothing, not a replacement.
        let proven = json!({"metadata": {"proof": proof_json(
            "real-probe", Some("x"), &ok("x"), "h", "now", &no_tree(), None).to_string()}});
        let rec = recorded_probe_for(&car, &proven).unwrap();
        assert_eq!(rec.probe, "real-probe");
        assert_eq!(rec.source, Source::Proof);

        // And a car with neither still says so.
        let bare = json!({"metadata": {}});
        let e = recorded_probe_for(&bare, &step).unwrap_err().to_string();
        assert!(e.contains("nothing to re-run"), "{e}");
    }

    // -----------------------------------------------------------------
    // THE UNATTENDED DOOR (backlog 9f00a805, consolidation H8 car 2):
    // what infra/forge/run-car-probe.sh did that this file did not,
    // each a gap measured before the twin was retired, each pinned.
    // -----------------------------------------------------------------

    /// THE PRELUDE IS ONE DEFINITION AND IT WORKS: a probe that pipes
    /// its own stderr into a `grep -q` still cannot hide which tool was
    /// missing (f9304366 — the shape the first two arrival probes had).
    /// This ran out of the shell twin's markers in boss-testing's
    /// run_car_probe_sh.rs until the twin was retired; it runs the
    /// door itself now.
    #[test]
    fn a_missing_tool_is_named_even_when_the_probe_swallows_its_own_stderr() {
        let o = execute("kubectl-no-such-tool -n boss get pods 2>&1 | grep -q Running")
            .expect("bash runs");
        assert_eq!(
            o.missing_tools,
            vec!["kubectl-no-such-tool".to_string()],
            "fd 9 must name the tool the probe's own redirection hid: {o:?}"
        );
        assert_ne!(o.exit, 0);
        // And the usual message is still printed, on the stream the
        // probe redirected — nothing is taken away.
        let o = execute("kubectl-no-such-tool get pods").expect("bash runs");
        assert!(
            o.stderr.contains("kubectl-no-such-tool: command not found"),
            "{o:?}"
        );
        assert_eq!(o.exit, 127);
        // A runnable probe leaves the channel empty.
        let o = execute("printf 'claim:ok\\n'").expect("bash runs");
        assert!(o.missing_tools.is_empty(), "{o:?}");
        assert_eq!(o.exit, 0);
    }

    /// THE SHELL'S ARGV, in the twin's order: `timeout -k 5 <secs>`,
    /// then `runuser -u <user> -- setpriv --no-new-privs` only when this
    /// process is root, then `bash -c` with the prelude ahead of the
    /// probe's text. The hand door is the bare `bash -c`.
    #[test]
    fn the_unattended_shell_runs_under_timeout_and_drops_to_the_probe_user() {
        let shell = Shell {
            cwd: Some("/home/david/boss".into()),
            user: Some("david".into()),
            timeout_secs: Some(60),
            env: Vec::new(),
            strip: Vec::new(),
            path_prefix: None,
            inherit: None,
            reader: None,
        };
        let as_root = shell.command_line("echo x", true);
        assert_eq!(
            &as_root[..10],
            &[
                "timeout",
                "-k",
                "5",
                "60",
                "runuser",
                "-u",
                "david",
                "--",
                "setpriv",
                "--no-new-privs"
            ]
        );
        assert_eq!(&as_root[10..12], &["bash", "-c"]);
        assert!(as_root[12].contains("command_not_found_handle()"));
        assert!(as_root[12].ends_with("\necho x"), "{}", as_root[12]);
        // Not root: the twin's by-hand path, as this user, still timed.
        let as_user = shell.command_line("echo x", false);
        assert_eq!(&as_user[..6], &["timeout", "-k", "5", "60", "bash", "-c"]);
        // The hand door: no timeout, no user.
        let hand = Shell::here(None).command_line("echo x", true);
        assert_eq!(&hand[..2], &["bash", "-c"]);
        assert_eq!(hand.len(), 3);
    }

    /// THE LAUNCH THE DOOR USES IS THE ONE THE TICK ASKS FOR (backlog
    /// dda26693, review acab446c F8b; CLAUDE.md §9a). The door drops to
    /// the account and then sets no_new_privs; verify-tick asks the
    /// account to run those same words before it writes the marker, so a
    /// host whose setpriv cannot set the flag stops probes (did-not-run)
    /// instead of answering each with exit 1 — NOT PROVEN on correct code.
    /// The words live twice, here and in the script, and are held equal.
    #[test]
    fn the_launch_words_are_the_ones_the_tick_asks_the_account_to_run() {
        let shell = Shell {
            user: Some("boss-probe".into()),
            ..Shell::here(None)
        };
        let prefix = shell.command_prefix(true, None);
        let after = prefix
            .split(|w| w == "--")
            .nth(1)
            .expect("runuser's words end at --");
        assert_eq!(after, NO_NEW_PRIVS, "{prefix:?}");
        let script =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/probe-account.sh"))
                .expect("infra/forge/probe-account.sh");
        let asked = format!("as_account {} ", NO_NEW_PRIVS.join(" "));
        assert_eq!(
            script.matches(&asked).count(),
            1,
            "probe-account.sh must ask the account to run `{asked}` once, in verify_now"
        );
        // And the words run here and leave the flag set. Both the dev pod
        // and the gate run with allowPrivilegeEscalation: false, where the
        // flag is already 1, so this cannot tell the flag from its absence
        // on those boxes; what it proves is that the spelling is one this
        // host's setpriv runs. The tick's own read of the flag is what
        // tells them apart on the forge.
        let o = std::process::Command::new(NO_NEW_PRIVS[0])
            .args(&NO_NEW_PRIVS[1..])
            .args(["grep", "^NoNewPrivs:", "/proc/self/status"])
            .output()
            .expect("setpriv is util-linux, beside runuser");
        let seen = String::from_utf8_lossy(&o.stdout);
        assert!(
            o.status.success(),
            "{seen}{}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            seen.split_whitespace().collect::<Vec<_>>(),
            ["NoNewPrivs:", "1"]
        );
    }

    /// A PROBE THAT OUTLIVES ITS TIMEOUT IS KILLED AND SAYS SO: exit
    /// 124 from `timeout`, and the record's stderr carries the note the
    /// twin appended, where the probe's own last words are.
    #[test]
    fn a_probe_that_outlives_the_timeout_is_killed_and_the_record_says_so() {
        let shell = Shell {
            timeout_secs: Some(1),
            ..Shell::here(None)
        };
        let o = execute_with("sleep 30; echo never", &shell).expect("bash runs");
        assert_eq!(o.exit, TIMEOUT_EXIT, "{o:?}");
        assert!(o.stderr.contains("killed at 1s timeout"), "{o:?}");
        assert!(!o.stdout.contains("never"));
        // ...and how long it ran, under which bound (backlog a32af300):
        // a kill at the bound is told apart from a fast false claim by
        // the record, not by re-running it.
        let timing = o.timing.expect("a run is timed");
        assert_eq!(timing.bound_s, Some(1));
        assert!(timing.elapsed_ms >= 1000, "{timing:?}");
        assert_eq!(timing.reads, [], "no reader door, no reads logged");
        // Under the timeout the exit is the probe's own.
        let o = execute_with("echo fast:ok", &shell).expect("bash runs");
        assert_eq!(o.exit, 0);
        assert!(o.stdout.contains("fast:ok"));
        assert!(o.timing.expect("a run is timed").elapsed_ms < 1000);
        // The hand door is timed too, and says it had no bound.
        let o = execute_with("true", &Shell::here(None)).expect("bash runs");
        assert_eq!(o.timing.expect("a run is timed").bound_s, None);
    }

    /// THE VERB OUTLIVES THE PROBE IT RUNS (backlog a32af300; CLAUDE.md
    /// §9a). The ops runner kills `run-car-probe` at the verb's own
    /// `timeout`, and a kill there records NOTHING on the car — no
    /// attempt, no elapsed time, the silence this item exists to end. So
    /// the verb's bound holds the probe's bound, the KILL that follows
    /// it, the door's vouch read and the verb's own reads and write
    /// around them (9.3 s on ops-request f155b56c: 69.3 s for a probe
    /// killed at 60) with room. And the unattended door's default bound
    /// is the door's own lifetime, one constant, never a second 60.
    #[test]
    fn the_verb_outlives_the_probe_it_runs() {
        let verb: Value = serde_json::from_str(
            &std::fs::read_to_string(
                boss_testing::repo_root().join("infra/ops/verbs/run-car-probe.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let verb_timeout = verb["timeout"]
            .as_u64()
            .expect("run-car-probe declares a timeout");
        let probe = crate::probe_reader::MAX_LIFETIME_SECS;
        assert!(
            verb_timeout >= probe + KILL_AFTER_SECS + 30,
            "run-car-probe's timeout ({verb_timeout}s) does not outlive its probe's bound \
             ({probe}s) plus the kill and the verb's own reads"
        );
        let about = verb["about"].as_str().unwrap();
        assert!(
            about.contains(&format!("under a {probe} s timeout")),
            "run-car-probe's about names the probe's bound: {probe} s"
        );
        // No host sets BOSS_PROBE_TIMEOUT (measured: nothing in the tree
        // writes it), so the default is the bound every probe runs under.
        assert_eq!(Shell::unattended().unwrap().timeout_secs, Some(probe));
    }

    /// THE PROBE'S ENVIRONMENT IS THE ONE IT WAS PROMISED and nothing
    /// of this verb's own: `env` set, `strip` removed, `path_prefix`
    /// first on PATH, and the run in `cwd`. The reader identity a
    /// recorded probe's `boss-sor-read` sends is exactly the one
    /// `Shell::with_probe_reader` builds for the unattended door.
    #[test]
    fn the_unattended_shell_hands_the_probe_its_env_and_strips_the_verbs_actor() {
        let dir = boss_testing::scratch::scratch_dir("prove-unattended-env");
        let bin = dir.join("pbin");
        std::fs::create_dir_all(&bin).unwrap();
        boss_testing::write_exec(&bin.join("tool-on-prefix"), "#!/bin/sh\necho prefix:ok\n");
        let shell = Shell {
            cwd: Some(dir.clone()),
            user: None,
            timeout_secs: None,
            env: vec![
                ("BOSS_JOBS_URL".into(), "http://sor.invalid:7900".into()),
                ("BOSS_SOR_USER".into(), reader_header(READER_ACTOR)),
                ("BOSS_SOR_PORTS".into(), "jobs=7900 events=7150".into()),
            ],
            strip: vec!["BOSS_PROVE_TEST_LEAK".into()],
            path_prefix: Some(bin),
            inherit: Some(&UNATTENDED_INHERITS),
            reader: None,
        };
        let o = execute_with(
            "tool-on-prefix; printf '[%s][%s][%s][%s]\\n' \"$BOSS_JOBS_URL\" \"$BOSS_SOR_PORTS\" \
             \"${BOSS_PROVE_TEST_LEAK:-}\" \"$PWD\"; printf '%s\\n' \"$BOSS_SOR_USER\"",
            &shell,
        )
        .expect("bash runs");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(o.stdout.contains("prefix:ok"), "{o:?}");
        assert!(
            o.stdout.contains(&format!(
                "[http://sor.invalid:7900][jobs=7900 events=7150][][{}]",
                dir.display()
            )),
            "{o:?}"
        );
        let user: Value = serde_json::from_str(o.stdout.lines().last().unwrap()).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_eq!(user["role"], READER_ROLE);
        assert_eq!(user["access_tier"], "auditor");
    }

    /// THE HAND DOOR RUNS THE SAME TEXT IN THE SAME ENVIRONMENT
    /// (backlog 18fee481, measured 2026-09-22). `--from-car` exists so
    /// an operator can rehearse locally what the forge will run, and
    /// its own help says so — but it ran that text with the operator's
    /// PATH and nothing else, while the unattended door put
    /// `infra/forge/probe-bin` first and exported the reader's identity
    /// and port table. So the one flag whose purpose is to make the two
    /// doors agree was the one that did not: every car whose probe
    /// reads the system of record answered
    /// `boss-sor-read not found` on the pod, and the shed sat at 8 of
    /// 12 cars open past 24 h with the forge's hourly recheck as its
    /// only worker. One construction, both doors (CLAUDE.md §9a): the
    /// env this test compares is BUILT once, so it cannot drift.
    #[test]
    fn both_doors_hand_a_probe_the_same_reader_environment() {
        let base = "http://sor.invalid:7900";
        let unattended = unattended_answered(base);
        // Its own checkout, so this holds wherever BOSS_PROBE_DIR points.
        let hand = Shell::here(None).with_probe_reader(unattended.cwd.as_deref(), base);
        assert_eq!(hand.env, unattended.env, "the promised env is one thing");
        assert_eq!(hand.path_prefix, unattended.path_prefix);
        // ...and the hand door is still the operator's own shell: here,
        // as them, with no timeout (`admit` says so too — a probe that
        // names an actor runs by hand and is only refused at the gate).
        assert_eq!(hand.cwd, None);
        assert_eq!(hand.user, None);
        assert_eq!(hand.timeout_secs, None);
    }

    /// THE SANCTIONED READER IS ON THE PROBE'S PATH AND THE READER IS
    /// NAMED. The measured refusal was `boss-sor-read not found`; the
    /// tool is in the tree beside its route table, so nothing has to be
    /// installed. And the identity it is handed is the read-scoped
    /// actor — never unset (the reader refuses that) and never
    /// `operator:unidentified`, which is answered with a narrower world
    /// silently and makes an absence assertion pass against a page the
    /// probe was never allowed to see (61085a9e).
    #[test]
    fn the_hand_door_puts_the_sanctioned_reader_on_the_probes_path() {
        let root = boss_testing::repo_root();
        let shell = Shell::here(None).with_probe_reader(Some(&root), "http://sor.invalid:7900");
        let o = execute_with(
            "command -v boss-sor-read > /dev/null || { echo no-reader; exit 1; }; \
             printf '[%s][%s]\\n' \"$BOSS_JOBS_URL\" \"$BOSS_SOR_PORTS\"; \
             printf '%s\\n' \"$BOSS_SOR_USER\"",
            &shell,
        )
        .expect("bash runs");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(o.missing_tools.is_empty(), "{o:?}");
        assert!(
            o.stdout.contains("[http://sor.invalid:7900][jobs=7900"),
            "{o:?}"
        );
        let user: Value = serde_json::from_str(o.stdout.lines().last().unwrap()).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_eq!(user["role"], READER_ROLE);
        assert_eq!(user["access_tier"], "auditor");
        assert_ne!(user["id"], crate::identity::UNIDENTIFIED);
    }

    /// A DOOR WITH NO TREE STILL NAMES THE READER. Outside a worktree
    /// there is no `probe-bin` to put on PATH and no port table to
    /// read — a probe needing the reader then says `not found`, which
    /// is the honest DID-NOT-RUN. What must not happen is the read
    /// going out unidentified: the identity does not come from the
    /// tree, so it is handed over either way.
    #[test]
    fn without_a_tree_the_reader_is_still_named_rather_than_unidentified() {
        let shell = Shell::here(None).with_probe_reader(None, "http://sor.invalid:7900");
        assert_eq!(shell.path_prefix, None);
        let user = shell
            .env
            .iter()
            .find(|(k, _)| k == "BOSS_SOR_USER")
            .map(|(_, v)| v.clone())
            .expect("the reader identity is not the tree's to give");
        let user: Value = serde_json::from_str(&user).unwrap();
        assert_eq!(user["id"], READER_ACTOR);
        assert_ne!(user["id"], crate::identity::UNIDENTIFIED);
    }

    /// The unattended door's shell as it stands once the account has
    /// answered: what `Shell::unattended` builds from the environment,
    /// with the reader half `unattended_ground` then adds out of its view
    /// (review 37f44583 F1) — without asking this process's environment
    /// for a marker.
    fn unattended_answered(base: &str) -> Shell {
        let built = Shell::unattended().expect("the unattended door builds");
        let view = built.cwd.clone();
        built.with_probe_reader(view.as_deref(), base)
    }

    /// THE FOUR DOORS A CAR'S PROBE RUNS AT, each built the way its own
    /// call site builds it, with the reader credential at `credential`
    /// and the system of record at `base`. The unattended door runs here
    /// as itself in a scratch directory — its user and checkout are the
    /// forge's — as `no_machine_token_reaches_a_probe_at_either_door`
    /// does. `every_door_names_the_reader_door_it_may_open` holds this
    /// list to the source.
    fn the_four_doors(base: &str, credential: &Path) -> Vec<(&'static str, Shell)> {
        let scratch = boss_testing::scratch::scratch_dir("prove-reader-door");
        let unattended = Shell {
            user: None,
            cwd: Some(scratch.clone()),
            ..unattended_answered(base)
        };
        let mut doors = vec![
            ("unattended", unattended),
            (
                "recheck",
                Shell::here(Some(&scratch)).with_probe_reader(None, base),
            ),
            ("hand", Shell::here(None).with_probe_reader(None, base)),
            ("disproved", Shell::here(None).with_probe_reader(None, base)),
        ];
        for (_, shell) in &mut doors {
            shell.reader.as_mut().unwrap().credential = credential.to_path_buf();
        }
        doors
    }

    /// Each of those four sites names the door in non-test source: the
    /// three `Shell::here(..).with_probe_reader(..)` and the unattended
    /// door's, which `unattended_ground` adds once the account answers.
    #[test]
    fn every_door_names_the_reader_door_it_may_open() {
        let prod = |src: &'static str| src.split("#[cfg(test)]").next().unwrap_or("");
        let prove = prod(include_str!("prove.rs"));
        let disprove = prod(include_str!("disprove.rs"));
        let named = |src: &str| src.matches(".with_probe_reader(").count();
        // recheck, hand, and inside `unattended_ground`.
        assert_eq!(
            named(prove),
            3,
            "a door in prove.rs gained or lost its reader"
        );
        assert_eq!(named(disprove), 1);
        assert_eq!(
            prove.matches("crate::probe_reader::open(").count(),
            1,
            "every door opens its reader in one place, execute_given"
        );
        assert!(!prove.contains("probe_reader::Guard::start"));
    }

    /// What a probe can see of the door it was or was not given: the
    /// socket's name (or `none`), who it reads as on the LAN path, and
    /// the program that started its shell — `timeout` when it is
    /// bounded.
    const DOOR_REPORT: &str = "printf 'DOOR=%s USER=%s PARENT=%s\\n' \"${BOSS_SOR_DOOR:-none}\" \
         \"${BOSS_SOR_USER:+named}\" \"$(cat /proc/$PPID/comm)\"; \
         test -z \"${BOSS_PROBE_READER_CREDENTIAL:-}\" || exit 42";

    #[test]
    fn every_admitted_probe_is_marked_even_when_its_channel_cannot_be_created() {
        const CHILD: &str = "BOSS_TEST_PROBE_CHANNEL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "prove::tests::every_admitted_probe_is_marked_even_when_its_channel_cannot_be_created", "--nocapture"])
                .env(CHILD, "1")
                .env_remove("BOSS_PROBE_NOTFOUND")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed; 0 failed"));
            return;
        }
        let no_token = crate::door_env::NoTokenInReach::new().unwrap();
        let root = boss_testing::scratch_dir("probe-channel-not-a-directory");
        boss_testing::create_dir(&root);
        let blocker = root.join("file");
        boss_testing::write_file(&blocker, "synthetic non-directory");
        // Only this exact child test runs; no parallel process-global
        // environment is changed. Construct the empty-token directory
        // first, then make only the optional channel creation fail.
        unsafe { std::env::set_var("TMPDIR", &blocker) };
        let out = execute_given(
            "test \"${BOSS_PROBE_NOTFOUND+set}\" = set || exit 41; printf 'ADMITTED_PROBE_MARKED\\n'",
            &Shell::here(None),
            Ok(no_token),
        ).unwrap();
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(out.stdout.contains("ADMITTED_PROBE_MARKED"));
    }

    /// B1 (review 0bd6a9c2), MEASURED AT ALL FOUR DOORS. A host with no
    /// reader credential — every host in the estate, until the delivery
    /// chain exists — gets no door and the probe RUNS, reading as it
    /// did before the door existed: the read-scoped identity in
    /// `BOSS_SOR_USER`, no socket, and no bound the door did not ask
    /// for. The first cut refused here, at every door, which would have
    /// stopped every proof in the estate the hour it landed.
    #[test]
    fn with_no_reader_credential_every_door_runs_the_probe_as_it_did_before() {
        let held = tempfile::tempdir().unwrap();
        let absent = held.path().join("never-deposited.credential");
        for (door, shell) in the_four_doors("http://sor.invalid:7900", &absent) {
            let o = execute_with(DOOR_REPORT, &shell)
                .unwrap_or_else(|e| panic!("{door}: refused with no credential: {e:#}"));
            assert_eq!(o.exit, 0, "{door}: {o:?}");
            assert!(o.stdout.contains("DOOR=none USER=named"), "{door}: {o:?}");
            // Only the unattended door asked for a timeout.
            assert_eq!(
                o.stdout.contains("PARENT=timeout"),
                door == "unattended",
                "{door}: {o:?}"
            );
        }
    }

    /// B1's other half, and B3, at all four doors: a credential that is
    /// PRESENT and cannot be used — not private, empty, or one the gate
    /// does not name a reader — is the host's refusal. The probe does
    /// not run (its marker is never written), nothing is judged, and it
    /// is never the quiet no-door of an absent file.
    #[test]
    fn a_present_but_unusable_credential_refuses_at_every_door_and_runs_nothing() {
        use crate::probe_reader::fixture;
        let upstream = fixture::gated_with_reader(Some(fixture::VALUE));
        let scratch = boss_testing::scratch::scratch_dir("prove-reader-door-refused");
        let marker = scratch.join("the-probe-ran");
        let probe = format!("touch '{}'", marker.display());
        for (what, text, mode, says) in [
            ("not private", fixture::VALUE, 0o644, "not private"),
            ("empty", "", 0o600, "is empty"),
            (
                "not a reader's",
                "fixture-value-no-slot-holds",
                0o600,
                "not a reader slot",
            ),
        ] {
            let held = tempfile::tempdir().unwrap();
            let credential = fixture::credential(held.path(), text, mode);
            for (door, shell) in the_four_doors(&upstream.base, &credential) {
                let _ = std::fs::remove_file(&marker);
                let refused = match execute_with(&probe, &shell) {
                    Ok(o) => panic!("{door}, {what}: the probe ran: {o:?}"),
                    Err(e) => e,
                };
                let said = refused
                    .downcast_ref::<EnvironmentRefusal>()
                    .unwrap_or_else(|| panic!("{door}, {what}: not a refusal: {refused:#}"))
                    .to_string();
                assert!(said.contains("the probe did not run"), "{door}: {said}");
                assert!(said.contains(says), "{door}, {what}: {said}");
                assert!(!said.contains(text) || text.is_empty(), "{door}: {said}");
                assert!(!marker.exists(), "{door}, {what}: the probe ran anyway");
            }
        }
        assert!(
            upstream.seen.lock().unwrap().is_empty(),
            "a refused door served a read"
        );
    }

    /// AND WITH A CREDENTIAL THE GATE NAMES A READER, the door stands
    /// for exactly the probe's life: a private socket in its
    /// environment, the shipped `boss-sor-read` reading through it and
    /// answered as the probe reader by the real gate, the credential's
    /// own path nowhere in reach, the probe bounded — and nothing left
    /// when it returns.
    #[test]
    fn a_probe_has_a_private_reader_socket_only_for_its_own_lifetime() {
        use crate::probe_reader::fixture;
        let upstream = fixture::gated_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let root = boss_testing::repo_root();
        let mut shell = Shell::here(None).with_probe_reader(Some(&root), &upstream.base);
        shell.reader.as_mut().unwrap().credential = credential.clone();
        // The tree's port table names the estate's ports; this fixture
        // has one, so every path is read on the base.
        shell.reader.as_mut().unwrap().ports = String::new();
        shell.env.retain(|(k, _)| k != "BOSS_SOR_PORTS");
        // Even an ambient child env entry is scrubbed after applying
        // Shell.env; the private config never lends its path to it.
        shell.env.push((
            crate::probe_reader::CREDENTIAL_ENV.into(),
            credential.display().to_string(),
        ));
        let out = execute_with(
            &format!(
                "test -S \"${{BOSS_SOR_DOOR:-}}\" || exit 41; {DOOR_REPORT}; \
                 boss-sor-read '/api/yard/status' || exit 43; echo; \
                 printf '%s\\n' \"$BOSS_SOR_DOOR\""
            ),
            &shell,
        )
        .unwrap();
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(out.stdout.contains("PARENT=timeout"), "{out:?}");
        assert!(out.stdout.contains(fixture::BODY), "{out:?}");
        {
            let seen = upstream.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "one read, through the door");
            assert_eq!(seen[0].1, "/api/yard/status");
            let user: Value = serde_json::from_slice(seen[0].2["x-boss-user"].as_bytes()).unwrap();
            assert_eq!(user["id"], boss_core::roles::PROBE_READER_ACTOR);
        }
        let socket = std::path::Path::new(out.stdout.lines().last().unwrap());
        assert!(
            socket.is_absolute(),
            "the probe observed a real socket path"
        );
        assert!(!socket.exists(), "the reader door outlived its probe");
        assert!(
            !socket.parent().unwrap().exists(),
            "private directory leaked"
        );
    }

    /// A KILLED PROBE NAMES THE READ ITS TIME WENT TO (backlog a32af300).
    /// Car 5159b6b6's probe read the jobs API's health and then
    /// `/api/yard/regions?window=24h`, which the system of record took
    /// 25-122 s to answer; killed at the bound, the attempt said only
    /// "killed at 60s timeout" beside an empty stdout. Through the
    /// shipped `boss-sor-read` and a real door, against an upstream that
    /// never answers the second read: the outcome carries the run's
    /// elapsed time, its bound and both reads — the first answered, the
    /// second in flight — and the kill note names the read in flight.
    #[test]
    fn a_killed_probe_names_the_read_in_flight_and_how_long_the_run_took() {
        use crate::probe_reader::fixture;
        let upstream = fixture::serve(|seen| {
            axum::Router::new()
                .route(
                    "/api/yard/regions",
                    axum::routing::get(std::future::pending::<http::StatusCode>),
                )
                .merge(fixture::recorder(seen))
                .merge(fixture::accepts_route("reader.current"))
        });
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let mut shell = reader_shell(&upstream.base, &credential, "");
        shell.timeout_secs = Some(2);
        let o = execute_with(
            "boss-sor-read /api/jobs/health >/dev/null || exit 43\n\
             map=$(boss-sor-read '/api/yard/regions?window=24h')\n\
             echo never",
            &shell,
        )
        .expect("the probe ran");
        assert_eq!(o.exit, TIMEOUT_EXIT, "{o:?}");
        let timing = o.timing.as_ref().expect("a run is timed");
        assert_eq!(timing.bound_s, Some(2), "{timing:?}");
        assert!(timing.elapsed_ms >= 2000, "{timing:?}");
        assert_eq!(timing.reads.len(), 2, "{timing:?}");
        assert_eq!(timing.reads[0].path, "/api/jobs/health");
        assert_eq!(timing.reads[0].status, Some(207));
        assert_eq!(timing.reads[1].path, "/api/yard/regions?window=24h");
        assert_eq!(timing.reads[1].status, None, "{timing:?}");
        assert!(o.stderr.contains("killed at 2s timeout"), "{o:?}");
        assert!(
            o.stderr
                .contains("still waiting on /api/yard/regions?window=24h after"),
            "{o:?}"
        );
    }

    /// A shell at the hand door whose reader door would open on
    /// `credential`, whose port table is `ports` and nothing from the
    /// tree's, and whose probe finds the shipped `boss-sor-read`.
    fn reader_shell(base: &str, credential: &Path, ports: &str) -> Shell {
        let root = boss_testing::repo_root();
        let mut shell = Shell::here(None).with_probe_reader(Some(&root), base);
        let reader = shell.reader.as_mut().unwrap();
        reader.credential = credential.to_path_buf();
        reader.ports = ports.to_string();
        shell.env.retain(|(k, _)| k != "BOSS_SOR_PORTS");
        shell.env.push(("BOSS_SOR_PORTS".into(), ports.into()));
        shell
    }

    /// N5 (review 991bb439), THE WHOLE DOOR AGAINST A GATE THAT ENFORCES
    /// — the reviewer's first measurement, in the tree. Every fixture
    /// before this reported, and the estate's gates enforce at row C of
    /// design b08725c2. The shipped `boss-sor-read`, through the door,
    /// is answered by the real gate in ENFORCE and the handler is told
    /// the probe reader; the same read with no door at all (no
    /// credential on the host) is refused by that gate, which is what
    /// makes the first leg a statement about the door.
    #[test]
    fn under_an_enforcing_gate_a_probe_reads_through_the_door_as_the_probe_reader() {
        use crate::probe_reader::fixture;
        let upstream = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let out = execute_with(
            "boss-sor-read /api/yard/status",
            &reader_shell(&upstream.base, &credential, ""),
        )
        .unwrap();
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(out.stdout.contains(fixture::BODY), "{out:?}");
        {
            let seen = upstream.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "one read, through the door");
            let user: Value = serde_json::from_slice(seen[0].2["x-boss-user"].as_bytes()).unwrap();
            assert_eq!(user["id"], boss_core::roles::PROBE_READER_ACTOR);
        }
        // The control: no credential, so no door, and the gate refuses
        // the LAN read `BOSS_SOR_USER` used to carry.
        let absent = held.path().join("never-deposited.credential");
        let out = execute_with(
            "boss-sor-read /api/yard/status",
            &reader_shell(&upstream.base, &absent, ""),
        )
        .unwrap();
        assert_eq!(out.exit, 22, "{out:?}");
        assert!(out.stderr.contains("401"), "{out:?}");
        assert_eq!(upstream.seen.lock().unwrap().len(), 1);
    }

    /// N5, the reviewer's second measurement, at all four doors: under
    /// ENFORCE a credential no reader slot holds is the host's refusal
    /// at door-open — naming the 401 the gate gave the accepts read —
    /// and nothing runs and nothing is read. Under a reporting gate the
    /// same value is refused for a different reason (`none`), which is
    /// the only leg the tree held until now.
    #[test]
    fn under_an_enforcing_gate_an_unaccepted_credential_is_refused_at_door_open() {
        use crate::probe_reader::fixture;
        let upstream = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let scratch = boss_testing::scratch::scratch_dir("prove-reader-door-enforce-refused");
        let marker = scratch.join("the-probe-ran");
        let probe = format!("touch '{}'", marker.display());
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), "fixture-value-no-slot-holds", 0o600);
        for (door, shell) in the_four_doors(&upstream.base, &credential) {
            let refused = match execute_with(&probe, &shell) {
                Ok(o) => panic!("{door}: the probe ran: {o:?}"),
                Err(e) => e,
            };
            let said = refused
                .downcast_ref::<EnvironmentRefusal>()
                .unwrap_or_else(|| panic!("{door}: not a refusal: {refused:#}"))
                .to_string();
            assert!(said.contains("the probe did not run"), "{door}: {said}");
            assert!(
                said.contains("answered HTTP 401 to the accepts read"),
                "{door}: {said}"
            );
            assert!(!said.contains("fixture-value-no-slot-holds"), "{door}");
            assert!(!marker.exists(), "{door}: the probe ran anyway");
        }
        assert!(
            upstream.seen.lock().unwrap().is_empty(),
            "a read was served"
        );
    }

    /// N1 (review 991bb439): A PORT NO GATE VOUCHES FOR, MID-PROBE, IS
    /// THE HOST'S REFUSAL AND NOT THE CAR'S FAILURE. The base's gate
    /// names the reader; a second service's does not (it mounts no
    /// reader slot). The probe reads that second service. Measured
    /// before this change: exit 1, `curl: (22) ... 502`, and the judge's
    /// "A probe that fails is evidence AGAINST the claim" — a failed
    /// proof on correct code.
    ///
    /// Now it is an [`EnvironmentRefusal`] whose words are true of it:
    /// the probe RAN (its marker is there) and its result was discarded,
    /// with the port and the gate's answer named. A probe that swallows
    /// the failed read and prints its marker is refused the same way —
    /// the strict side. The controls: the same probe against a second
    /// service that DOES name the reader passes, and a probe that reads
    /// only the base is untouched by the unvouched port's existence.
    #[test]
    fn a_port_no_gate_vouches_for_mid_probe_is_the_hosts_refusal_not_the_cars_failure() {
        use crate::probe_reader::fixture;
        let base = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let unmounted = fixture::enforcing_with_reader(None);
        let mounted = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let scratch = boss_testing::scratch::scratch_dir("prove-reader-door-unvouched");
        let marker = scratch.join("the-probe-ran");
        let reads = format!(
            "touch '{}'; boss-sor-read /api/events/tail | grep -q {} && echo EXPECTED",
            marker.display(),
            fixture::BODY
        );
        let swallows = "boss-sor-read /api/events/tail || true; echo EXPECTED";

        let ports = format!("events={}", unmounted.port);
        let shell = reader_shell(&base.base, &credential, &ports);
        for probe in [reads.as_str(), swallows] {
            let refused = match execute_with(probe, &shell) {
                Ok(o) => panic!("judged a probe that ran without its read: {o:?}"),
                Err(e) => e,
            };
            let said = refused.to_string();
            let cause = environment_refusal(refused).expect("the host's refusal");
            assert!(cause.starts_with(RAN_WITHOUT_A_READ), "{cause}");
            assert!(
                cause.contains(&format!("port {} answered HTTP 401", unmounted.port)),
                "{cause}"
            );
            assert!(cause.contains("is not a judgement of the claim"), "{cause}");
            assert!(!cause.contains(fixture::VALUE), "{cause}");
            // The fixed sentences follow the cause: this one RAN.
            assert!(said.contains("the probe's result was discarded"), "{said}");
            assert!(!said.contains("did not run"), "{said}");
            let why = environment_why("forge", &cause);
            assert!(why.starts_with("NOT YET: the probe's result was discarded"));
            assert!(!why.contains("did not run"), "{why}");
            // And on the car it is a not-yet the hourly recheck retries.
            let patch = refusal_patch(
                &json!({"metadata": {}}),
                probe,
                None,
                "forge",
                "a",
                "t",
                &cause,
            );
            assert!(boss_jobs::car::attempt_said_not_yet(
                &patch["proof_attempt"]
            ));
        }
        assert!(marker.exists(), "the first probe never started");
        assert!(
            unmounted.seen.lock().unwrap().is_empty(),
            "an unvouched service was read"
        );
        // A refusal made BEFORE the run still says so.
        assert!(
            EnvironmentRefusal::before_execution("the temp filesystem is full".into())
                .to_string()
                .contains("the probe did not run")
        );
        assert!(environment_why("forge", "full").contains("the probe did not run"));

        // Control one: a second service that names the reader is read.
        let ports = format!("events={}", mounted.port);
        let out = execute_with(&reads, &reader_shell(&base.base, &credential, &ports)).unwrap();
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(out.stdout.contains("EXPECTED"), "{out:?}");
        assert_eq!(mounted.seen.lock().unwrap().len(), 1);
        // Control two: the unvouched port, listed and never read.
        let ports = format!("events={}", unmounted.port);
        let out = execute_with(
            "boss-sor-read /api/yard/status > /dev/null && echo EXPECTED",
            &reader_shell(&base.base, &credential, &ports),
        )
        .unwrap();
        assert_eq!(out.exit, 0, "{out:?}");
        assert_eq!(unserved_read_refusal(&[], 1), None);
    }

    /// N1 (review cd3f6a99): A LISTED SERVICE THAT IS SIMPLY DARK when the
    /// probe first reads it is the probe's own failed read — judged, as
    /// it was before the door — and not the host's refusal. A car that
    /// took its own service down must read NOT PROVEN, not an hourly
    /// NOT YET that says "not a verdict against the change".
    #[test]
    fn a_listed_service_that_is_dark_is_the_probes_own_failed_read() {
        use crate::probe_reader::fixture;
        let base = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let dark = fixture::dark_port();
        let closed = dark.port;
        let shell = reader_shell(&base.base, &credential, &format!("events={closed}"));
        let probe = "boss-sor-read /api/events/tail && echo EXPECTED";
        let out = execute_with(probe, &shell)
            .unwrap_or_else(|e| panic!("a dark service was made the host's refusal: {e:#}"));
        assert_eq!(out.exit, 22, "{out:?}");
        assert!(out.stderr.contains("502"), "{out:?}");
        let said = format!("{:?}", judge_probe(probe, &out, Some("EXPECTED")));
        assert!(said.contains("AGAINST the claim"), "{said}");
    }

    /// N4 (review 991bb439), THE STRIP: a `BOSS_SOR_DOOR` standing in the
    /// probe's environment from anywhere but this door never reaches a
    /// probe. With no credential the probe is given NO door, and says
    /// `none` — not a socket somebody else named, which the shipped
    /// reader would send every read to. With a door open, the probe
    /// sees this door's socket and not the planted name. (The mutant
    /// that deleted the strip survived every test: each either opened a
    /// door, which overwrites the name, or started with none set.)
    #[test]
    fn a_door_name_standing_in_the_environment_never_reaches_a_probe() {
        use crate::probe_reader::fixture;
        let held = tempfile::tempdir().unwrap();
        let planted = held.path().join("somebody-elses.sock");
        let absent = held.path().join("never-deposited.credential");
        let plant = |mut shell: Shell| {
            shell.env.push((
                crate::probe_reader::DOOR_ENV.into(),
                planted.display().to_string(),
            ));
            shell
        };
        for (door, shell) in the_four_doors("http://sor.invalid:7900", &absent) {
            let o = execute_with(DOOR_REPORT, &plant(shell)).unwrap();
            assert!(
                o.stdout.contains("DOOR=none "),
                "{door}: a planted door name reached a probe given no door: {o:?}"
            );
        }
        let upstream = fixture::gated_with_reader(Some(fixture::VALUE));
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let o = execute_with(
            DOOR_REPORT,
            &plant(reader_shell(&upstream.base, &credential, "")),
        )
        .unwrap();
        assert!(o.stdout.contains("DOOR=/"), "{o:?}");
        assert!(!o.stdout.contains("somebody-elses.sock"), "{o:?}");
    }

    /// N4, THE RECIPIENT: the socket is handed to the door's user only
    /// when this process is root. Pure, so it is held on every uid —
    /// the mutant that dropped the condition is invisible to a root run,
    /// where the condition is true anyway.
    #[test]
    fn the_reader_socket_is_handed_over_only_by_root() {
        let named = Shell {
            user: Some("boss-probe".into()),
            ..Shell::here(None)
        };
        assert_eq!(door_recipient(&named, true), Some("boss-probe"));
        assert_eq!(door_recipient(&named, false), None);
        assert_eq!(door_recipient(&Shell::here(None), true), None);
        assert_eq!(door_recipient(&Shell::here(None), false), None);
    }

    /// N4 by effect, where it can be seen — as anyone but root. A door
    /// that names a user, run by a process that cannot drop to one,
    /// proves as itself: the reader door opens for THIS process and the
    /// probe reads through it. Handing the socket to the named user
    /// instead is a refusal there (root's own uid could rename within
    /// /tmp), so every hand run of the unattended shell would stop.
    #[test]
    fn a_door_that_names_a_user_still_proves_by_hand_as_anyone_but_root() {
        use crate::probe_reader::fixture;
        if running_as_root() {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED, NOT PASSED: prove::tests::\
                 a_door_that_names_a_user_still_proves_by_hand_as_anyone_but_root — this \
                 process is root, for which the hand-over is the real one. The gate's uid \
                 exercises it; the_reader_socket_is_handed_over_only_by_root holds the rule here."
            );
            return;
        }
        let upstream = fixture::gated_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let shell = Shell {
            user: Some("root".into()),
            ..reader_shell(&upstream.base, &credential, "")
        };
        let token = crate::door_env::NoTokenInReach::new();
        let out = execute_gated("boss-sor-read /api/yard/status", &shell, token, None)
            .expect("a hand run of a door that names a user");
        assert_eq!(out.exit, 0, "{out:?}");
        assert_eq!(upstream.seen.lock().unwrap().len(), 1);
    }

    /// N3 (review 991bb439): the door is told whose file its port list
    /// came out of — the tree's own, read once — and a door with no
    /// tree has no list and nothing to be told.
    #[test]
    fn the_reader_door_knows_whose_file_its_port_list_was() {
        use std::os::unix::fs::MetadataExt;
        let root = boss_testing::repo_root();
        let list = root.join(SOR_PORTS_ENV);
        let shell = Shell::here(None).with_probe_reader(Some(&root), "http://sor.invalid:7900");
        let reader = shell.reader.unwrap();
        let from = reader.ports_from.expect("the tree's list names its file");
        assert_eq!(from.path, list);
        assert_eq!(from.uid, std::fs::metadata(&list).unwrap().uid());
        assert!(reader.ports.contains("jobs=7900"), "{}", reader.ports);
        let none = Shell::here(None).with_probe_reader(None, "http://sor.invalid:7900");
        let reader = none.reader.unwrap();
        assert_eq!(reader.ports_from, None);
        assert_eq!(reader.ports, "");
    }

    /// F2 (review 0bd6a9c2). The door-lifetime cap belongs to an OPEN door. With
    /// none, each door keeps what it asked for — the hand door no
    /// timeout at all, as its help says — and when a door does shorten
    /// what was asked, the shell says so rather than killing a rehearsed
    /// probe at a bound nobody named.
    #[test]
    fn only_an_open_reader_door_bounds_a_probe_and_it_says_when_it_does() {
        for (asked, open, bound, told) in [
            (None, false, None, false),
            (Some(120), false, Some(120), false),
            (Some(2), false, Some(2), false),
            (None, true, Some(180), true),
            (Some(300), true, Some(180), true),
            (Some(180), true, Some(180), false),
            (Some(120), true, Some(120), false),
            (Some(2), true, Some(2), false),
        ] {
            let mut shell = Shell::here(None).with_probe_reader(None, "http://sor.invalid:7900");
            shell.timeout_secs = asked;
            assert_eq!(shell.bound(open), bound, "{asked:?} open={open}");
            let notice = shell.clamp_notice(open);
            assert_eq!(notice.is_some(), told, "{asked:?} open={open}: {notice:?}");
            let argv = shell.command_line_under("true", false, shell.bound(open));
            match bound {
                Some(t) => assert_eq!(&argv[..4], &["timeout", "-k", "5", &t.to_string()]),
                None => assert_eq!(argv[0], "bash"),
            }
        }
        let hand = Shell::here(None).with_probe_reader(None, "http://sor.invalid:7900");
        let notice = hand.clamp_notice(true).unwrap();
        assert!(notice.contains("bounded at 180s"), "{notice}");
        assert!(notice.contains("asked for no timeout"), "{notice}");
        // Naming a reader bounds nothing by itself.
        assert_eq!(hand.command_line("true", false)[0], "bash");
        assert_eq!(crate::probe_reader::MAX_LIFETIME_SECS, 180);
    }

    /// THE CAR'S OWN CONVERGED INSTANT IS PART OF THAT PROMISE
    /// (backlog a92571a6). A probe that needs a cutoff gets one that
    /// does NOT move: the commit time of the car's own merge, read
    /// from the checkout the probe runs in. A merge that is not in
    /// this checkout hands over no instant at all — the honest not-yet
    /// — and a ref that is not an object name is never handed to git.
    #[test]
    fn the_probe_is_handed_its_cars_own_converged_instant() {
        let dir = boss_testing::scratch::scratch_dir("prove-car-instant");
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(["-C", &dir.display().to_string()])
                .args(args)
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .expect("git runs");
            assert!(o.status.success(), "git {args:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("f"), "x").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-q", "-m", "landed"]);
        let sha = git(&["rev-parse", "HEAD"]);

        let shell = Shell::here(Some(&dir)).with_car_instant(Some(&sha[..12]));
        let at = |s: &Shell, k: &str| s.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(
            at(&shell, boss_jobs::probe::CAR_CONVERGED_AT_VAR).as_deref(),
            Some("1700000000"),
            "the merge's own commit time, in epoch seconds: {shell:?}"
        );
        assert_eq!(
            at(&shell, boss_jobs::probe::CAR_MERGE_REF_VAR).as_deref(),
            Some(&sha[..12])
        );
        // The probe sees it, and a stale one from the runner's own
        // environment cannot reach it.
        assert!(
            shell
                .strip
                .iter()
                .any(|n| n == boss_jobs::probe::CAR_CONVERGED_AT_VAR)
        );
        let o =
            execute_with("printf '[%s]\\n' \"$BOSS_CAR_CONVERGED_AT\"", &shell).expect("bash runs");
        assert!(o.stdout.contains("[1700000000]"), "{o:?}");

        // A merge this checkout does not have: the ref rides, the
        // instant does not, and the recipe's guard says not yet.
        let absent = Shell::here(Some(&dir)).with_car_instant(Some("0123456789ab"));
        assert_eq!(at(&absent, boss_jobs::probe::CAR_CONVERGED_AT_VAR), None);
        assert_eq!(
            at(&absent, boss_jobs::probe::CAR_MERGE_REF_VAR).as_deref(),
            Some("0123456789ab")
        );
        // Not an object name — never handed to git, so neither rides.
        for bad in ["--upload-pack=touch /tmp/x", "main", ""] {
            let s = Shell::here(Some(&dir)).with_car_instant(Some(bad));
            assert!(s.env.is_empty(), "{bad:?}: {s:?}");
        }
        assert!(
            Shell::here(Some(&dir))
                .with_car_instant(None)
                .env
                .is_empty()
        );
    }

    /// AND THE CAR SAYS WHICH MERGE THAT WAS — the `merge_ref` the
    /// conductor wrote on it, read the way every other door reads it.
    #[test]
    fn the_cars_merge_ref_is_read_off_its_metadata() {
        let car = json!({"metadata": {"merge_ref": "ead63bba8aff"}});
        assert_eq!(car_merge_ref(&car), Some("ead63bba8aff"));
        assert_eq!(car_merge_ref(&json!({"metadata": {}})), None);
        assert_eq!(
            car_merge_ref(&json!({"metadata": {"merge_ref": "not-a-sha"}})),
            None
        );
    }

    /// THE STARVED CUTOFF IS SAID AT THE DOOR A BUILDER IS STANDING AT,
    /// with the promised variable named (a92571a6). Car 372ac8fd's
    /// recorded probe is the live instance.
    #[test]
    fn a_probe_that_dates_its_cutoff_from_head_is_warned_about() {
        let probe = "c=$(git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote); \
                     since=$(git log -1 --format=%ct HEAD); \
                     j=$(boss-sor-read '/api/jobs?kind=maintenance-dev-scratch-reclaim')";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("DATES ITS CUTOFF"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(
            said.contains(boss_jobs::probe::CAR_CONVERGED_AT_VAR),
            "{said}"
        );
        assert!(
            said.contains(boss_jobs::probe::MOVING_HEAD_EVIDENCE),
            "{said}"
        );
        assert!(said.contains("warning, not a refusal"), "{said}");
        // The rewrite it names is not warned about in turn.
        let fixed = "c=$(git show HEAD:infra/cluster/dev-scratch-reclaim.sh | grep -c ls-remote); \
                     since=${BOSS_CAR_CONVERGED_AT}; \
                     case ${since:-empty} in empty|*[!0-9]*) echo 'not yet'; exit 75;; esac";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("DATES ITS CUTOFF")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// A LIMIT IS NOT A FILTER, SAID AT THE DOOR (e7cf78c6). Car
    /// ead4a6ed's recorded probe is the live instance: 300 rows asked
    /// of a list of 345, with the row it waited on in the tail.
    #[test]
    fn a_probe_that_counts_a_page_is_warned_about() {
        let probe = "n=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300' \
                     | jq '[.data[] | select(.metadata.flake == true)] | length'); \
                     [ \"$n\" -ge 1 ] && echo flake:seen";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("COUNTS A PAGE"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(said.contains("limit=300"), "{said}");
        assert!(
            said.contains(boss_jobs::probe::TRUNCATED_PAGE_EVIDENCE),
            "{said}"
        );
        // It says which way it fails, because that is what its reader
        // decides on.
        assert!(said.contains("CAN FAIL OPEN"), "{said}");
        // And the rewrite it names — one body, rows judged against the
        // total — is not warned about in turn.
        let fixed = "body=$(boss-sor-read '/api/jobs?kind=gate-run&limit=300'); \
                     rows=$(printf '%s' \"$body\" | jq '.data | length'); \
                     seen=$(printf '%s' \"$body\" | jq '.total'); \
                     [ \"$rows\" -eq \"$seen\" ] || { echo 'not yet: the page is not the list'; exit 75; }";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("COUNTS A PAGE")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// ONE FACT PER CALLER PER PROCESS, SAID AT THE DOOR (ce72aea9). Car
    /// 3b8d02f6's recorded probe is the live instance: PROVEN on a count
    /// of logged would-refuse facts, the live tally never read.
    #[test]
    fn a_probe_that_clears_a_caller_from_the_gate_log_alone_is_warned_about() {
        let log = r#"n=$(boss-sor-read "/api/events/gate-window?gate=machine-gate&hours=$hours" | jq -r --arg cut "$cut" 'if (.log_error == null) and ((.log.dirty | type) == "array") then [.log.dirty[] | select(.kind == "machine_gate.would_refuse" and .payload.service == "jobs" and .payload.key.user == "automation:estate-observer" and .at > $cut)] | length else empty end')
case ${n:-empty} in empty|*[!0-9]*) echo 'not yet: the machine-gate window could not be read'; exit 75;; esac
"#;
        let w: Vec<String> = shape_warnings(log).collect();
        let said = w
            .iter()
            .find(|w| w.contains("FROM THE GATE LOG ALONE"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(said.contains("machine_gate.would_refuse"), "{said}");
        assert!(
            said.contains(boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE),
            "{said}"
        );
        // It says which way it fails, and names the reading that repairs it.
        assert!(said.contains("CAN FAIL OPEN"), "{said}");
        assert!(said.contains(".live[].snapshot.rows"), "{said}");
        // The rewrite it names — the same window's live rows read too —
        // is not warned about in turn.
        let fixed = format!(
            "{log}{}",
            r#"live=$(boss-sor-read "/api/events/gate-window?gate=machine-gate&hours=$hours" | jq -r --arg cut "$cut" '([ .live[]? | select(.service == "jobs") ] | first) as $j | if $j == null or (($j.snapshot.rows | type) != "array") or $j.snapshot.overflow != 0 or $j.snapshot.mode == "off" or $j.snapshot.recording_since > $cut then "unreadable" else ([ $j.snapshot.rows[] | select(.user == "automation:estate-observer" and .presented == "none") ] | length | tostring) end')
case ${live:-empty} in unreadable|empty|*[!0-9]*) echo 'not yet: the live tally could not be read whole'; exit 75;; esac
"#
        );
        assert!(
            !shape_warnings(&fixed).any(|w| w.contains("FROM THE GATE LOG ALONE")),
            "{:?}",
            shape_warnings(&fixed).collect::<Vec<_>>()
        );
    }

    /// AND REFUSED AT THE EXECUTOR WHEN NOBODY IS THERE (review 4ce5ca85
    /// N1). The dispatcher refuses this shape before filing a
    /// run-car-probe request, but a request filed by hand, by another
    /// rule, or by a dispatcher still on the old build reaches
    /// `boss prove --from-car --unattended` all the same — and the car
    /// is recorded PROVEN on what comes back (176d5c9d, 2026-10-10T04:00Z,
    /// by a request filed before any refusal existed). So the unattended
    /// door refuses it too, while the hand door keeps the warning. The
    /// spelling with no kind named is car 59e0d3dd's.
    #[test]
    fn the_unattended_door_refuses_a_probe_that_clears_a_caller_from_the_gate_log_alone() {
        let named = r#"n=$(boss-sor-read "/api/events/gate-window?gate=machine-gate&hours=$hours" | jq -r --arg cut "$cut" '[.log.dirty[] | select(.kind == "machine_gate.would_refuse" and .payload.key.user == "automation:estate-observer" and .at > $cut)] | length')"#;
        let unnamed = r#"w=$(boss-sor-read '/api/events/gate-window?gate=machine-gate&hours=168'); n=$(printf '%s' "$w" | jq '[.log.dirty[] | select(.payload.key.user == "automation:probe-admission-source-access")] | length')"#;
        for probe in [named, unnamed] {
            assert!(
                admit(probe, true).refusal.is_none(),
                "the hand door warns and runs: {probe}"
            );
            let r = unattended_refusal("c0ffee12", probe)
                .unwrap_or_else(|| panic!("the unattended door must refuse: {probe}"));
            assert_eq!(r.rule, boss_jobs::probe::GATE_LOG_ALONE_RULE);
            assert!(r.text.contains("snapshot.rows"), "{r}");
            assert!(
                r.text
                    .contains(&boss_jobs::probe::hand_proof_command("c0ffee12")),
                "the refusal names the act that moves a landed car: {r}"
            );
            // F3 of review 446039a0: a presence claim has the hand door.
            assert!(
                r.text
                    .contains(&boss_jobs::probe::from_car_command("c0ffee12")),
                "{r}"
            );
        }
        // The two rules every door refuses still come first, unchanged.
        let unidentified = "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee";
        assert_eq!(
            unattended_refusal("c0ffee12", unidentified).map(|r| r.rule),
            Some(UNIDENTIFIED_RULE)
        );
        // And the probe that also reads the live rows runs.
        let both = format!(
            "{named}\nlive=$(boss-sor-read '/api/events/gate-window?gate=machine-gate&hours=1' | jq --arg cut \"$cut\" '[.live[] | select(.snapshot.mode != \"off\" and .snapshot.overflow == 0 and .snapshot.recording_since <= $cut) | .snapshot.rows[]] | length')"
        );
        assert!(unattended_refusal("c0ffee12", &both).is_none());
        // And the live rows read ALONE, the tally's start never ordered
        // against the cut (backlog 002f5f31), are refused like the log.
        let live_only = "live=$(boss-sor-read '/api/events/gate-window?gate=machine-gate&hours=1' | jq --arg cut \"$cut\" '[.live[] | select(.snapshot.mode != \"off\" and .snapshot.overflow == 0) | .snapshot.rows[] | select(.user == \"automation:estate-observer\" and .presented == \"none\")] | length')";
        let r = unattended_refusal("c0ffee12", live_only)
            .unwrap_or_else(|| panic!("the unattended door must refuse: {live_only}"));
        assert_eq!(r.rule, boss_jobs::probe::GATE_LOG_ALONE_RULE);
        assert!(r.text.contains(boss_jobs::probe::LIVE_ROWS_ALONE), "{r}");
        assert!(!r.text.contains("`.log.dirty`"), "{r}");
    }

    /// THE COMMANDS A REFUSAL PRINTS ARE COMMANDS THIS VERB TAKES (review
    /// 446039a0 F1). The unattended refusal printed `boss prove <car>
    /// --probe … --expect …`, and this verb refuses that line for want of
    /// `--verified` — so the act it named was itself refused. Both
    /// spellings every door prints are parsed here by the CLI's own
    /// parser, and the hand proof carries every value `run` requires.
    #[test]
    fn the_hand_proofs_a_refusal_names_parse_as_this_verb() {
        use clap::Parser;
        // Words as the shell would split them: a single-quoted run is one.
        let argv = |line: String| -> Vec<String> {
            line.split('\'')
                .enumerate()
                .flat_map(|(i, part)| {
                    if i % 2 == 1 {
                        vec![part.to_string()]
                    } else {
                        part.split_whitespace().map(str::to_string).collect()
                    }
                })
                .collect()
        };
        let by_hand =
            crate::Cli::try_parse_from(argv(boss_jobs::probe::hand_proof_command("c0ffee12")))
                .unwrap_or_else(|e| panic!("{e}"));
        let crate::Commands::Prove {
            car,
            probe,
            expect,
            verified,
            from_car,
            ..
        } = by_hand.command
        else {
            panic!("not a prove");
        };
        assert_eq!(car, "c0ffee12");
        assert!(!from_car);
        assert!(
            probe.is_some() && expect.is_some() && verified.is_some(),
            "run() refuses a hand proof missing any of the three"
        );
        let from = crate::Cli::try_parse_from(argv(boss_jobs::probe::from_car_command("c0ffee12")))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            from.command,
            crate::Commands::Prove { from_car: true, .. }
        ));
    }

    /// A MENTION IS NOT A DEFINITION, SAID AT THE SAME DOOR (e7cf78c6):
    /// the bare name matched the doc comment, the count came back
    /// nonzero, and nothing was defined.
    #[test]
    fn a_probe_that_greps_a_bare_name_is_warned_about() {
        let probe = "c=$(git show HEAD:crates/core/boss-jobs/src/claims.rs \
                     | grep -c in_flight_claims); \
                     [ \"$c\" -ge 1 ] && echo claim:ok";
        let w: Vec<String> = shape_warnings(probe).collect();
        let said = w
            .iter()
            .find(|w| w.contains("COUNTS A NAME"))
            .unwrap_or_else(|| panic!("{w:?}"));
        assert!(said.contains("in_flight_claims"), "{said}");
        assert!(
            said.contains(boss_jobs::probe::MENTION_NOT_DEFINITION_EVIDENCE),
            "{said}"
        );
        assert!(said.contains("warning, not a refusal"), "{said}");
        // The quoted definition it names is not warned about in turn —
        // the negative case that keeps this from warning about every
        // grep there is.
        let fixed = "c=$(git show HEAD:crates/core/boss-jobs/src/claims.rs \
                     | grep -c 'pub fn in_flight_claims'); \
                     [ \"$c\" -ge 1 ] && echo claim:ok";
        assert!(
            !shape_warnings(fixed).any(|w| w.contains("COUNTS A NAME")),
            "{:?}",
            shape_warnings(fixed).collect::<Vec<_>>()
        );
    }

    /// AND THE STRIP IS REAL. The door's `env_remove` cannot be shown
    /// from inside one process without mutating its environment (racy
    /// under the parallel runner), so the mechanism is proven through a
    /// wrapper: this test's own binary re-run under `BOSS_ACTOR=leak`
    /// executes the probe through the door and prints what the probe
    /// saw. The unattended shell names both spellings of the actor.
    #[test]
    fn a_stripped_name_does_not_reach_the_probe() {
        if std::env::var("BOSS_PROVE_STRIP_INNER").is_ok() {
            // Inner leg: the parent carries BOSS_ACTOR=leak.
            let shell = Shell {
                strip: vec!["BOSS_ACTOR".into()],
                ..Shell::here(None)
            };
            let o = execute_with("printf '%s' \"${BOSS_ACTOR:-unset}\"", &shell).unwrap();
            println!("STRIP-SAW={}", o.stdout);
            return;
        }
        let me = std::env::current_exe().unwrap();
        let out = std::process::Command::new(me)
            .args([
                "--exact",
                "prove::tests::a_stripped_name_does_not_reach_the_probe",
                "--nocapture",
            ])
            .env("BOSS_ACTOR", "leak")
            .env("BOSS_PROVE_STRIP_INNER", "1")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            printed.contains("STRIP-SAW=unset"),
            "the strip must remove it: {printed}"
        );
        let shell = Shell::unattended().unwrap();
        for name in ["BOSS_ACTOR", "BOSS_ACTOR_FILE"] {
            assert!(shell.strip.contains(&name.to_string()), "{:?}", shell.strip);
        }
    }

    /// The names a door sets on a probe by name, after any allow-list —
    /// shared by every test that reads a probe's whole environment back.
    const DOOR_SET: [&str; 8] = [
        "BOSS_JOBS_URL",
        "BOSS_SOR_USER",
        "BOSS_SOR_PORTS",
        "BOSS_PROBE_NOTFOUND",
        crate::probe_reader::DOOR_ENV,
        boss_core::machine_token::TOKEN_DIR_ENV,
        boss_jobs::probe::CAR_MERGE_REF_VAR,
        boss_jobs::probe::CAR_CONVERGED_AT_VAR,
    ];
    /// What bash exports of its own.
    const BASH_OWN: [&str; 4] = ["PWD", "OLDPWD", "SHLVL", "_"];

    /// A HOST-FREE RECHECK CANNOT SEE THE OPERATOR'S GIT ENVIRONMENT
    /// (backlog 51587416, from review 2c318e8f's rework of da937eb3).
    /// The git-object classifier admits a probe by its TEXT, and the text
    /// was clean; the shell it ran in was the operator's whole
    /// environment, so a `GIT_DIR`, a `GIT_CONFIG_*` or an exported
    /// function named `git` changed what clean text read. Measured on the
    /// pod before this: `boss prove afd61aa2 --recheck` answered HOLDS,
    /// and NOT YET with `git` exported as a function, in the same
    /// AnyCheckout placement. Proven by effect, as the allow-list is:
    /// this binary re-run with all three planted, the probe, the witness
    /// that places it and the instant it is handed each read at both
    /// shells. The hand door's shell must still see the plant — or the
    /// plant proves nothing.
    #[test]
    fn a_host_free_recheck_cannot_see_the_operators_git_environment() {
        const INNER: &str = "BOSS_PROVE_HOST_FREE_INNER";
        let probe = "printf 'SHOW=[%s] FN=[%s] ABBREV=[%s] AT=[%s]\\n' \
                     \"$(git show HEAD:f 2>&1)\" \"$(type -t git)\" \
                     \"$(git rev-parse --short HEAD 2>&1)\" \"${BOSS_CAR_CONVERGED_AT-}\"; \
                     printf 'NAMES=%s\\n' \"$(compgen -e | sort | tr '\\n' ' ')\"";
        if let Ok(repo) = std::env::var(INNER) {
            let repo = PathBuf::from(repo);
            let sha = std::env::var("BOSS_PROVE_HOST_FREE_SHA").unwrap();
            for (door, shell) in [
                ("here", Shell::here(Some(&repo))),
                ("host-free", Shell::here(Some(&repo)).host_free()),
            ] {
                let witness = checkout_holding(Some(&sha), &shell).unwrap_or_default();
                let shell = shell
                    .with_probe_reader(None, "http://sor.invalid:7900")
                    .with_car_instant(Some(&sha));
                let o = execute_with(probe, &shell).unwrap();
                for line in o.stdout.lines() {
                    println!("{door}: WITNESS=[{witness}] {line}");
                }
            }
            return;
        }
        let dir = boss_testing::scratch::scratch_dir("prove-host-free");
        let repo = dir.join("checkout");
        let decoy = dir.join("decoy.git");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git")
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .expect("git runs");
            assert!(o.status.success(), "git {args:?}: {o:?}");
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let at = repo.display().to_string();
        git(&["-C", &at, "init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f"), "the-checkout-holds-this").unwrap();
        git(&["-C", &at, "add", "f"]);
        git(&["-C", &at, "commit", "-q", "-m", "landed"]);
        let sha = git(&["-C", &at, "rev-parse", "HEAD"]);
        let top = git(&["-C", &at, "rev-parse", "--show-toplevel"]);
        git(&["init", "-q", "--bare", &decoy.display().to_string()]);

        let me = std::env::current_exe().unwrap();
        let out = std::process::Command::new(me)
            .args([
                "--exact",
                "prove::tests::a_host_free_recheck_cannot_see_the_operators_git_environment",
                "--nocapture",
            ])
            .env(INNER, &repo)
            .env("BOSS_PROVE_HOST_FREE_SHA", &sha)
            // The three plants the packet names: another repository, a
            // command-line config, and a function shadowing the program.
            .env("GIT_DIR", &decoy)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.abbrev")
            .env("GIT_CONFIG_VALUE_0", "40")
            .env("BASH_FUNC_git%%", "() {  echo PLANTED-FUNCTION\n}")
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = |door: &str, key: &str| -> String {
            printed
                .lines()
                .filter_map(|l| l.strip_prefix(&format!("{door}: ")))
                .find_map(|l| {
                    l.split_once(&format!("{key}="))
                        .map(|(_, rest)| rest.to_string())
                })
                .unwrap_or_else(|| panic!("no {door} {key}: {printed}"))
        };
        // The control: the hand door's shell sees every plant.
        let here = line("here", "SHOW");
        assert!(here.starts_with("[PLANTED-FUNCTION]"), "{printed}");
        assert!(here.contains("FN=[function]"), "{printed}");
        assert!(line("here", "WITNESS").starts_with("[]"), "{printed}");
        assert!(line("here", "NAMES").contains("GIT_DIR"), "{printed}");

        // The host-free shell sees none of them, and reads the checkout.
        let free = line("host-free", "SHOW");
        assert!(
            free.starts_with("[the-checkout-holds-this] FN=[file] "),
            "{printed}"
        );
        let abbrev = free
            .split_once("ABBREV=[")
            .and_then(|(_, r)| r.split_once(']'))
            .map(|(a, _)| a.to_string())
            .unwrap();
        assert!(
            sha.starts_with(&abbrev) && abbrev.len() < sha.len(),
            "core.abbrev=40 from GIT_CONFIG_* must not reach it: {printed}"
        );
        assert!(free.contains("AT=[1700000000]"), "{printed}");
        assert!(
            line("host-free", "WITNESS").starts_with(&format!("[{top}]")),
            "the witness reads the checkout the probe reads: {printed}"
        );
        let names: Vec<String> = line("host-free", "NAMES")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let git_pins: Vec<&str> = HOST_FREE_GIT.iter().map(|(k, _)| *k).collect();
        for name in &names {
            assert!(
                UNATTENDED_INHERITS.contains(&name.as_str())
                    || git_pins.contains(&name.as_str())
                    || DOOR_SET.contains(&name.as_str())
                    || BASH_OWN.contains(&name.as_str()),
                "the host-free probe was handed {name}: {names:?}"
            );
        }
        // What it legitimately reads still reaches it.
        for name in [
            "PATH",
            "BOSS_JOBS_URL",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            boss_jobs::probe::CAR_CONVERGED_AT_VAR,
            boss_core::machine_token::TOKEN_DIR_ENV,
        ] {
            assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
        }
    }

    /// The two placements that stand on a probe's text alone run host
    /// free, from the one allow-list the unattended door uses; a
    /// same-host recheck keeps the operator's shell, in the recorded
    /// directory (backlog 51587416).
    #[test]
    fn only_the_host_free_placements_run_host_free() {
        let free = |s: &Shell| {
            s.inherit == Some(&UNATTENDED_INHERITS[..])
                && HOST_FREE_GIT
                    .iter()
                    .all(|(k, v)| s.env.contains(&(k.to_string(), v.to_string())))
        };
        let any = recheck_shell(
            &Rerunnable::AnyCheckout { root: "/r".into() },
            Some("/gone"),
        );
        assert!(free(&any), "{any:?}");
        assert_eq!(any.cwd.as_deref(), Some(Path::new("/r")));
        let anywhere = recheck_shell(&Rerunnable::Anywhere, Some("/gone"));
        assert!(free(&anywhere), "{anywhere:?}");
        assert_eq!(anywhere.cwd, None);
        let here = recheck_shell(&Rerunnable::Here, Some("/recorded"));
        assert_eq!(here, Shell::here(Some(Path::new("/recorded"))));
        assert_eq!(recheck_shell(&Rerunnable::Here, Some("")).cwd, None);
    }

    /// THE UNATTENDED PROBE STARTS FROM AN ALLOW-LIST (backlog dda26693,
    /// review acab446c F8b, the environment half). The forge's ops runner
    /// carries the account's own names, the reader's, the host's id, the
    /// request's, every address in /etc/boss/sor.env and systemd's, and a
    /// strip list removed only the ones somebody had thought of. Proven
    /// by effect, as the strip is: this binary re-run with a runner-shaped
    /// environment, the unattended door's probe listing every exported
    /// name it was handed, and the hand door's beside it — which is the
    /// operator's own shell and keeps everything.
    #[test]
    fn the_unattended_probe_inherits_only_the_allow_list() {
        const INNER: &str = "BOSS_PROVE_ALLOW_INNER";
        // Names the forge's runner holds that no probe is promised, read
        // from the units and files that set them (infra/ops/
        // boss-ops-runner.service, install-ops-runner.sh's drop-in,
        // probe-account.sh's drop-in, render-sor-env.sh, ops-runner.sh's
        // verb launch, systemd's own).
        let runner: [(&str, &str); 10] = [
            (
                PROBE_VERIFIED_FILE_ENV,
                "/run/boss-ops-runner/probe-verified",
            ),
            ("BOSS_PROBE_USER", "boss-probe"),
            (
                "BOSS_PROBE_READER_ACTOR",
                "a-reader-of-the-runners-choosing",
            ),
            ("BOSS_PROBE_VIEW_SOURCE", "/var/lib/boss/tree/current"),
            ("HOST_ID", "forge"),
            ("OPS_REQUEST_ID", "00000000-0000-0000-0000-000000000000"),
            ("JOBS_API", "http://sor.invalid:7900"),
            ("BOSS_FORGE_URL", "http://forge.invalid:3000"),
            ("RUNTIME_DIRECTORY", "/run/boss-ops-runner"),
            ("INVOCATION_ID", "0123456789abcdef"),
        ];
        let list = "compgen -e | sort | tr '\\n' ' '";
        if std::env::var(INNER).is_ok() {
            let scratch = boss_testing::scratch::scratch_dir("prove-allow-list");
            let unattended = Shell {
                user: None,
                cwd: Some(scratch),
                ..unattended_answered("http://sor.invalid:7900")
            };
            for (door, shell) in [("here", Shell::here(None)), ("unattended", unattended)] {
                let o = execute_with(list, &shell).unwrap();
                println!("{door}: {}", o.stdout.trim());
            }
            return;
        }
        let me = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(me);
        cmd.args([
            "--exact",
            "prove::tests::the_unattended_probe_inherits_only_the_allow_list",
            "--nocapture",
        ])
        .env(INNER, "1")
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC");
        for (k, v) in runner {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        let printed = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let names = |door: &str| -> Vec<String> {
            printed
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{door}: ")))
                .unwrap_or_else(|| panic!("no {door} line: {printed}"))
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        let unattended = names("unattended");
        // What the door itself sets, what bash exports of its own, and
        // the allow-list: nothing else.
        for name in &unattended {
            assert!(
                UNATTENDED_INHERITS.contains(&name.as_str())
                    || DOOR_SET.contains(&name.as_str())
                    || BASH_OWN.contains(&name.as_str()),
                "the unattended probe was handed {name}, which is neither the door's own nor \
                 on the allow-list: {unattended:?}"
            );
        }
        for name in ["PATH", "LANG", "TZ", "BOSS_JOBS_URL", "BOSS_SOR_USER"] {
            assert!(
                unattended.iter().any(|n| n == name),
                "{name} must reach it: {unattended:?}"
            );
        }
        // The hand door is the operator's shell, as it was: it keeps the
        // runner-shaped names this door drops.
        let here = names("here");
        for (k, _) in runner {
            assert!(
                here.iter().any(|n| n == k),
                "{k} at the hand door: {here:?}"
            );
        }
    }

    /// THE LAUNCH WORDS RESOLVE ON THE RUNNER'S PATH, NOT THE TREE'S
    /// (review 717a7f7c N1 of car f5d53af7). `probe-bin` was put first on
    /// the PATH of the Command itself, so `timeout`, and through it
    /// `runuser`, `setpriv` and `bash`, were looked up there first: a file
    /// named `timeout` in the converged tree's `infra/forge/probe-bin` ran
    /// as root at the forge, ahead of the drop to the account, while
    /// verify-tick had checked the launch on the unit's own PATH. The
    /// prefix now reaches the probe inside `bash -c`, where it is the
    /// account's lookup and nobody else's.
    #[test]
    fn the_probe_bin_prefix_does_not_resolve_the_launch_words() {
        let dir = boss_testing::scratch::scratch_dir("prove-launch-words");
        let bin = dir.join("pbin");
        std::fs::create_dir_all(&bin).unwrap();
        boss_testing::write_exec(&bin.join("timeout"), "#!/bin/sh\necho HIJACKED\n");
        boss_testing::write_exec(&bin.join("tool-on-prefix"), "#!/bin/sh\necho prefix:ok\n");
        let shell = Shell {
            cwd: Some(dir.clone()),
            timeout_secs: Some(30),
            path_prefix: Some(bin),
            ..Shell::here(None)
        };
        let o = execute_with("tool-on-prefix", &shell).expect("bash runs");
        assert!(!o.stdout.contains("HIJACKED"), "{o:?}");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(o.stdout.contains("prefix:ok"), "{o:?}");
    }

    /// NO MACHINE TOKEN REACHES A PROBE, AT EITHER DOOR (design 6805c764
    /// car 4, review ef2da426 F1; backlog 844b936e). A probe is
    /// car-written text builders rehearse on the dev pod, and a handler
    /// test it runs would stamp the token onto a mock and print it into
    /// the record. Until 844b936e both doors STRIPPED the directory's
    /// name, and a stripped name falls back to boss-core's default —
    /// where a host that mounts the Secret there keeps the token — so
    /// the old version of this test asserted `unset` and called it safe.
    /// Proven by effect: this binary re-run with the doors' directory
    /// holding a live-looking token, and, separately, with no name at all
    /// (the conductor's shape); each door's probe must see a NAMED
    /// directory that is neither, and no token in it. The unattended
    /// door runs here as itself in a scratch dir — its user and checkout
    /// are the forge's — with every other piece its constructor builds.
    #[test]
    fn no_machine_token_reaches_a_probe_at_either_door() {
        let var = boss_core::machine_token::TOKEN_DIR_ENV;
        const INNER: &str = "BOSS_PROVE_TOKEN_DIR_INNER";
        let probe = format!(
            "d=\"${{{var}:-unset}}\"; t=absent; [ -e \"$d/current\" ] && t=present; \
             printf 'DIR=%s TOKEN=%s URL=[%s]\\n' \"$d\" \"$t\" \"${{BOSS_JOBS_URL:-}}\""
        );
        if std::env::var(INNER).is_ok() {
            let scratch = boss_testing::scratch::scratch_dir("prove-no-token-unattended");
            let unattended = Shell {
                user: None,
                cwd: Some(scratch),
                ..unattended_answered("http://sor.invalid:7900")
            };
            for (door, shell) in [("here", Shell::here(None)), ("unattended", unattended)] {
                let o = execute_with(&probe, &shell).unwrap();
                println!("{door}: {}", o.stdout.trim());
            }
            return;
        }
        let held = boss_testing::scratch::scratch_dir("prove-held-token");
        let doors = held.join("machine-token");
        std::fs::create_dir_all(&doors).unwrap();
        std::fs::write(doors.join("current"), "x".repeat(43)).unwrap();
        for parent in [Some(doors.as_path()), None] {
            let me = std::env::current_exe().unwrap();
            let mut cmd = std::process::Command::new(me);
            cmd.args([
                "--exact",
                "prove::tests::no_machine_token_reaches_a_probe_at_either_door",
                "--nocapture",
            ])
            .env(INNER, "1");
            match parent {
                Some(d) => cmd.env(var, d),
                None => cmd.env_remove(var),
            };
            let out = cmd.output().unwrap();
            let printed = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success(),
                "{printed}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            for door in ["here", "unattended"] {
                let line = printed
                    .lines()
                    .find(|l| l.starts_with(&format!("{door}: ")))
                    .unwrap_or_else(|| panic!("no {door} line (parent {parent:?}): {printed}"));
                for wrong in [
                    "unset",
                    boss_core::machine_token::DEFAULT_TOKEN_DIR,
                    &doors.display().to_string(),
                ] {
                    assert!(
                        !line.contains(&format!("DIR={wrong} ")),
                        "the {door} door's probe must see a named empty dir (parent {parent:?}): \
                         {line}"
                    );
                }
                assert!(line.contains("TOKEN=absent"), "{line}");
            }
            // The unattended door still hands the address it resolved.
            assert!(
                printed.contains("URL=[http://sor.invalid:7900]"),
                "{printed}"
            );
        }
    }

    /// THE PORT TABLE IS READ AS DATA: `name=port` lines become one
    /// space-separated table; comments, blanks and padding are not in
    /// it. The twin's `grep -Ev | tr -s | sed` extraction, in Rust.
    #[test]
    fn the_port_table_is_read_as_data() {
        assert_eq!(
            sor_ports_table("# a comment\n\njobs=7900\nevents=7150\n  people=7500  \n"),
            "jobs=7900 events=7150 people=7500"
        );
        assert_eq!(sor_ports_table(""), "");
        assert_eq!(sor_ports_table("# only\n# comments\n"), "");
        // The tree's own table parses to something a reader can route
        // by, and every entry is name=port.
        let tree =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/sor-ports.env"))
                .expect("the port table ships in the tree");
        let table = sor_ports_table(&tree);
        assert!(table.contains("jobs=7900"), "{table}");
        for entry in table.split(' ') {
            let (name, port) = entry.split_once('=').unwrap_or_else(|| panic!("{entry}"));
            assert!(!name.is_empty());
            port.parse::<u16>()
                .unwrap_or_else(|e| panic!("{entry}: {e}"));
        }
    }

    /// THE FACT THAT LIVES TWICE GETS AN EQUALITY TEST (CLAUDE.md §9a).
    /// The reader's role is named here and DEFINED in core policy's
    /// defaults; they cannot be collapsed, so they are pinned equal.
    /// `audit-readonly` must grant Read at Scope::All on what a probe
    /// reads — and no non-Read action anywhere, which is what makes
    /// handing it to program text a builder wrote safe (61085a9e).
    #[test]
    fn the_probes_reader_role_can_read_everything_and_write_nothing() {
        use boss_policy_client::{Action, Resource, Scope};

        let rules = boss_policy_client::defaults::default_rules();
        let mine: Vec<_> = rules.iter().filter(|r| r.role == READER_ROLE).collect();
        assert!(
            !mine.is_empty(),
            "the probe's reader role '{READER_ROLE}' is not seeded by core policy at all — \
             an unseeded role reads NOTHING, which is the defect with extra steps"
        );
        for resource in [Resource::job(), Resource::step(), Resource::event()] {
            assert!(
                mine.iter().any(|r| r.resource == resource
                    && r.action == Action::Read
                    && r.scope == Scope::All),
                "'{READER_ROLE}' has no Read/All on {resource:?} — a probe carrying it would \
                 see a narrower world than the operator, which is what 61085a9e measured"
            );
        }
        for rule in &mine {
            assert_eq!(
                rule.action,
                Action::Read,
                "'{READER_ROLE}' carries a non-Read grant ({:?} on {:?}) — it is handed to \
                 program text a builder wrote, so it must not be able to change anything",
                rule.action,
                rule.resource
            );
        }
    }

    /// THE ADMISSION, in the twin's order, each refusal changing
    /// nothing: kind, merged, probe (event-bound named), expect, step,
    /// status — and a completed `proven` is "nothing to run", because
    /// the daily recheck re-files for a car proven by hand meanwhile.
    #[test]
    fn the_unattended_door_admits_only_a_merged_probed_car_with_proven_open() {
        let car = |kind: &str, merged: Value, md: Value, status: &str| {
            let mut m = md;
            m["merged"] = merged;
            json!({
                "id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": kind,
                "metadata": m,
                "steps": [{"id": "s-proven", "spec_slug": "proven", "title": PROVEN, "status": status}]
            })
        };
        let probed = json!({"proof_probe": "true && echo x:ok", "proof_expect": "x:ok", "summary": "the claim"});
        match admit_unattended(&car(
            "ship-a-change",
            json!("true"),
            probed.clone(),
            "ready",
        )) {
            Admitted::Run {
                probe,
                expect,
                step_id,
                verified,
            } => {
                assert_eq!(probe, "true && echo x:ok");
                assert_eq!(expect, "x:ok");
                assert_eq!(step_id, "s-proven");
                assert_eq!(verified, "the claim");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            admit_unattended(&car(
                "ship-a-change",
                json!("true"),
                probed.clone(),
                "completed"
            )),
            Admitted::AlreadyProven
        );
        let refused = |c: &Value| match admit_unattended(c) {
            Admitted::Refused(why) => why,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(
            refused(&car("gate-run", json!("true"), probed.clone(), "ready"))
                .contains("is a gate-run, not a ship-a-change car")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("false"),
                probed.clone(),
                "ready"
            ))
            .contains("has not merged")
        );
        assert!(
            refused(&car("ship-a-change", Value::Null, probed.clone(), "ready"))
                .contains("has not merged")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                json!({"proof_event": "the next red train"}),
                "ready"
            ))
            .contains("EVENT-BOUND")
        );
        assert!(
            refused(&car("ship-a-change", json!("true"), json!({}), "ready"))
                .contains("recorded no `proof_probe`")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                json!({"proof_probe": "true"}),
                "ready"
            ))
            .contains("echo hi")
        );
        assert!(
            refused(&car(
                "ship-a-change",
                json!("true"),
                probed.clone(),
                "pending"
            ))
            .contains("proven step is \"pending\"")
        );
        let no_step = json!({"id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": "ship-a-change",
            "metadata": {"merged": "true", "proof_probe": "true", "proof_expect": "x"}, "steps": []});
        assert!(refused(&no_step).contains("has no proven step"));
        // A car with no summary is still run, with the twin's default
        // prose; `verified` stays required on the step.
        let bare = json!({"proof_probe": "true && echo x:ok", "proof_expect": "x:ok"});
        match admit_unattended(&car("ship-a-change", json!("true"), bare, "active")) {
            Admitted::Run { verified, .. } => {
                assert_eq!(
                    verified,
                    "proven by the probe the car recorded at park time"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    /// AN OWED PROOF IS PAID ON A CLOSED CAR (backlog b9005734, car 2).
    /// The obligation (car 1, rule `rerun-owed-proofs-on-job-closed`)
    /// files `run-car-probe` for a car that closed LANDED with its proof
    /// owed — closed, `proven` skipped by the landing, `proof_owed =
    /// "true"` — and until this car the door it files to refused exactly
    /// that car ("proven step is skipped"), so the obligation fired and
    /// nothing ran. Such a car is now run; a skipped `proven` on a car
    /// that owes NOTHING (abandoned, disproved, or already paid) is still
    /// refused, because the marker, not the step, is what is owed.
    #[test]
    fn the_unattended_door_runs_an_owed_proof_on_a_closed_car() {
        let owed = |marker: Value, proven: &str| {
            let mut m = json!({"merged": "true", "proof_probe": "true && echo x:ok",
                               "proof_expect": "x:ok", "summary": "the claim"});
            m[boss_jobs::car::PROOF_OWED] = marker;
            json!({
                "id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": "ship-a-change",
                "status": "closed", "metadata": m,
                "steps": [{"id": "s-proven", "spec_slug": "proven", "title": PROVEN, "status": proven}]
            })
        };
        match admit_unattended(&owed(json!("true"), "skipped")) {
            Admitted::Owed {
                probe,
                expect,
                verified,
            } => {
                assert_eq!(probe, "true && echo x:ok");
                assert_eq!(expect, "x:ok");
                assert_eq!(verified, "the claim");
            }
            other => panic!("an owed, closed car is run: {other:?}"),
        }
        for marker in [Value::Null, json!(true)] {
            match admit_unattended(&owed(marker.clone(), "skipped")) {
                Admitted::Refused(why) => assert!(why.contains("skipped"), "{why}"),
                other => panic!("{marker:?} owes nothing, so skipped is refused: {other:?}"),
            }
        }
        // A PAID debt answers as a proven step does — nothing to run,
        // exit 0 — never "skipped = abandoned" (review 9daaa7a9 F1).
        assert!(matches!(
            admit_unattended(&owed(json!("paid"), "skipped")),
            Admitted::AlreadyProven
        ));
        // THE MARKER ALONE ADMITS NOTHING (review 9daaa7a9 F2): a car
        // still carrying `proof_owed = "true"` that was disproved,
        // abandoned, unlanded (it keeps `merged = "true"`, car.rs
        // `is_landed`), or is not closed at all, is refused.
        let mark = |key: &str, value: Value| {
            let mut car = owed(json!("true"), "skipped");
            car["metadata"][key] = value;
            car
        };
        let mut open = owed(json!("true"), "skipped");
        open["status"] = json!("open");
        for (why_refused, car) in [
            ("disproved", mark("disproved", json!("true"))),
            ("abandoned", mark("abandoned", json!("true"))),
            (
                "unlanded",
                mark(boss_jobs::car_unland::UNLANDED_MARKER, json!("true")),
            ),
            ("not closed", open),
        ] {
            match admit_unattended(&car) {
                Admitted::Refused(why) => assert!(why.contains("skipped"), "{why}"),
                other => panic!("a {why_refused} car owes nothing to run: {other:?}"),
            }
        }
        // A car marked owed while still OPEN at `proven` takes the
        // ordinary road: the step is there to complete.
        assert!(matches!(
            admit_unattended(&owed(json!("true"), "ready")),
            Admitted::Run { .. }
        ));
    }

    /// THE SMALL FINDINGS OF BACKLOG 82946cd9 (reviews 9daaa7a9 and
    /// e7bf6b14 on car c8ecf66a), each a wrong answer from the door:
    /// (1) an owed car the standing guard refused was told "skipped =
    /// abandoned" whatever the cause — an unlanded or disproved car read
    /// as abandoned; (2) a car paid and LATER disproved answered "already
    /// proven", exit 0, about a claim its own record now calls false;
    /// (3) the disproved / abandoned markers were read as the STRING
    /// "true" only, while `is_landed` reads a bool `true` as the same
    /// mark, so a bool-disproved owed car was run.
    #[test]
    fn an_owed_car_the_door_refuses_is_told_its_own_cause() {
        let car = |marker: &str, set: &[(&str, Value)]| {
            let mut m = json!({"merged": "true", "proof_probe": "true && echo x:ok",
                               "proof_expect": "x:ok", "summary": "the claim"});
            m[boss_jobs::car::PROOF_OWED] = json!(marker);
            for (k, v) in set {
                m[*k] = v.clone();
            }
            json!({
                "id": "aaaaaaaa-0000-4000-8000-000000000000", "kind": "ship-a-change",
                "status": "closed", "metadata": m,
                "steps": [{"id": "s-proven", "spec_slug": "proven", "title": PROVEN, "status": "skipped"}]
            })
        };
        let refused = |c: &Value| match admit_unattended(c) {
            Admitted::Refused(why) => why,
            other => panic!("refused, not {other:?}"),
        };
        // (1) each cause by its own name, never another's.
        let unlanded = refused(&car(
            "true",
            &[(boss_jobs::car_unland::UNLANDED_MARKER, json!("true"))],
        ));
        assert!(unlanded.contains("unlanded"), "{unlanded}");
        assert!(!unlanded.contains("abandoned"), "{unlanded}");
        let disproved = refused(&car("true", &[("disproved", json!("true"))]));
        assert!(disproved.contains("disproved"), "{disproved}");
        assert!(!disproved.contains("abandoned"), "{disproved}");
        let mut open = car("true", &[]);
        open["status"] = json!("open");
        let open = refused(&open);
        assert!(open.contains("not closed"), "{open}");
        assert!(!open.contains("abandoned"), "{open}");
        let abandoned = refused(&car("true", &[("abandoned", json!("true"))]));
        assert!(abandoned.contains("abandoned"), "{abandoned}");
        // (2) paid, then disproved: the payment no longer stands.
        let paid_then_disproved = refused(&car("paid", &[("disproved", json!("true"))]));
        assert!(
            paid_then_disproved.contains("disproved"),
            "{paid_then_disproved}"
        );
        // (3) a bool mark is the same mark.
        for key in ["disproved", "abandoned"] {
            let why = refused(&car("true", &[(key, json!(true))]));
            assert!(why.contains(key), "{why}");
        }
        // CONTROL: an explicit false marks nothing, so the debt is run.
        assert!(matches!(
            admit_unattended(&car("true", &[("disproved", json!(false))])),
            Admitted::Owed { .. }
        ));
    }

    /// PAYING IT: the proof lands on the CAR, never on its skipped step
    /// (a closed packet's steps are history), and the marker turns to
    /// `paid`, so the obligation stops asking and a reader sees what paid
    /// it and when.
    #[test]
    fn an_owed_proof_is_paid_by_the_marker_and_the_proof_on_the_car() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let patch = owed_paid_patch("the claim", "{\"exit\":0}", now);
        assert_eq!(patch[boss_jobs::car::PROOF_OWED], "paid");
        let paid = &patch[OWED_PROOF];
        assert_eq!(paid["verified"], "the claim");
        assert_eq!(paid["proof"], "{\"exit\":0}");
        assert_eq!(paid["proven_by"], PROVEN_BY);
        assert!(paid["completed_at"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(!boss_jobs::car::owes_proof(&patch));
    }

    /// THE CREDENTIAL THE PAYMENT PRESENTS (backlog 82946cd9): what a
    /// read of this host's runner credential file makes a header of —
    /// the value as ops-runner.sh's `$(cat …)` reads it (trailing
    /// newlines dropped), marked sensitive so no Debug print carries it —
    /// and why not otherwise, never naming the value.
    #[test]
    fn the_runner_credential_is_one_sensitive_header_or_a_reason() {
        let h = runner_credential_header(Ok("fixture-credential\n\n".into())).unwrap();
        assert_eq!(h.to_str().unwrap(), "fixture-credential");
        assert!(h.is_sensitive());
        assert!(!format!("{h:?}").contains("fixture-credential"));

        let empty = runner_credential_header(Ok("\n".into())).unwrap_err();
        assert!(empty.contains("empty"), "{empty}");
        let two = runner_credential_header(Ok("secret-one\nsecret-two\n".into())).unwrap_err();
        assert!(two.contains("not one header value"), "{two}");
        assert!(
            !two.contains("secret"),
            "a reason never carries the value: {two}"
        );
        let denied = runner_credential_header(Err(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )))
        .unwrap_err();
        assert!(denied.contains("reading it failed"), "{denied}");
    }

    /// The file the payment reads its credential from IS the runner's:
    /// the variable and default infra/ops/ops-runner.sh reads, so the
    /// verb it runs presents what the runner itself presents (§9a: a
    /// fact that lives twice gets an equality test).
    #[test]
    fn the_payment_reads_the_runners_own_credential_file() {
        let runner =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/ops/ops-runner.sh"))
                .unwrap();
        let line = format!(
            "RUNNER_CREDENTIAL_FILE=\"${{{RUNNER_CREDENTIAL_FILE_ENV}:-{RUNNER_CREDENTIAL_FILE_DEFAULT}}}\""
        );
        assert!(
            runner.lines().any(|l| l.trim() == line),
            "ops-runner.sh must read the credential as `{line}`"
        );
    }

    /// WHERE THE CREDENTIAL RIDES, AND WHEN IT IS READ (backlog
    /// 82946cd9). Only the owed payment presents it — the one write the
    /// jobs API reserves to the runner — and it is read only after the
    /// probe has run, so the builder-authored probe never runs in a
    /// process that holds it. A source pin, because the order is the
    /// property and run_unattended has no seam short of a forge host.
    #[test]
    fn only_the_owed_payment_presents_the_runner_credential_and_after_the_probe() {
        let prod = include_str!("prove.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        let door = prod
            .split("pub(crate) async fn run_unattended(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("run_unattended");
        assert_eq!(door.matches("api_presenting_runner(").count(), 1, "{door}");
        assert_eq!(prod.matches("api_presenting_runner(").count(), 1);
        let ran = door.find("execute_with(").expect("the probe runs");
        let read = door
            .find("runner_credential()")
            .expect("the credential is read");
        assert!(ran < read, "the credential is read after the probe ran");
    }

    /// THE OUTPUT SHAPE THE PACKET'S READERS EXPECT: the verdict line
    /// leads with its ALL-CAPS word — PROVEN, NOT YET, NOT RUN, NOT
    /// PROVEN — then the car, then why; and the exit code is one of
    /// the three the ops-request carries, plus 0.
    #[test]
    fn the_unattended_verdict_line_leads_with_its_word() {
        let missing = vec!["kubectl".to_string()];
        let cases: [(Verdict, &str, i32); 4] = [
            (
                Verdict::Proven,
                "PROVEN c1 — exit 0 and printed \"x:ok\"",
                0,
            ),
            (
                Verdict::NotYet {
                    said: "not yet: none".into(),
                },
                "NOT YET c1 — why",
                NOT_YET_EXIT,
            ),
            (
                Verdict::Unrunnable {
                    missing: missing.clone(),
                },
                "NOT RUN c1 — why",
                UNRUNNABLE_EXIT,
            ),
            (
                Verdict::NotProven(anyhow::anyhow!("no")),
                "NOT PROVEN c1 — why",
                1,
            ),
        ];
        for (v, line, code) in &cases {
            assert_eq!(unattended_verdict_line("c1", v, "x:ok", "why"), *line);
            assert_eq!(unattended_exit(v), *code);
        }
    }

    /// The three non-zero codes are distinct from each other and from
    /// the refusal's, so a reader of the ops-request's `exit_code`
    /// knows what to do without opening the output.
    /// A PROBE WHOSE EMPTY TOKEN DIRECTORY CANNOT BE MADE DID NOT RUN,
    /// AND SAYS SO AS THE HOST'S REFUSAL (backlog 2e1f609e, review
    /// 595d8a89 N2). The making is handed in failed — the disk-full
    /// error `tempfile` would give — and the probe would leave a marker
    /// if it ran. Three halves: nothing ran; the error is an
    /// [`EnvironmentRefusal`] that names the cause and says nothing was
    /// judged; and an ordinary error is not one, so only this case is
    /// diverted to [`REFUSED_EXIT`] and never read as exit 1, "ran, not
    /// proven" — a red car over a full /tmp.
    #[test]
    fn a_probe_without_its_empty_token_dir_is_an_environment_refusal_not_a_red() {
        let dir = boss_testing::scratch::scratch_dir("prove-environment-refusal");
        let marker = dir.join("ran");
        let probe = format!("touch '{}'; echo claim:ok", marker.display());
        let made: Result<crate::door_env::NoTokenInReach> =
            Err(anyhow::anyhow!("No space left on device (os error 28)")
                .context("making the empty token directory a child runs with"));
        let e = execute_given(&probe, &Shell::here(None), made)
            .expect_err("no token directory, no run");
        assert!(
            !marker.exists(),
            "the probe ran without its empty token dir"
        );
        let said = format!("{e:#}");
        assert!(environment_refusal(e).is_ok(), "{said}");
        assert!(said.contains("ENVIRONMENT REFUSAL"), "{said}");
        assert!(said.contains("did not run"), "{said}");
        assert!(said.contains("No space left on device"), "{said}");
        assert!(
            environment_refusal(anyhow::anyhow!("could not run the probe (bash): gone")).is_err(),
            "only the environment's refusal is diverted from the ordinary error path"
        );
        assert_ne!(REFUSED_EXIT, 1, "a refusal must not read as ran-not-proven");
        // The made directory, handed in, runs the probe as before.
        let o = execute_given(
            "echo claim:ok",
            &Shell::here(None),
            crate::door_env::NoTokenInReach::new(),
        )
        .expect("bash runs");
        assert_eq!(o.exit, 0, "{o:?}");
    }

    #[test]
    fn reader_credential_refusal_preserves_the_cause_without_a_disk_only_remedy() {
        let cause = "reader credential is unavailable";
        for message in [
            EnvironmentRefusal::before_execution(cause.into()).to_string(),
            environment_why("forge", cause),
        ] {
            assert!(message.contains(cause), "{message}");
            assert!(message.contains("repair the named cause"), "{message}");
            assert!(!message.contains("free the"), "{message}");
        }
    }

    /// THE VERIFIED ACCOUNT, OR NO PROBE (decision c98c79aa, `fallback`).
    /// On a host that names the marker, a door that drops to a user runs
    /// the probe only when the marker names that user. Absent, or naming
    /// anyone else: nothing runs — as that user or as anyone — and the
    /// error is the environment's refusal, so the car records did-not-run
    /// and is never read as exit 1, "ran, not proven".
    #[test]
    fn a_probe_whose_account_is_not_verified_this_tick_does_not_run() {
        let dir = boss_testing::scratch::scratch_dir("prove-account-not-verified");
        let ran = dir.join("ran");
        let probe = format!("touch '{}'; echo claim:ok", ran.display());
        // `root` so the verified leg can run wherever this test does: as
        // root it drops to root, and as anyone else it drops to nobody.
        let shell = Shell {
            user: Some("root".into()),
            ..Shell::here(None)
        };
        let token = crate::door_env::NoTokenInReach::new;

        let marker = dir.join("probe-account.verified");
        let e =
            execute_gated(&probe, &shell, token(), Some(&marker)).expect_err("no marker, no run");
        assert!(!ran.exists(), "the probe ran with no verified account");
        let said = format!("{e:#}");
        assert!(environment_refusal(e).is_ok(), "{said}");
        assert!(said.contains("did not run"), "{said}");
        assert!(said.contains("is not verified on this host"), "{said}");
        assert!(said.contains("is absent"), "{said}");

        // A marker for somebody else is not this user's.
        std::fs::write(&marker, "david\n").unwrap();
        let e = execute_gated(&probe, &shell, token(), Some(&marker))
            .expect_err("another account's marker, no run");
        assert!(!ran.exists(), "the probe ran on another account's marker");
        assert!(format!("{e:#}").contains("names \"david\""), "{e:#}");
        assert!(environment_refusal(e).is_ok());

        // Verified this tick: it runs.
        std::fs::write(&marker, "root\n").unwrap();
        let o = execute_gated(&probe, &shell, token(), Some(&marker)).expect("verified, runs");
        assert_eq!(o.exit, 0, "{o:?}");
        assert!(ran.exists());

        // No marker handed in, and the hand door drops to nobody: both
        // run here. At the unattended door a host naming no marker never
        // gets this far — `unattended_ground` refuses it first
        // (`a_host_that_names_no_probe_account_runs_no_probe_as_anyone`).
        assert!(execute_gated("true", &shell, token(), None).is_ok());
        assert!(
            execute_gated("true", &Shell::here(None), token(), Some(&dir.join("none"))).is_ok()
        );
    }

    /// THE ACCOUNT IS ASKED BEFORE THE READER DOOR OPENS. On a host that
    /// holds a reader credential AND names the marker, an unverified
    /// account gets no door: no accepts read reaches any gate, no socket
    /// is made for it, and the refusal is the account's. The control is
    /// the same shell with the account verified — then the door IS tried,
    /// the upstream sees its accepts read, and (this upstream answering
    /// nonsense) the refusal is the door's. So the first leg's silence is
    /// the order, not a door that never asks.
    #[test]
    fn an_unverified_account_opens_no_reader_door_and_asks_no_gate() {
        use crate::probe_reader::fixture;
        let upstream = fixture::serve(fixture::recorder);
        let dir = boss_testing::scratch::scratch_dir("prove-account-before-door");
        let held = tempfile::tempdir().unwrap();
        let ran = dir.join("ran");
        let probe = format!("touch '{}'", ran.display());
        let shell = Shell {
            user: Some("root".into()),
            reader: Some(crate::probe_reader::Config {
                credential: fixture::credential(held.path(), fixture::VALUE, 0o600),
                base: upstream.base.clone(),
                ports: String::new(),
                ports_from: None,
            }),
            ..Shell::here(None)
        };
        let token = crate::door_env::NoTokenInReach::new;
        let marker = dir.join("probe-account.verified");

        let e = execute_gated(&probe, &shell, token(), Some(&marker))
            .expect_err("unverified: nothing runs");
        let said = format!("{e:#}");
        assert!(said.contains("is not verified on this host"), "{said}");
        assert!(environment_refusal(e).is_ok(), "{said}");
        assert!(
            upstream.seen.lock().unwrap().is_empty(),
            "a gate was asked about a reader credential for an unverified account"
        );
        assert!(!ran.exists());

        std::fs::write(&marker, "root\n").unwrap();
        let e = execute_gated(&probe, &shell, token(), Some(&marker))
            .expect_err("this upstream is no gate, so the door refuses");
        let said = format!("{e:#}");
        assert!(!said.contains("is not verified on this host"), "{said}");
        assert!(environment_refusal(e).is_ok(), "{said}");
        // The door was TRIED. As anyone but root it gets as far as asking
        // the gate, once. As root it is refused one step earlier — the
        // door will not make a socket directory in a /tmp the user it
        // drops to (here root itself) could rename — which is the door's
        // own refusal and still proves it was reached.
        let asked = upstream.seen.lock().unwrap().len();
        assert!(
            asked == 1 || (asked == 0 && said.contains("the reader door's directory")),
            "verified, the reader door is tried (gate asked {asked} time(s)): {said}"
        );
        assert!(!ran.exists());
    }

    /// The name the forge's drop-in sets for the marker, read off the one
    /// line of `infra/forge/probe-account.sh` that writes it.
    fn marker_name_the_drop_in_sets() -> (String, String) {
        let script =
            std::fs::read_to_string(boss_testing::repo_root().join("infra/forge/probe-account.sh"))
                .expect("infra/forge/probe-account.sh");
        let names: Vec<String> = script
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("echo \"Environment=")?
                    .strip_suffix("=$marker\"")
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            names.len(),
            1,
            "probe-account.sh must write exactly one drop-in line whose value is the marker: \
             {names:?}"
        );
        (names[0].clone(), script)
    }

    /// THE MARKER'S NAME LIVES TWICE AND IS HELD EQUAL (CLAUDE.md §9a;
    /// backlog dda26693, review acab446c F1). The door reads the marker's
    /// path from one environment name; the forge's drop-in sets it, the
    /// tick's verify writes the file it names, and the converge asks the
    /// installed binary whether it knows the name before it trusts the
    /// stop. Respell it on either side and nothing errors: the door finds
    /// no name, takes the host for one that has not converged onto the
    /// rule, and hands an unverified account to runuser.
    #[test]
    fn the_marker_name_is_the_one_the_forge_drop_in_sets() {
        let (name, script) = marker_name_the_drop_in_sets();
        assert_eq!(
            name, PROBE_VERIFIED_FILE_ENV,
            "the drop-in sets {name} and the door reads {PROBE_VERIFIED_FILE_ENV}: with two \
             names the forge stops refusing an unverified account, silently"
        );
        // Every spelling in the script is this one: the drop-in line, the
        // tick's own read, and the question put to the installed binary.
        let stem = "BOSS_PROBE_VERIFIED_";
        let spelled: Vec<&str> = script
            .match_indices(stem)
            .map(|(i, _)| {
                let rest = &script[i..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .collect();
        assert!(
            spelled.len() >= 3,
            "the script no longer names the marker: {spelled:?}"
        );
        for s in &spelled {
            assert_eq!(*s, PROBE_VERIFIED_FILE_ENV, "probe-account.sh spells {s}");
        }
        for line in [
            format!("local marker=\"${{{PROBE_VERIFIED_FILE_ENV}:-}}\""),
            format!("grep -aq '{PROBE_VERIFIED_FILE_ENV}' \"$cli\""),
        ] {
            assert!(
                script.contains(&line),
                "probe-account.sh must carry `{line}`"
            );
        }
    }

    /// THE TICK'S OWN REASON REACHES THE CAR (backlog dda26693, review
    /// acab446c F3). The refusal a car records used to say only that the
    /// marker was absent and send its reader to the converge packet's
    /// `probe_account` — written every ten minutes, and `verified` while a
    /// tick failed for a cause of its own (its 30 s fetch, a runuser bound
    /// under load). Now the tick leaves its reason beside the marker and the
    /// refusal carries it, bounded; a tick that left none says so rather
    /// than point at a packet that may not know.
    #[test]
    fn an_unverified_tick_hands_its_own_reason_to_the_car() {
        let dir = boss_testing::scratch::scratch_dir("prove-tick-why");
        let marker = dir.join("probe-account.verified");
        let why = dir.join(format!("probe-account.verified{PROBE_TICK_WHY_SUFFIX}"));
        let said_by_tick = "probe-account: verify-tick: NO PROBE RUNS THIS TICK — \
                            refresh-view FAILED — fetching /home/david/boss as its owner: timed out";
        std::fs::write(&why, format!("{said_by_tick}\n")).unwrap();
        let cause = probe_account_refusal("boss-probe", Some(&marker)).expect("refused");
        assert!(cause.contains("is not verified on this host"), "{cause}");
        assert!(
            cause.contains(&format!("this tick's verify-tick said: {said_by_tick}")),
            "{cause}"
        );
        assert!(!cause.contains("converge packet says why"), "{cause}");

        // A reason that is not one line of a sentence is cut, not read whole.
        std::fs::write(&why, "x".repeat(64 * 1024)).unwrap();
        let cause = probe_account_refusal("boss-probe", Some(&marker)).expect("refused");
        assert!(
            cause.len() < 2 * PROBE_TICK_WHY_MAX,
            "{} bytes",
            cause.len()
        );

        // No reason left: said, and the converge named as the other place.
        std::fs::remove_file(&why).unwrap();
        let cause = probe_account_refusal("boss-probe", Some(&marker)).expect("refused");
        assert!(cause.contains("verify-tick left no reason"), "{cause}");

        // The control: a marker naming the account runs, whatever is beside it.
        std::fs::write(&why, "stale\n").unwrap();
        std::fs::write(&marker, "boss-probe\n").unwrap();
        assert_eq!(probe_account_refusal("boss-probe", Some(&marker)), None);
    }

    /// The reason file's name lives twice — the suffix here and the line
    /// `verify_tick` names it on — and is held equal (CLAUDE.md §9a).
    #[test]
    fn the_tick_reason_is_written_where_the_door_reads_it() {
        let (_, script) = marker_name_the_drop_in_sets();
        let line = format!("local why=\"$marker{PROBE_TICK_WHY_SUFFIX}\"");
        assert!(
            script.contains(&line),
            "probe-account.sh's verify_tick must name its reason file `{line}`"
        );
    }

    /// AND THE DOOR READS IT FROM THE ENVIRONMENT (review acab446c F1,
    /// mutant X1). Both tests above hand the marker to `execute_gated`, so
    /// the one line that reads this process's environment was held by
    /// nothing: with `None` handed over instead, 132 tests stayed green and
    /// the forge would stop refusing. Proven by effect, through the door
    /// every caller uses: this binary re-run with the name THE SCRIPT sets
    /// (not this crate's constant) pointing at a marker that is absent,
    /// then at one that names the user, then with no name at all. The
    /// second leg is the control — the refusal of the first is the
    /// marker's, not a door that never runs.
    #[test]
    fn the_door_reads_the_marker_from_the_name_the_drop_in_sets() {
        const INNER: &str = "BOSS_PROVE_MARKER_WIRE_INNER";
        if let Ok(ran) = std::env::var(INNER) {
            // `root`, as above: it runs wherever this test does.
            let shell = Shell {
                user: Some("root".into()),
                ..Shell::here(None)
            };
            match execute_with(&format!("touch '{ran}'"), &shell) {
                Ok(o) => println!("WIRE=ran exit={}", o.exit),
                Err(e) => {
                    let said = format!("{e:#}");
                    let kind = if environment_refusal(e).is_ok() {
                        "refused"
                    } else {
                        "error"
                    };
                    println!("WIRE={kind} {said}");
                }
            }
            return;
        }
        let (name, _) = marker_name_the_drop_in_sets();
        let dir = boss_testing::scratch::scratch_dir("prove-marker-wire");
        let absent = dir.join("absent.verified");
        let verified = dir.join("root.verified");
        std::fs::write(&verified, "root\n").unwrap();
        let leg = |label: &str, marker: Option<&Path>| {
            let ran = dir.join(format!("ran-{label}"));
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "prove::tests::the_door_reads_the_marker_from_the_name_the_drop_in_sets",
                "--nocapture",
            ])
            .env(INNER, &ran)
            .env_remove(PROBE_VERIFIED_FILE_ENV)
            .env_remove(&name);
            if let Some(m) = marker {
                cmd.env(&name, m);
            }
            let out = cmd.output().unwrap();
            let printed = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                out.status.success(),
                "{label}: {printed}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            (printed, ran.exists())
        };

        let (printed, ran) = leg("unverified", Some(&absent));
        assert!(
            printed.contains("WIRE=refused"),
            "a host that names the marker and has none must refuse: {printed}"
        );
        assert!(
            printed.contains("is not verified on this host"),
            "{printed}"
        );
        assert!(printed.contains("is absent"), "{printed}");
        assert!(!ran, "the probe ran with no verified account: {printed}");

        let (printed, ran) = leg("verified", Some(&verified));
        assert!(printed.contains("WIRE=ran exit=0"), "{printed}");
        assert!(ran, "verified this tick, the probe runs: {printed}");

        let (printed, ran) = leg("unconverged", None);
        assert!(printed.contains("WIRE=ran exit=0"), "{printed}");
        // `execute_with` alone, as a hand run reaches it: the unattended
        // door refuses a host naming no marker before this, in
        // `unattended_ground` (a_host_that_names_no_probe_account_…).
        assert!(ran, "with no marker named, execute_with runs: {printed}");
    }

    /// THE UNATTENDED DOOR ASKS THE ACCOUNT BEFORE IT RUNS ANYTHING IN THE
    /// PROBE'S DIRECTORY (backlog dda26693, review acab446c F4). The door
    /// used to resolve the car's instant (root's own git in the view) and
    /// observe the tree (`pwd`, `id` and `git` as the named user) and only
    /// then reach the account's refusal, so an unverified account still
    /// had three programs run as it and a refused view was still opened.
    ///
    /// By effect, not by reading the order: this binary re-run with `git`,
    /// `id`, `pwd` and `runuser` replaced on PATH by recorders that write
    /// one line each time they are started and then run the real program.
    /// With the marker the drop-in names absent, the record must not
    /// exist. The second leg is the control — verified, the same call
    /// starts them, so an empty record in the first is the refusal's and
    /// not a recorder that never fires. The name set is the one THE SCRIPT
    /// writes, so the read of the environment is held here too.
    #[test]
    fn the_unattended_door_runs_nothing_for_an_unverified_account() {
        const INNER: &str = "BOSS_PROVE_GROUND_INNER";
        if std::env::var(INNER).is_ok() {
            // `root`, as above: it runs wherever this test does.
            let shell = Shell {
                user: Some("root".into()),
                ..Shell::here(Some(&std::env::current_dir().unwrap()))
            };
            let (shell, tree, refused) =
                unattended_ground(shell, Some("0123456789ab"), "http://sor.invalid:7900");
            match refused {
                Some(cause) => println!("GROUND=refused {cause}"),
                None => println!(
                    "GROUND=asked merge_ref={} unreadable={}",
                    shell.env.iter().any(|(k, _)| k == CAR_MERGE_REF_VAR),
                    tree.unreadable.len()
                ),
            }
            return;
        }
        let (name, _) = marker_name_the_drop_in_sets();
        let dir = boss_testing::scratch::scratch_dir("prove-ground-account-first");
        let bin = dir.join("bin");
        let view = dir.join("view");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&view).unwrap();
        for program in ["git", "id", "pwd", "runuser"] {
            boss_testing::scratch::write_exec(
                &bin.join(program),
                "#!/bin/sh\n\
                 me=${0##*/}\n\
                 printf '%s %s\\n' \"$me\" \"$*\" >> \"$GROUND_STARTED\"\n\
                 PATH=$GROUND_REAL_PATH exec \"$me\" \"$@\"\n",
            );
        }
        let real_path = std::env::var("PATH").unwrap();
        let absent = dir.join("absent.verified");
        let verified = dir.join("root.verified");
        std::fs::write(&verified, "root\n").unwrap();
        let leg = |label: &str, marker: &Path| {
            let started = dir.join(format!("started-{label}"));
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "prove::tests::the_unattended_door_runs_nothing_for_an_unverified_account",
                    "--nocapture",
                ])
                .current_dir(&view)
                .env(INNER, "1")
                .env("GROUND_STARTED", &started)
                .env("GROUND_REAL_PATH", &real_path)
                .env("PATH", format!("{}:{real_path}", bin.display()))
                .env_remove(PROBE_VERIFIED_FILE_ENV)
                .env(&name, marker)
                .output()
                .unwrap();
            let printed = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                out.status.success(),
                "{label}: {printed}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            (printed, std::fs::read_to_string(&started).ok())
        };

        let (printed, started) = leg("unverified", &absent);
        assert!(printed.contains("GROUND=refused"), "{printed}");
        assert!(
            printed.contains("is not verified on this host"),
            "{printed}"
        );
        assert_eq!(
            started, None,
            "programs were started in the probe's directory for an unverified account: {printed}"
        );

        let (printed, started) = leg("verified", &verified);
        assert!(printed.contains("GROUND=asked merge_ref=true"), "{printed}");
        let started = started.expect("verified, the door reads the instant and the tree");
        // As root each read goes through `runuser -u root -- <program>`,
        // so the program is a word of the line, not always its first.
        for program in ["git", "id", "pwd"] {
            assert!(
                started
                    .lines()
                    .any(|l| l.split_whitespace().any(|w| w == program)),
                "the control leg never started `{program}`, so the recorders prove nothing: \
                 {started}"
            );
        }

        // And `run_unattended` reaches the probe's directory only through
        // that one function: neither half is spelled at the door itself.
        let prod = include_str!("prove.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        let door = prod
            .split("pub(crate) async fn run_unattended(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("run_unattended");
        assert!(door.contains("unattended_ground("), "{door}");
        for early in [".with_car_instant(", "probe_tree("] {
            assert!(
                !door.contains(early),
                "run_unattended calls `{early}` itself, ahead of the account's answer"
            );
        }
    }

    /// The environment NAME the forge's drop-in sets for `value`, read off
    /// infra/forge/probe-account.sh's own `echo "Environment=<NAME>=<value>"`
    /// line — so the names the door reads are the script's, not a copy.
    fn drop_in_name(script: &str, value: &str) -> String {
        let names: Vec<String> = script
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("echo \"Environment=")?
                    .strip_suffix(&format!("={value}\""))
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            names.len(),
            1,
            "probe-account.sh must write exactly one drop-in line whose value is {value}: \
             {names:?}"
        );
        names[0].clone()
    }

    /// A HOST THAT NAMES NO PROBE ACCOUNT RUNS NO PROBE — NOT AS THE
    /// CHECKOUT'S OWNER, NOT AS ROOT (decision c98c79aa, question
    /// `fallback`; backlog 703358ce, run a4d6ded7). The unattended door
    /// used to fill an unset BOSS_PROBE_USER with `david` and an unset
    /// BOSS_PROBE_DIR with `/home/david/boss`, and a host that named no
    /// marker "behaved as it did": a forge whose drop-in was never written
    /// or was lost ran car text as the account that reaches root there.
    /// By effect, through `unattended_ground` with the shell
    /// `Shell::unattended` builds, in this binary re-run with exactly the
    /// names the script's drop-in sets: each one missing is a refusal that
    /// names it; all three named, with a marker for the account, is the
    /// control — the door then asks the tree, so the refusals are the
    /// names' and not a door that never runs.
    #[test]
    fn a_host_that_names_no_probe_account_runs_no_probe_as_anyone() {
        const INNER: &str = "BOSS_PROVE_UNNAMED_INNER";
        if std::env::var(INNER).is_ok() {
            let shell = Shell::unattended().unwrap();
            let (_, _, refused) =
                unattended_ground(shell, Some("0123456789ab"), "http://sor.invalid:7900");
            match refused {
                Some(cause) => println!("GROUND=refused {cause}"),
                None => println!("GROUND=asked"),
            }
            return;
        }
        let (marker_name, script) = marker_name_the_drop_in_sets();
        let user_name = drop_in_name(&script, "$ACCOUNT");
        let dir_name = drop_in_name(&script, "$VIEW");
        let scratch = boss_testing::scratch::scratch_dir("prove-unnamed-host");
        let view = scratch.join("view");
        std::fs::create_dir_all(&view).unwrap();
        // `root`, as above: it runs wherever this test does.
        let marker = scratch.join("root.verified");
        std::fs::write(&marker, "root\n").unwrap();
        let leg = |user: bool, dir: bool, mark: bool| {
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "prove::tests::a_host_that_names_no_probe_account_runs_no_probe_as_anyone",
                "--nocapture",
            ])
            .env(INNER, "1");
            for name in [&user_name, &dir_name, &marker_name] {
                cmd.env_remove(name);
            }
            if user {
                cmd.env(&user_name, "root");
            }
            if dir {
                cmd.env(&dir_name, &view);
            }
            if mark {
                cmd.env(&marker_name, &marker);
            }
            let out = cmd.output().unwrap();
            let printed = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                out.status.success(),
                "{printed}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            printed
        };

        for (user, dir, mark) in [
            (false, false, false),
            (true, true, false),
            (false, true, true),
            (true, false, true),
        ] {
            let printed = leg(user, dir, mark);
            assert!(
                printed.contains("GROUND=refused"),
                "a host naming account={user} view={dir} marker={mark} must refuse: {printed}"
            );
            assert!(
                printed.contains("never as the checkout's owner"),
                "{printed}"
            );
            for (named, name) in [(user, &user_name), (dir, &dir_name), (mark, &marker_name)] {
                assert_eq!(
                    printed.contains(name.as_str()),
                    !named,
                    "the refusal names exactly what the host left unset ({name}): {printed}"
                );
            }
        }

        let printed = leg(true, true, true);
        assert!(
            printed.contains("GROUND=asked"),
            "a host naming all three, its account verified, is asked: {printed}"
        );
    }

    /// ROOT OPENS NO FILE IN THE PROBE VIEW BEFORE THE ACCOUNT ANSWERS
    /// (backlog dda26693, review 37f44583 F1). `Shell::unattended` used to
    /// end in `with_probe_reader`, which opens
    /// `$BOSS_PROBE_DIR/infra/forge/sor-ports.env` as root — following a
    /// link, reading it whole — and `run_unattended` built that shell as
    /// the ARGUMENT to `unattended_ground`, so the read happened before
    /// the account's refusal on exactly the tick whose verify had refused
    /// the view. Measured by effect, through the shell the door builds and
    /// the names the drop-in sets: the file is a FIFO, which an open blocks
    /// on until a writer comes, so a door that opens it never returns and
    /// is killed at the deadline. The control is a verified account over a
    /// plain file: the door then reads that same path into the reader's
    /// port table, so a refusal that returns is the account's and not a
    /// path nobody opens.
    #[test]
    fn root_opens_nothing_in_the_view_before_the_account_answers() {
        const INNER: &str = "BOSS_PROVE_VIEW_READ_INNER";
        if std::env::var(INNER).is_ok() {
            let shell = Shell::unattended().unwrap();
            let (shell, _, refused) = unattended_ground(shell, None, "http://sor.invalid:7900");
            let ports = shell
                .env
                .iter()
                .find(|(k, _)| k == "BOSS_SOR_PORTS")
                .map(|(_, v)| v.clone());
            println!(
                "GROUND={} reader={} ports={ports:?}",
                if refused.is_some() {
                    "refused"
                } else {
                    "asked"
                },
                shell.reader.is_some()
            );
            return;
        }
        let (marker_name, script) = marker_name_the_drop_in_sets();
        let user_name = drop_in_name(&script, "$ACCOUNT");
        let dir_name = drop_in_name(&script, "$VIEW");
        let scratch = boss_testing::scratch::scratch_dir("prove-view-read-first");
        let verified = scratch.join("root.verified");
        std::fs::write(&verified, "root\n").unwrap();
        let view_with = |label: &str, fifo: bool| {
            let view = scratch.join(label);
            let forge = view.join("infra/forge");
            std::fs::create_dir_all(&forge).unwrap();
            let ports = view.join(SOR_PORTS_ENV);
            if fifo {
                let made = std::process::Command::new("mkfifo")
                    .arg(&ports)
                    .status()
                    .expect("mkfifo is coreutils");
                assert!(made.success(), "mkfifo {}", ports.display());
            } else {
                std::fs::write(&ports, "jobs=7900\n").unwrap();
            }
            view
        };
        // `root`, as the tests above: it runs wherever this test does.
        let leg = |view: &Path, marker: &Path| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "prove::tests::root_opens_nothing_in_the_view_before_the_account_answers",
                    "--nocapture",
                ])
                .env(INNER, "1")
                .env(&user_name, "root")
                .env(&dir_name, view)
                .env(&marker_name, marker)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "the door blocked opening {} — root read the probe view before the \
                         account answered",
                        view.join(SOR_PORTS_ENV).display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let out = child.wait_with_output().unwrap();
            let printed = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                out.status.success(),
                "{printed}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            printed
        };

        let printed = leg(
            &view_with("refused", true),
            &scratch.join("absent.verified"),
        );
        assert!(
            printed.contains("GROUND=refused reader=false ports=None"),
            "{printed}"
        );

        let printed = leg(&view_with("asked", false), &verified);
        assert!(
            printed.contains("GROUND=asked reader=true ports=Some(\"jobs=7900\")"),
            "the control: a verified account's door reads the view's port table: {printed}"
        );
    }

    /// Every site in non-test source where a door runs a car's probe —
    /// `match [prove::]execute_with(&probe, &shell)` — as (the function
    /// it sits in, the `Door::` its refusal arm names). A door that went
    /// back to `?` or a bare call has no `match` and no Door, and the
    /// bare-call count below names it.
    fn door_sites() -> Vec<(String, String, String)> {
        let mut sites = Vec::new();
        for src in [include_str!("prove.rs"), include_str!("disprove.rs")] {
            let prod = src.split("#[cfg(test)]").next().unwrap_or("");
            let lines: Vec<&str> = prod.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if !l.contains("execute_with(&probe, &shell)") {
                    continue;
                }
                let func = lines[..i]
                    .iter()
                    .rev()
                    .find_map(|p| {
                        let t = p.trim_start();
                        let t = t.strip_prefix("pub(crate) ").unwrap_or(t);
                        let t = t.strip_prefix("async ").unwrap_or(t);
                        t.strip_prefix("fn ")
                            .map(|r| r.split('(').next().unwrap_or("").to_string())
                    })
                    .unwrap_or_default();
                let door = if l.contains("match ") {
                    lines[i..lines.len().min(i + 40)]
                        .iter()
                        .find_map(|n| n.split("Door::").nth(1))
                        .map(|r| {
                            r.chars()
                                .take_while(char::is_ascii_alphanumeric)
                                .collect::<String>()
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                // The refusal arm: from the call to the door's exit.
                let end = lines[i..]
                    .iter()
                    .position(|n| n.contains("exit_refused("))
                    .map_or(i, |k| i + k);
                let arm = lines[i..=end.min(i + 60)].join("\n");
                sites.push((func, door, arm));
            }
        }
        sites
    }

    /// One door's pin: its site routes the refusal through its own Door,
    /// in the function that is that door, and the Door exits 2 — and
    /// records an attempt exactly when the door records attempts.
    fn the_door_routes_a_refusal_to_exit_2(door: Door, func: &str, records: bool) {
        let sites = door_sites();
        let bare: Vec<_> = sites.iter().filter(|(_, d, _)| d.is_empty()).collect();
        assert!(
            bare.is_empty(),
            "a door runs a probe without routing a refusal: {bare:?}"
        );
        let name = format!("{door:?}");
        let here: Vec<_> = sites.iter().filter(|(_, d, _)| *d == name).collect();
        assert_eq!(
            here.len(),
            1,
            "the {name} door must route a refusal exactly once: {sites:?}"
        );
        assert_eq!(
            here[0].0, func,
            "the {name} door's site sits in the wrong function"
        );
        // A recording door hands the refusal to `record_refusal`, built
        // by `refusal_patch`, unconditionally — no `if` at the door may
        // stand between the refusal and the record (review 2d0043ad).
        let arm = &here[0].2;
        let calls = arm.contains("record_refusal(") && arm.contains("refusal_patch(");
        assert_eq!(calls, records, "the {name} door's refusal arm:\n{arm}");
        if records {
            // ...and passes its own door and the only dry it has: the
            // unattended door none (`false`), the hand door its `--dry`.
            let flat: String = arm.chars().filter(|c| !c.is_whitespace()).collect();
            let dry = if door == Door::Unattended {
                "false"
            } else {
                "dry"
            };
            assert!(
                flat.contains(&format!("record_refusal(Door::{name},{dry},")),
                "the {name} door must call record_refusal(Door::{name}, {dry}, …):\n{arm}"
            );
            let before = arm.split("record_refusal(").next().unwrap_or("");
            assert!(
                !before.lines().any(|l| l.trim_start().starts_with("if ")),
                "the {name} door guards its record with an `if`:\n{arm}"
            );
        }
        let act = on_environment_refusal(door);
        assert_eq!(act.exit, REFUSED_EXIT, "{name}");
        assert_eq!(REFUSED_EXIT, 2);
        assert_eq!(act.record_attempt, records, "{name}");
    }

    /// `boss prove --from-car --unattended` — the arrival run and the
    /// hourly recheck's: exit 2, and the refusal recorded on the car.
    #[test]
    fn the_unattended_door_routes_an_environment_refusal_to_exit_2() {
        the_door_routes_a_refusal_to_exit_2(Door::Unattended, "run_unattended", true);
    }

    /// `boss prove --from-car` (or `--probe`) by hand: exit 2, recorded.
    #[test]
    fn the_from_car_door_routes_an_environment_refusal_to_exit_2() {
        the_door_routes_a_refusal_to_exit_2(Door::Hand, "run", true);
    }

    /// `boss prove --recheck`: exit 2, nothing written.
    #[test]
    fn the_recheck_door_routes_an_environment_refusal_to_exit_2() {
        the_door_routes_a_refusal_to_exit_2(Door::Recheck, "run", false);
    }

    /// `boss prove --disproved`: exit 2, no disproof.
    #[test]
    fn the_disproved_door_routes_an_environment_refusal_to_exit_2() {
        the_door_routes_a_refusal_to_exit_2(Door::Disproved, "run", false);
    }

    /// THE REFUSAL IS RECORDED AS A NOT YET THE RECHECK RETRIES (backlog
    /// 2e1f609e, review 19ecf37f). Without a record the arrival run left
    /// no `proof_attempt`, the hourly recheck (which re-runs only cars
    /// carrying one) never picked the car up, and it sat ~72 hours with
    /// the host fault shown nowhere. The record: an attempt, read as not
    /// yet by the one definition the shed and the streak share, never an
    /// exit anyone could read as a failure, naming host and cause, and
    /// marked as the environment's.
    #[test]
    fn an_environment_refusal_is_recorded_as_a_not_yet_attempt() {
        let a = environment_attempt_json(
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T09:00:00Z",
            "making the empty token directory a child runs with: No space left on device",
            None,
        );
        assert!(boss_jobs::car::attempt_said_not_yet(&a), "{a}");
        assert_eq!(a["not_yet"], json!(true), "{a}");
        assert_eq!(a["exit"], Value::Null, "nothing exited: {a}");
        assert_eq!(a["environment"], json!(true), "{a}");
        assert_eq!(a["unrunnable"], json!(false), "{a}");
        assert_eq!(
            a["probe"],
            json!("echo claim:ok"),
            "the recheck re-runs this text"
        );
        let why = a["why"].as_str().unwrap_or("");
        assert!(why.starts_with("NOT YET"), "{why}");
        assert!(why.contains("ENVIRONMENT REFUSAL"), "{why}");
        assert!(
            why.contains("forge") && why.contains("No space left on device"),
            "{why}"
        );
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], json!(1), "{a}");
        // A second refusal carries the streak, as any not-yet does.
        let b = environment_attempt_json(
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T10:00:00Z",
            "again",
            Some(&a),
        );
        assert_eq!(b[boss_jobs::car::NOT_YET_RUNS], json!(2), "{b}");
        assert_eq!(
            b[boss_jobs::car::NOT_YET_SINCE],
            a[boss_jobs::car::NOT_YET_SINCE],
            "{b}"
        );
    }

    /// A car as the doors read it, with this `proof_attempt` (if any).
    fn refused_car(prior: Option<Value>) -> Value {
        let mut md = json!({"proof_probe": "echo claim:ok", "proof_expect": "claim:ok"});
        if let Some(p) = prior {
            md["proof_attempt"] = p;
        }
        json!({"id": "00000000-0000-0000-0000-0000000000aa", "metadata": md})
    }

    /// The car's metadata after `patch` is applied the way the metadata
    /// endpoint applies it: top-level keys merged.
    fn merged(car: &Value, patch: &Value) -> Value {
        let mut md = car["metadata"].clone();
        for (k, v) in patch.as_object().expect("a patch is an object") {
            md[k] = v.clone();
        }
        md
    }

    const REFUSED_CAUSE: &str = "making the empty token directory: No space left on device";

    /// A RED PRIOR STILL READS FAILING AFTER A REFUSAL (review 2d0043ad
    /// F2). The hourly recheck re-runs exactly the failing cars, so a
    /// refusal written over their attempt turned every one of them into a
    /// calm NOT YET within the hour of a full /tmp. The refusal judged
    /// nothing; the judgement stands, and the refusal sits beside it.
    #[test]
    fn a_refusal_leaves_a_failed_attempt_reading_failing() {
        let failed = Outcome {
            exit: 1,
            stdout: "nope".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let red = attempt_json(
            "echo claim:ok",
            Some("claim:ok"),
            &failed,
            "forge",
            "operator:unidentified",
            "2026-10-01T08:00:00Z",
            "FAILED: printed nope",
            None,
        );
        let car = refused_car(Some(red.clone()));
        let before = boss_jobs::regions::shed_place(&car["metadata"]);
        let patch = refusal_patch(
            &car,
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T09:00:00Z",
            REFUSED_CAUSE,
        );
        assert!(patch.get("proof_attempt").is_none(), "{patch}");
        let after = merged(&car, &patch);
        assert_eq!(
            after["proof_attempt"], red,
            "the judgement is left exactly as it was"
        );
        assert_eq!(boss_jobs::regions::shed_place(&after), before);
        assert_eq!(
            boss_jobs::regions::shed_place(&after),
            boss_jobs::regions::ShedPlace::ProbePending {
                last: Some("FAILED: printed nope".into())
            },
            "a failing car still reads failing"
        );
        let r = &after[PROOF_ENVIRONMENT_REFUSAL];
        assert_eq!(r["host"], json!("forge"), "{r}");
        assert_eq!(r["at"], json!("2026-10-01T09:00:00Z"), "{r}");
        assert_eq!(r["cause"], json!(REFUSED_CAUSE), "{r}");
    }

    /// NO PRIOR ATTEMPT: retain the refusal for retry accounting and its
    /// complete cause, while the shed keeps the unexecuted outcome unread.
    /// A host refusal is not an observed wait on the declared condition.
    #[test]
    fn a_refusal_with_no_prior_becomes_a_not_yet_attempt() {
        let car = refused_car(None);
        let patch = refusal_patch(
            &car,
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T09:00:00Z",
            REFUSED_CAUSE,
        );
        let after = merged(&car, &patch);
        assert!(
            boss_jobs::car::attempt_said_not_yet(&after["proof_attempt"]),
            "{after}"
        );
        assert_eq!(
            after["proof_attempt"]["environment"],
            json!(true),
            "{after}"
        );
        assert_eq!(
            boss_jobs::regions::shed_place(&after),
            boss_jobs::regions::ShedPlace::ProbeUnread
        );
        let said = after["proof_attempt"]["why"].as_str().unwrap();
        assert!(said.contains("ENVIRONMENT REFUSAL"), "{said}");
        assert!(said.contains("No space left on device"), "{said}");
        assert_eq!(
            after[PROOF_ENVIRONMENT_REFUSAL]["cause"],
            json!(REFUSED_CAUSE)
        );
        // A `proof_attempt: null` is no prior either.
        let null = refused_car(Some(Value::Null));
        let p = refusal_patch(&null, "echo claim:ok", None, "forge", "a", "t", "c");
        assert!(p.get("proof_attempt").is_some(), "{p}");
    }

    /// A NOT-YET PRIOR: the refusal replaces it as the next not-yet, and
    /// the streak continues rather than restarting.
    #[test]
    fn a_refusal_after_a_not_yet_continues_the_streak() {
        let car = refused_car(None);
        let first = refusal_patch(
            &car,
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T09:00:00Z",
            REFUSED_CAUSE,
        );
        let car = json!({"id": car["id"], "metadata": merged(&car, &first)});
        let second = refusal_patch(
            &car,
            "echo claim:ok",
            Some("claim:ok"),
            "forge",
            "operator:unidentified",
            "2026-10-01T10:00:00Z",
            REFUSED_CAUSE,
        );
        let a = &second["proof_attempt"];
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], json!(2), "{second}");
        assert_eq!(
            a[boss_jobs::car::NOT_YET_SINCE],
            first["proof_attempt"][boss_jobs::car::NOT_YET_SINCE],
            "{second}"
        );
    }

    /// THE RECORD IS HANDED TO THE WRITER (review 2d0043ad: an `if false`
    /// at the door left every refusal test green). The decision lives in
    /// `record_refusal`, which each recording door calls unconditionally
    /// (pinned by the door tests above): the unattended door and a
    /// non-dry hand door hand over exactly the patch; a dry hand run,
    /// `--recheck` and `--disproved` write nothing; a failed write is an
    /// error the door can say, never a silent success.
    #[tokio::test]
    async fn the_recording_doors_hand_the_refusal_to_their_writer() {
        let car = refused_car(None);
        let patch = refusal_patch(
            &car,
            "echo claim:ok",
            None,
            "forge",
            "a",
            "t",
            REFUSED_CAUSE,
        );
        for (door, dry, writes) in [
            (Door::Unattended, false, true),
            (Door::Hand, false, true),
            (Door::Hand, true, false),
            (Door::Recheck, false, false),
            (Door::Disproved, false, false),
        ] {
            let seen = std::sync::Mutex::new(None::<Value>);
            let wrote = record_refusal(door, dry, &patch, |p| {
                *seen.lock().expect("the capture") = Some(p);
                async { Ok(()) }
            })
            .await
            .expect("the writer succeeded");
            assert_eq!(wrote, writes, "{door:?} dry={dry}");
            let got = seen.lock().expect("the capture").clone();
            assert_eq!(got.is_some(), writes, "{door:?} dry={dry}");
            if writes {
                assert_eq!(got.as_ref(), Some(&patch), "{door:?}: the patch, unchanged");
            }
        }
        let failed = record_refusal(Door::Unattended, false, &patch, |_| async {
            Err(anyhow::anyhow!("503 from the jobs API"))
        })
        .await;
        assert!(failed.is_err(), "a failed write is said, not swallowed");
    }

    #[test]
    fn the_unattended_door_exits_one_of_three_codes() {
        let codes = [1, UNRUNNABLE_EXIT, NOT_YET_EXIT, REFUSED_EXIT];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "{codes:?}");
        assert_eq!(REFUSED_EXIT, 2);
    }

    /// THE ATTEMPT RECORD CARRIES THE KEYS THE YARD READS
    /// (`apps/web/src/it/yard/yard.ts`, `proofAttempt`) and `--recheck`
    /// re-runs from: one shape from both doors.
    #[test]
    fn the_attempt_record_carries_the_keys_the_yard_reads() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &o,
            "h",
            "operator:unidentified",
            "now",
            "NOT YET: none",
            None,
        );
        let keys: std::collections::BTreeSet<&str> =
            a.as_object().unwrap().keys().map(String::as_str).collect();
        let want: std::collections::BTreeSet<&str> = [
            "at",
            "exit",
            "stdout",
            "stderr",
            "host",
            "actor",
            "probe",
            "expect",
            "why",
            "unrunnable",
            "not_yet",
            "missing_tools",
            "elapsed_ms",
            "bound_s",
            "reads",
            boss_jobs::car::NOT_YET_SINCE,
            boss_jobs::car::NOT_YET_RUNS,
        ]
        .into_iter()
        .collect();
        assert_eq!(keys, want);
        assert_eq!(a["not_yet"], true);
        assert_eq!(a["unrunnable"], false);
        assert_eq!(a["actor"], "operator:unidentified");
        // An outcome no run produced says so with nulls, never an absent
        // key: one shape for every reader.
        assert_eq!(a["elapsed_ms"], Value::Null);
        assert_eq!(a["bound_s"], Value::Null);
        assert_eq!(a["reads"], json!([]));
    }

    /// THE ATTEMPT SAYS HOW LONG THE RUN TOOK AND WHERE ITS TIME WENT
    /// (backlog a32af300). The shape of car 5159b6b6's attempt at 14:00Z,
    /// with what the run measured: killed at the bound, the health read
    /// answered in a third of a second, the map read still waiting.
    #[test]
    fn the_attempt_records_the_runs_elapsed_time_bound_and_reads() {
        use crate::probe_reader::ReadTiming;
        let o = Outcome {
            exit: TIMEOUT_EXIT,
            stdout: String::new(),
            stderr: "\n[boss prove: killed at 180s timeout]".into(),
            missing_tools: Vec::new(),
            timing: Some(Timing {
                elapsed_ms: 180_412,
                bound_s: Some(180),
                reads: vec![
                    ReadTiming {
                        path: "/api/jobs/health".into(),
                        ms: 315,
                        status: Some(200),
                    },
                    ReadTiming {
                        path: "/api/yard/regions?window=24h".into(),
                        ms: 179_870,
                        status: None,
                    },
                ],
            }),
        };
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &o,
            "forge",
            "operator:unidentified",
            "now",
            "the probe exited 124",
            None,
        );
        assert_eq!(a["elapsed_ms"], 180_412);
        assert_eq!(a["bound_s"], 180);
        assert_eq!(
            a["reads"],
            json!([
                {"path": "/api/jobs/health", "ms": 315, "status": 200, "in_flight": false},
                {"path": "/api/yard/regions?window=24h", "ms": 179_870, "status": null, "in_flight": true},
            ])
        );
    }

    /// WHO RAN THE PROBE (backlog 371b0443). The attempt names the host
    /// and, since this item, the actor a `boss` verb in the probe signed
    /// as — because car f4cdf451's FAILING was the forge reading as
    /// nobody what the builder had rehearsed as itself, and the record
    /// said where it ran but not as whom. The unattended door builds the
    /// probe's environment and names no actor, so it reads as
    /// unidentified whatever this process is; a door that sets one says
    /// it; the hand door's probe inherits this process whole, so it is
    /// whoever this process resolves to.
    #[test]
    fn the_attempt_names_the_actor_the_probe_ran_as() {
        let unattended = Shell {
            inherit: Some(&UNATTENDED_INHERITS),
            strip: vec![
                crate::identity::ACTOR_ENV.to_string(),
                crate::identity::ACTOR_FILE_ENV.to_string(),
            ],
            ..Shell::here(None)
        };
        let me = || {
            Some(crate::identity::Caller {
                id: "agent-claude".into(),
                source: crate::identity::Source::Env,
            })
        };
        assert_eq!(
            unattended.probe_actor_with(me),
            crate::identity::UNIDENTIFIED
        );
        let named = Shell {
            env: vec![(crate::identity::ACTOR_ENV.into(), "agent-x".into())],
            ..unattended.clone()
        };
        assert_eq!(named.probe_actor_with(me), "agent-x");
        assert_eq!(Shell::here(None).probe_actor_with(me), "agent-claude");
        assert_eq!(
            Shell::here(None).probe_actor_with(|| None),
            crate::identity::UNIDENTIFIED
        );
    }

    /// THE ATTEMPT CARRIES ITS NOT-YET STREAK (backlog adef5ddf). Both
    /// doors write through here, so both hand the car's PRIOR attempt in
    /// and the record says how long this probe has been answering not
    /// yet — the one signal that tells a starved probe from a patient
    /// one. A run that is not a not-yet carries no streak (null, zero),
    /// so the shape stays one shape.
    #[test]
    fn a_not_yet_attempt_carries_the_streak_from_the_prior_one() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let prior = json!({
            "at": "2026-09-23T06:00:00Z", "exit": 75, "not_yet": true, "probe": "true",
            "not_yet_since": "2026-09-19T05:50:00Z", "not_yet_runs": 85,
        });
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &o,
            "h",
            "a",
            "2026-09-23T07:00:00Z",
            "NOT YET: none",
            Some(&prior),
        );
        assert_eq!(a[boss_jobs::car::NOT_YET_SINCE], "2026-09-19T05:50:00Z");
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], 86);

        let red = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: "FAILED\n".into(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let a = attempt_json(
            "true",
            Some("x:ok"),
            &red,
            "h",
            "a",
            "2026-09-23T07:00:00Z",
            "FAILED",
            Some(&prior),
        );
        assert_eq!(a[boss_jobs::car::NOT_YET_SINCE], Value::Null);
        assert_eq!(a[boss_jobs::car::NOT_YET_RUNS], 0);
    }

    /// THE DID-NOT-RUN SENTENCE at the unattended door names the forge's
    /// vantage — the user and the checkout — with the same lead word and
    /// disclaimer as the hand door's.
    #[test]
    fn the_unattended_unrunnable_sentence_names_the_vantage() {
        let missing = vec!["kubectl".to_string()];
        let why =
            unrunnable_why_unattended("forge", &missing, "david", Path::new("/home/david/boss"));
        assert!(
            why.starts_with("THE PROBE DID NOT RUN on forge: kubectl not found"),
            "{why}"
        );
        assert!(why.contains("as david in /home/david/boss"), "{why}");
        assert!(
            why.contains("This says nothing about whether the change works"),
            "{why}"
        );
        let hand = unrunnable_why("pod", &missing);
        assert!(
            hand.starts_with("THE PROBE DID NOT RUN on pod: kubectl not found"),
            "{hand}"
        );
        assert!(
            hand.contains("This says nothing about whether the change works"),
            "{hand}"
        );
    }

    /// THE DECLARED WAIT IS OBSERVED BY THE DOOR THAT RECORDS THE
    /// NOT-YET (backlog b461341d): a `seen` check that exits 0 stamps a
    /// sighting, one that does not stamps none, and a car with no
    /// declaration — or an attempt that is not a not-yet — is untouched.
    #[test]
    fn a_not_yet_runs_the_cars_declared_seen_check_and_stamps_what_it_found() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
            missing_tools: Vec::new(),
            timing: None,
        };
        let attempt = attempt_json(
            "true",
            Some("x:ok"),
            &o,
            "h",
            "a",
            "T1",
            "NOT YET: none",
            None,
        );
        let car = |seen: &str| json!({"metadata": {"waits_on": boss_jobs::car::waits_on_value("an event", Some(seen))}});
        let shell = Shell::here(None);

        let a = with_wait_observed(attempt.clone(), &car("true"), &shell, None, "T1");
        assert_eq!(a["waits_on_exit"], 0);
        assert_eq!(a[boss_jobs::car::WAITS_ON_SEEN_AT], "T1");

        let a = with_wait_observed(attempt.clone(), &car("exit 3"), &shell, None, "T1");
        assert_eq!(a["waits_on_exit"], 3);
        assert_eq!(a[boss_jobs::car::WAITS_ON_SEEN_AT], Value::Null);

        let undeclared = json!({"metadata": {}});
        let a = with_wait_observed(attempt.clone(), &undeclared, &shell, None, "T1");
        assert_eq!(a, attempt);

        // A sighting keeps the date of the first run that saw it.
        let prior = json!({"waits_on_probe": "true", (boss_jobs::car::WAITS_ON_SEEN_AT): "T0"});
        let a = stamp_wait(attempt.clone(), Some(&prior), Some(0), "T1", "true");
        assert_eq!(a[boss_jobs::car::WAITS_ON_SEEN_AT], "T0");

        let failed = json!({"not_yet": false, "exit": 1});
        assert_eq!(
            with_wait_observed(failed.clone(), &car("true"), &shell, None, "T1"),
            failed
        );
    }

    #[test]
    fn the_observed_window_copies_the_machine_reading_instead_of_a_declared_pass() {
        let seen = r#"printf '%s' '{"observation":"unseen","coverage":{"begin_at":"2026-09-19T10:00:00Z","through_at":"2026-09-19T12:00:00Z","gaps":[]}}'; exit 1"#;
        let car = json!({"metadata": {"waits_on": {"on": "a clean window", "seen": seen}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &Shell::here(None),
            None,
            "T1",
        );
        assert_eq!(
            observed["waits_on_coverage"]["begin_at"],
            "2026-09-19T10:00:00Z"
        );
        assert_eq!(observed["waits_on_coverage"]["source"], seen);
        assert_eq!(observed["waits_on_exit"], 1);
    }

    #[test]
    fn the_observed_wait_records_its_source_and_does_not_inherit_another_observers_sighting() {
        let attempt = json!({"exit": 75, "not_yet": true});
        let car = json!({"metadata": {"waits_on": {"on": "the event", "seen": "exit 0"}}});
        let prior = json!({"waits_on_probe": "true", (boss_jobs::car::WAITS_ON_SEEN_AT): "T0"});
        let observed = with_wait_observed(attempt, &car, &Shell::here(None), Some(&prior), "T1");
        assert_eq!(
            observed["waits_on_probe"], "exit 0",
            "an outcome must identify the observer that actually ran"
        );
        assert_eq!(
            observed[boss_jobs::car::WAITS_ON_SEEN_AT],
            "T1",
            "changed observer cannot inherit old sighting time"
        );
    }

    #[test]
    fn every_supported_record_client_is_refused_without_a_reader_at_manual_doors() {
        for probe in [
            "boss-api GET /api/jobs/fixture | jq -r .status",
            "command /work/tools/bin/boss-api GET /api/jobs/fixture",
            "body=$(env BOSS_SOR_SERVICE=jobs boss-api GET /api/jobs/fixture); echo done",
            "boss-api GET /api/jobs/fixture | python3 -c 'import json,sys; print(json.load(sys.stdin))'",
            "boss-sor-read /api/jobs/fixture",
            "curl -fsS -H \"x-boss-user: $BOSS_SOR_USER\" $BOSS_JOBS_URL/api/jobs/fixture",
            "wget -qO- $BOSS_JOBS_URL/api/jobs/fixture",
            "python3 -c \"import urllib.request; urllib.request.urlopen('$BOSS_JOBS_URL/api/jobs/fixture')\"",
        ] {
            for door in [Door::Hand, Door::Recheck, Door::Disproved] {
                let refusal = record_read_refusal(probe, door, true)
                    .unwrap_or_else(|| panic!("{door:?} would run this record client: {probe}"));
                assert!(refusal.contains("BEFORE ANYTHING WAS SENT"));
                assert!(refusal.contains(ARRIVAL_PROBE_RULE));
                assert!(refusal.contains(HOURLY_PROBE_RULE));
                assert_eq!(record_read_refusal(probe, door, false), None);
            }
            assert_eq!(record_read_refusal(probe, Door::Unattended, true), None);
        }
    }

    #[test]
    fn the_actual_manual_guard_rejects_boss_api_before_a_client_can_execute() {
        let held = tempfile::tempdir().unwrap();
        let absent = held.path().join("never-deposited.credential");
        let present = held.path().join("synthetic-present.credential");
        std::fs::write(&present, "synthetic fixture").unwrap();
        let broken = held.path().join("synthetic-broken.credential");
        std::os::unix::fs::symlink(held.path().join("missing"), &broken).unwrap();
        let probe = "boss-api GET /api/jobs/fixture";
        for (name, door) in [
            ("hand", Door::Hand),
            ("recheck", Door::Recheck),
            ("disproved", Door::Disproved),
        ] {
            let shell = the_four_doors("http://sor.invalid:7900", &absent)
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, s)| s)
                .unwrap();
            let refusal = refuse_a_record_read_without_a_reader(probe, &shell, door)
                .expect_err("the existing guard must stop the client, without executing it");
            assert!(format!("{refusal:#}").contains("boss-api"));
            let bare = Shell {
                reader: None,
                ..shell
            };
            assert!(refuse_a_record_read_without_a_reader(probe, &bare, door).is_err());
            for credential in [&present, &broken] {
                let shell = the_four_doors("http://sor.invalid:7900", credential)
                    .into_iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, s)| s)
                    .unwrap();
                assert!(refuse_a_record_read_without_a_reader(probe, &shell, door).is_ok());
            }
            for source_only in [
                "git show HEAD:infra/dev/boss-api | grep -c 'GET /api/jobs'",
                "printf '%s' '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin))'",
                "boss-gateway-read /api/jobs",
            ] {
                assert_eq!(record_read_refusal(source_only, door, true), None);
            }
        }
    }

    #[test]
    fn identity_guidance_does_not_offer_a_tokenless_manual_record_door() {
        let refusal = admit("curl -fsS $BOSS_JOBS_URL/api/jobs/fixture", false)
            .refusal
            .unwrap();
        assert!(refusal.text.contains("no reader credential"));
        assert!(refusal.text.contains(HOURLY_PROBE_RULE));
    }

    #[test]
    fn wrapper_and_native_cli_record_reads_are_not_omitted_from_the_manual_guard() {
        for probe in [
            "bash infra/boss-api-curl.sh -fsS $BOSS_JOBS_URL/api/jobs/fixture",
            "boss-api-curl -fsS $BOSS_JOBS_URL/api/jobs/fixture",
            "bash infra/dev/boss-api GET /api/jobs/fixture",
            "boss receipt fixture-car",
            "boss job get fixture-job",
            "boss jobs --json",
            "boss orient",
        ] {
            for door in [Door::Hand, Door::Recheck, Door::Disproved] {
                assert!(
                    record_read_refusal(probe, door, true).is_some(),
                    "{door:?}: {probe}"
                );
            }
        }
    }

    /// DESIGN 00227628, DECIDED BY DAVID 2026-10-09 (item 64576122): a
    /// probe that reads the record through the reader, on a host holding
    /// no reader credential, is refused at a hand door BEFORE it runs —
    /// its reads would go out with no token, which the machine gate
    /// refuses under enforce — and the refusal names the forge's
    /// unattended door, which is where such a probe is proven. A probe
    /// that reads only the tree still runs, and so does any probe on a
    /// host that holds the credential (the door opens for it).
    #[test]
    fn a_record_read_with_no_reader_credential_is_refused_before_it_runs() {
        for reads in [
            "boss-sor-read /api/jobs/abc | jq -r .status",
            "n=$(boss-sor-read '/api/jobs?kind=x' | jq '.total // empty'); echo \"$n\"",
            "set -e\nboss-sor-read /api/yard/status >/dev/null && echo ok",
        ] {
            for door in [Door::Hand, Door::Recheck, Door::Disproved] {
                let r = record_read_refusal(reads, door, true).unwrap_or_else(|| {
                    panic!("{door:?}: a record read with no credential would run: {reads}")
                });
                for named in [
                    boss_jobs::probe::SOR_READER,
                    ARRIVAL_PROBE_RULE,
                    HOURLY_PROBE_RULE,
                    "00227628",
                ] {
                    assert!(r.contains(named), "{door:?} does not name {named}: {r}");
                }
                assert_eq!(
                    record_read_refusal(reads, door, false),
                    None,
                    "{door:?}: a host holding the credential opens the door: {reads}"
                );
            }
            // The forge's own door is the one this refusal points at; it
            // is never refused here (its credential is open()'s business).
            assert_eq!(record_read_refusal(reads, Door::Unattended, true), None);
        }
        for tree_only in [
            "git show HEAD:crates/x.rs | grep -q 'pub fn y' && echo y:ok",
            "echo boss-sor-read is the reader",
            "git show HEAD:infra/forge/probe-bin/boss-sor-read | grep -c curl",
        ] {
            assert_eq!(
                record_read_refusal(tree_only, Door::Hand, true),
                None,
                "only a probe that RUNS the reader reads the record: {tree_only}"
            );
        }
    }

    /// The same refusal through the doors' own shells: every hand door
    /// built on a host with no credential file refuses the record read,
    /// and the same doors on a host holding one do not. A dangling link
    /// is a deposit that broke, not a host without one — `open` refuses
    /// it as the environment's, so it is not refused HERE as absent.
    #[test]
    fn every_hand_door_refuses_a_record_read_on_a_host_with_no_reader_credential() {
        let held = tempfile::tempdir().unwrap();
        let absent = held.path().join("never-deposited.credential");
        let present = held.path().join("deposited.credential");
        std::fs::write(&present, "x").unwrap();
        let broken = held.path().join("broken.credential");
        std::os::unix::fs::symlink(held.path().join("gone"), &broken).unwrap();
        let probe = "boss-sor-read /api/jobs/abc | jq -r .status";
        for (name, door) in [
            ("hand", Door::Hand),
            ("recheck", Door::Recheck),
            ("disproved", Door::Disproved),
        ] {
            let shell = |credential: &Path| {
                the_four_doors("http://sor.invalid:7900", credential)
                    .into_iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, s)| s)
                    .unwrap()
            };
            let e = refuse_a_record_read_without_a_reader(probe, &shell(&absent), door)
                .expect_err(name);
            let said = format!("{e:#}");
            assert!(
                said.contains(&absent.display().to_string()),
                "{name}: {said}"
            );
            assert!(said.contains(HOURLY_PROBE_RULE), "{name}: {said}");
            assert!(
                refuse_a_record_read_without_a_reader(probe, &shell(&present), door).is_ok(),
                "{name}: a host holding the credential is refused"
            );
            assert!(
                refuse_a_record_read_without_a_reader(probe, &shell(&broken), door).is_ok(),
                "{name}: a broken deposit is open()'s refusal, not absence"
            );
            // A shell that names no reader at all can open no door.
            let bare = Shell {
                reader: None,
                ..shell(&present)
            };
            assert!(refuse_a_record_read_without_a_reader(probe, &bare, door).is_err());
        }
    }

    /// WHERE IT IS ASKED: at each hand door between the shell and the
    /// run, so nothing is sent before the refusal — and not at the
    /// unattended door, the forge's, which is the one it names. A door
    /// that moved the call after `execute_with`, or dropped it, would
    /// send the reads the decision forbids.
    #[test]
    fn each_hand_door_asks_before_it_runs_and_the_unattended_door_does_not() {
        let mut seen = Vec::new();
        for src in [include_str!("prove.rs"), include_str!("disprove.rs")] {
            let prod = src.split("#[cfg(test)]").next().unwrap_or("");
            let lines: Vec<&str> = prod.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if !l.contains("execute_with(&probe, &shell)") {
                    continue;
                }
                let door = lines[i..lines.len().min(i + 40)]
                    .iter()
                    .find_map(|n| n.split("Door::").nth(1))
                    .map(|r| {
                        r.chars()
                            .take_while(char::is_ascii_alphanumeric)
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                let start = lines[..i]
                    .iter()
                    .rposition(|p| {
                        p.contains(".with_probe_reader(")
                            || p.contains("unattended_ground(Shell::unattended")
                    })
                    .unwrap_or(0);
                let between = lines[start..i].join("\n").replace("prove::", "");
                let flat: String = between.chars().filter(|c| !c.is_whitespace()).collect();
                let asked = flat.contains(&format!(
                    "refuse_a_record_read_without_a_reader(&probe,&shell,Door::{door})"
                ));
                seen.push((door, asked));
            }
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("Disproved".to_string(), true),
                ("Hand".to_string(), true),
                ("Recheck".to_string(), true),
                ("Unattended".to_string(), false),
            ]
        );
    }

    /// The two rules the refusal names are the forge door's own rows:
    /// a rename there that left the sentence naming a rule nobody runs
    /// is the drift CLAUDE.md §9a pins.
    #[test]
    fn the_forge_door_the_refusal_names_is_the_rules_on_disk() {
        for (name, file) in [
            (
                ARRIVAL_PROBE_RULE,
                include_str!(
                    "../../../../infra/dispatcher/rules/run-car-probes-on-train-arrived.toml"
                ),
            ),
            (
                HOURLY_PROBE_RULE,
                include_str!(
                    "../../../../infra/dispatcher/rules/recheck-failing-probes-hourly.toml"
                ),
            ),
        ] {
            assert!(
                file.contains(&format!("name = \"{name}\"")),
                "the rule file does not define {name}"
            );
        }
    }
}

#[cfg(test)]
mod independent_observer_error_review {
    use super::*;

    #[test]
    fn independent_review_post_run_observer_refusal_keeps_process_evidence() {
        use crate::probe_reader::fixture;
        let base = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let unmounted = fixture::enforcing_with_reader(None);
        let mounted = fixture::enforcing_with_reader(Some(fixture::VALUE));
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), fixture::VALUE, 0o600);
        let root = boss_testing::repo_root();
        let make_shell = |port| {
            let ports = format!("events={port}");
            let mut shell = Shell::here(None).with_probe_reader(Some(&root), &base.base);
            let reader = shell.reader.as_mut().unwrap();
            reader.credential = credential.clone();
            reader.ports = ports.clone();
            shell.env.retain(|(key, _)| key != "BOSS_SOR_PORTS");
            shell.env.push(("BOSS_SOR_PORTS".into(), ports));
            shell
        };
        let seen = "printf 'ORIGINAL-OBSERVER-STDOUT-a8f30a42\\n'; printf 'ORIGINAL-OBSERVER-STDERR-a8f30a42\\n' >&2; boss-sor-read /api/events/tail >/dev/null; exit 2";
        assert!(admit(seen, true).refusal.is_none());
        let car =
            json!({"metadata":{"waits_on":{"on":"synthetic event", "seen":seen,"owner":"world"}}});
        let attempt = json!({"exit":75,"not_yet":true});
        let positive = with_wait_observed(
            attempt.clone(),
            &car,
            &make_shell(mounted.port),
            None,
            "2026-10-15T07:00:00Z",
        );
        eprintln!("QUALIFIED READER CONTROL: {positive}");
        assert_eq!(positive["waits_on_raw_exit"], 2);
        assert_eq!(
            positive["waits_on_stdout"],
            "ORIGINAL-OBSERVER-STDOUT-a8f30a42\n"
        );
        assert!(
            positive["waits_on_stderr"]
                .as_str()
                .unwrap()
                .contains("ORIGINAL-OBSERVER-STDERR-a8f30a42")
        );
        let refused = with_wait_observed(
            attempt,
            &car,
            &make_shell(unmounted.port),
            None,
            "2026-10-15T07:00:00Z",
        );
        eprintln!("POST-EXECUTION REFUSAL: {refused}");
        assert_eq!(refused["waits_on_exit"], Value::Null);
        assert_eq!(refused["waits_on_seen_at"], Value::Null);
        assert_eq!(refused["waits_on_observation"], Value::Null);
        assert!(
            refused["waits_on_error"]
                .as_str()
                .unwrap()
                .contains(RAN_WITHOUT_A_READ)
        );
        assert_eq!(
            boss_jobs::car::wait_observation(&refused),
            boss_jobs::car::WaitObservation::Unread
        );
        assert_eq!(
            refused["waits_on_stdout"], "ORIGINAL-OBSERVER-STDOUT-a8f30a42\n",
            "an actual executed observer's stdout must survive its reader refusal"
        );
        assert!(
            refused["waits_on_stderr"]
                .as_str()
                .unwrap()
                .contains("ORIGINAL-OBSERVER-STDERR-a8f30a42")
        );
        assert_eq!(refused["waits_on_raw_exit"], 2);
    }

    #[test]
    fn observer_missing_tools_preserve_raw_streams_and_separate_diagnosis() {
        let seen = "printf 'original observer stdout\\n'; printf 'original observer stderr\\n' >&2; synthetic_missing_observer_tool_716f";
        let shell = Shell::here(None);
        let raw = execute_with(seen, &shell).unwrap();
        assert_eq!(raw.missing_tools, ["synthetic_missing_observer_tool_716f"]);
        let car =
            json!({"metadata": {"waits_on": {"on": "the event", "seen": seen, "owner": "world"}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &shell,
            None,
            "T1",
        );
        assert_eq!(observed["waits_on_stdout"], raw.stdout);
        assert_eq!(
            observed["waits_on_stderr"], raw.stderr,
            "diagnosis cannot rewrite the native stderr"
        );
        assert_eq!(observed["waits_on_missing_tools"], json!(raw.missing_tools));
        assert_eq!(observed["waits_on_raw_exit"], raw.exit);
        assert_eq!(observed["waits_on_exit"], Value::Null);
        assert_eq!(
            boss_jobs::car::wait_observation(&observed),
            boss_jobs::car::WaitObservation::Unread
        );
    }

    #[test]
    fn refused_observer_records_admission_without_inventing_process_streams() {
        let seen = "git log -1 --format=%cI";
        let refusal = admit(seen, true).refusal.unwrap();
        let car =
            json!({"metadata": {"waits_on": {"on": "the event", "seen": seen, "owner": "world"}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &Shell::here(None),
            None,
            "T1",
        );
        assert_eq!(
            observed["waits_on_refusal"],
            json!({"rule": refusal.rule, "text": refusal.text})
        );
        assert_eq!(observed["waits_on_stdout"], Value::Null);
        assert_eq!(observed["waits_on_stderr"], Value::Null);
        assert_eq!(observed["waits_on_raw_exit"], Value::Null);
        assert_eq!(observed["waits_on_exit"], Value::Null);
        assert_eq!(
            boss_jobs::car::wait_observation(&observed),
            boss_jobs::car::WaitObservation::Unread
        );
    }

    #[test]
    fn observer_environment_failure_preserves_the_error_without_process_streams() {
        let root = boss_testing::scratch::scratch_dir("observer-missing-cwd");
        let absent = root.join(uuid::Uuid::new_v4().to_string());
        let shell = Shell::here(Some(&absent));
        let error = execute_with("true", &shell).unwrap_err();
        let car = json!({"metadata": {"waits_on": {"on": "the event", "seen": "true", "owner": "world"}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &shell,
            None,
            "T1",
        );
        assert_eq!(observed["waits_on_error"], format!("{error:#}"));
        assert_eq!(observed["waits_on_stdout"], Value::Null);
        assert_eq!(observed["waits_on_stderr"], Value::Null);
        assert_eq!(observed["waits_on_raw_exit"], Value::Null);
        assert_eq!(observed["waits_on_exit"], Value::Null);
        assert_eq!(
            boss_jobs::car::wait_observation(&observed),
            boss_jobs::car::WaitObservation::Unread
        );
    }

    #[test]
    fn observer_streams_keep_trailing_bytes_beyond_the_display_limit() {
        let stdout = format!(
            "{}\n\n",
            json!({"observation": "unseen", "padding": "x".repeat(MAX_STREAM + 32)})
        );
        let stderr = format!("{}\n\n", "observer warning ".repeat(MAX_STREAM / 8));
        assert!(stdout.len() > MAX_STREAM && stderr.len() > MAX_STREAM);
        let seen = format!("printf '%s' '{stdout}'; printf '%s' '{stderr}' >&2; exit 1");
        let car =
            json!({"metadata": {"waits_on": {"on": "the event", "seen": seen, "owner": "world"}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &Shell::here(None),
            None,
            "T1",
        );
        assert_eq!(observed["waits_on_stdout"], stdout);
        assert_eq!(observed["waits_on_stderr"], stderr);
        assert_eq!(observed["waits_on_raw_exit"], 1);
        assert_eq!(observed["waits_on_observation"], "unseen");
        assert_eq!(
            boss_jobs::car::wait_observation(&observed),
            boss_jobs::car::WaitObservation::Unseen
        );
    }

    #[test]
    fn a_missing_observer_tool_cannot_be_hidden_by_a_successful_shell() {
        let seen = "printf '%s' '{\"observation\":\"unseen\"}'; synthetic_missing_observer_tool_716f || true";
        let car =
            json!({"metadata": {"waits_on": {"on": "the event", "seen": seen, "owner": "world"}}});
        let observed = with_wait_observed(
            json!({"exit": 75, "not_yet": true}),
            &car,
            &Shell::here(None),
            None,
            "T1",
        );
        assert_eq!(observed["waits_on_raw_exit"], 0);
        assert_eq!(observed["waits_on_exit"], Value::Null);
        assert_eq!(observed["waits_on_seen_at"], Value::Null);
        assert_eq!(observed["waits_on_stdout"], "{\"observation\":\"unseen\"}");
        assert_eq!(
            observed["waits_on_missing_tools"],
            json!(["synthetic_missing_observer_tool_716f"])
        );
        assert_eq!(
            boss_jobs::car::wait_observation(&observed),
            boss_jobs::car::WaitObservation::Unread
        );
    }

    #[test]
    fn a_seen_writer_preserves_success_negative_and_error_as_distinct_outcomes() {
        for code in [0, 1, 2] {
            let seen = if code == 2 {
                let missing = std::env::current_dir().unwrap().join(format!(
                    "synthetic-observer-absent-{}",
                    uuid::Uuid::new_v4()
                ));
                assert!(!missing.exists());
                format!("grep -q synthetic_event '{}'", missing.display())
            } else if code == 1 {
                "printf '%s' '{\"observation\":\"unseen\"}'; exit 1".into()
            } else {
                format!("printf 'synthetic observer status {code}\\n'; exit {code}")
            };
            let raw = std::process::Command::new("sh")
                .args(["-c", &seen])
                .output()
                .unwrap();
            eprintln!(
                "BOUNDED ORIGINAL {code}: exit={:?}, stdout={:?}, stderr={:?}",
                raw.status.code(),
                String::from_utf8_lossy(&raw.stdout),
                String::from_utf8_lossy(&raw.stderr)
            );
            assert_eq!(raw.status.code(), Some(code));
            let car = json!({"metadata": {
                "proof_probe": "check-proof",
                "waits_on": {"on": "the event", "seen": seen, "owner": "world"}
            }});
            let attempt = with_wait_observed(
                json!({"at": "2026-09-19T12:00:00Z", "exit": 75, "not_yet": true, "probe": "check-proof", "not_yet_since": "2026-09-19T11:00:00Z", "not_yet_runs": 2}),
                &car,
                &Shell::here(None),
                None,
                "2026-09-19T12:00:00Z",
            );
            eprintln!("ACTUAL EXECUTED OBSERVER {code}: {attempt}");
            assert_eq!(attempt["waits_on_exit"], code);
            assert_eq!(attempt["waits_on_raw_exit"], code);
            assert_eq!(
                attempt["waits_on_observation"],
                if code == 1 {
                    json!("unseen")
                } else {
                    Value::Null
                }
            );
            assert_eq!(
                attempt["waits_on_stdout"],
                json!(String::from_utf8_lossy(&raw.stdout))
            );
            assert_eq!(
                attempt["waits_on_stderr"],
                json!(String::from_utf8_lossy(&raw.stderr))
            );
            let mut metadata = car["metadata"].clone();
            metadata["proof_attempt"] = attempt;
            let qualified = boss_jobs::car::owned_wait(&metadata);
            if code == 1 {
                assert!(
                    qualified.is_some(),
                    "executed negative remains positive control"
                );
            } else {
                assert!(
                    qualified.is_none(),
                    "seen success or observer execution error is not an unseen-condition wait: code={code}, actual={qualified:?}"
                );
            }
        }
    }

    #[test]
    fn legacy_nonzero_or_malformed_negative_is_unread_not_an_observed_wait() {
        for seen in [
            "exit 1",
            "printf invalid; exit 1",
            "printf '%s' '{\"observation\":\"unseen\"}'; exit 2",
            "printf '%s' '{\"observation\":\"unseen\"}{\"observation\":\"unseen\"}'; exit 1",
            "printf '%s' '{\"observation\":true}'; exit 1",
            "printf '%s' '[{\"observation\":\"unseen\"}]'; exit 1",
            "printf '%s' '{\"observation\":\"unseen\"}extra'; exit 1",
        ] {
            let car = json!({"metadata": {"waits_on": {"on": "an event", "seen": seen, "owner": "world"}}});
            let attempt = with_wait_observed(
                json!({"not_yet": true, "exit": 75}),
                &car,
                &Shell::here(None),
                None,
                "T1",
            );
            let mut md = car["metadata"].clone();
            md["proof_attempt"] = attempt;
            assert!(boss_jobs::car::owned_wait(&md).is_none(), "{md}");
        }
    }
}
