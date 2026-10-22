//! `GET /api/yard/regions?window=24h` — the IT system map's KPI read
//! (design 0524fc95, decided 2026-09-19; car 1).
//!
//! Nine regions — dock, gates, track, shed, arrivals, garage,
//! receiving, marshalling, shop-floor — each with a count, a
//! clear/attention/troubled
//! state and a trend over the window, computed ONCE here from the rows
//! this process already reads for the yard status, so the map (car 2),
//! the yard and `boss orient` answer with one voice. The aggregation is
//! [`crate::regions::regions`], pure and unit-tested; this handler is
//! the adapter that reads the rows.
//!
//! THE SAME PASS AS THE YARD STATUS. The status and the map used to be
//! two client-side derivations of six reads; here the map is a function
//! of the status's own pass ([`super::yard::read_yard`]) plus the
//! windowed tails the trends need — the closed trains, cars and
//! gate-runs of the last two windows, the receiving yard's inbound
//! packets, the marshalling stations' load and flow, and the shop
//! floor's runs, crews and declared capacity. Every read
//! that cannot answer states so (`None` → a troubled region with
//! `count: null`), never an empty region that reads as clear.

use super::*;

use crate::membership_replay::{FlowWindows, MapFlowReading};
use crate::regions::{self, RegionInputs, RunnerHost, StationReading};
use crate::yard;

/// How many rows each windowed tail may hold. A limit is not a filter:
/// the reads narrow in the query (`closed_since`, `metadata_contains`)
/// and this only caps the page. Measured 2026-09-19 at ~17 arrivals a
/// day and ~112 gate-runs in two days, a week-wide window (the maximum,
/// two weeks of tail) fits under it.
const TAIL_WINDOW: i64 = 400;

#[derive(Debug, Deserialize, Default)]
pub(super) struct RegionsQuery {
    window: Option<String>,
}

pub(super) async fn yard_regions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<RegionsQuery>,
) -> Response {
    // A window that cannot be read is refused, not defaulted: a map
    // asked for a week that answered a day would be a confident wrong
    // trend.
    let window_hours = match regions::parse_window(q.window.as_deref()) {
        Ok(h) => h,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    let rows = match read_map(&state, &user, now, window_hours).await {
        Ok(rows) => rows,
        Err(resp) => return resp,
    };
    let mut map = regions::Regions {
        outranks: rows.outranks(now),
        next_up: Some(read_next_up(&state, &user, &rows, now).await),
        ..regions::regions(&rows.inputs(now, window_hours))
    };
    let sensor =
        super::sensor_regions::read(&state, &user, now, window_hours, rows.reads_every_packet)
            .await;
    map.regions = map
        .regions
        .into_iter()
        .map(|r| {
            if r.name == "sensors" {
                sensor.clone()
            } else {
                r
            }
        })
        .collect();
    // THE OBSERVED-UNDECLARED READING (design e765b3fc §2b; M1 read it,
    // R2 made it judge): every route the moves record saw taken in this
    // window, judged against the routes derived now from the protocols
    // and the declared hand-offs. The window ends at this read's `now`,
    // the injected clock's, as `/api/yard/moves` reads the same crossings
    // (#1190) and as `with_undeclared` below already judged them: until
    // backlog 5fb858ea it started at the machine's clock minus the
    // window, so under a frozen test clock it read nothing once the real
    // clock ran a window past the fixture. A failed read of either
    // leaves the reading null and judges nothing —
    // never an empty list, which would say every move took a declared
    // route.
    //
    // A caller whose scope reads no packets — a request with no
    // identity is one — gets it NOT READ, null like an unwired record
    // (backlog e5f7b51e, its note of 2026-09-27). The moves record is
    // not scoped by packet, so until this such a caller was answered
    // the empty map with every undeclared crossing hung on it, count
    // and last instant: the half-a-gate the borders' machine firings
    // were withheld for.
    //
    // AND SO DOES A CALLER WHOSE SCOPE READS SOME PACKETS BUT NOT ALL
    // (backlog 070de88c, item 2). The record counts routes, not packets:
    // a crossing cannot be narrowed to the caller's packets without a
    // read per move, so below a full scope the reading is withheld,
    // exactly as `/api/yard/moves` refuses such a caller its rows.
    //
    // The routes are derived ONCE for both readings that judge against
    // them: this one and the automation reading below. The crossings
    // are read FIRST, because a dispatch among them may name a version
    // its packet is pinned to, and the routes walk that version too
    // (backlog df62a1eb).
    let crossings = match state.yard_moves.as_ref() {
        Some(feed) if rows.reads_every_packet => {
            let since = now - chrono::Duration::hours(window_hours);
            Some(feed.store.crossings(since).await.ok())
        }
        _ => None,
    };
    let pinned = crate::routes_every_stand::pinned_versions(
        crossings
            .iter()
            .flatten()
            .flatten()
            .map(|c| c.dispatch.as_ref()),
    );
    let routes = super::routes::derived(&state, None, &pinned).await.ok();
    let map = match crossings {
        Some(crossings) => crate::moves::with_undeclared(map, crossings, routes.as_ref(), now),
        None => map,
    };
    // WHO MOVED THE WORK (backlog 9ca41c08, design 393bdef6 car 1): per
    // region downstream of the shop floor, the completions in the window
    // of the steps that leave it, by the class of actor that completed
    // each. Read off the caller's own rows — scoped as every other count
    // here is — plus the steps of the gate-runs that closed in the
    // window, which the map's pass reads as rows only and whose
    // `record-verdict` is how a packet leaves the gates.
    let w = regions::Windows::of(now, window_hours);
    let verdicts = match gate_run_steps(&state, &rows.gate_runs, &w).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let stepped: Vec<&(Job, Vec<Step>)> = rows
        .read
        .open_trains
        .iter()
        .chain(&rows.closed_trains)
        .chain(&rows.cars)
        .chain(rows.publish_packets.iter().flatten())
        .chain(&verdicts)
        .collect();
    let map = regions::automation::with_automation(map, routes.as_ref(), &stepped, &w);
    let mut v = with_now(map, now);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("flow_as_of".to_string(), serde_json::json!(rows.flow_as_of));
    }
    Json(v).into_response()
}

/// The steps of each gate-run that CLOSED in the current window — the
/// only ones whose verdict step can have completed in it (completing
/// `record-verdict` is what closes a run). Read here, for the automation
/// reading alone, rather than in [`read_map_as`], which the borders and
/// the mover share and which needs the runs as rows only. A failed read
/// is refused like every step read of the map's pass, never a gate
/// region that reads as moved by nobody.
async fn gate_run_steps<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    gate_runs: &[Job],
    w: &regions::Windows,
) -> Result<Vec<(Job, Vec<Step>)>, Response> {
    let mut out = Vec::new();
    for job in gate_runs.iter().filter(|j| {
        j.status == JobStatus::Closed && regions::closed_at(j).is_some_and(|at| w.current(at))
    }) {
        match state.jobs.list_steps(&job.id).await {
            Ok(steps) => out.push((job.clone(), steps)),
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response());
            }
        }
    }
    Ok(out)
}

/// Everything ONE pass over the map's rows read, owned. Both
/// read-models over that pass — the regions (design 0524fc95) and the
/// borders (`super::borders`, design d2154293) — are functions of THESE
/// rows, never a second set of queries free to disagree with them
/// (which is the defect the regions read was built to end).
pub(super) struct MapRows {
    internal_protocols: Option<regions::PinnedProtocols>,
    read: super::yard::YardRead,
    closed_trains: Vec<(Job, Vec<Step>)>,
    cars: Vec<(Job, Vec<Step>)>,
    gate_runs: Vec<Job>,
    /// Every inbound row, open ones with their steps — see
    /// [`read_inbound`].
    inbound: Option<Vec<(Job, Vec<Step>)>>,
    stations: Option<Vec<StationReading>>,
    /// THE RUNNERS' EVIDENCE (design d2154293, car 5). A runner leaves
    /// no heartbeat this process can read — only the ops-requests it
    /// answered. `None` on any failed read, which the machine reports
    /// as unknown rather than as an idle runner.
    ops_requests: Option<Vec<(Job, Vec<Step>)>>,
    /// WHICH HOSTS SHOULD HAVE ONE (backlog 49ed87b4). The evidence
    /// above can only name the runners that HAPPENED to answer
    /// something, so a host that died is drawn nowhere — and an absent
    /// glyph is indistinguishable from a runner that does not exist.
    /// The estate registry is the independent statement of what should
    /// be there. `None` on a failed read: one unknown machine, never an
    /// estate with no runners in it.
    runner_hosts: Option<Vec<RunnerHost>>,
    /// THE SHOP FLOOR'S RUNS (backlog 94c6ffd0): the `agent-run`
    /// packets open or closed within reach. Steps ride the OPEN ones
    /// only — they are what says a run has finished without reporting —
    /// and there are at most a registry's worth of those.
    agent_runs: Option<Vec<(Job, Vec<Step>)>>,
    /// The crews standing on it: the open `work-session` packets.
    sessions: Option<Vec<Job>>,
    /// THE PUBLISH REGION'S PACKETS (design cb38d806): the newest
    /// `publish-to-github` packets with their steps — the pull request,
    /// its scan reading and the mirror heads that say whether anyone
    /// merged it. NOT windowed: a PR nobody merged is what the region
    /// exists to show, and it outlives every window. `None` on a failed
    /// read — a troubled region, never a mirror that reads as current.
    publish_packets: Option<Vec<(Job, Vec<Step>)>>,
    /// Whether `agent_runs` holds every OPEN run rather than a page cut
    /// at [`MAX_LIMIT`]: the top board reads a missing open run as a
    /// claim with no run, so it judges only a whole read (backlog
    /// 8e9d3723). The closed tail beside them is read apart and does
    /// not cut them (backlog e2519eca).
    agent_runs_whole: bool,
    /// The bound those runs are read against, from the agents registry.
    run_capacity: Option<usize>,
    /// Every id an agent acts as — the agents registry's ids and their
    /// aliases (backlog 8e9d3723): the top board judges a claimed
    /// step's run only where an agent holds it. `None` when the registry
    /// is unwired or unread.
    agent_ids: Option<std::collections::BTreeSet<String>>,
    /// THE DOCK'S ORDERING EDGES (backlog 4142d821): each predecessor a
    /// parked car declares, read by id the way the conductor reads it —
    /// found, absent, or unreadable, one reading per id.
    predecessors: Vec<(String, crate::car::Predecessor)>,
    /// WHAT OUTRANKS REGULAR ORDER (backlog 74569e94, design ea906603
    /// Q1): the open real packets of each priority that drains before
    /// standard, with their steps — see [`read_outranking`]. `None` on
    /// a failed read: the board says it could not read the list, never
    /// that nothing outranks.
    outranking: Option<Vec<(Job, Vec<Step>)>>,
    /// THE CALLER'S POLICY SCOPE READS NO PACKETS (backlog e5f7b51e) —
    /// which is what a request with no identity is. The rows above are
    /// then the empty map; a read beside them that is not scoped by
    /// packet (the borders' machine firing records) must withhold its
    /// half from this caller too, or the empty map is only half a gate.
    /// Read off the TRANSLATED scope (`JobScope::None`), not the policy's
    /// answer: a department grant held outside its department is a
    /// predicate that reads no packets (review 500f8a23, backlog 0964ba80).
    pub(super) reads_no_packets: bool,
    /// THE CALLER'S POLICY SCOPE READS EVERY PACKET (backlog 070de88c):
    /// `JobScope::All`, which the mover's own pass is too. A read beside
    /// the rows that is not scoped by packet — the moves record's
    /// crossings, the estate registry's hosts — is made only for such a
    /// caller: a NARROWED scope gets its own packets in the rows above
    /// and nothing that would describe everyone else's.
    pub(super) reads_every_packet: bool,
    /// When the flow reading the stations count from was taken — `None`
    /// when none was read. Shared between map reads for its lifetime
    /// (backlog b852ec37), so both read-models state it beside `now`.
    pub(super) flow_as_of: Option<chrono::DateTime<chrono::Utc>>,
}

impl MapRows {
    /// The top board's first row, over this pass's own rows: the
    /// outranking packets, the stations the marshalling region was
    /// judged on for where each stands, and the shop floor's open runs
    /// for whether a claimed step is being worked (backlog 8e9d3723).
    ///
    /// The runs are handed over only when they were read whole AND by a
    /// caller who reads every packet: a run outside a narrowed scope, or
    /// past a cut page, would read as a claim with no run — a confident
    /// wrong "stalled". Otherwise every active step says the runs were
    /// unread. The agents registry is not scoped by packet, so it rides
    /// whenever it was read: it is what says whether a ready step with
    /// no agent block is held by a person (backlog e2519eca).
    pub(super) fn outranks(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<Vec<crate::outranks::Outrank>> {
        // The closed runs the floor's trend reads ride along; the board
        // reads only the open ones.
        let runs = match self.agent_runs.as_deref() {
            Some(open) if self.agent_runs_whole && self.reads_every_packet => {
                Some(crate::outranks::Runs { open })
            }
            _ => None,
        };
        self.outranking.as_deref().map(|packets| {
            crate::outranks::outranks(
                packets,
                self.stations.as_deref(),
                runs,
                self.agent_ids.as_ref(),
                now,
            )
        })
    }

    /// The rows as the pure aggregations take them.
    pub(super) fn inputs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        window_hours: i64,
    ) -> RegionInputs<'_> {
        RegionInputs {
            internal_protocols: self.internal_protocols.as_ref(),
            status: &self.read.status,
            dock_reading: self.read.dock_reading,
            open_trains: &self.read.open_trains,
            closed_trains: &self.closed_trains,
            cars: &self.cars,
            gate_runs: &self.gate_runs,
            inbound: self.inbound.as_deref(),
            stations: self.stations.as_deref(),
            conductor: Some(&self.read.health),
            ops_requests: self.ops_requests.as_deref(),
            runner_hosts: self.runner_hosts.as_deref(),
            agent_runs: self.agent_runs.as_deref(),
            sessions: self.sessions.as_deref(),
            publish_packets: self.publish_packets.as_deref(),
            run_capacity: self.run_capacity,
            predecessors: &self.predecessors,
            now,
            window_hours,
        }
    }
}

/// The map's read sequence. A caller whose scope is empty gets an
/// empty, well-formed map rather than a 403 — the same read gate every
/// queue surface keeps — so this answers empty rows, not an error.
pub(super) async fn read_map<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    now: chrono::DateTime<chrono::Utc>,
    window_hours: i64,
) -> Result<MapRows, Response> {
    let predicate = match state.policy.scope_of(user, controls::READ_JOB).await {
        Ok(p) => p,
        Err(e) => {
            return Err(e.into_response());
        }
    };
    // THE TRANSLATED SCOPE DECIDES, not the policy's own answer (review
    // 500f8a23 of backlog 0964ba80). A department grant held outside its
    // department is a predicate, not `None`, and translates to an EMPTY
    // scope: it read no packets, but was handed the reads beside them
    // that are gated on reading none. `Predicate::None` translates to
    // the same empty scope, so one test covers both.
    let scope = JobScope::from_predicate(user, &predicate);
    if matches!(scope, crate::port::JobScope::None) {
        return Ok(MapRows {
            internal_protocols: Some(Default::default()),
            read: empty_read(),
            closed_trains: Vec::new(),
            cars: Vec::new(),
            gate_runs: Vec::new(),
            inbound: Some(Vec::new()),
            stations: Some(Vec::new()),
            ops_requests: Some(Vec::new()),
            runner_hosts: Some(Vec::new()),
            agent_runs: Some(Vec::new()),
            agent_runs_whole: true,
            sessions: Some(Vec::new()),
            publish_packets: Some(Vec::new()),
            run_capacity: None,
            agent_ids: None,
            predecessors: Vec::new(),
            outranking: Some(Vec::new()),
            reads_no_packets: true,
            reads_every_packet: false,
            flow_as_of: None,
        });
    }
    // THE ESTATE'S OWN QUESTION (backlog 0964ba80): the registry's hosts
    // are drawn only for a caller `/api/estate/nodes` would answer — Read
    // on `estate` at scope all — and asked only of a caller who reads
    // every packet, the only one the hosts are drawn for at all.
    let estate = if matches!(scope, crate::port::JobScope::All) {
        estate_read(state, user).await
    } else {
        EstateRead::Withheld
    };
    read_map_as(
        state,
        user,
        scope,
        estate,
        now,
        window_hours,
        StationFlow::Counted,
    )
    .await
}

/// Whether a map read counts each station's flow — the membership
/// snapshot, the one read of the map whose cost grows with the log
/// (backlog 29f7e897; the step-flow cube until item ab09f1af, car 2).
pub(super) enum StationFlow {
    /// Counted, this window and the previous, from one snapshot: the
    /// regions and borders reads, whose marshalling trend and judgement
    /// read `served`.
    Counted,
    /// Not read: the mover's placement reads station MEMBERSHIP, which
    /// comes from the open packets, and never `served`. Measured
    /// 2026-10-19, its readings were about a third of the map's reads,
    /// and each paid for two passes it discarded. Every station then
    /// reads `served: None` with no reason named — a reading no page
    /// draws, because no page reads the map this way.
    NotRead,
}

/// Whether the map may draw the estate registry's hosts for a caller.
pub(super) enum EstateRead {
    /// Policy grants Read on `estate` at scope all — what the registry
    /// door itself requires. The mover, which reads as the service
    /// itself over the whole yard, reads it too.
    Granted,
    /// Refused — by the estate grant, or by a packet scope below full.
    /// The map draws no host: the empty list, never an unread estate.
    Withheld,
    /// The policy service could not be asked: which hosts should have a
    /// runner is unknown, and the plant says so.
    Unasked,
}

/// The estate grant, asked as `/api/estate/nodes` asks it — through
/// [`super::jobs::estate_read_refusal`], the one place that question is
/// written, so the map and the registry door cannot disagree about who
/// reads the estate. Its refusal is a 403; any other answer is the
/// policy service failing to answer.
async fn estate_read<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
) -> EstateRead {
    match super::jobs::estate_read_refusal(state, user, "the estate's runner hosts").await {
        None => EstateRead::Granted,
        Some(refusal) if refusal.status() == StatusCode::FORBIDDEN => EstateRead::Withheld,
        Some(_) => EstateRead::Unasked,
    }
}

/// The map's read sequence within a scope already decided — the caller's
/// ([`read_map`]), or the whole yard for the moves record's mover, which
/// reads as the service itself (`super::moves::run_mover`) so that its
/// placement is the partition every region is pinned to, not a policy
/// scope's slice of it.
pub(super) async fn read_map_as<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    scope: crate::port::JobScope,
    estate: EstateRead,
    now: chrono::DateTime<chrono::Utc>,
    window_hours: i64,
    flow: StationFlow,
) -> Result<MapRows, Response> {
    // Whether this caller reads every packet — the one answer every read
    // below that is not scoped by packet is gated on, and the value
    // `MapRows::reads_every_packet` carries to the read-models.
    let reads_every_packet = matches!(scope, crate::port::JobScope::All);
    // The cadence record's firings — the conductor machine's heartbeat
    // and NEXT UP's last board — only for a scope that reads every
    // packet, the rule `/api/yard/status` keeps (backlog d0058c92). The
    // mover reads as the service itself, at `All`, so its pass is whole.
    let firings = if reads_every_packet {
        super::yard::Firings::Read
    } else {
        super::yard::Firings::Withheld
    };
    let mut lap = Lap::start();
    // The stations' flow reading is STARTED here and joined where the
    // stations are judged (backlog b852ec37), so a map read that must
    // take one pays the longer of it and the rest of the pass rather
    // than their sum — measured 2026-10-21, ~12 s of snapshot beside
    // ~11 s of everything else. Only where a station registry is wired:
    // without one no station is judged and no flow is read.
    let flows = match flow {
        StationFlow::Counted if state.stations.is_some() => Some(start_flows(
            state.clone(),
            user.clone(),
            scope.clone(),
            window_hours,
        )),
        _ => None,
    };
    let read = read_yard(state, user, scope.clone(), firings, now).await?;
    lap.mark("yard");

    // The tails reach back two windows: this one and the previous.
    let reach = (now - chrono::Duration::hours(2 * window_hours)).date_naive();
    let tail =
        |kind: &str, status: Option<JobStatus>, contains: Option<serde_json::Value>| JobFilter {
            kind: Some(kind.to_string()),
            status,
            closed_since: Some(reach),
            metadata_contains: contains,
            scope: scope.clone(),
            ..Default::default()
        };

    // Closed trains: the arrivals, narrowed on `outcome` in the query
    // (the recent tail is overwhelmingly refused boards — see
    // `read_yard`'s arrived read), WITH steps for the time at CI and
    // the boarding instant; and the cancellations, rows only — the
    // arrivals region reads `boarded_jobs` and `closed_at` off the
    // metadata to tell a red train from a refused board.
    let mut closed_trains: Vec<(Job, Vec<Step>)> = Vec::new();
    let arrived = match state
        .jobs
        .list_jobs(
            &tail(
                "pr-train",
                None,
                Some(serde_json::json!({ "outcome": "arrived" })),
            ),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    for job in arrived
        .into_iter()
        .filter(|j| j.status == JobStatus::Closed)
    {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response());
            }
        };
        closed_trains.push((job, steps));
    }
    let cancelled = match state
        .jobs
        .list_jobs(
            &tail(
                "pr-train",
                None,
                Some(serde_json::json!({ "outcome": "cancelled" })),
            ),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    closed_trains.extend(
        cancelled
            .into_iter()
            .filter(|j| j.status == JobStatus::Closed)
            .map(|j| (j, Vec::new())),
    );
    lap.mark("closed_trains");

    // Cars: live OR closed within reach, in one read (`closed_since`
    // combines with `status` as OR), with steps — the shed is a step,
    // and so is the dock wait's start.
    let car_rows = match state
        .jobs
        .list_jobs(
            &tail("ship-a-change", Some(JobStatus::Open), None),
            MAX_LIMIT,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };
    let mut cars: Vec<(Job, Vec<Step>)> = Vec::with_capacity(car_rows.len());
    for job in car_rows {
        let steps = match state.jobs.list_steps(&job.id).await {
            Ok(steps) => steps,
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response());
            }
        };
        cars.push((job, steps));
    }

    // THE DOCK'S ORDERING EDGES (backlog 4142d821, design cf820810 Q7):
    // the predecessor each parked car declares, read by id — the question
    // the conductor asks before it boards the car, asked here so the dock
    // stops promising a train the conductor will refuse.
    let predecessors =
        read_predecessors(state, &scope, &regions::declared_edges(&read.status, &cars)).await;
    lap.mark("cars");

    // Gate-runs: rows only. `opened_at`, `closed_at` and `outcome` are
    // on the metadata, which is all the duration and the reds need.
    let gate_runs = match state
        .jobs
        .list_jobs(
            &tail("gate-run", Some(JobStatus::Open), None),
            TAIL_WINDOW,
            0,
        )
        .await
    {
        Ok((rows, _)) => rows,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    };

    // THE OPEN PACKETS, WITH THEIR STEPS, READ ONCE (design 62de32ae
    // decision 4). The stations are judged over them, and the same steps
    // say which inbound packets have been taken in — the line between
    // receiving and marshalling — so the partition costs no second read.
    // Unread → `None`, which the stations report as unread; the inbound
    // read then fetches its own steps rather than guess.
    let active = super::stations::active_rows(state.as_ref()).await;
    let open_packets =
        super::stations::resolved_open_packets(state.as_ref(), scope.clone(), &active)
            .await
            .ok();
    lap.mark("gate_runs_and_open_packets");

    // The receiving yard's inbound packets: the kinds come from the
    // workflow registry (`regions::inbound_kinds`, the receiving page's
    // own rule), real partition only as that page reads them. No
    // registry, or a failed read → `None`: the region says it could not
    // be read rather than counting nothing.
    let inbound = match state.kind_registry.as_ref() {
        Some(reg) => match reg.list_active(None).await {
            Ok(specs) => {
                let filter = JobFilter {
                    kinds: Some(regions::inbound_kinds(&specs)),
                    status: Some(JobStatus::Open),
                    closed_since: Some(reach),
                    partition: Some(boss_core::partition::Partition::Real),
                    scope: scope.clone(),
                    ..Default::default()
                };
                read_inbound(state, &filter, open_packets.as_deref()).await
            }
            Err(_) => None,
        },
        None => None,
    };
    lap.mark("inbound");

    // The marshalling stations: every station but the dock (a region of
    // its own), bound to the caller as `/api/stations/load` binds them,
    // over the caller's open packets; and each station's counted flow in
    // this window and the previous, from one membership snapshot — or
    // none, for a reader that places packets only (`flow`). Unread → `None`.
    //
    // Each standing step's wait is dated from its RECORDED ready flip —
    // the queue-age read's exact rows, never its `updated_at` lower bound
    // — so a station that served nothing can be judged on whether what
    // stands there is still inside its declared bound (backlog 6254e602).
    // A failed read keeps its error, so each standing step says the read
    // failed rather than that the record holds no onset (backlog
    // 35d29890: this read was `.ok()`, and blamed the record).
    let ready = state
        .jobs
        .queue_age(&scope)
        .await
        .map(|rows| {
            rows.into_iter()
                .filter(|row| row.exact)
                .map(|row| (row.step_id.to_string(), row.since))
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .map_err(|e| e.to_string());
    let stations = marshalling_stations(
        state,
        user,
        open_packets.as_deref(),
        ready.as_ref().map_err(String::as_str),
        flows,
    )
    .await;
    let (stations, flow_as_of) = match stations {
        Some((readings, as_of)) => (Some(readings), as_of),
        None => (None, None),
    };
    lap.mark("queue_age_and_stations");

    // THE RUNNERS' EVIDENCE (design d2154293, car 5). A runner leaves
    // no heartbeat this process can read — only the ops-requests it
    // answered — so the newest request of each verb the map draws a
    // runner for is fetched here, with its steps (the `disposition`
    // rides the `execute` step). A failed read is `None`, which the
    // machine reports as unknown rather than as an idle runner.
    //
    // AND WHICH HOSTS SHOULD HAVE A RUNNER (backlog 49ed87b4): the
    // estate registry's own rows, so a host that has answered nothing
    // — including one that died — is still a machine on the map. The
    // evidence read above is widened to the newest request of each such
    // host, because ANY verb it answered proves the loop polled.
    //
    // ONLY FOR A CALLER WHO READS EVERY PACKET (backlog 0f462796 item 1,
    // on the car of 070de88c). The registry is not scoped by packet — it
    // names every host and what it runs — so a narrowed scope is drawn
    // no host from it, the empty list the empty map already carries:
    // its plant shows the runners its OWN ops-requests evidence, and
    // never "the estate could not be read", which would be a failure's
    // words for a refusal.
    //
    // AND ONLY FOR A CALLER THE ESTATE DOOR WOULD ANSWER (backlog
    // 0964ba80): reading every packet is not Read on `estate`, which
    // `/api/estate/nodes` asks before it names a host — so a full packet
    // scope the estate grant refuses is drawn none either.
    let runner_hosts = match estate {
        _ if !reads_every_packet => Some(Vec::new()),
        EstateRead::Withheld => Some(Vec::new()),
        EstateRead::Unasked => None,
        EstateRead::Granted => match state.jobs.list_estate_nodes().await {
            Ok(nodes) => Some(regions::runner_hosts_of(&nodes)),
            Err(_) => None,
        },
    };
    let host_ids: Vec<String> = runner_hosts
        .iter()
        .flatten()
        .map(|h| h.id.clone())
        .collect();
    let ops_requests = newest_ops_requests(state, scope.clone(), reach, &host_ids).await;
    lap.mark("runners");

    // THE SHOP FLOOR (backlog 94c6ffd0): the runs in flight and the
    // ones that closed within reach, the sessions that dispatched them,
    // and the bound the agents registry declares. Each read answers
    // `None` on failure — the region then says it could not be read,
    // rather than drawing a shop with nobody in it.
    let (agent_runs, agent_runs_whole) = match read_agent_runs(state, scope.clone(), reach).await {
        Some((rows, whole)) => (Some(rows), whole),
        None => (None, false),
    };
    let sessions = state
        .jobs
        .list_jobs(
            &JobFilter {
                kind: Some(regions::SESSION_KIND.to_string()),
                status: Some(JobStatus::Open),
                scope: scope.clone(),
                ..Default::default()
            },
            TAIL_WINDOW,
            0,
        )
        .await
        .ok()
        .map(|(rows, _)| rows);
    // A registry that could not be read and one that declares no cap
    // are the same drawing — no bound on the card — and neither is a
    // number the map would have to invent. The same rows say which
    // holders are agents, for the top board (backlog 8e9d3723).
    let agent_rows = match state.agent_budget.as_ref() {
        Some(door) => door.agents.list().await.ok(),
        None => None,
    };
    let run_capacity = agent_rows.as_deref().and_then(regions::run_capacity);
    let agent_ids = agent_rows.map(|rows| {
        rows.into_iter()
            .flat_map(|r| std::iter::once(r.id).chain(r.aliases))
            .collect::<std::collections::BTreeSet<String>>()
    });

    // THE PUBLISH REGION (design cb38d806): the newest publish packets,
    // with steps. No `closed_since` — a pull request nobody merged is
    // exactly what this region shows, and it outlives any window; the
    // page cap below is the only bound, and it is generous against a
    // DAILY cadence.
    lap.mark("shop_floor");
    let publish_packets = read_publish_packets(state, scope.clone()).await;

    // WHAT OUTRANKS REGULAR ORDER (backlog 74569e94): narrowed in the
    // query, one priority at a time, every page of each.
    let outranking = read_outranking(state, scope.clone(), open_packets.as_deref(), &active).await;
    lap.mark("publish_and_outranking");

    // The open packets' pinned versions too: a step standing at a station
    // is judged against the bound ITS packet's protocol declares, never
    // today's active row (backlog 6254e602).
    let internal_protocols = read_internal_protocols(
        state.kind_registry.as_deref(),
        cars.iter()
            .map(|(job, _)| job)
            .chain(gate_runs.iter())
            .chain(open_packets.iter().flatten().map(|(job, _)| job)),
    )
    .await;
    lap.mark("protocols");
    lap.report_if_slow();
    Ok(MapRows {
        internal_protocols,
        read,
        closed_trains,
        cars,
        gate_runs,
        inbound,
        stations,
        ops_requests,
        runner_hosts,
        agent_runs,
        agent_runs_whole,
        sessions,
        publish_packets,
        run_capacity,
        agent_ids,
        predecessors,
        outranking,
        reads_no_packets: matches!(scope, crate::port::JobScope::None),
        reads_every_packet,
        flow_as_of,
    })
}

/// A map read slower than this names where its time went (backlog
/// b852ec37): on 2026-10-21 regions and borders ran 127-187 s and the
/// only way to find the cost was to time narrower endpoints from outside.
/// Silent below it, so a healthy map logs nothing per tick.
const SLOW_MAP_READ: std::time::Duration = std::time::Duration::from_secs(5);

/// Each phase of one map pass and its elapsed milliseconds — the
/// per-read profile the slow-read line carries.
struct Lap {
    started: std::time::Instant,
    last: std::time::Instant,
    phases: Vec<(&'static str, u128)>,
}

impl Lap {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        self.phases
            .push((phase, now.duration_since(self.last).as_millis()));
        self.last = now;
    }

    fn report_if_slow(&self) {
        let total = self.started.elapsed();
        if total >= SLOW_MAP_READ {
            tracing::warn!(
                total_ms = total.as_millis() as u64,
                phases = ?self.phases,
                "a yard map read was slow; phases name where its time went"
            );
        }
    }
}

/// The open REAL packets whose priority outranks regular order
/// ([`crate::outranks::outranking_priorities`]), each with its steps.
///
/// Narrowed in the query — `JobFilter::priority`, which the Postgres
/// adapter ignored until this read became its first caller — and read
/// to the last page ([`list_every`]): measured 2026-09-27, 17 of 460 open
/// packets were urgent, and a board drawn off one page of the open set
/// would be a floor that does not say it is one. Real partition only: a
/// simulated emergency is the sim's work, not the operator's. The steps
/// come from `open`, the pass's resolved open packets, where that page
/// carried the packet; a packet past it is read on its own and resolved
/// against `active` the way that pass resolves its own, so whether a
/// ready step carries an agent block (backlog e2519eca) is one answer
/// whichever read carried it. `None` on any failed read.
async fn read_outranking<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: crate::port::JobScope,
    open: Option<&[(Job, Vec<Step>)]>,
    active: &std::collections::BTreeMap<String, crate::registry::WorkflowSpec>,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let steps_of: std::collections::HashMap<String, &Vec<Step>> = open
        .into_iter()
        .flatten()
        .map(|(j, s)| (j.id.to_string(), s))
        .collect();
    let mut out = Vec::new();
    for priority in crate::outranks::outranking_priorities() {
        let filter = JobFilter {
            status: Some(JobStatus::Open),
            priority: Some(priority),
            partition: Some(boss_core::partition::Partition::Real),
            scope: scope.clone(),
            ..Default::default()
        };
        for job in list_every(state, &filter).await.ok()? {
            let steps = match steps_of.get(&job.id.to_string()) {
                Some(steps) => (*steps).clone(),
                None => crate::agent_spec::resolved_steps(
                    &state.jobs.list_steps(&job.id).await.ok()?,
                    active.get(&job.kind),
                ),
            };
            out.push((job, steps));
        }
    }
    Some(out)
}

/// NEXT UP (backlog 74569e94, design ea906603 Q3): the yard status this
/// pass built — the boarding block, the trains, the gates — plus the two
/// reads that live elsewhere, folded by [`crate::next_up::next_up`].
///
/// The dispatcher's schedule is read AS THE VIEWER (the dispatcher
/// judges who may read it) and the credentials registry beside it,
/// concurrently. Either failing, or not wired, is a row saying so —
/// never a row missing. A caller whose scope reads no packets gets the
/// empty map's empty row and neither read is made for it: they are not
/// scoped by packet, so making them would be half a gate (e5f7b51e).
///
/// NOR FOR ONE WHOSE SCOPE READS SOME PACKETS BUT NOT ALL (backlog
/// 0964ba80) — the rule the crossings and the estate hosts keep
/// (070de88c). The schedule names every rule the dispatcher runs and the
/// registry every credential and where it is stored; neither narrows to
/// a caller's packets. Such a caller keeps the rows its own packets
/// give, and the two sources each stand as a row saying WITHHELD.
async fn read_next_up<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    rows: &MapRows,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<crate::next_up::NextEvent> {
    if rows.reads_no_packets {
        return Vec::new();
    }
    if !rows.reads_every_packet {
        return crate::next_up::next_up(&crate::next_up::NextUpInputs {
            yard: &rows.read.status,
            schedule: &Ok(Vec::new()),
            credentials: &Ok(Vec::new()),
            withheld: Some(NEXT_UP_WITHHELD),
            now,
        });
    }
    let schedule = async {
        match state.dispatcher_schedule.as_ref() {
            Some(reader) => reader.schedule(user).await,
            None => Err("no dispatcher schedule reader is wired to this jobs API".to_string()),
        }
    };
    let credentials = async {
        match state.credentials.as_ref() {
            Some(registry) => registry
                .list()
                .await
                .map_err(|e| format!("reading the credentials registry: {e}")),
            None => Err("no credentials registry is wired to this jobs API".to_string()),
        }
    };
    let (schedule, credentials) = tokio::join!(schedule, credentials);
    crate::next_up::next_up(&crate::next_up::NextUpInputs {
        yard: &rows.read.status,
        schedule: &schedule,
        credentials: &credentials,
        withheld: None,
        now,
    })
}

/// Why NEXT UP withholds the schedule and the credentials registry from
/// a narrowed scope (backlog 0964ba80).
const NEXT_UP_WITHHELD: &str = "this caller's policy scope does not read every packet, and \
                                this source is not scoped by packet";

/// Each declared predecessor, read the way the conductor reads it
/// (`edge_hold` in boss-cli's conductor): a row that came back is
/// `Found` with its steps, a row the store does not hold is `Absent`,
/// and every failure is `Unreadable` — which the dock, like the
/// conductor, counts as boardable and SAYS it could not judge. One
/// failed read therefore never fails the map, and never reads as "no
/// such car" either.
///
/// READ BY ID, JUDGED AGAINST THE CALLER'S SCOPE (review 500f8a23 of
/// backlog 0964ba80). The id comes off the caller's own car, but the row
/// it names may be another owner's, and the hold reason names a found
/// predecessor by branch and says where it stands. A row the caller's
/// scope does not admit is `Unscoped`: judged by the same predicates, so
/// the car still reads as held, and described by nothing.
async fn read_predecessors<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: &crate::port::JobScope,
    ids: &[String],
) -> Vec<(String, crate::car::Predecessor)> {
    use crate::car::Predecessor;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let reading = match parse_job_id(id) {
            // The edge is ref-checked and prefix-normalised at the write,
            // so a stored value that is not an id is a malformed stamp —
            // not an answer about any Job.
            None => Predecessor::Unreadable(format!("'{id}' is not a job id")),
            Some(job_id) => match state.jobs.get_job(&job_id).await {
                Ok(None) => Predecessor::Absent,
                Err(e) => Predecessor::Unreadable(e.to_string()),
                Ok(Some(job)) => match state.jobs.list_steps(&job.id).await {
                    Ok(steps) if scope.admits(&job) => {
                        Predecessor::Found(regions::packet_value(&job, &steps))
                    }
                    Ok(steps) => Predecessor::Unscoped(regions::packet_value(&job, &steps)),
                    Err(e) => Predecessor::Unreadable(e.to_string()),
                },
            },
        };
        out.push((id.clone(), reading));
    }
    out
}

/// How many publish packets the map reads. The cadence is daily, so
/// this is a season of them — and a limit is not a filter: the merge
/// test below reads every mirror head in the page, and a page that
/// ended mid-history could call a merged PR open. Well clear of that.
const PUBLISH_TAIL: i64 = 60;

/// The publish packets, newest first, with their steps — the pull
/// request and its snapshot ride `open-pr`, the scan reading rides
/// `read-checks`, and the judgement is `judge-checks`'s status. `None`
/// on any failed read: the region then says the read failed rather
/// than drawing a mirror with nothing outstanding.
async fn read_publish_packets<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: crate::port::JobScope,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let filter = JobFilter {
        kind: Some(regions::PUBLISH_KIND.to_string()),
        scope,
        ..Default::default()
    };
    let (rows, _) = state.jobs.list_jobs(&filter, PUBLISH_TAIL, 0).await.ok()?;
    let mut out = Vec::with_capacity(rows.len());
    for job in rows {
        let steps = state.jobs.list_steps(&job.id).await.ok()?;
        out.push((job, steps));
    }
    Some(out)
}

/// EVERY ROW A FILTER MATCHES, page after page, each id once — the
/// shared [`crate::list_every::list_every`] at the jobs API's own page
/// size. A limit is not a filter. The receiving read took ONE page of
/// `MAX_LIMIT` and counted what came back: measured 2026-09-24, 565
/// backlog-items had closed in two days and ~230 stood open, so a week's
/// window was past the page and the region's count was a floor that did
/// not say it was one (the receiving yard's own board said so in words —
/// "only 500 were read; the counts below are floors"; design 62de32ae
/// decision 4). The loop that answers it lived here until the station
/// reads needed it too (backlog f71d1e81), and is now the one copy.
async fn list_every<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    filter: &JobFilter,
) -> Result<Vec<Job>, crate::port::JobsError> {
    crate::list_every::list_every(state.jobs.as_ref(), filter, MAX_LIMIT).await
}

/// The receiving yard's inbound packets — every one the filter matches,
/// never a page of them ([`list_every`]) — with the steps of each OPEN
/// one: [`regions::taken_in`] reads them to say whether the packet is
/// still in receiving or has crossed into marshalling (design 62de32ae
/// decision 4). The steps come from `open`, the open packets this pass
/// already read; a row that page did not carry is read on its own, so a
/// packet is never judged on steps nobody read. A CLOSED row (every one
/// closed within two windows) carries its steps too: it stands in no
/// region, but its intake step's completion is a crossing of the
/// receiving -> marshalling border ([`regions::taken_in_at`]), and a
/// packet taken in and closed inside the window would otherwise be a
/// crossing nobody counted — the rail used to count the close instead,
/// a different event on most kinds (design 62de32ae). `None` on any
/// failed read: an unread intake is troubled, never an empty one.
async fn read_inbound<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    filter: &JobFilter,
    open: Option<&[(Job, Vec<Step>)]>,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let rows = list_every(state, filter).await.ok()?;
    let steps_of: std::collections::HashMap<String, &Vec<Step>> = open
        .into_iter()
        .flatten()
        .map(|(j, s)| (j.id.to_string(), s))
        .collect();
    let mut out = Vec::with_capacity(rows.len());
    for job in rows {
        let steps = match steps_of.get(&job.id.to_string()) {
            Some(steps) if job.status == JobStatus::Open => (*steps).clone(),
            _ => state.jobs.list_steps(&job.id).await.ok()?,
        };
        out.push((job, steps));
    }
    Some(out)
}

/// The shop floor's runs: open ones WITH their steps (a run that
/// finished without reporting is a `building` done over a `reported`
/// still open), plus the ones that closed within reach as rows — the
/// trend reads its two instants off the metadata. `None` on any failed
/// read: an unread floor is troubled, never empty. Beside the rows,
/// whether the OPEN runs are every one the filter matched — the top
/// board must not read a run past a cut page as no run at all (backlog
/// 8e9d3723).
///
/// TWO READS, EACH WITH ITS OWN TOTAL (backlog e2519eca item 4). The
/// open runs and the departed ones were one page of [`TAIL_WINDOW`],
/// so two busy days of FINISHED runs cut the page and every claim on
/// the board read unread — measured 2026-10-10 05:10Z at 9 open and 288
/// closed within reach, 297 of the 400. The open runs are bounded by
/// the agents registry's cap, so a page of [`MAX_LIMIT`] holds them;
/// the departed tail, which only the trend reads, keeps its window.
async fn read_agent_runs<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: crate::port::JobScope,
    reach: chrono::NaiveDate,
) -> Option<(Vec<(Job, Vec<Step>)>, bool)> {
    let (mut out, whole) = read_open_runs(state, scope.clone()).await?;
    let departed = JobFilter {
        kind: Some(crate::agent_budget::RUN_KIND.to_string()),
        scope,
        terminal: Some(true),
        closed_since: Some(reach),
        ..Default::default()
    };
    let (closed, _) = state.jobs.list_jobs(&departed, TAIL_WINDOW, 0).await.ok()?;
    out.extend(closed.into_iter().map(|job| (job, Vec::new())));
    Some((out, whole))
}

/// The OPEN `agent-run` packets with their steps, and whether the page
/// held every one the filter matched — the half of the shop floor a
/// claim's run is judged on ([`crate::outranks::claim_run`]). The top
/// board reads it here and the caller's own queue (`assignments?for=me`,
/// backlog e2519eca item 2) reads it through the same function, so both
/// judge a claim over one read. `None` on any failed read.
pub(super) async fn read_open_runs<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
) -> Option<(Vec<(Job, Vec<Step>)>, bool)> {
    let (rows, whole) = list_open_runs(state, scope).await?;
    Some((with_steps(state, rows).await?, whole))
}

/// The OPEN runs with their steps only when the page held every one —
/// `None` for a cut page, as for a failed read — for a reader that
/// judges nothing on a cut page (the caller's own queue). Wholeness is
/// decided from the list's total BEFORE any run's steps are read
/// (backlog 1927aca5, landed-review da21596d): reading them first cost
/// one sequential read per run on the page, up to [`MAX_LIMIT`], on
/// every `for=me` with an active row, to answer unread. The top board
/// keeps [`read_open_runs`], since the shop floor draws a cut page too.
pub(super) async fn read_whole_open_runs<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let (rows, whole) = list_open_runs(state, scope).await?;
    if !whole {
        return None;
    }
    with_steps(state, rows).await
}

/// One page of the OPEN runs, and whether it held every one the filter
/// matched.
async fn list_open_runs<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
) -> Option<(Vec<Job>, bool)> {
    let open = JobFilter {
        kind: Some(crate::agent_budget::RUN_KIND.to_string()),
        status: Some(JobStatus::Open),
        scope,
        ..Default::default()
    };
    let (rows, total) = state.jobs.list_jobs(&open, MAX_LIMIT, 0).await.ok()?;
    let whole = usize::try_from(total).is_ok_and(|t| t <= rows.len());
    Some((rows, whole))
}

/// Each run with its steps; `None` when any steps read fails.
async fn with_steps<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    rows: Vec<Job>,
) -> Option<Vec<(Job, Vec<Step>)>> {
    let mut out = Vec::with_capacity(rows.len());
    for job in rows {
        let steps = state.jobs.list_steps(&job.id).await.ok()?;
        out.push((job, steps));
    }
    Some(out)
}

/// The empty pass: every read answered, nothing in it.
fn empty_read() -> super::yard::YardRead {
    super::yard::YardRead {
        status: yard::build_status(yard::YardInputs::default()),
        dock_reading: yard::Reading::Read,
        health: yard::conductor_health(None, None, None, None, None),
        gate_runs_truncated: false,
        open_trains: Vec::new(),
    }
}

/// The newest ops-request of each verb [`regions::runner_verbs`] names
/// and of each host the estate registry declares a runner on, with its
/// steps. One windowed read, then one `list_steps` per verb and per
/// host — bounded by the runner table and the estate, not by how busy
/// either has been. `None` on ANY failure: an unread runner is not an
/// idle one.
async fn newest_ops_requests<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    reach: chrono::NaiveDate,
    hosts: &[String],
) -> Option<Vec<(Job, Vec<Step>)>> {
    let filter = JobFilter {
        kind: Some("ops-request".to_string()),
        status: Some(JobStatus::Open),
        closed_since: Some(reach),
        scope,
        ..Default::default()
    };
    let (rows, _) = state.jobs.list_jobs(&filter, TAIL_WINDOW, 0).await.ok()?;
    let opened = |j: &Job| {
        j.metadata
            .get("opened_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let mut out = Vec::new();
    for verb in regions::runner_verbs() {
        let newest = rows
            .iter()
            .filter(|j| j.metadata.get("verb").and_then(serde_json::Value::as_str) == Some(verb))
            .max_by_key(|j| opened(j));
        if let Some(job) = newest {
            let steps = state.jobs.list_steps(&job.id).await.ok()?;
            out.push((job.clone(), steps));
        }
    }
    // The same rows again, newest per HOST — whatever verb it was. A
    // request already collected for its verb is not fetched twice.
    for host in hosts {
        let newest = rows
            .iter()
            .filter(|j| {
                j.metadata.get("host").and_then(serde_json::Value::as_str) == Some(host.as_str())
            })
            .max_by_key(|j| opened(j));
        if let Some(job) = newest {
            if out.iter().any(|(j, _): &(Job, Vec<Step>)| j.id == job.id) {
                continue;
            }
            let steps = state.jobs.list_steps(&job.id).await.ok()?;
            out.push((job.clone(), steps));
        }
    }
    Some(out)
}

/// The station readings the marshalling region is judged on, or `None`
/// when the registry (or the packets under it) could not be read.
///
/// `packets` are the caller's open packets with their steps, resolved
/// against each kind's active row — the SAME member set the load and
/// the queue count (this read listed steps raw until backlog 6c06ef65,
/// so the map drew the agent station 43 short). [`read_map`] reads them
/// once and hands them here and to the inbound read.
///
/// `ready` is each outstanding step's RECORDED ready flip, by step id
/// (the queue-age read's exact rows): the onset a standing step's wait
/// is dated from. `Err` with the read's error when that read failed —
/// every standing step is then undated, says so, and none can be shown
/// to be inside its bound.
///
/// Beside the readings, the instant the flow reading they count from was
/// taken — `None` when no flow was read. The reading is SHARED between
/// map reads for its lifetime (backlog b852ec37, [`shared_flows`]), so
/// it is not this read's `now`, and the payload says which it is.
async fn marshalling_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    packets: Option<&[(Job, Vec<Step>)]>,
    ready: Result<&std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>, &str>,
    flows: Option<tokio::task::JoinHandle<Arc<MapFlowReading>>>,
) -> Option<(Vec<StationReading>, Option<chrono::DateTime<chrono::Utc>>)> {
    let reg = state.stations.as_ref()?;
    let specs = effective_stations(state.as_ref(), reg).await.ok()?;
    let packets = packets?;
    let flows = match flows {
        Some(reading) => Some(reading.await.unwrap_or_else(|e| {
            // The task did not finish (it panicked): no reading, and
            // every station says why rather than counting nothing.
            Arc::new(MapFlowReading {
                as_of: boss_clock_client::wall_now(),
                windows: Err(format!("the flow reading did not finish: {e}")),
            })
        })),
        None => None,
    };
    let flow_as_of = flows.as_ref().map(|f| f.as_of);
    let readings = Some(
        specs
            .iter()
            .filter(|spec| spec.name != "loading-dock")
            .filter_map(|spec| {
                let bound = spec.bind_self(self_id(user))?;
                let packets_here: Vec<&(Job, Vec<Step>)> = packets
                    .iter()
                    .filter(|(job, steps)| bound.predicate.matches(job, steps))
                    .collect();
                let standing: Vec<&Job> = packets_here.iter().map(|(job, _)| job).collect();
                let members: Vec<String> = standing.iter().map(|job| job.id.to_string()).collect();
                // The marshalling KPI's ages (design 62de32ae, decision 9),
                // per member, so the partition can narrow them.
                let opened = standing
                    .iter()
                    .filter_map(|job| Some((job.id.to_string(), regions::opened_at(job)?)))
                    .collect();
                let over_limit = bound
                    .wip_limit
                    .is_some_and(|l| members.len() as i64 > i64::from(l));
                let (served, flow_unread, previous_served) =
                    match flows.as_ref().map(|f| &f.windows) {
                        None => (None, None, None),
                        Some(Err(refused)) => (None, Some(refused.clone()), None),
                        Some(Ok(windows)) => station_served(windows, &spec.name),
                    };
                Some(StationReading {
                    name: bound.name.clone(),
                    over_limit,
                    members,
                    served,
                    flow_unread,
                    previous_served,
                    opened,
                    standing: standing_steps(&bound.predicate, &packets_here, ready),
                })
            })
            .collect(),
    );
    readings.map(|r| (r, flow_as_of))
}

/// THE MAP COUNTS FLOW ON THE CONSERVED MEMBERSHIP BASIS (item ab09f1af,
/// car 2), the reader `GET /api/stations/flow` answers by default: the
/// original packet history folded from a witnessed cut (design c195e83e),
/// bound to the caller and narrowed to the caller's scope. Until this car
/// the map counted the step-events cube, which is blind by design to a
/// predicate that binds the caller, names job tags or job metadata, or
/// names no step — every published station of those shapes drew `?`.
///
/// ONE SNAPSHOT for both windows, replayed twice (`replay_windows`), so a
/// crossing cannot land in the wider count alone and read as the previous
/// window's (backlog 29f7e897). The reader's bounds are the flow
/// surface's own; a read it refuses is that refusal, named on every
/// station, never a count. When only the wider read is refused — it holds
/// twice the window's rows — this window is read alone and the previous
/// one is unread. Wall clock, as the flow surface reads it: the window
/// is the instant the log's `created_at` is written on, not the sim's day.
async fn membership_flows<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    scope: &crate::port::JobScope,
    window_hours: i64,
) -> MapFlowReading {
    let wall = boss_clock_client::wall_now();
    MapFlowReading {
        as_of: wall,
        windows: flow_windows(state, user, scope, window_hours, wall).await,
    }
}

/// The map's flow reading, SHARED between map reads for its lifetime
/// (backlog b852ec37, `crate::reading_cache`). Measured 2026-10-21 from
/// the pod on a quiet API: the 48 h snapshot answered in 8.1-8.4 s and
/// the 24 h retry in 3.5 s, both refused on the byte bound — about twelve
/// seconds of every ~23 s regions and borders read, taken twice per map
/// tick (each read took its own) and once per cancelled poll under load,
/// when the same reads ran 127-187 s and the edge cut them at 100.
///
/// Keyed on everything the reading is a function of besides the log: the
/// caller (a station's predicate may bind them), the translated scope and
/// the window. A reading served is the one a read inside its lifetime
/// took, with that read's `as_of` — never re-dated to this read's now.
async fn shared_flows<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    scope: &crate::port::JobScope,
    window_hours: i64,
) -> Arc<MapFlowReading> {
    let key = format!("{}\u{1f}{scope:?}\u{1f}{window_hours}", user.id);
    state
        .map_flows
        .get_or_read(&key, || membership_flows(state, user, scope, window_hours))
        .await
}

/// [`shared_flows`], started as a task of its own so the map pass runs
/// beside it. A map read dropped mid-pass (its caller went away) leaves
/// the task to finish and leave its reading for the next map read —
/// at most one such read per key is ever in flight, which is the bound
/// the pile-up of 2026-10-21 did not have.
fn start_flows<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: Arc<JobsApiState<R, B>>,
    user: boss_policy_client::User,
    scope: crate::port::JobScope,
    window_hours: i64,
) -> tokio::task::JoinHandle<Arc<MapFlowReading>> {
    tokio::spawn(async move { shared_flows(&state, &user, &scope, window_hours).await })
}

/// [`membership_flows`]' windows, ending at `wall`.
async fn flow_windows<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    scope: &crate::port::JobScope,
    window_hours: i64,
    wall: chrono::DateTime<chrono::Utc>,
) -> Result<FlowWindows, String> {
    let window = chrono::Duration::hours(window_hours);
    let read = |since| {
        state.jobs.membership_snapshot(
            since,
            Some(user.id.clone()),
            scope,
            super::stations::MEMBERSHIP_ROW_BOUND,
        )
    };
    let by_station = |reading: crate::membership_replay::MembershipReading| {
        reading
            .stations
            .into_iter()
            .map(|flow| (flow.station.clone(), flow))
            .collect()
    };
    match read(wall - window - window).await {
        Ok(input) => {
            let [this, both] = crate::membership_replay::replay_windows(input, wall - window)?;
            Ok(FlowWindows {
                this: by_station(this),
                both: Ok(by_station(both)),
            })
        }
        Err(wider) => {
            let input = read(wall - window).await.map_err(|e| e.to_string())?;
            Ok(FlowWindows {
                this: by_station(crate::membership_replay::replay(&input)),
                both: Err(wider.to_string()),
            })
        }
    }
}

/// One station's `(served, why unread, previous served)` from the two
/// windows. A station the history holds no publication for is unread
/// with the flow surface's own reason, never zero.
fn station_served(windows: &FlowWindows, name: &str) -> (Option<i64>, Option<String>, Option<i64>) {
    let Some(this) = windows.this.get(name) else {
        return (
            None,
            Some(super::stations::NO_PUBLICATION_HISTORY.to_string()),
            None,
        );
    };
    let Some(served) = this.served.and_then(|n| i64::try_from(n).ok()) else {
        return (None, this.unavailable_reason(), None);
    };
    let both = windows
        .both
        .as_ref()
        .ok()
        .and_then(|both| both.get(name)?.served)
        .and_then(|n| i64::try_from(n).ok());
    (Some(served), None, both.map(|both| both - served))
}

/// THE STEPS THAT PUT EACH PACKET AT A STATION (backlog 6254e602): every
/// step of a member that satisfies the station's step clause — the same
/// [`crate::station_queue::StepMatch::matches`] the membership asked, so
/// the two cannot disagree — with its owner and its recorded onset. A
/// station with no step clause names no step, and so none can qualify a
/// wait there.
fn standing_steps(
    predicate: &crate::station_queue::StationPredicate,
    packets: &[&(Job, Vec<Step>)],
    ready: Result<&std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>, &str>,
) -> Vec<regions::StandingStep> {
    let Some(clause) = predicate.step.as_ref() else {
        return Vec::new();
    };
    packets
        .iter()
        .flat_map(|(job, steps)| {
            steps
                .iter()
                .filter(|step| clause.matches(step))
                .map(move |step| regions::StandingStep {
                    packet: job.id.to_string(),
                    kind: job.kind.clone(),
                    version: job.workflow_version,
                    title: job.title.clone(),
                    step: step.spec_slug.clone(),
                    assignee: step.assignee_id.clone(),
                    role: step
                        .metadata
                        .get("authority_role")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    since: ready
                        .map(|r| r.get(&step.id.to_string()).copied())
                        .map_err(str::to_string),
                })
        })
        .collect()
}

/// The map with the clock instant attached — the same `now` field the
/// yard status returns, so a client renders elapsed times against the
/// server's clock rather than its own.
fn with_now(map: regions::Regions, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
    let mut v = serde_json::to_value(map).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("now".to_string(), serde_json::json!(now));
    }
    v
}

async fn read_internal_protocols<'a>(
    registry: Option<&dyn crate::registry::WorkflowRegistry>,
    jobs: impl IntoIterator<Item = &'a Job>,
) -> Option<regions::PinnedProtocols> {
    let pinned = jobs
        .into_iter()
        .map(|job| (job.kind.clone(), job.workflow_version))
        .collect::<std::collections::BTreeSet<_>>();
    if pinned.is_empty() {
        return Some(Default::default());
    }
    let registry = registry?;
    let rows = futures::future::join_all(pinned.into_iter().map(async |(kind, version)| {
        let reading = if version <= 0 {
            Err(format!(
                "{kind}@{version}: no exact pinned version; latest active is not a substitute"
            ))
        } else {
            match registry.get_version(&kind, version).await {
                Ok(spec) if spec.kind == kind && spec.version == version => Ok(spec),
                Ok(_) => Err(format!(
                    "{kind}@{version}: registry returned a different binding"
                )),
                Err(error) => Err(format!("{kind}@{version}: {error}")),
            }
        };
        ((kind, version), reading)
    }))
    .await;
    Some(rows.into_iter().collect())
}

#[cfg(test)]
mod pinned_protocol_tests {
    use super::*;
    #[tokio::test]
    async fn no_visible_packets_need_no_registry_but_required_packets_do() {
        assert!(
            read_internal_protocols(None, std::iter::empty::<&Job>())
                .await
                .unwrap()
                .is_empty()
        );
        let packet = Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", "subject"),
            "review",
            "owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        );
        assert!(read_internal_protocols(None, [&packet]).await.is_none());
    }

    #[tokio::test]
    async fn an_unpinned_packet_cannot_resolve_to_the_active_protocol() {
        let registry = crate::registry::InMemoryWorkflows::for_fixture();
        registry
            .seed(crate::registry::WorkflowSpec::platform_seed(
                "ship-a-change",
                "Change",
                "platform",
                vec!["custom".into()],
                vec![],
            ))
            .unwrap();
        let mut packet = Job::new(
            "ship-a-change",
            boss_core::job::Subject::new("custom", "subject"),
            "review",
            "owner",
            boss_core::job::Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        );
        packet.workflow_version = 0;
        let rows = read_internal_protocols(Some(&registry), [&packet, &packet])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[&(packet.kind, 0)]
                .as_ref()
                .unwrap_err()
                .contains("latest active is not a substitute")
        );
    }

    #[tokio::test]
    async fn an_unread_protocol_does_not_discard_another_packets_pinned_version() {
        let registry = crate::registry::InMemoryWorkflows::for_fixture();
        let mut old = crate::registry::WorkflowSpec::platform_seed(
            "ship-a-change",
            "Change",
            "platform",
            vec!["custom".into()],
            vec![crate::registry::StepSpec {
                title: "review".into(),
                duration_hours: Some(2.0),
                ..Default::default()
            }],
        );
        old.status = crate::registry::WorkflowStatus::Retired;
        registry.seed(old.clone()).unwrap();
        let mut newer = old.clone();
        newer.version = 2;
        newer.status = crate::registry::WorkflowStatus::Active;
        newer.steps[0].duration_hours = Some(0.25);
        registry.seed(newer).unwrap();
        let packet_of = |kind| {
            Job::new(
                kind,
                boss_core::job::Subject::new("custom", "subject"),
                "review",
                "owner",
                boss_core::job::Priority::Standard,
                chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
            )
        };
        let mut packet = packet_of("ship-a-change");
        packet.workflow_version = 1;
        let missing = packet_of("missing-protocol");
        let rows = read_internal_protocols(Some(&registry), [&packet, &missing])
            .await
            .expect("one failed key must not discard an independently read protocol");
        let recorded = rows
            .get(&(packet.kind.clone(), 1))
            .unwrap()
            .as_ref()
            .unwrap();
        assert!(
            rows.get(&(missing.kind.clone(), 1))
                .unwrap()
                .as_ref()
                .unwrap_err()
                .contains("missing-protocol@1")
        );
        assert_eq!(
            recorded.steps[0].duration_hours,
            Some(2.0),
            "never substitute the active version"
        );
    }
}

/// THE MOVER'S READING WITHOUT THE FLOW IS THE MOVER'S READING (backlog
/// 29f7e897). The mover stopped paying for the flow read on the ground
/// that placement reads station membership and never `served`; this
/// holds that ground — the same rows read both ways place every packet
/// identically — and pins what the counted read asks of the adapter.
#[cfg(test)]
mod station_flow_tests {
    use super::*;
    use crate::{InMemoryJobs, JobsRepository};
    use boss_core::job::{Priority, StepStatus, Subject};
    use boss_core::port::EventBus;
    use boss_core::publisher::DomainPublisher;

    fn task_station() -> crate::StationSpec {
        let mut s = crate::StationSpec::draft(
            "q.platform-admin.task",
            "Ready tasks",
            crate::StationKind::Constraint,
            crate::station_queue::StationPredicate {
                status: Some(JobStatus::Open),
                step: Some(crate::station_queue::StepMatch {
                    kind: Some("task".into()),
                    status_in: vec![StepStatus::Ready],
                    ..Default::default()
                }),
                ..Default::default()
            },
            chrono::Utc::now(),
        );
        s.status = crate::registry::WorkflowStatus::Active;
        s
    }

    fn packet(kind: &str, status: JobStatus) -> Job {
        Job {
            status,
            ..Job::new(
                kind,
                Subject::new("custom", "bosspipeline"),
                "a packet",
                "emp-david",
                Priority::Standard,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            )
        }
    }

    fn step(job: &Job, slug: &str, kind: &str, status: StepStatus) -> Step {
        Step {
            spec_slug: Some(slug.into()),
            status,
            ..Step::new(job.id, kind, slug, 0)
        }
    }

    #[tokio::test]
    async fn placing_packets_without_the_flow_places_them_where_the_counted_read_does() {
        let jobs = Arc::new(InMemoryJobs::new());
        let now = chrono::Utc::now();
        let at = now - chrono::Duration::hours(72);
        // A triaged backlog-item waiting on its build: marshalling's.
        let item = packet("backlog-item", JobStatus::Open);
        jobs.create_job_at(&item, at, &[]).await.unwrap();
        let mut filed = step(&item, "filed", "trigger", StepStatus::Completed);
        filed.kind = crate::regions::TRIGGER_STEP_KIND.into();
        for s in [
            filed,
            step(&item, "triage", "task", StepStatus::Completed),
            step(&item, "build", "task", StepStatus::Ready),
        ] {
            jobs.add_step_at(&s, at, &[]).await.unwrap();
        }
        // A closed packet whose task was served an hour ago.
        let done = packet("backlog-item", JobStatus::Closed);
        jobs.create_job_at(&done, at, &[]).await.unwrap();
        let served = step(&done, "triage", "task", StepStatus::Completed);
        jobs.add_step_at(&served, at, &[]).await.unwrap();
        jobs.record_events(&[boss_core::event::Event::new(
            "jobs",
            "step.done.task",
            serde_json::json!({ "job_id": done.id.to_string(), "step_id": served.id.to_string() }),
            now - chrono::Duration::hours(1),
        )])
        .await
        .unwrap();

        let kinds = Arc::new(crate::registry::InMemoryWorkflows::for_fixture());
        for spec in crate::registry::seedable_platform_workflows() {
            kinds.seed(spec).unwrap();
        }
        let stations = Arc::new(crate::stations::InMemoryStations::new());
        stations.seed(task_station()).unwrap();
        let bus = boss_testing::RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone() as Arc<dyn EventBus>, "jobs");
        let state = Arc::new(JobsApiState {
            stations: Some(stations),
            kind_registry: Some(kinds.clone() as Arc<dyn crate::registry::WorkflowRegistry>),
            ..JobsApiState::minimal(
                jobs.clone(),
                bus,
                publisher,
                Arc::new(boss_policy_client::FakePolicyClient::builder().build()),
                Arc::new(boss_clock_client::WallClockClient),
            )
        });
        let actor = boss_policy_client::User {
            id: super::super::moves::MOVER_ACTOR.into(),
            role: "automation".into(),
            access_tier: boss_policy_client::AccessTier::User,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        };
        let read = |flow| {
            read_map_as(
                &state,
                &actor,
                crate::port::JobScope::All,
                EstateRead::Granted,
                now,
                regions::DEFAULT_WINDOW_HOURS,
                flow,
            )
        };
        let counted = read(StationFlow::Counted).await.ok().unwrap();
        assert_eq!(
            jobs.membership_reads(),
            2,
            "the wider window, refused here, then this window alone"
        );
        let placed = read(StationFlow::NotRead).await.ok().unwrap();
        assert_eq!(jobs.membership_reads(), 2, "placement reads no flow");
        assert_eq!(
            jobs.flow_cube_passes(),
            0,
            "the map never reads the step-events cube"
        );

        let station = |rows: &MapRows| {
            let stations = rows.stations.as_ref().unwrap();
            stations
                .iter()
                .find(|s| s.name == "q.platform-admin.task")
                .unwrap_or_else(|| panic!("no task station among {stations:?}"))
                .clone()
        };
        // The in-memory adapter holds no native history, so the counted
        // read is the reader's refusal, named; the placing read names
        // nothing, because it asked nothing.
        assert_eq!(station(&counted).served, None);
        assert!(
            station(&counted)
                .flow_unread
                .is_some_and(|why| why.contains("native membership history unavailable")),
            "the counted read names the refusal: {:?}",
            station(&counted)
        );
        assert_eq!(
            station(&placed).served,
            None,
            "the placing read does not count"
        );
        assert_eq!(station(&placed).flow_unread, None);
        assert_eq!(station(&counted).members, station(&placed).members);

        let specs = kinds.list_active(None).await.unwrap();
        let snapshot = |rows: &MapRows| {
            crate::moves::Snapshot::of_with_registry(
                &rows.inputs(now, regions::DEFAULT_WINDOW_HOURS),
                &specs,
            )
        };
        assert_eq!(
            snapshot(&counted).place(&item.id.to_string()),
            crate::moves::Where::At("marshalling"),
            "the fixture places a packet in marshalling, or it proves nothing"
        );
        assert_eq!(snapshot(&placed), snapshot(&counted));
    }
}
