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
//! EXEMPT: OPTIONS (CORS preflight never carries the header), and the
//! exact health paths a binary declares at mount — GET only, never a
//! prefix — because a probe answered 401 restarts pods and a watchdog
//! answered 401 goes blind (design choice 5).
//!
//! TWO ROUTES on every gated binary, both behind the gate itself:
//! `GET /api/machine-gate/accepts` answers `{mode, matched, degraded}`
//! for the header presented; `GET /api/machine-gate/misses` answers the
//! tally, and ONLY to a caller presenting an accepted token in every
//! mode — a caller's address is a record, and `report` admits everyone
//! else.

use crate::machine_token;
use axum::Json;
use axum::Router;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::Serialize;
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

/// How often the mounted files are re-read. Kubelet refreshes a mounted
/// Secret or ConfigMap in about a minute, so five seconds adds nothing
/// a rotation would notice.
const REREAD: Duration = Duration::from_secs(5);

/// Longest `x-boss-user` id kept in a tally key; the header is caller
/// text and a key is memory.
const MAX_USER_CHARS: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
                    "unknown machine gate mode `{}`: read as `report`, which admits every \
                     request and records the tokenless ones (off | report | enforce)",
                    other.chars().take(32).collect::<String>()
                )),
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Report => "report",
            Mode::Enforce => "enforce",
        }
    }
}

/// Which accepted slot a presented token matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Current,
    Next,
    Previous,
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
            mode_error: None,
        }
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
}

impl MountedFiles {
    pub fn new(mode_file: impl Into<PathBuf>, token_dir: impl Into<PathBuf>) -> Self {
        MountedFiles {
            mode_file: mode_file.into(),
            token_dir: token_dir.into(),
        }
    }

    /// The paths from the environment, or their defaults.
    pub fn from_env() -> Self {
        let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        MountedFiles::new(
            var(MODE_FILE_ENV, DEFAULT_MODE_FILE),
            var(TOKEN_DIR_ENV, DEFAULT_TOKEN_DIR),
        )
    }

    fn reading(
        mode: Result<Option<String>, String>,
        slot: impl Fn(&str) -> Option<String>,
    ) -> Reading {
        let (mode, mode_error) = match mode {
            Ok(text) => Mode::parse(text.as_deref()),
            Err(why) => (
                Mode::Report,
                Some(format!(
                    "machine gate mode file is unreadable ({why}): read as `report`, which \
                     admits every request and records the tokenless ones; only an ABSENT mode \
                     file is `off`"
                )),
            ),
        };
        Reading {
            mode,
            slots: Slots::new(slot("current"), slot("next"), slot("previous")),
            mode_error,
        }
    }

    /// Read once, blocking. For boot, before the server takes a request,
    /// so the first request is judged by the configured mode and not by
    /// a default.
    pub fn read_blocking(&self) -> Reading {
        // Each slot through the one bounded reader every caller uses
        // (machine_token::read_slot, car 2), so the gate and its callers
        // cannot disagree about what a slot file holds.
        Self::reading(read_mode_file(&self.mode_file), |name| {
            machine_token::read_slot(&self.token_dir, name)
        })
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

/// What a tallied request presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Presented {
    /// No `x-boss-machine-token` header at all.
    None,
    /// A header that matches no accepted slot.
    Mismatch,
    /// The `previous` slot — admitted, and counted so a rotation's
    /// revoke waits until nothing sends it.
    Previous,
}

/// One caller shape the tally counts. The peer is the IP alone — a
/// port is ephemeral and would make every connection its own key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct MissKey {
    pub peer: String,
    pub user: String,
    pub method: String,
    pub route: String,
    pub presented: Presented,
}

#[derive(Clone, Debug, Serialize)]
pub struct MissRow {
    #[serde(flatten)]
    pub key: MissKey,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Default)]
struct Tally {
    rows: HashMap<MissKey, (u64, DateTime<Utc>, DateTime<Utc>)>,
    overflow: u64,
}

/// What the tally route answers.
#[derive(Clone, Debug, Serialize)]
pub struct Misses {
    pub service: String,
    pub mode: Mode,
    pub rows: Vec<MissRow>,
    /// Requests whose key arrived after [`MAX_TALLY_KEYS`] were held.
    pub overflow: u64,
}

/// What the accepts route answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Accepts {
    pub service: String,
    pub mode: Mode,
    /// `current | next | previous | none` — a name, never a value.
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
        MachineGate {
            service: service.to_string(),
            health: health.iter().map(|p| p.to_string()).collect(),
            reading: RwLock::new(Arc::new(reading)),
            tally: Mutex::new(Tally::default()),
        }
    }

    pub fn reading(&self) -> Arc<Reading> {
        Arc::clone(&self.reading.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Take a fresh reading, and say so when it differs from the last —
    /// by mode and slot NAMES, never by value.
    pub fn observe(&self, next: Reading) {
        let mut cur = self.reading.write().unwrap_or_else(PoisonError::into_inner);
        if **cur == next {
            return;
        }
        let rotated = cur.slots != next.slots && cur.slots.present() == next.slots.present();
        *cur = Arc::new(next);
        drop(cur);
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
        if let Some(e) = &r.mode_error {
            tracing::error!(service = %self.service, mode = r.mode.name(), %slots, "{what}: {e}");
        } else if r.degraded() {
            tracing::error!(
                service = %self.service,
                mode = r.mode.name(),
                %slots,
                "{what}: DEGRADED — mode enforce with no readable token; admitting every request \
                 as report (design 6805c764 choice 4)"
            );
        } else {
            tracing::info!(
                service = %self.service,
                mode = r.mode.name(),
                %slots,
                health_exempt = ?self.health,
                "{what}"
            );
        }
    }

    fn exempt(&self, req: &Request) -> bool {
        *req.method() == Method::OPTIONS
            || (*req.method() == Method::GET && self.health.iter().any(|p| p == req.uri().path()))
    }

    fn record(&self, req: &Request, presented: Presented) {
        let key = MissKey {
            peer: req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            user: user_id(req.headers()),
            method: req.method().to_string(),
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
        if tally.rows.len() >= MAX_TALLY_KEYS {
            tally.overflow += 1;
            if tally.overflow == 1 {
                tracing::warn!(
                    service = %self.service,
                    "machine gate tally is full ({MAX_TALLY_KEYS} keys); further new keys are \
                     counted as overflow"
                );
            }
            return;
        }
        // Once per new key, so a restart loses the counts but never the
        // fact that this caller missed.
        tracing::warn!(
            service = %self.service,
            peer = %key.peer,
            user = %key.user,
            method = %key.method,
            route = %key.route,
            presented = ?key.presented,
            "machine gate: a request without an accepted current/next token"
        );
        tally.rows.insert(key, (1, now, now));
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
        Misses {
            service: self.service.clone(),
            mode: self.reading().mode,
            rows,
            overflow: tally.overflow,
        }
    }

    /// What the accepts route answers for this presented value.
    pub fn accepts(&self, provided: Option<&str>) -> Accepts {
        let r = self.reading();
        let matched = match r.slots.matched(provided) {
            Some(Slot::Current) => "current",
            Some(Slot::Next) => "next",
            Some(Slot::Previous) => "previous",
            None => "none",
        };
        Accepts {
            service: self.service.clone(),
            mode: r.mode,
            matched: matched.to_string(),
            degraded: r.degraded(),
        }
    }
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(machine_token::HEADER)
        .and_then(|v| v.to_str().ok())
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
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_string))
        .map(|id| id.chars().take(MAX_USER_CHARS).collect())
        .unwrap_or_else(|| "(unparsed)".to_string())
}

/// The middleware. Layered by [`gated`]; never mounted by hand.
pub async fn machine_gate(
    State(gate): State<Arc<MachineGate>>,
    req: Request,
    next: Next,
) -> Response {
    let reading = gate.reading();
    if reading.mode == Mode::Off || gate.exempt(&req) {
        return next.run(req).await;
    }
    let provided = presented_token(req.headers());
    let matched = reading.slots.matched(provided);
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

async fn accepts_route(State(gate): State<Arc<MachineGate>>, headers: HeaderMap) -> Response {
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
/// Reads the mounted files once now, then every few seconds, so a mode
/// or slot change lands without a restart. Call it inside the runtime,
/// as the last thing before `axum::serve`: a route merged after it is
/// not behind it.
pub fn mount(
    router: Router,
    service: &str,
    health: &[&str],
) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    let files = MountedFiles::from_env();
    let gate = Arc::new(MachineGate::new(service, health, files.read_blocking()));
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

    fn app(gate: &Arc<MachineGate>) -> Router {
        let inner = Router::new()
            .route("/api/things/{id}", put(|| async { "written" }))
            .route("/api/things/{id}", get(|| async { "read" }))
            .route(HEALTH, get(|| async { "ok" }).post(|| async { "posted" }));
        gated(inner, Arc::clone(gate))
    }

    async fn call(
        gate: &Arc<MachineGate>,
        method: &str,
        path: &str,
        token: Option<&str>,
    ) -> (StatusCode, String) {
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("x-boss-user", r#"{"id":"agent-seeder"}"#);
        if let Some(t) = token {
            req = req.header(machine_token::HEADER, t);
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

    #[tokio::test]
    async fn the_tally_is_bounded() {
        let g = gate(Mode::Report, slots());
        for i in 0..(MAX_TALLY_KEYS + 5) {
            let req = axum::http::Request::builder()
                .method("GET")
                .uri(format!("/api/things/{i}"))
                .header("x-boss-user", format!(r#"{{"id":"caller-{i}"}}"#))
                .body(Body::empty())
                .unwrap();
            g.record(&req, Presented::None);
        }
        let m = g.misses();
        assert_eq!(m.rows.len(), MAX_TALLY_KEYS);
        assert_eq!(m.overflow, 5);
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
}
