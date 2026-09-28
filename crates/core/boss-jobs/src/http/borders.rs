//! `GET /api/yard/borders?window=24h` — the IT world map's rails
//! (design d2154293, decided 2026-09-19; car 2).
//!
//! For every border the world layout declares, what crosses it (a rate
//! this window against the previous), what is waiting to cross right
//! now with each hold's reason, and the machinery that moves it with
//! its last-fired time. The aggregation is [`crate::borders::borders`],
//! pure and unit-tested; this handler is the adapter that reads the
//! rows.
//!
//! THE SAME PASS AS THE REGIONS. It reads through
//! [`super::regions::read_map`] — the one pass the regions map already
//! makes — so the two read-models over the world cannot disagree about
//! what is on the dock or which trains are in transit. The only read
//! this one adds is the machines' firing records — `cadence_firings`
//! for a declared heartbeat and `dispatcher_firings` for a rule that
//! fires on an event (backlog b14afc48).

use super::*;

use crate::borders::{self, BORDERS, CadenceFiring, DispatcherFiring, MachineKind};

/// Why a caller whose scope reads no packets reads no machine's firing
/// record — the rule of backlog e5f7b51e, said as a refusal by scope.
const MACHINES_WITHHELD: &str = "this caller's policy scope reads no packets, so the firing \
                                 record of the machinery that moves them is not read for it";

#[derive(Debug, Deserialize, Default)]
pub(super) struct BordersQuery {
    window: Option<String>,
}

pub(super) async fn yard_borders<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<BordersQuery>,
) -> Response {
    // Refused rather than defaulted, as the regions read refuses it: a
    // rail asked for a week that answered a day is a confident wrong
    // rate.
    let window_hours = match crate::regions::parse_window(q.window.as_deref()) {
        Ok(h) => h,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    let rows = match super::regions::read_map(&state, &user, now, window_hours).await {
        Ok(rows) => rows,
        Err(resp) => return resp,
    };
    // The machine records are not scoped by packet, so they ride the
    // map's own gate: a caller whose scope reads no packets — a request
    // with no identity is one — gets them withheld, as it gets the empty
    // map. Until backlog e5f7b51e they asked nothing and answered every
    // machine's last-fired instant to that caller, beside the region
    // half that refused it; until 493cebf3 the border then called the
    // refusal "could not be read", a failure's words.
    let withheld = rows.reads_no_packets.then_some(MACHINES_WITHHELD);
    let (firings, dispatcher) = if rows.reads_no_packets {
        (None, None)
    } else {
        (
            cadence_firings(&state).await,
            dispatcher_firings(&state).await,
        )
    };
    let regions = rows.inputs(now, window_hours);
    let map = borders::borders(&borders::BorderInputs {
        regions: &regions,
        firings: firings.as_deref(),
        dispatcher_firings: dispatcher.as_deref(),
        machines_withheld: withheld,
    });
    let mut v = serde_json::to_value(map).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        // The server's clock, so a client ages a last-fired time against
        // it rather than against the browser's.
        obj.insert("now".to_string(), serde_json::json!(now));
    }
    Json(v).into_response()
}

/// The declared cadence machines' firing records — the ONE place a
/// machine's own heartbeat is written (`cadence_firings`).
///
/// `None` means the record could not be read at all (no registry wired,
/// or a failed read), and every cadence machine then says so. A rule the
/// registry does not hold is simply absent from the list, which the
/// border reports by NAME: a machine name that has drifted from the
/// registry is a defect to see, not a blank to skip past.
async fn cadence_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
) -> Option<Vec<CadenceFiring>> {
    let repo = state.cadence.as_ref()?;
    let rules = repo.active_rules().await.ok()?;
    let mut wanted: Vec<&'static str> = BORDERS
        .iter()
        .filter(|spec| spec.machine_kind == MachineKind::Cadence)
        .map(|spec| spec.machine)
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::with_capacity(wanted.len());
    for name in wanted {
        let Some(rule) = rules.iter().find(|r| r.name == name) else {
            continue;
        };
        // A failed firing read is the whole record unread: one rule
        // answering and another erroring would leave a border saying
        // "never fired" on no evidence.
        let firing = repo.last_firing(name).await.ok()?;
        out.push(CadenceFiring {
            rule: name.to_string(),
            fired_at: firing.map(|f| f.fired_at),
            every_minutes: rule.every_minutes.map(i64::from),
        });
    }
    Some(out)
}

/// The declared dispatcher machines' firing records
/// (`dispatcher_firings`, backlog b14afc48). One entry per dispatcher
/// rule the layout names, always — a rule that has never fired is an
/// entry with no instant, which the border says out loud, because the
/// alternative is a rail that looks quiet whether the machine is idle
/// or stopped.
///
/// `None` is the whole record unread (no repository wired, or a failed
/// read). A PARTIAL answer is refused the same way `cadence_firings`
/// refuses one: one rule answering and another erroring would leave a
/// border claiming "never fired" on no evidence.
async fn dispatcher_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
) -> Option<Vec<DispatcherFiring>> {
    let repo = state.dispatcher_firings.as_ref()?;
    let mut wanted: Vec<&'static str> = BORDERS
        .iter()
        .filter(|spec| spec.machine_kind == MachineKind::DispatcherRule)
        .map(|spec| spec.machine)
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::with_capacity(wanted.len());
    for name in wanted {
        let firing = repo.last_firing(name).await.ok()?;
        out.push(DispatcherFiring {
            rule: name.to_string(),
            fired_at: firing.map(|f| f.fired_at),
        });
    }
    Some(out)
}
