//! The jobs-API blip guard and the jobs-api helpers.

use super::*;

// ---------------------------------------------------------------------------
// The jobs-API blip guard
//
// The cluster is the system of record, and it rolls. Twice on
// 2026-08-13 a reconcile hit `Connection refused` to the jobs API
// mid-converge and returned rc=1 for the whole verb — right to refuse
// to act blind, needlessly brittle about an outage that lasted
// seconds (the cadence loop's dock probe held the queue-depth rules
// for that tick on the same blip). A bounded retry covers the roll.
//
// Two rules keep it from papering over anything real: a 4xx is an
// ANSWER and is never retried, and every retry journals one line, so
// blips stay measurable instead of invisible.
// ---------------------------------------------------------------------------

/// What a failed jobs-API attempt was. The classifier reads this and
/// nothing else — pure, and pinned by tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The connection never established — refused, DNS, TLS. Proof
    /// that the request did not reach the system of record.
    Connect,
    /// A timeout, or a response that died mid-body: nothing usable
    /// came back, and whether the write happened is UNKNOWN.
    Ambiguous,
    /// The jobs API answered, with this status.
    Http(u16),
    /// The answer arrived and was unusable — an unparseable body.
    Malformed,
    /// A refusal whose reason is the policy service FAILING, not
    /// deciding. Since 45553536 (2026-09-26) the jobs API answers that
    /// as 503 + Retry-After with the body `policy-unreachable`, which
    /// [`http_failure`] leaves as `Http(503)` — retried the same way —
    /// and only the roll wait ([`unless_a_policy_outage`]) names as
    /// this. Before it, the policy client's fail-closed deny was
    /// rendered as a 403 like any other: a blip wearing an answer's
    /// status (backlog 5d4ad086). A 403 carrying the word is still
    /// read as this, for a server built before that change.
    PolicyOutage,
}

/// Does this refusal name the policy service failing rather than a
/// policy decision? THE one predicate for it in boss-cli: the conductor
/// and cadence classifiers reach it through [`http_failure`], and `boss
/// gate --wait`'s `is_transient` reads the rendered message through it
/// directly (CLAUDE.md §9a — two classifiers, one fact).
///
/// Keyed on the REASON the refusal carries, never on the status: a
/// scope refusal is a 403 too and is a real answer. Today every door
/// answers an outage with the one body `policy-unreachable` (a 503;
/// backlog fe9d212c). A server built before 45553536 denied instead,
/// with `policy-unreachable` for a transport failure or `policy service
/// returned <status>` for a non-2xx, and only a 5xx of the second is an
/// outage; a 4xx there is the jobs API asking wrongly, which asking
/// again does not fix.
pub(crate) fn names_a_policy_outage(text: &str) -> bool {
    const RETURNED: &str = "policy service returned ";
    let t = text.to_ascii_lowercase();
    t.contains("policy-unreachable")
        || t.match_indices(RETURNED).any(|(at, _)| {
            let code = &t.as_bytes()[at + RETURNED.len()..];
            code.len() >= 3 && code[0] == b'5' && code[..3].iter().all(u8::is_ascii_digit)
        })
}

/// A non-2xx answer, classified: a 403 naming a policy outage is
/// [`Failure::PolicyOutage`]; every other status is itself.
pub(crate) fn http_failure(status: u16, body: &str) -> Failure {
    if status == 403 && names_a_policy_outage(body) {
        Failure::PolicyOutage
    } else {
        Failure::Http(status)
    }
}

/// Retry, or surface? Two rules, and the second is the one that keeps
/// the retry honest:
///
///   - a 4xx is an ANSWER (a 422 is the SoR saying no, and asking the
///     same question three times does not change it); only transport
///     failures, 5xx (a policy outage's 503 among them), and a 403
///     naming a policy outage ([`Failure::PolicyOutage`], from a server
///     built before 45553536) are blips;
///   - a blip that leaves the write AMBIGUOUS may only be re-sent when
///     the call is idempotent. Re-POSTing an ambiguous create is how
///     one blip becomes two train Jobs. A refused connection is not
///     ambiguous — nothing was received — so anything may go again,
///     which is exactly the production case this exists for.
///
/// Idempotence is judged per CALL, not per method, so the path rides
/// along: a PATCH through one of the two merge doors is re-sent like a
/// PUT ([`a_merge_door_patch`]), and every other PATCH is not.
pub(crate) fn retryable(method: &Method, path: &str, failure: &Failure) -> bool {
    let idempotent = idempotent(method) || a_merge_door_patch(method, path);
    match failure {
        Failure::Connect => true,
        Failure::Ambiguous => idempotent,
        Failure::Http(status) => idempotent && (500..600).contains(status),
        Failure::Malformed => false,
        // Retried like a 5xx and for the same reason: the SoR failed to
        // decide. Idempotent only — the brief's bound, and the ordering
        // of a handler's policy check against its write is not this
        // classifier's to assume.
        Failure::PolicyOutage => idempotent,
    }
}

/// May this call be sent again when whether it landed is unknown? The
/// one set both waits use — the conductor's [`retryable`] and the roll
/// wait's policy-outage read (backlog f13c719e).
fn idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::PUT | Method::DELETE | Method::HEAD
    )
}

/// Is this a PATCH through a metadata merge door —
/// `/api/jobs/{id}/metadata` or `/api/jobs/{id}/steps/{sid}/metadata`?
/// Backlog 5e8b3f26: b15b0f4e moved the conductor's metadata merge off
/// a full job PUT onto the job's merge door, and the method-grained
/// classifier stopped retrying it — a 5xx or a timeout on a merge
/// failed at once where the PUT had been re-sent.
///
/// PATCH is not idempotent in general (RFC 5789), which is why this is
/// a path and not a method, and why [`idempotent`] — the set the
/// operator verbs' roll wait also reads (f13c719e) — does not change.
/// These two doors are idempotent by their contract: the body names
/// the keys it sets and the keys it deletes (a `null`), and the server
/// merges that into the row as it stands, so a repeat lands the same
/// metadata. Not the same log: each send appends one `JOB_UPDATED`
/// (the Pg `merge_job_metadata_at` records one per merge, with no
/// no-op check) — exactly what a re-sent PUT does today, and replay
/// reaches the same row either way. A re-sent step merge on a terminal
/// step is answered 204 without a write.
fn a_merge_door_patch(method: &Method, path: &str) -> bool {
    if *method != Method::PATCH {
        return false;
    }
    let path = path.split('?').next().unwrap_or_default();
    let named = |s: &str| !s.is_empty();
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["", "api", "jobs", job, "metadata"] => named(job),
        ["", "api", "jobs", job, "steps", step, "metadata"] => named(job) && named(step),
        _ => false,
    }
}

/// A reqwest error, classified. Connect / timeout / mid-flight body
/// failures are the blips a rolling SoR produces; a builder or
/// redirect error is a misconfiguration, and retrying one just burns
/// the window three times over.
fn classify_transport(e: &reqwest::Error) -> Failure {
    if e.is_connect() {
        Failure::Connect
    } else if e.is_timeout() || e.is_request() || e.is_body() {
        Failure::Ambiguous
    } else {
        Failure::Malformed
    }
}

/// A jobs-API call that did not succeed: what it was (for the
/// classifier) and the error to surface once the retries run out.
pub(crate) struct ApiFailure {
    pub(crate) kind: Failure,
    pub(crate) cause: anyhow::Error,
}

impl ApiFailure {
    /// A reqwest failure — classified by what reqwest says went wrong.
    pub(crate) fn transport(e: impl Into<anyhow::Error>, context: String) -> Self {
        let cause = e.into();
        ApiFailure {
            kind: cause
                .downcast_ref::<reqwest::Error>()
                .map(classify_transport)
                .unwrap_or(Failure::Malformed),
            cause: cause.context(context),
        }
    }
}

/// The bounded retry: how many attempts in total, and the first wait
/// between them (each further wait doubles).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    pub(crate) base: Duration,
}

/// The jobs-API policy: 3 attempts, 2s then 4s. A pod roll is over
/// inside that budget, and a jobs API still refusing after it is an
/// outage the verb should surface rather than paper over.
pub(crate) const JOBS_API_RETRY: RetryPolicy = RetryPolicy {
    attempts: 3,
    base: Duration::from_secs(2),
};

impl RetryPolicy {
    /// The wait before attempt `n + 1`, doubling from `base`.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        self.base * 2u32.pow(attempt.saturating_sub(1).min(16))
    }

    /// The same decisions with no waiting — the tests' policy, so the
    /// retry semantics get pinned without spending the backoff.
    #[cfg(test)]
    pub(crate) const fn immediate(attempts: u32) -> Self {
        RetryPolicy {
            attempts,
            base: Duration::ZERO,
        }
    }
}

/// The one-line cause of a blip: the INNERMOST error, which is where
/// the fact lives ("Connection refused (os error 61)") — the layers
/// above it just repeat the url the journal line already implies.
///
/// `budget` is policy (`delivery_policy.blip_cause_budget`); the
/// truncation is mechanism.
pub(crate) fn short_cause(err: &anyhow::Error, budget: usize) -> String {
    let innermost = err
        .chain()
        .last()
        .map(|c| c.to_string())
        .unwrap_or_default();
    let line = innermost.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= budget {
        return line.to_string();
    }
    format!("{}…", line.chars().take(budget).collect::<String>())
}

/// Run `op` until it succeeds, its failure turns out to be an answer
/// rather than a blip, or the attempt budget runs out. Every retry
/// journals one line through `journal` — the caller's idiom, so the
/// conductor's blips read `conductor: ` and the cadence loop's read
/// `cadence: `. (`+ Sync` because the cadence loop's spawned verb
/// tasks record their outcome through this door, and a future that
/// crosses `tokio::spawn` must be `Send`.)
pub(crate) async fn retrying<T, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    path: &str,
    cause_budget: usize,
    journal: &(dyn Fn(&str) + Sync),
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, ApiFailure>>,
{
    let mut attempt = 1u32;
    loop {
        let failure = match op().await {
            Ok(v) => return Ok(v),
            Err(f) => f,
        };
        if attempt >= policy.attempts || !retryable(method, path, &failure.kind) {
            return Err(failure.cause);
        }
        journal(&format!(
            "jobs API blip ({attempt}/{}): {}",
            policy.attempts,
            short_cause(&failure.cause, cause_budget)
        ));
        tokio::time::sleep(policy.backoff(attempt)).await;
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// The operator verbs' roll wait
//
// Backlog 034002b3, measured twice on 2026-09-23: the stack deployment
// rolls with strategy Recreate (two RWO claims, three with boss-files),
// so every train that converges takes the jobs API dark for about a
// minute. Every verb that writes through it — `boss gate`, `boss
// design`, `boss job file`, `boss dispatch` — exited 1 at once on `No
// route to host`, and the operator (the 263f6b9 rollout) or the builder
// (the 3669951 rollout, pod 28s old) had to notice and relaunch by
// hand. The conductor's blip guard above is three attempts over six
// seconds, sized for a pod that rolls one replica at a time; it does
// not cover a Recreate minute, and its rules (5xx and ambiguous blips
// retried when idempotent) are the loop's, not an operator's.
//
// This wait is narrower and longer. It retries ONE failure — a connect
// that never established — because that is the one failure that proves
// the request never reached a server: nothing was received, so nothing
// landed, and a POST may go again. Any status is an answer. A timeout
// or a body that died after the request went out may be a packet
// already filed, so it is surfaced on the first attempt under every
// method, GET included — a verb's read and its write are not told apart
// here, and the price of a read surfaced once is a relaunch, the same
// as before. The wait is visible (one line per retry, on stderr) and
// bounded: past the window the verb fails naming how long it waited.
//
// It waits out ONE answer too (backlog f13c719e): on an idempotent
// call, a 403 or 503 whose body names the policy service failing
// closed. That status is the policy client refusing to decide while its
// service rolls, not a decision, and a GET, PUT or DELETE asked again
// lands nothing twice. A POST or PATCH that meets it is still answered.
// ---------------------------------------------------------------------------

/// How long an operator verb waits out a jobs API that refuses its
/// connections, and how the waits between attempts grow.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RollWait {
    /// Past this much elapsed time a refused connect is an outage, not
    /// a roll, and the verb surfaces it.
    pub(crate) window: Duration,
    /// The first wait between attempts; each further wait doubles.
    pub(crate) first: Duration,
    /// No single wait is longer than this, so the attempt after the
    /// API comes back is at most this late.
    pub(crate) cap: Duration,
}

/// Two minutes: a Recreate roll is about one (backlog 034002b3), and a
/// jobs API still refusing after twice that is worth a human's eyes.
pub(crate) const ROLL_WAIT: RollWait = RollWait {
    window: Duration::from_secs(120),
    first: Duration::from_secs(2),
    cap: Duration::from_secs(15),
};

impl RollWait {
    /// The wait after attempt `n` failed: doubling from `first`,
    /// capped at `cap`.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        (self.first * 2u32.pow(attempt.saturating_sub(1).min(16))).min(self.cap)
    }
}

/// Run `op` until it gets past the connect, or the roll outlasts
/// `wait.window`. `what` names the call (`jobs api POST /api/jobs`) in
/// every line `say` prints and in the failure. The ONE definition every
/// operator verb's jobs-API call goes through — via [`send_through_a_roll`]
/// — so the rule "only a refused connect, or a policy outage on an
/// idempotent call, is retried" lives once.
pub(crate) async fn waiting_out_a_roll<T, F, Fut>(
    wait: &RollWait,
    what: &str,
    say: &(dyn Fn(&str) + Sync),
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, ApiFailure>>,
{
    // The runtime's clock, not the wall's: the same instant `sleep`
    // below advances, so a paused-time test can walk the whole window
    // without spending it (a best-effort read against a dark SoR would
    // otherwise cost every gate two real minutes).
    let started = tokio::time::Instant::now();
    let mut attempt = 1u32;
    loop {
        let failure = match op().await {
            Ok(v) => return Ok(v),
            Err(f) => f,
        };
        let refused = match failure.kind {
            Failure::Connect => true,
            // Only ever produced for an idempotent call — the send
            // decides that before it gets here (backlog f13c719e).
            Failure::PolicyOutage => false,
            _ => return Err(failure.cause),
        };
        let elapsed = started.elapsed();
        if elapsed >= wait.window {
            let cause = short_cause(&failure.cause, 120);
            let over = if refused {
                format!(
                    "the jobs API refused every connection for {:.0}s ({attempt} attempts, \
                     waited out for up to {:.0}s in case it was a rollout) — nothing was sent, \
                     so nothing landed and relaunching is safe",
                    elapsed.as_secs_f64(),
                    wait.window.as_secs_f64(),
                )
            } else {
                format!(
                    "the jobs API's policy service was still failing closed after {:.0}s \
                     ({attempt} attempts of an idempotent call, waited out for up to {:.0}s in \
                     case it was a rollout) — look at the policy service, not the jobs API",
                    elapsed.as_secs_f64(),
                    wait.window.as_secs_f64(),
                )
            };
            return Err(failure
                .cause
                .context(format!("{what}: {over}. Last: {cause}")));
        }
        let pause = wait.backoff(attempt).min(wait.window - elapsed);
        let waiting_on = if refused {
            "the jobs API is not answering"
        } else {
            "the jobs API's policy service is failing closed"
        };
        say(&format!(
            "{waiting_on} (a rollout?) — retrying {what} in {:.0}s ({:.0}s of {:.0}s): {}",
            pause.as_secs_f64(),
            elapsed.as_secs_f64(),
            wait.window.as_secs_f64(),
            short_cause(&failure.cause, 120),
        ));
        tokio::time::sleep(pause).await;
        attempt += 1;
    }
}

/// Send the request `build` makes, waiting out a jobs-API roll
/// ([`ROLL_WAIT`]) and saying so on stderr. `build` is called once per
/// attempt because a request with a body cannot be re-sent. Every
/// status comes back to the caller as the answer it is — except, on an
/// idempotent call, a 403 or 503 whose body names a policy outage,
/// which is waited out inside the same window (backlog f13c719e).
pub(crate) async fn send_through_a_roll<B: Into<crate::machine_http::MachineRequest>>(
    what: &str,
    build: impl Fn() -> B,
) -> Result<reqwest::Response> {
    send_through(&ROLL_WAIT, what, &say_on_stderr, build).await
}

/// The roll wait's voice, written WITHOUT `eprintln!` (review 0facb916,
/// N1): `eprintln!` panics when the write fails, so a verb whose
/// terminal had gone died on the retry line of the very write it was
/// waiting to make — for `boss dispatch --launch`, Building's `died`. A
/// line nobody can read is dropped; the write it narrates goes on.
fn say_on_stderr(m: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "boss: {m}");
}

/// [`send_through_a_roll`] with the wait and the voice chosen — the
/// seam the wire tests go through, so a real socket can walk the
/// window in milliseconds.
async fn send_through<B: Into<crate::machine_http::MachineRequest>>(
    wait: &RollWait,
    what: &str,
    say: &(dyn Fn(&str) + Sync),
    build: impl Fn() -> B,
) -> Result<reqwest::Response> {
    waiting_out_a_roll(wait, what, say, || {
        let request = build().into();
        async move {
            let (method, resp) = request
                .send_with_method()
                .await
                .map_err(|cause| ApiFailure::transport(cause, what.to_string()))?;
            unless_a_policy_outage(&method, resp, what).await
        }
    })
    .await
}

/// The answer as it is — unless it is the policy service failing on a
/// call that may be asked again. Backlog f13c719e: after 5d4ad086 the
/// conductor's classifier and `boss gate --wait` rode out the jobs
/// API's fail-closed 403 `policy-unreachable`, but this wait returned
/// it, so `boss rerail --finish` (the chain that broke at ~21:35Z on
/// 2026-09-25), `boss job file` and `boss hold` still died on the first
/// one. Read on a 503 — how the jobs API answers an outage since the
/// server half (45553536) landed on 2026-09-26 — and on a 403, how a
/// server built before it still does. Idempotent calls only,
/// the bound [`retryable`] keeps: a POST or PATCH that met it stays an
/// answer. Any other status is never read here, and a 403/503 that is
/// an answer goes back rebuilt from the bytes read to classify it.
async fn unless_a_policy_outage(
    method: &Method,
    resp: reqwest::Response,
    what: &str,
) -> std::result::Result<reqwest::Response, ApiFailure> {
    let status = resp.status();
    if !idempotent(method) || !matches!(status.as_u16(), 403 | 503) {
        return Ok(resp);
    }
    let version = resp.version();
    let headers = resp.headers().clone();
    let body = resp
        .bytes()
        .await
        .map_err(|e| ApiFailure::transport(e, what.to_string()))?;
    let text = String::from_utf8_lossy(&body);
    if names_a_policy_outage(&text) {
        return Err(ApiFailure {
            kind: Failure::PolicyOutage,
            cause: anyhow!("{status}: {}", text.trim()).context(what.to_string()),
        });
    }
    let mut answer = http::Response::new(body);
    *answer.status_mut() = status;
    *answer.version_mut() = version;
    *answer.headers_mut() = headers;
    Ok(reqwest::Response::from(answer))
}

// ---------------------------------------------------------------------------
// jobs-api helpers
// ---------------------------------------------------------------------------

/// Does this open train hold the single track? Only while it is
/// PRE-MERGE — its `merged` step not yet completed. A merged train's
/// content is already on main: the next consist merges on top of it,
/// and the earlier train's `converged` step is designed to arrive by
/// ANCESTRY (`convergence_verdict` accepts a running commit that is a
/// descendant of its merge), so a later train landing first is what
/// converges it, not what strands it. Holding the track for a merged
/// train deadlocked delivery twice on 2026-09-07 (f3796323): a merged
/// train whose sha bricked its boot sat at `converged` forever, and the
/// fix-forward car could not board because the track was "occupied"
/// by the very train it would have converged.
///
/// Fails closed: a row with no `steps`, or no `merged` step, is
/// pre-merge and holds — unknown is not "clear".
///
/// Twin: `boss_jobs::yard::holds_the_track` reads the same rule off
/// typed steps for the yard board. The typed read model and this JSON
/// cannot share one signature, so each test names its twin
/// (CLAUDE.md §9a).
pub(crate) fn holds_the_track(train: &Value) -> bool {
    // `is_none_or`: no step, or no readable status, holds (fail closed);
    // only a `completed` merge releases.
    find_step(train, "merged", "Merged into main")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_none_or(|s| s != "completed")
}

/// The train holding the track, named for the journal, or None when
/// the track is clear. Only a PRE-MERGE open train occupies it
/// (`holds_the_track`): a second consist assembled while the first is
/// still merging would merge onto a main the first is about to change
/// (a8c6773b); once the first has merged, the next consist merges on
/// top of it and converges it by ancestry. Arrived and cancelled trains
/// are closed, so both clear the track — a red train that stall-cancels
/// never blocks the next one. Names the first holder.
pub(crate) fn track_occupied_by(open_trains: &[Value]) -> Option<String> {
    open_trains.iter().find(|t| holds_the_track(t)).map(|t| {
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("an unnamed train");
        let id = t.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("{title} ({})", &id[..id.len().min(8)])
    })
}

/// The rows of a list read — a bare array, or the envelope's `data`
/// array — or a refusal. THE one rows helper in boss-cli: the
/// conductor, every operator verb, the census, the doctor and the
/// queue all read a listing through here (backlog 7b7e0529).
///
/// There were five, and they disagreed. `gate::rows` — ~40 callers,
/// orient, park, dispatch, cadence and job among them — answered an
/// EMPTY list for anything that was not a list, while this one refused;
/// `crate::sor_client::api` answers `Ok(None)` for a 200 whose body is not JSON (a
/// proxy's login page, an error envelope), so through that helper a
/// dark or wrong door read as an empty yard: the "a wrong target
/// answers instead of erroring" failure CLAUDE.md §Doors names. The
/// census's and the doctor's copies were the same decision written
/// again (§9a), so they call this one now.
///
/// An empty ARRAY is still an honest answer — a registry may hold
/// nothing. Only a body that is not a list is refused, and the refusal
/// quotes what came back, cut short, because a login page is kilobytes.
/// A caller that is deliberately best-effort turns the refusal into its
/// own fallback, AT its call site and saying so — never by this helper
/// guessing zero on its behalf.
pub(crate) fn rows(resp: Option<Value>) -> Result<Vec<Value>> {
    let resp = resp.ok_or_else(|| {
        anyhow!(
            "a list read answered no JSON body (an empty 200, or a page that is not JSON — a \
             proxy's login page answers this way), so its rows cannot be read as zero"
        )
    })?;
    let list = match resp {
        Value::Object(mut o) if o.contains_key("data") => o.remove("data").unwrap_or(Value::Null),
        other => other,
    };
    match list {
        Value::Array(v) => Ok(v),
        other => {
            let seen = other.to_string();
            let cut: String = seen.chars().take(ROWS_REFUSAL_QUOTE).collect();
            let more = if cut.len() < seen.len() { "…" } else { "" };
            bail!(
                "a list read answered no array (neither bare nor under `data`), so its rows \
                 cannot be read as zero; it answered: {cut}{more}"
            )
        }
    }
}

/// The rows of a list read that must be the WHOLE list — [`rows`], and
/// a refusal when the envelope's `total` counts more rows than it
/// carries (backlog 6cf47547).
///
/// For a reader that cannot page — a door that takes no `offset`, where
/// [`list_all_pages`] would only re-read page one — and whose caller
/// treats the rows as the registry: `boss tenant export` rewrites the
/// tenant repo's seed files from them, so one page read as the whole
/// drops the rest from the repo, well-formed and silent. Measured
/// 2026-09-23, no door the export reads pages today (`/api/agents` and
/// `/api/sensors` answer `total` as the length of `data`); this is what
/// makes the day one starts to loud rather than lossy. A bare array, or
/// an envelope with no numeric `total`, carries no count to hold the
/// rows to and reads as [`rows`] does.
pub(crate) fn every_row(resp: Option<Value>) -> Result<Vec<Value>> {
    let total = resp
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_u64);
    let got = rows(resp)?;
    match total {
        Some(total) if (got.len() as u64) < total => bail!(
            "a list read answered {} of {total} rows — one page of a paged list, so reading \
             it as the whole registry would drop the rest",
            got.len()
        ),
        _ => Ok(got),
    }
}

/// How much of a non-list body [`rows`]' refusal quotes — and of a
/// non-JSON answer to a write, `crate::sor_client::success_answer`'s: enough to
/// recognise an error envelope or a login page, not the whole page.
pub(crate) const ROWS_REFUSAL_QUOTE: usize = 200;

/// One page of a paginated `/api/jobs` read. Kept at the historical
/// 100 so a backlog that fits under a page still makes exactly one
/// call: the defect this file fixes is paging PAST a page, not making
/// the page bigger.
pub(crate) const PAGE_LIMIT: usize = 100;

/// One explicit bound for a CENSUS response before JSON materialization
/// and for retained recovery inventory, including a union of censuses.
/// A census only: an ordinary read (the dock, the verdict sweep) is not
/// inventory, and a bound on it is a refusal only a code change
/// recovers (backlog de67e500).
pub(crate) const MAX_INVENTORY_BYTES: usize = 16 * 1024 * 1024;

/// Read a response body whole, refusing it unjudged past `bound` when
/// one is given. `None` is every read but a census: the conductor's
/// dock page of 100 open cars measured over 16MiB at the p90 car on
/// 2026-10-17, and a bounded dock read would stop the conductor until a
/// code change (backlog de67e500).
pub(crate) async fn read_api_body(
    mut response: reqwest::Response,
    method: &Method,
    path: &str,
    bound: Option<usize>,
) -> std::result::Result<Vec<u8>, ApiFailure> {
    let status = response.status();
    let refusal = |limit: usize| ApiFailure {
        kind: Failure::Malformed,
        cause: anyhow!(
            "{method} {path}: HTTP {status} exceeds response byte bound {limit}; body unjudged"
        ),
    };
    if let Some(limit) = bound
        && response.content_length().is_some_and(|n| n > limit as u64)
    {
        return Err(refusal(limit));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        ApiFailure::transport(error, format!("reading {method} {path} response"))
    })? {
        if let Some(limit) = bound
            && chunk.len() > limit.saturating_sub(body.len())
        {
            return Err(refusal(limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Charge serialized bytes without allocating a second JSON document.
/// An over-budget inventory is UNKNOWN, never an accepted prefix.
pub(crate) fn charge_census_json(value: &Value, remaining: &mut usize) -> Result<()> {
    struct Counter<'a>(&'a mut usize);
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > *self.0 {
                return Err(std::io::Error::other(
                    "recovery inventory byte bound exceeded",
                ));
            }
            *self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(remaining), value)
        .context("recovery inventory byte bound; census unjudged")
}

/// The `total` a list response carries — the DB-wide count of rows
/// matching the filter AND the caller's policy scope, authoritative
/// over any single page's length (`http/jobs.rs` builds it beside
/// `data`). A body without it is an error, never zero: zero is what a
/// wrong deployment answers (CLAUDE.md §Doors), and this number decides
/// whether every matching car has been read.
pub(crate) fn list_total(body: &Value) -> Result<usize> {
    let total = body.get("total").and_then(Value::as_u64).ok_or_else(|| {
        // An invalid envelope is not a census. Echoing unrelated fields
        // here made its own refusal PATCH exceed ordinary ingress, so
        // the unresolved loss aged out without a handback (0ce12d49).
        let kind = match body.get("total") {
            None => "missing",
            Some(Value::Null) => "null",
            Some(Value::Bool(_)) => "boolean",
            Some(Value::Number(_)) => "outside the unsigned integer range",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
        };
        anyhow!("list response `total` is {kind}; expected an unsigned integer")
    })?;
    usize::try_from(total).context("list total does not fit a usize")
}

/// The offset to request next, or `None` once the rows already gathered
/// cover `total`. The pure pagination decision behind `list_all_pages`.
///
/// A limit is not a filter (memory: a-limit-is-not-a-filter). The job
/// list is `ORDER BY opened_on DESC`, so a car opened days ago but
/// gated and parked today has an OLD `opened_on` and sorts to the tail.
/// Once more than one page of open cars exist (in-flight + parked +
/// landed-but-unclosed residue), a parked-ready car falls off page one
/// and, read with a bare `limit=`, never boards — silently, and exactly
/// when a backlog builds. Looping on this until it returns `None` makes
/// a read see every matching row.
pub(crate) fn next_offset(total: usize, fetched: usize) -> Option<usize> {
    if fetched >= total {
        None
    } else {
        Some(fetched)
    }
}

/// Every row of a paginated `/api/jobs` list, not just page one.
///
/// `fetch` is handed the offset to request and returns that page's body
/// (with `data` and `total`); this pages on `offset` — via
/// [`next_offset`] over the response's [`list_total`] — until the rows
/// gathered cover `total`. Shared by every whole-open-set read in the
/// conductor: boarding (`candidates`), the branch-sweep guard
/// (`open_car_branches`), the dock's merge preview (`preview_dock`),
/// and the cadence loop's dock-depth probe (`cadence::probe_dock_depth`)
/// — one paginator so none of them can under-read the dock again.
///
/// Terminates: each page advances `offset` by the rows it returned, and
/// a page that returns nothing stops the loop, so a `total` that shrinks
/// mid-read (a car closing between pages) cannot spin it.
pub(crate) async fn list_all_pages<F, Fut>(fetch: F) -> Result<Vec<Value>>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out: Vec<Value> = Vec::new();
    loop {
        let body = fetch(out.len())
            .await?
            .ok_or_else(|| anyhow!("empty response for a list call"))?;
        let total = list_total(&body)?;
        let page = rows(Some(body))?;
        if page.is_empty() {
            break;
        }
        out.extend(page);
        if next_offset(total, out.len()).is_none() {
            break;
        }
    }
    Ok(out)
}

/// A launch decision needs a complete, stable census, not the dock's
/// best-effort view while cars move. SELECT and COUNT can race; an
/// inconsistent response therefore holds the train rather than granting
/// another CREATE (971c9006, review 40b35d72).
pub(crate) async fn list_original_attempts<F, Fut>(fetch: F) -> Result<Vec<Value>>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut out = Vec::new();
    let mut expected_total = None;
    let mut identities = std::collections::HashSet::new();
    let mut remaining_bytes = MAX_INVENTORY_BYTES;
    loop {
        let body = fetch(out.len())
            .await?
            .ok_or_else(|| anyhow!("empty original-attempt list response"))?;
        let total = list_total(&body)?;
        charge_census_json(&body, &mut remaining_bytes)?;
        if expected_total.is_some_and(|expected| total != expected) {
            bail!("original-attempt total changed during pagination");
        }
        expected_total = Some(total);
        let page = rows(Some(body))?;
        let remaining = total
            .checked_sub(out.len())
            .ok_or_else(|| anyhow!("original-attempt census exceeds total"))?;
        if page.len() != remaining.min(PAGE_LIMIT) {
            bail!("original-attempt page is truncated or exceeds its requested bound");
        }
        for row in &page {
            let id = job_id(row)?;
            if id.is_empty() || !identities.insert(id.to_owned()) {
                bail!("original-attempt census has missing or repeated identity");
            }
        }
        out.extend(page);
        if out.len() == total {
            return Ok(out);
        }
    }
}

pub(crate) fn find_step<'a>(job: &'a Value, slug: &str, title: &str) -> Option<&'a Value> {
    // One lookup, defined in core beside the car builders: the parkers
    // read the same review step this file boards (CLAUDE.md 9a).
    boss_jobs::car::find_step(job, slug, title)
}

pub(crate) fn step_done(step: Option<&Value>) -> bool {
    step.and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| s == "completed" || s == "skipped")
}

/// `spec_slug or title` — the label the python conductor logged.
pub(super) fn step_label(step: &Value) -> String {
    step.get("spec_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| step.get("title").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

pub(crate) fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

pub(super) fn job_id(job: &Value) -> Result<&str> {
    job.get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("job without an id"))
}

/// Python truthiness for the metadata fields the conductor reads —
/// absent, null, "", 0 and empty containers are all "not set".
pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// How long a gate-run may stay active before it is presumed dead: the
/// gate Job's own `activeDeadlineSeconds` (10800 = 3h) from
/// `infra/gate-runner/gate-runner.yaml`. Past it Kubernetes has killed
/// the Job. CLAUDE.md §9a — the manifest is the authority; if that
/// deadline moves, move this. A CEILING, not an expectation (a normal
/// gate finishes in ~15-90 min), so it can only settle runs truly gone.
pub(crate) const GATE_DEADLINE_HOURS: i64 = 3;

/// How long a gate-run has been active with no verdict, when that is
/// long enough to call it dead — `None` means leave it alone.
///
/// Pure so the decision is testable without an API: a run is dead when
/// it has NOT reported a verdict AND its newest sign of life
/// ([`ALIVE_STAMPS`]) predates the Job deadline. A run with no
/// `opened_at` yields `None` — absence of a stamp is not evidence of
/// death, and settling on a guess would put a verdict nobody observed
/// into the audit log.
///
/// FROM THE NEWEST STAMP, NOT THE FILING (backlog 95d81e50; b24e29cb
/// item 4). The deadline is the Job's, and a Job starts no earlier than
/// the last stamp its launcher wrote: a queued run launches after its
/// last beat, and a REUSED packet carries the `opened_at` of the run
/// that filed it. Read from `opened_at` alone, a packet filed four hours
/// ago and relaunched thirty minutes ago was settled `lost` under a Job
/// with two and a half hours left to run.
///
/// A RESIDENT PLACE STILL IN LINE IS NOT A GATE (backlog b56bfc29). A
/// durable submission no launch has begun for ([`resident_in_line`])
/// has had no Job, so no Job deadline can have passed — and its client
/// stamps never move, because a resident place carries no client
/// heartbeat. Resident dec70bf9 would have been settled `lost` three
/// hours after its last stamp while it waited for a bay, under a receipt
/// naming a Job that never existed. Its place is the supervisor's to
/// judge; the Job's clock starts once a launch has begun.
pub(crate) fn dead_gate_run_hours(run: &Value, now: DateTime<Utc>) -> Option<i64> {
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    if step_done(verdict_step) || run.get("metadata").is_some_and(resident_in_line) {
        return None;
    }
    let (_, (_, at)) = last_sign_of_life(&metadata_map(run))?;
    let hours = (now - at).num_hours();
    (hours >= GATE_DEADLINE_HOURS).then_some(hours)
}

/// The receipt a `lost` settle by the clock writes. `jobs_seen` is how
/// many gate Jobs the cluster read carried for the packet: with none, no
/// Job deadline is claimed, because nothing shows a Job existed to have
/// one (backlog b56bfc29 — the receipt that would have settled resident
/// dec70bf9 said its Job's deadline had passed when no Job was ever
/// created). Either way it says what was observed and nothing more.
pub(crate) fn clock_lost_receipt(hours: i64, branch: &str, jobs_seen: usize) -> String {
    let observed = if jobs_seen == 0 {
        format!(
            "NO VERDICT WAS RECORDED, AND NO GATE JOB CARRIES THIS RUN. Quiet {hours}h \
             with no verdict, and the cluster's read of gate Jobs held none for this \
             packet — so no Job deadline is claimed: nothing observed shows a Job existed \
             to have one. Whether a launch never reached the cluster, or its Job was \
             removed, is in the conductor's journal for this gate-run."
        )
    } else {
        format!(
            "NO VERDICT WAS RECORDED. Active {hours}h with none, past the gate Job's \
             {GATE_DEADLINE_HOURS}h activeDeadlineSeconds, by which the cluster has ended \
             any Job this run had. What was observed is only that: no verdict reached this \
             packet in that time. Whether the runner left none, or left one in a pod or a \
             log the conductor could not read, is in the conductor's journal for this \
             gate-run and — for a day after the Job ended — in `kubectl logs job/<its gate \
             Job> -c gate`."
        )
    };
    format!(
        "{observed} Settled as LOST by the conductor's reconcile: this run says nothing \
         about {branch}, and an infrastructure death is not a consist failure. Re-gate \
         for a real verdict."
    )
}

/// A resident request still waiting in line: a durable submission with
/// no protected native observation (`waiting_for_native`, the yard's
/// reading) AND no launch begun — no `gate_launch_intent`, no guarded
/// `gate_launch_attempt`. Either key means a send may have reached the
/// cluster and a Job may exist, so the Job deadline is the clock again;
/// neither means nothing was ever sent.
fn resident_in_line(metadata: &Value) -> bool {
    use boss_jobs::gate_reservation as model;
    model::waiting_for_native(metadata)
        && metadata.get(model::INTENT).is_none()
        && metadata.get(model::ATTEMPT).is_none()
}

/// What the cluster's gate Jobs for a run say of the clock's reading:
/// `Ok(hours)` when the run may be settled `lost`, dated from the newest
/// of the clock and the Jobs; `Err(why)` when it must be left alone.
///
/// WHY (backlog d1ea4141). The clock reads the packet's stamps, and those
/// stand in for the Job only while no Job can be seen. Gate-run 98b80bb0
/// waited 173 minutes in the queue and its launch stamped nothing, so its
/// newest stamp was its place in line; the reap settled it `lost` seven
/// minutes into its Job's three hours, and the running Job's verdict had
/// nowhere to land. `activeDeadlineSeconds` runs from the Job, so the
/// cluster's record of the Job is the deadline's own evidence:
///
/// - a Job not yet finished is RUNNING — Kubernetes, not this clock, ends
///   it, and ends it as Failed (`DeadlineExceeded`) for the recorder and
///   the estate observer to read, however long it has run;
/// - a Job created inside the deadline has a verdict or a pod still to
///   be read, which the recorder does before this clock (`settle_gate_runs`);
/// - a Job whose creation does not parse cannot be dated, and an undated
///   Job is no evidence the deadline passed.
///
/// No Job leaves the clock's reading as it was: the orphan window and
/// the estate observer cover a run that never had one.
pub(crate) fn job_deadline(
    clock_hours: i64,
    jobs: &[&GateJob],
    now: DateTime<Utc>,
) -> Result<i64, String> {
    if let Some(j) = jobs.iter().find(|j| j.finished.is_none()) {
        return Err(format!("its gate Job {} is still running", j.name));
    }
    jobs.iter().try_fold(clock_hours, |hours, j| {
        let created = DateTime::parse_from_rfc3339(&j.created)
            .map_err(|_| {
                format!(
                    "its gate Job {} carries no readable creation time ({:?})",
                    j.name, j.created
                )
            })?
            .with_timezone(&Utc);
        let since = (now - created).num_hours();
        if since < GATE_DEADLINE_HOURS {
            Err(format!(
                "its gate Job {} was created {since}h ago, inside the {GATE_DEADLINE_HOURS}h \
                 deadline",
                j.name
            ))
        } else {
            Ok(hours.min(since))
        }
    })
}

/// How long a recorded verdict is left to the settle of the write that
/// recorded it. That settle — the readiness pass that readies the
/// verdict's outcome, and the close — runs inside the same request,
/// milliseconds after the step lands (gate-run a2cb4f44: 12 ms), so a
/// packet still open a reconcile pass later was never settled.
pub(crate) const VERDICT_SETTLE_GRACE_MINUTES: i64 = 2;

/// PURE: the `record-verdict` step id of an OPEN gate-run whose verdict
/// is recorded and older than [`VERDICT_SETTLE_GRACE_MINUTES`] — a packet
/// the recording write never settled. `None` leaves the run alone.
///
/// WHY (backlog f4f5c271). Gate-run 21ad0189's verdict was completed
/// green at 2026-10-09T01:40:46Z and its readiness pass did not run, so
/// `green` stayed pending and the packet stood open for a day and a half,
/// drawn as a gate still running. The jobs API answered that write 204
/// then; since 17bf6f3e it answers 500 and runs the missed pass on a
/// re-send of the completed step. Nothing here re-sent: `verdict_owed`
/// and `complete_step` both skip a verdict step that is done, so the
/// retry the 500 asks for never came. The re-send is the status alone
/// ([`step_completion_writes`] with no fields), which writes nothing to
/// the step and is idempotent on the server.
///
/// A step with no readable `completed_at` is owed the re-send: an
/// absent stamp is not evidence of a recent write, and the re-send
/// changes nothing a settled packet holds. Nor is a stamp a minute or
/// more AHEAD of `now`: `completed_at` comes off the jobs API's clock
/// port (the sim clock on a simulated instance) and `now` off the
/// conductor's, so such a stamp is two clocks disagreeing, and read as
/// recent it would keep the run from ever being re-sent (backlog
/// c98ff222). Seconds of skew truncate to zero minutes and stay recent.
pub(crate) fn unsettled_verdict_step(run: &Value, now: DateTime<Utc>) -> Option<&str> {
    if run.get("kind").and_then(Value::as_str) != Some("gate-run")
        || run.get("status").and_then(Value::as_str) != Some("open")
    {
        return None;
    }
    let step = find_step(run, "record-verdict", "Record the gate verdict")?;
    if step.get("status").and_then(Value::as_str) != Some("completed") {
        return None;
    }
    let recent = step
        .get("completed_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|at| {
            (0..VERDICT_SETTLE_GRACE_MINUTES)
                .contains(&(now - at.with_timezone(&Utc)).num_minutes())
        });
    if recent {
        return None;
    }
    step.get("id").and_then(Value::as_str)
}

/// The gate-run's count of settling re-sends sent so far — written on
/// the packet before each re-send, so the next pass reads how many have
/// already settled nothing (backlog c98ff222).
pub(crate) const SETTLE_RESENDS: &str = "settle_resends";

/// The id of the alarm filed for a gate-run its re-sends never settled,
/// stamped on the run so a long stall files one alarm, not one a pass.
pub(crate) const SETTLE_ALARM: &str = "settle_alarm";

/// The key an unsettled-verdict alarm names its gate-run by — how an
/// alarm already open for the run is found and adopted.
pub(crate) const SETTLE_ALARM_RUN: &str = "unsettled_gate_run";

/// How many re-sends may settle nothing before an alarm names the run.
/// The settle runs inside the re-send's own request (17bf6f3e), so one
/// re-send that left the packet open is already strange; three, a pass
/// apart, is a packet no re-send will settle.
pub(crate) const SETTLE_RESEND_ALARM_AFTER: u64 = 3;

/// The run's [`SETTLE_RESENDS`]; a stamp that is not a count reads as none.
pub(crate) fn settle_resends(run: &Value) -> u64 {
    run.pointer(&format!("/metadata/{SETTLE_RESENDS}"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// The refusal an unsettled-verdict alarm was filed on when the run
/// would not take its [`SETTLE_RESENDS`] write — the error, verbatim.
pub(crate) const SETTLE_COUNT_REFUSED: &str = "settle_count_refused";

/// Who closed an unsettled-verdict alarm whose run settled, as the
/// left-behind alarm's `cleared_by` is.
pub(crate) const SETTLE_ALARM_CLEARED_BY: &str = "conductor.unsettled-verdict-alarm";

/// PURE: is an alarm owed for an unsettled run? Once [`SETTLE_RESEND_ALARM_AFTER`]
/// re-sends have left it open — or at once when this pass's count was
/// refused — and only while it names no alarm.
///
/// WHY (backlog c98ff222, from the review of car 3b58374d). The re-send
/// ran once per pass with no count, cap or alarm: a packet the API kept
/// refusing, or one the re-send could never settle, got one PUT and one
/// log line forever and filed nothing — a check nobody reads.
///
/// `count_refused` (backlog bcaeae82). A run that refuses its count
/// never reaches the bound, so the count alone left that case exactly
/// as quiet as before: a journal line a pass, nothing in the record.
/// The refusal is the fact, and it is owed an alarm the pass it is
/// seen. Not a counter of refusals: each pass is its own `boss train
/// reconcile` process, so a counter in memory starts at zero every
/// time, and the run — the only durable place — is what refused.
pub(crate) fn settle_alarm_due(run: &Value, count_refused: bool) -> bool {
    (count_refused || settle_resends(run) >= SETTLE_RESEND_ALARM_AFTER)
        && run
            .pointer(&format!("/metadata/{SETTLE_ALARM}"))
            .is_none_or(Value::is_null)
}

/// The open alarm already naming `rid`, if one is among `open` — adopted
/// in place of a twin when the run's own stamp did not land.
pub(crate) fn open_settle_alarm(open: &[Value], rid: &str) -> Option<String> {
    open.iter().find_map(|j| {
        (j.pointer(&format!("/metadata/{SETTLE_ALARM_RUN}"))
            .and_then(Value::as_str)
            == Some(rid))
        .then(|| j.get("id").and_then(Value::as_str).map(str::to_string))
        .flatten()
    })
}

/// Every open unsettled-verdict alarm and the run it names:
/// `(alarm id, gate-run id)`.
pub(crate) fn settle_alarms(open: &[Value]) -> Vec<(String, String)> {
    open.iter()
        .filter_map(|a| {
            let rid = a
                .pointer(&format!("/metadata/{SETTLE_ALARM_RUN}"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?;
            let id = a.get("id").and_then(Value::as_str)?;
            Some((id.to_string(), rid.to_string()))
        })
        .collect()
}

/// PURE: why an unsettled-verdict alarm's claim has ended, named from
/// its run — `None` while the run is open, or when no status was read
/// (unread is not ended).
///
/// THE HALF THAT WAS MISSING (backlog bcaeae82, gap 1). The alarm was
/// filed and nothing closed it, so a stall that settled on the next
/// re-send left a standing alarm an operator had to close by hand — the
/// left-behind alarm's defect (7919fdcc), in this family. The run's own
/// close is the end of the claim. When it settled is the newest step
/// completion it carries (the outcome that closed it), else its
/// `closed_on` date.
pub(crate) fn settle_ended(run: &Value) -> Option<String> {
    let status = run
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    if status == "open" {
        return None;
    }
    let settled_at = run
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("completed_at").and_then(Value::as_str))
        .filter_map(|t| DateTime::parse_from_rfc3339(t).ok().map(|at| (at, t)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, t)| t.to_string())
        .or_else(|| {
            run.get("closed_on")
                .and_then(Value::as_str)
                .map(|d| format!("{d} (its close date; no step completion read)"))
        })
        .unwrap_or_else(|| "(no settle time read)".to_string());
    Some(format!(
        "the gate-run is {status}, settled at {settled_at}, after {} settling re-send(s) \
         counted on it",
        settle_resends(run)
    ))
}

/// The triage fields that CLOSE an unsettled-verdict alarm whose run
/// settled: the `stale` terminal, through `close_alarm_stale`.
pub(crate) fn settle_alarm_clear_writes(rid: &str, why: &str) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("disposition".into(), json!("stale"));
    metadata.insert(
        "evidence".into(),
        json!(format!(
            "The conductor re-read gate-run {rid} and its verdict has settled: {why}. The \
             claim this alarm carried no longer holds; closed by machine, not by judgement. \
             A run that stalls again files a new alarm. Backlog bcaeae82."
        )),
    );
    metadata.insert("cleared_by".into(), json!(SETTLE_ALARM_CLEARED_BY));
    metadata
}

/// PURE: the backlog-item a gate-run its re-sends never settled becomes.
/// It names the run, its branch, the verdict it recorded and the count,
/// because detection is not diagnosis: the reader starts at the packet,
/// not at the conductor's journal. `count_refused` is the error the run
/// answered its count with, when that is why the alarm is filed now
/// (backlog bcaeae82): the count it carries is then not the re-sends
/// sent, and the alarm says so rather than reporting a zero.
pub(crate) fn unsettled_verdict_alarm_body(
    run: &Value,
    now: DateTime<Utc>,
    owner: &str,
    count_refused: Option<&str>,
) -> Value {
    let rid = run.get("id").and_then(Value::as_str).unwrap_or_default();
    let md = metadata_map(run);
    let branch = md
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("(no branch)");
    let verdict = find_step(run, "record-verdict", "Record the gate verdict")
        .and_then(|s| s.pointer("/metadata/verdict"))
        .and_then(Value::as_str)
        .unwrap_or("(none read)");
    let resends = settle_resends(run);
    let title = match count_refused {
        None => format!(
            "UNSETTLED GATE VERDICT: {branch} gate-run {} recorded {verdict} and is still open \
             after {resends} settling re-sends",
            id8(rid)
        ),
        Some(_) => format!(
            "UNSETTLED GATE VERDICT: {branch} gate-run {} recorded {verdict}, is still open, \
             and its settling re-sends cannot be counted on it",
            id8(rid)
        ),
    };
    let refused = count_refused.map_or(String::new(), |e| {
        format!(
            " THE COUNT ITSELF WAS REFUSED: the run answered the `{SETTLE_RESENDS}` write with \
             \"{e}\", so the {resends} it carries is not the re-sends sent, and this alarm is \
             filed on the refusal rather than at {SETTLE_RESEND_ALARM_AFTER} re-sends. The same \
             refusal may keep the alarm's id off the run; each pass then adopts this alarm \
             rather than filing a twin. Backlog bcaeae82."
        )
    });
    let mut body = json!({
        "kind": "backlog-item",
        "status": "open",
        "title": title,
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        // The platform owner as the registry answers it, never a literal
        // person — the same as every conductor alarm (3c23662d). No
        // `opened_on`: the create handler stamps it (dd3624a0).
        "owner_id": owner,
        "priority": "standard",
        "tags": ["pipeline", "gate"],
        "metadata": {
            "area": "pipeline",
            SETTLE_ALARM_RUN: rid,
            "branch": branch,
            "verdict": verdict,
            SETTLE_RESENDS: resends,
            "last_measured_at": now.to_rfc3339(),
            "detail": format!(
                "Gate-run {rid} ({branch}) has its record-verdict step completed as {verdict}, \
                 and the packet is still open after {resends} status-only re-sends of that \
                 completion, one per reconcile pass. Each re-send asks the jobs API to run the \
                 readiness pass and the close the recording write missed (17bf6f3e); none did, \
                 so the verdict's outcome step never readied and the car it gates waits. The \
                 conductor keeps re-sending and counts on the run as `{SETTLE_RESENDS}`; this \
                 alarm is filed once per run (its id rides on the run as `{SETTLE_ALARM}`). \
                 Read the run's steps and the jobs API's answer to the re-send in the \
                 conductor's journal. Backlog c98ff222.{refused} It closes itself, \
                 `stale`, once the run reads closed."
            ),
        },
    });
    if let Some(e) = count_refused {
        body["metadata"][SETTLE_COUNT_REFUSED] = json!(e);
    }
    body
}

/// Whole minutes since a gate-run last said it was alive — the same
/// reading as both clocks above — or `None` when it carries no stamp
/// that parses. For the recorder's note on a run it is keeping waiting
/// (`carried_verdict::blocked_note`, backlog 06ae925a).
pub(crate) fn gate_run_quiet_minutes(run: &Value, now: DateTime<Utc>) -> Option<i64> {
    let (_, (_, at)) = last_sign_of_life(&metadata_map(run))?;
    Some((now - at).num_minutes())
}

/// The newest parseable of [`ALIVE_STAMPS`] — its key, its value verbatim
/// and its instant — or `None` when `opened_at` itself does not parse
/// (no stamp, no claim). A stamp that does not parse is skipped, never a
/// reason to call a run older than it is. Shared by both clocks, so the
/// orphan window and the Job deadline cannot read different lives.
fn last_sign_of_life(md: &Map<String, Value>) -> Option<(&'static str, (String, DateTime<Utc>))> {
    // A dotted stamp names a field inside a metadata object: the
    // reservation row's beat (`RESERVATION_BEAT`).
    let instant = |key: &str| {
        let mut path = key.split('.');
        let first = md.get(path.next()?);
        path.fold(first, |v, k| v.and_then(|v| v.get(k)))
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok().map(|t| (s, t)))
            .map(|(s, t)| (s.to_string(), t.with_timezone(&Utc)))
    };
    instant("opened_at")?;
    ALIVE_STAMPS
        .iter()
        .filter_map(|k| instant(k).map(|v| (*k, v)))
        .max_by_key(|(_, (_, t))| *t)
}

/// How long a gate-run may go with nothing saying it is alive before the
/// conductor asks the cluster whether any Job carries it (backlog
/// 137c176d). Three times a queue place's TTL
/// (`gate::QUEUE_PLACE_TTL_SECS`, 300s), so a waiter between beats — or
/// riding out an SoR roll, which it abandons after three minutes anyway
/// (`gate::ABSENCE_TOLERANCE`) — never reads as gone; and a twelfth of
/// the Job deadline it stands in for.
pub(crate) const ORPHAN_GATE_RUN_MINUTES: i64 = 15;

// Both bounds are held at compile time, so neither constant can move
// past the other without the build saying so: the window outlasts a
// place in line, and stays well under the deadline it stands in for.
const _: () = assert!(ORPHAN_GATE_RUN_MINUTES * 60 > crate::gate::QUEUE_PLACE_TTL_SECS);
const _: () = assert!(ORPHAN_GATE_RUN_MINUTES < GATE_DEADLINE_HOURS * 60 / 4);

/// The stamps on a gate-run that each say "a process held this run at
/// this instant", in the order they are written: filed, queued, the
/// waiter's beat (kept on release, so a run leaving the line to launch
/// is dated from its last beat, not from its filing), and the launch of
/// a REUSED packet, whose other stamps belong to an earlier run.
///
/// THREE CLOCKS MEET HERE. `opened_at` is stamped by the jobs API's
/// create handler from its clock port (`boss_clock_client::now_from` —
/// the deploy-mode clock, wall time in production and sim-aware by
/// design, not the record stamp `wall_now`); the queue stamps and
/// `launching_at` are the clock of the host running `boss gate` (the dev
/// pod for a builder, the conductor for a train gate — `now` minted
/// once, carried forward by monotonic time); and the `now` they are
/// judged against is the conductor's. All are cluster hosts on
/// synchronised UTC, so their skew is seconds against a fifteen-minute
/// window — and a skew of minutes would shorten or lengthen the window
/// by exactly that much, never settle a run a Job carries, because the
/// cluster read decides that.
///
/// Both clocks read these: the orphan window ([`orphaned_gate_run`]) and
/// the Job deadline ([`dead_gate_run_hours`]).
///
/// THE BEAT WHEREVER IT IS WRITTEN, AND THE LAUNCH (backlog d1ea4141).
/// A waiter on the versioned reservation beats `gate_reservation.
/// heartbeat_at`; `renew` copies it to `queue_heartbeat_at` today
/// (since 498e6f750), and the clock reads both so that a writer of only
/// the row is not read as silence. And `launched_at`, which `boss gate`
/// stamps once the Job exists (and the native observation writes with
/// the Job's own creation time), is the one stamp that dates the Job
/// itself: until this list read it, a fresh packet that waited three
/// hours to launch was dated from its last queue beat at best.
const ALIVE_STAMPS: [&str; 6] = [
    "opened_at",
    crate::gate::QUEUED_AT,
    crate::gate::QUEUE_HEARTBEAT_AT,
    RESERVATION_BEAT,
    crate::gate::LAUNCHING_AT,
    crate::gate::LAUNCHED_AT,
];

/// The reservation row's beat, as a dotted path into the packet's
/// metadata. Both segments are boss-jobs' facts — `gate_reservation::KEY`
/// and the serialised name of `Reservation::heartbeat_at` — and
/// `the_job_deadline_reads_the_reservation_beat_and_the_launch` pins
/// them by serialising a real `Reservation` and reading its beat back,
/// so a rename in boss-jobs turns that test red (backlog 54a6832b).
const RESERVATION_BEAT: &str = "gate_reservation.heartbeat_at";

/// A gate-run with no verdict that nothing has held for the window —
/// the half of an orphan the system of record can see. The other half,
/// that no gate Job carries it, is a cluster fact the adapter reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrphanCandidate {
    /// Which stamp was the latest sign of life, and its value verbatim.
    pub last_alive_key: &'static str,
    pub last_alive: String,
    /// Whole minutes since then.
    pub idle_minutes: i64,
    /// The place in line it held, if it ever queued.
    pub queued_at: Option<String>,
}

/// Is this open gate-run a candidate orphan — `None` means leave it.
///
/// WHY (backlog 137c176d). Gate-run 91594262 was queued at 01:26:24 on
/// 2026-09-24; its waiter's last beat was 01:26:54, the dev pod was
/// evicted at 01:27, and no Job was ever created. It stayed open until
/// [`dead_gate_run_hours`] closed it at 04:30:45, and because the
/// estate observer's dead-host pass defers to an open gate-run, the
/// builder run it belonged to stayed `building` for the same three
/// hours. The clock is a ceiling for a Job that EXISTS and might still
/// be running; a run no Job carries is not running at all.
///
/// Pure, like its neighbour: the run has not reported, its `opened_at`
/// parses (no stamp, no claim), and the latest readable of
/// [`ALIVE_STAMPS`] is at least [`ORPHAN_GATE_RUN_MINUTES`] old. A stamp
/// that does not parse is skipped — never a reason to call a run older
/// than it is.
///
/// NOT READ: the verdict step's `heartbeat_at`. gate-run.toml defaults it
/// to "" and nothing writes it — run.sh does not heartbeat — so a rule
/// on it was always true and a receipt citing it claimed a reading
/// nobody took (review of car 2ca8c7e9). The runner lives inside the
/// Job, so the cluster read the adapter makes is the heartbeat.
pub(crate) fn orphaned_gate_run(run: &Value, now: DateTime<Utc>) -> Option<OrphanCandidate> {
    if run
        .get("metadata")
        .is_some_and(boss_jobs::gate_reservation::unresolved_intent)
    {
        return None;
    }
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict")?;
    if step_done(Some(verdict_step)) {
        return None;
    }
    let md = metadata_map(run);
    let (last_alive_key, (last_alive, at)) = last_sign_of_life(&md)?;
    let idle_minutes = (now - at).num_minutes();
    (idle_minutes >= ORPHAN_GATE_RUN_MINUTES).then(|| OrphanCandidate {
        last_alive_key,
        last_alive,
        idle_minutes,
        queued_at: md
            .get("queued_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// The evidence an orphan settle leaves on the packet, as data: the
/// cluster read that came back empty and the last sign of life.
/// `observed_at` is the instant of that cluster read — the reading the
/// evidence reports — not the start of the pass (b24e29cb item 3).
pub(crate) fn orphan_evidence(
    o: &OrphanCandidate,
    packet: &str,
    namespace: &str,
    observed_at: DateTime<Utc>,
) -> Value {
    serde_json::json!({
        "runner_jobs": 0,
        "namespace": namespace,
        "selector": format!("boss.dev/packet={packet}"),
        "last_alive_key": o.last_alive_key,
        "last_alive": o.last_alive,
        "idle_minutes": o.idle_minutes,
        "queued_at": o.queued_at,
        "observed_at": crate::sor_client::stamp(observed_at),
        "observer": "boss train cadence (conductor reconcile)",
    })
}

/// The receipt an orphan settle writes: what was read, what it means,
/// and what a re-gate needs that this packet cannot carry forward.
pub(crate) fn orphan_receipt(o: &OrphanCandidate, branch: &str, namespace: &str) -> String {
    format!(
        "NO VERDICT WAS PRODUCED. The cluster holds no gate Job for this packet (namespace \
         {namespace}, label boss.dev/packet), and nothing has held it since {key} {at} — {mins} min, past the {ORPHAN_GATE_RUN_MINUTES} \
         min window. No runner exists to report, so the checks never ran or never finished. \
         Settled as LOST by the conductor's reconcile: this run says nothing about {branch}, \
         and an infrastructure death is not a consist failure. Re-gate for a real verdict — \
         with its --park-* flags (or --park-file), because a closed packet is not reused and \
         its park intent does not carry to the new one. Evidence: the packet's \
         orphaned_gate_run.",
        key = o.last_alive_key,
        at = o.last_alive,
        mins = o.idle_minutes,
    )
}

/// What the orphan settle knows when its `withdrawn` completion returned
/// an error, once it has read the packet back (backlog 847566e6).
///
/// WHY FOUR, AND NOT "PREDATES THE WORD". The settle used to log every
/// such error as "its protocol version predates the word; settling as
/// lost" — so a transient 5xx, a rollout gap or a write that landed and
/// lost only its answer became a dead runner on the record with a cause
/// nobody read. Car fix/gate-withdraw-waits-for-recorded-closure
/// (7905cb5d) took the same inference out of the door; the read's
/// classification here is that car's [`crate::gate::WithdrawalReadback`],
/// not a second copy.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WithdrawnNotRecorded {
    /// The read shows the verdict landed after all —
    /// [`crate::gate::WithdrawalReadback::Closed`] or `ClosurePending`.
    /// Nothing more is owed from the settle.
    Recorded(crate::gate::WithdrawalReadback),
    /// The verdict landed `withdrawn`, and the version the packet is
    /// pinned to put no `withdrawn` outcome step on it. The step is
    /// completed, so `lost` cannot replace it; it is said, not settled.
    MissingTerminal { version: Option<i64> },
    /// The jobs API ANSWERED the write with a refusal, and the read shows
    /// the verdict not recorded: the one case that falls back to `lost`,
    /// with the refusal itself as the cause.
    Refused { cause: String },
    /// Neither the error nor the read says what happened — a 5xx, a
    /// transport failure, a policy outage, a read that failed. Held for
    /// the next pass rather than settled on a guess.
    Unknown { cause: String },
}

/// How many reads the orphan settle makes of a gate-run whose `withdrawn`
/// completion returned an error, and how far apart. The measured lag
/// from a verdict landing to its `withdrawn` terminal completing was
/// 1.39 s and 1.95 s (gate-runs 80f7d03b, 5286a359, 2026-10-17), but the
/// settle does not wait for closure — a recorded verdict ends the read —
/// so five reads cover a write whose answer was lost in a roll without
/// holding a reconcile pass for the door's thirty. Shorter under test.
pub(crate) const ORPHAN_WITHDRAW_READS: u32 = 5;
pub(crate) const ORPHAN_WITHDRAW_READ_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(1)
};

/// PURE: the status of an answered refusal in a jobs-API error, read out
/// of the message `api_once` builds (`<method> <path>: HTTP <status>:
/// <body>`), the way `is_no_such_job` reads its 404 and for the reason
/// it gives. A 4xx is an answer, except a 408 or 429 (the server did not
/// decide) and a 403 naming a policy outage (`names_a_policy_outage`).
pub(crate) fn answered_refusal(err: &anyhow::Error) -> Option<u16> {
    err.chain().find_map(|c| {
        let text = c.to_string();
        let at = text.find("HTTP ")?;
        let status: u16 = text.get(at + 5..at + 8)?.parse().ok()?;
        let refusal = (400..500).contains(&status)
            && status != 408
            && status != 429
            && !names_a_policy_outage(&text);
        refusal.then_some(status)
    })
}

/// PURE: classify a failed `withdrawn` completion from its error and the
/// read-back that followed it. The read decides whether it landed; only
/// when the read shows it did not does the error's kind matter.
pub(crate) fn withdrawn_completion_failure(
    err: &anyhow::Error,
    read: &std::result::Result<crate::gate::WithdrawalReadback, String>,
) -> WithdrawnNotRecorded {
    use crate::gate::WithdrawalReadback as R;
    match read {
        Ok(landed @ (R::Closed | R::ClosurePending { .. })) => {
            WithdrawnNotRecorded::Recorded(landed.clone())
        }
        Ok(R::MissingTerminal { version }) => {
            WithdrawnNotRecorded::MissingTerminal { version: *version }
        }
        Ok(R::VerdictNotRead) if answered_refusal(err).is_some() => WithdrawnNotRecorded::Refused {
            cause: format!("{err:#}"),
        },
        Ok(R::VerdictNotRead) => WithdrawnNotRecorded::Unknown {
            cause: format!("{err:#}; the read-back does not show the verdict recorded"),
        },
        Err(read) => WithdrawnNotRecorded::Unknown {
            cause: format!("{err:#}; the read-back failed too: {read}"),
        },
    }
}

/// PURE: the journal line for each [`WithdrawnNotRecorded`], naming its
/// own cause.
pub(crate) fn withdrawn_failure_line(short: &str, w: &WithdrawnNotRecorded) -> String {
    use crate::gate::WithdrawalReadback as R;
    let pinned = |v: &Option<i64>| match v {
        Some(n) => format!("gate-run v{n}"),
        None => "the gate-run version it is pinned to (the read did not say which)".into(),
    };
    match w {
        WithdrawnNotRecorded::Recorded(R::ClosurePending { version }) => format!(
            "reconcile: gate-run {short}: the `withdrawn` completion returned an error, but the \
             read-back shows the verdict recorded and the `withdrawn` outcome step of {} not \
             completed yet — closure PENDING, nothing more to settle",
            pinned(version)
        ),
        WithdrawnNotRecorded::Recorded(_) => format!(
            "reconcile: gate-run {short}: the `withdrawn` completion returned an error, but the \
             read-back shows the packet closed — withdrawn"
        ),
        WithdrawnNotRecorded::MissingTerminal { version } => format!(
            "reconcile: gate-run {short} recorded `withdrawn`, but {} put no `withdrawn` outcome \
             step on this packet, so nothing on it closes on that word; the completed verdict \
             cannot be replaced by `lost`, so it is left as written",
            pinned(version)
        ),
        WithdrawnNotRecorded::Refused { cause } => format!(
            "reconcile: gate-run {short}: the jobs API REFUSED `withdrawn` ({cause}) and the \
             read-back shows the verdict not recorded — settling as lost"
        ),
        WithdrawnNotRecorded::Unknown { cause } => format!(
            "reconcile: gate-run {short}: `withdrawn` was not confirmed ({cause}) — UNKNOWN, \
             not refused; nothing settled, the next pass reads it again"
        ),
    }
}

/// The receipt a gate-run the conductor FILED and could not start is
/// settled with (backlog 7919fdcc, item 11): the cause verbatim — the
/// only copy of it — and what the run does not say.
pub(crate) fn unstarted_receipt(branch: &str, cause: &anyhow::Error) -> String {
    format!(
        "NO VERDICT WAS PRODUCED. The conductor filed this gate-run for {branch} and then could \
         not start its runner Job: {cause:#}. No runner ever existed, so no check ran. Settled \
         as LOST by the conductor at once, rather than left open for the orphan window with \
         nothing referencing it: this run says nothing about {branch}, and an infrastructure \
         failure is not a consist failure."
    )
}

/// Should this closed gate-run's verdict be buried if its sha landed?
/// Returns the (sha, verdict) to check when the run is closed with a
/// `failed` or `lost` verdict, names a sha, is not already superseded,
/// and was opened within the yard's approach window — the same two days
/// after which the lens calls a closed gate archaeology, so nothing
/// older is touched. Everything else is None: a green needs no burial,
/// an open run is live activity, and an annotated run is settled.
pub(crate) const APPROACH_FRESH_DAYS: i64 = 2;

pub(crate) fn verdict_to_bury(run: &Value, now: DateTime<Utc>) -> Option<(String, String)> {
    if run.get("status").and_then(Value::as_str) != Some("closed") {
        return None;
    }
    // Main's full gate gates main itself: "landed" is always true of it,
    // and its red is answered by its own item (backlog 77596188).
    if run
        .get("metadata")
        .is_some_and(boss_jobs::full_gate_on_main::is_main_gate)
    {
        return None;
    }
    let md = metadata_map(run);
    if md
        .get("superseded")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        return None;
    }
    let verdict_step = find_step(run, "record-verdict", "Record the gate verdict");
    let verdict = verdict_step
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get("verdict"))
        .and_then(Value::as_str)
        .or_else(|| md.get("outcome").and_then(Value::as_str))?;
    if verdict != "failed" && verdict != "lost" {
        return None;
    }
    let sha = md.get("sha").and_then(Value::as_str)?.to_string();
    if sha.is_empty() {
        return None;
    }
    let opened = md
        .get("opened_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))?;
    if (now - opened).num_days() > APPROACH_FRESH_DAYS {
        return None;
    }
    Some((sha, verdict.to_string()))
}

pub(crate) fn metadata_map(v: &Value) -> Map<String, Value> {
    match v.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

/// The writes that complete step `sid` on packet `jid` carrying
/// `writes`, in order: the fields through the step's MERGE door, then
/// the status alone through the PUT. An empty `writes` is the flip
/// alone. Pure, so the shape is pinned without a socket.
///
/// WHY TWO WRITES AND NOT THE ONE PUT THESE USED TO BE (backlog
/// e39a9d2a, stage 2 of design 93d2bddb). The conductor and the
/// publish-request drain completed a step with one PUT of `{status,
/// metadata}`, the metadata a read-merge-write of the step AS THE PASS
/// READ IT. Stage 1's rule refused only a body that omitted a stored
/// key, so that worked — until a concurrent writer added a key between
/// the pass's read and its PUT, when it was refused 409 instead of kept
/// (the conductor's read can be minutes old: a reconcile reads the
/// train once and completes steps along the way). The PUT now refuses
/// ANY metadata body, David's decided end state. The merge door lands the
/// keys against the row as it stands, in one transaction, so it can
/// neither race nor shed a key it does not name. It goes FIRST because
/// required-at-done fields are judged when the step flips. If the flip
/// is then refused, the step stays open carrying the fields, and the
/// next pass re-merges them — the same outcome a refused PUT left.
pub(crate) fn step_completion_writes(
    jid: &str,
    sid: &str,
    writes: Map<String, Value>,
) -> Vec<(Method, String, Value)> {
    let merge = (!writes.is_empty()).then(|| {
        (
            Method::PATCH,
            format!("/api/jobs/{jid}/steps/{sid}/metadata"),
            Value::Object(writes),
        )
    });
    merge
        .into_iter()
        .chain(std::iter::once((
            Method::PUT,
            format!("/api/jobs/{jid}/steps/{sid}"),
            json!({"status": "completed"}),
        )))
        .collect()
}

/// The write `merge_job_metadata` makes, pure: ONE PATCH through the
/// job's merge door carrying only the keys it changes. A `Value::Null`
/// is sent as-is, and the server DELETES that key: how a boarding car
/// sheds a stale `skip_reason` instead of carrying "" forever. No other
/// key rides along, so a concurrent writer's keys survive (design
/// 38f3a488 D4; the full-PUT overlay this replaced erased them).
pub(crate) fn job_metadata_patch(jid: &str, kv: Vec<(&str, Value)>) -> (Method, String, Value) {
    let body: Map<String, Value> = kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    (
        Method::PATCH,
        format!("/api/jobs/{jid}/metadata"),
        Value::Object(body),
    )
}

/// Has this train's arrival report already been filed?
///
/// Reads the JOB's metadata, not the `arrived` step's. The report moved
/// there when terminal steps became immutable (f402a681) — and the
/// idempotence check has to move with it, or every reconcile re-files a
/// report it already wrote. That is the failure mode the 2026-08-13
/// journal records as "re-file its arrival report" making the conductor
/// look broken while the trains had in fact landed.
pub(crate) fn arrival_already_filed(train: &Value) -> bool {
    train
        .get("metadata")
        .and_then(|m| m.get("arrival_report"))
        .is_some_and(|v| !v.is_null())
}

#[cfg(test)]
mod track_tests {
    use super::{holds_the_track, track_occupied_by};
    use serde_json::{Value, json};

    /// An open train whose `merged` step sits at `status`.
    fn train(id: &str, title: &str, merged: &str) -> Value {
        json!({
            "id": id,
            "title": title,
            "status": "open",
            "steps": [
                {"spec_slug": "collect", "title": "Collect what is ready to board", "status": "completed"},
                {"spec_slug": "merged", "title": "Merged into main", "status": merged},
                {"spec_slug": "converged", "title": "Cluster converged", "status": "pending"}
            ]
        })
    }

    #[test]
    fn an_open_train_occupies_the_track_and_is_named() {
        let open = vec![train(
            "48b67f2e-9970-456f-817a-d085975b915f",
            "PR train 2026-09-05 07:26",
            "ready",
        )];
        assert_eq!(
            track_occupied_by(&open).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
    }

    #[test]
    fn no_open_train_means_a_clear_track() {
        // The caller lists status=open only; an arrived or cancelled
        // train is closed and never reaches this list.
        assert_eq!(track_occupied_by(&[]), None);
    }

    /// Twin of boss-jobs `yard::tests::a_merged_train_waiting_to_converge_does_not_hold_the_track`
    /// — the board counts the track by the same rule off typed steps.
    #[test]
    fn a_merged_train_waiting_to_converge_does_not_hold_the_track() {
        // 2026-09-07 (f3796323), twice: a merged train whose sha bricked
        // its boot sat at `converged`, and the fix-forward car could not
        // board because the track was "occupied" by the very train it
        // would have converged. Its content is on main; the next consist
        // merges on top and converges it by ancestry.
        let mut merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        merged["steps"][2]["status"] = json!("ready");
        assert!(!holds_the_track(&merged));
        assert_eq!(track_occupied_by(&[merged]), None);
    }

    /// Twin of boss-jobs `yard::tests::only_the_pre_merge_train_counts_as_the_track`.
    #[test]
    fn the_pre_merge_train_is_named_when_a_merged_one_is_also_open() {
        let merged = train(
            "a1b2c3d4-0000-0000-0000-000000000000",
            "PR train 2026-09-07 21:00",
            "completed",
        );
        let pre_merge = train(
            "e5f6a7b8-0000-0000-0000-000000000000",
            "PR train 2026-09-07 22:51",
            "ready",
        );
        assert_eq!(
            track_occupied_by(&[merged, pre_merge]).as_deref(),
            Some("PR train 2026-09-07 22:51 (e5f6a7b8)")
        );
    }

    #[test]
    fn a_train_with_no_steps_fails_closed_as_pre_merge() {
        // Unknown is not "clear": a row the list did not enrich, or a
        // train with no merged step at all, holds the track.
        let bare = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "status": "open"
        });
        assert!(holds_the_track(&bare));
        assert_eq!(
            track_occupied_by(&[bare]).as_deref(),
            Some("PR train 2026-09-05 07:26 (48b67f2e)")
        );
        let no_merged = json!({
            "id": "48b67f2e-9970-456f-817a-d085975b915f",
            "title": "PR train 2026-09-05 07:26",
            "steps": [{"spec_slug": "collect", "title": "Collect what is ready to board", "status": "ready"}]
        });
        assert!(holds_the_track(&no_merged));
    }
}

#[cfg(test)]
mod burial_tests {
    use super::verdict_to_bury;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn run(
        status: &str,
        verdict: &str,
        opened: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut md = json!({"branch": "fix/lean-ci-builds", "sha": "6c0e31a34a7659aebd63c07218cfddc1e3542fb9", "opened_at": opened});
        if let Some(o) = extra.as_object() {
            for (k, v) in o {
                md[k] = v.clone();
            }
        }
        json!({
            "id": "335d5f6d-0000-0000-0000-000000000000", "kind": "gate-run", "status": status, "metadata": md,
            "steps": [{"title": "Record the gate verdict", "spec_slug": "record-verdict", "status": "completed", "metadata": {"verdict": verdict}}]
        })
    }

    #[test]
    fn a_fresh_lost_or_failed_verdict_with_a_sha_is_a_candidate() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        for v in ["lost", "failed"] {
            let r = run("closed", v, "2026-09-04T00:20:43Z", json!({}));
            assert_eq!(
                verdict_to_bury(&r, now),
                Some((
                    "6c0e31a34a7659aebd63c07218cfddc1e3542fb9".to_string(),
                    v.to_string()
                ))
            );
        }
    }

    #[test]
    fn a_green_an_open_run_an_annotated_run_and_archaeology_are_left_alone() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        assert_eq!(
            verdict_to_bury(
                &run("closed", "green", "2026-09-04T00:20:43Z", json!({})),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(&run("open", "lost", "2026-09-04T00:20:43Z", json!({})), now),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run(
                    "closed",
                    "lost",
                    "2026-09-04T00:20:43Z",
                    json!({"superseded": "by hand"})
                ),
                now
            ),
            None
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-01T00:20:43Z", json!({})),
                now
            ),
            None,
            "older than the approach window"
        );
        assert_eq!(
            verdict_to_bury(
                &run("closed", "lost", "2026-09-04T00:20:43Z", json!({"sha": ""})),
                now
            ),
            None,
            "no sha, nothing to ask git"
        );
    }

    /// Main's full gate (backlog 77596188) gates a sha that IS main, so
    /// "its sha landed" is true the moment it is filed. Buried, a red
    /// main would be stamped "the change went in without this gate" — a
    /// false sentence on the one red that says main is broken.
    #[test]
    fn a_main_gate_is_never_buried() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 17, 0, 0).unwrap();
        for v in ["lost", "failed"] {
            let r = run(
                "closed",
                v,
                "2026-09-04T00:20:43Z",
                json!({"branch": "main", "main_gate": true}),
            );
            assert_eq!(verdict_to_bury(&r, now), None, "{v}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Owed a settle: open, verdict completed, past the grace. Not owed:
    /// a verdict still open, a closed packet, one recorded a moment ago,
    /// a skipped verdict step, or another kind (backlog f4f5c271).
    #[test]
    fn only_an_open_gate_run_with_an_old_recorded_verdict_is_owed_a_settle() {
        let now = DateTime::parse_from_rfc3339("2026-10-10T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let run = |status: &str, step: &str, at: Option<&str>| {
            let mut s = json!({"id": "s-v", "spec_slug": "record-verdict",
                               "title": "Record the gate verdict", "status": step,
                               "metadata": {"verdict": "green"}});
            if let Some(at) = at {
                s["completed_at"] = json!(at);
            }
            json!({"id": "r", "kind": "gate-run", "status": status, "steps": [s]})
        };
        // The live case: 21ad0189, recorded 2026-10-09T01:40:46Z.
        let stuck = run("open", "completed", Some("2026-10-09T01:40:46.453747Z"));
        assert_eq!(unsettled_verdict_step(&stuck, now), Some("s-v"));
        assert_eq!(
            unsettled_verdict_step(&run("open", "completed", None), now),
            Some("s-v"),
            "no stamp is not evidence of a recent write"
        );
        assert_eq!(
            unsettled_verdict_step(&run("open", "completed", Some("2026-10-10T14:59:30Z")), now),
            None,
            "inside the grace the recording write's own settle is still running"
        );
        assert_eq!(
            unsettled_verdict_step(&run("open", "ready", None), now),
            None
        );
        assert_eq!(
            unsettled_verdict_step(&run("open", "skipped", None), now),
            None
        );
        assert_eq!(
            unsettled_verdict_step(&run("closed", "completed", None), now),
            None
        );
        let mut other = stuck.clone();
        other["kind"] = json!("ship-a-change");
        assert_eq!(unsettled_verdict_step(&other, now), None);
    }

    /// `completed_at` is stamped off the jobs API's clock port — the sim
    /// clock on a simulated instance — and `now` is the conductor's wall
    /// clock (backlog c98ff222, from the review of car 3b58374d). A stamp
    /// a minute or more AHEAD of the reader is two clocks disagreeing, not
    /// a write still settling, and read as "recent" it kept the run from
    /// ever being re-sent. Seconds of skew still read as recent.
    #[test]
    fn a_verdict_stamped_ahead_of_the_readers_clock_is_still_owed_a_settle() {
        let now = DateTime::parse_from_rfc3339("2026-10-10T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let run = |at: &str| {
            json!({"id": "r", "kind": "gate-run", "status": "open",
                   "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                              "title": "Record the gate verdict", "status": "completed",
                              "completed_at": at, "metadata": {"verdict": "green"}}]})
        };
        assert_eq!(
            unsettled_verdict_step(&run("2026-10-10T18:00:00Z"), now),
            Some("s-v"),
            "a stamp three hours ahead is a clock disagreement, not a recent write"
        );
        assert_eq!(
            unsettled_verdict_step(&run("2026-10-10T15:00:20Z"), now),
            None,
            "twenty seconds of skew is still inside the grace"
        );
    }

    /// The count, the due test and the alarm a run its re-sends never
    /// settled becomes (backlog c98ff222).
    #[test]
    fn an_alarm_is_due_once_after_the_resends_that_settled_nothing() {
        let run = |md: Value| {
            json!({"id": "run-1234abcd", "kind": "gate-run", "status": "open",
                   "metadata": md,
                   "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                              "title": "Record the gate verdict", "status": "completed",
                              "metadata": {"verdict": "green"}}]})
        };
        assert_eq!(settle_resends(&run(json!({}))), 0);
        assert_eq!(settle_resends(&run(json!({SETTLE_RESENDS: "two"}))), 0);
        let below = run(json!({SETTLE_RESENDS: SETTLE_RESEND_ALARM_AFTER - 1}));
        assert!(
            !settle_alarm_due(&below, false),
            "fewer re-sends than the bound"
        );
        let due = run(json!({"branch": "fix/stuck", SETTLE_RESENDS: SETTLE_RESEND_ALARM_AFTER}));
        assert!(settle_alarm_due(&due, false));
        let filed = run(json!({SETTLE_RESENDS: 9, SETTLE_ALARM: "a1"}));
        assert!(!settle_alarm_due(&filed, false), "one alarm per run");
        let cleared = run(json!({SETTLE_RESENDS: 9, SETTLE_ALARM: null}));
        assert!(settle_alarm_due(&cleared, false));

        let now = DateTime::parse_from_rfc3339("2026-10-10T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let body = unsettled_verdict_alarm_body(&due, now, "emp-owner", None);
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["owner_id"], "emp-owner");
        assert_eq!(body["metadata"][SETTLE_ALARM_RUN], "run-1234abcd");
        assert_eq!(body["metadata"][SETTLE_RESENDS], SETTLE_RESEND_ALARM_AFTER);
        assert_eq!(body["metadata"]["verdict"], "green");
        let title = body["title"].as_str().unwrap();
        assert!(
            title.contains("fix/stuck") && title.contains("run-1234"),
            "{title}"
        );
        assert!(
            body.get("opened_on").is_none(),
            "the create handler stamps it"
        );

        // An open alarm already naming the run is adopted, never twinned.
        let open = vec![
            json!({"id": "other", "metadata": {SETTLE_ALARM_RUN: "run-elsewhere"}}),
            json!({"id": "mine", "metadata": {SETTLE_ALARM_RUN: "run-1234abcd"}}),
        ];
        assert_eq!(
            open_settle_alarm(&open, "run-1234abcd").as_deref(),
            Some("mine")
        );
        assert_eq!(open_settle_alarm(&open, "run-none"), None);
    }

    /// A REFUSED COUNT IS ITSELF THE ALARM (backlog bcaeae82, gap 2). When
    /// the run will not take its `settle_resends` write the count never
    /// grows, so the bound above is never reached; the refusal is the
    /// fact, and it is due at once — once, while the run names no alarm.
    #[test]
    fn a_refused_count_makes_the_alarm_due_at_once_and_says_why() {
        let run = |md: Value| {
            json!({"id": "run-1234abcd", "kind": "gate-run", "status": "open",
                   "metadata": md,
                   "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                              "title": "Record the gate verdict", "status": "completed",
                              "metadata": {"verdict": "green"}}]})
        };
        let fresh = run(json!({"branch": "fix/stuck"}));
        assert!(!settle_alarm_due(&fresh, false), "a counted first re-send");
        assert!(settle_alarm_due(&fresh, true), "a refused count is due now");
        let named = run(json!({SETTLE_ALARM: "a1"}));
        assert!(!settle_alarm_due(&named, true), "still one alarm per run");

        let now = DateTime::parse_from_rfc3339("2026-10-10T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let body = unsettled_verdict_alarm_body(&fresh, now, "emp-owner", Some("HTTP 409: no"));
        assert_eq!(body["metadata"][SETTLE_ALARM_RUN], "run-1234abcd");
        assert_eq!(body["metadata"][SETTLE_COUNT_REFUSED], "HTTP 409: no");
        let title = body["title"].as_str().unwrap();
        assert!(title.contains("cannot be counted"), "{title}");
        let detail = body["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("HTTP 409: no"), "{detail}");
        let counted = unsettled_verdict_alarm_body(&fresh, now, "emp-owner", None);
        assert!(counted["metadata"].get(SETTLE_COUNT_REFUSED).is_none());
    }

    /// THE ALARM'S CLEARER (backlog bcaeae82, gap 1). An open alarm names
    /// its run; once that run reads closed the claim no longer holds, and
    /// the close names the count the run carries and when it settled —
    /// the newest completion among its steps. An open run, or a run that
    /// could not be read, keeps its alarm: unread is not ended.
    #[test]
    fn a_settled_run_ends_its_alarm_with_the_count_and_the_settle_time() {
        let open = vec![
            json!({"id": "a-other", "metadata": {"left_behind_car": "car-1"}}),
            json!({"id": "a-1", "metadata": {SETTLE_ALARM_RUN: "run-1234abcd"}}),
            json!({"id": "a-blank", "metadata": {SETTLE_ALARM_RUN: ""}}),
        ];
        assert_eq!(
            settle_alarms(&open),
            vec![("a-1".to_string(), "run-1234abcd".to_string())]
        );
        let run = |status: &str| {
            json!({"id": "run-1234abcd", "status": status, "closed_on": "2026-10-10",
                   "metadata": {"branch": "fix/stuck", SETTLE_RESENDS: 4},
                   "steps": [
                       {"status": "completed", "completed_at": "2026-10-10T14:00:00Z"},
                       {"status": "completed", "completed_at": "2026-10-10T15:20:07Z"},
                       {"status": "skipped", "completed_at": null}]})
        };
        assert_eq!(
            settle_ended(&run("open")),
            None,
            "still open: the alarm stands"
        );
        assert_eq!(settle_ended(&json!({})), None, "no status read: unread");
        let why = settle_ended(&run("closed")).expect("a closed run ended it");
        assert!(
            why.contains("closed") && why.contains("4") && why.contains("2026-10-10T15:20:07Z"),
            "{why}"
        );
        let bare = json!({"id": "r", "status": "closed", "closed_on": "2026-10-11"});
        assert!(settle_ended(&bare).unwrap().contains("2026-10-11"));

        let writes = settle_alarm_clear_writes("run-1234abcd", &why);
        assert_eq!(writes["disposition"], "stale");
        assert_eq!(writes["cleared_by"], SETTLE_ALARM_CLEARED_BY);
        let evidence = writes["evidence"].as_str().unwrap();
        assert!(
            evidence.contains("run-1234abcd") && evidence.contains("2026-10-10T15:20:07Z"),
            "{evidence}"
        );
    }

    /// A completion is the fields through the merge door, then a PUT
    /// carrying the status and NOTHING else — the only form that
    /// survives the step PUT refusing any metadata body (e39a9d2a).
    #[test]
    fn a_step_completes_as_a_merge_then_a_status_only_put() {
        let mut fields = Map::new();
        fields.insert("merge_ref".into(), json!("abcdef123456"));
        let writes = step_completion_writes("j1", "s1", fields.clone());
        assert_eq!(
            writes,
            vec![
                (
                    Method::PATCH,
                    "/api/jobs/j1/steps/s1/metadata".to_string(),
                    Value::Object(fields),
                ),
                (
                    Method::PUT,
                    "/api/jobs/j1/steps/s1".to_string(),
                    json!({"status": "completed"}),
                ),
            ]
        );
    }

    /// Nothing to record sends no empty merge — the flip alone.
    #[test]
    fn a_completion_with_no_fields_is_the_flip_alone() {
        let writes = step_completion_writes("j1", "s1", Map::new());
        assert_eq!(
            writes,
            vec![(
                Method::PUT,
                "/api/jobs/j1/steps/s1".to_string(),
                json!({"status": "completed"}),
            )]
        );
    }

    /// Both list shapes read, and an empty array stays an honest zero.
    #[test]
    fn rows_reads_a_bare_array_and_the_envelope() {
        let bare = rows(Some(serde_json::json!([1, 2, 3]))).expect("bare");
        assert_eq!(bare.len(), 3);
        let wrapped = rows(Some(serde_json::json!({"data": [1, 2], "total": 9}))).expect("env");
        assert_eq!(wrapped.len(), 2);
        assert!(
            rows(Some(serde_json::json!({"data": [], "total": 0})))
                .expect("an empty registry is an answer")
                .is_empty()
        );
    }

    /// Everything that is not a list refuses — no body (what `crate::sor_client::api`
    /// answers for a 200 that is not JSON), an error envelope, a `data`
    /// that is not an array — and the refusal quotes the body, CUT, so
    /// a login page does not bury the line that says what failed
    /// (backlog 7b7e0529).
    #[test]
    fn rows_refuses_what_is_not_a_list_and_quotes_it_short() {
        for body in [
            None,
            Some(Value::Null),
            Some(serde_json::json!({})),
            Some(serde_json::json!({"error": "forbidden"})),
            Some(serde_json::json!({"data": null})),
            Some(serde_json::json!({"data": {"id": "x"}})),
            Some(serde_json::json!("<html>sign in</html>")),
        ] {
            let why = rows(body.clone())
                .expect_err(&format!("{body:?} must refuse, not read as zero rows"))
                .to_string();
            assert!(why.contains("cannot be read as zero"), "{why}");
        }
        let page = "x".repeat(10_000);
        let why = rows(Some(Value::String(page))).unwrap_err().to_string();
        assert!(
            why.len() < 600,
            "a page is quoted short: {} chars",
            why.len()
        );
        assert!(why.ends_with('…'), "and says it was cut: {why}");
    }

    /// 2026-09-04: two gate-runs whose pods were evicted sat at
    /// `record-verdict` for 17 hours, each holding one of three gate
    /// slots and rendering as a live gate, while their branches had long
    /// since landed. A third died the same way and silently ate a car —
    /// the change was never gated and nobody noticed until a census.
    /// gate-runner.yaml already promised "a runner that dies anyway
    /// leaves an overdue packet"; nothing was listening.
    #[test]
    fn a_gate_run_past_the_job_deadline_is_dead() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let run = |opened: &str, verdict_done: bool| {
            serde_json::json!({
                "id": "11111111-1111-1111-1111-111111111111",
                "metadata": { "branch": "feat/x", "opened_at": opened },
                "steps": [{
                    "spec_slug": "record-verdict",
                    "title": "Record the gate verdict",
                    "status": if verdict_done { "completed" } else { "ready" },
                    "metadata": {}
                }]
            })
        };
        // 17h with no verdict: dead, and it reports how long.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", false), now),
            Some(17)
        );
        // Inside the deadline it is simply a gate that is running.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-04T11:30:00Z", false), now),
            None
        );
        // A run that REPORTED is never ours to touch, however old.
        assert_eq!(
            dead_gate_run_hours(&run("2026-09-03T20:00:00Z", true), now),
            None
        );
        // No stamp, no claim — settling on a guess would write a verdict
        // nobody observed into the audit log.
        let unstamped = serde_json::json!({
            "id": "22222222-2222-2222-2222-222222222222",
            "metadata": { "branch": "feat/y" },
            "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict","status":"ready","metadata":{}}]
        });
        assert_eq!(dead_gate_run_hours(&unstamped, now), None);
    }

    /// Backlog 95d81e50 (and item 4 of b24e29cb). The Job deadline runs
    /// from the Job's START, and a REUSED packet's `opened_at` belongs to
    /// the run that filed it: a packet opened four hours ago and relaunched
    /// thirty minutes ago carries a Job with two and a half hours left.
    /// Read from `opened_at` alone, the clock settled it `lost` under that
    /// running Job. The clock now reads the same signs of life the orphan
    /// judgement reads, and measures from the newest.
    #[test]
    fn the_job_deadline_is_measured_from_the_newest_sign_of_life() {
        let now = at("2026-09-24T13:00:00Z");
        let run = |extra: Value| {
            let mut md = serde_json::json!({ "branch": "feat/x",
                                             "opened_at": "2026-09-24T09:00:00Z" });
            for (k, v) in extra.as_object().cloned().unwrap_or_default() {
                md[k.as_str()] = v;
            }
            serde_json::json!({
                "id": "55555555-5555-5555-5555-555555555555",
                "metadata": md,
                "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict",
                           "status":"ready","metadata":{}}]
            })
        };
        // The control: filed four hours ago and nothing since — dead.
        assert_eq!(
            dead_gate_run_hours(&run(serde_json::json!({})), now),
            Some(4)
        );
        // Relaunched thirty minutes ago: its Job is inside its deadline.
        let relaunched = crate::gate::launching_patch(at("2026-09-24T12:30:00Z"));
        assert_eq!(dead_gate_run_hours(&run(relaunched), now), None);
        // Held in the queue until an hour ago, then launched: the same.
        assert_eq!(
            dead_gate_run_hours(
                &run(serde_json::json!({ "queue_heartbeat_at": "2026-09-24T12:00:00Z" })),
                now
            ),
            None
        );
        // A relaunch that is itself past the deadline is dead, and the
        // hours reported are counted from it.
        let old_launch = crate::gate::launching_patch(at("2026-09-24T09:40:00Z"));
        assert_eq!(dead_gate_run_hours(&run(old_launch), now), Some(3));
    }

    /// Backlog d1ea4141. Gate-run 98b80bb0 as it stood at 18:39:01Z on
    /// 2026-10-17, when the clock settled it `lost`: filed 15:38:21,
    /// queued 15:38:14, its top-level beat frozen at 15:38:14 for the
    /// whole 173-minute wait, its Job created at 18:31:57. Two signs of
    /// life the clock did not read: the waiter's beat inside the
    /// reservation row, which a writer may stamp without the top-level
    /// copy, and `launched_at`, the stamp `boss gate` writes once the Job
    /// exists. Either says the Job's deadline has not begun to run out.
    #[test]
    fn the_job_deadline_reads_the_reservation_beat_and_the_launch() {
        let now = at("2026-10-17T18:39:01Z");
        let run = |extra: Value| {
            let mut md = serde_json::json!({ "branch": "fix/a-died-runs-own-outcome-is-not-movement",
                                             "opened_at": "2026-10-17T15:38:21.398799458+00:00",
                                             "queued_at": "2026-10-17T15:38:14Z",
                                             "queue_heartbeat_at": "2026-10-17T15:38:14Z" });
            for (k, v) in extra.as_object().cloned().unwrap_or_default() {
                md[k.as_str()] = v;
            }
            serde_json::json!({
                "id": "98b80bb0-e20b-42c8-a02a-626cb379e05a",
                "metadata": md,
                "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict",
                           "status":"ready","metadata":{}}]
            })
        };
        // The control, the packet exactly as it was: three hours dead.
        assert_eq!(
            dead_gate_run_hours(&run(serde_json::json!({})), now),
            Some(3)
        );
        // The live waiter's beat, written only in the reservation row —
        // serialised from boss-jobs' own row type, never spelled here.
        // A literal row repeated `RESERVATION_BEAT`'s field name, so a
        // rename of the field in boss-jobs left this green while the
        // clock read no beat and counted the waiter dead (backlog
        // 54a6832b; measured: `#[serde(rename = "beat_at")]` on the field
        // kept the literal fixture green).
        let row = boss_jobs::gate_reservation::Reservation {
            contract_version: 1,
            generation: uuid::Uuid::parse_str("f4ef2fcb-ba92-44e6-b3fd-74a624c3abd6").unwrap(),
            queued_at: "2026-10-17T15:38:14Z".into(),
            heartbeat_at: "2026-10-17T18:31:30Z".into(),
            consumed: false,
        };
        let beating = serde_json::json!({ boss_jobs::gate_reservation::KEY: row });
        assert_eq!(dead_gate_run_hours(&run(beating.clone()), now), None);
        // And the clock dates the run FROM that beat, by the dotted path.
        let o = orphaned_gate_run(&run(beating), at("2026-10-17T18:51:30Z"))
            .expect("20 min past the row's beat is past the window");
        assert_eq!(
            (o.last_alive_key, o.last_alive.as_str(), o.idle_minutes),
            (RESERVATION_BEAT, "2026-10-17T18:31:30Z", 20)
        );
        // The launch: the Job exists as of 18:31:57.
        let launched = crate::gate::launched_patch(at("2026-10-17T18:31:57Z"));
        assert_eq!(dead_gate_run_hours(&run(launched), now), None);
        // And the orphan window names the stamp it dated the run from.
        let launched = crate::gate::launched_patch(at("2026-10-17T18:00:00Z"));
        let o = orphaned_gate_run(&run(launched), now).expect("39 min idle is past the window");
        assert_eq!(
            (o.last_alive_key, o.idle_minutes),
            (crate::gate::LAUNCHED_AT, 39)
        );
    }

    /// Backlog b56bfc29. Resident gate-run dec70bf9 as it stood at
    /// 06:50Z on 2026-10-19: a durable submission accepted at 05:42:37Z,
    /// waiting for a bay, its client stamps frozen at 05:41:50Z because a
    /// resident place carries no client heartbeat. No Job had ever been
    /// created, so no Job deadline could have passed — yet the clock read
    /// only the stamps and would have settled it `lost` at 08:42Z with a
    /// receipt naming a Job that never existed. Once a native Job is
    /// observed the run is a launched gate again, and the clock is its.
    #[test]
    fn a_queued_resident_place_is_not_a_dead_gate() {
        let now = at("2026-10-19T09:45:00Z");
        let packet = "dec70bf9-5c78-4f1b-a706-7caf438ab26a";
        let intent = serde_json::json!({
            "contract_version": 1, "packet": packet, "namespace": "boss-dev",
            "name": "gate-dec70bf95c784f1ba7067caf438ab26a",
            "head": "097e370a27a0c773cf1ce015e242215ee200f8ec", "mode": "--auto",
            "file_id": "6c5f68b5-5bfc-4ca9-8a9d-2d92872fe5ba",
            "sha256": "deab6e4dad06ad2a73f7e08585305e8b04385142583300d985feace96e815078",
            "size_bytes": 5537 });
        let branch = "fix/a-gate-place-belongs-to-its-packet-by-default";
        let sha = "097e370a27a0c773cf1ce015e242215ee200f8ec";
        let submission = serde_json::json!({
            "contract_version": 1, "command": "3690ab16-f07e-441a-a404-055ab4e4e8a1",
            "submitted_by": "agent-claude-b", "supervisor": "automation:train-conductor",
            "intent": intent, "source": { "branch": branch, "sha": sha } });
        let md = serde_json::json!({
            "branch": branch, "sha": sha,
            "opened_at": "2026-10-19T05:42:33.154541701+00:00",
            "queued_at": "2026-10-19T05:41:50Z",
            "queue_heartbeat_at": "2026-10-19T05:41:50Z",
            boss_jobs::gate_reservation::KEY: {
                "contract_version": 1, "generation": "974ba91e-5160-47f6-b638-be587ce10c1e",
                "queued_at": "2026-10-19T05:41:50Z", "heartbeat_at": "2026-10-19T05:41:50Z",
                "consumed": false },
            boss_jobs::gate_reservation::SUBMISSION: submission,
            boss_jobs::metadata_immutability::KEY: {
                boss_jobs::gate_reservation::SUBMISSION: submission } });
        let run = |md: Value| {
            serde_json::json!({
                "id": packet,
                "metadata": md,
                "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict",
                           "status":"ready","metadata":{}}]
            })
        };
        assert!(boss_jobs::gate_reservation::waiting_for_native(&md));
        assert_eq!(
            dead_gate_run_hours(&run(md.clone()), now),
            None,
            "a resident place still waiting for a bay has no Job to have died"
        );
        // The control: the same stamps under legacy ownership are four
        // hours of silence, and the clock still owns them.
        let mut legacy = md.clone();
        for key in [
            boss_jobs::gate_reservation::SUBMISSION,
            boss_jobs::metadata_immutability::KEY,
        ] {
            legacy.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(dead_gate_run_hours(&run(legacy), now), Some(4));
        // Launched: the protected native observation binds the Job, and
        // from then on the Job deadline is the clock again.
        let observation = serde_json::json!({
            "uid": "6f0c1e2a-0000-4000-8000-000000000001",
            "request_sha256": intent["sha256"], "created_at": "2026-10-19T05:44:00Z" });
        let mut launched = md;
        launched[boss_jobs::gate_reservation::INTENT] = intent.clone();
        launched[boss_jobs::gate_reservation::OBSERVATION] = observation.clone();
        launched[boss_jobs::metadata_immutability::KEY][boss_jobs::gate_reservation::INTENT] =
            intent;
        launched[boss_jobs::metadata_immutability::KEY][boss_jobs::gate_reservation::OBSERVATION] =
            observation;
        assert!(boss_jobs::gate_reservation::native_observed(&launched));
        assert_eq!(dead_gate_run_hours(&run(launched.clone()), now), Some(4));
        // A launch begun and not yet observed may have a Job: the guarded
        // attempt alone hands the run back to the clock.
        let mut sending = launched;
        for key in [
            boss_jobs::gate_reservation::INTENT,
            boss_jobs::gate_reservation::OBSERVATION,
        ] {
            sending.as_object_mut().unwrap().remove(key);
            sending[boss_jobs::metadata_immutability::KEY]
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        sending[boss_jobs::gate_reservation::ATTEMPT] = serde_json::json!({ "state": "sent" });
        assert!(boss_jobs::gate_reservation::waiting_for_native(&sending));
        assert_eq!(dead_gate_run_hours(&run(sending), now), Some(4));
    }

    /// The same item's receipt half: a clock settle that found no gate
    /// Job for the packet claims no Job deadline, and one that found a
    /// finished Job still names the deadline that ended it.
    #[test]
    fn a_lost_receipt_claims_a_job_deadline_only_when_a_job_was_seen() {
        let none = clock_lost_receipt(4, "fix/x", 0);
        assert!(!none.contains("activeDeadlineSeconds"), "{none}");
        assert!(none.contains("NO GATE JOB CARRIES THIS RUN"), "{none}");
        assert!(none.contains("Quiet 4h"), "{none}");
        let seen = clock_lost_receipt(4, "fix/x", 1);
        assert!(
            seen.contains(&format!(
                "Active 4h with none, past the gate Job's {GATE_DEADLINE_HOURS}h \
                 activeDeadlineSeconds"
            )),
            "{seen}"
        );
        for receipt in [none, seen] {
            assert!(receipt.contains("this run says nothing about fix/x"));
        }
    }

    fn gate_job(name: &str, created: &str, finished: bool) -> GateJob {
        GateJob {
            name: name.into(),
            uid: "uid".into(),
            packet: "98b80bb0-e20b-42c8-a02a-626cb379e05a".into(),
            created: created.into(),
            carrier: true,
            judged_init: false,
            finished: finished.then(|| Finished {
                failed: true,
                reason: "DeadlineExceeded".into(),
                at: "2026-10-17T21:31:57Z".into(),
            }),
        }
    }

    /// Backlog d1ea4141, acceptance 2: a gate whose Job is running is
    /// never settled `lost` on a clock dated by the queue. The Job's
    /// `activeDeadlineSeconds` runs from the Job, so the cluster's own
    /// record of it bounds the deadline; the packet's stamps only stand
    /// in for it when no Job exists.
    #[test]
    fn a_gate_job_the_cluster_holds_bounds_the_deadline() {
        let now = at("2026-10-17T18:39:01Z");
        // No Job: the clock's own reading stands.
        assert_eq!(job_deadline(3, &[], now), Ok(3));
        // 98b80bb0's Job, created seven minutes earlier and running.
        let running = gate_job("gate-98b80bb0", "2026-10-17T18:31:57Z", false);
        let held = job_deadline(3, &[&running], now).unwrap_err();
        assert!(
            held.contains("gate-98b80bb0") && held.contains("running"),
            "{held}"
        );
        // Running past its own deadline is still running: Kubernetes, not
        // this clock, ends it, and its end is the cluster's to record.
        let late = gate_job("gate-98b80bb0", "2026-10-17T15:00:00Z", false);
        assert!(job_deadline(3, &[&late], now).is_err());
        // Ended inside the deadline: the recorder reads what it left.
        let ended = gate_job("gate-98b80bb0", "2026-10-17T18:31:57Z", true);
        assert!(job_deadline(3, &[&ended], now).is_err());
        // Ended, and created past the deadline: dead, dated from the Job.
        let old = gate_job("gate-98b80bb0", "2026-10-17T14:00:00Z", true);
        assert_eq!(job_deadline(5, &[&old], now), Ok(4));
        // A Job whose creation cannot be read cannot be dated: held.
        let blank = gate_job("gate-98b80bb0", "", true);
        assert!(job_deadline(3, &[&old, &blank], now).is_err());
    }

    /// Backlog db6bdb4f (landed-review run 7dab7d38 of car b670d6b3). A
    /// gate Job whose pod was never created — admission or quota refused
    /// it for the whole 10800 s — ends `Failed` / `DeadlineExceeded` with
    /// NO pod count: `failed: 0`. Read from the counts alone it was still
    /// running, so the clock above held its run open until the Job's
    /// 86400 s TTL deleted it, about a day late, logging "is still
    /// running" every pass. The cluster's terminal condition is the end.
    #[test]
    fn a_gate_job_that_failed_before_any_pod_existed_is_finished_and_settles_on_the_clock() {
        let now = at("2026-10-17T18:39:01Z");
        let list = |status: Value| {
            json!({"items": [{"metadata": {"name": "gate-podless-11111", "uid": "u",
                "creationTimestamp": "2026-10-17T14:00:00Z",
                "labels": {"app": "gate-runner", "boss.dev/packet": "p",
                           "boss.dev/verdict-carrier": "pod-log"}},
                "status": status}]})
        };
        let podless = gate_jobs_from_json(&list(json!({"failed": 0, "conditions": [
            {"type": "FailureTarget", "status": "True", "reason": "DeadlineExceeded",
             "lastTransitionTime": "2026-10-17T17:00:00Z"},
            {"type": "Failed", "status": "True", "reason": "DeadlineExceeded",
             "lastTransitionTime": "2026-10-17T17:00:01Z"}]})));
        assert_eq!(
            podless[0].finished,
            Some(Finished {
                failed: true,
                reason: "DeadlineExceeded".into(),
                at: "2026-10-17T17:00:01Z".into(),
            }),
            "a Job the cluster ended Failed with no pod count read as running"
        );
        assert_eq!(job_deadline(5, &[&podless[0]], now), Ok(4));
        // A condition that is not True is no end, and neither is one the
        // Job holds while a pod is still terminating: the bay's reading.
        for live in [
            json!({"conditions": [{"type": "Failed", "status": "False"}]}),
            json!({"terminating": 1, "conditions": [{"type": "Failed", "status": "True"}]}),
            json!({}),
        ] {
            let jobs = gate_jobs_from_json(&list(live.clone()));
            assert_eq!(jobs[0].finished, None, "{live} read as finished");
            assert!(job_deadline(5, &[&jobs[0]], now).is_err(), "{live}");
        }
    }

    /// CLAUDE.md §9a: the bay ([`crate::gate::job_live`]) and the recorder
    /// read one Job. Every Job the bay calls ended, the recorder calls
    /// finished — the two definitions of an ended gate Job are one.
    #[test]
    fn every_job_the_bay_calls_ended_the_recorder_calls_finished() {
        for status in [
            json!({"succeeded": 1, "conditions": [{"type": "Complete", "status": "True"}]}),
            json!({"failed": 1, "conditions": [{"type": "Failed", "status": "True"}]}),
            json!({"conditions": [{"type": "Failed", "status": "True"}]}),
            json!({"conditions": [{"type": "Complete", "status": "True"}]}),
            json!({"active": 1}),
            json!({"conditions": [{"type": "Failed", "status": "False"}]}),
            json!({"terminating": 1, "conditions": [{"type": "Failed", "status": "True"}]}),
            json!({}),
        ] {
            let job = json!({"metadata": {"name": "gate-x", "labels":
                {"app": "gate-runner", "boss.dev/packet": "p"}}, "status": status});
            let finished = gate_jobs_from_json(&json!({"items": [job.clone()]}))[0]
                .finished
                .is_some();
            if !crate::gate::job_live(&job) {
                assert!(
                    finished,
                    "the bay frees {status} but the recorder waits on it"
                );
            }
        }
    }

    /// Gate-run 91594262 as it stood on 2026-09-24: queued at 01:26:24,
    /// filed 01:26:33, its waiter's last queue beat at 01:26:54 — then
    /// the dev pod was evicted at 01:27 and nothing ever launched a Job.
    /// The verdict step's `heartbeat_at` is the registry's empty default,
    /// and nothing ever writes it (run.sh does not heartbeat).
    fn stranded_run(verdict_status: &str, heartbeat_at: &str) -> Value {
        serde_json::json!({
            "id": "91594262-4f4a-4aee-988e-4c7d2c492de7",
            "metadata": {
                "branch": "fix/boss-prove-and-the-observer-settle-write-through-the-step-merge-door",
                "opened_at": "2026-09-24T01:26:33.216272518+00:00",
                "queued_at": "2026-09-24T01:26:24Z",
                "queue_heartbeat_at": "2026-09-24T01:26:54Z"
            },
            "steps": [{
                "spec_slug": "record-verdict",
                "title": "Record the gate verdict",
                "status": verdict_status,
                "metadata": { "heartbeat_at": heartbeat_at }
            }]
        })
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Backlog 137c176d. The run above held its builder's agent-run for
    /// three hours — the dead-host pass defers to an open gate-run, and
    /// the only thing that closed this one was the 3h clock, at 04:30Z.
    /// Everything that could ever have said it was alive had stopped by
    /// 01:26:54. It is judged from THAT instant, so it is a candidate a
    /// window later rather than three hours later.
    #[test]
    fn a_gate_run_nothing_has_held_for_the_window_is_an_orphan_candidate() {
        let run = stranded_run("ready", "");
        // Thirteen minutes after the last beat: still inside the window.
        assert_eq!(orphaned_gate_run(&run, at("2026-09-24T01:40:00Z")), None);
        // Past it: a candidate, dated from the waiter's last beat — the
        // LATEST sign of life, not the filing.
        let o = orphaned_gate_run(&run, at("2026-09-24T01:45:00Z"))
            .expect("nothing has held it for the window");
        assert_eq!(o.last_alive_key, "queue_heartbeat_at");
        assert_eq!(o.last_alive, "2026-09-24T01:26:54Z");
        assert_eq!(o.idle_minutes, 18);
        assert_eq!(o.queued_at.as_deref(), Some("2026-09-24T01:26:24Z"));
        // A run that REPORTED is never ours to touch.
        assert_eq!(
            orphaned_gate_run(&stranded_run("completed", ""), at("2026-09-24T04:00:00Z")),
            None
        );
    }

    /// NOTHING WRITES THE VERDICT STEP'S `heartbeat_at` (review of car
    /// 2ca8c7e9, 2026-09-25): gate-run.toml defaults it to "" and run.sh
    /// never stamps it, so a rule keyed on it being empty was a rule that
    /// was always true — and a receipt that said so claimed a measurement
    /// nobody took. The runner lives INSIDE the Job, so the cluster read
    /// already answers what a heartbeat would: a run whose runner is
    /// alive has a Job. The field is therefore not read at all, and a
    /// value in it changes nothing.
    #[test]
    fn the_verdict_steps_heartbeat_at_is_not_read() {
        let now = at("2026-09-24T01:45:00Z");
        assert_eq!(
            orphaned_gate_run(&stranded_run("ready", "2026-09-24T01:30:00Z"), now),
            orphaned_gate_run(&stranded_run("ready", ""), now),
        );
        let o = orphaned_gate_run(&stranded_run("ready", ""), now).unwrap();
        let ev = orphan_evidence(&o, "91594262-4f4a-4aee-988e-4c7d2c492de7", "boss-dev", now);
        assert!(
            ev.get("heartbeat_at").is_none(),
            "the evidence must not carry a field nothing writes: {ev}"
        );
        let receipt = orphan_receipt(&o, "fix/x", "boss-dev");
        // (`queue_heartbeat_at` is a different stamp, and legitimately
        // named when it was the last sign of life.)
        assert!(
            !receipt.contains("step's heartbeat_at"),
            "the receipt must not claim the verdict step's heartbeat was read: {receipt}"
        );
    }

    /// A REUSED PACKET IS STAMPED ALIVE AT LAUNCH (review of car 2ca8c7e9,
    /// 2026-09-25). `boss gate` reuses an open packet for the same branch
    /// and head — one filed an hour ago, whose waiter died — and then
    /// spends up to minutes rebasing, judging and admitting before
    /// `kubectl create`. Dated only from its old stamps, that packet is an
    /// orphan the whole time; a reconcile landing in the gap would settle
    /// it `lost` with its Job seconds from existing. The launch writes
    /// `launching_at`, and the judgement reads it like any other sign of
    /// life.
    #[test]
    fn a_reused_packet_stamped_at_launch_is_not_an_orphan() {
        let mut run = stranded_run("ready", "");
        let launch = crate::gate::launching_patch(at("2026-09-24T03:00:00Z"));
        for (k, v) in launch.as_object().expect("a metadata patch is an object") {
            run["metadata"][k] = v.clone();
        }
        // Minutes after the relaunch — two hours after the old beat.
        assert_eq!(orphaned_gate_run(&run, at("2026-09-24T03:05:00Z")), None);
        // A launch that then never made a Job is an orphan in its turn,
        // dated from the launch.
        let o = orphaned_gate_run(&run, at("2026-09-24T03:16:00Z")).expect("16 min, no Job");
        assert_eq!(o.last_alive_key, crate::gate::LAUNCHING_AT);
        assert_eq!(o.last_alive, "2026-09-24T03:00:00Z");
        assert_eq!(o.idle_minutes, 16);
    }

    /// A run launched straight onto a free slot has no queue keys; it is
    /// dated from `opened_at`. A stamp that does not parse is no stamp —
    /// never a reason to call a run older than it is, and no `opened_at`
    /// at all is no claim (the `dead_gate_run_hours` rule).
    #[test]
    fn an_orphan_is_dated_from_its_latest_readable_stamp() {
        let launched = serde_json::json!({
            "id": "33333333-3333-3333-3333-333333333333",
            "metadata": { "branch": "feat/z", "opened_at": "2026-09-24T10:00:00Z",
                          "queue_heartbeat_at": "not a time" },
            "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict",
                       "status":"ready","metadata":{"heartbeat_at":""}}]
        });
        let o = orphaned_gate_run(&launched, at("2026-09-24T10:20:00Z")).expect("20m, no Job");
        assert_eq!(o.last_alive_key, "opened_at");
        assert_eq!(o.idle_minutes, 20);
        assert_eq!(o.queued_at, None);
        assert_eq!(
            orphaned_gate_run(&launched, at("2026-09-24T10:05:00Z")),
            None
        );
        let unstamped = serde_json::json!({
            "id": "44444444-4444-4444-4444-444444444444",
            "metadata": { "branch": "feat/w", "queue_heartbeat_at": "2026-09-24T01:00:00Z" },
            "steps": [{"spec_slug":"record-verdict","title":"Record the gate verdict",
                       "status":"ready","metadata":{}}]
        });
        assert_eq!(
            orphaned_gate_run(&unstamped, at("2026-09-24T04:00:00Z")),
            None
        );
    }

    /// The evidence rides the packet as data, not only as prose: the Job
    /// read that came back empty (namespace + selector + count) and the
    /// last sign of life, beside the instant it was judged.
    #[test]
    fn an_orphan_settle_records_the_absent_job_and_the_last_sign_of_life() {
        let now = at("2026-09-24T01:45:00Z");
        let run = stranded_run("ready", "");
        let o = orphaned_gate_run(&run, now).unwrap();
        let ev = orphan_evidence(&o, "91594262-4f4a-4aee-988e-4c7d2c492de7", "boss-dev", now);
        assert_eq!(ev["runner_jobs"], 0);
        assert_eq!(ev["namespace"], "boss-dev");
        assert_eq!(
            ev["selector"],
            "boss.dev/packet=91594262-4f4a-4aee-988e-4c7d2c492de7"
        );
        assert_eq!(ev["last_alive_key"], "queue_heartbeat_at");
        assert_eq!(ev["last_alive"], "2026-09-24T01:26:54Z");
        assert_eq!(ev["idle_minutes"], 18);
        assert_eq!(ev["queued_at"], "2026-09-24T01:26:24Z");
        assert_eq!(ev["observed_at"], "2026-09-24T01:45:00Z");
        let receipt = orphan_receipt(&o, "fix/x", "boss-dev");
        for needle in [
            "NO VERDICT WAS PRODUCED",
            "no gate Job",
            "boss-dev",
            "queue_heartbeat_at 2026-09-24T01:26:54Z",
            "18 min",
            "fix/x",
            "--park-",
        ] {
            assert!(
                receipt.contains(needle),
                "{needle:?} missing from: {receipt}"
            );
        }
    }

    // -- the conductor reads all its cars, not just page one -----------

    #[test]
    fn next_offset_pages_past_the_first_hundred() {
        // A backlog under one page needs no second read.
        assert_eq!(next_offset(0, 0), None);
        assert_eq!(next_offset(42, 42), None);
        assert_eq!(next_offset(PAGE_LIMIT, PAGE_LIMIT), None);
        // 150 open cars, 100 read: page two starts at offset 100.
        assert_eq!(next_offset(150, 100), Some(100));
        // page two read: the whole backlog is covered.
        assert_eq!(next_offset(150, 150), None);
        // defensive — a `total` that shrank mid-read never asks for more.
        assert_eq!(next_offset(150, 160), None);
    }

    /// A SHORT PAGE IS NOT THE REGISTRY (backlog 6cf47547). A read that
    /// must hold every row — a tenant export, which rewrites the repo's
    /// seed files from it — refuses a body whose `total` counts more
    /// rows than it carries, rather than reading one page as the whole.
    #[test]
    fn every_row_refuses_a_short_page_and_reads_a_whole_one() {
        let short = json!({"data": [{"id": "a"}], "total": 2});
        let why = every_row(Some(short))
            .expect_err("1 of 2 is a page")
            .to_string();
        assert!(why.contains("1 of 2"), "names the shortfall: {why}");

        let whole = json!({"data": [{"id": "a"}, {"id": "b"}], "total": 2});
        assert_eq!(every_row(Some(whole)).unwrap().len(), 2);
        // A bare array, and an envelope without `total`, carry no count
        // to compare against — read as they always were.
        assert_eq!(every_row(Some(json!([{"id": "a"}]))).unwrap().len(), 1);
        assert_eq!(every_row(Some(json!({"data": []}))).unwrap().len(), 0);
        // Still the one rows helper underneath: a non-list refuses.
        assert!(every_row(Some(json!({"data": "nope", "total": 0}))).is_err());
    }

    #[test]
    fn list_total_reads_total_not_the_page() {
        let body = json!({"data": [{"id": "car-1"}], "limit": 100, "offset": 0, "total": 150});
        assert_eq!(list_total(&body).unwrap(), 150);
        // no `total` is an error, never zero: zero is a wrong deployment.
        assert!(list_total(&json!({"data": []})).is_err());
    }

    #[test]
    fn list_total_invalid_field_describes_type_without_echoing_payload() {
        for (total, kind) in [
            (Value::Null, "null"),
            (json!(true), "boolean"),
            (json!(-1), "outside the unsigned integer range"),
            (json!(1.5), "outside the unsigned integer range"),
            (json!("x".repeat(MAX_INVENTORY_BYTES)), "string"),
            (json!([]), "array"),
            (json!({}), "object"),
        ] {
            let error =
                list_total(&json!({"total":total,"data":[],"other":"private unrelated field"}))
                    .unwrap_err()
                    .to_string();
            assert_eq!(
                error,
                format!("list response `total` is {kind}; expected an unsigned integer")
            );
        }
        assert_eq!(
            list_total(&json!({"data":[],"other":"x".repeat(MAX_INVENTORY_BYTES)}))
                .unwrap_err()
                .to_string(),
            "list response `total` is missing; expected an unsigned integer"
        );
        assert_eq!(list_total(&json!({"total":0,"data":[]})).unwrap(), 0);
        assert_eq!(list_total(&json!({"total":123,"data":[]})).unwrap(), 123);
    }

    #[tokio::test]
    async fn original_attempt_census_requires_complete_stable_unique_pages() {
        let full: Vec<Value> = (0..101)
            .map(|id| json!({"id":format!("gate-{id}")}))
            .collect();
        let all = &full;
        let gathered = list_original_attempts(|offset| async move {
            Ok(Some(json!({"total":all.len(), "data":all.iter().skip(offset).take(PAGE_LIMIT).collect::<Vec<_>>()})))
        }).await.unwrap();
        assert_eq!(gathered, full);
        assert!(
            list_original_attempts(|_| async { Ok(Some(json!({"total":0,"data":[]}))) })
                .await
                .unwrap()
                .is_empty()
        );
        for body in [
            json!({"total":1,"data":[]}),
            json!({"total":0,"data":[{"id":"extra"}]}),
            json!({"total":1,"data":[{}]}),
            json!({"data":[]}),
        ] {
            assert!(
                list_original_attempts(|_| {
                    let body = body.clone();
                    async move { Ok(Some(body)) }
                })
                .await
                .is_err()
            );
        }
        for tail in [
            json!({"total":101,"data":[]}),
            json!({"total":101,"data":[{"id":"gate-0"}]}),
            json!({"total":102,"data":[{"id":"gate-100"}]}),
            json!({"total":100,"data":[]}),
        ] {
            let head = &full;
            assert!(
                list_original_attempts(|offset| {
                    let body = if offset == 0 {
                        json!({"total":101,"data":&head[..100]})
                    } else {
                        tail.clone()
                    };
                    async move { Ok(Some(body)) }
                })
                .await
                .is_err()
            );
        }
        assert!(
            list_original_attempts(|_| async { anyhow::bail!("unreadable original page") })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn original_attempt_census_refuses_aggregate_bytes_across_small_pages() {
        let all: Vec<Value> = (0..200)
            .map(|id| json!({"id":format!("gate-{id}"),"padding":"x".repeat(100_000)}))
            .collect();
        let rows = &all;
        let result=list_original_attempts(|offset|async move {
            Ok(Some(json!({"total":rows.len(),"data":rows.iter().skip(offset).take(PAGE_LIMIT).collect::<Vec<_>>()})))
        }).await;
        assert!(
            result.is_err(),
            "two individually bounded pages must not create an unbounded inventory"
        );
    }

    /// THE STARVATION REGRESSION. A parked-ready car opened days ago
    /// sorts to the tail of `opened_on DESC`; with 150 open cars it sits
    /// on page two (offset 100). The old bare `limit=100` read left it
    /// off page one forever. `list_all_pages` — the read every whole-dock
    /// caller now shares (candidates, open_car_branches, preview_dock,
    /// probe_dock_depth) — must gather it.
    #[tokio::test]
    async fn list_all_pages_reads_the_car_on_page_two() {
        let all: Vec<Value> = (0..150)
            .map(|i| json!({"id": format!("car-{i}")}))
            .collect();
        let all_ref = &all;
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                let page: Vec<Value> = all_ref
                    .iter()
                    .skip(offset)
                    .take(PAGE_LIMIT)
                    .cloned()
                    .collect();
                anyhow::Ok(Some(json!({
                    "data": page,
                    "total": all_ref.len(),
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 150, "every open car must be read");
        assert!(
            gathered.iter().any(|j| j["id"] == "car-149"),
            "the car on page two must be gathered, not left off page one"
        );
        assert_eq!(calls.get(), 2, "150 cars is two pages of 100");
    }

    /// A backlog that fits under a page still makes exactly one call —
    /// the fix is paging PAST a page, never changing behaviour below it.
    #[tokio::test]
    async fn list_all_pages_makes_one_call_below_a_page() {
        let calls = std::cell::Cell::new(0u32);
        let gathered = list_all_pages(|offset| {
            calls.set(calls.get() + 1);
            async move {
                assert_eq!(offset, 0, "a sub-page backlog never asks for page two");
                anyhow::Ok(Some(json!({
                    "data": (0..42).map(|i| json!({"id": i})).collect::<Vec<_>>(),
                    "total": 42,
                    "offset": offset,
                    "limit": PAGE_LIMIT,
                })))
            }
        })
        .await
        .unwrap();
        assert_eq!(gathered.len(), 42);
        assert_eq!(calls.get(), 1);
    }

    // -- metadata merges through the server's merge door -------------------
    //
    // Design 38f3a488 D4 (backlog b15b0f4e): the conductor merged by GET,
    // overlay and full job PUT, so a key another writer set between the
    // read and the write was erased. The server's PATCH merges against
    // the row as it stands, in one transaction; the body names only the
    // keys this write changes, and a null deletes one.

    #[test]
    fn a_metadata_merge_is_one_patch_through_the_merge_door() {
        let (method, path, body) = job_metadata_patch("j-1", vec![("skip_reason", json!("x"))]);
        assert_eq!(method, Method::PATCH);
        assert_eq!(path, "/api/jobs/j-1/metadata");
        assert_eq!(body, json!({"skip_reason": "x"}));
    }

    /// Backlog 5e8b3f26: moving the merge from a full PUT to the merge
    /// door took it out of the retry, because the classifier judged by
    /// METHOD and PATCH is not idempotent in general. The two merge
    /// doors are, by their contract — setting a key twice, or deleting
    /// it twice, lands the same row — so a blip on one is re-sent
    /// exactly as the PUT it replaced was. Any other PATCH is not.
    #[test]
    fn a_merge_door_patch_is_retried_on_a_blip_and_no_other_patch_is() {
        let (method, job_door, _) = job_metadata_patch("j-1", vec![("skip_reason", Value::Null)]);
        let step_door = "/api/jobs/j-1/steps/s-1/metadata";
        let blips = [
            Failure::Ambiguous,
            Failure::Http(500),
            Failure::Http(503),
            Failure::PolicyOutage,
        ];
        for failure in &blips {
            assert!(retryable(&method, &job_door, failure), "{failure:?}");
            assert!(retryable(&method, step_door, failure), "{failure:?}");
            for other in [
                "/api/jobs/j-1",
                "/api/jobs/j-1/steps/s-1",
                "/api/jobs/j-1/metadata/extra",
                "/api/jobs/j-1/steps/s-1/metadata/extra",
                "/api/jobs//metadata",
                "/api/jobs/j-1/steps//metadata",
                "/api/jobs/j-1/other/s-1/metadata",
                "/api/stations/x/metadata",
                "/api/v1/repos/o/r/pulls/7",
            ] {
                assert!(
                    !retryable(&Method::PATCH, other, failure),
                    "PATCH {other} is not a merge door: {failure:?}"
                );
            }
            // The door is idempotent to a merge, not to every method.
            assert!(!retryable(&Method::POST, &job_door, failure), "{failure:?}");
        }
        // An answer stays an answer at the merge door too.
        for failure in [Failure::Http(409), Failure::Http(422), Failure::Malformed] {
            assert!(!retryable(&method, &job_door, &failure), "{failure:?}");
        }
        // A query string does not hide the door.
        assert!(retryable(
            &method,
            "/api/jobs/j-1/metadata?x=1",
            &Failure::Ambiguous
        ));
    }

    /// The same decision end to end through the conductor's driver: a
    /// merge-door PATCH that times out is sent again and lands; a PATCH
    /// anywhere else is surfaced on its first attempt.
    #[tokio::test]
    async fn the_retry_driver_resends_a_merge_door_patch_and_no_other() {
        async fn drive(path: &str) -> (Result<u8>, u32) {
            let mut calls = 0u32;
            let lines = AtomicU32::new(0);
            let out = retrying(
                &RetryPolicy::immediate(3),
                &Method::PATCH,
                path,
                policy().blip_cause_budget,
                &counting_journal(&lines),
                || {
                    calls += 1;
                    let attempt = calls;
                    async move {
                        if attempt == 1 {
                            Err(blip(Failure::Ambiguous))
                        } else {
                            Ok(7)
                        }
                    }
                },
            )
            .await;
            (out, calls)
        }
        let (out, calls) = drive("/api/jobs/j-1/metadata").await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "the job merge door is re-sent once and lands");
        let (out, calls) = drive("/api/jobs/j-1/steps/s-1/metadata").await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "the step merge door is re-sent once and lands");
        let (out, calls) = drive("/api/jobs/j-1/steps/s-1").await;
        assert!(out.is_err(), "an ambiguous PATCH elsewhere is surfaced");
        assert_eq!(calls, 1, "and sent once");
    }

    #[test]
    fn a_metadata_merge_names_only_the_keys_it_changes() {
        // Boarding stamps `train` and sheds the stale skip note in one
        // write; the null is SENT, because the server deletes on it —
        // and no other key rides along to overwrite a concurrent writer.
        let (_, _, body) = job_metadata_patch(
            "j-1",
            vec![("train", json!("t-1")), ("skip_reason", Value::Null)],
        );
        assert_eq!(body, json!({"train": "t-1", "skip_reason": null}));
        assert!(body.get("branch").is_none());
    }

    /// f402a681: the report moved to the job when terminal steps became
    /// immutable, so the "already filed" check has to read the job too.
    /// Reading the step instead means every reconcile re-files a report
    /// it already wrote.
    #[test]
    fn arrival_filed_is_read_from_the_job_not_the_step() {
        let unfiled = json!({
            "metadata": {"boarded_jobs": []},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(!arrival_already_filed(&unfiled));

        let filed = json!({
            "metadata": {"arrival_report": {"consist": []}, "arrival_summary": "2 cars"},
            "steps": [{"title": "Train arrived", "status": "completed", "metadata": {}}],
        });
        assert!(arrival_already_filed(&filed));

        // A report on the STEP is the OLD location. It must not count as
        // filed, or trains that pre-date the move never get a job-level
        // report and their branch sweep stays blocked.
        let old_location = json!({
            "metadata": {},
            "steps": [{
                "title": "Train arrived",
                "status": "completed",
                "metadata": {"arrival_report": {"consist": []}},
            }],
        });
        assert!(
            !arrival_already_filed(&old_location),
            "a report on the step is not a report on the job"
        );
    }

    /// PATCH merges, and a null value DELETES a key — so a null report
    /// must read as "not filed" rather than as a filed one.
    #[test]
    fn a_null_report_is_not_filed() {
        let nulled = json!({"metadata": {"arrival_report": null}});
        assert!(!arrival_already_filed(&nulled));
    }

    // -- the jobs-API retry classifier -------------------------------------
    //
    // The cluster is the system of record and it rolls. Twice on
    // 2026-08-13 a reconcile hit `Connection refused` to the jobs API
    // mid-converge and failed the whole verb; the blip lasted seconds.
    // A bounded retry covers the roll — but only for failures that are
    // blips, and only where re-sending is safe.

    #[test]
    fn a_refused_connection_is_a_blip_under_any_method() {
        // Nothing was received, so nothing was done: even a create may
        // go again.
        assert!(retryable(&Method::GET, "/api/jobs/j-1", &Failure::Connect));
        assert!(retryable(&Method::PUT, "/api/jobs/j-1", &Failure::Connect));
        assert!(retryable(&Method::POST, "/api/jobs/j-1", &Failure::Connect));
    }

    #[test]
    fn an_ambiguous_blip_only_retries_an_idempotent_call() {
        // A timeout leaves the write UNKNOWN — re-POSTing an ambiguous
        // create is how one blip becomes two train Jobs.
        assert!(retryable(
            &Method::GET,
            "/api/jobs/j-1",
            &Failure::Ambiguous
        ));
        assert!(retryable(
            &Method::PUT,
            "/api/jobs/j-1",
            &Failure::Ambiguous
        ));
        assert!(!retryable(
            &Method::POST,
            "/api/jobs/j-1",
            &Failure::Ambiguous
        ));
    }

    #[test]
    fn a_5xx_is_a_blip_and_a_4xx_is_an_answer() {
        for status in [500, 502, 503, 504] {
            assert!(
                retryable(&Method::GET, "/api/jobs/j-1", &Failure::Http(status)),
                "{status} is the SoR failing to answer"
            );
            assert!(
                !retryable(&Method::POST, "/api/jobs/j-1", &Failure::Http(status)),
                "{status} leaves a create ambiguous"
            );
        }
        // A 422 is the jobs API telling the conductor no. Retrying an
        // answer just asks the same question three times — including
        // 429, which is an answer about rate, not a transport blip.
        for status in [400, 404, 409, 422, 429] {
            assert!(
                !retryable(&Method::GET, "/api/jobs/j-1", &Failure::Http(status)),
                "{status}"
            );
            assert!(
                !retryable(&Method::PUT, "/api/jobs/j-1", &Failure::Http(status)),
                "{status}"
            );
        }
        // 2xx/3xx never reach the classifier, and are not blips either.
        assert!(!retryable(
            &Method::GET,
            "/api/jobs/j-1",
            &Failure::Http(200)
        ));
        assert!(!retryable(
            &Method::GET,
            "/api/jobs/j-1",
            &Failure::Http(301)
        ));
    }

    /// Backlog 5d4ad086, 2026-09-25 ~21:35Z, the rollout of train #689:
    /// for about a minute every packet read answered this 403, because
    /// the jobs API's policy client fails CLOSED when the policy service
    /// is dark (boss-policy-client, D9). That is the infrastructure
    /// saying it cannot decide — not a permission fact — so it retries
    /// like a 5xx: an idempotent call only, inside the same budget.
    #[test]
    fn a_policy_outage_is_a_blip_and_a_scope_refusal_is_an_answer() {
        for body in [
            "reading packets is refused: policy-unreachable",
            "reading packets is refused: policy service returned 503 Service Unavailable",
            "reading packets is refused: policy service returned 502 Bad Gateway",
        ] {
            let kind = http_failure(403, body);
            assert_eq!(kind, Failure::PolicyOutage, "{body}");
            assert!(retryable(&Method::GET, "/api/jobs/j-1", &kind), "{body}");
            assert!(retryable(&Method::PUT, "/api/jobs/j-1", &kind), "{body}");
            assert!(
                !retryable(&Method::POST, "/api/jobs/j-1", &kind),
                "a create stays unretried on anything but a refused connect: {body}"
            );
        }
        // A policy ANSWER stays an answer: a scope refusal, and a 4xx
        // from the policy service (the jobs API asked it wrongly —
        // asking again asks wrongly again).
        for body in [
            "job is outside your scope",
            "reading packets is refused: not permitted",
            "reading packets is refused: policy service returned 400 Bad Request",
        ] {
            let kind = http_failure(403, body);
            assert_eq!(kind, Failure::Http(403), "{body}");
            assert!(!retryable(&Method::GET, "/api/jobs/j-1", &kind), "{body}");
        }
        // Only a 403 is the deny's rendering; any other status keeps
        // its own meaning whatever its body quotes.
        assert_eq!(http_failure(422, "policy-unreachable"), Failure::Http(422));
        assert_eq!(http_failure(503, "policy-unreachable"), Failure::Http(503));
    }

    #[test]
    fn a_policy_outage_is_named_by_its_reason_not_its_status() {
        assert!(names_a_policy_outage("policy-unreachable"));
        assert!(names_a_policy_outage(
            "jobs api GET /api/jobs/x -> 403 Forbidden: reading packets is refused: \
             policy-unreachable"
        ));
        assert!(names_a_policy_outage(
            "Policy Service Returned 500 Internal Server Error"
        ));
        assert!(!names_a_policy_outage(
            "policy service returned 404 Not Found"
        ));
        assert!(!names_a_policy_outage("policy service returned 5"));
        assert!(!names_a_policy_outage(
            "403 forbidden: job is outside your scope"
        ));
        assert!(!names_a_policy_outage(""));
    }

    #[test]
    fn an_unusable_answer_is_never_a_blip() {
        // The SoR answered; the body was garbage. Retrying re-reads
        // the same garbage.
        assert!(!retryable(
            &Method::GET,
            "/api/jobs/j-1",
            &Failure::Malformed
        ));
        assert!(!retryable(
            &Method::POST,
            "/api/jobs/j-1",
            &Failure::Malformed
        ));
    }

    #[test]
    fn the_backoff_doubles_from_the_base() {
        assert_eq!(JOBS_API_RETRY.attempts, 3);
        assert_eq!(JOBS_API_RETRY.backoff(1), Duration::from_secs(2));
        assert_eq!(JOBS_API_RETRY.backoff(2), Duration::from_secs(4));
        // The tests' policy makes the same decisions and never waits.
        assert_eq!(RetryPolicy::immediate(3).backoff(1), Duration::ZERO);
    }

    #[test]
    fn a_blip_cause_reads_the_innermost_error() {
        // "GET /api/jobs: error sending request: ... : Connection
        // refused" — the fact is at the bottom; the url is already
        // implied by the line around it.
        let e = anyhow!("Connection refused (os error 61)")
            .context("error sending request for url (http://10.20.0.34:7900/api/jobs)")
            .context("GET /api/jobs?kind=pr-train");
        assert_eq!(
            short_cause(&e, policy().blip_cause_budget),
            "Connection refused (os error 61)"
        );
        // A bare error is its own innermost cause.
        assert_eq!(
            short_cause(&anyhow!("HTTP 503"), policy().blip_cause_budget),
            "HTTP 503"
        );
        // And it stays journal-sized.
        let long = short_cause(&anyhow!("{}", "x".repeat(500)), policy().blip_cause_budget);
        assert!(long.chars().count() <= 81, "{} chars", long.chars().count());
        assert!(long.ends_with('…'), "says it truncated: {long}");
    }

    #[test]
    fn a_real_refused_connection_classifies_as_a_blip() {
        // The production failure end to end: reqwest's own error for a
        // refused connect must land on a retryable Failure, or the
        // classifier above is pinning a shape the wire never produces.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(async {
            reqwest::Client::builder()
                .timeout(Duration::from_millis(250))
                .build()
                .unwrap()
                // Port 1 refuses; a filtered port times out. Both are
                // blips, and neither is an answer.
                .get("http://127.0.0.1:1/api/jobs")
                .send()
                .await
                .expect_err("nothing serves port 1")
        });
        let kind = classify_transport(&err);
        assert!(
            matches!(kind, Failure::Connect | Failure::Ambiguous),
            "a refused/timed-out connect must be a transport failure, got {kind:?}"
        );
        assert!(retryable(&Method::GET, "/api/jobs/j-1", &kind));
    }

    // -- the retry driver --------------------------------------------------

    fn blip(kind: Failure) -> ApiFailure {
        ApiFailure {
            kind,
            cause: anyhow!("Connection refused (os error 61)"),
        }
    }

    /// A journal that counts its lines instead of printing them.
    /// Atomic rather than `Cell` because `retrying` now takes a
    /// `Sync` journal (the cadence loop's spawned verb tasks report
    /// through it).
    fn counting_journal(lines: &AtomicU32) -> impl Fn(&str) + Sync {
        move |_| {
            lines.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn a_blip_retries_to_the_attempt_budget_then_surfaces() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::GET,
            "/api/jobs/j-1",
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async { Err(blip(Failure::Connect)) }
            },
        )
        .await;
        assert!(out.is_err(), "the verb still surfaces a real outage");
        assert_eq!(calls, 3, "three attempts, not more");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            2,
            "one line per retry — blips stay countable"
        );
    }

    #[tokio::test]
    async fn a_recovered_blip_costs_nothing_but_a_line() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<u8> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            "/api/jobs/j-1",
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                let attempt = calls;
                async move {
                    if attempt == 1 {
                        Err(blip(Failure::Ambiguous))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls, 2, "stops the moment the SoR answers");
        assert_eq!(lines.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_answer_is_surfaced_on_the_first_attempt() {
        let mut calls = 0u32;
        let lines = AtomicU32::new(0);
        let out: Result<()> = retrying(
            &RetryPolicy::immediate(3),
            &Method::PUT,
            "/api/jobs/j-1",
            policy().blip_cause_budget,
            &counting_journal(&lines),
            || {
                calls += 1;
                async {
                    Err(ApiFailure {
                        kind: Failure::Http(422),
                        cause: anyhow!("PUT /api/jobs/x: HTTP 422: metadata_schema"),
                    })
                }
            },
        )
        .await;
        assert!(
            out.unwrap_err().to_string().contains("422"),
            "the answer reaches the operator unchanged"
        );
        assert_eq!(calls, 1, "a 422 is an answer — asked once");
        assert_eq!(
            lines.load(Ordering::Relaxed),
            0,
            "an answer is not a blip and journals none"
        );
    }

    // -- the operator verbs' roll wait --------------------------------------
    //
    // Backlog 034002b3, twice on 2026-09-23: the stack rolls with
    // strategy Recreate, so every converging train takes the jobs API
    // dark for about a minute, and an operator's `boss design` and a
    // builder's first `boss gate` each died at once on `No route to
    // host`. A refused connect proves nothing was sent, so the verb
    // waits it out — and ONLY it: anything past the connect may have
    // landed a write.

    // Every test below that can reach the wait runs on a PAUSED runtime
    // clock and walks the production [`ROLL_WAIT`] itself, so what it
    // asserts is how many attempts went out and in what order — never
    // how many real seconds that took. Until backlog 2bd6c5a8 they
    // shared a 40 ms real-time window (`QUICK_ROLL`), and the window is
    // measured from before the FIRST attempt: gate-run b3c19ff2
    // (2026-10-08) spent 7 s of a stalled build node inside one
    // loopback round trip and the wait gave up after "1 attempts" on a
    // car that had touched nothing here. On a paused clock only the
    // wait's own sleeps move time, so a slow attempt costs no budget.
    // A test whose op never waits holds no bound to miss; it is paused
    // too, so a regression that made it wait fails on its count at
    // once instead of sleeping the two minutes first.

    /// How many attempts [`ROLL_WAIT`] makes against an API that never
    /// comes back: one at 0 s, then after waits of 2, 4, 8 and seven of
    /// 15 s (119 s), and a last one after the 1 s the window had left.
    const WINDOW_ATTEMPTS: u32 = 12;

    /// A journal that keeps its lines, so a test can read what the
    /// operator would have seen.
    fn keeping_journal(lines: &std::sync::Mutex<Vec<String>>) -> impl Fn(&str) + Sync {
        move |l| lines.lock().unwrap().push(l.to_string())
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_connect_is_waited_out_until_the_api_answers() {
        let mut calls = 0u32;
        let lines = std::sync::Mutex::new(Vec::new());
        let out: Result<u8> = waiting_out_a_roll(
            &ROLL_WAIT,
            "jobs api POST /api/jobs",
            &keeping_journal(&lines),
            || {
                calls += 1;
                let attempt = calls;
                async move {
                    if attempt < 3 {
                        Err(blip(Failure::Connect))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(out.unwrap(), 7, "the write goes out once the API answers");
        assert_eq!(calls, 3, "stops the moment the connect succeeds");
        let lines = lines.into_inner().unwrap();
        assert_eq!(lines.len(), 2, "one visible line per wait: {lines:?}");
        for l in &lines {
            assert!(
                l.contains("the jobs API is not answering (a rollout?) — retrying"),
                "the wait must be visible and say what it is: {l}"
            );
            assert!(l.contains("POST /api/jobs"), "names the call: {l}");
        }
        // The order of the waits, read off the clock the wait itself
        // moved: 2 s from a standing start, then 4 s.
        assert!(lines[0].contains("in 2s (0s of 120s)"), "{}", lines[0]);
        assert!(lines[1].contains("in 4s (2s of 120s)"), "{}", lines[1]);
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_refused_connect_is_waited_out() {
        // Everything past the connect is either an ANSWER (any status)
        // or a request that may have reached the server — a timeout or
        // a dropped body under a POST could be a packet already filed,
        // so it is surfaced on the first attempt under every method.
        for method in [Method::GET, Method::POST] {
            for kind in [
                Failure::Ambiguous,
                Failure::Http(503),
                Failure::Http(422),
                Failure::Malformed,
            ] {
                let mut calls = 0u32;
                let lines = std::sync::Mutex::new(Vec::new());
                let out: Result<()> = waiting_out_a_roll(
                    &ROLL_WAIT,
                    &format!("jobs api {method} /api/jobs"),
                    &keeping_journal(&lines),
                    || {
                        calls += 1;
                        let kind = kind.clone();
                        async move { Err(blip(kind)) }
                    },
                )
                .await;
                assert!(out.is_err(), "{method} {kind:?} surfaces");
                assert_eq!(calls, 1, "{method} {kind:?} is asked once");
                assert!(
                    lines.into_inner().unwrap().is_empty(),
                    "{method} {kind:?} is not a roll and journals none"
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time() {
        let mut calls = 0u32;
        let lines = std::sync::Mutex::new(Vec::new());
        let err = waiting_out_a_roll::<(), _, _>(
            &ROLL_WAIT,
            "jobs api POST /api/jobs",
            &keeping_journal(&lines),
            || {
                calls += 1;
                async { Err(blip(Failure::Connect)) }
            },
        )
        .await
        .expect_err("a jobs API still dark after the window is an outage");
        assert_eq!(calls, WINDOW_ATTEMPTS, "it walked the whole window");
        assert_eq!(lines.into_inner().unwrap().len() as u32, calls - 1);
        let said = format!("{err:#}");
        assert!(
            said.contains("jobs api POST /api/jobs")
                && said.contains("refused every connection for 120s (12 attempts")
                && said.contains("Connection refused (os error 61)"),
            "the failure names the call, the elapsed time and the cause: {said}"
        );
        assert!(
            said.contains("nothing was sent"),
            "and says a refused connect landed nothing, so relaunching is safe: {said}"
        );
    }

    #[test]
    fn the_roll_wait_covers_a_recreate_roll_and_backs_off() {
        // About a minute dark per converge (Recreate, three RWO
        // claims); two minutes covers it with room, and is short
        // enough that a real outage still reaches the operator.
        assert_eq!(ROLL_WAIT.window, Duration::from_secs(120));
        assert_eq!(ROLL_WAIT.backoff(1), Duration::from_secs(2));
        assert_eq!(ROLL_WAIT.backoff(2), Duration::from_secs(4));
        assert_eq!(ROLL_WAIT.backoff(3), Duration::from_secs(8));
        assert_eq!(ROLL_WAIT.backoff(9), ROLL_WAIT.cap, "capped");
    }

    #[tokio::test]
    async fn a_real_refused_connect_is_what_the_wait_retries() {
        // The production failure end to end: reqwest's error for a port
        // nothing serves must classify as Connect, or the wait above
        // pins a shape the wire never produces.
        let err = reqwest::Client::new()
            .get("http://127.0.0.1:1/api/jobs")
            .send()
            .await
            .expect_err("nothing serves port 1");
        let f = ApiFailure::transport(err, "GET /api/jobs".into());
        assert_eq!(f.kind, Failure::Connect);
    }

    // -- a policy outage inside the roll wait -------------------------------
    //
    // Backlog f13c719e: after 5d4ad086 the conductor's classifier and
    // `boss gate --wait` rode out the fail-closed 403 a policy roll
    // produces, but the roll wait every operator verb's `api()` goes
    // through returned it as an answer, so `boss rerail --finish` — the
    // chain that broke at ~21:35Z on 2026-09-25 — `boss job file` and
    // `boss hold` still died on the first one. These drive the real wire
    // (a loopback stub) so the rebuilt answer is proven, not assumed.

    /// A loopback jobs API that answers `answers` in order, one per
    /// connection, repeating the last for ever; the counter is how many
    /// requests reached it.
    async fn serving(
        answers: Vec<(&'static str, &'static str)>,
    ) -> (String, std::sync::Arc<AtomicU32>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(AtomicU32::new(0));
        let count = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                crate::sor_client::stub::read_request(&mut sock).await;
                let n = count.fetch_add(1, Ordering::SeqCst) as usize;
                let (status, body) = answers[n.min(answers.len() - 1)];
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}/api/jobs/x"), seen)
    }

    /// Today's rendering of the fail-closed deny, and the 503 the
    /// server half (45553536) moves it to — the two sides meet here.
    const OUTAGES: [(&str, &str); 2] = [
        (
            "403 Forbidden",
            "reading packets is refused: policy-unreachable",
        ),
        ("503 Service Unavailable", "policy-unreachable"),
    ];

    #[tokio::test(start_paused = true)]
    async fn a_policy_outage_is_waited_out_on_an_idempotent_call() {
        for (status, body) in OUTAGES {
            for method in [Method::GET, Method::PUT, Method::DELETE] {
                let (url, seen) = serving(vec![(status, body), ("200 OK", "{\"ok\":true}")]).await;
                let lines = std::sync::Mutex::new(Vec::new());
                let client = reqwest::Client::new();
                let resp = send_through(
                    &ROLL_WAIT,
                    &format!("jobs api {method} /api/jobs/x"),
                    &keeping_journal(&lines),
                    || client.request(method.clone(), &url),
                )
                .await
                .unwrap_or_else(|e| panic!("{method} {status} is waited out: {e:#}"));
                assert_eq!(resp.status(), 200, "{method} {status}");
                assert_eq!(resp.text().await.unwrap(), "{\"ok\":true}");
                assert_eq!(
                    seen.load(Ordering::SeqCst),
                    2,
                    "{method} {status}: asked again"
                );
                let lines = lines.into_inner().unwrap();
                assert_eq!(lines.len(), 1, "one visible line per wait: {lines:?}");
                assert!(
                    lines[0].contains("policy service") && lines[0].contains("in 2s"),
                    "the wait names the policy service, not a dark API: {}",
                    lines[0]
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_policy_answer_is_returned_first_time_with_its_body() {
        // A scope refusal is the same status and a real answer; a 503
        // that does not name the policy service is the roll wait's old
        // answer too (the wait retries a refused connect, not a 5xx).
        for (status, body, code) in [
            ("403 Forbidden", "job is outside your scope", 403),
            ("503 Service Unavailable", "upstream busy", 503),
        ] {
            let (url, seen) = serving(vec![(status, body), ("200 OK", "{}")]).await;
            let lines = std::sync::Mutex::new(Vec::new());
            let client = reqwest::Client::new();
            let resp = send_through(
                &ROLL_WAIT,
                "jobs api GET /api/jobs/x",
                &keeping_journal(&lines),
                || client.get(&url),
            )
            .await
            .expect("an answer is not a failure of the send");
            assert_eq!(resp.status(), code);
            assert_eq!(
                resp.headers()["content-type"],
                "text/plain",
                "the answer keeps its headers"
            );
            assert_eq!(
                resp.text().await.unwrap(),
                body,
                "the body read to classify it reaches the caller intact"
            );
            assert_eq!(seen.load(Ordering::SeqCst), 1, "{status}: asked once");
            assert!(lines.into_inner().unwrap().is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_that_meets_a_policy_outage_is_answered_not_resent() {
        // POST and PATCH stay final: whether a handler checks policy
        // before it writes is not this wait's to assume (the same bound
        // `retryable` keeps).
        for (status, body) in OUTAGES {
            for method in [Method::POST, Method::PATCH] {
                let (url, seen) = serving(vec![(status, body), ("200 OK", "{}")]).await;
                let client = reqwest::Client::new();
                let resp = send_through(
                    &ROLL_WAIT,
                    &format!("jobs api {method} /api/jobs"),
                    &|_| {},
                    || client.request(method.clone(), &url).body("{}"),
                )
                .await
                .expect("the answer comes back as it is");
                assert_ne!(resp.status(), 200, "{method} {status}");
                assert_eq!(resp.text().await.unwrap(), body);
                assert_eq!(
                    seen.load(Ordering::SeqCst),
                    1,
                    "{method} {status}: sent once"
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_policy_outage_that_outlasts_the_window_names_the_policy_service() {
        let (url, seen) = serving(vec![OUTAGES[0]]).await;
        let client = reqwest::Client::new();
        let err = send_through(&ROLL_WAIT, "jobs api GET /api/jobs/x", &|_| {}, || {
            client.get(&url)
        })
        .await
        .expect_err("a policy service still failing after the window is an outage");
        assert_eq!(
            seen.load(Ordering::SeqCst),
            WINDOW_ATTEMPTS,
            "it asked through the whole window before giving up"
        );
        let said = format!("{err:#}");
        assert!(
            said.contains("jobs api GET /api/jobs/x")
                && said.contains("policy service was still failing closed after 120s (12 attempts")
                && said.contains("403 Forbidden: reading packets is refused: policy-unreachable"),
            "names the call, the service and the answer it kept getting: {said}"
        );
        assert!(
            !said.contains("nothing was sent"),
            "a request that reached the API must not claim it never left: {said}"
        );
        assert!(
            crate::gate::is_transient(&said),
            "boss gate --wait still reads it as a roll: {said}"
        );
    }

    /// Set to a directory, [`the_roll_voice_in_its_own_process`] runs;
    /// unset, it returns at once.
    const ROLL_VOICE_HELPER: &str = "BOSS_TEST_ROLL_VOICE";

    /// Not a test on its own: the process that
    /// [`a_roll_wait_whose_stderr_has_gone_does_not_panic`] starts with
    /// [`ROLL_VOICE_HELPER`] set and its stderr's reader gone. It walks
    /// the whole roll window of [`send_through_a_roll`] — the one every
    /// operator verb's write goes through — against a port that refuses,
    /// on a paused clock, and leaves the error it came back with.
    #[tokio::test(start_paused = true)]
    async fn the_roll_voice_in_its_own_process() {
        let Some(tell) = std::env::var_os(ROLL_VOICE_HELPER).map(std::path::PathBuf::from) else {
            return;
        };
        // Held, not bound and released (review 465e723f F3).
        let refusing = boss_testing::dark_port();
        let url = format!("{}/api/jobs/x", refusing.base);
        let client = reqwest::Client::new();
        let err = send_through_a_roll("jobs api PATCH /api/jobs/x/metadata", || {
            client.patch(&url).body("{}")
        })
        .await
        .expect_err("nothing answers");
        std::fs::write(tell.join("ended"), format!("{err:#}")).unwrap();
    }

    /// Review 0facb916, N1. THE ROLL WAIT'S RETRY LINE DOES NOT PANIC
    /// WHEN STDERR HAS GONE. Its voice was `eprintln!`, which panics when
    /// the write fails — so a verb whose terminal had closed, writing
    /// while the jobs API refused connects (a roll: about a minute dark
    /// per converge), died 101 on the retry line and never wrote what it
    /// was writing: for `boss dispatch --launch` that is Building's
    /// `died` (the N19 failure of review b149c579, through this file).
    #[test]
    fn a_roll_wait_whose_stderr_has_gone_does_not_panic() {
        let tell = boss_testing::scratch_dir("roll-voice-no-stderr");
        let (reader, writer) = std::io::pipe().unwrap();
        // Nobody reads the verb's stderr: every write to it fails.
        drop(reader);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "train::jobs_api::tests::the_roll_voice_in_its_own_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLL_VOICE_HELPER, &tell)
            .stdout(std::process::Stdio::from(
                std::fs::File::create(tell.join("stdout")).unwrap(),
            ))
            .stderr(std::process::Stdio::from(writer))
            .status()
            .unwrap();
        let ended = std::fs::read_to_string(tell.join("ended")).unwrap_or_else(|_| {
            panic!(
                "the roll wait ended ({status}) without coming back: a retry line panicked. {}",
                std::fs::read_to_string(tell.join("stdout")).unwrap_or_default()
            )
        });
        assert!(
            ended.contains("refused every connection"),
            "it walked the whole window and came back with the outage: {ended}"
        );
        assert!(status.success(), "{status}");
    }
}
