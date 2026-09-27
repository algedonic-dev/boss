//! HTTP surface: health + readiness probes, the read-only cascade-viz
//! `rules` feed, and the rule-authoring write endpoints (create-draft /
//! validate / publish / retire) that back the SPA authoring UI. The
//! authoring writes go through `crate::rules::authoring`; the binary's
//! supervision loop polls a fingerprint of `dispatcher_rules` every 30s
//! and rebuilds the runners when it moves (backlog 1e576baf), so a
//! published change is live without a restart. A product draft no
//! authored file names is refused at the door — the boot seed would
//! retire it (backlog 7d9df2fe).
//!
//! `/api/dispatcher/health` answers 200 while the PROCESS is up — necessary
//! but NOT sufficient: the consumer loops run detached and can die while the
//! process keeps serving 200 (a NATS blip, JetStream not ready at cold start
//! under a no-restart launcher). `/api/dispatcher/readyz` reports the actual
//! consumer liveness (see [`crate::liveness`]) so operators — and the brewery
//! sim's pre-Go readiness gate — can tell "up" from "actually working."
//!
//! `/api/dispatcher/schedule` (design ea906603) answers, per scheduled
//! rule, when it last fired and when the runner next fires it — the
//! top board's "scheduled rules due next". It asks policy: a caller whose
//! scope reads no packets is told the schedule is withheld, the rule the
//! machine-firings car applied to the yard's reads (backlog e5f7b51e).
//!
//! The rule reads — `rules` and the two version reads — take the same
//! rule since backlog 493cebf3: until then they asked nothing, so any
//! gateway session, a basic guest that reads no packets among them, read
//! every rule's name, trigger, guard and handler. Two reads stay open, by
//! decision rather than oversight: `health`, which says the process is
//! up, and `readyz`, which carries liveness NUMBERS and flags — no rule's
//! name, no packet. Its readers (the estate observer's dead-letter read,
//! the OSS quickstart's launch wait, a tenant sim's pre-Go gate) read
//! it with no identity, and an alarm that must ask policy before it can
//! speak is an arm that needs the patient (CLAUDE.md §Diagnosis).
//! `readyz_answers_anyone_and_names_no_rule` holds it to numbers, so a
//! name added to it later fails a test instead of reaching a stranger.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_calendar_client::CalendarClient;
use boss_clock_client::ClockClient;
use boss_core::calendar::BusinessCalendar;
use boss_jobs::dispatcher_firings::{DispatcherFiringsRepository, RETENTION_DAYS, RuleLastFiring};
use boss_policy_client::{CurrentUser, PolicyClient, Predicate, Resource, User};
use chrono::{DateTime, Utc};

use crate::cascade;
use crate::liveness::DispatcherLiveness;
use crate::rules::authoring::{self, AuthoringError};
use crate::rules::registry::{
    ENFORCED_STATUS, RawRule, authored_why, load_active_rules, parse_raw_path,
};
use crate::rules::schedule_runner::next_due;

/// HTTP state: the consumer-liveness handle + the Postgres pool, so the
/// read-only `/api/dispatcher/rules` surface can serve the rule registry
/// (the `dispatcher_rules` table) for the cascade visualization, plus
/// the authored registry directory each rule's `why` is read from.
#[derive(Clone)]
pub struct HttpState {
    pub live: Arc<DispatcherLiveness>,
    pub pool: sqlx::PgPool,
    /// `DispatcherConfig::authored_rules_dir` — the authored
    /// `infra/dispatcher/rules` directory, source of each rule's `why`.
    /// `None` is served as `why: null` with the reason stated, never as
    /// "no rule records a why".
    pub authored_rules_dir: Option<PathBuf>,
    /// What the assembler declared about the handlers it registered —
    /// served verbatim as `handler_emits` + `system_edges`. Core spells
    /// no handler of its own (backlog ec40e269; see [`cascade`]).
    pub cascade: Arc<cascade::Cascade>,
    /// Who may read the schedule: a caller whose scope reads no packets
    /// may not (backlog e5f7b51e's rule, see the module doc).
    pub policy: Arc<dyn PolicyClient>,
    /// The clock the schedule runner fires by — the schedule's "now".
    pub clock: Arc<dyn ClockClient>,
    /// Where the business calendars a schedule names are read, the way
    /// the runner reads them at start.
    pub calendar: Arc<dyn CalendarClient>,
    /// The firing record's reader (`boss_jobs::dispatcher_firings`, the
    /// one copy of that read). `None` answers every last firing null
    /// with the reason, never "never fired".
    pub firings: Option<Arc<dyn DispatcherFiringsRepository>>,
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/api/dispatcher/health", get(health))
        .route("/api/dispatcher/readyz", get(readyz))
        // GET serves the cascade-viz feed; POST creates a new rule draft.
        .route("/api/dispatcher/rules", get(rules).post(create_rule_draft))
        .route("/api/dispatcher/rules/_validate", post(validate_rule))
        .route(
            "/api/dispatcher/rules/{name}/versions",
            get(list_rule_versions),
        )
        .route(
            "/api/dispatcher/rules/{name}/versions/{version}",
            get(get_rule_version),
        )
        .route("/api/dispatcher/rules/{name}/publish", post(publish_rule))
        .route("/api/dispatcher/rules/{name}/retire", post(retire_rule))
        .route("/api/dispatcher/schedule", get(schedule))
        .with_state(state)
}

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-dispatcher",
        env!("CARGO_PKG_VERSION"),
        "nats-subscriber",
    ))
}

/// Real readiness: are both durable consumers bound + draining? Returns
/// `{ready, assigning, assignment_events, rules_running, rules_events,
/// last_event_unix, dead_letters, dead_letters_unrecorded,
/// last_dead_letter_unix, last_unrecorded_dead_letter_unix}`. `ready=false`
/// while health is 200 is the exact "process up, but assigning nothing, so
/// Jobs never close" failure. The dead-letter counters are the read the
/// cluster estate observer (`boss-estate-observe.yaml`) carries into its
/// observation (8834804a): this surface owes nothing to the jobs API, so a
/// dead-letter that could not be annotated — or whose annotation write was
/// the very thing that failed — still has a number a reader can see.
async fn readyz(State(state): State<HttpState>) -> Json<serde_json::Value> {
    Json(state.live.snapshot())
}

/// Join the enforced rules with their authored justification — the
/// per-rule view this surface serves. Pure: rows in, JSON out.
///
/// Each object is the stored rule (`name`, `version`, the trigger —
/// `on_event` or `schedule` — `when`, `do`, `delay`) plus three fields
/// the registry row does not itself carry:
///
/// - `status` — which registry status these rows were selected on, so a
///   row SAYS what it is instead of the reader having to know the
///   handler's filter.
/// - `why` — the justification the rule's authored file records, or
///   `null` when no file does.
/// - `authored` — whether the authored registry holds a file for the
///   rule at all. `authored: false` is the §9a drift this surface
///   exists to show: a reaction the system is enforcing that the
///   authored registry does not record, and therefore one the `why`
///   guard (`dispatcher-rules-ratchet.sh`, `parse_raw_dir`) never saw.
/// - `source` — who declared the row (backlog 458971ef): `product`
///   for the authored directory's own, `tenant:<tenant_id>` for a rule
///   a tenant declared in its `seeds/rules.toml`. A tenant's rule is
///   `authored: false` by construction — no product file can name it —
///   and `source` is what keeps that from reading as the drift above.
fn rule_views(
    rules: &[RawRule],
    status: &str,
    why: &BTreeMap<String, String>,
    sources: &BTreeMap<String, String>,
) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| {
            // Defensive, not fallible in practice: RawRule serializes to
            // an object. Falling back to an empty map keeps the rule in
            // the list (named below) rather than dropping it silently.
            let mut obj = match serde_json::to_value(r) {
                Ok(serde_json::Value::Object(m)) => m,
                _ => serde_json::Map::new(),
            };
            obj.insert("name".into(), serde_json::Value::String(r.name.clone()));
            obj.insert("status".into(), serde_json::Value::String(status.into()));
            obj.insert(
                "why".into(),
                why.get(&r.name).map_or(serde_json::Value::Null, |w| {
                    serde_json::Value::String(w.clone())
                }),
            );
            obj.insert(
                "authored".into(),
                serde_json::Value::Bool(why.contains_key(&r.name)),
            );
            obj.insert(
                "source".into(),
                serde_json::Value::String(
                    authoring::source_label(sources.get(&r.name).map(String::as_str)).into(),
                ),
            );
            serde_json::Value::Object(obj)
        })
        .collect()
}

/// Read each rule's `why` from the authored registry directory,
/// alongside a `authored_registry` block describing WHERE the whys came
/// from and what went wrong if they did not.
///
/// The block is the point: without it, an unset or unreadable directory
/// and a registry in which no rule records a why are the same response
/// — well-formed, confident and wrong (CLAUDE.md §Doors). A reader
/// seeing `why: null` everywhere must be able to tell which it is.
fn authored_whys(dir: Option<&std::path::Path>) -> (BTreeMap<String, String>, serde_json::Value) {
    let Some(dir) = dir else {
        return (
            BTreeMap::new(),
            serde_json::json!({
                "dir": null, "rules": 0,
                "error": "BOSS_DISPATCHER_RULES is unset — there is no authored \
                          registry to read `why` from, so every `why` below is \
                          null for want of a source, not for want of a reason",
            }),
        );
    };
    let dir_str = dir.display().to_string();
    match authored_why(dir) {
        Ok(map) => {
            let n = map.len();
            (
                map,
                serde_json::json!({ "dir": dir_str, "rules": n, "error": null }),
            )
        }
        Err(e) => (
            BTreeMap::new(),
            serde_json::json!({ "dir": dir_str, "rules": 0, "error": e.to_string() }),
        ),
    }
}

/// Read-only rule-registry surface: what the dispatcher is enforcing,
/// and why.
///
/// Serves the rows of `dispatcher_rules` at [`ENFORCED_STATUS`] — per
/// rule its `name`, `version`, `status`, trigger (`on_event` or
/// `schedule`), `when`, `do`/args, `delay`, plus the `why` its authored
/// file records and whether it is `authored` at all (see
/// [`rule_views`]) — alongside `authored_registry` (where the whys came
/// from) and the cascade metadata the assembler declared
/// ([`HttpState::cascade`]): per-handler emitted events + the
/// jobs-api/external "system edges" that close the feedback loops.
///
/// Queries the table per request — a low-traffic admin view, and reading
/// live reflects any rule edits without a restart.
///
/// Asked of policy FIRST (backlog 493cebf3): a caller whose scope reads
/// no packets gets the feed's failure shape with the reason it was
/// withheld, and never reaches the table. Its readers all sign — the SPA
/// through its session, the departments readiness read as its viewer,
/// `boss tenant export` as the seed identity, a recorded probe as its
/// `audit-readonly` reader — so the caller this turns away is the one a
/// rule's name was never owed to.
async fn rules(
    State(state): State<HttpState>,
    CurrentUser(user): CurrentUser,
) -> Json<serde_json::Value> {
    let failed = |error: String| {
        Json(serde_json::json!({
            "error": error,
            "rules": [], "authored_registry": null,
            "handler_emits": {}, "system_edges": [],
        }))
    };
    if let Err(why) = reads_packets(state.policy.as_ref(), &user, "the rule registry").await {
        return failed(why);
    }
    let raw = match load_active_rules(&state.pool).await {
        Ok(raw) => raw,
        Err(e) => return failed(format!("load dispatcher_rules: {e}")),
    };
    let (why, authored_registry) = authored_whys(state.authored_rules_dir.as_deref());
    // Not best-effort: a failed read here would leave every row reading
    // `product`, well-formed and wrong about a tenant's rule (CLAUDE.md
    // §Doors), so it fails the way a failed rule load does.
    let sources = match authoring::enforced_sources(&state.pool).await {
        Ok(sources) => sources,
        Err(e) => return failed(format!("load dispatcher_rules sources: {e}")),
    };
    let mut out = serde_json::Map::new();
    out.insert(
        "rules".into(),
        serde_json::Value::Array(rule_views(&raw.rules, ENFORCED_STATUS, &why, &sources)),
    );
    out.insert("authored_registry".into(), authored_registry);
    insert_cascade(&mut out, &state.cascade);
    Json(serde_json::Value::Object(out))
}

/// The assembler's declared cascade, as the feed's two fields.
fn insert_cascade(out: &mut serde_json::Map<String, serde_json::Value>, c: &cascade::Cascade) {
    out.insert(
        "handler_emits".into(),
        serde_json::to_value(&c.handler_emits).unwrap_or_default(),
    );
    out.insert(
        "system_edges".into(),
        serde_json::to_value(&c.system_edges).unwrap_or_default(),
    );
}

// ---------------------------------------------------------------------------
// The schedule (design ea906603) — last firing and next due, per rule.
// ---------------------------------------------------------------------------

/// `GET /api/dispatcher/schedule` — every scheduled rule the dispatcher
/// enforces: `name`, `version`, `cadence`, `anchor_date`,
/// `business_calendar`, `when`, its newest firing off the record
/// (`last_fired`), and when the runner next fires it (`next_due`, from
/// [`next_due`]), soonest first — beside `now` and `simulated`, the
/// clock both are read against.
///
/// WHY (design ea906603). The IT department's top board shows what
/// happens next, and scheduled rules are half of that: the daily
/// publish, the sweeps, the polls. `/api/dispatcher/rules` carries each
/// schedule's cadence and anchor with no time of day, and the firing
/// record says only what already ran, so "when is this next" was a
/// derivation every reader would have to redo from the runner's source.
///
/// NOTHING IS OMITTED FOR WANT OF A READ. A last firing or a next due
/// that cannot be given is `null` beside a `*_why` saying which: no row
/// in the record's window, a record that could not be read, a calendar
/// that could not be read. The whole schedule is `null` beside
/// `schedule_error` when the caller may not read it or the rule table
/// will not load — never an empty list, which reads as "nothing is
/// scheduled".
async fn schedule(
    State(state): State<HttpState>,
    CurrentUser(user): CurrentUser,
) -> Json<serde_json::Value> {
    let clock = state.clock.now().await;
    let answer = |schedule: serde_json::Value, error: Option<String>| {
        Json(serde_json::json!({
            "now": clock.now,
            "simulated": clock.simulated,
            // The firing record's window: "no firing recorded" means none
            // in this many days, never "never".
            "retention_days": RETENTION_DAYS,
            "schedule": schedule,
            "schedule_error": error,
        }))
    };
    // Asked FIRST, so a caller the scope refuses never reaches the table.
    if let Err(why) = reads_packets(state.policy.as_ref(), &user, "the schedule").await {
        return answer(serde_json::Value::Null, Some(why));
    }
    let raw = match load_active_rules(&state.pool).await {
        Ok(raw) => raw,
        Err(e) => {
            return answer(
                serde_json::Value::Null,
                Some(format!("load dispatcher_rules: {e}")),
            );
        }
    };
    let firings = match &state.firings {
        None => Err("the dispatcher firing record is not wired to this dispatcher".to_string()),
        Some(repo) => repo
            .last_firings()
            .await
            .map_err(|e| format!("reading dispatcher_firings: {e}")),
    };
    let calendars = read_calendars(state.calendar.as_ref(), &raw.rules).await;
    answer(
        serde_json::Value::Array(schedule_views(&raw.rules, &firings, &calendars, clock.now)),
        None,
    )
}

/// THE SCOPE RULE (backlog e5f7b51e, applied to the yard's machine
/// reads): a caller whose policy scope reads no packets — which is what
/// a request with no identity is — reads nothing about the machinery
/// that moves them. Any scope that reads packets at all, however narrow,
/// reads the schedule: it is about the machine, not about any one
/// packet. A policy service that cannot answer refuses (D9, fail
/// closed), and its detail is logged rather than handed to the caller,
/// because it names the policy service's internal address (fe9d212c).
///
/// `what` names the read in the reason — "the schedule", "the rules" —
/// so a caller is told which record was withheld from it, and that it
/// was WITHHELD: a refusal by scope is not a read that failed.
async fn reads_packets(policy: &dyn PolicyClient, user: &User, what: &str) -> Result<(), String> {
    match policy.scope_predicate(user, Resource::job()).await {
        Ok(Predicate::None) => Err(format!(
            "this caller's policy scope reads no packets, so {what} of the machinery that \
             moves them is withheld from it"
        )),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(error = %e, read = what, "dispatcher read: policy check failed; withheld");
            Err(format!(
                "policy check failed, so {what} is withheld until policy can answer"
            ))
        }
    }
}

/// Read each business calendar a scheduled rule names, once. An error
/// is kept as the error — the rule it moves answers "unknown", not the
/// nominal day passed off as the real one.
async fn read_calendars(
    client: &dyn CalendarClient,
    rules: &[RawRule],
) -> BTreeMap<String, Result<Option<BusinessCalendar>, String>> {
    let codes: std::collections::BTreeSet<&str> = rules
        .iter()
        .filter_map(|r| r.schedule.as_ref()?.business_calendar.as_deref())
        .collect();
    let mut out = BTreeMap::new();
    for code in codes {
        let read = client
            .get_business_calendar(code)
            .await
            .map_err(|e| e.to_string());
        out.insert(code.to_string(), read);
    }
    out
}

/// The schedule rows — pure: rules, the firing record's read, the
/// calendars' reads and the clock instant in, rows out, soonest
/// `next_due` first (a row with none last), then by name. Event rules
/// have no schedule and no row.
fn schedule_views(
    rules: &[RawRule],
    firings: &Result<Vec<RuleLastFiring>, String>,
    calendars: &BTreeMap<String, Result<Option<BusinessCalendar>, String>>,
    now: DateTime<Utc>,
) -> Vec<serde_json::Value> {
    let mut rows: Vec<(Option<DateTime<Utc>>, &str, serde_json::Value)> = rules
        .iter()
        .filter_map(|r| {
            let s = r.schedule.as_ref()?;
            let (last_fired, last_fired_why) = match firings {
                Err(e) => (
                    serde_json::Value::Null,
                    Some(format!("the firing record could not be read: {e}")),
                ),
                Ok(all) => match all.iter().find(|f| f.rule == r.name) {
                    Some(f) => (
                        serde_json::json!({ "at": f.fired_at, "fired_on": f.fired_on }),
                        None,
                    ),
                    None => (
                        serde_json::Value::Null,
                        Some(format!(
                            "no firing recorded in the last {RETENTION_DAYS} days, which \
                             is as far back as the firing record keeps"
                        )),
                    ),
                },
            };
            // `Ok(None)` for a calendar that does not EXIST is the
            // runner's own fail-open (`ScheduleRunner::load_calendars`):
            // it fires on the nominal day, so the answer is that day,
            // and the row says what it assumed.
            let (cal, assumed) = match s.business_calendar.as_deref() {
                None => (Ok(None), None),
                Some(code) => match calendars.get(code) {
                    Some(Ok(Some(c))) => (Ok(Some(c)), None),
                    Some(Ok(None)) => (
                        Ok(None),
                        Some(format!(
                            "business calendar `{code}` does not exist, so the runner \
                             fires on the nominal day and this is that day"
                        )),
                    ),
                    Some(Err(e)) => (
                        Err(format!(
                            "business calendar `{code}` could not be read ({e}), so the \
                             day it moves this firing to is unknown"
                        )),
                        None,
                    ),
                    None => (
                        Err(format!("business calendar `{code}` was not read")),
                        None,
                    ),
                },
            };
            let (next, next_due_why) = match cal.and_then(|cal| next_due(s, cal, now)) {
                Ok(at) => (Some(at), assumed),
                Err(why) => (None, Some(why)),
            };
            let view = serde_json::json!({
                "name": r.name,
                "version": r.version,
                "cadence": s.cadence.token(),
                "anchor_date": s.anchor_date,
                "business_calendar": s.business_calendar,
                // A guard can decline a due firing; a reader is told it
                // is there rather than handed a certainty.
                "when": r.when,
                "last_fired": last_fired,
                "last_fired_why": last_fired_why,
                "next_due": next,
                "next_due_why": next_due_why,
            });
            Some((next, r.name.as_str(), view))
        })
        .collect();
    rows.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.1.cmp(b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(b.1),
    });
    rows.into_iter().map(|(_, _, v)| v).collect()
}

// ---------------------------------------------------------------------------
// Rule authoring (control-plane writes) — see crate::rules::authoring.
// ---------------------------------------------------------------------------

fn authoring_err(e: AuthoringError) -> Response {
    let code = match &e {
        AuthoringError::NotFound(_) => StatusCode::NOT_FOUND,
        AuthoringError::Invalid(_) => StatusCode::BAD_REQUEST,
        // 422, not 400: the stored row is well-formed — it just would not
        // load as a Rule. Same posture as the Workflow and station
        // registries' publish refusals.
        AuthoringError::Unviable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        AuthoringError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string()).into_response()
}

/// The draft door's body, split by hand: the rule spec, plus who
/// declares it. `source` is absent from the SPA's editor (the product's
/// own) and `tenant:<tenant_id>` from `boss tenant publish` (backlog
/// 458971ef); the rest of the object is exactly [`RawRule`], so the
/// wire shape is unchanged.
///
/// NOT `#[serde(flatten)]`, which was the shape until 2026-09-19
/// (backlog a2358e7c F3): serde hands a flattened struct only the keys
/// it names, so `RawRule`'s `deny_unknown_fields` is INERT underneath a
/// flatten — measured, a body carrying `onevent` parsed clean and
/// landed a draft with no trigger at all. Closing the FILE door while
/// the HTTP door stayed open would have been the worse outcome of the
/// two: the same registry, refusing a typo from a file and accepting it
/// from a POST.
fn split_draft_body(body: serde_json::Value) -> Result<(RawRule, Option<String>), String> {
    let mut obj = match body {
        serde_json::Value::Object(m) => m,
        other => return Err(format!("expected a rule object, got {other}")),
    };
    let source = match obj.remove("source") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => return Err(format!("`source` must be a string, got {other}")),
    };
    let rule: RawRule = serde_json::from_value(serde_json::Value::Object(obj))
        .map_err(|e| format!("the rule did not parse: {e}"))?;
    Ok((rule, source))
}

/// Refuse a PRODUCT draft (`source` absent) under a name no file in the
/// authored registry declares; `None` lets it through.
///
/// WHY (backlog 7d9df2fe, design ff1c3615 — David chose option b,
/// 2026-09-23). `rules::seed` runs at every dispatcher boot and retires
/// each active product-sourced rule no file names. So a product rule
/// created here, once published, fired until the next restart and was
/// then retired, the only trace a name in a boot log's `retired` list.
/// The SPA's "+ New rule" was the one caller that made them — `boss
/// tenant publish` always sends `tenant:<id>` — and it now points at
/// the two durable paths instead; this refusal makes the class
/// impossible rather than merely unoffered. A NEW VERSION of a rule a
/// file does name is still accepted: the seed never walks a live
/// version back (it reports it `behind`), so that edit survives.
///
/// The authored names are read with `parse_raw_path`, the seed's own
/// reader, so the door and the seed cannot disagree about what the
/// tree authors. A registry that is unset or will not read refuses
/// every product draft (503, naming the knob or the directory): the
/// door cannot vouch for a rule it cannot compare, and answering
/// "allowed" there is the confident wrong answer.
fn unauthored_product_draft(
    name: &str,
    source: Option<&str>,
    authored_dir: Option<&std::path::Path>,
) -> Option<(StatusCode, String)> {
    if source.is_some() {
        return None;
    }
    let Some(dir) = authored_dir else {
        return Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "BOSS_DISPATCHER_RULES is unset, so this dispatcher cannot read which product \
             rules the tree authors, and a product draft it cannot compare is refused"
                .to_string(),
        ));
    };
    let authored = match parse_raw_path(dir) {
        Ok(authored) => authored,
        Err(e) => {
            return Some((
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "the authored rule registry at {} will not read ({e}), so a product \
                     draft cannot be compared against it and is refused",
                    dir.display()
                ),
            ));
        }
    };
    if authored.rules.iter().any(|r| r.name == name) {
        return None;
    }
    Some((
        StatusCode::BAD_REQUEST,
        format!(
            "no file in the authored registry names `{name}`, so the dispatcher's boot seed \
             would retire this product rule at its next restart. A rule that lasts is \
             authored one of two ways: a file infra/dispatcher/rules/{name}.toml carried by a \
             car, or a [[rule]] in a tenant's seeds/rules.toml published by `boss tenant \
             publish` (source tenant:<id>)"
        ),
    ))
}

/// `POST /api/dispatcher/rules` — append a new draft version of a rule.
/// Body is the rule spec (name, on_event, when?, do[], delay?, version?)
/// plus an optional `source` ([`split_draft_body`]). The draft is validated
/// (must load via `Rule::from_raw`) before it persists; `201` on success
/// returns the stored draft. A name another source owns is refused 400,
/// and so is a product draft no authored file names
/// ([`unauthored_product_draft`]).
async fn create_rule_draft(
    State(state): State<HttpState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let (rule, source) = match split_draft_body(body) {
        Ok(split) => split,
        Err(e) => return (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    };
    if let Some(refusal) = unauthored_product_draft(
        &rule.name,
        source.as_deref(),
        state.authored_rules_dir.as_deref(),
    ) {
        return refusal.into_response();
    }
    match authoring::create_draft(&state.pool, &rule, source.as_deref()).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/_validate` — dry-run a draft without
/// persisting. Returns `{ ok, error }` so the authoring UI can surface
/// topic/predicate/arg parse errors live, before publish.
async fn validate_rule(Json(raw): Json<RawRule>) -> Json<serde_json::Value> {
    match authoring::validate(&raw) {
        Ok(()) => Json(serde_json::json!({ "ok": true, "error": null })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

/// The rule reads' gate, as a refusal: 403 with the reason it was
/// withheld (backlog 493cebf3). A version read names the rule and
/// carries its whole definition, drafts included, so it takes the feed's
/// scope rule.
async fn rule_read_refusal(state: &HttpState, user: &User) -> Option<Response> {
    reads_packets(state.policy.as_ref(), user, "the rule registry")
        .await
        .err()
        .map(|why| (StatusCode::FORBIDDEN, why).into_response())
}

/// `GET /api/dispatcher/rules/{name}/versions` — all versions, oldest first
/// (draft + active + retired).
async fn list_rule_versions(
    State(state): State<HttpState>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if let Some(refused) = rule_read_refusal(&state, &user).await {
        return refused;
    }
    match authoring::list_versions(&state.pool, &name).await {
        Ok(vs) => Json(vs).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `GET /api/dispatcher/rules/{name}/versions/{version}` — one version.
async fn get_rule_version(
    State(state): State<HttpState>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    if let Some(refused) = rule_read_refusal(&state, &user).await {
        return refused;
    }
    match authoring::get_version(&state.pool, &name, version).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/publish` — activate the latest draft,
/// retiring the prior active version.
async fn publish_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::publish(&state.pool, &name).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => authoring_err(e),
    }
}

/// `POST /api/dispatcher/rules/{name}/retire` — retire the active version.
async fn retire_rule(State(state): State<HttpState>, Path(name): Path<String>) -> Response {
    match authoring::retire(&state.pool, &name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => authoring_err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::registry::RawDoStep;

    fn rule(name: &str) -> RawRule {
        RawRule {
            name: name.into(),
            on_event: Some("step.done.task".into()),
            schedule: None,
            when: Some("spec_slug = \"merged\"".into()),
            do_steps: vec![RawDoStep {
                handler: "jobs.spawn".into(),
                args: Default::default(),
            }],
            delay: None,
            version: 3,
            why: None,
        }
    }

    /// The seven fields an operator asking "what are you enforcing, and
    /// why" needs, in one row: name, version, status, the trigger, the
    /// predicate, the side effects, and the justification.
    #[test]
    fn a_rule_view_carries_what_the_system_enforces_and_why() {
        let why = BTreeMap::from([(
            "converge-on-merge".to_string(),
            "a cross-protocol reactor".to_string(),
        )]);
        let views = rule_views(
            &[rule("converge-on-merge")],
            ENFORCED_STATUS,
            &why,
            &BTreeMap::new(),
        );

        let v = &views[0];
        assert_eq!(v["name"], "converge-on-merge");
        assert_eq!(v["version"], 3);
        assert_eq!(v["status"], ENFORCED_STATUS);
        assert_eq!(v["on_event"], "step.done.task");
        assert_eq!(v["when"], "spec_slug = \"merged\"");
        assert_eq!(v["do"][0]["handler"], "jobs.spawn");
        assert_eq!(v["why"], "a cross-protocol reactor");
        assert_eq!(v["authored"], serde_json::Value::Bool(true));
        assert_eq!(
            v["source"], "product",
            "a row no tenant declared is the product's, and says so"
        );
    }

    /// A TENANT'S RULE READS AS THE TENANT'S (backlog 458971ef). It has
    /// no file in the product's authored registry — `authored: false`,
    /// `why: null` — which without `source` is indistinguishable from
    /// the §9a drift the row below it reports. The source is what tells
    /// a reader "this is a tenant's reactor, declared in its own
    /// directory" from "someone published live and never wrote it down".
    #[test]
    fn a_tenant_sourced_rule_names_its_tenant_in_the_view() {
        let sources = BTreeMap::from([(
            "complete-site-live-on-converge-closed".to_string(),
            "tenant:acme".to_string(),
        )]);
        let views = rule_views(
            &[
                rule("complete-site-live-on-converge-closed"),
                rule("auto-park-on-gate-green"),
            ],
            ENFORCED_STATUS,
            &BTreeMap::new(),
            &sources,
        );
        assert_eq!(views[0]["source"], "tenant:acme");
        assert_eq!(views[0]["authored"], serde_json::Value::Bool(false));
        assert_eq!(views[1]["source"], "product");
    }

    /// The draft door's body is the rule plus WHO declares it: the
    /// SPA's editor sends the rule alone (the product's), `boss tenant
    /// publish` adds `source`. The rule's own shape on the wire is
    /// unchanged.
    #[test]
    fn a_draft_body_is_the_rule_with_an_optional_source() {
        let (rule, source) = split_draft_body(
            serde_json::from_str(
                r#"{"name":"r","on_event":"a.b","when":null,"delay":null,"do":[{"handler":"jobs.spawn","args":{}}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(rule.name, "r");
        assert_eq!(rule.version, 1, "the default the editor sends");
        assert!(source.is_none());

        let (rule, source) = split_draft_body(
            serde_json::from_str(
                r#"{"name":"r","version":4,"on_event":"a.b","do":[{"handler":"jobs.spawn","args":{}}],"source":"tenant:acme"}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(rule.version, 4);
        assert_eq!(source.as_deref(), Some("tenant:acme"));
    }

    /// The POST door refuses the key the FILE door refuses (backlog
    /// a2358e7c F3). `RawRule` carries `deny_unknown_fields`, but serde
    /// hands a `#[serde(flatten)]`ed struct only the keys it names, so
    /// under the flatten this body parsed CLEAN and landed a draft with
    /// no trigger at all — one registry answering a typo two different
    /// ways depending on which door it came through.
    #[test]
    fn a_draft_key_the_registry_does_not_read_is_refused() {
        let e = split_draft_body(
            serde_json::from_str(
                r#"{"name":"r","onevent":"a.b","do":[{"handler":"jobs.spawn","args":{}}],"source":"tenant:acme"}"#,
            )
            .unwrap(),
        )
        .unwrap_err();
        assert!(e.contains("onevent"), "{e}");
    }

    /// A PRODUCT DRAFT NO FILE NAMES IS REFUSED AT THE DOOR (backlog
    /// 7d9df2fe, design ff1c3615 option b). The boot seed retires every
    /// active product-sourced rule no file in the authored registry
    /// names, so such a draft, once published, lived until the next
    /// dispatcher restart and its retirement showed only in a boot log.
    /// The SPA's "+ New rule" was the only caller that made one; the
    /// refusal makes the class impossible instead of merely unoffered.
    #[test]
    fn a_product_draft_no_authored_file_names_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sweep.toml"),
            "[[rule]]\nname = \"sweep\"\nwhy = \"\"\"\na timer\n\"\"\"\n\
             on_event = \"x.y\"\n[[rule.do]]\nhandler = \"noop\"\n",
        )
        .unwrap();

        assert!(
            unauthored_product_draft("sweep", None, Some(dir.path())).is_none(),
            "a new version of a rule a file authors is the live-edit path the seed keeps"
        );

        let (code, why) = unauthored_product_draft("scratch", None, Some(dir.path()))
            .expect("a product rule no file names is the seed's to retire");
        assert_eq!(code, StatusCode::BAD_REQUEST);
        for named in [
            "scratch",
            "infra/dispatcher/rules/scratch.toml",
            "seeds/rules.toml",
        ] {
            assert!(why.contains(named), "the refusal must name {named}: {why}");
        }

        assert!(
            unauthored_product_draft("scratch", Some("tenant:acme"), Some(dir.path())).is_none(),
            "a tenant's rule is the tenant's protocol data; the seed never retires it"
        );
    }

    /// When the door cannot read what the tree authors it cannot tell a
    /// durable product draft from a doomed one, so it refuses rather
    /// than answer (CLAUDE.md §Doors: a wrong target answers instead of
    /// erroring) — and says which knob or which directory.
    #[test]
    fn a_product_draft_is_refused_when_the_authored_registry_will_not_read() {
        let (code, why) = unauthored_product_draft("sweep", None, None)
            .expect("an unset registry cannot vouch for any product rule");
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(why.contains("BOSS_DISPATCHER_RULES"), "{why}");

        let missing = std::path::Path::new("/nonexistent/dispatcher/rules");
        let (code, why) = unauthored_product_draft("sweep", None, Some(missing))
            .expect("an unreadable registry cannot vouch for any product rule");
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(why.contains("/nonexistent/dispatcher/rules"), "{why}");

        assert!(
            unauthored_product_draft("sweep", Some("tenant:acme"), None).is_none(),
            "a tenant draft does not depend on the product's directory"
        );
    }

    /// A rule the system enforces that NO authored file records reads as
    /// `authored: false, why: null` — the §9a drift, visible in the
    /// response itself rather than only to whoever runs the check.
    /// Measured 2026-09-10 against the live registry: four of sixty-four
    /// enforced rules had no file, so the `why` guard covered sixty.
    #[test]
    fn an_unauthored_rule_says_so_instead_of_going_quiet() {
        let views = rule_views(
            &[rule("auto-park-on-gate-green")],
            ENFORCED_STATUS,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        assert_eq!(views[0]["name"], "auto-park-on-gate-green");
        assert_eq!(views[0]["authored"], serde_json::Value::Bool(false));
        assert!(views[0]["why"].is_null(), "{}", views[0]);
    }

    /// An unset or unreadable authored registry must be DISTINGUISHABLE
    /// from one in which no rule records a why. Both serve `why: null`;
    /// only the `authored_registry` block says which, and without it the
    /// response is the confident wrong answer CLAUDE.md §Doors names.
    #[test]
    fn the_response_says_why_the_whys_are_missing() {
        let (map, block) = authored_whys(None);
        assert!(map.is_empty());
        assert!(block["dir"].is_null(), "{block}");
        assert!(
            block["error"]
                .as_str()
                .unwrap_or_default()
                .contains("BOSS_DISPATCHER_RULES"),
            "an unset directory must name the knob that sets it: {block}"
        );

        let missing = std::path::Path::new("/nonexistent/dispatcher/rules");
        let (map, block) = authored_whys(Some(missing));
        assert!(map.is_empty());
        assert_eq!(block["dir"], missing.display().to_string());
        assert!(
            !block["error"].is_null(),
            "an unreadable directory must report its error, not read as \
             an empty registry: {block}"
        );

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sweep.toml"),
            "[[rule]]\nname = \"sweep\"\nwhy = \"\"\"\na timer\n\"\"\"\n\
             on_event = \"x.y\"\n[[rule.do]]\nhandler = \"noop\"\n",
        )
        .unwrap();
        let (map, block) = authored_whys(Some(dir.path()));
        assert_eq!(map.len(), 1);
        assert_eq!(block["rules"], 1);
        assert!(block["error"].is_null(), "{block}");
    }

    // ----- the schedule view (design ea906603) -----------------------

    fn scheduled(name: &str, cadence: &str, anchor: &str, calendar: Option<&str>) -> RawRule {
        RawRule {
            name: name.into(),
            on_event: None,
            schedule: Some(crate::rules::registry::RawSchedule {
                cadence: boss_core::calendar::Cadence::parse(cadence).unwrap(),
                anchor_date: anchor.parse().unwrap(),
                business_calendar: calendar.map(str::to_string),
            }),
            when: None,
            do_steps: vec![RawDoStep {
                handler: "jobs.spawn".into(),
                args: Default::default(),
            }],
            delay: None,
            version: 2,
            why: None,
        }
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn last(rule: &str, fired_at: &str) -> RuleLastFiring {
        RuleLastFiring {
            rule: rule.into(),
            fired_on: "clock.day".into(),
            fired_at: at(fired_at),
        }
    }

    /// One row per SCHEDULED rule — an event rule has no schedule to
    /// read — carrying its cadence and anchor, its last firing off the
    /// record, and its next due time off the runner's own math, soonest
    /// first.
    #[test]
    fn the_schedule_answers_each_scheduled_rule_last_and_next() {
        let mut guarded = scheduled("publish-daily", "daily", "2026-08-14", None);
        guarded.when = Some("NOT open_publish_exists(\"github-mirror\")".into());
        let rules = [
            rule("converge-on-merge"),
            guarded,
            scheduled("sensors-poll", "every-5-minutes", "2026-09-17", None),
        ];
        let views = schedule_views(
            &rules,
            &Ok(vec![last("sensors-poll", "2026-09-27T14:30:00Z")]),
            &BTreeMap::new(),
            at("2026-09-27T14:32:10Z"),
        );
        assert_eq!(views.len(), 2, "the event rule has no schedule: {views:?}");

        let poll = &views[0];
        assert_eq!(poll["name"], "sensors-poll", "soonest first: {views:?}");
        assert_eq!(poll["cadence"], "every-5-minutes");
        assert_eq!(poll["anchor_date"], "2026-09-17");
        assert_eq!(poll["last_fired"]["at"], "2026-09-27T14:30:00Z");
        assert_eq!(poll["last_fired"]["fired_on"], "clock.day");
        assert!(poll["last_fired_why"].is_null(), "{poll}");
        assert_eq!(poll["next_due"], "2026-09-27T14:35:00Z");
        assert!(poll["next_due_why"].is_null(), "{poll}");

        let daily = &views[1];
        assert_eq!(daily["name"], "publish-daily");
        assert_eq!(daily["version"], 2);
        assert_eq!(daily["next_due"], "2026-09-28T00:00:00Z");
        assert_eq!(
            daily["when"], "NOT open_publish_exists(\"github-mirror\")",
            "a guard can still decline a due firing, so it rides beside it"
        );
        // Never omitted: no row in the record is a reason, not a gap.
        assert!(daily["last_fired"].is_null(), "{daily}");
        assert!(
            daily["last_fired_why"].as_str().is_some_and(
                |w| w.contains("no firing recorded") && w.contains(&RETENTION_DAYS.to_string())
            ),
            "{daily}"
        );
    }

    /// UNREAD IS NOT NEVER. A firing record that could not be read makes
    /// every last firing null WITH the error, so a stalled cadence is
    /// not painted "never fired" on no evidence.
    #[test]
    fn an_unread_firing_record_is_said_on_every_row() {
        let views = schedule_views(
            &[scheduled("publish-daily", "daily", "2026-08-14", None)],
            &Err("reading dispatcher_firings: connection refused".into()),
            &BTreeMap::new(),
            at("2026-09-27T14:32:10Z"),
        );
        assert!(views[0]["last_fired"].is_null());
        assert!(
            views[0]["last_fired_why"]
                .as_str()
                .is_some_and(|w| w.contains("connection refused")),
            "{}",
            views[0]
        );
        assert_eq!(views[0]["next_due"], "2026-09-28T00:00:00Z");
    }

    /// A business calendar that cannot be read is a next due that cannot
    /// be computed — null with the calendar named, never the nominal day
    /// passed off as the real one. A calendar that does not EXIST is the
    /// runner's documented fail-open: it fires on the nominal day, and so
    /// does the answer.
    #[test]
    fn a_calendar_that_cannot_be_read_is_a_next_due_that_says_so() {
        let calendars = BTreeMap::from([
            (
                "us-banking".to_string(),
                Err::<Option<BusinessCalendar>, String>("calendar-api: 503".into()),
            ),
            ("us-tax".to_string(), Ok(None)),
        ]);
        let views = schedule_views(
            &[
                scheduled("bank-sweep", "daily", "2026-01-01", Some("us-banking")),
                scheduled("tax-file", "daily", "2026-01-01", Some("us-tax")),
            ],
            &Ok(vec![]),
            &calendars,
            at("2026-09-27T14:32:10Z"),
        );
        let sweep = views.iter().find(|v| v["name"] == "bank-sweep").unwrap();
        assert!(sweep["next_due"].is_null(), "{sweep}");
        assert!(
            sweep["next_due_why"]
                .as_str()
                .is_some_and(|w| w.contains("us-banking") && w.contains("503")),
            "{sweep}"
        );
        assert_eq!(sweep["business_calendar"], "us-banking");
        let tax = views.iter().find(|v| v["name"] == "tax-file").unwrap();
        assert_eq!(tax["next_due"], "2026-09-28T00:00:00Z", "{tax}");
        assert_eq!(
            views.last().unwrap()["name"],
            "bank-sweep",
            "a row with no next due sorts last"
        );
    }

    /// The cascade the read surface serves is the one its ASSEMBLER
    /// declared, not a roster core spells (backlog ec40e269): core names
    /// no handler, so a handler only a tenant's assembly registers is
    /// served exactly as that assembly declared it.
    #[test]
    fn the_cascade_served_is_the_one_the_assembler_declared() {
        let declared = cascade::Cascade {
            handler_emits: BTreeMap::from([("tenant.only.thing", vec!["tenant.only.done"])]),
            system_edges: vec![cascade::SystemEdge {
                from: "tenant.only.done",
                to: "step.ready.*",
                kind: "jobs-api",
                label: "a label",
            }],
        };
        let mut out = serde_json::Map::new();
        insert_cascade(&mut out, &declared);
        assert_eq!(
            out["handler_emits"],
            serde_json::json!({"tenant.only.thing": ["tenant.only.done"]})
        );
        assert_eq!(out["system_edges"][0]["from"], "tenant.only.done");

        let mut out = serde_json::Map::new();
        insert_cascade(&mut out, &cascade::Cascade::default());
        assert_eq!(out["handler_emits"], serde_json::json!({}));
        assert_eq!(out["system_edges"], serde_json::json!([]));
    }
}
