//! A reader door owned by one probe execution (b35c22b4, d26515c5).
//! Dropping the guard stops its runtime, including active connections,
//! before removing the private socket directory. No credential reaches
//! the child environment or a standing server.
//!
//! THREE RULES A REVIEW BOUGHT (run 0bd6a9c2, 2026-10-06), each held by
//! a test below and none of them a matter of care:
//!
//! * NO CREDENTIAL FILE MEANS NO DOOR, and the probe reads exactly as it
//!   did before this file existed. A file that is PRESENT and unusable
//!   — not private, not root's or ours, empty, not a header value, or
//!   one no gate accepts as a reader — is a refusal that the probe did
//!   not run. Never the other way round: the first cut refused on an
//!   absent file, which nothing in the estate delivers yet, so landing
//!   it would have stopped every proof at every door ([`open`]).
//! * ROOT NEVER NAMES A PATH THE PROBE USER CAN REWRITE. The socket's
//!   directory stays this process's own; the probe user is given the
//!   socket alone, by a chown that does not follow a link, while the
//!   directory is still closed to everyone else ([`hand_socket_to`]).
//! * THE DOOR SERVES NO READ IT CANNOT VOUCH AN IDENTITY FOR. It sends
//!   no `x-boss-user`: the gate names the caller from the credential. So
//!   a value no reader slot holds would be admitted by a reporting gate
//!   as `operator:unidentified` — the narrower world of 61085a9e, with
//!   an absence assertion passing against it. The door asks each port's
//!   accepts route once and serves only where the answer is
//!   `reader.<slot>` ([`Forwarder::vouch`]).

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const CREDENTIAL_ENV: &str = "BOSS_PROBE_READER_CREDENTIAL";
pub(crate) const DOOR_ENV: &str = "BOSS_SOR_DOOR";
pub(crate) const MAX_LIFETIME_SECS: u64 = 60;

/// Where the reader credential is deposited on a host that proves.
/// Nothing in the tree delivers it yet (the delivery chain is its own
/// work under d26515c5), which is why absence is the ordinary case.
#[cfg(not(test))]
pub(crate) const DEFAULT_CREDENTIAL: &str = "/etc/boss/probe-reader.credential";
/// Under test the default names nothing, so a suite run on a host that
/// DOES hold the credential never opens it: a test that wants a door
/// names a fixture file.
#[cfg(test)]
pub(crate) const DEFAULT_CREDENTIAL: &str = "/nonexistent/boss-probe-reader.credential";

/// How long one accepts read may take before the door calls the
/// credential unvouched.
const VOUCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub credential: PathBuf,
    pub base: String,
    pub ports: String,
}

pub(crate) struct Guard {
    directory: tempfile::TempDir,
    socket: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// The door for one probe, or `None` when this host holds no reader
/// credential — in which case the caller runs the probe as it always
/// did. Every OTHER failure is an error the caller reports as "the
/// probe did not run": a credential that is present and cannot be
/// used must never read as a quiet fallback.
pub(crate) fn open(config: &Config, user: Option<&str>) -> Result<Option<Guard>> {
    // Asked of the name itself, without following it: a link whose
    // target is gone is a deposit that broke, not a host without one.
    match std::fs::symlink_metadata(&config.credential) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => bail!(
            "cannot tell whether the reader credential at {} exists ({error}); refusing \
             rather than reading without the door it may be there to open",
            config.credential.display()
        ),
        Ok(_) => {}
    }
    Guard::start(config, user, Duration::from_secs(MAX_LIFETIME_SECS)).map(Some)
}

/// The credential's value as the header the door stamps. `me` is the
/// uid this process creates files as. Every refusal names the path and
/// the reason and never the value.
fn read_credential(path: &Path, me: u32) -> Result<http::HeaderValue> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let at = path.display();
    let mut file = std::fs::File::open(path).with_context(|| {
        format!("the reader credential at {at} is present but cannot be opened")
    })?;
    // Judged on the open file, so what is checked is what is read.
    let metadata = file
        .metadata()
        .with_context(|| format!("the reader credential at {at} cannot be examined"))?;
    if !metadata.is_file() {
        bail!("the reader credential at {at} is not a regular file");
    }
    if metadata.mode() & 0o077 != 0 {
        bail!(
            "the reader credential at {at} is not private (mode {:04o}; it must be readable \
             by its owner alone)",
            metadata.mode() & 0o7777
        );
    }
    if metadata.uid() != 0 && metadata.uid() != me {
        bail!(
            "the reader credential at {at} is owned by uid {}, which is neither root nor this \
             process (uid {me})",
            metadata.uid()
        );
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .with_context(|| format!("the reader credential at {at} cannot be read as text"))?;
    let value = text.trim();
    if value.is_empty() {
        bail!("the reader credential at {at} is empty");
    }
    let mut credential = http::HeaderValue::from_str(value).map_err(|_| {
        anyhow::anyhow!(
            "the reader credential at {at} is malformed: its {} bytes cannot be a header value",
            value.len()
        )
    })?;
    credential.set_sensitive(true);
    Ok(credential)
}

/// Could `uid` rename an entry it does not own inside a directory with
/// this owner and mode? Conservative: any group or other write bit
/// counts as that user's, because resolving its groups would be a
/// second read of the account database for no gain.
fn can_rename_within(parent_uid: u32, parent_mode: u32, uid: u32) -> bool {
    let sticky = parent_mode & 0o1000 != 0;
    parent_uid == uid || (parent_mode & 0o022 != 0 && !sticky)
}

/// Give the probe user the SOCKET and nothing else (review 0bd6a9c2, B2
/// and F6).
///
/// The first cut chowned the directory to the probe user and then
/// chowned `reader.sock` BY PATH with a call that follows links. In
/// between, the directory was the probe user's: one `rename(2)` of a
/// prepared link over the socket made root chown any file on the host
/// to that user — the reader credential, the estate token's slots.
/// A lost race was silent and the hourly recheck retried it for free.
///
/// Narrowing that window would leave it open, so there is no window:
///
/// * the directory stays this process's, so the probe user can create,
///   remove or rename nothing inside it, before or after;
/// * the socket's owner is set with `lchown`, which changes the name it
///   is given and never what a link points at;
/// * all of it happens while the directory is still 0700 — the socket
///   has its final mode and owner before anyone but us can reach it —
///   and the last act opens the directory to traversal ONLY, on the
///   open directory rather than by name;
/// * the directory's parent is refused if the probe user could rename
///   the directory itself away, which would leave the door unreachable
///   and its removal failing quietly.
fn hand_socket_to(directory: &Path, socket: &Path, uid: u32) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = directory
        .parent()
        .context("the reader door's directory has no parent")?;
    let above =
        std::fs::metadata(parent).with_context(|| format!("examining {}", parent.display()))?;
    if can_rename_within(above.uid(), above.mode(), uid) {
        bail!(
            "the reader door's directory would sit in {} (owner uid {}, mode {:04o}), where \
             the probe user (uid {uid}) could rename it away; point TMPDIR at a sticky or \
             root-only directory",
            parent.display(),
            above.uid(),
            above.mode() & 0o7777
        );
    }
    std::os::unix::fs::lchown(socket, Some(uid), None)
        .context("giving the probe user the reader socket")?;
    let held = std::fs::File::open(directory).context("opening the reader door's directory")?;
    // mode-bits-ok: a directory's traversal bit for the probe user, never an executed file
    held.set_permissions(std::fs::Permissions::from_mode(0o711))
        .context("opening the reader door's directory to traversal")?;
    Ok(())
}

/// The numeric id of `user`, asked of `id` as `prove::running_as_root`
/// asks, so no libc binding rides in for one question.
fn uid_of(user: &str) -> Result<u32> {
    let output = std::process::Command::new("id")
        .args(["-u", user])
        .output()?;
    if !output.status.success() {
        bail!("could not resolve the reader probe user");
    }
    Ok(std::str::from_utf8(&output.stdout)?.trim().parse()?)
}

impl Guard {
    /// A door for a credential [`open`] found present. `lifetime` is the
    /// door's own bound, independent of the `timeout` around the probe:
    /// production passes [`MAX_LIFETIME_SECS`], and it is a parameter
    /// only so a test can watch the door close.
    fn start(config: &Config, user: Option<&str>, lifetime: Duration) -> Result<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let directory = tempfile::Builder::new()
            .prefix("boss-probe-reader-")
            .tempdir()
            .context("creating the private reader door directory")?;
        // mode-bits-ok: private socket directory traversal, never an executed file
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        // The uid this process creates files as, read off the directory
        // it just made rather than through a libc binding.
        let me = std::fs::metadata(directory.path())?.uid();
        let credential = read_credential(&config.credential, me)?;
        let forwarder = Arc::new(Forwarder::new(config, credential)?);
        let socket = directory.path().join("reader.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket)
            .context("binding the per-probe reader socket")?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        if let Some(user) = user {
            hand_socket_to(directory.path(), &socket, uid_of(user)?)?;
        }
        listener.set_nonblocking(true)?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, readiness) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
        let worker = std::thread::Builder::new()
            .name("probe-reader".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready.send(Err("reader socket server could not start".into()));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::UnixListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(_) => {
                            let _ = ready.send(Err("reader socket server could not start".into()));
                            return;
                        }
                    };
                    // BEFORE the door serves anything: is this value a
                    // reader's, where the probe's reads will land?
                    let base = forwarder.base_port;
                    if let Err(why) = forwarder.vouched_at(base).await {
                        let _ = ready.send(Err(why));
                        return;
                    }
                    let app =
                        axum::Router::new().fallback(move |request: axum::extract::Request| {
                            let forwarder = forwarder.clone();
                            async move { forwarder.read(request).await }
                        });
                    let _ = ready.send(Ok(()));
                    tokio::select! {
                        _ = stopped => {},
                        _ = tokio::time::sleep(lifetime) => {},
                        _ = axum::serve(listener, app) => {},
                    }
                });
                // Drop the runtime, rather than waiting for untrusted
                // clients to finish a graceful connection shutdown.
            })?;
        let mut guard = Self {
            directory,
            socket,
            stop: Some(stop),
            worker: Some(worker),
        };
        let started = readiness
            .recv()
            .unwrap_or_else(|_| Err("reader socket server could not start".into()));
        if let Err(why) = started {
            guard.stop();
            bail!(
                "the reader credential at {} opens no door: {why}",
                config.credential.display()
            );
        }
        Ok(guard)
    }

    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Forwarder {
    base: reqwest::Url,
    base_port: u16,
    ports: BTreeSet<u16>,
    credential: http::HeaderValue,
    client: reqwest::Client,
    /// What each port's accepts route said of this credential, asked
    /// once per port for the door's life: `Ok` where it named a reader
    /// slot, the reason where it did not.
    vouched: tokio::sync::Mutex<BTreeMap<u16, Result<(), String>>>,
}

impl Forwarder {
    fn new(config: &Config, credential: http::HeaderValue) -> Result<Self> {
        let base = reqwest::Url::parse(&config.base).context("parsing pinned reader base")?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.fragment().is_some()
            || base.query().is_some()
            || base.path() != "/"
        {
            bail!("reader base must be an HTTP origin without userinfo, query or fragment");
        }
        let base_port = base
            .port_or_known_default()
            .context("reader base has no port")?;
        let mut ports = BTreeSet::new();
        ports.insert(base_port);
        for entry in config.ports.split_whitespace() {
            let (name, port) = entry
                .split_once('=')
                .context("malformed pinned reader port table")?;
            if name.is_empty() {
                bail!("reader port entry has no service name");
            }
            let port: u16 = port.parse().context("invalid pinned reader port")?;
            if port == 0 {
                bail!("reader port must be nonzero");
            }
            ports.insert(port);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base,
            base_port,
            ports,
            credential,
            client,
            vouched: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    /// Ask the gate at `port` what it makes of this credential (review
    /// 0bd6a9c2, B3). Only `reader.<slot>` is a yes. `none` is a value
    /// no slot holds — a reader directory the service does not mount
    /// yet, or a rotation that has moved past it — and an ESTATE slot's
    /// name is a reader file filled from the estate token, which would
    /// hand probe text a credential that writes. An answer that cannot
    /// be had (dark, refused, not this route) is a no as well: silence
    /// is not a pass. The reason carries names and numbers, no value.
    async fn vouch(&self, port: u16) -> Result<(), String> {
        let mut url = self.base.clone();
        url.set_port(Some(port))
            .map_err(|_| format!("port {port} cannot be asked what it accepts"))?;
        url.set_path(boss_core::machine_gate::ACCEPTS_PATH);
        let response = self
            .client
            .get(url)
            .header(boss_core::machine_token::HEADER, self.credential.clone())
            .timeout(VOUCH_TIMEOUT)
            .send()
            .await
            .map_err(|error| {
                format!(
                    "the gate at port {port} did not say whether it accepts this credential \
                     (timeout {}, connect {})",
                    error.is_timeout(),
                    error.is_connect()
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "the gate at port {port} answered HTTP {} to the accepts read",
                status.as_u16()
            ));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| format!("the gate at port {port} did not finish its accepts answer"))?;
        let accepts: boss_core::machine_gate::Accepts = serde_json::from_slice(&body)
            .map_err(|_| format!("port {port} answered the accepts read with something else"))?;
        if accepts.matched.starts_with("reader.") {
            return Ok(());
        }
        Err(format!(
            "service `{}` at port {port} (gate mode {}) names this credential `{}`, not a \
             reader slot, so a read stamped with it would be answered as an unidentified \
             caller — a narrower world, silently (61085a9e)",
            accepts.service,
            accepts.mode.name(),
            accepts.matched
        ))
    }

    /// [`Self::vouch`], asked once per port.
    async fn vouched_at(&self, port: u16) -> Result<(), String> {
        let mut vouched = self.vouched.lock().await;
        if let Some(answer) = vouched.get(&port) {
            return answer.clone();
        }
        let answer = self.vouch(port).await;
        vouched.insert(port, answer.clone());
        answer
    }

    fn target(&self, request: &axum::extract::Request) -> Result<reqwest::Url> {
        if !matches!(*request.method(), http::Method::GET | http::Method::HEAD) {
            bail!("reader method refused");
        }
        let headers = request.headers();
        if headers.contains_key(http::header::TRANSFER_ENCODING)
            || headers.contains_key(http::header::UPGRADE)
            || headers
                .get_all(http::header::CONTENT_LENGTH)
                .iter()
                .any(|v| v != "0")
        {
            bail!("reader bodies and upgrades are refused");
        }
        let mut hosts = headers.get_all(http::header::HOST).iter();
        let host = hosts
            .next()
            .context("reader request has no host")?
            .to_str()?;
        if hosts.next().is_some() {
            bail!("reader request has multiple hosts");
        }
        let origin = reqwest::Url::parse(&format!("{}://{host}", self.base.scheme()))?;
        if origin.host() != self.base.host()
            || origin.scheme() != self.base.scheme()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.fragment().is_some()
            || origin.query().is_some()
            || origin.path() != "/"
            || !origin
                .port_or_known_default()
                .is_some_and(|port| self.ports.contains(&port))
        {
            bail!("reader target is outside its pinned origin and ports");
        }
        let uri = request.uri();
        if uri
            .scheme()
            .is_some_and(|scheme| scheme.as_str() != self.base.scheme())
            || uri
                .authority()
                .is_some_and(|authority| authority.as_str() != host)
        {
            bail!("reader absolute target and host disagree");
        }
        let path = uri
            .path_and_query()
            .context("reader request has no path")?
            .as_str();
        if !path.starts_with('/') || path.starts_with("//") || path.contains('#') {
            bail!("reader path is not origin-form");
        }
        Ok(reqwest::Url::parse(&format!(
            "{}{path}",
            origin.as_str().trim_end_matches('/')
        ))?)
    }

    async fn read(&self, request: axum::extract::Request) -> axum::response::Response {
        use axum::response::IntoResponse;
        let target = match self.target(&request) {
            Ok(target) => target,
            Err(_) => {
                return (http::StatusCode::FORBIDDEN, "reader request refused").into_response();
            }
        };
        let method = request.method().clone();
        // HTTP/2 can carry DATA without a Content-Length header. Check
        // the actual body before forwarding, bounded even for a client
        // that never ends its stream.
        if !matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                axum::body::to_bytes(request.into_body(), 0)
            )
            .await,
            Ok(Ok(_))
        ) {
            return (http::StatusCode::FORBIDDEN, "reader body refused").into_response();
        }
        // The system of record is many ports and each mounts its own
        // gate, so the base's yes is not another service's. A port that
        // does not name this credential a reader is answered here, as a
        // failure `curl -f` makes loud, rather than upstream as a 200
        // about a smaller world.
        let port = target.port_or_known_default().unwrap_or(0);
        if let Err(why) = self.vouched_at(port).await {
            tracing::warn!(port, why = %why, "probe reader refused a read it cannot vouch for");
            return (
                http::StatusCode::BAD_GATEWAY,
                format!("reader credential is not accepted there: {why}"),
            )
                .into_response();
        }
        // No caller headers survive. In particular, duplicate or asserted
        // x-boss headers cannot precede the one parent-held credential.
        let response = match self
            .client
            .request(method, target)
            .header(boss_core::machine_token::HEADER, self.credential.clone())
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    timeout = error.is_timeout(),
                    connect = error.is_connect(),
                    "probe reader upstream unavailable"
                );
                return (http::StatusCode::BAD_GATEWAY, "reader upstream unavailable")
                    .into_response();
            }
        };
        let status = response.status();
        let mut headers = http::HeaderMap::new();
        for name in [
            http::header::CONTENT_TYPE,
            http::header::CONTENT_ENCODING,
            http::header::CONTENT_DISPOSITION,
            http::header::ETAG,
            http::header::LAST_MODIFIED,
        ] {
            if let Some(value) = response.headers().get(&name) {
                headers.insert(name, value.clone());
            }
        }
        let mut result =
            axum::response::Response::new(axum::body::Body::from_stream(response.bytes_stream()));
        *result.status_mut() = status;
        *result.headers_mut() = headers;
        result
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
        // TempDir owns removal; keep it alive through the worker join.
        let _ = self.directory.path();
    }
}

/// What a test stands in front of the door: an upstream on loopback,
/// served from a thread of its own so a synchronous test (prove.rs's
/// are) can use it, and a credential file. No value here is, or was
/// ever, an estate credential.
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;
    use boss_core::machine_gate::{Accepts, MachineGate, Mode, Reading, Slots, gated};
    use std::sync::Mutex;

    pub(crate) const VALUE: &str = "fixture-reader-value-never-an-estate-credential";
    pub(crate) const ESTATE: &str = "fixture-estate-value-never-an-estate-credential";
    pub(crate) const BODY: &str = "actual-upstream-body";

    /// Every request a handler saw: method, path and query, headers.
    pub(crate) type Seen = Arc<Mutex<Vec<(String, String, http::HeaderMap)>>>;

    pub(crate) struct Upstream {
        pub base: String,
        pub port: u16,
        pub seen: Seen,
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Upstream {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    pub(crate) fn serve(build: impl FnOnce(Seen) -> axum::Router + Send + 'static) -> Upstream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Seen::default();
        let handed = seen.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let app =
                    build(handed).into_make_service_with_connect_info::<std::net::SocketAddr>();
                tokio::select! {
                    _ = stopped => {},
                    _ = axum::serve(listener, app) => {},
                }
            });
        });
        Upstream {
            base: format!("http://127.0.0.1:{port}"),
            port,
            seen,
            stop: Some(stop),
            worker: Some(worker),
        }
    }

    /// Answers every path 207 with [`BODY`] and records what it saw.
    pub(crate) fn recorder(seen: Seen) -> axum::Router {
        axum::Router::new().fallback(move |request: axum::extract::Request| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push((
                    request.method().to_string(),
                    request.uri().to_string(),
                    request.headers().clone(),
                ));
                (http::StatusCode::MULTI_STATUS, BODY)
            }
        })
    }

    /// The accepts route alone, answering `matched` to whoever asks.
    pub(crate) fn accepts_route(matched: &'static str) -> axum::Router {
        axum::Router::new().route(
            boss_core::machine_gate::ACCEPTS_PATH,
            axum::routing::get(move || async move {
                axum::Json(Accepts {
                    service: "jobs".into(),
                    mode: Mode::Report,
                    matched: matched.into(),
                    degraded: false,
                })
            }),
        )
    }

    /// A recorder that says `matched` on accepts and gates nothing, so a
    /// test reads exactly the headers the DOOR sent.
    pub(crate) fn ungated(matched: &'static str) -> Upstream {
        serve(move |seen| recorder(seen).merge(accepts_route(matched)))
    }

    /// A recorder behind the REAL machine gate, reporting, whose estate
    /// slot holds [`ESTATE`] and whose reader slot holds `reader` — the
    /// gate a service mounts, not a lookalike.
    pub(crate) fn gated_with_reader(reader: Option<&str>) -> Upstream {
        let reader = reader.map(str::to_string);
        serve(move |seen| {
            let reading = Reading::new(Mode::Report, Slots::new(Some(ESTATE.into()), None, None))
                .with_reader(Slots::new(reader, None, None));
            gated(
                recorder(seen),
                Arc::new(MachineGate::new("jobs", &[], reading)),
            )
        })
    }

    /// A credential file holding `text` at `mode`, in `directory`.
    pub(crate) fn credential(directory: &Path, text: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("reader.credential");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    /// `curl` through the door: (exit ok, stdout with the status code
    /// on its last line).
    pub(crate) fn curl(socket: &Path, url: &str, extra: &[&str]) -> (bool, String) {
        let output = std::process::Command::new("curl")
            .args(["-sS", "--max-time", "5", "--unix-socket"])
            .arg(socket)
            .args(extra)
            .args(["-w", "\\n%{http_code}"])
            .arg(url)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{BODY, ESTATE, VALUE};
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::Mutex;

    fn config(credential: PathBuf, base: &str, ports: &str) -> Config {
        Config {
            credential,
            base: base.into(),
            ports: ports.into(),
        }
    }

    /// A door on a 0600 fixture credential holding `value`.
    fn door(value: &str, base: &str, ports: &str) -> (tempfile::TempDir, Result<Option<Guard>>) {
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), value, 0o600);
        let guard = open(&config(credential, base, ports), None);
        (held, guard)
    }

    fn refusal(guard: Result<Option<Guard>>) -> String {
        match guard {
            Ok(Some(_)) => panic!("a door opened where a refusal was owed"),
            Ok(None) => panic!("no door, silently, where a refusal was owed"),
            Err(error) => format!("{error:#}"),
        }
    }

    /// Can this test process give a file to `uid`? The B2 controls mean
    /// something only then. Being uid 0 is not enough: the dev pod's
    /// root holds no CAP_CHOWN (measured 2026-10-06: `lchown` answered
    /// EPERM there), and the gate runs as an ordinary user.
    fn can_chown_to(uid: u32) -> bool {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("probe");
        std::fs::write(&file, "").unwrap();
        // A chown to oneself succeeds for anyone and gives nothing
        // away — and the gate's uid IS `nobody` (65534), so without
        // this the test ran there and failed on its first assertion.
        std::fs::metadata(&file).unwrap().uid() != uid
            && std::os::unix::fs::lchown(&file, Some(uid), None).is_ok()
            && std::fs::metadata(&file).unwrap().uid() == uid
    }

    /// SAID ON THE REAL STDERR, which the test harness does not capture,
    /// so a gate that could not exercise this shows it in its own log —
    /// a skip nobody can read is a pass nobody earned.
    fn skipped_loudly(test: &str, what: &str) {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "SKIPPED, NOT PASSED: probe_reader::tests::{test} — {what}. It needs a root \
             that holds CAP_CHOWN; run the boss-cli suite as one to exercise it."
        );
    }

    /// The owner rule, where an ordinary user can see it (the gate is
    /// one): a private, well-formed file is still refused when it is
    /// neither root's nor the reading process's. Root's own file is
    /// good for any reader, so as root there is nothing to refuse and
    /// the other-owner case is in the CAP_CHOWN test below.
    #[test]
    fn a_credential_that_is_another_users_file_is_refused() {
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), VALUE, 0o600);
        let owner = std::fs::metadata(&credential).unwrap().uid();
        assert!(read_credential(&credential, owner).is_ok());
        if owner == 0 {
            assert!(read_credential(&credential, 1000).is_ok());
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED, NOT PASSED: probe_reader::tests::\
                 a_credential_that_is_another_users_file_is_refused — as root every fixture \
                 file is root's, which the rule accepts. Run the boss-cli suite as an \
                 ordinary user (the gate does) to exercise the refusal."
            );
            return;
        }
        let said = format!("{:#}", read_credential(&credential, owner + 1).unwrap_err());
        assert!(said.contains("neither root nor this process"), "{said}");
        assert!(!said.contains(VALUE), "{said}");
    }

    /// B2, THE HALF EVERY RUN CHECKS — because the test below needs a
    /// root that can chown, which neither the gate nor the dev pod is.
    /// The defect was a CALL: `fs::chown`, which follows a link, on a
    /// path inside a directory the probe user owned. So the non-test
    /// source holds no such call at all, hands over exactly one name
    /// with `lchown`, and never names the directory in a chown.
    #[test]
    fn no_chown_in_the_door_follows_a_link_or_names_the_directory() {
        let prod = include_str!("probe_reader.rs")
            .split("#[cfg(test)]\npub(crate) mod fixture")
            .next()
            .unwrap();
        let code: String = prod
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            code.matches("fs::chown(").count(),
            0,
            "a chown that follows links"
        );
        assert_eq!(code.matches("fchownat").count(), 0);
        assert_eq!(
            code.matches("lchown(").count(),
            1,
            "more than the one hand-over"
        );
        assert!(
            code.contains("lchown(socket, Some(uid), None)"),
            "the one chown names the socket"
        );
        assert!(
            !code.contains("\"chown\""),
            "no chown by a child process either"
        );
    }

    #[test]
    fn the_socket_forwards_a_read_with_only_its_fixed_credential() {
        let upstream = fixture::ungated("reader.current");
        let (_held, guard) = door("fixture-reader-token", &upstream.base, "");
        let guard = guard.unwrap().expect("a present credential opens a door");
        let (ok, out) = fixture::curl(
            guard.socket(),
            &format!("{}/api/records?limit=2", upstream.base),
            &[
                "-H",
                "x-boss-machine-token: attacker-value",
                "-H",
                "x-boss-user: attacker-admin",
            ],
        );
        assert!(ok, "{out}");
        assert_eq!(out, format!("{BODY}\n207"));
        let seen = upstream.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, "/api/records?limit=2");
        assert_eq!(
            seen[0]
                .2
                .get_all("x-boss-machine-token")
                .iter()
                .collect::<Vec<_>>(),
            vec!["fixture-reader-token"]
        );
        assert!(seen[0].2.get("x-boss-user").is_none());
    }

    /// F1, likewise for every run: the door sets three modes and no
    /// other — its directory closed, its socket its owner's, and the
    /// handed-over directory open to traversal alone. The last is seen
    /// by effect only by a root that can chown.
    #[test]
    fn the_door_sets_three_modes_and_no_wider_one() {
        let prod = include_str!("probe_reader.rs")
            .split("#[cfg(test)]\npub(crate) mod fixture")
            .next()
            .unwrap();
        let mut modes: Vec<&str> = prod
            .split("from_mode(")
            .skip(1)
            .map(|rest| rest.split(')').next().unwrap())
            .collect();
        modes.sort_unstable();
        assert_eq!(modes, ["0o600", "0o700", "0o711"]);
    }

    /// F6 by effect, as any user: a hand-over into a parent the probe
    /// user could rename within is refused BEFORE anything is chowned
    /// or opened up.
    #[test]
    fn the_hand_over_refuses_a_parent_the_probe_user_could_rename_within() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let me = std::fs::metadata(parent.path()).unwrap().uid();
        let directory = parent.path().join("boss-probe-reader-fixture");
        std::fs::create_dir(&directory).unwrap();
        // mode-bits-ok: a fixture directory's mode; nothing execs it
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = directory.join("reader.sock");
        std::fs::write(&socket, "").unwrap();
        let closed = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o7777;
        // The parent is the probe user's own.
        let said = format!("{:#}", hand_socket_to(&directory, &socket, me).unwrap_err());
        assert!(said.contains("could rename it away"), "{said}");
        // The parent is anyone's to write, and not sticky.
        // mode-bits-ok: a fixture directory made world-writable like a bad TMPDIR; nothing execs it
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let said = format!(
            "{:#}",
            hand_socket_to(&directory, &socket, me + 1).unwrap_err()
        );
        assert!(said.contains("could rename it away"), "{said}");
        assert_eq!(
            closed(&directory),
            0o700,
            "a refused hand-over opened the door"
        );
        assert_eq!(std::fs::metadata(&socket).unwrap().uid(), me);
    }

    /// B1, the half this file owns: a host with no credential file gets
    /// no door and no error, and nothing is left behind for having
    /// asked.
    #[test]
    fn an_absent_credential_means_no_door_and_no_refusal() {
        let held = tempfile::tempdir().unwrap();
        let absent = held.path().join("never-deposited.credential");
        let guard = open(&config(absent, "http://sor.invalid:7900", ""), None)
            .expect("absence is not an error");
        assert!(guard.is_none(), "a door opened with no credential");
        assert_eq!(
            std::fs::read_dir(held.path()).unwrap().count(),
            0,
            "asking left something behind"
        );
        // The default itself is a name no test host holds.
        assert!(!Path::new(DEFAULT_CREDENTIAL).exists());
    }

    /// B1, the other half: PRESENT and unusable refuses — it never
    /// becomes the quiet no-door of absence — and the refusal names the
    /// path and the reason without the value.
    #[test]
    fn a_present_but_unusable_credential_refuses_and_never_reads_as_absent() {
        // The upstream would accept VALUE, so each refusal below is the
        // file's own and not the accepts read's.
        let upstream = fixture::gated_with_reader(Some(VALUE));
        for (what, text, mode, says) in [
            ("group-readable", VALUE, 0o640, "not private"),
            ("world-readable", VALUE, 0o604, "not private"),
            ("empty", "", 0o600, "is empty"),
            ("only whitespace", " \n\t\n", 0o600, "is empty"),
            (
                "two lines",
                "fixture-first-line\nfixture-second-line",
                0o600,
                "malformed",
            ),
            ("a control byte", "fixture\u{7}value", 0o600, "malformed"),
        ] {
            let held = tempfile::tempdir().unwrap();
            let credential = fixture::credential(held.path(), text, mode);
            let said = refusal(open(&config(credential.clone(), &upstream.base, ""), None));
            assert!(said.contains(says), "{what}: {said}");
            assert!(
                said.contains(&credential.display().to_string()),
                "{what}: the refusal does not name the file: {said}"
            );
            for word in ["fixture-first-line", "fixture\u{7}value", VALUE] {
                assert!(!said.contains(word), "{what}: the refusal carries a value");
            }
        }
        // Not a file at all, and a deposit whose target is gone.
        let held = tempfile::tempdir().unwrap();
        let directory = held.path().join("reader.credential");
        std::fs::create_dir(&directory).unwrap();
        let said = refusal(open(&config(directory, &upstream.base, ""), None));
        assert!(said.contains("reader credential"), "{said}");
        let dangling = held.path().join("dangling.credential");
        std::os::unix::fs::symlink(held.path().join("gone"), &dangling).unwrap();
        let said = refusal(open(&config(dangling, &upstream.base, ""), None));
        assert!(said.contains("present but cannot be opened"), "{said}");
        assert!(upstream.seen.lock().unwrap().is_empty());
    }

    /// B3 against the REAL gate, reporting as every service's is today.
    /// Accepted: the handler is told the probe reader — by the gate,
    /// from the credential, not by anything the door or the probe
    /// asserted. Not accepted, in each way that can happen: no door.
    #[test]
    fn the_door_opens_only_for_a_credential_the_gate_names_a_reader() {
        let upstream = fixture::gated_with_reader(Some(VALUE));
        let (_held, guard) = door(VALUE, &upstream.base, "");
        let guard = guard.unwrap().expect("an accepted reader opens the door");
        let (ok, out) = fixture::curl(
            guard.socket(),
            &format!("{}/api/jobs?limit=1", upstream.base),
            &["-H", "x-boss-user: attacker-admin"],
        );
        assert!(ok && out.ends_with("207"), "{out}");
        {
            let seen = upstream.seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            let user: serde_json::Value =
                serde_json::from_slice(seen[0].2["x-boss-user"].as_bytes()).unwrap();
            assert_eq!(user["id"], boss_core::roles::PROBE_READER_ACTOR);
            assert!(seen[0].2.get("x-boss-machine-token").is_none());
        }
        drop(guard);

        // A value no reader slot holds: the gate would ADMIT its reads
        // in report mode, unidentified. The door must not exist.
        let (_held, guard) = door("fixture-value-no-slot-holds", &upstream.base, "");
        let said = refusal(guard);
        assert!(said.contains("`none`"), "{said}");
        assert!(said.contains("61085a9e"), "{said}");
        assert!(!said.contains("fixture-value-no-slot-holds"), "{said}");

        // A service that mounts no reader slots at all.
        let unmounted = fixture::gated_with_reader(None);
        let (_held, guard) = door(VALUE, &unmounted.base, "");
        assert!(refusal(guard).contains("`none`"));

        // A reader file filled from the ESTATE token (review 177b4976,
        // N2): a credential that writes is not a reader's.
        let (_held, guard) = door(ESTATE, &upstream.base, "");
        let said = refusal(guard);
        assert!(said.contains("`current`"), "{said}");
        assert!(!said.contains(ESTATE), "{said}");

        // Not this route (the gateway answers 404 here), and dark.
        let no_route = fixture::serve(fixture::recorder);
        let (_held, guard) = door(VALUE, &no_route.base, "");
        assert!(refusal(guard).contains("something else"));
        let dark = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let (_held, guard) = door(VALUE, &dark, "");
        assert!(refusal(guard).contains("did not say whether it accepts"));

        // Only the one accepted read ever reached a handler.
        assert_eq!(upstream.seen.lock().unwrap().len(), 1);
        assert!(unmounted.seen.lock().unwrap().is_empty());
    }

    /// B3 one port over: the base's gate naming the reader says nothing
    /// about a service whose gate does not. That read fails loudly at
    /// the door and never reaches the service.
    #[test]
    fn a_port_whose_gate_does_not_name_the_reader_is_never_read() {
        let base = fixture::gated_with_reader(Some(VALUE));
        let other = fixture::gated_with_reader(None);
        let ports = format!("events={}", other.port);
        let (_held, guard) = door(VALUE, &base.base, &ports);
        let guard = guard.unwrap().expect("the base accepts the reader");
        for _ in 0..2 {
            let (ok, out) = fixture::curl(
                guard.socket(),
                &format!("{}/api/events/tail", other.base),
                &["-f"],
            );
            assert!(!ok, "curl -f passed a read the door cannot vouch for");
            assert!(out.ends_with("502"), "{out}");
        }
        assert!(other.seen.lock().unwrap().is_empty());
        let (ok, _) = fixture::curl(guard.socket(), &format!("{}/api/jobs", base.base), &["-f"]);
        assert!(ok, "the vouched port still reads");
    }

    /// F1: the modes are the protection for a door nobody was handed —
    /// the socket its owner's alone, the directory closed.
    #[test]
    fn the_door_is_private_to_its_owner_while_it_stands() {
        let upstream = fixture::ungated("reader.current");
        let (_held, guard) = door(VALUE, &upstream.base, "");
        let guard = guard.unwrap().unwrap();
        let socket = std::fs::symlink_metadata(guard.socket()).unwrap();
        let directory = std::fs::symlink_metadata(guard.socket().parent().unwrap()).unwrap();
        assert_eq!(socket.mode() & 0o7777, 0o600, "socket mode");
        assert_eq!(directory.mode() & 0o7777, 0o700, "directory mode");
        assert_eq!(socket.uid(), directory.uid());
    }

    /// F1: the door closes itself at its own bound, whatever became of
    /// the `timeout` around the probe — here a quarter second.
    #[test]
    fn the_door_stops_serving_at_its_own_bound() {
        let upstream = fixture::ungated("reader.current");
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), VALUE, 0o600);
        let guard = Guard::start(
            &config(credential, &upstream.base, ""),
            None,
            Duration::from_millis(250),
        )
        .unwrap();
        let url = format!("{}/api/jobs", upstream.base);
        let (ok, out) = fixture::curl(guard.socket(), &url, &[]);
        assert!(ok && out.ends_with("207"), "{out}");
        std::thread::sleep(Duration::from_millis(900));
        let (ok, out) = fixture::curl(guard.socket(), &url, &[]);
        assert!(!ok, "the door outlived its bound: {out}");
        assert_eq!(upstream.seen.lock().unwrap().len(), 1);
        assert_eq!(MAX_LIFETIME_SECS, 60);
    }

    /// F1: an upstream that answers with a redirect is handed back as
    /// that redirect. The door never carries its credential to wherever
    /// a response points.
    #[test]
    fn the_door_follows_no_redirect() {
        let elsewhere = fixture::serve(fixture::recorder);
        let location = format!("{}/landed", elsewhere.base);
        let upstream = fixture::serve(move |seen| {
            fixture::recorder(seen)
                .merge(fixture::accepts_route("reader.current"))
                .route(
                    "/api/moved",
                    axum::routing::get(move || {
                        let location = location.clone();
                        async move {
                            (
                                http::StatusCode::FOUND,
                                [(http::header::LOCATION, location)],
                            )
                        }
                    }),
                )
        });
        let (_held, guard) = door(VALUE, &upstream.base, "");
        let guard = guard.unwrap().unwrap();
        let (_, out) = fixture::curl(guard.socket(), &format!("{}/api/moved", upstream.base), &[]);
        assert!(out.ends_with("302"), "{out}");
        assert!(
            elsewhere.seen.lock().unwrap().is_empty(),
            "the door followed a redirect, credential in hand"
        );
    }

    /// F6, the half any user can check: which parents let the probe
    /// user move the door's directory.
    #[test]
    fn a_parent_the_probe_user_could_rename_within_is_refused() {
        // /tmp: root's, world-writable, sticky.
        assert!(!can_rename_within(0, 0o1777, 1000));
        // Root-only.
        assert!(!can_rename_within(0, 0o700, 1000));
        assert!(!can_rename_within(0, 0o755, 1000));
        // World- or group-writable and not sticky; or the user's own.
        assert!(can_rename_within(0, 0o777, 1000));
        assert!(can_rename_within(0, 0o775, 1000));
        assert!(can_rename_within(1000, 0o700, 1000));
        assert!(can_rename_within(1000, 0o1777, 1000));
    }

    /// B2 AND F6, AS ROOT — the only uid for which a chown is an act.
    ///
    /// WHAT THIS PROVES. With the attacker's link ALREADY standing where
    /// the socket should be — the state the race tried to reach, handed
    /// over rather than raced for — the hand-over changes the owner of
    /// nothing outside the directory; the directory is still root's and
    /// closed to writing; and the probe user, acting as itself, cannot
    /// plant the link, remove the socket, or rename the directory away,
    /// yet can still read through the door.
    ///
    /// WHAT IT CANNOT. A unit test does not schedule two processes
    /// against each other, so it does not show a race lost or won; it
    /// shows there is no step whose outcome the timing could change,
    /// which is a claim about the code's shape. It says nothing of a
    /// kernel whose `lchown` follows links, of a parent directory that
    /// is not the sticky one this runs in (the pure rule above covers
    /// that), or of a second process already holding the probe uid's
    /// rights by some other road.
    #[test]
    fn root_hands_over_the_socket_and_nothing_a_planted_link_points_at() {
        const NAME: &str = "root_hands_over_the_socket_and_nothing_a_planted_link_points_at";
        let Ok(uid) = uid_of("nobody") else {
            skipped_loudly(
                NAME,
                "this host has no `nobody` to stand in as the probe user",
            );
            return;
        };
        if !can_chown_to(uid) {
            skipped_loudly(
                NAME,
                "this process cannot chown, so there is no owner to watch",
            );
            return;
        }
        let owner = |path: &Path| std::fs::symlink_metadata(path).unwrap().uid();

        // The planted link, with the function alone.
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("stands-in-for-the-estate-token");
        std::fs::write(&victim, "fixture").unwrap();
        let directory = tempfile::Builder::new()
            .prefix("boss-probe-reader-")
            .tempdir()
            .unwrap();
        let socket = directory.path().join("reader.sock");
        std::os::unix::fs::symlink(&victim, &socket).unwrap();
        hand_socket_to(directory.path(), &socket, uid).unwrap();
        assert_eq!(owner(&victim), 0, "root chowned what the link pointed at");
        assert_eq!(owner(outside.path()), 0);
        assert_eq!(owner(&socket), uid, "the name itself was handed over");
        assert_eq!(
            owner(directory.path()),
            0,
            "the directory left root's hands"
        );
        let mode = std::fs::metadata(directory.path()).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o711, "the directory is open to more than traversal");

        // The whole door, and the probe user acting as itself.
        let upstream = fixture::ungated("reader.current");
        let held = tempfile::tempdir().unwrap();
        let credential = fixture::credential(held.path(), VALUE, 0o600);
        let guard = open(&config(credential, &upstream.base, ""), Some("nobody"))
            .unwrap()
            .unwrap();
        let socket = guard.socket().to_path_buf();
        let directory = socket.parent().unwrap().to_path_buf();
        assert_eq!(owner(&directory), 0);
        assert_eq!(owner(&socket), uid);
        assert_eq!(
            std::fs::symlink_metadata(&socket).unwrap().mode() & 0o7777,
            0o600
        );
        let as_probe_user = |script: &str| {
            std::process::Command::new("setpriv")
                .args([
                    "--reuid",
                    "nobody",
                    "--regid",
                    "nogroup",
                    "--clear-groups",
                    "bash",
                    "-c",
                    script,
                ])
                .env("D", &directory)
                .env("S", &socket)
                .env("V", &victim)
                .env("URL", format!("{}/api/jobs", upstream.base))
                .output()
                .unwrap()
        };
        let reads = as_probe_user("curl -fsS --max-time 5 --unix-socket \"$S\" \"$URL\"");
        assert!(
            reads.status.success(),
            "the probe user cannot read through its own door: {reads:?}"
        );
        assert_eq!(String::from_utf8_lossy(&reads.stdout), BODY);
        for attack in [
            "ln -sf \"$V\" \"$S\"",
            "ln -s \"$V\" \"$D/planted\"",
            "rm -f \"$S\" && ! test -e \"$S\"",
            "mv \"$D\" \"$D.away\"",
            "mv \"$S\" \"$D/elsewhere.sock\"",
        ] {
            let done = as_probe_user(attack);
            assert!(!done.status.success(), "the probe user managed: {attack}");
        }
        assert_eq!(owner(&victim), 0);
        // A third account is handed nothing.
        let stranger = std::process::Command::new("setpriv")
            .args(["--reuid", "daemon", "--regid", "daemon", "--clear-groups"])
            .args(["curl", "-fsS", "--max-time", "5", "--unix-socket"])
            .arg(&socket)
            .arg(format!("{}/api/jobs", upstream.base))
            .output()
            .unwrap();
        assert!(
            !stranger.status.success(),
            "another user read through the door"
        );
        // F6: nothing the probe user did keeps the cleanup from working.
        drop(guard);
        assert!(!directory.exists(), "the door's directory leaked");

        // And a credential that is someone else's file is refused.
        let theirs = fixture::credential(held.path(), VALUE, 0o600);
        std::os::unix::fs::chown(&theirs, Some(uid), None).unwrap();
        let said = refusal(open(&config(theirs, &upstream.base, ""), None));
        assert!(said.contains("neither root nor this process"), "{said}");
    }

    #[test]
    fn the_parent_enforces_its_pinned_target_and_read_methods() {
        let config = Config {
            credential: PathBuf::new(),
            base: "http://sor.invalid:7900".into(),
            ports: "events=7150".into(),
        };
        let forwarder =
            Forwarder::new(&config, http::HeaderValue::from_static("fixture-reader")).unwrap();
        for (method, host, path, allowed) in [
            ("GET", "sor.invalid:7900", "/api/jobs?limit=2", true),
            ("HEAD", "sor.invalid:7150", "/api/events/tail", true),
            ("POST", "sor.invalid:7900", "/api/jobs", false),
            ("PUT", "sor.invalid:7900", "/api/jobs", false),
            ("PATCH", "sor.invalid:7900", "/api/jobs", false),
            ("DELETE", "sor.invalid:7900", "/api/jobs", false),
            ("GET", "elsewhere.invalid:7900", "/api/jobs", false),
            ("GET", "sor.invalid:9999", "/api/jobs", false),
            ("GET", "attacker@sor.invalid:7900", "/api/jobs", false),
            (
                "GET",
                "sor.invalid:7900",
                "http://elsewhere.invalid:7900/api/jobs",
                false,
            ),
            (
                "GET",
                "sor.invalid:7900",
                "https://sor.invalid:7900/api/jobs",
                false,
            ),
            (
                "GET",
                "sor.invalid:7900",
                "//elsewhere.invalid/api/jobs",
                false,
            ),
        ] {
            let request = http::Request::builder()
                .method(method)
                .uri(path)
                .header(http::header::HOST, host)
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(
                forwarder.target(&request).is_ok(),
                allowed,
                "{method} {host} {path}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_undeclared_request_body_never_reaches_the_upstream() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = hits.clone();
        let app = axum::Router::new().fallback(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                "upstream"
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = Config {
            credential: PathBuf::new(),
            base: format!("http://{address}"),
            ports: String::new(),
        };
        let forwarder =
            Forwarder::new(&config, http::HeaderValue::from_static("fixture-reader")).unwrap();
        let request = http::Request::builder()
            .uri("/api/jobs")
            .header(http::header::HOST, address.to_string())
            .body(axum::body::Body::from("undeclared-body"))
            .unwrap();
        let response = forwarder.read(request).await;
        assert_eq!(response.status(), http::StatusCode::FORBIDDEN);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_door_cancels_an_observed_in_flight_read() {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let entered = Arc::new(Mutex::new(Some(entered)));
        let app = axum::Router::new()
            .fallback(move || {
                let entered = entered.clone();
                async move {
                    if let Some(entered) = entered.lock().unwrap().take() {
                        let _ = entered.send(());
                    }
                    std::future::pending::<http::StatusCode>().await
                }
            })
            .merge(fixture::accepts_route("reader.current"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let fixture = tempfile::tempdir().unwrap();
        let credential = fixture::credential(fixture.path(), "fixture-reader-token", 0o600);
        let config = Config {
            credential,
            base: base.clone(),
            ports: String::new(),
        };
        let guard = tokio::task::spawn_blocking(move || open(&config, None))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let socket = guard.socket().to_path_buf();
        let mut command = tokio::process::Command::new("curl");
        command
            .args(["-sS", "--max-time", "5", "--unix-socket"])
            .arg(&socket)
            .arg(format!("{base}/api/records"))
            .kill_on_drop(true);
        let reader = tokio::spawn(async move { command.output().await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), observed)
            .await
            .expect("upstream actually observed the read")
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || drop(guard)),
        )
        .await
        .expect("an in-flight request cannot hold its guard open")
        .unwrap();
        assert!(!socket.exists());
        assert!(!socket.parent().unwrap().exists());
        let output = tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .expect("the client sees the cancelled connection")
            .unwrap();
        assert!(
            !output.status.success(),
            "cancelled read is not a passing empty body"
        );
        server.abort();
    }
}
