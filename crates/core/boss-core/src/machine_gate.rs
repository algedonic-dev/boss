//! The machine gate: every service port's check that a caller holds the
//! estate machine token (design 6805c764, car 1; backlog 2710c8fc).
//!
//! WHY IT MOVED HERE. The LAN machine door (`boss-jobs-internal`, and
//! every service port on the WireGuard mesh) trusts `x-boss-user` as
//! sent, so anyone who can route to a port can act as platform-admin at
//! it. The gate that answers that lived in `boss-jobs` and was mounted
//! in ONE of 27 HTTP binaries — and dormant there, because nothing set
//! its env var. David, 2026-09-25: "require the machine token on every
//! service". A middleware 26 other binaries must mount cannot live in
//! one of them, so it lives in the crate every one of them already
//! depends on, and `every_service_mounts_the_machine_gate.rs` holds each
//! server to it.
//!
//! THREE MODES, read from a mounted file so a change needs no pod roll:
//!
//! * `off` — admits everything and records nothing. Today's dormant
//!   behaviour, and the default: a missing or blank mode file is `off`,
//!   so this car changes nothing any caller sees. A mode file that EXISTS
//!   but cannot be read is not `off`: it is `report`, said at error level.
//! * `report` — admits everything, and tallies every request that
//!   presents no accepted token (and every one that presents the
//!   `previous` token, which a rotation's revoke step reads). The tally
//!   is how enforcement is earned: a clean window, not a belief.
//! * `enforce` — refuses what `report` tallies, with a 401 naming the
//!   header and the mode. With NO readable token it degrades to
//!   `report` and says so at error level on every request and on
//!   `accepts` — never refuses everything, because a guard that stops
//!   the system it guards is the 2026-09-05 shape (design choice 4).
//!
//! THREE TOKEN SLOTS, `current`, `next` and `previous`, one file each in
//! the mounted token directory. Any non-empty slot is accepted, so the
//! broker can stage `next` on every port before any caller sends it, and
//! revoke `previous` only after nothing presents it (car 3). A match
//! answers the slot's NAME; no value, and no hash of one, is logged,
//! recorded or returned.
//!
//! THE PROBE-READER SLOTS (design b35c22b4, Q1 and Q2; backlog d26515c5).
//! A second set of `current`, `next` and `previous`, in a directory of
//! its own, for the one credential a recorded car probe reads the system
//! of record through once the gate enforces. The estate token lets its
//! holder assert any `x-boss-user`; this one is scoped HERE, on the
//! server, so whatever door hands it to a probe is a second layer and
//! never the only one. A value that matches a reader slot (and no estate
//! slot) is, in EVERY mode including `off`:
//!
//! * admitted for GET and HEAD only — any other method is refused 403,
//!   because the scope is a property of the credential, not of a window;
//! * stripped of every inbound `x-boss-*` header (the gateway's edge
//!   strip, applied to this caller), the credential itself included, and
//!   handed `x-boss-user` = [`crate::roles::PROBE_READER_ACTOR`] at
//!   `audit-readonly` — never the identity it asserted;
//! * named `reader.<slot>` on `accepts`, and tallied as `reader.<slot>`
//!   in `report` and `enforce`, so the window reads probe traffic as
//!   probe traffic. It never reads the tally: that stays the estate
//!   token's.
//!
//! No manifest mounts the directory yet, and an absent one is an empty
//! set that matches nothing, so mounting this code changes no caller's
//! answer: the Secret, its mounts and the broker's rule are the next car.
//!
//! EXEMPT: OPTIONS (CORS preflight never carries the header), and the
//! exact health paths a binary declares at mount — GET only, never a
//! prefix — because a probe answered 401 restarts pods and a watchdog
//! answered 401 goes blind (design choice 5).
//!
//! THE TALLY IS ALSO STATED ON THE LOG (design 21946380). It lives in
//! memory and every train restarts the pod, so it can never be 72 hours
//! old: each mount and mode move, each key's first sighting and each
//! overflow is a `machine_gate.*` fact through the service's outbox
//! ([`crate::gate_evidence`]), and the clean window is read there. The
//! tally stays the live half — its `not_clean` names a fact the log did
//! not take.
//!
//! TWO ROUTES on every gated binary, both behind the gate itself:
//! `GET /api/machine-gate/accepts` answers `{mode, matched, degraded}`
//! for the header presented; `GET /api/machine-gate/misses` answers the
//! tally, and ONLY to a caller presenting an accepted token in every
//! mode — a caller's address is a record, and `report` admits everyone
//! else.

use crate::gate_evidence::{Evidence, EvidenceHealth, Fact, Gate as EvidenceGate};
use crate::machine_token;
use crate::port::EventRecorder;
use axum::Json;
use axum::Router;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

/// The mode file's location. A ConfigMap key in the cluster (car 4);
/// absent here, which reads as `off`.
pub const MODE_FILE_ENV: &str = "BOSS_MACHINE_GATE_MODE_FILE";
pub const DEFAULT_MODE_FILE: &str = "/etc/boss/machine-gate/mode";
/// The most the mode file may hold. It holds one word; past this it is
/// not a mode file, and the gate re-reads it every few seconds, so an
/// unbounded read would pull an arbitrary file into memory on that
/// cadence (review of car 1, a159e1ee; of car 2 slice 1, S8).
pub const MAX_MODE_BYTES: u64 = 4096;
/// The directory holding the `current`, `next` and `previous` slots —
/// the `boss-machine-token` Secret's mount (car 4). Defined ONCE, in
/// `machine_token`, because every caller reads its `current` from the
/// same directory this gate accepts from (car 2).
pub use crate::machine_token::{DEFAULT_TOKEN_DIR, TOKEN_DIR_ENV};
/// The directory holding the probe-reader credential's `current`,
/// `next` and `previous` slots (design b35c22b4) — the
/// `boss-probe-reader` Secret's mount, once a manifest mounts it. Absent
/// is an empty slot set, which matches nothing.
pub const READER_DIR_ENV: &str = "BOSS_MACHINE_GATE_READER_DIR";
pub const DEFAULT_READER_DIR: &str = "/etc/boss/probe-reader";

/// The gate's own routes live under this prefix on every service.
pub const ROUTE_PREFIX: &str = "/api/machine-gate/";
pub const MISSES_PATH: &str = "/api/machine-gate/misses";
pub const ACCEPTS_PATH: &str = "/api/machine-gate/accepts";

/// Does `path` lead to the gate's own routes, however it is spelled?
/// Percent-decoded, case-folded, with empty segments and `\` read as a
/// separator, so no spelling a proxy or an upstream might normalise
/// differently slips past. The gateway asks this before it stamps the
/// token: stamped, a signed-in user's request would read `/misses` —
/// every caller's address — with the gateway's credential (review of
/// car 1, a159e1ee).
pub fn is_gate_route(path: &str) -> bool {
    let decoded = percent_decode(path).to_ascii_lowercase();
    let mut segments = decoded.split(['/', '\\']).filter(|s| !s.is_empty());
    let prefix: Vec<&str> = ROUTE_PREFIX.split('/').filter(|s| !s.is_empty()).collect();
    prefix.iter().all(|p| segments.next() == Some(p))
}

/// `%XX` decoded as bytes, repeatedly until it stops changing (so a
/// double-encoded `%252d` is read the way a second decoder would read
/// it); anything that is not valid UTF-8 afterwards is read lossily.
fn percent_decode(path: &str) -> String {
    let mut cur = path.to_string();
    for _ in 0..4 {
        let bytes = cur.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if bytes[i] == b'%'
                && i + 2 < bytes.len()
                && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
            {
                out.push((h * 16 + l) as u8);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        let next = String::from_utf8_lossy(&out).into_owned();
        if next == cur {
            break;
        }
        cur = next;
    }
    cur
}

/// How many distinct miss keys one process holds. Past it, a new key is
/// counted in `overflow` rather than stored: a caller spraying routes
/// must not grow a service's memory without bound.
pub const MAX_TALLY_KEYS: usize = 1024;

/// How many of those keys one SOURCE may hold (see [`source_of`]). Past
/// it, a new key from that source is counted in the tally's
/// `source_overflow`, by source and by what it presented, rather than
/// stored. WHY (backlog 93bcf490, from review 1829e95f finding 2): the
/// key carries the caller's own `x-boss-user`, so ONE LAN caller varying
/// that header filled all [`MAX_TALLY_KEYS`], every other caller's first
/// miss after that was only a number, and the credential broker held
/// every machine-token revoke while that number was above zero. A
/// sixteenth of the tally each means it takes sixteen full sources
/// before anyone else's miss goes unnamed. A `previous` and a loopback
/// caller are not held to it (`held_to_a_share`, review ebc7b1cc).
pub const MAX_KEYS_PER_SOURCE: usize = MAX_TALLY_KEYS / 16;

/// How often the mounted files are re-read. Kubelet refreshes a mounted
/// Secret or ConfigMap in about a minute, so five seconds adds nothing
/// a rotation would notice.
const REREAD: Duration = Duration::from_secs(5);

/// Longest `x-boss-user` id kept in a tally key; the header is caller
/// text and a key is memory.
const MAX_USER_CHARS: usize = 96;

/// The tally's name for every method that is not one of the nine
/// standard ones.
pub const OTHER_METHOD: &str = "(other)";

/// The tally's method for every write refused under the probe-reader
/// credential. Such a request reached no handler, so which method it
/// tried says nothing a reader of the tally needs, and keying it by
/// method let one holder multiply its keys by ten (review 177b4976, B1).
pub const REFUSED_WRITE: &str = "(refused write)";

/// The methods a tally key keeps by name — RFC 9110's eight and PATCH.
const STANDARD_METHODS: [&str; 9] = [
    "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
];

/// A request's method as a tally key: a standard method by name, any
/// other as [`OTHER_METHOD`]. The method is caller text — hyper admits
/// any token as an extension method, hundreds of kilobytes of it — and
/// the tally bounds its keys by COUNT, so a raw method made each of its
/// [`MAX_TALLY_KEYS`] as large as a caller liked, and printed it on the
/// key's WARN line (review of car 1, a159e1ee: due before any service is
/// set to `report`, backlog 2710c8fc). No BOSS route answers an
/// extension method, so the name of one says nothing a reader needs.
fn method_key(method: &Method) -> String {
    let m = method.as_str();
    STANDARD_METHODS
        .iter()
        .find(|s| **s == m)
        .map_or(OTHER_METHOD, |s| s)
        .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Report,
    Enforce,
}

impl Mode {
    /// Parse the mode file's text. Absent or blank is `off` (the
    /// default this car ships). An unknown word is read as `report` —
    /// which admits everything, like `off`, but records — and the second
    /// value says so, because a typo of `enforce` read silently as `off`
    /// is exactly the quiet this gate exists to end.
    pub fn parse(raw: Option<&str>) -> (Mode, Option<String>) {
        Mode::parse_as(raw, GATE)
    }

    /// [`Mode::parse`] for the switch named `what` — the one parse every
    /// mode word in the estate goes through (see [`ModeSwitch`]).
    pub fn parse_as(raw: Option<&str>, what: &str) -> (Mode, Option<String>) {
        let Some(word) = raw.map(|s| s.trim().to_ascii_lowercase()) else {
            return (Mode::Off, None);
        };
        match word.as_str() {
            "" | "off" => (Mode::Off, None),
            "report" => (Mode::Report, None),
            "enforce" => (Mode::Enforce, None),
            other => (
                Mode::Report,
                Some(format!(
                    "unknown {what} mode `{}`: read as `report`, which admits every \
                     request and records what `enforce` would refuse (off | report | enforce)",
                    other.chars().take(32).collect::<String>()
                )),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Report => "report",
            Mode::Enforce => "enforce",
        }
    }

    /// A reading that moves from `enforce` to anything else: a refusal
    /// switched off. It is the rollback road of every refusing switch
    /// (design b08725c2), so it is said at WARN, never folded into an
    /// INFO line beside a slot rotation — an arm lowered is a fact the
    /// next reader of the log is owed.
    pub fn leaves_enforce(self, next: Mode) -> bool {
        self == Mode::Enforce && next != Mode::Enforce
    }
}

/// The machine gate's own name in what its mode reader says.
const GATE: &str = "machine gate";

/// Which accepted slot a presented token matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Current,
    Next,
    Previous,
}

impl Slot {
    /// The slot's name, as its file and the accepts route spell it.
    pub fn name(self) -> &'static str {
        match self {
            Slot::Current => "current",
            Slot::Next => "next",
            Slot::Previous => "previous",
        }
    }
}

/// The accepted token values. `Debug` names which slots are present and
/// never prints a value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Slots {
    current: Option<String>,
    next: Option<String>,
    previous: Option<String>,
}

impl std::fmt::Debug for Slots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Slots{:?}", self.present())
    }
}

impl Slots {
    /// Each value whitespace-trimmed; a blank one is absent, not a token
    /// every empty header would match.
    pub fn new(current: Option<String>, next: Option<String>, previous: Option<String>) -> Self {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Slots {
            current: clean(current),
            next: clean(next),
            previous: clean(previous),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.present().is_empty()
    }

    /// The names of the slots that hold a value.
    pub fn present(&self) -> Vec<Slot> {
        self.entries()
            .into_iter()
            .filter(|(_, v)| v.is_some())
            .map(|(s, _)| s)
            .collect()
    }

    fn entries(&self) -> [(Slot, Option<&str>); 3] {
        [
            (Slot::Current, self.current.as_deref()),
            (Slot::Next, self.next.as_deref()),
            (Slot::Previous, self.previous.as_deref()),
        ]
    }

    /// The slot the presented value matches, compared in constant time
    /// against EVERY non-empty slot before one is chosen, so timing
    /// does not say which slot, or how many, a guess was checked against.
    pub fn matched(&self, provided: Option<&str>) -> Option<Slot> {
        self.entries()
            .map(|(slot, v)| (slot, v.is_some_and(|v| machine_token::verify(v, provided))))
            .into_iter()
            .find(|(_, hit)| *hit)
            .map(|(slot, _)| slot)
    }
}

/// One reading of the mounted files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub mode: Mode,
    pub slots: Slots,
    /// The probe-reader credential's slots (design b35c22b4): a match
    /// reads as the probe reader, GET and HEAD only. Empty until a
    /// manifest mounts its Secret.
    pub reader: Slots,
    /// Set when the mode file held a word that is not a mode, or exists
    /// and could not be read; either reads as `report`, announced at
    /// error level.
    pub mode_error: Option<String>,
}

impl Reading {
    pub fn new(mode: Mode, slots: Slots) -> Self {
        Reading {
            mode,
            slots,
            reader: Slots::default(),
            mode_error: None,
        }
    }

    /// This reading with `reader` as its probe-reader slots.
    pub fn with_reader(self, reader: Slots) -> Self {
        Reading { reader, ..self }
    }

    /// `enforce` with no token to enforce: admitted as `report`, loudly.
    pub fn degraded(&self) -> bool {
        self.mode == Mode::Enforce && self.slots.is_empty()
    }
}

/// Where the gate reads its mode and slots: the mounted ConfigMap key
/// and the mounted Secret directory. The adapter half of the gate.
#[derive(Clone, Debug)]
pub struct MountedFiles {
    pub mode_file: PathBuf,
    pub token_dir: PathBuf,
    /// The probe-reader slots' directory; `None` reads no reader slots.
    pub reader_dir: Option<PathBuf>,
}

impl MountedFiles {
    pub fn new(mode_file: impl Into<PathBuf>, token_dir: impl Into<PathBuf>) -> Self {
        MountedFiles {
            mode_file: mode_file.into(),
            token_dir: token_dir.into(),
            reader_dir: None,
        }
    }

    /// These files, with the probe-reader slots read from `dir`.
    pub fn with_reader_dir(self, dir: impl Into<PathBuf>) -> Self {
        MountedFiles {
            reader_dir: Some(dir.into()),
            ..self
        }
    }

    /// The paths from the environment, or their defaults.
    pub fn from_env() -> Self {
        let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        MountedFiles::new(
            var(MODE_FILE_ENV, DEFAULT_MODE_FILE),
            var(TOKEN_DIR_ENV, DEFAULT_TOKEN_DIR),
        )
        .with_reader_dir(var(READER_DIR_ENV, DEFAULT_READER_DIR))
    }

    /// Read once, blocking. For boot, before the server takes a request,
    /// so the first request is judged by the configured mode and not by
    /// a default.
    pub fn read_blocking(&self) -> Reading {
        // Each slot through the one bounded reader every caller uses
        // (machine_token::read_slot, car 2), so the gate and its callers
        // cannot disagree about what a slot file holds. The reader's
        // slots go through the same reader, from their own directory.
        let slots = |dir: &std::path::Path| {
            let slot = |name: &str| machine_token::read_slot(dir, name);
            Slots::new(slot("current"), slot("next"), slot("previous"))
        };
        let (mode, mode_error) = judge_mode_text(read_mode_file(&self.mode_file), GATE);
        Reading {
            mode,
            slots: slots(&self.token_dir),
            reader: self.reader_dir.as_deref().map(slots).unwrap_or_default(),
            mode_error,
        }
    }

    /// Read once, without blocking the runtime.
    pub async fn read(&self) -> Reading {
        let files = self.clone();
        match tokio::task::spawn_blocking(move || files.read_blocking()).await {
            Ok(reading) => reading,
            // Never a default: a default reading is `off`, and a gate
            // that fell to `off` because a read task died is the quiet
            // this gate exists to end. Read again, here.
            Err(e) => {
                tracing::error!("machine gate: the file read task failed ({e}); reading inline");
                self.read_blocking()
            }
        }
    }
}

/// The mode file's text, read to at most [`MAX_MODE_BYTES`]. `Ok(None)`
/// only when the file is ABSENT — the one case that is `off`. A file
/// that exists and cannot be read as bounded UTF-8 (a directory in its
/// place, a permission, bytes that are not text, more than the bound)
/// is `Err` naming the path and why. Until 2026-09-27 every such case
/// went through `.ok()` to `off` and logged INFO, so a broken ConfigMap
/// mount read as a gate switched off on purpose (reviews of car 1,
/// a159e1ee, and of car 2 slice 1, S8; backlog 2710c8fc).
fn read_mode_file(path: &std::path::Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut raw = String::new();
    file.take(MAX_MODE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if raw.len() as u64 > MAX_MODE_BYTES {
        return Err(format!(
            "{}: more than {MAX_MODE_BYTES} bytes, which is not one word",
            path.display()
        ));
    }
    Ok(Some(raw))
}

/// A mode file's text judged as a mode: parsed when it was read,
/// `report` said at error level when it exists and could not be.
fn judge_mode_text(text: Result<Option<String>, String>, what: &str) -> (Mode, Option<String>) {
    match text {
        Ok(text) => Mode::parse_as(text.as_deref(), what),
        Err(why) => (
            Mode::Report,
            Some(format!(
                "{what} mode file is unreadable ({why}): read as `report`, which admits every \
                 request and records what `enforce` would refuse; only an ABSENT mode file is \
                 `off`"
            )),
        ),
    }
}

/// Read one mode file the way the machine gate reads its own: bounded,
/// ABSENT is `off`, unreadable or unknown is `report` with the reason.
pub fn read_mode(path: &std::path::Path, what: &str) -> (Mode, Option<String>) {
    judge_mode_text(read_mode_file(path), what)
}

/// One more refusal switched by a mode word in a mounted file — the
/// machine gate's reader lent to a second arm, so every refusing switch
/// in the estate reads, defaults, degrades and speaks the same way.
///
/// WHY IT EXISTS (design b08725c2 row D, David 2026-09-29): the signed
/// policy check (F7 of backlog b8e75382) "ships behind its own mode key,
/// beside the machine-gate mode and read the same way: off, then report,
/// then enforce", because its misfire denies every signed-in write, so a
/// revert car may not be able to ride — its rollback must be one edit to
/// a mounted word, not a deploy. A second reader written beside this one
/// would drift from it (CLAUDE.md §9a); this is the same `read_mode`,
/// the same bound, the same re-read cadence, and one more thing the
/// machine gate also now does: leaving `enforce` is said at WARN.
pub struct ModeSwitch {
    what: String,
    path: PathBuf,
    reading: RwLock<Held>,
}

/// A switch's last reading and when its MODE began, under one lock, so
/// no reader sees a new mode beside the old mode's start (the D2 race
/// of review fd151a97, in its second place).
struct Held {
    reading: (Mode, Option<String>),
    since: DateTime<Utc>,
}

impl std::fmt::Debug for ModeSwitch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeSwitch")
            .field("what", &self.what)
            .field("path", &self.path)
            .field("reading", &self.reading())
            .finish()
    }
}

impl ModeSwitch {
    /// A switch named `what` (as its log lines and refusals say it),
    /// whose word lives in `path`, taking `reading` as its first.
    pub fn new(what: &str, path: impl Into<PathBuf>, reading: (Mode, Option<String>)) -> Self {
        ModeSwitch {
            what: what.to_string(),
            path: path.into(),
            reading: RwLock::new(Held {
                reading,
                since: Utc::now(),
            }),
        }
    }

    pub fn what(&self) -> &str {
        &self.what
    }

    /// The mounted file this switch reads — named in a refusal, because
    /// the file is the way back.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The last reading: the mode, and why it is not what the file says
    /// when the file could not be read as one.
    pub fn reading(&self) -> (Mode, Option<String>) {
        self.reading
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .reading
            .clone()
    }

    pub fn mode(&self) -> Mode {
        self.reading().0
    }

    /// The mode, and when it began: the switch's mount, or the last
    /// reading that moved the MODE (a new error text under the same mode
    /// is not a move). A tally kept beside the switch restarts from this
    /// instant, because a count that spans a move describes two modes —
    /// and one that spans a stretch of `off` claims a watch that did not
    /// happen (enforce checklist F1 of backlog b8e75382). A restart
    /// empties both, being a new switch.
    pub fn mode_since(&self) -> (Mode, DateTime<Utc>) {
        let (reading, since) = self.reading_since();
        (reading.0, since)
    }

    /// [`ModeSwitch::reading`] and [`ModeSwitch::mode_since`]'s instant
    /// from ONE read of the lock, for a reader that answers both.
    pub fn reading_since(&self) -> ((Mode, Option<String>), DateTime<Utc>) {
        let held = self.reading.read().unwrap_or_else(PoisonError::into_inner);
        (held.reading.clone(), held.since)
    }

    /// Read the file now, blocking — for boot, before the first request.
    pub fn read_blocking(&self) -> (Mode, Option<String>) {
        read_mode(&self.path, &self.what)
    }

    /// Take a fresh reading and say so when it differs from the last:
    /// ERROR for a file that is not a mode, WARN on leaving `enforce`,
    /// INFO otherwise. Answers whether the reading changed.
    pub fn observe(&self, next: (Mode, Option<String>)) -> bool {
        let mut cur = self.reading.write().unwrap_or_else(PoisonError::into_inner);
        if cur.reading == next {
            return false;
        }
        let from = cur.reading.0;
        if from != next.0 {
            cur.since = Utc::now();
        }
        cur.reading = next;
        drop(cur);
        self.announce_from(Some(from), "mode changed");
        true
    }

    /// One line naming the switch, its file and its mode.
    pub fn announce(&self, what: &str) {
        self.announce_from(None, what);
    }

    fn announce_from(&self, from: Option<Mode>, what: &str) {
        let (mode, error) = self.reading();
        let file = self.path.display().to_string();
        let switch = self.what.as_str();
        let was = from.map_or("-", Mode::name);
        if let Some(e) = error {
            tracing::error!(
                switch,
                file,
                from = was,
                mode = mode.name(),
                "{switch} {what}: {e}"
            );
        } else if from.is_some_and(|f| f.leaves_enforce(mode)) {
            tracing::warn!(
                switch,
                file,
                from = was,
                mode = mode.name(),
                "{switch} {what}: LEFT enforce — what it refused is answered again"
            );
        } else {
            tracing::info!(
                switch,
                file,
                from = was,
                mode = mode.name(),
                "{switch} {what}"
            );
        }
    }

    /// The switch whose file `env` names (else `default`), read once now
    /// and then on the machine gate's cadence, so a mode change lands
    /// without a restart. Call it inside the runtime, at boot.
    pub fn mount(what: &str, env: &str, default: &str) -> Arc<Self> {
        let path = PathBuf::from(std::env::var(env).unwrap_or_else(|_| default.to_string()));
        let first = read_mode(&path, what);
        let switch = Arc::new(ModeSwitch::new(what, path, first));
        switch.announce("mounted");
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let watched = Arc::clone(&switch);
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(REREAD).await;
                    let files = Arc::clone(&watched);
                    let next =
                        match tokio::task::spawn_blocking(move || files.read_blocking()).await {
                            Ok(next) => next,
                            // Never a default, as the gate's own read: a
                            // switch that fell to `off` because a read task
                            // died is the quiet this reader exists to end.
                            Err(e) => {
                                tracing::error!(
                                    switch = watched.what(),
                                    "the mode file read task failed ({e}); reading inline"
                                );
                                watched.read_blocking()
                            }
                        };
                    watched.observe(next);
                }
            });
        } else {
            tracing::error!(
                switch = what,
                "mode switch mounted outside a runtime: its mode will not be re-read"
            );
        }
        switch
    }
}

/// What a tallied request presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presented {
    /// No `x-boss-machine-token` header at all.
    None,
    /// A header that matches no accepted slot.
    Mismatch,
    /// The `previous` slot — admitted, and counted so a rotation's
    /// revoke waits until nothing sends it.
    Previous,
    /// A probe-reader slot (design b35c22b4): admitted for GET and HEAD
    /// only, as the probe reader, and counted so the window sees probe
    /// traffic as probe traffic — and, for `reader.previous`, so the
    /// reader's own rotation can drain it. Unlike the estate `previous`,
    /// every reader presentation is held to its source's share (review
    /// 177b4976, B1; see `held_to_a_share`), so past the share it is
    /// counted in `source_overflow` under its source, not keyed.
    #[serde(rename = "reader.current")]
    ReaderCurrent,
    #[serde(rename = "reader.next")]
    ReaderNext,
    #[serde(rename = "reader.previous")]
    ReaderPrevious,
}

impl Presented {
    /// The wire name, as the tally route spells it.
    pub fn name(self) -> &'static str {
        match self {
            Presented::None => "none",
            Presented::Mismatch => "mismatch",
            Presented::Previous => "previous",
            Presented::ReaderCurrent => "reader.current",
            Presented::ReaderNext => "reader.next",
            Presented::ReaderPrevious => "reader.previous",
        }
    }

    /// A probe-reader match on `slot`.
    pub fn reader(slot: Slot) -> Self {
        match slot {
            Slot::Current => Presented::ReaderCurrent,
            Slot::Next => Presented::ReaderNext,
            Slot::Previous => Presented::ReaderPrevious,
        }
    }

    /// Does `enforce` refuse what `report` admits under this
    /// presentation — the ONE question the clean window asks (design
    /// 21946380)? Only no token and a mismatch. The estate `previous` is
    /// admitted in every mode; a probe-reader read is admitted in every
    /// mode and a probe-reader write is refused in every mode (design
    /// b35c22b4), so a mode move changes nothing for any of them.
    pub fn enforce_refuses(self) -> bool {
        matches!(self, Presented::None | Presented::Mismatch)
    }

    /// Is this a probe-reader presentation?
    pub fn is_reader(self) -> bool {
        matches!(
            self,
            Presented::ReaderCurrent | Presented::ReaderNext | Presented::ReaderPrevious
        )
    }
}

/// One caller shape the tally counts. The peer is the IP alone — a
/// port is ephemeral and would make every connection its own key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MissKey {
    pub peer: String,
    pub user: String,
    /// A standard method's name, or [`OTHER_METHOD`] — never the
    /// caller's own token (see `method_key`).
    pub method: String,
    pub route: String,
    pub presented: Presented,
}

/// One source's requests past its own share of the tally, by what they
/// presented. At most [`MAX_TALLY_KEYS`] / [`MAX_KEYS_PER_SOURCE`]
/// sources can fill a share, times the five presentations a share holds
/// (`none`, `mismatch`, and the three `reader.*`), so this list is
/// bounded without a bound of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOverflow {
    /// The source as [`source_of`] names it: an IPv4 address, or an
    /// IPv6 `/64`.
    pub source: String,
    pub presented: Presented,
    pub count: u64,
    /// When the first and the latest of these requests arrived, so a
    /// reader can age them out as it ages a row (review ebc7b1cc, B1).
    /// Absent (serde default) reads as "when is unknown", which a reader
    /// that must not release on a `previous` treats as inside any window.
    #[serde(default)]
    pub first_seen: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissRow {
    #[serde(flatten)]
    pub key: MissKey,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

struct Tally {
    rows: HashMap<MissKey, (u64, DateTime<Utc>, DateTime<Utc>)>,
    /// How many of `rows` each source holds under its share (a
    /// `previous` or a loopback key counts against none). Bounded by
    /// `rows`: a source is here only while it holds such a key.
    per_source: HashMap<String, usize>,
    /// Requests past their source's share, by source and presentation:
    /// count, first seen, last seen. Only a source holding a full share
    /// reaches it, so at most MAX_TALLY_KEYS / MAX_KEYS_PER_SOURCE
    /// sources, times the five presentations a share applies to.
    source_overflow: HashMap<(String, Presented), (u64, DateTime<Utc>, DateTime<Utc>)>,
    overflow: u64,
    /// When this tally began: the gate's mount, or the last time it
    /// left `off` or was lowered from `enforce` ([`MachineGate::observe`]).
    since: DateTime<Utc>,
}

impl Tally {
    fn starting(since: DateTime<Utc>) -> Self {
        Tally {
            rows: HashMap::new(),
            per_source: HashMap::new(),
            source_overflow: HashMap::new(),
            overflow: 0,
            since,
        }
    }
}

/// What the tally route answers. Both route answers derive
/// `Deserialize` because the credential broker reads them back (design
/// 6805c764, car 3): one definition of the wire shape, on both sides.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Misses {
    pub service: String,
    pub mode: Mode,
    pub rows: Vec<MissRow>,
    /// Requests whose key arrived after [`MAX_TALLY_KEYS`] were held that
    /// no one source's share took: what this counts cannot be attributed
    /// to one caller, and may include a `previous`.
    pub overflow: u64,
    /// Requests whose key arrived after their own source already held
    /// [`MAX_KEYS_PER_SOURCE`] keys, by source and by what they presented
    /// (`none`, `mismatch`, or a `reader.*`: only the estate `previous`
    /// is always keyed, review 177b4976), with when
    /// (backlog 93bcf490; review ebc7b1cc). Absent from a gate built
    /// before it, which counted all of these in `overflow` instead.
    #[serde(default)]
    pub source_overflow: Vec<SourceOverflow>,
    /// When this tally began to record: the gate's mount, or the last
    /// time the gate left `off` (which records nothing) or was lowered
    /// from `enforce`. The tally lives in memory, so a restart empties
    /// it, and a gate that spent part of a window in `off` saw nothing
    /// of that part: a reader judging "has nothing presented `previous`
    /// for a window" must refuse a tally that began inside the window,
    /// whose silence covers only part of it (adversarial review
    /// 70d449f9 of car 3, B2: a boot-time start read a gate switched to
    /// `report` a minute earlier as a clean day).
    pub recording_since: DateTime<Utc>,
    /// Why THIS process's tally is not clean, one reason each, empty
    /// when it is: `off`, a key `enforce` refuses, either overflow, a
    /// fact the recorder is refusing right now, or a fact lost that no
    /// `facts_lost` on the log states yet — which no tally restart
    /// forgives (review e4417d48, B2). The LIVE half of
    /// the clean window (design 21946380 point 3); the log half is
    /// `gate_evidence::window`, and neither is the verdict alone. A
    /// `previous` row is not a reason — every mode admits it. Absent
    /// from a gate built before it.
    #[serde(default)]
    pub not_clean: Vec<String>,
    /// Whether this process's facts reach the log at all.
    #[serde(default)]
    pub evidence: EvidenceHealth,
}

impl Misses {
    pub(crate) fn judge(&mut self) {
        let mut why = Vec::new();
        if self.mode == Mode::Off {
            why.push("mode `off` records nothing, so its silence is no evidence".to_string());
        }
        let refused: Vec<&MissRow> = self
            .rows
            .iter()
            .filter(|r| r.key.presented.enforce_refuses())
            .collect();
        if !refused.is_empty() {
            let n: u64 = refused.iter().map(|r| r.count).sum();
            why.push(format!(
                "{} caller shape(s), {n} request(s), that `enforce` refuses",
                refused.len()
            ));
        }
        if self.overflow > 0 {
            why.push(format!(
                "overflow {}: requests past the tally's {MAX_TALLY_KEYS} keys, which name no \
                 caller — a full tally is never clean",
                self.overflow
            ));
        }
        // A probe reader past its share is still a probe reader: only an
        // overflow of what `enforce` refuses names a caller it would.
        let spilled = self
            .source_overflow
            .iter()
            .filter(|o| o.presented.enforce_refuses())
            .count();
        if spilled > 0 {
            why.push(format!(
                "{spilled} source(s) past their share of the tally, whose further callers \
                 `enforce` refuses are unnamed"
            ));
        }
        why.extend(self.evidence.not_clean());
        self.not_clean = why;
    }
}

/// A server that deliberately mounts no machine gate: its `boss-ports`
/// name, its source file, and why. ONE definition, read by the pin that
/// holds every other server to the gate
/// (`every_service_mounts_the_machine_gate.rs`) and by the credential
/// broker's rotation roster, which must not count such a port as a gate
/// that never accepts (review 70d449f9, B1: the gateway answered 404 on
/// `/api/machine-gate/accepts`, and a rotation's verify waited forever).
#[derive(Clone, Copy, Debug)]
pub struct Ungated {
    /// None for a private socket that owns no network service port.
    pub service: Option<&'static str>,
    pub file: &'static str,
    pub why: &'static str,
}

pub const UNGATED: &[Ungated] = &[
    Ungated {
        service: Some("gateway"),
        file: "crates/core/boss-gateway/src/main.rs",
        why: "the edge, not a machine door: its first act on every request is the edge strip \
          (role_headers.rs strip_boss_headers), which removes every inbound x-boss-* header \
          INCLUDING x-boss-machine-token, so it trusts no asserted identity and a gate on it \
          could only ever refuse the browsers it exists to serve. The gateway's side of the \
          design is stamping the token on what it forwards (car 2).",
    },
    Ungated {
        service: None,
        file: "crates/orchestrators/boss-cli/src/probe_reader.rs",
        why: "a per-probe Unix socket, mode 0600, in a directory that stays its parent's own \
          (0700; 0711 when root hands the socket alone to the probe user); no network \
          listener or estate service port. It exists only where a reader credential is \
          deposited AND a gate names that credential a reader slot; its parent holds the \
          credential and forwards only body-free GET/HEAD requests to pinned estate \
          origins. The guard cancels all connections and removes the socket at probe exit \
          or at its own 60 s bound. Each of these is held by a test beside the code \
          (review 0bd6a9c2, F1), not by this sentence.",
    },
];

/// Does the server named `service` (a `boss-ports` name) mount a gate?
pub fn is_gated(service: &str) -> bool {
    !UNGATED.iter().any(|u| u.service == Some(service))
}

/// What the accepts route answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepts {
    pub service: String,
    pub mode: Mode,
    /// `current | next | previous | none`, or `reader.current |
    /// reader.next | reader.previous` for the probe-reader credential
    /// (design b35c22b4) — a name, never a value.
    pub matched: String,
    pub degraded: bool,
}

/// One service's gate: its name, its exact health exemptions, the last
/// reading of the mounted files, and its miss tally.
pub struct MachineGate {
    service: String,
    health: Vec<String>,
    reading: RwLock<Arc<Reading>>,
    tally: Mutex<Tally>,
    /// Where each new key, overflow and mode move is stated on the log
    /// (design 21946380): the tally restarts with the process, the log
    /// does not.
    evidence: Evidence,
}

impl std::fmt::Debug for MachineGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineGate")
            .field("service", &self.service)
            .field("health", &self.health)
            .field("reading", &self.reading())
            .finish_non_exhaustive()
    }
}

impl MachineGate {
    pub fn new(service: &str, health: &[&str], reading: Reading) -> Self {
        Self::starting_at(service, health, reading, Utc::now())
    }

    /// [`MachineGate::new`] with its tally's start given: for a reader's
    /// tests that need a gate which has been recording since before a
    /// window opened.
    pub fn starting_at(
        service: &str,
        health: &[&str],
        reading: Reading,
        since: DateTime<Utc>,
    ) -> Self {
        let evidence = Evidence::none(EvidenceGate::MachineGate, service);
        MachineGate {
            service: service.to_string(),
            health: health.iter().map(|p| p.to_string()).collect(),
            reading: RwLock::new(Arc::new(reading)),
            tally: Mutex::new(Tally::starting(since)),
            evidence,
        }
    }

    /// The gate stating its facts through `evidence`, beginning with
    /// the mode it records in now — a process start is a
    /// `recording_began` (design 21946380 point 2).
    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        let since = self
            .tally
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .since;
        let mode = self.reading().mode;
        evidence.recording_began(mode, since);
        self.evidence = evidence;
        self
    }

    pub fn reading(&self) -> Arc<Reading> {
        Arc::clone(&self.reading.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Take a fresh reading, and say so when it differs from the last —
    /// by mode and slot NAMES, never by value.
    pub fn observe(&self, next: Reading) {
        // The tally FIRST, held across the swap and the reset. `misses`
        // reads the mode under this same lock, so no reader can see the
        // new mode beside the old tally's start — `off` turned `report`
        // with a recording_since from a boot days ago reads as a window
        // that was watched and was not (review fd151a97, D2). The order
        // is the one `misses` takes, tally then reading, so the two
        // cannot deadlock; the request path takes the reading and
        // releases it before `record` takes the tally.
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cur = self.reading.write().unwrap_or_else(PoisonError::into_inner);
        if **cur == next {
            return;
        }
        let rotated_in = |a: &Slots, b: &Slots| a != b && a.present() == b.present();
        let rotated = rotated_in(&cur.slots, &next.slots) || rotated_in(&cur.reader, &next.reader);
        let left = cur.mode.leaves_enforce(next.mode);
        // A gate that recorded nothing (`off`) and now records has
        // watched only from now; one lowered from `enforce` starts over
        // too. Either way the tally begins again, so no reader takes an
        // earlier start for a watch that did not happen (review
        // 70d449f9, B2).
        let restarts = (cur.mode == Mode::Off && next.mode != Mode::Off) || left;
        let moved = (cur.mode != next.mode).then_some(next.mode);
        *cur = Arc::new(next);
        drop(cur);
        let now = Utc::now();
        if restarts {
            *tally = Tally::starting(now);
        }
        // EVERY mode move is on the log, `off` included — the move the
        // tally does not restart on is the one a projection most needs
        // to see, because `off` watches nothing (design 21946380 point
        // 2). Stated under the tally lock, so it precedes any key of
        // the new mode.
        if let Some(mode) = moved {
            self.evidence.recording_began(mode, now);
        }
        drop(tally);
        if left {
            tracing::warn!(
                service = %self.service,
                mode = self.reading().mode.name(),
                "machine gate LEFT enforce — a tokenless caller is admitted again"
            );
        }
        self.announce(if rotated {
            "machine gate reading changed (a slot's value changed)"
        } else {
            "machine gate reading changed"
        });
    }

    /// One line naming the mode and which slots are present.
    pub fn announce(&self, what: &str) {
        let r = self.reading();
        let slots = format!("{:?}", r.slots.present());
        let reader = format!("{:?}", r.reader.present());
        if let Some(e) = &r.mode_error {
            tracing::error!(service = %self.service, mode = r.mode.name(), %slots, %reader, "{what}: {e}");
        } else if r.degraded() {
            tracing::error!(
                service = %self.service,
                mode = r.mode.name(),
                %slots,
                %reader,
                "{what}: DEGRADED — mode enforce with no readable token; admitting every request \
                 as report (design 6805c764 choice 4)"
            );
        } else {
            tracing::info!(
                service = %self.service,
                mode = r.mode.name(),
                %slots,
                %reader,
                health_exempt = ?self.health,
                "{what}"
            );
        }
    }

    fn exempt(&self, req: &Request) -> bool {
        *req.method() == Method::OPTIONS
            || (*req.method() == Method::GET && self.health.iter().any(|p| p == req.uri().path()))
    }

    /// Tally one request, and state the first sighting of its key — or
    /// of an overflow — on the log: a key first seen while the gate
    /// refuses is `refused`, one it admits (`report`, a degraded
    /// `enforce`) is `would_refuse`. Both end a clean window, so a mode
    /// move between the request's judgement and this reading cannot make
    /// the record read cleaner than what happened.
    fn record(&self, req: &Request, presented: Presented) {
        let reading = self.reading();
        // A probe-reader presentation is its own fact, never a
        // would-refuse: its reads are admitted and its writes refused in
        // every mode, so `enforce` changes nothing for it and it ends no
        // clean window — but it is on the log, as `previous` is, for the
        // reader's own rotation to drain on.
        let fact = match presented {
            Presented::Previous => Fact::PreviousPresented,
            p if p.is_reader() => Fact::ReaderPresented,
            _ if reading.mode == Mode::Enforce && !reading.degraded() => Fact::Refused,
            _ => Fact::WouldRefuse,
        };
        let mode = reading.mode;
        let ip = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        let source = ip.map_or_else(|| "unknown".to_string(), source_of);
        let key = MissKey {
            peer: ip.map_or_else(|| "unknown".to_string(), |ip| ip.to_string()),
            user: user_id(req.headers()),
            method: if presented.is_reader() && !reads(req.method()) {
                REFUSED_WRITE.to_string()
            } else {
                method_key(req.method())
            },
            route: req
                .extensions()
                .get::<MatchedPath>()
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| "(unmatched)".to_string()),
            presented,
        };
        let now = Utc::now();
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(row) = tally.rows.get_mut(&key) {
            row.0 += 1;
            row.2 = now;
            return;
        }
        let since = tally.since;
        // The source's own share BEFORE the global bound: what a full
        // source sends is counted under its name even once the tally is
        // full, so `overflow` holds only what no one source can be
        // charged with (backlog 93bcf490).
        let shared = held_to_a_share(ip, presented);
        if shared && tally.per_source.get(&source).copied().unwrap_or(0) >= MAX_KEYS_PER_SOURCE {
            let n = tally
                .source_overflow
                .entry((source.clone(), presented))
                .or_insert((0, now, now));
            n.0 += 1;
            n.2 = now;
            // Once per source and presentation: at most two lines (and
            // two facts) for a source however long it sends.
            if n.0 == 1 {
                // Past its share, a probe reader is still named by what it
                // presented: a reader fact, which dirties nothing. Only an
                // overflow of what `enforce` refuses is a tally overflow.
                self.evidence.emit(
                    if presented.enforce_refuses() {
                        Fact::TallyOverflowed
                    } else {
                        Fact::ReaderPresented
                    },
                    serde_json::json!({
                        "mode": mode,
                        "recording_since": since,
                        "scope": "source",
                        "source": source,
                        "presented": presented,
                    }),
                );
                tracing::warn!(
                    service = %self.service,
                    source = %source,
                    presented = presented.name(),
                    "machine gate: source {source} holds its whole share of the tally \
                     ({MAX_KEYS_PER_SOURCE} keys); its further new keys presenting {} are \
                     counted by source, not keyed",
                    presented.name()
                );
            }
            return;
        }
        if tally.rows.len() >= MAX_TALLY_KEYS {
            tally.overflow += 1;
            if tally.overflow == 1 {
                self.evidence.emit(
                    Fact::TallyOverflowed,
                    serde_json::json!({
                        "mode": mode,
                        "recording_since": since,
                        "scope": "tally",
                    }),
                );
                tracing::warn!(
                    service = %self.service,
                    "machine gate tally is full ({MAX_TALLY_KEYS} keys); further new keys \
                     no one source's share can take are counted as overflow, which names no \
                     caller"
                );
            }
            return;
        }
        // Once per new key, so a restart loses the counts but never the
        // fact that this caller missed. A probe-reader READ is the
        // traffic the credential exists for, so it is INFO; a reader
        // write was refused, and is said at WARN like a miss.
        if presented.is_reader() && key.method != REFUSED_WRITE {
            tracing::info!(
                service = %self.service,
                peer = %key.peer,
                method = %key.method,
                route = %key.route,
                presented = presented.name(),
                "machine gate: a read under the probe-reader credential"
            );
        } else if presented.is_reader() {
            tracing::warn!(
                service = %self.service,
                peer = %key.peer,
                method = %key.method,
                route = %key.route,
                presented = presented.name(),
                "machine gate: a WRITE under the probe-reader credential, refused"
            );
        } else {
            tracing::warn!(
                service = %self.service,
                peer = %key.peer,
                user = %key.user,
                method = %key.method,
                route = %key.route,
                presented = ?key.presented,
                "machine gate: a request without an accepted current/next token"
            );
        }
        // And once on the LOG: the line above dies with the pod, this
        // does not (design 21946380 point 1 — one fact per key per
        // process, never one per request).
        self.evidence.emit(
            fact,
            serde_json::json!({
                "mode": mode,
                "recording_since": since,
                "key": &key,
            }),
        );
        tally.rows.insert(key, (1, now, now));
        if shared {
            *tally.per_source.entry(source).or_insert(0) += 1;
        }
    }

    /// The tally, sorted by key.
    pub fn misses(&self) -> Misses {
        let tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows: Vec<MissRow> = tally
            .rows
            .iter()
            .map(|(k, (count, first, last))| MissRow {
                key: k.clone(),
                count: *count,
                first_seen: *first,
                last_seen: *last,
            })
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        let mut source_overflow: Vec<SourceOverflow> = tally
            .source_overflow
            .iter()
            .map(
                |((source, presented), (count, first, last))| SourceOverflow {
                    source: source.clone(),
                    presented: *presented,
                    count: *count,
                    first_seen: Some(*first),
                    last_seen: Some(*last),
                },
            )
            .collect();
        source_overflow.sort_by(|a, b| (&a.source, a.presented).cmp(&(&b.source, b.presented)));
        let mut misses = Misses {
            service: self.service.clone(),
            mode: self.reading().mode,
            rows,
            overflow: tally.overflow,
            source_overflow,
            recording_since: tally.since,
            not_clean: Vec::new(),
            evidence: self.evidence.health(),
        };
        misses.judge();
        misses
    }

    /// What the accepts route answers for this presented value.
    pub fn accepts(&self, provided: Option<&str>) -> Accepts {
        let r = self.reading();
        let matched = match Matched::of(&r, provided) {
            Some(Matched::Estate(slot)) => slot.name().to_string(),
            Some(Matched::Reader(slot)) => Presented::reader(slot).name().to_string(),
            None => "none".to_string(),
        };
        self.answers(&r, matched)
    }

    fn answers(&self, r: &Reading, matched: String) -> Accepts {
        Accepts {
            service: self.service.clone(),
            mode: r.mode,
            matched,
            degraded: r.degraded(),
        }
    }

    /// A probe-reader match (design b35c22b4, Q2), in every mode: every
    /// inbound `x-boss-*` header removed — the credential itself among
    /// them, so no handler ever sees it — `x-boss-user` set to the probe
    /// reader, the match tallied outside `off`, and anything but GET or
    /// HEAD refused 403. The match rides on as an extension so the
    /// accepts route can still name it once the header is gone.
    async fn as_reader(&self, mode: Mode, slot: Slot, mut req: Request, next: Next) -> Response {
        let inbound: Vec<axum::http::HeaderName> = req
            .headers()
            .keys()
            .filter(|name| name.as_str().starts_with("x-boss-"))
            .cloned()
            .collect();
        for name in inbound {
            req.headers_mut().remove(&name);
        }
        let reader = crate::roles::reader_header(crate::roles::PROBE_READER_ACTOR);
        let Ok(user) = axum::http::HeaderValue::from_str(&reader) else {
            // Unreachable for the fixed ASCII JSON above; refused rather
            // than forwarded without an identity if it ever were not.
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "machine gate: the probe reader's identity is not a header value",
            )
                .into_response();
        };
        req.headers_mut().insert("x-boss-user", user);
        req.extensions_mut().insert(ReaderMatched(slot));
        let presented = Presented::reader(slot);
        if mode != Mode::Off {
            self.record(&req, presented);
        }
        if reads(req.method()) {
            return next.run(req).await;
        }
        (
            StatusCode::FORBIDDEN,
            format!(
                "machine gate ({}): the `{}` header carries the probe-reader credential ({}), \
                 which reads and never writes — {} is refused in every mode (design b35c22b4)",
                self.service,
                machine_token::HEADER,
                presented.name(),
                method_key(req.method()),
            ),
        )
            .into_response()
    }
}

/// The methods the probe-reader credential may use: GET and HEAD, by the
/// exact (case-sensitive) method, and nothing else (design b35c22b4, Q2).
/// One predicate for the refusal and the tally's key, so the two cannot
/// disagree about which request was a write.
fn reads(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD)
}

/// Which credential, and which of its slots, a presented value matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Matched {
    Estate(Slot),
    Reader(Slot),
}

impl Matched {
    /// Both slot sets are compared, each in constant time over its own
    /// slots, before either is chosen; the estate token wins a value in
    /// both, so a reader slot can only narrow what would otherwise miss.
    fn of(r: &Reading, provided: Option<&str>) -> Option<Self> {
        let estate = r.slots.matched(provided);
        let reader = r.reader.matched(provided);
        estate.map(Matched::Estate).or(reader.map(Matched::Reader))
    }
}

/// The probe-reader slot a request matched, set by the gate on a reader
/// match for the routes behind it; the header itself is gone by then.
#[derive(Clone, Copy, Debug)]
struct ReaderMatched(Slot);

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(machine_token::HEADER)
        .and_then(|v| v.to_str().ok())
}

/// The source a peer address counts against for [`MAX_KEYS_PER_SOURCE`]:
/// an IPv4 address as itself, and an IPv6 one as its `/64`, because a
/// host holds a whole /64 and can pick a fresh address in it for every
/// request — per-address shares would be no bound there. An IPv4-mapped
/// IPv6 address is the IPv4 host it maps.
pub fn source_of(ip: std::net::IpAddr) -> String {
    use std::net::{IpAddr, Ipv6Addr};
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                format!("{}/64", Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
        },
    }
}

/// Is a request from `ip` presenting `presented` held to its source's
/// [`MAX_KEYS_PER_SOURCE`]? Two are not (review ebc7b1cc), and both stay
/// under the global [`MAX_TALLY_KEYS`]:
///
/// * a `previous`: only a holder of the old secret can present it, so no
///   tokenless sprayer can use it to grow the tally, and a caller still
///   on the old value must be keyed by name — a row ages out of the
///   broker's drain window, and a count past a share held every later
///   revoke until the pod restarted (B1);
/// * loopback (127.0.0.0/8, ::1, and either mapped into IPv6): every
///   in-pod caller — the gateway forwarding every signed-in user, each
///   service client, the dispatcher's rule actors — reaches a gated port
///   from loopback (`boss_ports` hands out `http://127.0.0.1:<port>`), so
///   one share for loopback is one share for the whole pod, and report
///   mode could no longer name the in-pod callers enforce would refuse
///   (B2). A loopback caller is inside the pod, not a LAN caller.
///
/// A request with no peer (`None`) is the `unknown` source, held to one
/// share like any caller nothing can attribute.
///
/// The probe reader's `reader.previous` is NOT a `previous` for this
/// rule (review 177b4976, B1). The estate `previous` is exempt because
/// whoever holds the estate secret can already write anything; the
/// reader value is handed to builder-written probe code, and design
/// b35c22b4 treats its leak as a case that must yield reads and nothing
/// more. Exempt, one holder of a superseded reader value filled the
/// whole tally and spilled into `overflow`, which names no caller and
/// holds every estate revoke until a restart. Held to its share, what it
/// sends past the share is counted under its own source in
/// `source_overflow`, where the reader's rotation can still find it.
fn held_to_a_share(ip: Option<std::net::IpAddr>, presented: Presented) -> bool {
    presented != Presented::Previous && !ip.is_some_and(|ip| ip.to_canonical().is_loopback())
}

/// The asserted `x-boss-user` id, for the tally: `-` when absent, and
/// `(unparsed)` when the header is not the JSON the gateway and every
/// client send.
fn user_id(headers: &HeaderMap) -> String {
    let Some(raw) = headers.get("x-boss-user").and_then(|v| v.to_str().ok()) else {
        return "-".to_string();
    };
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(printable_user))
        .unwrap_or_else(|| "(unparsed)".to_string())
}

/// A decoded id as printable text of at most [`MAX_USER_CHARS`]. The
/// header is JSON, so `"a\nb"` or `"\u001b[31m"` decodes to a real
/// newline or ESC, and the id is printed on the key's WARN line and in
/// the credential broker's hold message, which lands in packet prose:
/// kept raw, a LAN caller could forge a log line or a packet line
/// (backlog 71b58708, from review 1829e95f). Every character Rust's
/// debug escape would escape is written as that escape — controls, NUL,
/// bidi overrides, and the backslash itself, so an escape in the output
/// can only have come from here. Quotes stay as they are; they end no
/// line. The bound counts the escaped text and never splits an escape.
fn printable_user(id: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in id.chars() {
        let piece = match c {
            '"' | '\'' => c.to_string(),
            _ => c.escape_debug().to_string(),
        };
        let n = piece.chars().count();
        if len + n > MAX_USER_CHARS {
            break;
        }
        out.push_str(&piece);
        len += n;
    }
    out
}

/// The middleware. Layered by [`gated`]; never mounted by hand.
pub async fn machine_gate(
    State(gate): State<Arc<MachineGate>>,
    req: Request,
    next: Next,
) -> Response {
    let reading = gate.reading();
    let provided = presented_token(req.headers());
    // The reader's scope comes FIRST and holds in every mode, health and
    // OPTIONS included: a probe-reader value reads, and never writes,
    // whatever window the gate is in (design b35c22b4, Q2).
    let matched = match Matched::of(&reading, provided) {
        Some(Matched::Reader(slot)) => return gate.as_reader(reading.mode, slot, req, next).await,
        Some(Matched::Estate(slot)) => Some(slot),
        None => None,
    };
    if reading.mode == Mode::Off || gate.exempt(&req) {
        return next.run(req).await;
    }
    match (matched, provided) {
        (Some(Slot::Previous), _) => gate.record(&req, Presented::Previous),
        (Some(_), _) => {}
        (None, None) => gate.record(&req, Presented::None),
        (None, Some(_)) => gate.record(&req, Presented::Mismatch),
    }
    if matched.is_some() || reading.mode == Mode::Report {
        return next.run(req).await;
    }
    if reading.degraded() {
        tracing::error!(
            service = %gate.service,
            "machine gate DEGRADED: mode enforce with no readable token; admitting as report \
             (design 6805c764 choice 4)"
        );
        return next.run(req).await;
    }
    (
        StatusCode::UNAUTHORIZED,
        format!(
            "machine gate ({}, mode enforce): this port requires the `{}` header to match an \
             accepted machine token (design 6805c764); this caller sent {}",
            gate.service,
            machine_token::HEADER,
            if provided.is_some() {
                "a token that does not match"
            } else {
                "no token"
            },
        ),
    )
        .into_response()
}

async fn accepts_route(
    State(gate): State<Arc<MachineGate>>,
    reader: Option<axum::Extension<ReaderMatched>>,
    headers: HeaderMap,
) -> Response {
    // A reader match reaches here with its header already removed (the
    // gate strips it before any handler); the gate's extension names it.
    if let Some(axum::Extension(ReaderMatched(slot))) = reader {
        let r = gate.reading();
        let matched = Presented::reader(slot).name().to_string();
        return Json(gate.answers(&r, matched)).into_response();
    }
    Json(gate.accepts(presented_token(&headers))).into_response()
}

async fn misses_route(State(gate): State<Arc<MachineGate>>, headers: HeaderMap) -> Response {
    if gate
        .reading()
        .slots
        .matched(presented_token(&headers))
        .is_none()
    {
        return (
            StatusCode::UNAUTHORIZED,
            format!(
                "the machine gate's tally names callers' addresses; read it with the `{}` \
                 header carrying an accepted machine token, in every mode",
                machine_token::HEADER
            ),
        )
            .into_response();
    }
    Json(gate.misses()).into_response()
}

/// The router with the gate's two routes merged and the gate layered
/// over everything. The testable half of [`mount`].
pub fn gated(router: Router, gate: Arc<MachineGate>) -> Router {
    let own = Router::new()
        .route(MISSES_PATH, get(misses_route))
        .route(ACCEPTS_PATH, get(accepts_route))
        .with_state(Arc::clone(&gate));
    router
        .merge(own)
        .layer(axum::middleware::from_fn_with_state(gate, machine_gate))
}

/// Mount the gate on a service's finished router and hand back what
/// `axum::serve` takes — with the peer address on every request, which
/// the tally keys on. `service` is the service's `boss-ports` name;
/// `health` is its exact health paths, exempt for GET.
///
/// `recorder` is where the gate states its facts on the log — the
/// transactional outbox (`boss_events::outbox::PgOutboxRecorder`) in
/// every binary with a database (design 21946380). A binary with none
/// passes `None`, says so at WARN here, and its service never shows as
/// recording in the log half of the clean window — so a window that
/// names it is never clean, rather than clean on a watch nobody kept.
///
/// A recorder makes the gate's evidence OWN SIGTERM for the process: at
/// SIGTERM it drains, states the process's end on the log and exits 143
/// (within 10 s). A binary passing one must not register its own SIGTERM
/// shutdown, which that exit would cut short — boss-testing's
/// `a_gated_binary_does_not_listen_for_sigterm` refuses one that does.
///
/// Reads the mounted files once now, then every few seconds, so a mode
/// or slot change lands without a restart. Call it inside the runtime,
/// as the last thing before `axum::serve`: a route merged after it is
/// not behind it.
pub fn mount(
    router: Router,
    service: &str,
    health: &[&str],
    recorder: Option<Arc<dyn EventRecorder>>,
) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    let files = MountedFiles::from_env();
    let evidence = match recorder {
        Some(r) => Evidence::spawn(EvidenceGate::MachineGate, service, r),
        None => {
            tracing::warn!(
                service,
                "machine gate mounted with no recorder: what it tallies reaches no log, so no \
                 clean window can name this service (design 21946380)"
            );
            Evidence::none(EvidenceGate::MachineGate, service)
        }
    };
    let gate =
        Arc::new(MachineGate::new(service, health, files.read_blocking()).with_evidence(evidence));
    gate.announce("machine gate mounted");
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        let watched = Arc::clone(&gate);
        rt.spawn(async move {
            loop {
                tokio::time::sleep(REREAD).await;
                watched.observe(files.read().await);
            }
        });
    } else {
        tracing::error!(
            service,
            "machine gate mounted outside a runtime: its mode and slots will not be re-read"
        );
    }
    gated(router, gate).into_make_service_with_connect_info::<SocketAddr>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, put};
    use tower::ServiceExt;

    const HEALTH: &str = "/api/things/health";

    fn slots() -> Slots {
        Slots::new(
            Some("cur-value".into()),
            Some("next-value".into()),
            Some("prev-value".into()),
        )
    }

    fn gate(mode: Mode, slots: Slots) -> Arc<MachineGate> {
        Arc::new(MachineGate::new(
            "things",
            &[HEALTH],
            Reading::new(mode, slots),
        ))
    }

    /// What a handler behind the gate sees: the `x-boss-user` it is
    /// handed, then every `x-boss-*` header name, sorted.
    const WHOAMI: &str = "/api/whoami";

    async fn whoami(headers: HeaderMap) -> String {
        let user = headers
            .get("x-boss-user")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        let mut boss: Vec<&str> = headers
            .keys()
            .map(|k| k.as_str())
            .filter(|k| k.starts_with("x-boss-"))
            .collect();
        boss.sort();
        format!("{user}|{}", boss.join(","))
    }

    fn app(gate: &Arc<MachineGate>) -> Router {
        let inner = Router::new()
            .route("/api/things/{id}", put(|| async { "written" }))
            .route("/api/things/{id}", get(|| async { "read" }))
            .route(HEALTH, get(|| async { "ok" }).post(|| async { "posted" }))
            .route(
                WHOAMI,
                get(whoami)
                    .post(whoami)
                    .put(whoami)
                    .patch(whoami)
                    .delete(whoami),
            );
        gated(inner, Arc::clone(gate))
    }

    async fn call(
        gate: &Arc<MachineGate>,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        call_with(gate, method, path, token, &[]).await
    }

    /// [`call`] with more headers beside the caller's `x-boss-user`.
    async fn call_with(
        gate: &Arc<MachineGate>,
        method: &str,
        path: &str,
        token: Option<&str>,
        extra: &[(&str, &str)],
    ) -> (StatusCode, String) {
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("x-boss-user", r#"{"id":"agent-seeder"}"#);
        if let Some(t) = token {
            req = req.header(machine_token::HEADER, t);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 20, 0, 7], 51234))));
        let resp = app(gate).oneshot(req).await.unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    /// `user_id` of a header whose JSON `id` is `json_id`, spelled as
    /// JSON text — a header cannot carry a raw control byte, but its
    /// JSON escape decodes to one.
    fn user_of(json_id: &str) -> String {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-boss-user",
            format!(r#"{{"id":"{json_id}"}}"#).parse().unwrap(),
        );
        user_id(&headers)
    }

    /// U+202E RIGHT-TO-LEFT OVERRIDE as JSON text, spelled in two halves
    /// so neither this source nor an editor turns it into the codepoint.
    const RLO: &str = concat!(r"\", "u202e");

    #[test]
    fn the_user_id_is_printed_escaped_so_a_caller_cannot_forge_a_line() {
        // Backlog 71b58708: the id is printed on the WARN line and in the
        // credential broker's hold message, which lands in packet prose.
        assert_eq!(user_of("agent-seeder"), "agent-seeder");
        assert_eq!(user_of(r"a\nb"), r"a\nb");
        assert_eq!(user_of(r"\u001b[31mred"), r"\u{1b}[31mred");
        assert_eq!(
            user_of(&format!("evil{RLO}gnp.exe")),
            r"evil\u{202e}gnp.exe"
        );
        assert_eq!(user_of(r"a\u0000b"), r"a\0b");
        // A literal backslash is escaped too, so an escape in the output
        // can only have come from the escaper.
        assert_eq!(user_of(r"a\\nb"), r"a\\nb");
        for id in [r"a\nb", r"\u001b[31m", RLO, r"\u0000"] {
            let shown = user_of(id);
            assert!(
                !shown.chars().any(|c| c.is_control() || c == '\u{202e}'),
                "{id} printed as {shown:?}"
            );
        }
    }

    #[test]
    fn the_user_id_bound_applies_after_escaping() {
        let shown = user_of(&r"\u001b".repeat(200));
        assert!(shown.chars().count() <= MAX_USER_CHARS, "{shown}");
        // Truncation never splits an escape: every piece is whole.
        assert_eq!(shown, r"\u{1b}".repeat(MAX_USER_CHARS / 6));
        let plain = user_of(&"x".repeat(200));
        assert_eq!(plain, "x".repeat(MAX_USER_CHARS));
    }

    #[test]
    fn the_gate_routes_are_recognised_however_they_are_spelled() {
        for path in [
            MISSES_PATH,
            ACCEPTS_PATH,
            "/api/machine-gate",
            "//api//machine-gate/misses",
            "/API/Machine-Gate/misses",
            "/api/machine%2dgate/misses",
            "/api/machine%252Dgate/misses",
            "%2fapi%2fmachine-gate%2fmisses",
            "\\api\\machine-gate\\misses",
        ] {
            assert!(is_gate_route(path), "{path} was not read as the gate's");
        }
        for path in [
            "/api/jobs/health",
            "/api/machine-gates/misses",
            "/api/jobs/machine-gate/misses",
            "/api/machine",
            "/",
            "",
            "/api/%zz/x",
        ] {
            assert!(!is_gate_route(path), "{path} was read as the gate's");
        }
    }

    #[test]
    fn mode_parses_the_three_words_and_defaults_to_off() {
        assert_eq!(Mode::parse(None), (Mode::Off, None));
        assert_eq!(Mode::parse(Some("")), (Mode::Off, None));
        assert_eq!(Mode::parse(Some(" off\n")), (Mode::Off, None));
        assert_eq!(Mode::parse(Some("report\n")), (Mode::Report, None));
        assert_eq!(Mode::parse(Some("ENFORCE")), (Mode::Enforce, None));
        // A typo admits everything, like off, but records — and says so.
        let (m, e) = Mode::parse(Some("enforc"));
        assert_eq!(m, Mode::Report);
        assert!(e.unwrap().contains("enforc"));
    }

    #[test]
    fn a_match_names_its_slot_and_debug_never_prints_a_value() {
        let s = slots();
        assert_eq!(s.matched(Some("cur-value")), Some(Slot::Current));
        assert_eq!(s.matched(Some("next-value")), Some(Slot::Next));
        assert_eq!(s.matched(Some("prev-value")), Some(Slot::Previous));
        assert_eq!(s.matched(Some("guess")), None);
        assert_eq!(s.matched(None), None);
        // A blank slot is absent, not a value an empty header matches.
        let blank = Slots::new(Some("  \n".into()), None, None);
        assert!(blank.is_empty());
        assert_eq!(blank.matched(Some("")), None);
        let shown = format!("{s:?} {:?}", gate(Mode::Enforce, slots()));
        assert!(!shown.contains("value"), "{shown}");
    }

    // ---- The probe-reader slots (design b35c22b4, Q1 and Q2) ----

    /// The probe-reader credential's three slots. No value here is a
    /// substring of an estate slot's, and none contains `value`, which
    /// the accepts test reads as an echo.
    fn reader() -> Slots {
        Slots::new(
            Some("rd-cur".into()),
            Some("rd-nxt".into()),
            Some("rd-prv".into()),
        )
    }

    fn gate_with_reader(mode: Mode) -> Arc<MachineGate> {
        Arc::new(MachineGate::new(
            "things",
            &[HEALTH],
            Reading::new(mode, slots()).with_reader(reader()),
        ))
    }

    const READER_SLOTS: [(&str, &str); 3] = [
        ("rd-cur", "reader.current"),
        ("rd-nxt", "reader.next"),
        ("rd-prv", "reader.previous"),
    ];

    /// Q2: on a reader match the server, not the door, decides who the
    /// caller is. Whatever `x-boss-user` it sent, and whatever other
    /// `x-boss-*` it carried (a runner credential, a role, a presence
    /// ticket), the handler sees the probe reader at `audit-readonly`
    /// and nothing else — not even the credential itself. In EVERY
    /// mode, including `off`, because the scope is a property of the
    /// credential and not of the window.
    #[tokio::test]
    async fn a_reader_value_reads_as_the_probe_reader_in_every_mode() {
        let want = crate::roles::reader_header(crate::roles::PROBE_READER_ACTOR);
        for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
            let g = gate_with_reader(mode);
            for (token, _) in READER_SLOTS {
                for method in ["GET", "HEAD"] {
                    let (s, body) = call_with(
                        &g,
                        method,
                        WHOAMI,
                        Some(token),
                        &[
                            ("x-boss-runner-credential", "smuggled"),
                            ("x-boss-role", "platform-admin"),
                            ("x-boss-presence", "ticket"),
                        ],
                    )
                    .await;
                    assert_eq!(s, StatusCode::OK, "{mode:?} {method} {token}");
                    if method == "GET" {
                        assert_eq!(body, format!("{want}|x-boss-user"), "{mode:?} {token}");
                    }
                }
            }
        }
        let who: serde_json::Value = serde_json::from_str(&want).unwrap();
        assert_eq!(who["id"], "automation:run-car-probe-reader");
        assert_eq!(who["role"], "audit-readonly");
    }

    /// Q2: a reader match never writes, in any mode. The door in front
    /// of it is a second layer, never the only one, so a door bug
    /// yields reads and nothing more.
    #[tokio::test]
    async fn a_reader_value_never_writes_in_any_mode() {
        for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
            let g = gate_with_reader(mode);
            for (token, name) in READER_SLOTS {
                for method in ["POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE"] {
                    let (s, body) = call(&g, method, WHOAMI, Some(token)).await;
                    assert_eq!(s, StatusCode::FORBIDDEN, "{mode:?} {method} {token}");
                    assert!(
                        body.contains(name) && body.contains(method) && !body.contains(token),
                        "{body}"
                    );
                }
                // The health exemption is for GET without a token; it
                // opens no write to the reader either.
                assert_eq!(
                    call(&g, "POST", HEALTH, Some(token)).await.0,
                    StatusCode::FORBIDDEN,
                    "{mode:?} {token}"
                );
            }
        }
    }

    /// The estate token is judged exactly as before beside a reader slot
    /// set: its writes pass, its caller's identity is its own, and a
    /// tokenless or guessed caller is answered as before in each mode.
    #[tokio::test]
    async fn the_estate_token_is_unchanged_beside_a_reader_slot() {
        for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
            let g = gate_with_reader(mode);
            for t in ["cur-value", "next-value", "prev-value"] {
                let (s, body) = call(&g, "PUT", WHOAMI, Some(t)).await;
                assert_eq!(s, StatusCode::OK, "{mode:?} {t}");
                assert!(body.starts_with(r#"{"id":"agent-seeder"}|"#), "{body}");
                assert!(body.contains(machine_token::HEADER), "{body}");
            }
            let want = if mode == Mode::Enforce {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::OK
            };
            assert_eq!(call(&g, "PUT", WHOAMI, None).await.0, want, "{mode:?}");
            assert_eq!(
                call(&g, "PUT", WHOAMI, Some("guess")).await.0,
                want,
                "{mode:?}"
            );
        }
    }

    /// A value present in BOTH sets is the estate token's: the reader
    /// slot can narrow only what would otherwise have missed.
    #[tokio::test]
    async fn a_value_in_both_sets_is_the_estate_tokens() {
        let both = Slots::new(Some("cur-value".into()), None, None);
        let g = Arc::new(MachineGate::new(
            "things",
            &[HEALTH],
            Reading::new(Mode::Report, slots()).with_reader(both),
        ));
        assert_eq!(
            call(&g, "PUT", WHOAMI, Some("cur-value")).await.0,
            StatusCode::OK
        );
        let (_, body) = call(&g, "GET", ACCEPTS_PATH, Some("cur-value")).await;
        let a: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(a["matched"], "current");
    }

    /// The accepts route names a reader match `reader.<slot>` — what the
    /// forge's deposit and the broker's verify read back — and the tally
    /// stays the estate token's alone: a caller's address is a record,
    /// and a read credential is not the credential that reads it.
    #[tokio::test]
    async fn accepts_names_a_reader_slot_and_the_tally_refuses_it() {
        for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
            let g = gate_with_reader(mode);
            for (token, name) in READER_SLOTS {
                let (s, body) = call(&g, "GET", ACCEPTS_PATH, Some(token)).await;
                assert_eq!(s, StatusCode::OK, "{mode:?}");
                let a: Accepts = serde_json::from_str(&body).unwrap();
                assert_eq!(a.matched, name, "{mode:?}");
                assert!(!body.contains(token), "{body}");
                assert_eq!(g.accepts(Some(token)).matched, name);
                assert_eq!(
                    call(&g, "GET", MISSES_PATH, Some(token)).await.0,
                    StatusCode::UNAUTHORIZED,
                    "{mode:?} {token}"
                );
            }
            let (_, body) = call(&g, "GET", ACCEPTS_PATH, Some("cur-value")).await;
            let a: Accepts = serde_json::from_str(&body).unwrap();
            assert_eq!(a.matched, "current");
        }
    }

    /// The window sees probe traffic AS probe traffic: every reader
    /// presentation is tallied `reader.<slot>` under the reader's own id
    /// (not the caller's text), a refused write keeps its method, and
    /// `off` records nothing, as it records nothing else.
    #[tokio::test]
    async fn the_tally_names_reader_traffic_by_its_slot() {
        for mode in [Mode::Report, Mode::Enforce] {
            let g = gate_with_reader(mode);
            for (token, _) in READER_SLOTS {
                call(&g, "GET", "/api/things/1", Some(token)).await;
                call(&g, "GET", "/api/things/2", Some(token)).await;
            }
            call(&g, "PUT", "/api/things/1", Some("rd-cur")).await;
            let m = g.misses();
            let rows: Vec<(String, String, Presented, u64)> = m
                .rows
                .iter()
                .map(|r| {
                    (
                        r.key.method.clone(),
                        r.key.user.clone(),
                        r.key.presented,
                        r.count,
                    )
                })
                .collect();
            let reader_id = crate::roles::PROBE_READER_ACTOR.to_string();
            assert_eq!(
                rows,
                vec![
                    // A refused write keys as one method-less row per
                    // route (review 177b4976, B1); it sorts first.
                    (
                        REFUSED_WRITE.into(),
                        reader_id.clone(),
                        Presented::ReaderCurrent,
                        1
                    ),
                    ("GET".into(), reader_id.clone(), Presented::ReaderCurrent, 2),
                    ("GET".into(), reader_id.clone(), Presented::ReaderNext, 2),
                    (
                        "GET".into(),
                        reader_id.clone(),
                        Presented::ReaderPrevious,
                        2
                    ),
                ],
                "{mode:?} {m:?}"
            );
            assert!(m.rows.iter().all(|r| r.key.route == "/api/things/{id}"));
        }
        let g = gate_with_reader(Mode::Off);
        call(&g, "GET", "/api/things/1", Some("rd-cur")).await;
        assert!(g.misses().rows.is_empty());
    }

    /// One request through the whole gate on a router of `routes` paths,
    /// from `peer`, presenting `token`.
    async fn sweep_call(
        gate: &Arc<MachineGate>,
        routes: usize,
        method: &str,
        path: &str,
        peer: [u8; 4],
        token: Option<&str>,
    ) -> StatusCode {
        let inner = (0..routes).fold(Router::new(), |r, i| {
            r.route(
                &format!("/api/r{i}/{{id}}"),
                get(|| async { "read" })
                    .post(|| async { "w" })
                    .put(|| async { "w" })
                    .patch(|| async { "w" })
                    .delete(|| async { "w" }),
            )
        });
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("x-boss-user", r#"{"id":"agent-seeder"}"#);
        if let Some(t) = token {
            req = req.header(machine_token::HEADER, t);
        }
        let mut req = req.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((peer, 40000))));
        gated(inner, Arc::clone(gate))
            .oneshot(req)
            .await
            .unwrap()
            .status()
    }

    /// Review 177b4976, B1: the reader value goes to builder-written
    /// probe code, so a superseded one is NOT a secret only a writer
    /// holds, and `reader.previous` must be held to its source's share
    /// like any other caller. Unheld, one peer sweeping 120 routes with
    /// ten methods filled all [`MAX_TALLY_KEYS`] and spilled into
    /// `overflow` — which names no caller, so a later tokenless caller
    /// went unnamed, and the broker holds every estate revoke while it
    /// is above zero, until a restart.
    #[tokio::test]
    async fn one_reader_previous_peer_cannot_fill_the_tally() {
        const ROUTES: usize = 120;
        let methods = [
            "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "CONNECT",
            "PROPFIND",
        ];
        let g = gate_with_reader(Mode::Report);
        for i in 0..ROUTES {
            for m in methods {
                sweep_call(
                    &g,
                    ROUTES,
                    m,
                    &format!("/api/r{i}/1"),
                    [10, 0, 0, 9],
                    Some("rd-prv"),
                )
                .await;
            }
        }
        // A later caller from another source, sending no token.
        sweep_call(&g, ROUTES, "PUT", "/api/r0/1", [10, 0, 0, 50], None).await;
        let m = g.misses();
        assert_eq!(m.overflow, 0, "nothing past the global cap: {m:?}");
        let sweeper = m.rows.iter().filter(|r| r.key.peer == "10.0.0.9").count();
        assert!(sweeper <= MAX_KEYS_PER_SOURCE, "{sweeper} keys");
        assert!(
            m.rows
                .iter()
                .any(|r| r.key.peer == "10.0.0.50" && r.key.presented == Presented::None),
            "the later caller is keyed by name: {m:?}"
        );
        assert!(
            m.source_overflow
                .iter()
                .any(|o| o.source == "10.0.0.9" && o.presented == Presented::ReaderPrevious),
            "what the sweeper sent past its share is counted under its name: {:?}",
            m.source_overflow
        );
    }

    /// A refused reader write never reached a handler, so its method says
    /// nothing a reader of the tally needs: every refused method on a
    /// route is ONE key, and a sweep cannot multiply its footprint by the
    /// methods it tries (review 177b4976, B1).
    #[tokio::test]
    async fn a_refused_reader_write_is_one_key_per_route_whatever_its_method() {
        let g = gate_with_reader(Mode::Report);
        for m in [
            "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "PROPFIND",
        ] {
            assert_eq!(
                call(&g, m, "/api/things/1", Some("rd-cur")).await.0,
                StatusCode::FORBIDDEN
            );
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), 1, "{m:?}");
        assert_eq!(m.rows[0].key.method, REFUSED_WRITE);
        assert_eq!(m.rows[0].count, 7);
        assert_eq!(m.rows[0].key.presented, Presented::ReaderCurrent);
    }

    /// The wire names, read back by the broker as the one type.
    #[test]
    fn a_reader_presentation_is_named_by_its_slot_on_the_wire() {
        for (p, name) in [
            (Presented::ReaderCurrent, "reader.current"),
            (Presented::ReaderNext, "reader.next"),
            (Presented::ReaderPrevious, "reader.previous"),
        ] {
            assert_eq!(p.name(), name);
            let wire = serde_json::to_string(&p).unwrap();
            assert_eq!(wire, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Presented>(&wire).unwrap(), p);
        }
        assert_eq!(Presented::reader(Slot::Next), Presented::ReaderNext);
    }

    /// The reader slots are their own mounted directory. Unnamed, or
    /// named and absent, the set is empty — every pod until the Secret
    /// is mounted — and an empty set matches nothing, so this car
    /// changes no caller's answer.
    #[tokio::test]
    async fn mounted_files_read_the_reader_slots_from_their_own_directory() {
        let dir = std::env::temp_dir().join(format!(
            "boss-core-machine-gate-reader-{}",
            uuid::Uuid::new_v4()
        ));
        let tokens = dir.join("tokens");
        let readers = dir.join("reader");
        std::fs::create_dir_all(&tokens).unwrap();
        std::fs::write(tokens.join("current"), "cur-value\n").unwrap();

        let unnamed = MountedFiles::new(dir.join("mode"), &tokens);
        assert!(unnamed.read_blocking().reader.is_empty());
        let files = unnamed.with_reader_dir(&readers);
        assert!(files.read_blocking().reader.is_empty(), "absent dir");

        std::fs::create_dir_all(&readers).unwrap();
        std::fs::write(readers.join("current"), "rd-cur\n").unwrap();
        std::fs::write(readers.join("next"), "  \n").unwrap();
        let r = files.read().await;
        assert_eq!(r.reader.present(), vec![Slot::Current]);
        assert_eq!(r.slots.present(), vec![Slot::Current]);
        assert_eq!(r.reader.matched(Some("rd-cur")), Some(Slot::Current));
        assert_eq!(r.reader.matched(Some("cur-value")), None);
        assert!(!format!("{r:?}").contains("rd-cur"), "{r:?}");

        let g = gate(Mode::Off, Slots::default());
        g.observe(r);
        assert_eq!(g.accepts(Some("rd-cur")).matched, "reader.current");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn off_admits_everything_and_records_nothing() {
        // The default this car ships: nothing any caller sees changes.
        let g = gate(Mode::Off, slots());
        assert_eq!(
            call(&g, "PUT", "/api/things/1", None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(&g, "GET", "/api/things/1", Some("guess")).await.0,
            StatusCode::OK
        );
        assert!(g.misses().rows.is_empty());
    }

    #[tokio::test]
    async fn report_admits_the_tokenless_and_tallies_them_by_caller_and_route() {
        let g = gate(Mode::Report, slots());
        assert_eq!(
            call(&g, "PUT", "/api/things/1", None).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(&g, "PUT", "/api/things/2", None).await.0,
            StatusCode::OK
        );
        // Reads join the gate (design choice 7).
        assert_eq!(
            call(&g, "GET", "/api/things/3", Some("guess")).await.0,
            StatusCode::OK
        );
        // A current token is not a miss.
        assert_eq!(
            call(&g, "GET", "/api/things/4", Some("cur-value")).await.0,
            StatusCode::OK
        );
        let m = g.misses();
        assert_eq!(m.rows.len(), 2, "{m:?}");
        let put = m.rows.iter().find(|r| r.key.method == "PUT").unwrap();
        assert_eq!(put.count, 2);
        assert_eq!(put.key.peer, "10.20.0.7");
        assert_eq!(put.key.user, "agent-seeder");
        assert_eq!(put.key.route, "/api/things/{id}");
        assert_eq!(put.key.presented, Presented::None);
        let get = m.rows.iter().find(|r| r.key.method == "GET").unwrap();
        assert_eq!(get.key.presented, Presented::Mismatch);
    }

    #[tokio::test]
    async fn the_previous_slot_is_admitted_and_counted_for_the_revoke() {
        let g = gate(Mode::Enforce, slots());
        assert_eq!(
            call(&g, "PUT", "/api/things/1", Some("prev-value")).await.0,
            StatusCode::OK
        );
        assert_eq!(
            call(&g, "PUT", "/api/things/1", Some("next-value")).await.0,
            StatusCode::OK
        );
        let m = g.misses();
        assert_eq!(m.rows.len(), 1);
        assert_eq!(m.rows[0].key.presented, Presented::Previous);
    }

    #[tokio::test]
    async fn enforce_refuses_tokenless_reads_and_writes_naming_the_mode() {
        let g = gate(Mode::Enforce, slots());
        let (s, body) = call(&g, "PUT", "/api/things/1", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(
            body.contains("mode enforce") && body.contains("no token"),
            "{body}"
        );
        let (s, body) = call(&g, "GET", "/api/things/1", Some("guess")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert!(body.contains("does not match"), "{body}");
        for t in ["cur-value", "next-value", "prev-value"] {
            assert_eq!(
                call(&g, "PUT", "/api/things/1", Some(t)).await.0,
                StatusCode::OK
            );
        }
    }

    #[tokio::test]
    async fn enforce_with_no_readable_token_degrades_to_report_and_says_so() {
        // Nothing takes the SoR dark: a missing Secret never refuses all.
        let g = gate(Mode::Enforce, Slots::default());
        assert_eq!(
            call(&g, "PUT", "/api/things/1", None).await.0,
            StatusCode::OK
        );
        assert_eq!(g.misses().rows.len(), 1);
        let (s, body) = call(&g, "GET", ACCEPTS_PATH, None).await;
        assert_eq!(s, StatusCode::OK);
        let a: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(a["degraded"], true);
        assert_eq!(a["mode"], "enforce");
    }

    #[tokio::test]
    async fn health_is_exempt_by_exact_path_for_get_only_and_options_stays_open() {
        let g = gate(Mode::Enforce, slots());
        assert_eq!(call(&g, "GET", HEALTH, None).await.0, StatusCode::OK);
        assert_eq!(
            call(&g, "POST", HEALTH, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&g, "GET", "/api/things/health/extra", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_ne!(
            call(&g, "OPTIONS", "/api/things/1", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert!(
            g.misses()
                .rows
                .iter()
                .all(|r| r.key.route != HEALTH || r.key.method != "GET")
        );
    }

    #[tokio::test]
    async fn accepts_names_the_matched_slot_and_never_echoes_a_value() {
        let g = gate(Mode::Report, slots());
        for (token, want) in [
            (Some("cur-value"), "current"),
            (Some("next-value"), "next"),
            (Some("prev-value"), "previous"),
            (Some("guess"), "none"),
            (None, "none"),
        ] {
            let (s, body) = call(&g, "GET", ACCEPTS_PATH, token).await;
            assert_eq!(s, StatusCode::OK);
            let a: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(a["matched"], want, "{body}");
            assert_eq!(a["mode"], "report");
            assert_eq!(a["degraded"], false);
            assert!(!body.contains("value"), "{body}");
        }
    }

    #[tokio::test]
    async fn the_tally_answers_only_an_accepted_token_in_every_mode() {
        for mode in [Mode::Off, Mode::Report, Mode::Enforce] {
            let g = gate(mode, slots());
            assert_eq!(
                call(&g, "GET", MISSES_PATH, None).await.0,
                StatusCode::UNAUTHORIZED,
                "{mode:?}"
            );
            assert_eq!(
                call(&g, "GET", MISSES_PATH, Some("guess")).await.0,
                StatusCode::UNAUTHORIZED,
                "{mode:?}"
            );
            let (s, body) = call(&g, "GET", MISSES_PATH, Some("cur-value")).await;
            assert_eq!(s, StatusCode::OK, "{mode:?}");
            assert!(!body.contains("cur-value"), "{body}");
        }
    }

    /// The broker's rotation (design 6805c764, car 3) reads both routes
    /// back into these same types — one definition of the wire shape, so
    /// the reader cannot drift from the gate. And the tally says when it
    /// began: a process younger than the revoke's drain window has not
    /// watched the whole window, so its empty tally is not a clean one
    /// (review of car 1, a159e1ee: "a reader must treat a process
    /// younger than the window as NOT clean").
    #[tokio::test]
    async fn both_routes_read_back_as_their_types_and_the_tally_names_its_start() {
        let before = Utc::now();
        let g = gate(Mode::Report, slots());
        call(&g, "GET", "/api/things/7", Some("prev-value")).await;
        let (s, body) = call(&g, "GET", MISSES_PATH, Some("cur-value")).await;
        assert_eq!(s, StatusCode::OK);
        let m: Misses = serde_json::from_str(&body).expect("the tally reads back as Misses");
        assert_eq!(m.service, "things");
        assert_eq!(m.mode, Mode::Report);
        assert_eq!(m.rows.len(), 1);
        assert_eq!(m.rows[0].key.presented, Presented::Previous);
        assert_eq!(m.rows[0].key.route, "/api/things/{id}");
        assert!(
            m.recording_since >= before - chrono::Duration::seconds(1)
                && m.recording_since <= Utc::now(),
            "recording_since is when this gate began to tally: {}",
            m.recording_since
        );
        let (_, body) = call(&g, "GET", ACCEPTS_PATH, Some("next-value")).await;
        let a: Accepts = serde_json::from_str(&body).expect("accepts reads back as Accepts");
        assert_eq!(
            a,
            Accepts {
                service: "things".into(),
                mode: Mode::Report,
                matched: "next".into(),
                degraded: false,
            }
        );
    }

    /// A gate that records nothing in `off` and then begins recording
    /// has watched only since it began. Its tally's start is that moment,
    /// not its boot — else a broker's drain read a gate booted days ago,
    /// flipped to `report` a minute ago, as a clean day (adversarial
    /// review 70d449f9 of car 3, B2). And a gate lowered out of `enforce`
    /// starts over too.
    #[tokio::test]
    async fn the_tally_restarts_whenever_the_gate_leaves_off_or_lowers_from_enforce() {
        let days_ago = Utc::now() - chrono::Duration::days(3);
        for (from, to, restarts) in [
            (Mode::Off, Mode::Report, true),
            (Mode::Off, Mode::Enforce, true),
            (Mode::Enforce, Mode::Report, true),
            (Mode::Enforce, Mode::Off, true),
            (Mode::Report, Mode::Enforce, false),
            (Mode::Report, Mode::Report, false),
        ] {
            let g = Arc::new(MachineGate::starting_at(
                "things",
                &[HEALTH],
                Reading::new(from, slots()),
                days_ago,
            ));
            if from != Mode::Off {
                call(&g, "GET", "/api/things/7", None).await;
            }
            // A slot change alongside, so an unchanged mode still re-reads.
            g.observe(Reading::new(
                to,
                Slots::new(Some("cur-value".into()), None, None),
            ));
            let m = g.misses();
            if restarts {
                assert!(
                    m.recording_since > Utc::now() - chrono::Duration::minutes(1),
                    "{from:?} -> {to:?}: the tally starts over: {}",
                    m.recording_since
                );
                assert!(m.rows.is_empty() && m.overflow == 0, "{from:?} -> {to:?}");
            } else {
                assert_eq!(
                    m.recording_since, days_ago,
                    "{from:?} -> {to:?} keeps its start"
                );
                assert_eq!(m.rows.len(), 1, "{from:?} -> {to:?} keeps its rows");
            }
        }
    }

    /// The one list of servers that mount no gate, read by the pin that
    /// holds every server to the gate and by the broker's roster alike
    /// (review 70d449f9, B1: the roster counted the gateway as a gate).
    #[test]
    fn the_ungated_servers_are_named_once_by_their_ports_name() {
        assert_eq!(
            UNGATED.iter().filter_map(|u| u.service).collect::<Vec<_>>(),
            vec!["gateway"]
        );
        assert!(!is_gated("gateway"));
        assert!(is_gated("jobs") && is_gated("dispatcher"));
    }

    #[test]
    fn the_private_probe_reader_has_no_network_gate_to_rotate() {
        let reader = UNGATED
            .iter()
            .find(|entry| entry.file == "crates/orchestrators/boss-cli/src/probe_reader.rs");
        assert!(
            reader.is_some(),
            "the private reader requires an explicit ungated rationale"
        );
        assert_eq!(reader.unwrap().service, None);
        assert!(is_gated("jobs") && is_gated("dispatcher"));
    }

    /// A request from `peer` asserting `user`, as the tally sees it.
    fn from(peer: std::net::IpAddr, user: &str) -> Request {
        let mut req = axum::http::Request::builder()
            .method("GET")
            .uri("/api/things/1")
            .header("x-boss-user", format!(r#"{{"id":"{user}"}}"#))
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer, 40000)));
        req
    }

    fn v4(a: u8, b: u8) -> std::net::IpAddr {
        std::net::IpAddr::from([10, 20, a, b])
    }

    /// The global bound still holds when the keys come from many sources,
    /// each under its own share: memory stays bounded however many
    /// addresses a spray uses, and what it could not key is `overflow`.
    #[tokio::test]
    async fn the_tally_is_bounded() {
        let g = gate(Mode::Report, slots());
        for i in 0..(MAX_TALLY_KEYS + 5) {
            // Each key its own source, so no share fills first.
            let peer = v4((i / 256) as u8, (i % 256) as u8);
            g.record(&from(peer, &format!("caller-{i}")), Presented::None);
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), MAX_TALLY_KEYS);
        assert_eq!(m.overflow, 5);
        assert!(m.source_overflow.is_empty(), "{:?}", m.source_overflow);
    }

    /// Backlog 93bcf490 (review 1829e95f, finding 2): one LAN caller that
    /// varies `x-boss-user` filled every key, so every other caller's
    /// first miss — a host still sending `previous`, the very thing the
    /// broker's revoke waits on — was only a number. One source now holds
    /// its own share and no more; what it sends past that is counted
    /// under its name, and the rest of the tally stays free.
    #[tokio::test]
    async fn one_source_spraying_users_leaves_room_for_every_other_sources_misses() {
        let g = gate(Mode::Report, slots());
        let noisy = v4(0, 66);
        for i in 0..5000 {
            g.record(&from(noisy, &format!("spray-{i}")), Presented::None);
        }
        // A real caller after the spray, still on the old value.
        g.record(
            &from(v4(0, 30), "automation:forge-converge"),
            Presented::Previous,
        );
        let m = g.misses();
        assert_eq!(m.overflow, 0, "one source filled nobody else's room");
        let theirs = m.rows.iter().filter(|r| r.key.peer == "10.20.0.66").count();
        assert_eq!(theirs, MAX_KEYS_PER_SOURCE);
        let named = m
            .rows
            .iter()
            .find(|r| r.key.peer == "10.20.0.30")
            .expect("the real caller is keyed, by address and user");
        assert_eq!(named.key.user, "automation:forge-converge");
        assert_eq!(named.key.presented, Presented::Previous);
        assert_eq!(
            counts(&m),
            vec![(
                "10.20.0.66".to_string(),
                Presented::None,
                (5000 - MAX_KEYS_PER_SOURCE) as u64
            )]
        );
    }

    /// `source_overflow` as (source, presentation, count), times aside.
    fn counts(m: &Misses) -> Vec<(String, Presented, u64)> {
        m.source_overflow
            .iter()
            .map(|o| (o.source.clone(), o.presented, o.count))
            .collect()
    }

    /// A `previous` is never held to a source's share (review ebc7b1cc,
    /// B1). Only a holder of the old secret can present it, so a
    /// tokenless sprayer cannot use it to grow the tally — and a caller
    /// still on the old value that shares the sprayer's address must be
    /// keyed by name, so its row ages out of the drain window like any
    /// other. Counted past the share it had no time, and held every
    /// later revoke until the pod restarted.
    #[tokio::test]
    async fn a_previous_is_never_held_to_a_sources_share() {
        let g = gate(Mode::Report, slots());
        let host = v4(0, 66);
        for i in 0..(MAX_KEYS_PER_SOURCE + 3) {
            g.record(&from(host, &format!("spray-{i}")), Presented::None);
        }
        g.record(&from(host, "a-real-caller"), Presented::Previous);
        g.record(&from(host, "another"), Presented::Previous);
        g.record(&from(host, "guesser"), Presented::Mismatch);
        let m = g.misses();
        assert_eq!(m.overflow, 0);
        let prev: Vec<&str> = m
            .rows
            .iter()
            .filter(|r| r.key.presented == Presented::Previous)
            .map(|r| r.key.user.as_str())
            .collect();
        assert_eq!(prev, vec!["a-real-caller", "another"], "keyed by name");
        assert_eq!(
            counts(&m),
            vec![
                ("10.20.0.66".to_string(), Presented::None, 3),
                ("10.20.0.66".to_string(), Presented::Mismatch, 1),
            ]
        );
    }

    /// What a source sent past its share says when, so it can age out
    /// like a row (review ebc7b1cc, B1).
    #[tokio::test]
    async fn a_sources_overflow_says_when_it_was_seen() {
        let before = Utc::now();
        let g = gate(Mode::Report, slots());
        for i in 0..(MAX_KEYS_PER_SOURCE + 2) {
            g.record(&from(v4(0, 66), &format!("spray-{i}")), Presented::None);
        }
        let m = g.misses();
        let o = &m.source_overflow[0];
        let (first, last) = (o.first_seen.expect("first"), o.last_seen.expect("last"));
        assert!(
            before <= first && first <= last && last <= Utc::now(),
            "{o:?}"
        );
    }

    /// Every in-pod caller reaches a gated port from loopback — boss-ports
    /// hands out `http://127.0.0.1:<port>`, and the gateway forwards every
    /// signed-in user from the same pod — so a loopback share would give
    /// the whole pod 64 keys where it had 1,024, and report mode could no
    /// longer name the in-pod callers enforce would refuse (review
    /// ebc7b1cc, B2). Loopback is inside the pod, not a LAN caller: it is
    /// held to the global cap only.
    #[tokio::test]
    async fn in_pod_callers_are_held_only_to_the_global_cap() {
        let g = gate(Mode::Report, slots());
        let loopbacks: [std::net::IpAddr; 3] = [
            "127.0.0.1".parse().unwrap(),
            "::1".parse().unwrap(),
            "::ffff:127.0.0.2".parse().unwrap(),
        ];
        for peer in loopbacks {
            for i in 0..200 {
                g.record(&from(peer, &format!("rule-{i}")), Presented::None);
            }
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), 600, "every in-pod caller keyed by name");
        assert!(m.source_overflow.is_empty(), "{:?}", m.source_overflow);
        for i in 0..1000 {
            g.record(&from(loopbacks[0], &format!("more-{i}")), Presented::None);
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), MAX_TALLY_KEYS, "still bounded");
        assert_eq!(m.overflow, (600 + 1000 - MAX_TALLY_KEYS) as u64);
    }

    /// A request with no peer address (no ConnectInfo — a router served
    /// without `mount`) is one source, `unknown`, held to one share like
    /// any unattributable caller (review ebc7b1cc, N4).
    #[tokio::test]
    async fn a_request_with_no_peer_is_the_unknown_source() {
        let g = gate(Mode::Report, slots());
        for i in 0..(MAX_KEYS_PER_SOURCE + 2) {
            let req = axum::http::Request::builder()
                .uri("/api/things/1")
                .header("x-boss-user", format!(r#"{{"id":"u-{i}"}}"#))
                .body(Body::empty())
                .unwrap();
            g.record(&req, Presented::None);
        }
        let m = g.misses();
        assert!(m.rows.iter().all(|r| r.key.peer == "unknown"));
        assert_eq!(m.rows.len(), MAX_KEYS_PER_SOURCE);
        assert_eq!(
            counts(&m),
            vec![("unknown".to_string(), Presented::None, 2)]
        );
    }

    /// Many sources are not one noisy caller: sixteen full shares are the
    /// whole tally, and every key after that is `overflow`, unattributed,
    /// which the broker holds a revoke on exactly as before.
    #[tokio::test]
    async fn a_many_source_spray_still_overflows_the_tally() {
        let g = gate(Mode::Report, slots());
        for i in 0..100 {
            for s in 0..64u8 {
                g.record(&from(v4(1, s), &format!("spray-{i}")), Presented::None);
            }
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), MAX_TALLY_KEYS);
        assert!(m.overflow > 0, "{} overflowed", m.overflow);
        // A late real caller from a fresh source is past the whole tally.
        g.record(
            &from(v4(0, 30), "automation:forge-converge"),
            Presented::Previous,
        );
        assert_eq!(g.misses().overflow, m.overflow + 1);
    }

    /// An IPv6 host picks from a /64, so its /64 is the source; an
    /// IPv4-mapped address is the IPv4 host it maps.
    #[test]
    fn an_ipv6_source_is_its_slash_64() {
        let a: std::net::IpAddr = "fd00:1:2:3::10".parse().unwrap();
        let b: std::net::IpAddr = "fd00:1:2:3:ffff:1:2:3".parse().unwrap();
        let c: std::net::IpAddr = "fd00:1:2:4::10".parse().unwrap();
        assert_eq!(source_of(a), "fd00:1:2:3::/64");
        assert_eq!(source_of(a), source_of(b));
        assert_ne!(source_of(a), source_of(c));
        let mapped: std::net::IpAddr = "::ffff:10.20.0.66".parse().unwrap();
        assert_eq!(source_of(mapped), "10.20.0.66");
        assert_eq!(source_of(v4(0, 66)), "10.20.0.66");
    }

    /// The method is caller text: hyper admits any token as an extension
    /// method, hundreds of kilobytes long, so a tally keyed on the raw
    /// method held up to MAX_TALLY_KEYS of them — bounded in count, not in
    /// bytes — and printed each on its WARN line. A standard method is
    /// kept by name; every other is `(other)` (review of car 1, a159e1ee:
    /// due before any service is set to `report`; backlog 2710c8fc).
    #[tokio::test]
    async fn the_tally_keys_a_standard_method_by_name_and_folds_every_other() {
        let g = gate(Mode::Report, slots());
        let long = "X".repeat(64 * 1024);
        for method in ["PATCH", "DELETE", "PROPFIND", "get", long.as_str()] {
            let (status, _) = call(&g, method, "/api/things/1", None).await;
            assert_ne!(status, StatusCode::UNAUTHORIZED, "report refuses nothing");
        }
        let methods: Vec<String> = g.misses().rows.into_iter().map(|r| r.key.method).collect();
        assert_eq!(
            methods,
            vec![OTHER_METHOD, "DELETE", "PATCH"],
            "{methods:?}"
        );
        for m in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::CONNECT,
            Method::OPTIONS,
            Method::TRACE,
            Method::PATCH,
        ] {
            assert_eq!(method_key(&m), m.as_str());
        }
    }

    /// A reader of the tally must never see the new mode beside the old
    /// tally's start: `off` read as `report`, with a recording_since from
    /// a boot days ago, is a clean window that was never watched. So the
    /// reading does not change while anyone holds the tally, and a
    /// restart's reset lands under the same hold (review fd151a97, D2).
    #[test]
    fn the_reading_never_changes_under_a_held_tally() {
        let days_ago = Utc::now() - chrono::Duration::days(3);
        let g = Arc::new(MachineGate::starting_at(
            "things",
            &[HEALTH],
            Reading::new(Mode::Off, slots()),
            days_ago,
        ));
        let held = g.tally.lock().unwrap();
        let observer = {
            let g = Arc::clone(&g);
            std::thread::spawn(move || g.observe(Reading::new(Mode::Report, slots())))
        };
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            g.reading().mode,
            Mode::Off,
            "the mode changed while the tally still began {days_ago}"
        );
        drop(held);
        observer.join().unwrap();
        let m = g.misses();
        assert_eq!(m.mode, Mode::Report);
        assert!(m.recording_since > Utc::now() - chrono::Duration::minutes(1));
    }

    #[tokio::test]
    async fn mounted_files_read_the_mode_and_the_three_slots() {
        let dir =
            std::env::temp_dir().join(format!("boss-core-machine-gate-{}", uuid::Uuid::new_v4()));
        let tokens = dir.join("tokens");
        std::fs::create_dir_all(&tokens).unwrap();
        let files = MountedFiles::new(dir.join("mode"), &tokens);

        // Nothing mounted: off, no slots. This is every pod today.
        let r = files.read_blocking();
        assert_eq!(r, Reading::new(Mode::Off, Slots::default()));
        assert_eq!(files.read().await, r);

        std::fs::write(dir.join("mode"), "enforce\n").unwrap();
        std::fs::write(tokens.join("current"), "cur-value\n").unwrap();
        std::fs::write(tokens.join("next"), "").unwrap();
        let r = files.read().await;
        assert_eq!(r.mode, Mode::Enforce);
        assert_eq!(r.slots.present(), vec![Slot::Current]);
        assert_eq!(files.read_blocking(), r);

        let g = gate(Mode::Off, Slots::default());
        g.observe(r);
        assert_eq!(g.reading().mode, Mode::Enforce);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn an_unreadable_or_oversized_mode_file_reads_as_report_and_says_so() {
        // Review of car 1 (a159e1ee) and of car 2 slice 1 (S8): a mode
        // file that exists but cannot be read fell to `off` through
        // `.ok()`, logged at INFO — the quiet this gate exists to end —
        // and was read without a bound. Only an ABSENT file is `off`.
        let dir = std::env::temp_dir().join(format!(
            "boss-core-machine-gate-mode-{}",
            uuid::Uuid::new_v4()
        ));
        let tokens = dir.join("tokens");
        std::fs::create_dir_all(&tokens).unwrap();
        std::fs::write(tokens.join("current"), "cur-value\n").unwrap();
        let mode = dir.join("mode");
        let files = MountedFiles::new(&mode, &tokens);

        // A directory where the file should be: opens, cannot be read.
        // (Permission-denied is the same arm, but root reads through
        // it, so the fixture is the shape every uid sees alike.)
        std::fs::create_dir_all(&mode).unwrap();
        let r = files.read_blocking();
        assert_eq!(r.mode, Mode::Report, "{r:?}");
        let e = r.mode_error.clone().unwrap_or_default();
        assert!(e.contains("unreadable") && e.contains("report"), "{e}");
        assert_eq!(files.read().await, r);
        std::fs::remove_dir_all(&mode).unwrap();

        // Not UTF-8: unreadable as text, the same arm.
        std::fs::write(&mode, [0xff, 0xfe, b'e']).unwrap();
        let r = files.read_blocking();
        assert_eq!(r.mode, Mode::Report, "{r:?}");
        assert!(r.mode_error.is_some());

        // Past the bound, even when it trims to a mode word: a mode file
        // holds one word, and a reader never pulls an arbitrary file into
        // memory every few seconds.
        let big = format!("enforce{}", " ".repeat(MAX_MODE_BYTES as usize));
        std::fs::write(&mode, big).unwrap();
        let r = files.read_blocking();
        assert_eq!(r.mode, Mode::Report, "{r:?}");
        let e = r.mode_error.clone().unwrap_or_default();
        assert!(e.contains(&MAX_MODE_BYTES.to_string()), "{e}");

        // At the bound it is still a mode file.
        let fits = format!("enforce{}", " ".repeat(MAX_MODE_BYTES as usize - 7));
        std::fs::write(&mode, fits).unwrap();
        assert_eq!(files.read_blocking().mode, Mode::Enforce);

        // Absent is still `off`, with nothing to say.
        std::fs::remove_file(&mode).unwrap();
        let r = files.read_blocking();
        assert_eq!((r.mode, r.mode_error), (Mode::Off, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Design b08725c2 row D: a second refusing switch reads its word
    /// "the same way" as this gate — the same reader, not a copy. Every
    /// arm of the gate's own reading holds for a switch, and each thing
    /// it says names the switch, not the machine gate.
    #[tokio::test]
    async fn a_mode_switch_reads_its_file_the_way_the_gate_does() {
        let dir = std::env::temp_dir().join(format!(
            "boss-core-mode-switch-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("policy-check");
        let what = "policy check";

        // Absent: off, nothing to say. Every pod that mounts no key.
        let switch = ModeSwitch::new(what, &file, read_mode(&file, what));
        assert_eq!(switch.reading(), (Mode::Off, None));
        assert_eq!(switch.path(), file.as_path());

        std::fs::write(&file, "Report\n").unwrap();
        assert!(switch.observe(switch.read_blocking()));
        assert_eq!(switch.reading(), (Mode::Report, None));
        // The same reading again is no change, and says nothing.
        assert!(!switch.observe(switch.read_blocking()));

        std::fs::write(&file, "enforc").unwrap();
        let (mode, e) = switch.read_blocking();
        assert_eq!(mode, Mode::Report);
        let e = e.unwrap_or_default();
        assert!(e.contains("unknown policy check mode"), "{e}");
        assert!(!e.contains("machine gate"), "{e}");

        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(&file).unwrap();
        let (mode, e) = switch.read_blocking();
        assert_eq!(mode, Mode::Report);
        let e = e.unwrap_or_default();
        assert!(
            e.contains("policy check mode file is unreadable") && e.contains("report"),
            "{e}"
        );
        std::fs::remove_dir_all(&file).unwrap();

        let big = format!("enforce{}", " ".repeat(MAX_MODE_BYTES as usize));
        std::fs::write(&file, big).unwrap();
        assert_eq!(switch.read_blocking().0, Mode::Report);

        std::fs::write(&file, "enforce").unwrap();
        assert!(switch.observe(switch.read_blocking()));
        assert_eq!(switch.mode(), Mode::Enforce);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A switch says when its current mode began: its mount, or the last
    /// reading that moved the MODE. A tally kept beside it (the policy
    /// check's refusals) restarts from that instant, so a count that
    /// spanned a move — `report` to `off` and back, where the `off` part
    /// watched nothing — cannot read as one watched window (enforce
    /// checklist F1 of backlog b8e75382, review 1c2860f4). The same word
    /// again, or a new error text under the same mode, is not a move.
    #[test]
    fn a_mode_switch_says_when_its_mode_began() {
        let t0 = Utc::now();
        let switch = ModeSwitch::new("policy check", "/nonexistent", (Mode::Report, None));
        let (mode, since) = switch.mode_since();
        assert_eq!(mode, Mode::Report);
        assert!(since >= t0 && since <= Utc::now(), "{since}");

        assert!(!switch.observe((Mode::Report, None)));
        assert_eq!(switch.mode_since().1, since, "no change is no move");
        assert!(switch.observe((Mode::Report, Some("unknown word".into()))));
        assert_eq!(
            switch.mode_since().1,
            since,
            "an error text under the same mode is no move"
        );

        for next in [Mode::Off, Mode::Report, Mode::Enforce, Mode::Report] {
            let (_, before) = switch.mode_since();
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert!(switch.observe((next, None)));
            let (mode, after) = switch.mode_since();
            assert_eq!(mode, next);
            assert!(after > before, "a move to {next:?} restarts the clock");
        }
    }

    /// Leaving `enforce` is the rollback road of every refusing switch,
    /// so it is its own fact (said at WARN); entering it, or moving
    /// between the two modes that refuse nothing, is not.
    #[test]
    fn only_a_move_out_of_enforce_leaves_enforce() {
        use Mode::*;
        for (from, to, left) in [
            (Enforce, Report, true),
            (Enforce, Off, true),
            (Enforce, Enforce, false),
            (Report, Enforce, false),
            (Off, Enforce, false),
            (Report, Off, false),
            (Off, Report, false),
        ] {
            assert_eq!(from.leaves_enforce(to), left, "{from:?} -> {to:?}");
        }
    }

    /// Every fact the gate stated so far, by kind.
    fn stated(
        rx: &mut tokio::sync::mpsc::Receiver<crate::event::Event>,
    ) -> Vec<crate::event::Event> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn kinds(events: &[crate::event::Event]) -> Vec<String> {
        events
            .iter()
            .map(|e| e.kind.trim_start_matches("machine_gate.").to_string())
            .collect()
    }

    /// Design 21946380 point 1 and 2: a process start and EVERY mode move
    /// (`off` included, which the tally does not restart on) is a
    /// `recording_began`; a key's FIRST sighting is one fact — never one
    /// per request — named `refused` when the gate refused it and
    /// `would_refuse` when it admitted it; a `previous` is its own fact,
    /// which dirties no window.
    #[test]
    fn a_start_a_mode_move_and_each_new_key_are_one_fact_each() {
        let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        let g = MachineGate::new("things", &[HEALTH], Reading::new(Mode::Report, slots()))
            .with_evidence(evidence);
        let began = stated(&mut rx);
        assert_eq!(kinds(&began), vec!["recording_began"]);
        assert_eq!(began[0].payload["mode"], "report");
        assert_eq!(began[0].payload["service"], "things");
        assert_eq!(
            began[0].payload["since"],
            serde_json::json!(g.misses().recording_since)
        );

        for _ in 0..3 {
            g.record(&from(v4(0, 7), "agent-seeder"), Presented::None);
        }
        g.record(&from(v4(0, 7), "rotating"), Presented::Previous);
        let keyed = stated(&mut rx);
        assert_eq!(
            kinds(&keyed),
            vec!["would_refuse", "previous_presented"],
            "three requests of one key are ONE fact"
        );
        assert_eq!(keyed[0].payload["key"]["peer"], "10.20.0.7");
        assert_eq!(keyed[0].payload["key"]["user"], "agent-seeder");
        assert_eq!(keyed[0].payload["key"]["presented"], "none");
        assert_eq!(keyed[0].payload["mode"], "report");

        g.observe(Reading::new(Mode::Enforce, slots()));
        g.record(&from(v4(0, 8), "guesser"), Presented::Mismatch);
        g.record(&from(v4(0, 7), "agent-seeder"), Presented::None);
        g.observe(Reading::new(Mode::Off, slots()));
        g.observe(Reading::new(Mode::Off, Slots::default()));
        let moved = stated(&mut rx);
        assert_eq!(
            kinds(&moved),
            vec!["recording_began", "refused", "recording_began"],
            "a slot change is no mode move; a key the tally holds is not new"
        );
        assert_eq!(moved[0].payload["mode"], "enforce");
        assert_eq!(moved[2].payload["mode"], "off");

        // Degraded `enforce` (no token) admits: what it would refuse is
        // `would_refuse`, not `refused`.
        let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        let g = MachineGate::new(
            "things",
            &[HEALTH],
            Reading::new(Mode::Enforce, Slots::default()),
        )
        .with_evidence(evidence);
        g.record(&from(v4(0, 7), "x"), Presented::None);
        assert_eq!(
            kinds(&stated(&mut rx)),
            vec!["recording_began", "would_refuse"]
        );
    }

    /// An overflow names no caller, so its first sighting — of the whole
    /// tally, or of one source's share — is a fact of its own, once.
    #[test]
    fn each_overflow_is_one_fact() {
        let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        let g = MachineGate::new("things", &[HEALTH], Reading::new(Mode::Report, slots()))
            .with_evidence(evidence);
        for i in 0..(MAX_KEYS_PER_SOURCE + 3) {
            g.record(&from(v4(0, 66), &format!("spray-{i}")), Presented::None);
        }
        let loopback = std::net::IpAddr::from([127, 0, 0, 1]);
        for i in 0..(MAX_TALLY_KEYS + 3) {
            g.record(&from(loopback, &format!("in-pod-{i}")), Presented::None);
        }
        let facts = stated(&mut rx);
        let overflowed: Vec<&str> = facts
            .iter()
            .filter(|e| e.kind == "machine_gate.tally_overflowed")
            .map(|e| e.payload["scope"].as_str().unwrap())
            .collect();
        assert_eq!(overflowed, vec!["source", "tally"]);
        assert_eq!(
            facts
                .iter()
                .filter(|e| e.kind == "machine_gate.would_refuse")
                .count(),
            MAX_TALLY_KEYS,
            "one fact per key the tally holds, and no more"
        );
        let m = g.misses();
        assert!(
            m.not_clean.iter().any(|w| w.contains("overflow 67"))
                && m.not_clean.iter().any(|w| w.contains("1 source(s)")),
            "{:?}",
            m.not_clean
        );
    }

    /// The probe-reader credential (design b35c22b4) meets the evidence
    /// (design 21946380), decided on the rerail of car 6e022f85: a reader
    /// presentation is its own fact, `machine_gate.reader_presented`,
    /// and NEVER a would-refuse or a refusal — its reads are admitted
    /// and its writes refused in every mode, so `enforce` changes
    /// nothing for it, and it must end no clean window and leave the
    /// live half clean. Keyed as the reader slots are: a refused write
    /// as `(refused write)`. Past its source's share it is still a
    /// reader fact, never a tally overflow.
    #[tokio::test]
    async fn a_probe_reader_is_its_own_fact_and_never_a_would_refuse() {
        let reader = Slots::new(Some("reader-cur".into()), None, None);
        for mode in [Mode::Report, Mode::Enforce] {
            let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
            let g = Arc::new(
                MachineGate::new(
                    "things",
                    &[HEALTH],
                    Reading::new(mode, slots()).with_reader(reader.clone()),
                )
                .with_evidence(evidence),
            );
            let (read, _) = call(&g, "GET", "/api/things/1", Some("reader-cur")).await;
            assert_eq!(read, StatusCode::OK, "{mode:?}: a reader reads");
            let (write, _) = call(&g, "PUT", "/api/things/1", Some("reader-cur")).await;
            assert_eq!(
                write,
                StatusCode::FORBIDDEN,
                "{mode:?}: a reader never writes"
            );
            let facts = stated(&mut rx);
            assert_eq!(
                kinds(&facts),
                vec!["recording_began", "reader_presented", "reader_presented"],
                "{mode:?}"
            );
            assert_eq!(facts[1].payload["key"]["presented"], "reader.current");
            assert_eq!(facts[1].payload["key"]["method"], "GET");
            assert_eq!(facts[2].payload["key"]["method"], REFUSED_WRITE);
            let m = g.misses();
            assert_eq!(m.rows.len(), 2, "{mode:?}: both are tallied");
            assert!(m.not_clean.is_empty(), "{mode:?}: {:?}", m.not_clean);
            let w = crate::gate_evidence::window(
                EvidenceGate::MachineGate,
                &["things"],
                facts[0].timestamp,
                facts[2].timestamp,
                &facts,
            );
            assert!(w.dirty.is_empty(), "{mode:?}: {w:?}");
        }

        // Past its source's share, a reader is still a reader.
        let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        let g = MachineGate::new("things", &[HEALTH], Reading::new(Mode::Report, slots()))
            .with_evidence(evidence);
        for i in 0..(MAX_KEYS_PER_SOURCE + 2) {
            g.record(
                &from(v4(0, 66), &format!("probe-{i}")),
                Presented::ReaderCurrent,
            );
        }
        let facts = stated(&mut rx);
        assert!(
            !facts
                .iter()
                .any(|e| e.kind == "machine_gate.tally_overflowed"),
            "a reader past its share is no tally overflow"
        );
        assert!(
            facts.iter().any(
                |e| e.kind == "machine_gate.reader_presented" && e.payload["scope"] == "source"
            ),
            "it is a reader fact, scoped to its source"
        );
        let m = g.misses();
        assert_eq!(m.source_overflow.len(), 1);
        assert!(m.not_clean.is_empty(), "{:?}", m.not_clean);
    }

    /// Review e4417d48, B2: a `refused` fact the log did not take, then
    /// the gate lowered from `enforce` to `report` — a tally restart that
    /// leaves the log's watch unbroken. The loss is not on the log, so
    /// the restart must not forgive it: the live half reads NOT clean
    /// until the loss itself is stated on the log.
    #[test]
    fn lowering_the_gate_does_not_forgive_a_lost_fact() {
        let (evidence, mut rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        let g = MachineGate::new("things", &[HEALTH], Reading::new(Mode::Enforce, slots()))
            .with_evidence(evidence.clone());
        // The queue to the log is full (the database is down and the
        // recorder is behind), so the `refused` fact cannot be taken.
        for _ in 1..crate::gate_evidence::QUEUE_DEPTH {
            evidence.emit(Fact::RecordingBegan, serde_json::json!({}));
        }
        g.record(&from(v4(0, 8), "guesser"), Presented::Mismatch);
        assert!(!g.misses().not_clean.is_empty());
        // The recorder catches up; then the gate is lowered — a tally
        // restart whose own fact the log takes.
        while rx.try_recv().is_ok() {}
        g.observe(Reading::new(Mode::Report, slots()));
        let m = g.misses();
        assert!(
            m.rows.is_empty(),
            "lowered from enforce, the tally began again"
        );
        assert!(
            m.not_clean.iter().any(|w| w.contains("not on the log")),
            "{:?}",
            m.not_clean
        );
    }

    /// The live half (design 21946380 point 3): `not_clean` is empty
    /// only for a watching tally holding nothing `enforce` refuses —
    /// a `previous` row is not a reason — and a fact the log did not
    /// take reads the tally NOT clean, through every restart, until a
    /// `facts_lost` on the log states it.
    #[test]
    fn a_fact_the_log_did_not_take_reads_the_tally_not_clean() {
        let g = gate(Mode::Report, slots());
        assert!(g.misses().not_clean.is_empty(), "{:?}", g.misses());
        assert!(!g.misses().evidence.recorder);
        g.record(&from(v4(0, 7), "rotating"), Presented::Previous);
        assert!(
            g.misses().not_clean.is_empty(),
            "a previous is admitted in every mode"
        );
        g.record(&from(v4(0, 7), "x"), Presented::None);
        assert!(g.misses().not_clean[0].contains("1 caller shape"));
        assert!(gate(Mode::Off, slots()).misses().not_clean[0].contains("off"));

        let (evidence, rx) = Evidence::channel(EvidenceGate::MachineGate, "things");
        drop(rx);
        let g = MachineGate::new("things", &[HEALTH], Reading::new(Mode::Report, slots()))
            .with_evidence(evidence);
        let m = g.misses();
        assert_eq!(m.evidence.lost, 1, "the process start itself was lost");
        assert!(
            m.not_clean[0].contains("not on the log"),
            "{:?}",
            m.not_clean
        );
        // Lowered from enforce, the tally begins again — and every loss
        // it held stands, with the moves' own lost facts beside it.
        g.observe(Reading::new(Mode::Enforce, slots()));
        g.observe(Reading::new(Mode::Report, slots()));
        let m = g.misses();
        assert_eq!(m.evidence.unstated, 3);
        assert!(m.not_clean[0].contains("3 fact(s)"), "{:?}", m.not_clean);
    }
}
