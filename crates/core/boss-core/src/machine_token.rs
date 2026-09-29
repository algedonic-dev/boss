//! The machine door's shared secret (feedback 7fcd78fa, phase 1; design
//! 6805c764, car 2).
//!
//! The service ports read identity from the caller-supplied
//! `x-boss-user` header and trust it verbatim — measured from a laptop
//! with a made-up header, any host that can route to a port may declare
//! itself any role at any tier (backlog 2710c8fc). The gate
//! (`machine_gate.rs`) is how a port refuses such a caller; this module
//! is how a legitimate caller proves it is not one.
//!
//! ONE SOURCE, THE MOUNTED FILE. Callers and the gate read the token
//! from the same place: the `current` file in the directory the
//! `boss-machine-token` Secret is mounted at ([`TOKEN_DIR_ENV`], default
//! [`DEFAULT_TOKEN_DIR`]). Until car 2 of design 6805c764 the callers
//! read an env var, `BOSS_MACHINE_TOKEN`, while car 1's gate read this
//! directory — two sources for one fact (CLAUDE.md §9a), and an env var
//! from a `secretKeyRef` is fixed at process start, so a rotation would
//! have meant restarting every caller in order against the gates, the
//! ordering that takes a system of record dark (design choice 2). The
//! env var is deleted, not kept as a fallback, for the same reason.
//! Callers always SEND `current`; the gate ACCEPTS `current`, `next` and
//! `previous`, which is what lets the broker rotate without a restart.
//!
//! STAMPED PER REQUEST, FROM ONE WATCHED SOURCE (review S1 of car 2
//! slice 1, 2026-09-26). Slice 1 read the file once and baked the value
//! into a client's `default_headers`, so a client built at boot sent the
//! boot-time token for the life of the process: the broker's revoke of
//! `previous` would then have had to wait for a restart of every caller,
//! which is exactly the ordering the file source was chosen to remove.
//! A [`Client`] now stamps [`shared`]'s value on each request it builds;
//! the one [`Source`] per process re-reads the file on its own thread,
//! so a rotation reaches every caller within [`REREAD`] plus kubelet's
//! Secret refresh, and a read on the request path is a lock, never file
//! I/O (review S5). [`attach`] is the old bake-it-in door, kept only for
//! the callers the pin `no_client_bakes_the_machine_token_in` still
//! names; that list only shrinks. `BlockingClient` (feature
//! `blocking`) is the same stamp on reqwest's blocking half, for the
//! seed and publish walks that run on it.
//!
//! DEPLOY-ORDER SAFETY: everything is inert until the Secret is mounted
//! (car 4). No file means no token, and no token means no header is
//! attached — and every gate is in mode `off` until then.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::Duration;

/// Header the token rides in. Lowercase because axum/reqwest header
/// names are; the gateway's edge strip covers it via the `x-boss-`
/// prefix rule, so a browser can never smuggle one through.
pub const HEADER: &str = "x-boss-machine-token";

/// The directory holding the `current`, `next` and `previous` slots —
/// the `boss-machine-token` Secret's mount (design 6805c764, car 4).
/// ONE definition, read by the gate and by every caller.
pub const TOKEN_DIR_ENV: &str = "BOSS_MACHINE_TOKEN_DIR";
pub const DEFAULT_TOKEN_DIR: &str = "/etc/boss/machine-token";

/// The slot a caller sends.
pub const CURRENT: &str = "current";

/// The most a slot file may hold. A token is 32 random bytes,
/// base64url — 43 characters; anything past this is not a token, and a
/// reader must not pull an arbitrary file into memory on every request
/// (review of car 1, a159e1ee).
pub const MAX_SLOT_BYTES: u64 = 4096;

/// How often a [`Source`] re-reads its file. Kubelet refreshes a mounted
/// Secret in about a minute, so five seconds adds nothing a rotation
/// would notice — the same interval the gate re-reads at.
pub const REREAD: Duration = Duration::from_secs(5);

/// The mounted directory, from the environment or its default.
pub fn token_dir() -> PathBuf {
    std::env::var_os(TOKEN_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TOKEN_DIR))
}

/// One slot's value from `dir`: whitespace-trimmed, absent when the file
/// is missing, unreadable, blank, larger than [`MAX_SLOT_BYTES`], not a
/// regular file, or not sendable as a header value. A blank value reads
/// as absent rather than as a token every empty header would match.
/// Blocking; the file is a Secret key on a tmpfs mount.
pub fn read_slot(dir: &Path, slot: &str) -> Option<String> {
    read_slot_checked(dir, slot).ok().flatten()
}

/// [`read_slot`], telling "no token" (`Ok(None)`: nothing there, or a
/// blank file — a deliberate state, every pod until car 4) from "the
/// file could not be read" (`Err`), which a [`Source`] must not mistake
/// for a revocation (review S7).
///
/// Anything that is not a regular file is refused BEFORE it is opened:
/// `open` on a FIFO blocks until a writer appears, and this read runs at
/// boot, so a FIFO at the path hung the process before it served
/// (review S7 and the mode-file review (b), 2026-09-27).
pub fn read_slot_checked(dir: &Path, slot: &str) -> io::Result<Option<String>> {
    let path = dir.join(slot);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut raw = Vec::new();
    std::fs::File::open(&path)?
        .take(MAX_SLOT_BYTES + 1)
        .read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_SLOT_BYTES {
        return Err(io::Error::other(format!(
            "{} is larger than {MAX_SLOT_BYTES} bytes",
            path.display()
        )));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| io::Error::other(format!("{} is not UTF-8", path.display())))?;
    clean(&text).map_err(|why| io::Error::other(format!("{}: {why}", path.display())))
}

/// Trim; blank is no token; a value no request could carry (a control
/// character inside it) is an error rather than a token that fails
/// every send it is stamped on.
fn clean(raw: &str) -> Result<Option<String>, &'static str> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok(None);
    }
    if reqwest::header::HeaderValue::from_str(v).is_err() {
        return Err("not sendable as a header value");
    }
    Ok(Some(v.to_string()))
}

/// Insert the token header into a map that becomes a client's
/// `default_headers` — the value read ONCE, now. Superseded by
/// [`Client`] (review S1): a client built this way sends the boot-time
/// token until its process restarts. Only the callers the pin
/// `no_client_bakes_the_machine_token_in` names may still use it.
pub fn attach(headers: &mut reqwest::header::HeaderMap) {
    attach_value(headers, shared().current());
}

fn attach_value(headers: &mut reqwest::header::HeaderMap, token: Option<String>) {
    if let Some(token) = token
        && let Ok(v) = reqwest::header::HeaderValue::from_str(&token)
    {
        headers.insert(HEADER, v);
    }
}

/// The process's one watched [`Source`] over [`token_dir`]'s `current`
/// slot, started on first use.
///
/// A process-wide value on purpose: the token is a fact about the pod
/// (its mounted Secret), like its environment, not about any one
/// client, and one watcher per process is what "one shared Source"
/// means — every client, and the gateway's per-request stamp, read the
/// same observation, so no two callers in one process can send
/// different tokens after a rotation. The same `OnceLock` shape as
/// `roles::EXECUTIVE_ROLES`.
pub fn shared() -> Arc<Source> {
    static SHARED: OnceLock<Arc<Source>> = OnceLock::new();
    Arc::clone(SHARED.get_or_init(|| Source::watch(token_dir())))
}

/// Stamp `source`'s current value on one request; nothing when it has
/// none. PRIVATE, [`Client::request`] its one caller: public, it
/// stamped a builder from ANY client — `stamp(reqwest::Client::new()
/// .get(u), &shared())` rode reqwest's follow-ten-hops default and
/// passed both pins (review of 39949355, M4, 2026-09-28).
fn stamp(rb: reqwest::RequestBuilder, source: &Source) -> reqwest::RequestBuilder {
    match source.current() {
        Some(token) => rb.header(HEADER, token),
        None => rb,
    }
}

/// A `reqwest::Client` that stamps the machine token on every request it
/// builds, from a watched [`Source`] (review S1). Deliberately NOT
/// `Deref<Target = reqwest::Client>`: a `&Client` coerced to a
/// `&reqwest::Client` would build requests with no token and compile
/// clean, so every way out of this type is a method that stamps.
///
/// IT NEVER FOLLOWS A REDIRECT (review of 6fbc7fc7, 2026-09-28, finding
/// 1). reqwest's default policy follows up to ten redirects and, on a
/// hop to another host, strips only `authorization`, `cookie`,
/// `proxy-authorization` and `www-authenticate`: every other header of
/// the original request rides along, so a `302 Location:` from anything
/// a stamped client reached would hand the estate token to the host it
/// names. So the only way to make one is from a `ClientBuilder`, which
/// [`Client::build`] finishes with `redirect::Policy::none()` whatever
/// the caller set — a built `reqwest::Client` has no way to change its
/// policy afterwards, which is why there is no constructor, and no
/// `From`, that takes one. A 3xx comes back to the caller as a response.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    token: Arc<Source>,
}

impl Client {
    /// Finish `builder` with redirects off and stamp from the process's
    /// [`shared`] source.
    pub fn build(builder: reqwest::ClientBuilder) -> reqwest::Result<Self> {
        Self::build_with_source(builder, shared())
    }

    /// [`Client::build`] with a given source (tests; a caller with its
    /// own mount).
    pub fn build_with_source(
        builder: reqwest::ClientBuilder,
        token: Arc<Source>,
    ) -> reqwest::Result<Self> {
        let http = builder
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Client { http, token })
    }

    pub fn request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> reqwest::RequestBuilder {
        stamp(self.http.request(method, url), &self.token)
    }

    pub fn get(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, url)
    }

    pub fn put(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PUT, url)
    }

    pub fn patch(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PATCH, url)
    }

    pub fn delete(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::DELETE, url)
    }

    pub fn head(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::HEAD, url)
    }
}

/// [`Client`] on reqwest's BLOCKING half, for the walks that run on it —
/// `boss tenant publish` and `export`, `boss estate declare` (design
/// 6805c764 car 2, the CLI slice, 2026-09-29). The same two guarantees
/// and the same shape: the token is stamped per request from a watched
/// [`Source`], so a walk that outlasts a rotation sends the new value,
/// and the only way in is a `ClientBuilder` finished with redirects off.
/// A blocking client must not be built or dropped on an async worker —
/// the walks that use it already run under `spawn_blocking`.
#[cfg(feature = "blocking")]
#[derive(Clone, Debug)]
pub struct BlockingClient {
    http: reqwest::blocking::Client,
    token: Arc<Source>,
}

#[cfg(feature = "blocking")]
impl BlockingClient {
    /// Finish `builder` with redirects off and stamp from the process's
    /// [`shared`] source.
    pub fn build(builder: reqwest::blocking::ClientBuilder) -> reqwest::Result<Self> {
        Self::build_with_source(builder, shared())
    }

    /// [`BlockingClient::build`] with a given source (tests).
    pub fn build_with_source(
        builder: reqwest::blocking::ClientBuilder,
        token: Arc<Source>,
    ) -> reqwest::Result<Self> {
        let http = builder
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(BlockingClient { http, token })
    }

    pub fn request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> reqwest::blocking::RequestBuilder {
        let rb = self.http.request(method, url);
        match self.token.current() {
            Some(token) => rb.header(HEADER, token),
            None => rb,
        }
    }

    pub fn get(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::POST, url)
    }

    pub fn put(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::PUT, url)
    }

    pub fn patch(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::PATCH, url)
    }

    pub fn delete(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::DELETE, url)
    }

    pub fn head(&self, url: impl reqwest::IntoUrl) -> reqwest::blocking::RequestBuilder {
        self.request(reqwest::Method::HEAD, url)
    }
}

/// The `current` slot, re-read every [`REREAD`] on a thread of its own,
/// for a caller that stamps the token on every request: a read of the
/// last value is a lock, never file I/O on the request path, and a
/// rotation reaches it without a restart.
///
/// A read that FAILS keeps the last good value (review S7): a transient
/// error is not a revocation, and dropping to no token would turn every
/// request into a gate miss until the next read. A file that is GONE or
/// blank is a deliberate state and is followed. Every change is logged,
/// never the value.
#[derive(Default)]
pub struct Source {
    value: RwLock<Option<String>>,
    failing: AtomicBool,
}

/// Never prints the value: a `Debug` of a struct holding a Source (the
/// gateway's state) must not put the estate token in a log line.
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source")
            .field("holds_token", &self.current().is_some())
            .finish()
    }
}

impl Source {
    /// A fixed value that is never re-read. For tests, and for a process
    /// that has no mount to watch.
    pub fn fixed(value: Option<String>) -> Self {
        Source {
            value: RwLock::new(value.and_then(|v| clean(&v).ok().flatten())),
            failing: AtomicBool::new(false),
        }
    }

    /// Read `dir`'s `current` now, then every [`REREAD`].
    pub fn watch(dir: PathBuf) -> Arc<Self> {
        Self::watch_every(dir, REREAD)
    }

    /// [`Source::watch`] at a chosen interval. The watcher is an OS
    /// thread, not a runtime task: it needs no runtime to exist when the
    /// first client is built (a CLI's), its blocking read never holds an
    /// executor thread, and it holds the source weakly, so it ends when
    /// the last holder drops it.
    pub fn watch_every(dir: PathBuf, every: Duration) -> Arc<Self> {
        let source = Arc::new(Source::default());
        source.observe(&dir, read_slot_checked(&dir, CURRENT));
        let weak = Arc::downgrade(&source);
        let label = dir.display().to_string();
        let spawned = std::thread::Builder::new()
            .name("machine-token".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(every);
                    let Some(source) = weak.upgrade() else {
                        return;
                    };
                    source.observe(&dir, read_slot_checked(&dir, CURRENT));
                }
            });
        if let Err(e) = spawned {
            tracing::error!(
                dir = %label,
                error = %e,
                "machine token source could not start its watcher: its `current` slot will not be re-read, so a rotation needs a restart"
            );
        }
        source
    }

    fn observe(&self, dir: &Path, read: io::Result<Option<String>>) {
        match read {
            Ok(next) => {
                if self.failing.swap(false, Ordering::Relaxed) {
                    tracing::info!(dir = %dir.display(), "machine token slot readable again");
                }
                let mut value = self.value.write().unwrap_or_else(PoisonError::into_inner);
                if *value != next {
                    let change = match (value.is_some(), next.is_some()) {
                        (false, true) => "appeared",
                        (true, false) => "removed: requests now carry no token",
                        _ => "changed (a rotation)",
                    };
                    tracing::info!(dir = %dir.display(), "machine token {change}");
                    *value = next;
                }
            }
            Err(e) => {
                if !self.failing.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        dir = %dir.display(),
                        error = %e,
                        holds_token = self.current().is_some(),
                        "machine token slot unreadable: keeping the last value read"
                    );
                }
            }
        }
    }

    /// The last value read.
    pub fn current(&self) -> Option<String> {
        self.value
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Does the provided header value match the expected token?
///
/// Byte-wise constant-time over the compared length: the accumulator
/// folds every byte rather than returning at the first mismatch, so
/// response timing does not leak a prefix. The length check short-
/// circuits, which leaks only the token's length — acceptable for a
/// high-entropy random value.
pub fn verify(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let (a, b) = (expected.as_bytes(), provided.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "boss-core-machine-token-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Poll until `f` holds or ten seconds pass — a watcher test waits on
    /// the effect, never a fixed sleep that races the interval (review
    /// S6: the old test slept a real REREAD + 500 ms and could flake).
    fn eventually(f: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        f()
    }

    fn sent(client: &Client) -> Option<String> {
        client
            .get("http://127.0.0.1:9/")
            .build()
            .unwrap()
            .headers()
            .get(HEADER)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[test]
    fn verify_requires_exact_match() {
        assert!(verify("s3cret", Some("s3cret")));
        assert!(!verify("s3cret", Some("s3creT")));
        assert!(!verify("s3cret", Some("s3cre")));
        assert!(!verify("s3cret", Some("")));
        assert!(!verify("s3cret", None));
    }

    #[test]
    fn a_caller_sends_the_current_slot_of_the_mounted_directory() {
        let d = dir();
        // Nothing mounted: no token, and no header. This is every pod
        // until car 4 mounts the Secret — the deploy-order safety.
        assert_eq!(read_slot(&d, CURRENT), None);
        let mut h = reqwest::header::HeaderMap::new();
        attach_value(&mut h, read_slot(&d, CURRENT));
        assert!(!h.contains_key(HEADER));

        std::fs::write(d.join("current"), "cur-value\n").unwrap();
        std::fs::write(d.join("next"), "next-value\n").unwrap();
        assert_eq!(read_slot(&d, CURRENT).as_deref(), Some("cur-value"));
        attach_value(&mut h, read_slot(&d, CURRENT));
        assert_eq!(h.get(HEADER).unwrap(), "cur-value");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_blank_or_oversized_slot_is_no_token() {
        let d = dir();
        std::fs::write(d.join("current"), "  \n").unwrap();
        assert_eq!(read_slot(&d, CURRENT), None);
        std::fs::write(d.join("current"), "x".repeat(MAX_SLOT_BYTES as usize + 1)).unwrap();
        assert_eq!(read_slot(&d, CURRENT), None);
        std::fs::write(d.join("current"), "x".repeat(MAX_SLOT_BYTES as usize)).unwrap();
        assert!(read_slot(&d, CURRENT).is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_slot_that_is_not_a_regular_file_is_refused_without_opening_it() {
        let d = dir();
        // A FIFO: `open` would block until a writer appeared, hanging
        // boot. mkfifo is coreutils, on every host this suite runs on.
        let fifo = d.join("current");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo failed");
        let (tx, rx) = std::sync::mpsc::channel();
        let dd = d.clone();
        std::thread::spawn(move || tx.send(read_slot_checked(&dd, CURRENT).is_err()).unwrap());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "a FIFO slot must be refused as an error, promptly"
        );
        std::fs::remove_file(&fifo).unwrap();
        // A directory where the key should be.
        std::fs::create_dir(&fifo).unwrap();
        assert!(read_slot_checked(&d, CURRENT).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_value_no_header_can_carry_is_an_error_not_a_token() {
        let d = dir();
        std::fs::write(d.join("current"), "abc\u{1}def\n").unwrap();
        assert!(read_slot_checked(&d, CURRENT).is_err());
        assert_eq!(read_slot(&d, CURRENT), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_env_var_is_no_longer_a_source() {
        // Two sources for one fact drift (CLAUDE.md §9a): the token is
        // the mounted file's, and a process whose environment still
        // carries the old variable sends nothing because of it.
        let src = include_str!("machine_token.rs");
        let old = ["BOSS_MACHINE", "_TOKEN\""].concat();
        assert!(
            !src.contains(&old),
            "machine_token.rs reads the env var again"
        );
    }

    #[test]
    fn a_source_follows_its_file_without_a_restart() {
        let d = dir();
        std::fs::write(d.join("current"), "first\n").unwrap();
        let s = Source::watch_every(d.clone(), Duration::from_millis(20));
        assert_eq!(s.current().as_deref(), Some("first"));
        std::fs::write(d.join("current"), "second\n").unwrap();
        assert!(eventually(|| s.current().as_deref() == Some("second")));
        // Removed on purpose (the Secret unmounted): followed.
        std::fs::remove_file(d.join("current")).unwrap();
        assert!(eventually(|| s.current().is_none()));
        assert_eq!(Source::fixed(Some(" \n".into())).current(), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_failed_read_keeps_the_last_good_value() {
        let d = dir();
        std::fs::write(d.join("current"), "good\n").unwrap();
        let s = Source::watch_every(d.clone(), Duration::from_millis(20));
        assert_eq!(s.current().as_deref(), Some("good"));
        // The key replaced by something unreadable — an error, not a
        // revocation. Then a real value again, which is followed.
        std::fs::remove_file(d.join("current")).unwrap();
        std::fs::create_dir(d.join("current")).unwrap();
        assert!(eventually(|| s.failing.load(Ordering::Relaxed)));
        assert_eq!(s.current().as_deref(), Some("good"));
        std::fs::remove_dir(d.join("current")).unwrap();
        std::fs::write(d.join("current"), "rotated\n").unwrap();
        assert!(eventually(|| s.current().as_deref() == Some("rotated")));
        assert!(!s.failing.load(Ordering::Relaxed));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_client_stamps_each_request_with_the_value_read_now() {
        // The rotation the broker performs (design 6805c764, car 3): the
        // value in `current` changes under a client built once, and the
        // client's NEXT request carries the new one — no restart, which
        // is what default_headers could not do (review S1).
        let d = dir();
        let client = Client::build_with_source(
            reqwest::Client::builder(),
            Source::watch_every(d.clone(), Duration::from_millis(20)),
        )
        .unwrap();
        assert_eq!(sent(&client), None, "nothing mounted, no header");
        std::fs::write(d.join("current"), "before\n").unwrap();
        assert!(eventually(|| sent(&client).as_deref() == Some("before")));
        std::fs::write(d.join("current"), "after\n").unwrap();
        assert!(eventually(|| sent(&client).as_deref() == Some("after")));
        // Every verb goes through the stamp.
        for rb in [
            client.post("http://127.0.0.1:9/"),
            client.put("http://127.0.0.1:9/"),
            client.patch("http://127.0.0.1:9/"),
            client.delete("http://127.0.0.1:9/"),
            client.head("http://127.0.0.1:9/"),
            client.request(reqwest::Method::OPTIONS, "http://127.0.0.1:9/"),
        ] {
            assert_eq!(rb.build().unwrap().headers().get(HEADER).unwrap(), "after");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_source_never_prints_its_value() {
        let s = Arc::new(Source::fixed(Some("s3cret-value".into())));
        let client = Client::build_with_source(reqwest::Client::builder(), Arc::clone(&s)).unwrap();
        let shown = format!("{s:?} {client:?}");
        assert!(!shown.contains("s3cret-value"), "{shown}");
    }

    /// One HTTP exchange on `listener`: the request head it read, sent
    /// down `seen`, then `reply` written back verbatim.
    async fn answer_once(
        listener: tokio::net::TcpListener,
        reply: String,
        seen: tokio::sync::mpsc::UnboundedSender<String>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let _ = seen.send(String::from_utf8_lossy(&head).to_lowercase());
        let _ = sock.write_all(reply.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    /// A 302 from a port the client reached, naming ANOTHER host, and
    /// what that other host saw. `127.0.0.2` is a different host string
    /// from `127.0.0.1` (reqwest's cross-host test compares host and
    /// port) and is loopback on every Linux network namespace, so no
    /// resolver is involved.
    async fn redirected(
        send: impl FnOnce(String) -> reqwest::RequestBuilder,
    ) -> (u16, Vec<String>) {
        let other = tokio::net::TcpListener::bind("127.0.0.2:0").await.unwrap();
        let other_at = other.local_addr().unwrap();
        let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(answer_once(
            other,
            "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            other_tx,
        ));
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_at = first.local_addr().unwrap();
        let (first_tx, _first_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(answer_once(
            first,
            format!(
                "HTTP/1.1 302 Found\r\nlocation: http://{other_at}/elsewhere\r\n\
                 content-length: 0\r\nconnection: close\r\n\r\n"
            ),
            first_tx,
        ));
        let status = send(format!("http://{first_at}/api/x"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16();
        // Whatever reached the other host by now is all that will.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut reached = Vec::new();
        while let Ok(h) = other_rx.try_recv() {
            reached.push(h);
        }
        (status, reached)
    }

    #[cfg(feature = "blocking")]
    fn sent_blocking(client: &BlockingClient) -> Option<String> {
        client
            .get("http://127.0.0.1:9/")
            .build()
            .unwrap()
            .headers()
            .get(HEADER)
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn a_blocking_client_stamps_each_request_with_the_value_read_now() {
        // The seed and publish walks run on reqwest's blocking client
        // (boss tenant publish / export, boss estate declare): the same
        // rotation, the same per-request stamp as `Client`.
        let d = dir();
        let client = BlockingClient::build_with_source(
            reqwest::blocking::Client::builder(),
            Source::watch_every(d.clone(), Duration::from_millis(20)),
        )
        .unwrap();
        assert_eq!(sent_blocking(&client), None, "nothing mounted, no header");
        std::fs::write(d.join("current"), "before\n").unwrap();
        assert!(eventually(
            || sent_blocking(&client).as_deref() == Some("before")
        ));
        std::fs::write(d.join("current"), "after\n").unwrap();
        assert!(eventually(
            || sent_blocking(&client).as_deref() == Some("after")
        ));
        for rb in [
            client.post("http://127.0.0.1:9/"),
            client.put("http://127.0.0.1:9/"),
            client.patch("http://127.0.0.1:9/"),
            client.delete("http://127.0.0.1:9/"),
            client.head("http://127.0.0.1:9/"),
            client.request(reqwest::Method::OPTIONS, "http://127.0.0.1:9/"),
        ] {
            assert_eq!(rb.build().unwrap().headers().get(HEADER).unwrap(), "after");
        }
        let shown = format!("{client:?}");
        assert!(!shown.contains("after"), "{shown}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// One HTTP exchange on a std listener, on its own thread: the
    /// request head it read comes back on the channel.
    #[cfg(feature = "blocking")]
    fn answer_once_std(
        listener: std::net::TcpListener,
        reply: String,
    ) -> std::sync::mpsc::Receiver<String> {
        use std::io::Write;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).to_lowercase());
            let _ = sock.write_all(reply.as_bytes());
        });
        rx
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn a_blocking_client_follows_no_redirect_and_no_token_leaves() {
        // The same finding as the async client's (review of 6fbc7fc7,
        // finding 1), for the blocking twin: a 302 naming another host
        // comes back as a response, and the other host sees nothing.
        let serve = |follows: bool| {
            let other = std::net::TcpListener::bind("127.0.0.2:0").unwrap();
            let other_at = other.local_addr().unwrap();
            let reached = answer_once_std(
                other,
                "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
            );
            let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let first_at = first.local_addr().unwrap();
            let _first = answer_once_std(
                first,
                format!(
                    "HTTP/1.1 302 Found\r\nlocation: http://{other_at}/elsewhere\r\n\
                     content-length: 0\r\nconnection: close\r\n\r\n"
                ),
            );
            let url = format!("http://{first_at}/api/x");
            let status = if follows {
                // The control: a plain blocking client carrying the
                // header follows, and the other host sees the token.
                reqwest::blocking::Client::new()
                    .get(url)
                    .header(HEADER, "estate-token-value")
                    .send()
            } else {
                BlockingClient::build_with_source(
                    reqwest::blocking::Client::builder()
                        .redirect(reqwest::redirect::Policy::limited(10)),
                    Arc::new(Source::fixed(Some("estate-token-value".into()))),
                )
                .unwrap()
                .get(url)
                .send()
            }
            .unwrap()
            .status()
            .as_u16();
            (
                status,
                reached.recv_timeout(Duration::from_millis(300)).ok(),
            )
        };
        let (status, reached) = serve(true);
        assert_eq!(status, 200, "the control client followed the 302");
        assert!(
            reached.is_some_and(|h| h.contains("estate-token-value")),
            "the control's fixture saw no leak"
        );
        let (status, reached) = serve(false);
        assert_eq!(status, 302, "the stamped client must hand the 3xx back");
        assert_eq!(
            reached, None,
            "a stamped client followed a redirect to another host"
        );
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_is_not_followed_and_no_token_leaves() {
        // Review of 6fbc7fc7 (2026-09-28), finding 1: reqwest follows a
        // redirect with every header but the four it calls sensitive, so
        // a stamped client that followed a 302 would hand the estate
        // token to whatever host the Location names.
        //
        // The control first: a plain reqwest client carrying the header
        // DOES follow, and the other host DOES see the token — so the
        // fixture below can see a leak when there is one.
        let (status, reached) = redirected(|url| {
            reqwest::Client::new()
                .get(url)
                .header(HEADER, "estate-token-value")
        })
        .await;
        assert_eq!(status, 200, "the control client followed the 302");
        assert!(
            reached.iter().any(|h| h.contains("estate-token-value")),
            "the control's fixture saw no leak: {reached:?}"
        );

        // The stamped client, built from a builder that asked for the
        // default policy explicitly: the 302 comes back as a response
        // and nothing reaches the other host.
        let client = Client::build_with_source(
            reqwest::Client::builder().redirect(reqwest::redirect::Policy::limited(10)),
            Arc::new(Source::fixed(Some("estate-token-value".into()))),
        )
        .unwrap();
        let (status, reached) = redirected(|url| client.get(url)).await;
        assert_eq!(status, 302, "the stamped client must hand the 3xx back");
        assert!(
            reached.is_empty(),
            "a stamped client followed a redirect to another host: {reached:?}"
        );
    }
}
