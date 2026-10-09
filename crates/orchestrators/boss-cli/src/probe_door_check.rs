//! `boss probe-door-check` — as root, does the reader door's socket
//! hand-over hold ON THIS HOST? The one definition of the
//! `probe-door-check` ops verb (backlog e65dde24, from review 991bb439).
//!
//! WHY IT IS IN THE BINARY. The door a recorded probe reads through
//! (`probe_reader`) is made by root and handed to the probe account, and
//! the hand-over is where the first cut was exploitable: a chown that
//! followed a link, inside a directory the account owned, gave the
//! account any file on the host (review 0bd6a9c2, B2). The repair is
//! held by its shape and by a source pin. Its test BY EFFECT —
//! `root_hands_over_the_socket_and_nothing_a_planted_link_points_at` —
//! needs a root that holds CAP_CHOWN, and has never run anywhere: the
//! dev pod's root has no such capability, the gate runs as uid 65534,
//! and the forge host, whose root can chown, has no cargo. So the check
//! ships in the CLI the forge already installs from the converged image,
//! and the host runs it on itself, through an ops-request, BEFORE any
//! reader credential is deposited there.
//!
//! WHAT IT DOES. It makes a scratch directory under the temp directory
//! that only root can write, and inside it one directory that is sticky
//! and open like the temp directory itself, so the kernel rules under
//! test are the ones a door meets. Then it:
//!
//! 1. plants the attacker's link: a door-shaped directory whose
//!    `reader.sock` is a symlink to a file standing in for a secret, and
//!    hands that name over with the door's own function
//!    (`probe_reader::hand_socket_to`). The secret must still be root's.
//! 2. makes a door the way `Guard::start` does
//!    (`probe_reader::door_directory_in`, `probe_reader::bind_socket`),
//!    handed to the account the host NAMES for `boss prove --unattended`
//!    (`BOSS_PROBE_USER`), and stands a listener of its own behind it
//!    that answers a fixed line.
//! 3. AS THAT ACCOUNT, entered the way the door enters it (`runuser -u`
//!    under `timeout`): connects and reads; then tries to plant a link
//!    in the directory, replace the socket with a link, remove it,
//!    rename it, and rename the directory away. Each is judged by what
//!    root then finds on disk, never by the command's exit.
//! 4. as a THIRD account: tries to connect, and the listener must not
//!    have been reached.
//! 5. reads whose file the door's port list is (`BOSS_PROBE_DIR`'s
//!    `infra/forge/sor-ports.env`) and judges it by the door's own rule
//!    (`probe_reader::ports_list_refusal`): root's, and closed to group
//!    and other.
//!
//! WHAT IT NEVER DOES. It opens no reader credential and no machine
//! token — it does not call the door's `open`, which is the only thing
//! that reads one — and it speaks to no network: the listener is its
//! own, on a socket in its scratch directory. The commands it runs as
//! other accounts get an empty environment but PATH.
//!
//! IT WRITES NOTHING THROUGH A NAME SOMEBODY ELSE COULD HAVE PUT THERE
//! (review cd3f6a99, B1). The first cut opened the whole scratch
//! directory to every account and THEN wrote its two fixture files by
//! name, with calls that follow a link and truncate: a link standing at
//! `capability` or `stands-in-for-a-secret` had its target truncated or
//! overwritten by root, and only `fs.protected_symlinks` — a kernel
//! setting nobody had read on the forge — stood in the way. Narrowing
//! that would leave it open, so there is no window, the same way the
//! hand-over has none:
//!
//! * the scratch directory stays root's alone to write (0711: others
//!   may pass through it, and can put nothing in it);
//! * both fixture files are made there with `create_new` — which never
//!   follows a link and refuses a name already present — at 0600 from
//!   the moment they exist; nothing is chmod'ed by path afterwards;
//! * the one place other accounts may write is a directory inside it
//!   (`open/`), and root makes only door directories there, each by
//!   `mkdtemp` under a random name, exactly as a door is made in /tmp;
//! * a name that IS already there is not written, not removed, and ends
//!   the check as REFUSED or FAILED naming it.
//!
//! EXIT. 0 every effect held; 1 one did not — the first is named on a
//! FAIL line and nothing after it is claimed; 2 the check could not be
//! made here (not root, a root that cannot chown, no account named, a
//! tool missing), said as REFUSED and never as a pass. The LAST line is
//! the one verdict: HELD, FAILED or REFUSED. A kill mid-run leaves the
//! scratch directory behind, root's and holding nothing but fixtures;
//! its name starts `boss-probe-door-check-`.
//!
//! WHAT IT CANNOT SHOW is what the unit test's own comment says of
//! itself: no two processes are raced here, so it shows there is no step
//! whose outcome timing could change, not a race lost.

use crate::probe_reader::{
    PortsSource, bind_socket, door_directory_in, hand_socket_to, ports_list_refusal,
    read_ports_file,
};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const HELD_EXIT: i32 = 0;
pub(crate) const FAILED_EXIT: i32 = 1;
pub(crate) const REFUSED_EXIT: i32 = 2;

const ME: &str = "probe-door-check";
/// What the check's own listener answers, and so what a connect that
/// really went through the handed-over socket reads back.
const ANSWER: &str = "boss-probe-door-check: reached";
/// Seconds one command run as another account may take.
const COMMAND_BOUND: &str = "10";
/// The third account, in the order tried: one that is neither root nor
/// the probe account.
const STRANGERS: [&str; 2] = ["nobody", "daemon"];
/// What must be on PATH: how the door enters the account, and the
/// commands the account is made to try.
const TOOLS: [&str; 7] = ["id", "runuser", "timeout", "curl", "ln", "mv", "rm"];
/// The two files root makes in the scratch directory, and the one
/// directory in it that other accounts may write in.
const CAPABILITY: &str = "capability";
const SECRET: &str = "stands-in-for-a-secret";
const OPEN: &str = "open";

/// How a check ended, with the lines it printed on the way.
#[derive(Debug)]
pub(crate) struct Report {
    pub exit: i32,
    pub lines: Vec<String>,
}

/// What the HOST names for the unattended door, read from the ops
/// runner's environment: the account a probe runs as and the checkout
/// it runs in. `None` is a name the host did not give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Named {
    pub account: Option<String>,
    pub view: Option<PathBuf>,
}

impl Named {
    fn by_this_host() -> Self {
        let (account, named) = crate::prove::probe_user();
        Self {
            account: named.then_some(account),
            view: std::env::var_os("BOSS_PROBE_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// WHAT ROOT FOUND, each a fact read from disk or from the listener
/// after the act it follows. [`effects`] judges them and does nothing
/// else, so the judgement is held by tests on every uid while the facts
/// can be gathered only by a root that chowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Facts {
    pub account: String,
    pub uid: u32,
    pub stranger: String,
    /// Owner of the file the planted link points at, before and after
    /// the hand-over of the link's name.
    pub secret_owner: (u32, u32),
    /// The planted name itself, and the directory it sits in.
    pub planted_name_owner: u32,
    pub planted_directory: (u32, u32),
    /// The real door after the hand-over: socket owner and mode,
    /// directory owner and mode.
    pub socket: (u32, u32),
    pub directory: (u32, u32),
    /// Did the account read [`ANSWER`] through the socket, and how many
    /// connections the listener had taken by then.
    pub account_read: (bool, usize),
    /// Each thing the account tried, and whether root found the door
    /// exactly as it was afterwards.
    pub attempts: Vec<(&'static str, bool)>,
    /// The secret's owner, and the door's directory, after all of it.
    pub secret_owner_after: u32,
    pub directory_after: (u32, u32),
    /// The account reads again once it has tried everything.
    pub account_read_again: (bool, usize),
    /// Did the third account read, and the listener's count afterwards.
    pub stranger_read: (bool, usize),
}

/// One effect: what must hold, and what was found when it did not.
type Effect = (String, Result<(), String>);

/// THE JUDGEMENT, in the order the acts were made. Pure.
pub(crate) fn effects(f: &Facts) -> Vec<Effect> {
    let Facts {
        account,
        uid,
        stranger,
        ..
    } = f;
    let held = |ok: bool, saw: String| if ok { Ok(()) } else { Err(saw) };
    let mut out = vec![
        (
            "planted link: what it points at is still root's".to_string(),
            held(
                f.secret_owner == (0, 0),
                format!(
                    "owner uid {} before the hand-over, uid {} after",
                    f.secret_owner.0, f.secret_owner.1
                ),
            ),
        ),
        (
            format!("planted link: the name itself went to {account}"),
            held(
                f.planted_name_owner == *uid,
                format!(
                    "the name is uid {}'s, not uid {uid}'s",
                    f.planted_name_owner
                ),
            ),
        ),
        (
            "planted link: its directory stayed root's, open to traversal alone".to_string(),
            held(
                f.planted_directory == (0, 0o711),
                format!(
                    "owner uid {}, mode {:04o}",
                    f.planted_directory.0, f.planted_directory.1
                ),
            ),
        ),
        (
            format!("door: the socket is {account}'s alone"),
            held(
                f.socket == (*uid, 0o600),
                format!("owner uid {}, mode {:04o}", f.socket.0, f.socket.1),
            ),
        ),
        (
            "door: its directory stayed root's, open to traversal alone".to_string(),
            held(
                f.directory == (0, 0o711),
                format!("owner uid {}, mode {:04o}", f.directory.0, f.directory.1),
            ),
        ),
        (
            format!("{account} connects through the socket and is answered"),
            held(
                f.account_read == (true, 1),
                format!(
                    "answered: {}; connections the listener took: {}",
                    f.account_read.0, f.account_read.1
                ),
            ),
        ),
    ];
    for (attempt, unchanged) in &f.attempts {
        out.push((
            format!("{account} cannot {attempt}"),
            held(
                *unchanged,
                "root found the door changed afterwards".to_string(),
            ),
        ));
    }
    out.extend([
        (
            "after every attempt, the secret is still root's".to_string(),
            held(
                f.secret_owner_after == 0,
                format!("owner uid {}", f.secret_owner_after),
            ),
        ),
        (
            "after every attempt, the door's directory is as it was".to_string(),
            held(
                f.directory_after == (0, 0o711),
                format!(
                    "owner uid {}, mode {:04o}",
                    f.directory_after.0, f.directory_after.1
                ),
            ),
        ),
        (
            format!("after every attempt, {account} still connects"),
            held(
                f.account_read_again == (true, 2),
                format!(
                    "answered: {}; connections the listener took: {}",
                    f.account_read_again.0, f.account_read_again.1
                ),
            ),
        ),
        (
            format!("{stranger}, a third account, cannot connect"),
            held(
                f.stranger_read == (false, 2),
                format!(
                    "answered: {}; connections the listener took: {}",
                    f.stranger_read.0, f.stranger_read.1
                ),
            ),
        ),
    ]);
    out
}

/// WHOSE FILE THE DOOR'S PORT LIST IS (review cd3f6a99, N5), judged by
/// the door's own rule for a door root hands to another account. Root
/// sends the reader credential to every port the list names, so the
/// list must be root's and closed to group and other — and until this
/// line nothing had read that file's owner or mode on a host. Pure:
/// `view` is the checkout the host names (`BOSS_PROBE_DIR`), `found`
/// what one open of the list in it gave.
pub(crate) fn ports_effect(view: Option<&Path>, found: Option<&PortsSource>) -> Effect {
    let Some(view) = view else {
        return (
            "the door's port list is root's alone".to_string(),
            Err(
                "BOSS_PROBE_DIR is unset here, so the door would read its port list out of \
                 the checkout owner's own tree"
                    .to_string(),
            ),
        );
    };
    let list = view.join(crate::prove::SOR_PORTS_ENV);
    let name = format!("the door's port list {} is root's alone", list.display());
    let held = match found {
        None => Err(
            "it is absent or unreadable, so a door here would serve the jobs port alone"
                .to_string(),
        ),
        Some(source) => match ports_list_refusal(source, true) {
            Some(cause) => Err(cause),
            None => Ok(()),
        },
    };
    let name = match (found, &held) {
        (Some(source), Ok(())) => format!("{name} (uid {}, mode {:04o})", source.uid, source.mode),
        _ => name,
    };
    (name, held)
}

/// The scratch directory's removal, as the last effect: judged BEFORE
/// the verdict is printed, so a run prints one verdict and a removal
/// that failed cannot follow a line that says HELD (review cd3f6a99,
/// N3 — any account can make the removal of a directory it may write in
/// worth checking).
pub(crate) fn scratch_effect(at: &Path, removed: bool) -> Effect {
    (
        format!("the scratch directory {} is removed", at.display()),
        if removed {
            Ok(())
        } else {
            Err("it is still there".to_string())
        },
    )
}

/// The lines and the exit for a run's effects: `ok` down to the first
/// that did not hold, which is named and ends the list — an effect
/// after a failed one was observed on a door already known broken, and
/// is not claimed either way. `cleanup` is the scratch directory's
/// removal, always reported, and the verdict is the last line: FAILED
/// at the first effect that did not hold, the removal included.
pub(crate) fn judge(account: &str, effects: &[Effect], cleanup: &Effect) -> Report {
    let mut lines = Vec::new();
    let mut failed: Option<&str> = None;
    let mut held = 0;
    let mut say = |(name, found): &'_ Effect, lines: &mut Vec<String>| match found {
        Ok(()) => {
            lines.push(format!("ok    {name}"));
            held += 1;
            true
        }
        Err(saw) => {
            lines.push(format!("FAIL  {name} — {saw}"));
            false
        }
    };
    for effect in effects {
        if !say(effect, &mut lines) {
            failed = Some(&effect.0);
            break;
        }
    }
    if !say(cleanup, &mut lines) {
        failed = failed.or(Some(&cleanup.0));
    }
    match failed {
        Some(name) => {
            lines.push(format!(
                "{ME}: FAILED at `{name}`. The reader door would not hold on this host as it \
                 stands; deposit no reader credential here."
            ));
            Report {
                exit: FAILED_EXIT,
                lines,
            }
        }
        None => {
            lines.push(format!(
                "{ME}: HELD — {held} effects, for {account}, by what root found on disk."
            ));
            Report {
                exit: HELD_EXIT,
                lines,
            }
        }
    }
}

/// A refusal: the check was not made, and that is not a pass.
fn refused(mut lines: Vec<String>, why: String) -> Report {
    lines.push(format!(
        "{ME}: REFUSED — {why}. Nothing was checked; this is not a pass."
    ));
    Report {
        exit: REFUSED_EXIT,
        lines,
    }
}

/// Why a process with this uid cannot make the check, if it cannot.
pub(crate) fn not_root(uid: u32) -> Option<String> {
    (uid != 0).then(|| {
        format!(
            "this process is uid {uid}, not root; only root hands a socket to another account, \
             so only root can check that hand-over (file it: boss ops forge probe-door-check \
             --wait)"
        )
    })
}

/// WHO PLAYS WHOM, or why the check cannot be cast. Pure: `uid` is the
/// named account's id when the host has such an account, `others` the
/// candidate third accounts with theirs.
///
/// THE ACCOUNT MUST BE NAMED BY THE HOST (review cd3f6a99, N4). The
/// unattended door falls back to the checkout's owner when
/// `BOSS_PROBE_USER` is unset, and a check that fell back with it could
/// end HELD, exit 0, about an account that is not the probe account —
/// with one word on its first line to say so. This check exists to
/// prove the hand-over to the PROBE ACCOUNT before a credential is
/// deposited, so with none named it refuses.
///
/// Root is not an account to hand to, and the third account is neither
/// root nor the first: a "stranger" that is the probe account would
/// connect, and one that is root would connect to anything.
pub(crate) fn cast(
    account: Option<&str>,
    uid: Option<u32>,
    others: &[(&'static str, Option<u32>)],
) -> Result<(u32, &'static str, u32), String> {
    let Some(account) = account else {
        return Err(
            "BOSS_PROBE_USER is unset here, so this host names no probe account to check; the \
             door's fallback is the checkout's owner, and a hand-over to that account proves \
             nothing about the probe account (infra/forge/probe-account.sh writes the drop-in \
             that names it)"
                .to_string(),
        );
    };
    let Some(uid) = uid else {
        return Err(format!(
            "this host has no account `{account}` to hand a socket to"
        ));
    };
    if uid == 0 {
        return Err(format!("the probe account `{account}` is root itself"));
    }
    others
        .iter()
        .filter_map(|(name, other)| other.map(|other| (*name, other)))
        .find(|(_, other)| *other != 0 && *other != uid)
        .map(|(name, other)| (uid, name, other))
        .ok_or_else(|| {
            format!(
                "this host has no third account ({}) that is neither root nor {account}",
                others
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// A FILE OF ROOT'S THAT DID NOT EXIST BEFORE THIS CALL, private from
/// the moment it does. `create_new` is O_CREAT|O_EXCL: it never follows
/// a link standing at the name and refuses any name already there, so
/// nothing this writes can land in a file somebody else chose; and the
/// mode is the open's own, so there is no chmod by path to redirect.
fn new_private_file(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body.as_bytes())
}

/// Can this process give a file to `uid` — does it hold CAP_CHOWN? Asked
/// by doing it, to a file it has just made in `scratch`. Being uid 0 is
/// not the answer: the dev pod's root holds no such capability. `Err`
/// when the file could not be made at all — a name already standing
/// there is not this check's to write through or to remove.
fn can_give_a_file_to(scratch: &Path, uid: u32) -> std::io::Result<bool> {
    let file = scratch.join(CAPABILITY);
    new_private_file(&file, "")?;
    let gave = std::os::unix::fs::lchown(&file, Some(uid), None).is_ok()
        && std::fs::symlink_metadata(&file).is_ok_and(|found| found.uid() == uid);
    let _ = std::fs::remove_file(&file);
    Ok(gave)
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(tool))
                .is_ok_and(|found| found.is_file() && found.mode() & 0o111 != 0)
        })
    })
}

fn uid_named(user: &str) -> Option<u32> {
    crate::probe_reader::uid_of(user).ok()
}

/// `words` as `user`, the way the door enters an account — `timeout`
/// around `runuser -u` — with nothing of this process's environment but
/// PATH. `None` when it could not be started at all.
fn run_as(user: &str, words: &[&std::ffi::OsStr]) -> Option<std::process::Output> {
    let mut command = std::process::Command::new("timeout");
    command
        .args(["-k", "2", COMMAND_BOUND, "runuser", "-u", user, "--"])
        .args(words)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir("/")
        .stdin(std::process::Stdio::null());
    command.output().ok()
}

/// THE METHOD'S OWN CONTROL: a command run as `user` must BE `user`.
/// Every "cannot" below is "a command run as that account changed
/// nothing", which is as true of an account `runuser` refuses to enter
/// as of one the kernel stopped. `Err` is what `id -u` said instead.
fn runs_as(user: &str, uid: u32) -> Result<(), String> {
    let said = run_as(user, &["id".as_ref(), "-u".as_ref()])
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    if said.as_deref() == Some(uid.to_string().as_str()) {
        return Ok(());
    }
    Err(format!(
        "a command run as {user} did not run as uid {uid} (`id -u` said {said:?}), so nothing \
         it failed to do would mean anything"
    ))
}

fn owner(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).map_or(u32::MAX, |found| found.uid())
}

fn owner_and_mode(path: &Path) -> (u32, u32) {
    std::fs::symlink_metadata(path)
        .map_or((u32::MAX, 0), |found| (found.uid(), found.mode() & 0o7777))
}

fn is_socket(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_socket())
}

fn absent(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err()
}

/// The check's own listener: answers every connection [`ANSWER`] over
/// HTTP and counts the connections it took.
struct Listener {
    taken: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    fn serve(listener: std::os::unix::net::UnixListener) -> std::io::Result<Self> {
        listener.set_nonblocking(true)?;
        let taken = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, stopping) = (taken.clone(), stop.clone());
        let worker = std::thread::Builder::new()
            .name("probe-door-check".into())
            .spawn(move || {
                use std::io::{Read, Write};
                while !stopping.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    };
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                    let mut asked = Vec::new();
                    let mut chunk = [0u8; 512];
                    while !asked.windows(4).any(|w| w == b"\r\n\r\n") && asked.len() < 8192 {
                        match stream.read(&mut chunk) {
                            Ok(n) if n > 0 => asked.extend_from_slice(&chunk[..n]),
                            _ => break,
                        }
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{ANSWER}",
                        ANSWER.len()
                    );
                }
            })?;
        Ok(Self {
            taken,
            stop,
            worker: Some(worker),
        })
    }

    fn taken(&self) -> usize {
        self.taken.load(Ordering::SeqCst)
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The read a probe's reader makes, aimed at `socket`: `curl -f` over a
/// Unix socket, as `boss-sor-read` does through `BOSS_SOR_DOOR`.
fn curl_words(socket: &Path) -> [&std::ffi::OsStr; 7] {
    [
        "curl".as_ref(),
        "-fsS".as_ref(),
        "--max-time".as_ref(),
        "5".as_ref(),
        "--unix-socket".as_ref(),
        socket.as_os_str(),
        "http://door.invalid/".as_ref(),
    ]
}

/// Was that read answered by the check's own listener?
fn answered(out: &std::process::Output) -> bool {
    out.status.success() && String::from_utf8_lossy(&out.stdout) == ANSWER
}

/// Does `user` read [`ANSWER`] through `socket`?
fn reads_through(user: &str, socket: &Path) -> bool {
    run_as(user, &curl_words(socket)).is_some_and(|out| answered(&out))
}

/// The one directory in the scratch that other accounts may write in,
/// made by root while the scratch is root's alone: `mkdir` refuses a
/// name already there and follows nothing, and the mode is set on the
/// directory it opened rather than on a name.
fn open_area_in(scratch: &Path) -> std::io::Result<PathBuf> {
    let open = scratch.join(OPEN);
    std::fs::create_dir(&open)?;
    let held = std::fs::File::open(&open)?;
    // mode-bits-ok: a directory given /tmp's own mode so the door meets the same kernel rules; nothing execs it
    held.set_permissions(std::fs::Permissions::from_mode(0o1777))?;
    Ok(open)
}

/// Make the acts and read the facts. `scratch` is root's alone to write
/// and `open` the sticky directory inside it. `Err` is an act of ROOT'S
/// that did not happen — a fixture that could not be made, the
/// hand-over itself refused or failed — named as the effect it would
/// have produced.
fn observe(
    scratch: &Path,
    open: &Path,
    account: &str,
    uid: u32,
    stranger: &str,
) -> Result<Facts, (String, String)> {
    let failed = |what: &str, error: &dyn std::fmt::Display| (what.to_string(), error.to_string());

    // 1. The planted link, with the door's own function alone.
    let secret = scratch.join(SECRET);
    new_private_file(&secret, "fixture, not a secret\n").map_err(|e| {
        failed(
            &format!("making the fixture secret {}", secret.display()),
            &e,
        )
    })?;
    let secret_before = owner(&secret);
    let planted = door_directory_in(open)
        .map_err(|e| failed("making the planted door's directory", &format!("{e:#}")))?;
    let planted_name = planted.path().join(crate::probe_reader::SOCKET_NAME);
    std::os::unix::fs::symlink(&secret, &planted_name)
        .map_err(|e| failed("planting the link", &e))?;
    hand_socket_to(planted.path(), &planted_name, uid).map_err(|e| {
        failed(
            "planted link: the hand-over of the planted name",
            &format!("{e:#}"),
        )
    })?;
    let secret_owner = (secret_before, owner(&secret));
    let planted_name_owner = owner(&planted_name);
    let planted_directory = owner_and_mode(planted.path());

    // 2. A door, made and handed over as `Guard::start` makes one.
    let door = door_directory_in(open)
        .map_err(|e| failed("making the door's directory", &format!("{e:#}")))?;
    let directory = door.path().to_path_buf();
    let (socket, listener) = bind_socket(&directory, Some(uid)).map_err(|e| {
        failed(
            "door: binding the socket and handing it over",
            &format!("{e:#}"),
        )
    })?;
    let listener =
        Listener::serve(listener).map_err(|e| failed("standing the check's listener", &e))?;
    let socket_facts = owner_and_mode(&socket);
    let directory_facts = owner_and_mode(&directory);

    // 3. As the account.
    let account_read = (reads_through(account, &socket), listener.taken());
    let inside = directory.join("planted");
    let moved = directory.join("elsewhere.sock");
    let away = open.join("door.away");
    let door_stands = || {
        is_socket(&socket)
            && owner_and_mode(&socket) == socket_facts
            && owner_and_mode(&directory) == directory_facts
    };
    let tries: [(&'static str, Vec<&std::ffi::OsStr>, &dyn Fn() -> bool); 5] = [
        (
            "plant a link in the door's directory",
            vec![
                "ln".as_ref(),
                "-s".as_ref(),
                secret.as_os_str(),
                inside.as_os_str(),
            ],
            &|| absent(&inside),
        ),
        (
            "replace the socket with a link",
            vec![
                "ln".as_ref(),
                "-sf".as_ref(),
                secret.as_os_str(),
                socket.as_os_str(),
            ],
            &|| true,
        ),
        (
            "remove the socket",
            vec!["rm".as_ref(), "-f".as_ref(), socket.as_os_str()],
            &|| true,
        ),
        (
            "rename the socket",
            vec!["mv".as_ref(), socket.as_os_str(), moved.as_os_str()],
            &|| absent(&moved),
        ),
        (
            "rename the door's directory away",
            vec!["mv".as_ref(), directory.as_os_str(), away.as_os_str()],
            &|| absent(&away),
        ),
    ];
    let mut attempts = Vec::new();
    for (name, words, also) in tries {
        // The command's exit is not read: what root finds is the answer.
        let _ = run_as(account, &words);
        attempts.push((name, door_stands() && also()));
    }
    let secret_owner_after = owner(&secret);
    let directory_after = owner_and_mode(&directory);
    let account_read_again = (reads_through(account, &socket), listener.taken());

    // 4. As a third account.
    let stranger_read = (reads_through(stranger, &socket), listener.taken());

    Ok(Facts {
        account: account.to_string(),
        uid,
        stranger: stranger.to_string(),
        secret_owner,
        planted_name_owner,
        planted_directory,
        socket: socket_facts,
        directory: directory_facts,
        account_read,
        attempts,
        secret_owner_after,
        directory_after,
        account_read_again,
        stranger_read,
    })
}

/// The whole check, with its scratch directory made in `parent` — the
/// temp directory in production — for the account and checkout the
/// host `named`.
pub(crate) fn check_in(parent: &Path, named: &Named) -> Report {
    let mut lines = Vec::new();
    let me = crate::own_temp::current_uid();
    if let Some(why) = not_root(me) {
        return refused(lines, why);
    }
    let account = named.account.as_deref();
    let others = STRANGERS.map(|name| (name, uid_named(name)));
    let (uid, stranger, stranger_uid) = match cast(account, account.and_then(uid_named), &others) {
        Ok(cast) => cast,
        Err(why) => return refused(lines, why),
    };
    let account = account.unwrap_or_default();
    if let Some(missing) = TOOLS.iter().find(|tool| !on_path(tool)) {
        return refused(lines, format!("`{missing}` is not on PATH"));
    }
    let scratch = match tempfile::Builder::new()
        .prefix("boss-probe-door-check-")
        .tempdir_in(parent)
    {
        Ok(scratch) => scratch,
        Err(error) => {
            return refused(
                lines,
                format!(
                    "no scratch directory could be made in {}: {error}",
                    parent.display()
                ),
            );
        }
    };
    // Root's alone to write, and passable: the accounts must reach the
    // open directory inside it and can put nothing beside it. Set on
    // the directory this process opened, which `mkdtemp` has just made
    // under a name nobody else can rename in a sticky parent.
    let closed = std::fs::File::open(scratch.path()).and_then(|held| {
        // mode-bits-ok: a directory's traversal bit for the accounts under test; nothing execs it
        held.set_permissions(std::fs::Permissions::from_mode(0o711))
    });
    let open = match closed.and_then(|()| open_area_in(scratch.path())) {
        Ok(open) => open,
        Err(error) => {
            return refused(
                lines,
                format!(
                    "the scratch directory {} could not be laid out: {error}",
                    scratch.path().display()
                ),
            );
        }
    };
    lines.push(format!(
        "{ME}: handing to {account} (uid {uid}, named by BOSS_PROBE_USER), a third account \
         {stranger} (uid {stranger_uid}), scratch {}",
        scratch.path().display()
    ));
    match can_give_a_file_to(scratch.path(), uid) {
        Ok(true) => {}
        Ok(false) => {
            return refused(
                lines,
                "this root cannot chown (no CAP_CHOWN), so there is no hand-over to check"
                    .to_string(),
            );
        }
        Err(error) => {
            return refused(
                lines,
                format!(
                    "the capability question could not be asked: {} could not be made ({error})",
                    scratch.path().join(CAPABILITY).display()
                ),
            );
        }
    }
    // The method's own controls, before any refusal is read as a
    // refusal — for BOTH accounts whose failures the lines below report.
    for (user, id) in [(account, uid), (stranger, stranger_uid)] {
        if let Err(why) = runs_as(user, id) {
            return refused(lines, why);
        }
        lines.push(format!("ok    method: a command run as {user} is uid {id}"));
    }
    let mut found = match observe(scratch.path(), &open, account, uid, stranger) {
        Ok(facts) => effects(&facts),
        Err((what, saw)) => vec![(what, Err(saw))],
    };
    let view = named.view.as_deref();
    let list = view.and_then(|view| read_ports_file(&view.join(crate::prove::SOR_PORTS_ENV)));
    found.push(ports_effect(view, list.as_ref().map(|(_, source)| source)));
    let at = scratch.path().to_path_buf();
    let removed = scratch.close().is_ok() && absent(&at);
    let report = judge(account, &found, &scratch_effect(&at, removed));
    lines.extend(report.lines);
    Report {
        exit: report.exit,
        lines,
    }
}

/// Print the report; the exit is the verdict.
pub(crate) fn run() -> i32 {
    // shared-tmp-ok: the root alone, in which the scratch directory gets a random mkdtemp name
    let report = check_in(&std::env::temp_dir(), &Named::by_this_host());
    for line in &report.lines {
        println!("{line}");
    }
    report.exit
}

/// Where the check's own scratch would be, for a test to look.
#[cfg(test)]
fn scratches_in(parent: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(parent)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts as a host where everything holds reports them.
    fn held() -> Facts {
        Facts {
            account: "boss-probe".into(),
            uid: 997,
            stranger: "nobody".into(),
            secret_owner: (0, 0),
            planted_name_owner: 997,
            planted_directory: (0, 0o711),
            socket: (997, 0o600),
            directory: (0, 0o711),
            account_read: (true, 1),
            attempts: vec![
                ("plant a link in the door's directory", true),
                ("replace the socket with a link", true),
                ("remove the socket", true),
                ("rename the socket", true),
                ("rename the door's directory away", true),
            ],
            secret_owner_after: 0,
            directory_after: (0, 0o711),
            account_read_again: (true, 2),
            stranger_read: (false, 2),
        }
    }

    fn view() -> PathBuf {
        PathBuf::from("/var/lib/boss/probe-view")
    }

    fn roots_list() -> PortsSource {
        PortsSource {
            path: view().join(crate::prove::SOR_PORTS_ENV),
            uid: 0,
            mode: 0o644,
        }
    }

    /// Every effect of a run, as [`check_in`] assembles them, with the
    /// scratch directory's removal apart.
    fn run_of(facts: &Facts, list: Option<&PortsSource>, removed: bool) -> Report {
        let mut found = effects(facts);
        found.push(ports_effect(Some(&view()), list));
        let scratch = scratch_effect(Path::new("the-checks-scratch"), removed);
        judge(&facts.account, &found, &scratch)
    }

    #[test]
    fn every_effect_holding_is_exit_zero_and_says_for_whom() {
        let report = run_of(&held(), Some(&roots_list()), true);
        assert_eq!(report.exit, HELD_EXIT, "{:#?}", report.lines);
        assert_eq!(report.lines.len(), 18, "{:#?}", report.lines);
        assert!(report.lines[..17].iter().all(|l| l.starts_with("ok    ")));
        assert!(
            report.lines[15].contains(
                "the door's port list /var/lib/boss/probe-view/infra/forge/sor-ports.env is \
                 root's alone (uid 0, mode 0644)"
            ),
            "{}",
            report.lines[15]
        );
        assert!(report.lines[16].contains("is removed"));
        let last = report.lines.last().unwrap();
        assert!(last.contains("HELD — 17 effects, for boss-probe"), "{last}");
    }

    /// ONE FACT WRONG AT A TIME: the list of effects stops at that one,
    /// which is named with what was found; the scratch directory's
    /// removal is still reported; the verdict is the LAST line, FAILED
    /// at that effect, exit 1 — and no line says HELD.
    #[test]
    fn the_first_effect_that_did_not_hold_is_named_and_ends_the_report() {
        type Break = fn(&mut Facts);
        let cases: [(Break, &str, &str); 15] = [
            (
                |f| f.secret_owner = (0, 997),
                "what it points at is still root's",
                "uid 997 after",
            ),
            (
                |f| f.planted_name_owner = 0,
                "the name itself went to boss-probe",
                "uid 0's",
            ),
            (
                |f| f.planted_directory = (997, 0o711),
                "planted link: its directory",
                "uid 997",
            ),
            (
                |f| f.socket = (997, 0o666),
                "the socket is boss-probe's alone",
                "mode 0666",
            ),
            (
                |f| f.directory = (0, 0o755),
                "door: its directory stayed root's",
                "mode 0755",
            ),
            (
                |f| f.account_read = (false, 0),
                "connects through the socket",
                "answered: false",
            ),
            (
                |f| f.attempts[0].1 = false,
                "cannot plant a link",
                "changed afterwards",
            ),
            (
                |f| f.attempts[1].1 = false,
                "cannot replace the socket",
                "changed afterwards",
            ),
            (
                |f| f.attempts[2].1 = false,
                "cannot remove the socket",
                "changed afterwards",
            ),
            (
                |f| f.attempts[3].1 = false,
                "cannot rename the socket",
                "changed afterwards",
            ),
            (
                |f| f.attempts[4].1 = false,
                "cannot rename the door's directory",
                "changed",
            ),
            (
                |f| f.secret_owner_after = 997,
                "the secret is still root's",
                "uid 997",
            ),
            (
                |f| f.directory_after = (0, 0o777),
                "directory is as it was",
                "mode 0777",
            ),
            (
                |f| f.account_read_again = (false, 1),
                "still connects",
                "answered: false",
            ),
            (
                |f| f.stranger_read = (true, 3),
                "a third account, cannot connect",
                "took: 3",
            ),
        ];
        for (nth, (breaks, effect, saw)) in cases.iter().enumerate() {
            let mut facts = held();
            breaks(&mut facts);
            let report = run_of(&facts, Some(&roots_list()), true);
            assert_eq!(report.exit, FAILED_EXIT, "{effect}");
            // nth ok lines, the FAIL, the scratch line, the verdict.
            assert_eq!(report.lines.len(), nth + 3, "{effect}: {:#?}", report.lines);
            assert!(report.lines[..nth].iter().all(|l| l.starts_with("ok    ")));
            let fail = &report.lines[nth];
            assert!(
                fail.starts_with("FAIL  ") && fail.contains(effect),
                "{fail}"
            );
            assert!(fail.contains(saw), "{effect}: {fail}");
            assert!(report.lines[nth + 1].starts_with("ok    the scratch directory"));
            let last = report.lines.last().unwrap();
            assert!(
                last.contains("FAILED at") && last.contains(effect),
                "{last}"
            );
            assert!(last.contains("deposit no reader credential here"), "{last}");
            assert!(!report.lines.iter().any(|l| l.contains("HELD")));
        }
        // A listener reached by nobody, or by the account twice over, is
        // not the one connection the account made.
        for wrong in [(true, 0), (true, 2)] {
            let mut facts = held();
            facts.account_read = wrong;
            assert_eq!(run_of(&facts, Some(&roots_list()), true).exit, FAILED_EXIT);
        }
        // The third account failing to read is not enough when the
        // listener was reached anyway.
        let mut facts = held();
        facts.stranger_read = (false, 3);
        assert_eq!(run_of(&facts, Some(&roots_list()), true).exit, FAILED_EXIT);
    }

    /// N3 (review cd3f6a99): ONE VERDICT, LAST, and a scratch directory
    /// that was not removed is never preceded by a line that says HELD.
    /// With everything else holding it IS the failure; after an earlier
    /// failure it is reported and the verdict still names the first.
    #[test]
    fn a_scratch_directory_left_behind_fails_the_run_and_never_follows_a_held_line() {
        let report = run_of(&held(), Some(&roots_list()), false);
        assert_eq!(report.exit, FAILED_EXIT, "{:#?}", report.lines);
        assert!(!report.lines.iter().any(|l| l.contains("HELD")));
        let n = report.lines.len();
        assert!(
            report.lines[n - 2].starts_with("FAIL  the scratch directory")
                && report.lines[n - 2].contains("it is still there"),
            "{:#?}",
            report.lines
        );
        assert!(
            report.lines[n - 1].contains("FAILED at `the scratch directory"),
            "{}",
            report.lines[n - 1]
        );
        let verdicts = |report: &Report| {
            report
                .lines
                .iter()
                .filter(|l| l.starts_with("probe-door-check: "))
                .count()
        };
        assert_eq!(verdicts(&report), 1);

        let mut facts = held();
        facts.socket = (0, 0o600);
        let report = run_of(&facts, Some(&roots_list()), false);
        assert_eq!(verdicts(&report), 1);
        let last = report.lines.last().unwrap();
        assert!(last.contains("FAILED at `door: the socket"), "{last}");
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.starts_with("FAIL  the scratch directory"))
        );
        assert_eq!(verdicts(&run_of(&held(), Some(&roots_list()), true)), 1);
    }

    /// N5 (review cd3f6a99): the port list's owner and mode, by the
    /// door's own rule. The forge's view passes and the line prints what
    /// was read; anything else is a FAIL that says what and where.
    #[test]
    fn the_port_lists_owner_and_mode_are_judged_by_the_doors_own_rule() {
        let (name, found) = ports_effect(Some(&view()), Some(&roots_list()));
        assert_eq!(found, Ok(()));
        assert!(
            name.ends_with("is root's alone (uid 0, mode 0644)"),
            "{name}"
        );
        for (uid, mode) in [(1000, 0o644), (0, 0o664), (0, 0o666), (997, 0o600)] {
            let list = PortsSource {
                uid,
                mode,
                ..roots_list()
            };
            let (name, found) = ports_effect(Some(&view()), Some(&list));
            let saw = found.expect_err("a list that is not root's alone was passed");
            assert!(
                saw.contains(&format!("uid {uid}'s (mode {mode:04o})")),
                "{saw}"
            );
            assert!(
                name.contains("probe-view/infra/forge/sor-ports.env"),
                "{name}"
            );
            assert_eq!(run_of(&held(), Some(&list), true).exit, FAILED_EXIT);
        }
        let (_, found) = ports_effect(Some(&view()), None);
        assert!(found.unwrap_err().contains("absent or unreadable"));
        let (name, found) = ports_effect(None, None);
        assert!(found.unwrap_err().contains("BOSS_PROBE_DIR is unset"));
        assert!(name.contains("port list"), "{name}");
        // Read from the tree, the way the check reads the host's view.
        let root = boss_testing::repo_root();
        let list = read_ports_file(&root.join(crate::prove::SOR_PORTS_ENV)).unwrap();
        let (name, _) = ports_effect(Some(&root), Some(&list.1));
        assert!(name.contains(&root.display().to_string()), "{name}");
    }

    /// N4 and N6 (review cd3f6a99), THE CAST, pure. No account named by
    /// the host is a refusal — never a run for the door's fallback; root
    /// is not a probe account; and the third account is neither root nor
    /// the first, taken in order.
    #[test]
    fn the_check_is_cast_only_for_a_named_account_and_a_distinct_third() {
        let both = [("nobody", Some(65534)), ("daemon", Some(1))];
        assert_eq!(
            cast(Some("boss-probe"), Some(997), &both),
            Ok((997, "nobody", 65534))
        );
        // The probe account IS nobody: the third is the next one.
        assert_eq!(
            cast(Some("nobody"), Some(65534), &both),
            Ok((65534, "daemon", 1))
        );
        let said = cast(None, None, &both).unwrap_err();
        assert!(said.contains("BOSS_PROBE_USER is unset here"), "{said}");
        assert!(said.contains("names no probe account"), "{said}");
        // Unset is refused even where the fallback account exists.
        assert!(cast(None, Some(1000), &both).is_err());
        let said = cast(Some("boss-probe"), None, &both).unwrap_err();
        assert!(said.contains("no account `boss-probe`"), "{said}");
        let said = cast(Some("root"), Some(0), &both).unwrap_err();
        assert!(said.contains("is root itself"), "{said}");
        // No third account that is distinct: root is not one, and the
        // probe account is not one.
        for others in [
            vec![("nobody", None), ("daemon", None)],
            vec![("nobody", Some(0)), ("daemon", Some(0))],
            vec![("nobody", Some(997)), ("daemon", None)],
        ] {
            let said = cast(Some("boss-probe"), Some(997), &others).unwrap_err();
            assert!(said.contains("no third account (nobody, daemon)"), "{said}");
        }
        // And each refusal is a refusal, not a pass.
        let report = refused(Vec::new(), cast(None, None, &both).unwrap_err());
        assert_eq!(report.exit, REFUSED_EXIT);
    }

    #[test]
    fn anyone_but_root_is_refused_and_told_how_to_file_it() {
        assert_eq!(not_root(0), None);
        let said = not_root(65534).unwrap();
        assert!(said.contains("uid 65534, not root"), "{said}");
        assert!(
            said.contains("boss ops forge probe-door-check --wait"),
            "{said}"
        );
        let report = refused(Vec::new(), said);
        assert_eq!(report.exit, REFUSED_EXIT);
        let last = report.lines.last().unwrap();
        assert!(last.contains("REFUSED"), "{last}");
        assert!(
            last.contains("Nothing was checked; this is not a pass."),
            "{last}"
        );
        assert_ne!(REFUSED_EXIT, HELD_EXIT);
        assert_ne!(REFUSED_EXIT, FAILED_EXIT);
    }

    /// B1 (review cd3f6a99): NOTHING IS WRITTEN THROUGH A NAME ALREADY
    /// THERE. A link standing at either fixture's name — root's own
    /// here, so no kernel protection is what refuses it — leaves its
    /// target exactly as it was, byte and mode; the link itself is not
    /// removed; and the act fails naming the path. A regular file at the
    /// name is refused the same way. Measured before the repair, as pod
    /// root: the secret's target was overwritten and set 0600, the
    /// capability's truncated.
    #[test]
    fn a_name_already_standing_in_the_scratch_is_never_written_through() {
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("victim");
        let mode_of = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o7777;
        let fresh = |body: &str| {
            std::fs::write(&victim, body).unwrap();
            let held = std::fs::File::open(&victim).unwrap();
            // mode-bits-ok: a fixture victim at a mode the check must not change; nothing execs it
            held.set_permissions(std::fs::Permissions::from_mode(0o644))
                .unwrap();
        };

        // The capability question.
        let scratch = tempfile::tempdir().unwrap();
        fresh("VICTIM\n");
        let name = scratch.path().join(CAPABILITY);
        std::os::unix::fs::symlink(&victim, &name).unwrap();
        let asked = can_give_a_file_to(scratch.path(), 65534);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "VICTIM\n");
        assert_eq!(mode_of(&victim), 0o644);
        assert_eq!(
            asked.expect_err("answered through a planted name").kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(
            std::fs::symlink_metadata(&name).unwrap().is_symlink(),
            "the check removed a name it did not make"
        );

        // The fixture secret, through `observe` itself — which fails at
        // its first act, before anything that needs root.
        let scratch = tempfile::tempdir().unwrap();
        let open = open_area_in(scratch.path()).unwrap();
        let name = scratch.path().join(SECRET);
        std::os::unix::fs::symlink(&victim, &name).unwrap();
        let (what, saw) = observe(scratch.path(), &open, "nobody", 65534, "daemon")
            .expect_err("observed through a planted name");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "VICTIM\n");
        assert_eq!(mode_of(&victim), 0o644);
        assert!(what.contains("making the fixture secret"), "{what}");
        assert!(what.contains(&name.display().to_string()), "{what}");
        assert!(saw.contains("exists"), "{saw}");
        assert!(std::fs::symlink_metadata(&name).unwrap().is_symlink());
        // ...and that is a FAILED run that names it, never a HELD one.
        let report = judge(
            "nobody",
            &[(what, Err(saw))],
            &scratch_effect(scratch.path(), true),
        );
        assert_eq!(report.exit, FAILED_EXIT);
        assert!(report.lines[0].contains(SECRET), "{:#?}", report.lines);

        // A dangling link, and a file already there.
        let scratch = tempfile::tempdir().unwrap();
        let gone = elsewhere.path().join("not-there-and-must-stay-so");
        std::os::unix::fs::symlink(&gone, scratch.path().join(CAPABILITY)).unwrap();
        assert!(can_give_a_file_to(scratch.path(), 65534).is_err());
        assert!(absent(&gone), "the write made the link's target");
        let scratch = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join(SECRET), "somebody's\n").unwrap();
        assert!(new_private_file(&scratch.path().join(SECRET), "x").is_err());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(SECRET)).unwrap(),
            "somebody's\n"
        );

        // The control: with nothing there the file is made, private.
        let scratch = tempfile::tempdir().unwrap();
        let made = scratch.path().join(SECRET);
        new_private_file(&made, "fixture\n").unwrap();
        assert_eq!(std::fs::read_to_string(&made).unwrap(), "fixture\n");
        assert_eq!(mode_of(&made), 0o600);
        assert!(can_give_a_file_to(scratch.path(), 65534).is_ok());
        assert!(absent(&scratch.path().join(CAPABILITY)));
    }

    /// B1, the layout: the open directory is made inside the scratch by
    /// a call that refuses a name already there, and is the only place
    /// with /tmp's mode.
    #[test]
    fn the_open_directory_is_made_new_and_is_the_only_one_others_may_write() {
        let scratch = tempfile::tempdir().unwrap();
        let open = open_area_in(scratch.path()).unwrap();
        assert_eq!(open, scratch.path().join(OPEN));
        assert_eq!(owner_and_mode(&open).1, 0o1777);
        assert!(
            open_area_in(scratch.path()).is_err(),
            "made over a name already there"
        );
        let elsewhere = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), scratch.path().join(OPEN)).unwrap();
        let before = owner_and_mode(elsewhere.path()).1;
        assert!(open_area_in(scratch.path()).is_err());
        assert_eq!(owner_and_mode(elsewhere.path()).1, before);
    }

    /// THE CHECK ITSELF, WHEREVER THIS SUITE RUNS, and what it must say
    /// there. As anyone but root (the gate): refused as not root, with
    /// nothing made. As a root that cannot chown (the dev pod): refused
    /// for that, scratch removed. As a root that can — which no suite
    /// has ever run as, the reason this verb exists — every effect must
    /// hold, and this test is then the by-effect test on that host.
    /// Either way nothing is left in the parent it was given.
    #[test]
    fn the_check_refuses_where_it_cannot_be_made_and_leaves_nothing_behind() {
        let parent = tempfile::tempdir().unwrap();
        let held = std::fs::File::open(parent.path()).unwrap();
        // mode-bits-ok: a fixture parent other accounts can traverse, as /tmp is; nothing execs it
        held.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        // `nobody` stands in as the probe account: every host has it,
        // so a root here gets as far as the capability question. The
        // tree stands in as the view, its list read as the host's is.
        let named = Named {
            account: Some("nobody".into()),
            view: Some(boss_testing::repo_root()),
        };
        let report = check_in(parent.path(), &named);
        let said = report.lines.join("\n");
        let me = crate::own_temp::current_uid();
        let chowns = {
            let held = tempfile::tempdir().unwrap();
            can_give_a_file_to(held.path(), uid_named("nobody").unwrap_or(65534)).unwrap()
        };
        if me != 0 {
            assert_eq!(report.exit, REFUSED_EXIT, "{said}");
            assert!(said.contains(&format!("uid {me}, not root")), "{said}");
            assert_eq!(report.lines.len(), 1, "{said}");
        } else if !chowns {
            assert_eq!(report.exit, REFUSED_EXIT, "{said}");
            assert!(
                said.contains("this root cannot chown (no CAP_CHOWN)"),
                "{said}"
            );
            assert!(said.contains("handing to nobody"), "{said}");
            assert!(!said.contains("HELD") && !said.contains("ok    "), "{said}");
        } else {
            // The tree's list is not root's alone in a builder's
            // checkout, so only the hand-over's own lines are asked.
            assert!(!said.contains("REFUSED"), "{said}");
            assert!(said.matches("\nok    ").count() >= 17, "{said}");
        }
        assert!(
            said.contains("this is not a pass") || report.exit != REFUSED_EXIT,
            "{said}"
        );
        assert_eq!(
            scratches_in(parent.path()),
            Vec::<PathBuf>::new(),
            "the check left something behind: {said}"
        );
        // With no account named, the same host refuses before it makes
        // anything — whoever runs it.
        let unnamed = Named {
            account: None,
            view: None,
        };
        let report = check_in(parent.path(), &unnamed);
        assert_eq!(report.exit, REFUSED_EXIT);
        assert_eq!(report.lines.len(), 1);
        assert_eq!(scratches_in(parent.path()), Vec::<PathBuf>::new());
    }

    /// The machinery every "connects" and "cannot connect" line rests
    /// on, where it can run without a hand-over: the check's listener
    /// answers the reader's own curl with [`ANSWER`], counts each
    /// connection it took, and a socket nobody listens on is not read.
    #[test]
    fn the_checks_listener_answers_the_readers_curl_and_counts_it() {
        let scratch = tempfile::tempdir().unwrap();
        let door = door_directory_in(scratch.path()).unwrap();
        let (socket, listener) = bind_socket(door.path(), None).unwrap();
        let listener = Listener::serve(listener).unwrap();
        assert_eq!(listener.taken(), 0);
        let curl = |socket: &Path| {
            let words = curl_words(socket);
            std::process::Command::new(words[0])
                .args(&words[1..])
                .output()
                .unwrap()
        };
        for nth in 1..=2 {
            let out = curl(&socket);
            assert!(answered(&out), "{out:?}");
            assert_eq!(listener.taken(), nth);
        }
        drop(listener);
        let out = curl(&socket);
        assert!(!answered(&out), "a dead socket was read: {out:?}");
        assert!(is_socket(&socket) && !absent(&socket));
        assert_eq!(owner_and_mode(&socket).1, 0o600);
        assert_eq!(owner_and_mode(door.path()).1, 0o700);
        assert_eq!(owner(&scratch.path().join("nothing")), u32::MAX);
    }

    /// The capability is asked by effect, and a process is always able
    /// to "give" a file to itself — which must not read as CAP_CHOWN.
    #[test]
    fn giving_a_file_away_is_asked_by_doing_it() {
        let scratch = tempfile::tempdir().unwrap();
        let me = crate::own_temp::current_uid();
        let other = if me == 65534 { 65533 } else { 65534 };
        let could = can_give_a_file_to(scratch.path(), other).unwrap();
        let by_hand = {
            let file = scratch.path().join("by-hand");
            new_private_file(&file, "").unwrap();
            std::os::unix::fs::lchown(&file, Some(other), None).is_ok()
        };
        assert_eq!(could, by_hand);
        assert!(absent(&scratch.path().join(CAPABILITY)));
    }

    /// WHAT THE CHECK MAY AND MAY NOT TOUCH, held on the source: it
    /// hands over with the door's own functions and makes no chown of
    /// its own but the capability question; it writes no file by a call
    /// that follows a link and sets no mode by path (B1); and it names
    /// nothing that reads a reader credential or a machine token — not
    /// the door's `open`, not its guard, not the token client.
    #[test]
    fn the_check_reuses_the_doors_hand_over_and_reads_no_credential() {
        let code: String = include_str!("probe_door_check.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(code.matches("hand_socket_to(planted.path()").count(), 1);
        assert_eq!(
            code.matches("bind_socket(&directory, Some(uid))").count(),
            1
        );
        assert_eq!(code.matches("door_directory_in(open)").count(), 2);
        assert_eq!(
            code.matches("lchown(").count(),
            1,
            "only the capability question"
        );
        assert_eq!(code.matches("fs::chown(").count(), 0);
        // B1: one way to make a file, and it is exclusive.
        assert_eq!(code.matches(".create_new(true)").count(), 1);
        for follows in [
            "fs::write(",
            "File::create(",
            "fs::set_permissions(",
            "fs::copy(",
            ".create(true)",
            ".truncate(true)",
            "remove_dir_all(",
        ] {
            assert_eq!(
                code.matches(follows).count(),
                0,
                "the check calls {follows}"
            );
        }
        for never in [
            "probe_reader::open",
            "Guard",
            "read_credential",
            "CREDENTIAL",
            "machine_token",
            "door_env",
            "reqwest",
            "BOSS_JOBS_URL",
        ] {
            assert!(!code.contains(never), "the check names `{never}`");
        }
        // The door's own source still makes its one hand-over through
        // the function the check calls.
        let door = include_str!("probe_reader.rs");
        assert!(
            door.contains("let (socket, listener) = bind_socket(directory.path(), recipient)?;")
        );
    }

    /// The ops verb runs exactly this command, as a read: no arguments,
    /// on the forge, and never under the word that marks a verb as one
    /// that changes its host.
    #[test]
    fn the_ops_verb_runs_this_check_and_nothing_else() {
        let path = boss_testing::repo_root().join("infra/ops/verbs/probe-door-check.json");
        let verb: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            verb["argv"],
            serde_json::json!(["boss", "probe-door-check"])
        );
        assert_eq!(verb["params"], serde_json::json!([]));
        assert_eq!(verb["hosts"], serde_json::json!(["forge"]));
        assert!(verb.get("requires_approval").is_none());
        let about = verb["about"].as_str().unwrap();
        assert!(about.starts_with("READ-ONLY"), "{about}");
        assert!(!about.contains("MUTATING"), "{about}");
        use clap::CommandFactory;
        let cli = crate::Cli::command();
        assert!(
            cli.get_subcommands()
                .any(|c| c.get_name() == "probe-door-check"),
            "the CLI has no such subcommand"
        );
    }
}
