//! A HOLD ON THE GATES — one row in the system of record that every gate
//! launcher reads before it launches (backlog 95401d82, 2026-11-07).
//!
//! WHY. During the gate-volume move that day the conductor wrote GATE
//! HOLD orders to both builder sessions (15:03Z, 17:50Z) and both
//! complied, yet w-1 was never free of a gate pod for the retire plan to
//! pass: main's full gate after each landing (aab40be4, b4d2fb46) and the
//! dock-depth board (train e3de7680) each launched a ~40-minute pod that
//! mounts /var/mnt/gate. An order holds actors; no automaton reads one.
//! The move's step 0, "hold the gates", was a hope, and it cost about
//! three hours. The converge already had the shape that works —
//! `infra/forge/converge-hold.sh`, a hold the runner reads before it
//! rolls — and this is that shape for the gates, as registry data
//! instead of a host file: a `station-hold` packet
//! (`infra/platform/workflows/station-hold.toml`) whose `station` is
//! `gates`. Open is held.
//!
//! WHO READS IT — every door that can put a pod on the gate volume:
//!
//! - the board (`Conductor::board`): assembles nothing, departs nothing,
//!   and says `NoDeparture::GatesHeld` on its firing;
//! - every conductor launch (`Conductor::launch_gate`): main's full gate,
//!   a train's gate, a red's relaunch — refused as a place in the gate
//!   line (`AtGateBound`), so a train already on the track waits for the
//!   release instead of counting a fault;
//! - the dock's base re-gates (`Conductor::launch_base_regate`): held
//!   BEFORE the branch is replayed, so nothing moves on the forge;
//! - the resident gate line (`reservation::supervisor`): every accepted
//!   request stays in line, except one gated `--despite-hold`;
//! - `boss gate` itself: refused with the hold's reason, unless
//!   `--despite-hold '<why>'`, which rides on the gate-run.
//!
//! An UNREADABLE hold launches nothing either: each of those doors
//! needs the same jobs API for its own launch, so failing closed costs a
//! pass, never a window.
//!
//! A HOLD NEVER LIFTS ITSELF. Past its `until`, the conductor's reconcile
//! files ONE urgent alarm naming it (`overdue_alarm_body`), and the
//! gates stay held: a quiet window that ended on a timer would start a
//! pod under a move that ran long, which is the failure the row exists
//! to prevent. `boss gates release` is the one way out.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value, json};

/// The protocol a hold is a packet of.
pub(crate) const KIND: &str = "station-hold";
/// The station this module holds. The row names its station so it says
/// what it holds; only the gates have readers today.
pub(crate) const STATION: &str = "gates";
/// The step `boss gates release` completes; its terminal closes the hold.
pub(crate) const RELEASE_STEP: &str = "release";
/// The key an overdue alarm carries the hold's id under — one alarm per
/// hold, never one per pass.
pub(crate) const ALARM_KEY: &str = "gate_hold_overdue";
/// The key a gate-run launched DESPITE a hold carries its reason under;
/// the resident line launches such a request through the hold.
pub(crate) const DESPITE_KEY: &str = "despite_hold";
/// The key naming WHICH hold that reason crossed: an override binds to
/// one hold, never to the next (review R1 on car 0d2e0449).
pub(crate) const DESPITE_OF_KEY: &str = "despite_hold_of";
/// The longest bound `boss gates hold --for` takes. A hold is a quiet
/// window for one operation, not a parking brake: a longer one is a
/// release and a fresh hold, each on the record with its own reason.
pub(crate) const MAX_BOUND_HOURS: i64 = 24;

/// The read every launcher makes: the open holds on the gates.
pub(crate) fn path() -> String {
    let filter = json!({ "station": STATION }).to_string();
    let q = percent_encoding::utf8_percent_encode(&filter, crate::job::QUERY_VALUE).to_string();
    format!("/api/jobs?kind={KIND}&status=open&full=true&limit=50&metadata={q}")
}

/// One standing hold, as its row says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateHold {
    pub(crate) id: String,
    pub(crate) held_by: String,
    pub(crate) why: String,
    pub(crate) since: Option<DateTime<Utc>>,
    pub(crate) until: Option<DateTime<Utc>>,
}

fn text<'a>(row: &'a Value, pointer: &str) -> Option<&'a str> {
    row.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn when(row: &Value, pointer: &str) -> Option<DateTime<Utc>> {
    text(row, pointer)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// PURE: every open hold on the gates in `rows`, oldest first. Checked
/// here as well as in the query, so a server that ignored a filter
/// cannot hold the gates with a closed row or another station's —
/// while a row missing its `status` still holds (fails closed).
pub(crate) fn all_standing(rows: &[Value]) -> Vec<GateHold> {
    let mut holds: Vec<GateHold> = rows
        .iter()
        .filter(|r| {
            r.get("kind")
                .and_then(Value::as_str)
                .is_none_or(|k| k == KIND)
        })
        .filter(|r| text(r, "/metadata/station") == Some(STATION))
        .filter(|r| text(r, "/status").is_none_or(|s| s == "open"))
        .map(|r| GateHold {
            id: text(r, "/id").unwrap_or("(no id)").to_string(),
            held_by: text(r, "/metadata/held_by")
                .or_else(|| text(r, "/owner_id"))
                .unwrap_or("(nobody recorded)")
                .to_string(),
            why: text(r, "/metadata/why")
                .unwrap_or("(no reason recorded)")
                .to_string(),
            since: when(r, "/metadata/since"),
            until: when(r, "/metadata/until"),
        })
        .collect();
    holds.sort_by(|a, b| (a.since, &a.id).cmp(&(b.since, &b.id)));
    holds
}

/// PURE: the hold the launchers obey — the oldest standing one.
pub(crate) fn standing(rows: &[Value]) -> Option<GateHold> {
    all_standing(rows).into_iter().next()
}

fn stamp(t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(
        || "(unrecorded)".to_string(),
        |t| t.format("%Y-%m-%dT%H:%MZ").to_string(),
    )
}

/// PURE: how far past its bound a hold stands; `None` while inside it.
/// A hold with no readable `until` has no bound, so it is past it.
pub(crate) fn overdue_by(hold: &GateHold, now: DateTime<Utc>) -> Option<Duration> {
    match hold.until {
        None => Some(Duration::zero()),
        Some(until) if now > until => Some(now - until),
        Some(_) => None,
    }
}

/// PURE: the line every automaton records when it launches nothing —
/// on the board's firing, a train's gate wait, a car's skip reason, the
/// conductor's journal and the resident line's retained reasons.
pub(crate) fn held_line(hold: &GateHold) -> String {
    format!(
        "held: {} — the gates are held by {} since {}, until {} (station-hold {}); \
         `boss gates release` lifts it",
        hold.why,
        hold.held_by,
        stamp(hold.since),
        stamp(hold.until),
        crate::train::id8(&hold.id)
    )
}

/// PURE: what `boss gate` says when a hold stands and no
/// `--despite-hold` was given. Nothing has been filed or pushed yet.
pub(crate) fn refusal(hold: &GateHold, branch: &str) -> String {
    format!(
        "boss gate: REFUSED — the gates are held: {}\n  held by {} since {}, until {} \
         (station-hold {}).\n  Nothing was filed for {branch}, nothing was pushed and none of the \
         gates' capacity was taken. Wait for `boss gates release`, or gate anyway with --despite-hold '<why this \
         gate cannot wait>', which is recorded on the gate-run.",
        hold.why,
        hold.held_by,
        stamp(hold.since),
        stamp(hold.until),
        hold.id
    )
}

/// PURE: `boss gate`'s answer to the hold. `Err` is the refusal; `Ok`
/// carries a line to print when it launches through one.
pub(crate) fn despite_guard(
    hold: Option<&GateHold>,
    despite: Option<&str>,
    branch: &str,
) -> std::result::Result<Option<String>, String> {
    let reason = despite.map(str::trim).filter(|r| !r.is_empty());
    match (hold, reason) {
        (None, None) => Ok(None),
        (None, Some(_)) => Ok(Some(
            "boss gate: --despite-hold given, but no hold stands on the gates — gating as usual"
                .to_string(),
        )),
        (Some(h), None) if despite.is_some() => Err(format!(
            "{}\n  --despite-hold needs a reason: it is the record of why this gate ran \
             inside a declared quiet window.",
            refusal(h, branch)
        )),
        (Some(h), None) => Err(refusal(h, branch)),
        (Some(h), Some(r)) => Ok(Some(format!(
            "boss gate: launching DESPITE the hold on the gates ({}; station-hold {}) — because \
             {r}. Recorded on the gate-run as `{DESPITE_KEY}`.",
            h.why,
            crate::train::id8(&h.id)
        ))),
    }
}

/// The record a gate launched through a hold carries.
pub(crate) fn despite_patch(hold: &GateHold, reason: &str) -> Value {
    json!({ DESPITE_KEY: reason.trim(), DESPITE_OF_KEY: hold.id })
}

/// PURE: whether a gate-run's metadata says it was launched through THIS
/// hold — the one request the resident line still launches under it. The
/// reason must be nonempty AND recorded against `hold_id`: an override
/// crossed one hold, and a request still queued when that hold lifts has
/// no reason on record to cross the next (review R1 on car 0d2e0449).
pub(crate) fn launched_despite(metadata: &Value, hold_id: &str) -> bool {
    let reason = metadata
        .get(DESPITE_KEY)
        .and_then(Value::as_str)
        .is_some_and(|r| !r.trim().is_empty());
    reason && metadata.get(DESPITE_OF_KEY).and_then(Value::as_str) == Some(hold_id)
}

/// PURE: the GATING lane's line about the hold, for `boss orient`.
pub(crate) fn orient_line(
    read: &std::result::Result<Option<GateHold>, String>,
    now: DateTime<Utc>,
) -> String {
    match read {
        Ok(None) => "    HOLD — none: the gate launchers launch".to_string(),
        Ok(Some(h)) => {
            let late = overdue_by(h, now).map_or(String::new(), |d| {
                format!(
                    " — OVERDUE by {}m: the conductor files an alarm, and the hold does not \
                     lift itself",
                    d.num_minutes()
                )
            });
            format!(
                "    HELD — {} (by {} since {}, until {}; station-hold {}){late}. The board, main's \
                 full gate, the dock's re-gates and the resident line launch nothing; `boss \
                 gates release` lifts it",
                h.why,
                h.held_by,
                stamp(h.since),
                stamp(h.until),
                crate::train::id8(&h.id)
            )
        }
        Err(e) => format!("    HOLD — unread: {e} (every gate launcher waits while it is)"),
    }
}

/// PURE: a `--for` bound — whole minutes or hours (`90m`, `2h`), above
/// zero and at most [`MAX_BOUND_HOURS`].
pub(crate) fn parse_bound(raw: &str) -> std::result::Result<Duration, String> {
    let s = raw.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let shape = || format!("--for {raw:?}: a bound is a whole number and a unit, e.g. 90m or 2h");
    let n: i64 = n.parse().map_err(|_| shape())?;
    // Whole minutes as an integer, compared against the cap BEFORE any
    // Duration is built: a duration constructor panics on a valid i64 it
    // cannot hold (review R2 on car 0d2e0449).
    let minutes = match unit {
        "m" | "min" => Some(n),
        "h" => n.checked_mul(60),
        _ => return Err(shape()),
    };
    if n <= 0 {
        return Err(format!("--for {raw:?}: a hold needs a bound above zero"));
    }
    let Some(minutes) = minutes.filter(|m| *m <= MAX_BOUND_HOURS * 60) else {
        return Err(format!(
            "--for {raw:?}: at most {MAX_BOUND_HOURS}h — a hold is a quiet window for one \
             operation, not a parking brake; a longer one is a release and a fresh hold, each \
             with its own reason"
        ));
    };
    Ok(Duration::minutes(minutes))
}

/// PURE: the packet `boss gates hold` files.
pub(crate) fn hold_body(
    why: &str,
    held_by: &str,
    now: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Value {
    json!({
        "kind": KIND,
        "status": "open",
        "title": format!("Gates held: {why}"),
        "subject": {"subject_kind": "custom", "id": STATION},
        "owner_id": held_by,
        "priority": "standard",
        "tags": ["pipeline", "gate"],
        "metadata": {
            "station": STATION,
            "why": why,
            "held_by": held_by,
            "since": now.to_rfc3339(),
            "until": until.to_rfc3339(),
        },
    })
}

/// The read that finds a hold's overdue alarm.
pub(crate) fn alarm_path(hold_id: &str) -> String {
    let filter = json!({ ALARM_KEY: hold_id }).to_string();
    let q = percent_encoding::utf8_percent_encode(&filter, crate::job::QUERY_VALUE).to_string();
    format!("/api/jobs?kind=backlog-item&status=open&limit=10&metadata={q}")
}

/// PURE: whether an overdue alarm for `hold_id` is among `rows` —
/// checked on the row, so a server that ignored the filter cannot
/// answer "already filed" with every open item.
pub(crate) fn alarm_filed(rows: &[Value], hold_id: &str) -> bool {
    rows.iter().any(|r| {
        r.pointer(&format!("/metadata/{ALARM_KEY}"))
            .and_then(Value::as_str)
            == Some(hold_id)
    })
}

/// PURE: the alarm a hold past its bound becomes. It names the hold, its
/// reason, who placed it and by how long it is late, because detection
/// is not diagnosis: the reader starts at the packet.
pub(crate) fn overdue_alarm_body(hold: &GateHold, now: DateTime<Utc>, owner: &str) -> Value {
    let late = overdue_by(hold, now).map_or(0, |d| d.num_minutes());
    json!({
        "kind": "backlog-item",
        "status": "open",
        "title": format!(
            "GATES HELD PAST THEIR BOUND: station-hold {} ({}) was to end {} and still stands \
             — no gate launches",
            crate::train::id8(&hold.id),
            hold.why,
            stamp(hold.until)
        ),
        "subject": {"subject_kind": "custom", "id": "bosspipeline"},
        "owner_id": owner,
        "priority": "urgent",
        "tags": ["pipeline", "gate"],
        "metadata": {
            "area": "infra/gate",
            ALARM_KEY: hold.id,
            "last_measured_at": now.to_rfc3339(),
            "detail": format!(
                "station-hold {} has held the gates since {} for: {}. It was placed by {} with a \
                 bound of {}, and is {late} minute(s) past it. While it stands the board departs \
                 no train, main's full gate and the dock's re-gates defer, the resident gate \
                 line launches nothing and `boss gate` refuses. A hold never lifts itself: \
                 either the operation it declared is still running (say so on that hold, or \
                 release it and hold again with a new bound) or it is over and `boss gates \
                 release --why '<what finished>'` lifts it. Filed once per hold by the \
                 conductor's reconcile (backlog 95401d82).",
                hold.id,
                stamp(hold.since),
                hold.why,
                hold.held_by,
                stamp(hold.until)
            ),
        },
    })
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Hold or release the gates station: while a hold stands, no
    /// automaton launches a gate pod (the board, main's full gate, the
    /// dock's re-gates, the resident gate line) and `boss gate` refuses
    /// unless --despite-hold says why. A data move on the gate volume
    /// declares its quiet window here (backlog 95401d82).
    Gates {
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Hold the gates: file one station-hold row with a reason and a
    /// bound, then read it back. Refused while a hold already stands.
    /// Past its bound the hold raises an alarm; it never lifts itself.
    Hold {
        /// Why the gates are held — the line every launcher records.
        #[arg(long, required_unless_present = "why_file")]
        why: Option<String>,
        /// The reason from a file (no word expansion).
        #[arg(long, conflicts_with = "why")]
        why_file: Option<std::path::PathBuf>,
        /// The bound: whole minutes or hours, e.g. 90m or 2h, at most 24h.
        #[arg(long = "for", value_name = "BOUND")]
        bound: String,
    },
    /// Release the gates: complete every standing hold's `release` step,
    /// then read each back by id until its terminal closes (up to 30 s).
    /// Exit 0 when closed and no hold stands; 75 when the release is
    /// recorded but the close still waits on the dispatcher; 1 when NOT
    /// released.
    Release {
        /// Why the hold is over — recorded on its release step.
        #[arg(long, required_unless_present = "why_file")]
        why: Option<String>,
        /// The reason from a file (no word expansion).
        #[arg(long, conflicts_with = "why")]
        why_file: Option<std::path::PathBuf>,
    },
    /// Show the hold standing on the gates, if any.
    Show,
}

/// `now` is the wall clock, minted at the CLI boundary: `since` and
/// `until` are real elapsed time, the clock the conductor judges the
/// bound by.
pub async fn dispatch(cmd: Cmd, now: DateTime<Utc>) -> Result<()> {
    let Cmd::Gates { action } = cmd;
    match action {
        Action::Hold {
            why,
            why_file,
            bound,
        } => {
            let why = reason("--why", why, why_file)?;
            hold(&why, &bound, now).await
        }
        Action::Release { why, why_file } => {
            let why = reason("--why", why, why_file)?;
            release(&why).await
        }
        Action::Show => {
            let http = crate::sor_client::machine_client()?;
            let read = read_hold(&http).await.map_err(|e| format!("{e:#}"));
            println!("{}", orient_line(&read, now).trim_start());
            Ok(())
        }
    }
}

fn reason(flag: &str, text: Option<String>, file: Option<std::path::PathBuf>) -> Result<String> {
    let why = crate::prose::opt_text_or_file(flag, &format!("{flag}-file"), text, file.as_deref())?
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if why.chars().count() < 10 {
        bail!("{flag}: a reason of at least 10 characters — it is the line every launcher records");
    }
    Ok(why)
}

async fn read_hold(http: &crate::machine_http::MachineClient) -> Result<Option<GateHold>> {
    Ok(standing(&read_all(http).await?))
}

async fn read_all(http: &crate::machine_http::MachineClient) -> Result<Vec<Value>> {
    crate::train::rows(crate::sor_client::api(http, reqwest::Method::GET, &path(), None).await?)
        .context("reading the holds on the gates")
}

fn caller_id() -> Result<String> {
    crate::identity::caller().map(|c| c.id).with_context(|| {
        format!(
            "boss gates: nothing names the actor running this command (BOSS_ACTOR or {}) — a \
             hold says who placed it",
            crate::identity::actor_file_display()
        )
    })
}

async fn hold(why: &str, bound: &str, now: DateTime<Utc>) -> Result<()> {
    let bound = parse_bound(bound).map_err(anyhow::Error::msg)?;
    let who = caller_id()?;
    let http = crate::sor_client::machine_client()?;
    if let Some(h) = read_hold(&http).await? {
        bail!(
            "boss gates: NOT HELD — a hold already stands: {}. Release it first; a second hold \
             would leave two rows to release and one reason the launchers do not print",
            held_line(&h)
        );
    }
    let created = crate::sor_client::api(
        &http,
        reqwest::Method::POST,
        "/api/jobs",
        Some(hold_body(why, &who, now, now + bound)),
    )
    .await?;
    let id = created
        .as_ref()
        .and_then(|c| c.get("data").unwrap_or(c).get("id"))
        .and_then(Value::as_str)
        .context("boss gates: the jobs API returned no id for the hold — NOT HELD as far as this verb can read")?
        .to_string();
    // READ BACK, NEVER ASSUMED (the converge hold's rule, d94d287e).
    match read_hold(&http).await? {
        Some(h) if h.id == id => {
            println!("boss gates: HELD — {}", held_line(&h));
            Ok(())
        }
        other => bail!(
            "boss gates: NOT HELD — station-hold {id} was filed but the launchers' read answers {}",
            other.map_or_else(|| "no hold".to_string(), |h| held_line(&h))
        ),
    }
}

async fn release(why: &str) -> Result<()> {
    let who = caller_id()?;
    let http = crate::sor_client::machine_client()?;
    let holds = all_standing(&read_all(&http).await?);
    if holds.is_empty() {
        println!("boss gates: no hold was standing");
        return Ok(());
    }
    for h in &holds {
        let job = crate::sor_client::api(
            &http,
            reqwest::Method::GET,
            &format!("/api/jobs/{}", h.id),
            None,
        )
        .await?
        .with_context(|| format!("station-hold {} could not be read", h.id))?;
        let job = job.get("data").cloned().unwrap_or(job);
        let step = crate::train::find_step(&job, RELEASE_STEP, RELEASE_STEP)
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .with_context(|| format!("station-hold {} has no `{RELEASE_STEP}` step", h.id))?
            .to_string();
        let mut writes = Map::new();
        writes.insert("released_why".into(), json!(why));
        writes.insert("released_by".into(), json!(who));
        for (method, path, body) in crate::train::step_completion_writes(&h.id, &step, writes) {
            crate::sor_client::api(&http, method, &path, Some(body)).await?;
        }
    }
    // READ BACK BY ID, AND WAIT FOR THE CLOSE (backlog 07a053ff): the
    // launchers' list read, taken at once, lands before the dispatcher
    // completes `released` and says the hold still stands.
    let mut pending = Vec::new();
    for h in &holds {
        let (http, path) = (&http, format!("/api/jobs/{}", h.id));
        let path = path.as_str();
        let read = move || async move {
            crate::sor_client::api(http, reqwest::Method::GET, path, None)
                .await?
                .context("the jobs API returned no body")
        };
        match settle(read, RELEASE_SETTLE, RELEASE_POLL)
            .await
            .with_context(|| {
                format!(
                    "boss gates: the release of station-hold {} was written, but reading it \
                     back failed — re-read with `boss gates show`",
                    h.id
                )
            })? {
            Settled::Released => println!("boss gates: released (was: {})", held_line(h)),
            Settled::ClosePending => pending.push(h),
            Settled::NotReleased(status) => bail!(
                "boss gates: NOT released — station-hold {}'s `{RELEASE_STEP}` step reads \
                 `{status}` after this verb completed it: {}",
                h.id,
                held_line(h)
            ),
        }
    }
    if !pending.is_empty() {
        for h in &pending {
            eprintln!("{}", close_pending_line(&h.id));
        }
        std::process::exit(CLOSE_PENDING_EXIT);
    }
    // The launchers' read, last: exit 0 says the launchers launch, so a
    // hold someone filed meanwhile is named rather than hidden.
    let left = all_standing(&read_all(&http).await?);
    if let Some(h) = left.first() {
        bail!(
            "boss gates: the {} hold(s) this verb released are closed, but {} other(s) \
             stand(s): {}",
            holds.len(),
            left.len(),
            held_line(h)
        );
    }
    Ok(())
}

/// HOW LONG `boss gates release` WAITS FOR THE CLOSE ITS RELEASE CAUSES
/// (backlog 07a053ff, 2026-11-11). Completing `release` does not close
/// the hold: its `released` step is an outcome marker, and markers are
/// completed by the dispatcher (`complete-marker-on-step-ready`) off the
/// `step.ready.*` event, AFTER the step PUT has returned. On
/// station-hold f7ccc64c that took 238 ms, and the verb's read-back,
/// taken at once, said NOT released (exit 1) for a hold it had just
/// released; a minute later orient read no hold and a gate launched.
/// The gap has no upper bound — the dispatcher's consumer has fallen
/// hours behind (fba16f3f) — so a sleep is no fix: the verb reads the
/// hold back BY ID, judges the operator's act from the `release` step
/// itself, and waits this long for the close, re-reading every
/// `RELEASE_POLL`.
pub(crate) const RELEASE_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);
/// How often it re-reads the hold while it waits.
pub(crate) const RELEASE_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// THE EXIT CODE OF A RELEASE WHOSE CLOSE IS STILL PENDING: 75, the
/// tree's "not yet" (`crate::prove::NOT_YET_EXIT`, EX_TEMPFAIL). Not 0,
/// because the launchers still read the hold as standing, and a script
/// that starts a pod on this verb's success must not start it yet. Not
/// 1, because 1 is NOT released, which sends the operator to release
/// again — and the act a retry would repeat is already on the record.
/// 75 says: nothing to redo, re-read later (`boss gates show`).
pub(crate) const CLOSE_PENDING_EXIT: i32 = crate::prove::NOT_YET_EXIT;

/// What one by-id read of a hold says about its release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseRead {
    /// The packet is no longer open: no launcher reads it.
    Closed,
    /// `release` is completed and the packet is still open: the
    /// `released` terminal has not been completed yet.
    Recorded,
    /// `release` is not completed — its status, or `missing`.
    NotRecorded(String),
}

/// PURE: judge one by-id read (`GET /api/jobs/{id}`) of a hold. A row
/// missing its `status` reads as open, as `all_standing` reads it.
pub(crate) fn read_release(job: &Value) -> ReleaseRead {
    let job = job.get("data").unwrap_or(job);
    if text(job, "/status").is_some_and(|s| s != "open") {
        return ReleaseRead::Closed;
    }
    let step = crate::train::find_step(job, RELEASE_STEP, RELEASE_STEP);
    if crate::train::step_done(step) {
        ReleaseRead::Recorded
    } else {
        ReleaseRead::NotRecorded(
            step.and_then(|s| text(s, "/status"))
                .unwrap_or("missing")
                .to_string(),
        )
    }
}

/// The verdict on one released hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Settled {
    /// Closed: the launchers launch.
    Released,
    /// `release` recorded, the terminal still open past the bound.
    ClosePending,
    /// The `release` step does not read completed after the write.
    NotReleased(String),
}

/// Read one hold back by id until it settles: closed is `Released` at
/// once; a `release` step that does not read completed is `NotReleased`
/// at once, because the step PUT is synchronous and waiting would only
/// delay the truth; a recorded release re-reads every `every` until
/// `bound` has passed, then is `ClosePending`.
pub(crate) async fn settle<F, Fut>(
    mut read: F,
    bound: std::time::Duration,
    every: std::time::Duration,
) -> Result<Settled>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        match read_release(&read().await?) {
            ReleaseRead::Closed => return Ok(Settled::Released),
            ReleaseRead::NotRecorded(status) => return Ok(Settled::NotReleased(status)),
            ReleaseRead::Recorded if tokio::time::Instant::now() >= deadline => {
                return Ok(Settled::ClosePending);
            }
            ReleaseRead::Recorded => tokio::time::sleep(every).await,
        }
    }
}

/// PURE: the verdict for a release recorded on a hold whose terminal has
/// not closed within the bound — never "NOT released".
pub(crate) fn close_pending_line(id: &str) -> String {
    format!(
        "boss gates: release recorded on station-hold {id}, but its `released` terminal has not \
         closed within {}s — it waits on the dispatcher's complete-marker-on-step-ready rule, \
         and until it closes the launchers still read it as held. Nothing to redo: re-read with \
         `boss gates show`",
        RELEASE_SETTLE.as_secs()
    )
}

/// A standing hold's row, as the jobs API lists it — the fixture every
/// launcher's pin answers its hold read with.
#[cfg(test)]
pub(crate) fn hold_row(id: &str, since: &str, until: &str) -> Value {
    json!({"id": id, "kind": KIND, "status": "open", "owner_id": "agent-claude",
        "metadata": {"station": STATION, "why": "retire-a-user-volume on w-1",
            "held_by": "agent-claude", "since": since, "until": until}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_oldest_open_hold_on_the_gates_is_the_one_obeyed() {
        let rows = vec![
            hold_row("bbbbbbbb-2", "2026-11-07T19:00:00Z", "2026-11-07T20:30:00Z"),
            hold_row("aaaaaaaa-1", "2026-11-07T18:00:00Z", "2026-11-07T19:30:00Z"),
            // Another station's, and a closed one, hold nothing here.
            json!({"id":"c","kind":KIND,"status":"open","metadata":{"station":"converge","why":"x"}}),
            json!({"id":"d","kind":KIND,"status":"closed","metadata":{"station":STATION,"why":"x"}}),
        ];
        let h = standing(&rows).expect("held");
        assert_eq!(h.id, "aaaaaaaa-1");
        assert_eq!(all_standing(&rows).len(), 2);
        assert_eq!(standing(&[]), None);
    }

    #[test]
    fn a_hold_past_its_bound_is_overdue_and_one_with_no_bound_already_is() {
        let h = standing(&[hold_row(
            "a",
            "2026-11-07T18:00:00Z",
            "2026-11-07T19:30:00Z",
        )])
        .unwrap();
        assert_eq!(overdue_by(&h, at("2026-11-07T19:00:00Z")), None);
        assert_eq!(
            overdue_by(&h, at("2026-11-07T19:45:00Z")),
            Some(Duration::minutes(15))
        );
        let unbounded = GateHold { until: None, ..h };
        assert_eq!(
            overdue_by(&unbounded, at("2026-11-07T18:01:00Z")),
            Some(Duration::zero())
        );
    }

    /// THE REFUSAL TEXT `boss gate` prints: the hold's reason, who, the
    /// bound, that nothing was filed, and both ways forward.
    #[test]
    fn boss_gate_refuses_under_a_hold_naming_it_unless_despite_hold_says_why() {
        let h = standing(&[hold_row(
            "95401d82-aaaa",
            "2026-11-07T18:00:00Z",
            "2026-11-07T19:30:00Z",
        )])
        .unwrap();
        let refused = despite_guard(Some(&h), None, "fix/x").unwrap_err();
        for said in [
            "boss gate: REFUSED — the gates are held: retire-a-user-volume on w-1",
            "held by agent-claude since 2026-11-07T18:00Z, until 2026-11-07T19:30Z",
            "station-hold 95401d82-aaaa",
            "Nothing was filed for fix/x",
            "`boss gates release`",
            "--despite-hold '<why this gate cannot wait>'",
        ] {
            assert!(refused.contains(said), "{said:?} missing from: {refused}");
        }
        // An empty reason is no reason.
        let empty = despite_guard(Some(&h), Some("  "), "fix/x").unwrap_err();
        assert!(empty.contains("--despite-hold needs a reason"), "{empty}");
        // A reason launches, and says it did.
        let note = despite_guard(Some(&h), Some("the move's own probe"), "fix/x")
            .unwrap()
            .unwrap();
        assert!(note.contains("launching DESPITE the hold"), "{note}");
        assert!(note.contains("because the move's own probe"), "{note}");
        assert_eq!(despite_guard(None, None, "fix/x"), Ok(None));
        let crossed = despite_patch(&h, "the move's own probe");
        assert!(launched_despite(&crossed, &h.id));
        assert!(!launched_despite(&crossed, "another-hold"));
        assert!(!launched_despite(
            &json!({DESPITE_KEY: "no hold named"}),
            &h.id
        ));
        assert!(!launched_despite(&json!({}), &h.id));
        assert!(!launched_despite(
            &json!({DESPITE_KEY: " ", DESPITE_OF_KEY: h.id}),
            &h.id
        ));
    }

    /// THE ORIENT LINE: none, held (with the bound), overdue, unread.
    #[test]
    fn orient_shows_the_hold_in_gating() {
        let h = standing(&[hold_row(
            "95401d82-aaaa",
            "2026-11-07T18:00:00Z",
            "2026-11-07T19:30:00Z",
        )])
        .unwrap();
        let held = orient_line(&Ok(Some(h.clone())), at("2026-11-07T19:00:00Z"));
        assert!(
            held.starts_with("    HELD — retire-a-user-volume on w-1 (by agent-claude since 2026-11-07T18:00Z, until 2026-11-07T19:30Z; station-hold 95401d82)"),
            "{held}"
        );
        assert!(!held.contains("OVERDUE"), "{held}");
        let late = orient_line(&Ok(Some(h)), at("2026-11-07T20:00:00Z"));
        assert!(late.contains("OVERDUE by 30m"), "{late}");
        assert!(late.contains("does not lift itself"), "{late}");
        assert_eq!(
            orient_line(&Ok(None), at("2026-11-07T20:00:00Z")),
            "    HOLD — none: the gate launchers launch"
        );
        let unread = orient_line(&Err("HTTP 503".into()), at("2026-11-07T20:00:00Z"));
        assert!(unread.contains("HOLD — unread: HTTP 503"), "{unread}");
    }

    #[test]
    fn a_bound_is_whole_minutes_or_hours_above_zero_and_at_most_a_day() {
        assert_eq!(parse_bound("90m"), Ok(Duration::minutes(90)));
        assert_eq!(parse_bound("2h"), Ok(Duration::hours(2)));
        assert_eq!(parse_bound("24h"), Ok(Duration::hours(24)));
        for bad in ["", "90", "m", "1.5h", "-5m", "0m", "25h", "2d"] {
            assert!(parse_bound(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// REVIEW R2 (car 0d2e0449): a bound too large for a duration is
    /// refused with the cap, never a panic building it.
    #[test]
    fn oversized_duration_text_is_refused_without_panicking() {
        for huge in [
            "9223372036854775807h",
            "9223372036854775807m",
            "153722867280912931m",
        ] {
            let refused = parse_bound(huge).unwrap_err();
            assert!(refused.contains("at most 24h"), "{huge}: {refused}");
        }
        assert!(parse_bound("99999999999999999999h").is_err());
        assert!(parse_bound("1441m").unwrap_err().contains("at most 24h"));
        assert_eq!(parse_bound("1440m"), Ok(Duration::hours(24)));
    }

    #[test]
    fn the_hold_row_carries_who_why_since_and_until() {
        let now = at("2026-11-07T18:00:00Z");
        let body = hold_body(
            "retire-a-user-volume on w-1",
            "agent-claude",
            now,
            now + Duration::minutes(90),
        );
        assert_eq!(body["kind"], KIND);
        let h = standing(&[json!({"id": "x", "status": "open",
            "metadata": body["metadata"].clone()})])
        .unwrap();
        assert_eq!(h.held_by, "agent-claude");
        assert_eq!(h.since, Some(now));
        assert_eq!(h.until, Some(at("2026-11-07T19:30:00Z")));
    }

    /// THE OVERDUE ALARM names the hold and how late it is, is urgent,
    /// and carries the hold's id so it is filed once.
    #[test]
    fn a_hold_past_its_bound_becomes_one_urgent_alarm_naming_it() {
        let h = standing(&[hold_row(
            "95401d82-aaaa",
            "2026-11-07T18:00:00Z",
            "2026-11-07T19:30:00Z",
        )])
        .unwrap();
        let body = overdue_alarm_body(&h, at("2026-11-07T20:15:00Z"), "emp-x");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["metadata"][ALARM_KEY], "95401d82-aaaa");
        let title = body["title"].as_str().unwrap();
        assert!(
            title.starts_with("GATES HELD PAST THEIR BOUND: station-hold 95401d82"),
            "{title}"
        );
        let detail = body["metadata"]["detail"].as_str().unwrap();
        assert!(detail.contains("45 minute(s) past it"), "{detail}");
        assert!(detail.contains("never lifts itself"), "{detail}");
        assert!(alarm_filed(std::slice::from_ref(&body), "95401d82-aaaa"));
        assert!(!alarm_filed(&[body], "another"));
        assert!(!alarm_filed(&[json!({"metadata": {}})], "95401d82-aaaa"));
    }

    /// One by-id read of a hold, as `GET /api/jobs/{id}` answers it.
    fn hold_read(status: &str, release: &str) -> Value {
        json!({"data": {"id": "f7ccc64c-79ba", "kind": KIND, "status": status,
        "metadata": {"station": STATION},
        "steps": [
            {"id": "s-release", "title": RELEASE_STEP, "spec_slug": RELEASE_STEP, "status": release},
            {"id": "s-released", "title": "released", "spec_slug": "released",
                "status": if status == "open" { "ready" } else { "completed" }},
        ]}})
    }

    /// A fake read: answers `reads` in order, then the last one forever.
    fn reads(seq: Vec<Value>) -> impl FnMut() -> std::future::Ready<Result<Value>> {
        let mut seq = std::collections::VecDeque::from(seq);
        move || {
            let next = if seq.len() > 1 {
                seq.pop_front()
            } else {
                seq.front().cloned()
            };
            std::future::ready(next.context("no read"))
        }
    }

    /// BACKLOG 07a053ff (2026-11-11, station-hold f7ccc64c): completing
    /// `release` does not close the hold — the dispatcher completes its
    /// `released` outcome step off `step.ready.*`, 238 ms later that
    /// time and with no upper bound. The verb read back inside that
    /// window and said NOT released (exit 1) for a hold it had released.
    /// A read that lags one pass must still settle `Released`.
    #[tokio::test(start_paused = true)]
    async fn a_release_whose_terminal_closes_a_pass_later_is_released() {
        let got = settle(
            reads(vec![
                hold_read("open", "completed"),
                hold_read("closed", "completed"),
            ]),
            RELEASE_SETTLE,
            RELEASE_POLL,
        )
        .await
        .unwrap();
        assert_eq!(got, Settled::Released);
    }

    /// Past the bound with `release` completed and the packet still open,
    /// the verdict is the pending close — never NOT released.
    #[tokio::test(start_paused = true)]
    async fn a_terminal_that_never_closes_is_a_pending_close_not_a_refusal() {
        let started = tokio::time::Instant::now();
        let got = settle(
            reads(vec![hold_read("open", "completed")]),
            RELEASE_SETTLE,
            RELEASE_POLL,
        )
        .await
        .unwrap();
        assert_eq!(got, Settled::ClosePending);
        assert!(
            started.elapsed() >= RELEASE_SETTLE,
            "{:?}",
            started.elapsed()
        );
        let said = close_pending_line("f7ccc64c-79ba-4e6c");
        for want in [
            "release recorded on station-hold f7ccc64c",
            "`released` terminal",
            "dispatcher",
            "the launchers still read it as held",
        ] {
            assert!(said.contains(want), "{want:?} missing from: {said}");
        }
        assert!(!said.contains("NOT released"), "{said}");
        assert_eq!(CLOSE_PENDING_EXIT, crate::prove::NOT_YET_EXIT);
    }

    /// A `release` step the write did not complete is a real refusal,
    /// judged on the first read — the step PUT is synchronous, so
    /// waiting would only delay the truth.
    #[tokio::test(start_paused = true)]
    async fn a_release_step_left_active_is_not_released_at_once() {
        let started = tokio::time::Instant::now();
        let got = settle(
            reads(vec![hold_read("open", "active")]),
            RELEASE_SETTLE,
            RELEASE_POLL,
        )
        .await
        .unwrap();
        assert_eq!(got, Settled::NotReleased("active".into()));
        assert_eq!(started.elapsed(), std::time::Duration::ZERO);
        assert_eq!(
            read_release(&hold_read("closed", "completed")),
            ReleaseRead::Closed
        );
        assert_eq!(
            read_release(&hold_read("open", "completed")),
            ReleaseRead::Recorded
        );
    }
}
