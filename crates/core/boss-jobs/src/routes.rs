//! THE ROUTES — every line the IT map may draw, each with the sources
//! that support it (design e765b3fc §2b; car R2 on feedback 84cba7e2).
//!
//! WHY THIS EXISTS. Every edge on both /it maps came from one hand-written
//! list that lived in three copies (`borders::BORDERS`, `world.ts`,
//! `transit.ts`), pinned equal to each other and to nothing else. The
//! design measured it against the record: of ten drawn edges, five match
//! their source, three are partly wrong and two carry no packet — and ten
//! real routes are drawn nowhere, among them the one David named: "like
//! how the dock routes a train over to the gates before it departs onto
//! the tracks." A pin between copies cannot catch an edge nobody takes.
//! This module derives the edges instead, and `GET /api/yard/routes`
//! serves them.
//!
//! A ROUTE EXISTS WHEN ONE OF THREE SOURCES SUPPORTS IT, and only then:
//!
//! 1. **The protocol** ([`walk`]). For every platform kind, through the
//!    family the placement reads it by (`regions::family_of`), the
//!    ACTIVE Workflow row is walked:
//!    one packet is admitted through the engine's own materialisation
//!    and re-evaluation (`registry::materialize_steps`,
//!    `registry::reevaluate`), and every state it can reach is explored —
//!    each ready step completed under every value its successors'
//!    predicates compare its metadata to, each job-metadata value a
//!    predicate waits on written, each station marker (a car's `hold`)
//!    set and released, and every ready terminal completed the moment it
//!    is ready, as the dispatcher's complete-on-ready rule does. At every
//!    state the packet is placed by `regions::place_alone` — the ONE
//!    placement function, over a one-packet reading — and wherever the
//!    region changes, a route is recorded, citing the workflow, its
//!    version and the step that moved it. The train's dock → gates →
//!    track → arrivals comes out of pr-train's own steps this way.
//! 2. **A declared hand-off** ([`HandOff`], `infra/platform/yard/
//!    handoffs.toml`): a rule, cadence or verb that turns one packet into
//!    another names the two by `kind@step`, and each end is resolved by
//!    the same walk — so the file names steps, never regions.
//! 3. **An observed move**: the moves record (`crate::moves`) counts the
//!    moves each route took. One on a route with no source 1 or 2 is
//!    `declared: false` — drawn dashed red, counted, and it troubles its
//!    region ([`crate::region_states::MOVES_UNDECLARED`]). An EXIT is
//!    counted per kind and terminal — the move records the terminal its
//!    packet left by — and is declared only by a source naming that
//!    terminal ([`RouteMap::judges`]; backlog e23005c0).
//!
//! OFF THE MAP IS AN END, NOT A GAP (David, `added_2026_09_25_david_
//! offramps`: "every packet that leaves the map must leave by a drawn
//! route"). A route's end is a region or `None`, off the map. A packet
//! enters when it is first placed (its admission, or the step that puts
//! it on the map later); it LEAVES only by a terminal — the step it
//! closed on names the off-ramp. A packet a terminal closes ONTO a region
//! (a train closed on `arrived` stands in arrivals) leaves when the
//! reading's window passes its closing: the walk places the closed state
//! again one window later, and the off-ramp it finds is cited to that
//! terminal ([`AGED_OUT`]; backlog d085dc47). An open packet that stops being placed
//! is carried (a car aboard its train) or absorbed (a hand-off), and the
//! record draws that move where the carrier stands; the walk, which sees
//! one packet, does not guess it as an exit.
//!
//! WHAT A ONE-PACKET WALK CANNOT SEE is stated, not filled in: a route
//! whose placement turns on ANOTHER packet (a green gate-run leaving the
//! map because a car already claims its branch) is a hand-off, or it is
//! observed-undeclared and says so on the map. The walk seeds only what
//! its placement reads and no protocol writes — the `branch` every filer
//! stamps ([`crate::moves::SAME_BRANCH`]), the job keys a station's own
//! predicate requires present, and the required fields of a step it
//! completes (the engine refuses a completion without them).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::moves::RouteCount;
use crate::regions::{Instant, Placed, Stations, family_of, inbound_kinds, place_alone};
use crate::registry::{WorkflowSpec, WorkflowStatus};

/// How many states one protocol's walk may explore. The platform's
/// largest measured under a thousand; past this the walk stops and says
/// it was cut short ([`Walk::truncated`]) rather than serving a partial
/// answer as the whole.
pub const MAX_STATES: usize = 4096;

/// The value the walk writes where a placement reads a key no protocol
/// writes (see the module note).
pub const SEED: &str = "walk";

/// The hand-off declarations, compiled in from the one file that holds
/// them.
pub const HANDOFFS_TOML: &str = include_str!("../../../../infra/platform/yard/handoffs.toml");

/// One end of a route: a region, or off the map.
pub type End = Option<&'static str>;

/// ONE CROSSING a protocol walk found: the packet stood at `from`, and
/// `step` moved it to `to` — completed (`via: "completed"`, with the
/// value it recorded), a job key written, a marker set or released, or
/// its admission.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Crossing {
    pub from: End,
    pub to: End,
    pub step: String,
    pub via: String,
}

/// One protocol, walked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Walk {
    pub kind: String,
    pub version: i32,
    /// How many distinct states the walk reached.
    pub states: usize,
    /// The walk stopped at [`MAX_STATES`]: what it found is real, and
    /// not all there is.
    pub truncated: bool,
    pub crossings: Vec<Crossing>,
    /// Where the packet first stands while each step is ready — or, for
    /// a terminal, once it has closed on it. What a hand-off's
    /// `kind@step` resolves to.
    #[serde(skip)]
    pub at: BTreeMap<String, Placed>,
    /// The protocol's terminals, by slug: what a hand-off naming one
    /// hands off, and what an exit move names.
    #[serde(skip)]
    pub terminals: BTreeSet<String>,
}

/// A packet's state as the walk holds it.
#[derive(Debug, Clone)]
struct State {
    job: Job,
    steps: Vec<Step>,
    /// The terminal it closed on, once it has.
    closed_on: Option<String>,
}

impl State {
    /// What distinguishes two states: every step's status and metadata,
    /// and the job's status and metadata.
    fn key(&self) -> String {
        let steps: Vec<(&StepStatus, &Value)> = self
            .steps
            .iter()
            .map(|s| (&s.status, &s.metadata))
            .collect();
        serde_json::to_string(&(&self.job.status, &self.job.metadata, steps)).unwrap_or_default()
    }

    fn open(&self) -> bool {
        self.job.status == JobStatus::Open
    }
}

/// The values the walk tries: for each step's metadata key and each job
/// metadata key some predicate compares, the literals it is compared
/// with plus [`SEED`] (a value equal to none of them).
#[derive(Debug, Default)]
struct Candidates {
    step: BTreeMap<String, BTreeMap<String, Vec<Value>>>,
    job: BTreeMap<String, Vec<Value>>,
}

fn literal(v: &boss_expr::Value) -> Option<Value> {
    match v {
        boss_expr::Value::Bool(b) => Some(Value::Bool(*b)),
        boss_expr::Value::Int(i) => Some(Value::from(*i)),
        boss_expr::Value::Float(f) => serde_json::Number::from_f64(*f).map(Value::Number),
        boss_expr::Value::String(s) => Some(Value::String(s.clone())),
        _ => None,
    }
}

impl Candidates {
    fn of(spec: &WorkflowSpec) -> Self {
        let mut out = Candidates::default();
        for s in &spec.steps {
            if let Ok(expr) = boss_expr::parse(&s.ready_when) {
                out.collect(&expr);
            }
        }
        let seed = Value::String(SEED.to_string());
        let with_seed = |vals: &mut Vec<Value>| {
            if !vals.contains(&seed) {
                vals.push(seed.clone());
            }
        };
        out.step
            .values_mut()
            .flat_map(|m| m.values_mut())
            .for_each(with_seed);
        out.job.values_mut().for_each(with_seed);
        out
    }

    fn collect(&mut self, expr: &boss_expr::Expr) {
        use boss_expr::{BinaryOp, Expr};
        match expr {
            Expr::BinaryOp(BinaryOp::And | BinaryOp::Or, l, r) => {
                self.collect(l);
                self.collect(r);
            }
            Expr::UnaryOp(_, inner) => self.collect(inner),
            Expr::BinaryOp(_, l, r) => {
                let (path, lit) = match (l.as_ref(), r.as_ref()) {
                    (Expr::Identifier(p), Expr::Literal(v))
                    | (Expr::Literal(v), Expr::Identifier(p)) => (p, v),
                    _ => return,
                };
                let Some(value) = literal(lit) else { return };
                let slot = match path.as_slice() {
                    [steps, slug, md, key] if steps == "steps" && md == "metadata" => self
                        .step
                        .entry(slug.clone())
                        .or_default()
                        .entry(key.clone())
                        .or_default(),
                    [job, md, key] if job == "job" && md == "metadata" => {
                        self.job.entry(key.clone()).or_default()
                    }
                    _ => return,
                };
                if !slot.contains(&value) {
                    slot.push(value);
                }
            }
            _ => {}
        }
    }

    /// Every assignment of values to `slug`'s compared keys — one empty
    /// assignment when its successors compare none.
    fn branches(&self, slug: &str) -> Vec<Vec<(String, Value)>> {
        let mut out: Vec<Vec<(String, Value)>> = vec![Vec::new()];
        for (key, vals) in self.step.get(slug).into_iter().flatten() {
            out = out
                .into_iter()
                .flat_map(|b| {
                    vals.iter().map(move |v| {
                        let mut b = b.clone();
                        b.push((key.clone(), v.clone()));
                        b
                    })
                })
                .collect();
        }
        out
    }
}

/// A placeholder for a required field of `field_type`.
fn seed_of(field_type: &str) -> Value {
    match field_type {
        "number" | "integer" => Value::from(1),
        "boolean" | "bool" => Value::Bool(true),
        "array" => Value::Array(Vec::new()),
        "object" => Value::Object(Map::new()),
        _ => Value::String(SEED.to_string()),
    }
}

fn set(md: &mut Value, key: &str, v: Value) {
    if !md.is_object() {
        *md = Value::Object(Map::new());
    }
    if let Some(obj) = md.as_object_mut() {
        obj.insert(key.to_string(), v);
    }
}

/// Re-evaluate, then complete a ready terminal the moment it is ready —
/// the dispatcher's complete-on-ready rule — which closes the packet on
/// it, stamps its outcome and skips what is left.
fn settle(spec: &WorkflowSpec, st: &mut State, now: Instant) {
    crate::registry::reevaluate(spec, &mut st.steps, &st.job.subject, &st.job.metadata);
    let terminal = spec.steps.iter().find_map(|ss| {
        let t = ss.terminal.as_ref()?;
        let i = st.steps.iter().position(|s| {
            s.spec_slug.as_deref() == Some(ss.title.as_str()) && s.status == StepStatus::Ready
        })?;
        Some((i, ss.title.clone(), t.outcome.clone()))
    });
    let Some((i, slug, outcome)) = terminal else {
        return;
    };
    for (j, s) in st.steps.iter_mut().enumerate() {
        if j == i {
            s.status = StepStatus::Completed;
            s.completed_at = Some(now);
        } else if s.status != StepStatus::Completed {
            s.status = StepStatus::Skipped;
        }
    }
    st.job.status = JobStatus::Closed;
    st.job.closed_on = Some(now.date_naive());
    set(
        &mut st.job.metadata,
        crate::job_outcome::OUTCOME_KEY,
        Value::String(outcome),
    );
    set(
        &mut st.job.metadata,
        "closed_at",
        Value::String(now.to_rfc3339()),
    );
    st.closed_on = Some(slug);
}

/// Complete step `i` with `branch`'s values and its required fields.
fn complete(
    spec: &WorkflowSpec,
    st: &State,
    i: usize,
    branch: &[(String, Value)],
    now: Instant,
) -> State {
    let mut next = st.clone();
    let slug = next.steps[i].spec_slug.clone().unwrap_or_default();
    let fields = spec
        .steps
        .iter()
        .find(|s| s.title == slug)
        .map(|s| s.fields.clone())
        .unwrap_or_default();
    let step = &mut next.steps[i];
    for f in fields.iter().filter(|f| f.required) {
        if step.metadata.get(&f.name).is_none_or(Value::is_null) {
            set(&mut step.metadata, &f.name, seed_of(&f.field_type));
        }
    }
    for (k, v) in branch {
        set(&mut step.metadata, k, v.clone());
    }
    step.status = StepStatus::Completed;
    step.completed_at = Some(now);
    settle(spec, &mut next, now);
    next
}

/// The packet the protocol admits: materialised by the engine, its
/// trigger completed (admission completes it), then settled.
fn admit(spec: &WorkflowSpec, seeds: &Value, now: Instant) -> State {
    let subject = Subject::new("custom", SEED);
    let mut job = Job::new(
        spec.kind.clone(),
        subject.clone(),
        SEED,
        SEED,
        Priority::Standard,
        now.date_naive(),
    )
    .with_id(JobId::from_uuid(uuid::Uuid::from_u128(1)));
    job.status = JobStatus::Open;
    job.workflow_version = spec.version;
    job.opened_at = Some(now);
    job.metadata = seeds.clone();
    set(
        &mut job.metadata,
        "opened_at",
        Value::String(now.to_rfc3339()),
    );
    let mut n = 1u128;
    let mut steps =
        crate::registry::materialize_steps(spec, &subject, job.id, &job.metadata, || {
            n += 1;
            StepId::from_uuid(uuid::Uuid::from_u128(n))
        });
    for s in steps.iter_mut() {
        if s.kind == crate::regions::TRIGGER_STEP_KIND && s.status == StepStatus::Ready {
            s.status = StepStatus::Completed;
            s.completed_at = Some(now);
        }
    }
    let mut st = State {
        job,
        steps,
        closed_on: None,
    };
    settle(spec, &mut st, now);
    st
}

/// How a closed packet still standing on a region leaves it: the
/// reading's window passes its closing (see [`walk`]).
pub const AGED_OUT: &str = "closed, then aged out of the window";

/// WALK ONE PROTOCOL (see the module note). `place` stands one packet on
/// the map as a reading taken at the instant it is handed; `seeds` is the
/// job metadata the walk admits the packet with; `markers` the `(step,
/// key)` pairs a station reads as a marker; `window_hours` the window the
/// placement reads, past which a closed packet it still stands on a
/// region — an arrived train — is placed again, and wherever that leaves
/// it is the terminal's off-ramp ([`AGED_OUT`]; backlog d085dc47).
pub fn walk(
    spec: &WorkflowSpec,
    seeds: &Value,
    markers: &[(String, String)],
    now: Instant,
    window_hours: i64,
    place: impl Fn(&Job, &[Step], Instant) -> Placed,
) -> Walk {
    let aged = now + chrono::Duration::hours(window_hours) + chrono::Duration::seconds(1);
    let place_now = |job: &Job, steps: &[Step]| place(job, steps, now);
    let candidates = Candidates::of(spec);
    let region = |p: &Placed| match p {
        Placed::At(r) => Some(*r),
        _ => None,
    };
    let first = admit(spec, seeds, now);
    // The step that admits the packet: its trigger, or — for a protocol
    // that declares none — its first step.
    let trigger = spec
        .steps
        .iter()
        .find(|s| s.kind == crate::regions::TRIGGER_STEP_KIND)
        .or_else(|| spec.steps.first())
        .map(|s| s.title.clone())
        .unwrap_or_default();
    let mut crossings: BTreeSet<Crossing> = BTreeSet::new();
    let mut at: BTreeMap<String, Placed> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(State, Placed)> = VecDeque::new();
    let placed = place_now(&first.job, &first.steps);
    if let Some(r) = region(&placed) {
        crossings.insert(Crossing {
            from: None,
            to: Some(r),
            step: trigger,
            via: "admitted".into(),
        });
    }
    seen.insert(first.key());
    queue.push_back((first, placed));
    let mut truncated = false;

    while let Some((st, here)) = queue.pop_front() {
        for s in st.steps.iter().filter(|s| s.status == StepStatus::Ready) {
            if let Some(slug) = &s.spec_slug {
                at.entry(slug.clone()).or_insert_with(|| here.clone());
            }
        }
        if let Some(slug) = &st.closed_on {
            at.entry(slug.clone()).or_insert_with(|| here.clone());
        }
        if !st.open() {
            // A closed packet the placement still stands on a region (an
            // arrived train, read inside its window) leaves it when the
            // window passes its closing: the same packet, placed by the
            // same function one window later. Without this the walk
            // stopped here and arrivals served no exit, so every train
            // that ever arrived vanished off the map by no drawn route
            // (backlog d085dc47; David's off-ramps, design e765b3fc).
            if let (Some(a), Some(slug)) = (region(&here), &st.closed_on) {
                let to = region(&place(&st.job, &st.steps, aged));
                if to != Some(a) {
                    crossings.insert(Crossing {
                        from: Some(a),
                        to,
                        step: slug.clone(),
                        via: AGED_OUT.into(),
                    });
                }
            }
            continue;
        }
        // Every move out of this state: (the next state, the step that
        // moved it, how).
        let mut next: Vec<(State, String, String)> = Vec::new();
        for (i, s) in st.steps.iter().enumerate() {
            if s.status != StepStatus::Ready {
                continue;
            }
            let slug = s.spec_slug.clone().unwrap_or_default();
            for branch in candidates.branches(&slug) {
                let via = if branch.is_empty() {
                    "completed".to_string()
                } else {
                    let said: Vec<String> = branch
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "{k}={}",
                                v.as_str().map_or_else(|| v.to_string(), str::to_string)
                            )
                        })
                        .collect();
                    format!("completed, {}", said.join(", "))
                };
                next.push((complete(spec, &st, i, &branch, now), slug.clone(), via));
            }
        }
        for (key, vals) in &candidates.job {
            for v in vals {
                if st.job.metadata.get(key) == Some(v) {
                    continue;
                }
                let mut n = st.clone();
                set(&mut n.job.metadata, key, v.clone());
                let before: Vec<StepStatus> = n.steps.iter().map(|s| s.status).collect();
                settle(spec, &mut n, now);
                // Written only where it opens a step: a key nobody waits on
                // yet is a state the record never passes through.
                let opened = n.steps.iter().zip(&before).find(|(s, b)| {
                    **b == StepStatus::Pending
                        && s.status != StepStatus::Pending
                        && s.status != StepStatus::Skipped
                });
                if let Some((s, _)) = opened {
                    let slug = s.spec_slug.clone().unwrap_or_default();
                    let said = v.as_str().map_or_else(|| v.to_string(), str::to_string);
                    next.push((n, slug, format!("job.metadata.{key}={said}")));
                }
            }
        }
        for (slug, key) in markers {
            let Some(i) = st.steps.iter().position(|s| {
                s.spec_slug.as_deref() == Some(slug.as_str()) && s.status == StepStatus::Ready
            }) else {
                continue;
            };
            let mut n = st.clone();
            let marked = crate::stranded::marked(&n.steps[i].metadata, key).is_some();
            let (v, via) = if marked {
                (Value::Bool(false), format!("marker {key} released"))
            } else {
                (
                    Value::String(format!("set by the route walk ({SEED})")),
                    format!("marker {key} set"),
                )
            };
            set(&mut n.steps[i].metadata, key, v);
            next.push((n, slug.clone(), via));
        }

        for (n, step, via) in next {
            let there = place_now(&n.job, &n.steps);
            // A move that closed the packet is named by the terminal it
            // closed on — the off-ramp, or the region a closed packet
            // still stands in (an arrived train) — and says what closed it.
            let (step, via) = match &n.closed_on {
                Some(terminal) => (terminal.clone(), format!("closed, after {step} {via}")),
                None => (step, via),
            };
            let crossing = match (region(&here), region(&there)) {
                (Some(a), Some(b)) if a != b => Some((Some(a), Some(b))),
                // Off the map only by a terminal.
                (Some(a), None) if !n.open() => Some((Some(a), None)),
                (None, Some(b)) => Some((None, Some(b))),
                _ => None,
            };
            if let Some((from, to)) = crossing {
                crossings.insert(Crossing {
                    from,
                    to,
                    step,
                    via,
                });
            }
            if seen.len() >= MAX_STATES {
                truncated = true;
                continue;
            }
            if seen.insert(n.key()) {
                queue.push_back((n, there));
            }
        }
    }
    Walk {
        kind: spec.kind.clone(),
        version: spec.version,
        states: seen.len(),
        truncated,
        crossings: crossings.into_iter().collect(),
        at,
        terminals: spec
            .steps
            .iter()
            .filter(|s| s.terminal.is_some())
            .map(|s| s.title.clone())
            .collect(),
    }
}

/// The job metadata a walk admits a packet of `kind` with: the `branch`
/// every filer stamps, and every key a station's own predicate requires
/// present or equal for this kind — so "can this packet stand at that
/// station" is asked of the station's clauses, not assumed.
pub fn seeds_for(kind: &str, stations: &Stations) -> Value {
    let mut md = Value::Object(Map::new());
    set(
        &mut md,
        crate::moves::SAME_BRANCH,
        Value::String(SEED.to_string()),
    );
    for p in stations.for_kind(kind) {
        for key in &p.metadata_present {
            set(&mut md, key, Value::String(SEED.to_string()));
        }
        for (key, want) in &p.metadata_equals {
            set(&mut md, key, Value::String(want.clone()));
        }
    }
    md
}

/// The `(step, key)` markers a station reads for `kind` (a car's `hold`
/// on its review step).
pub fn markers_for(kind: &str, stations: &Stations) -> Vec<(String, String)> {
    let mut out: BTreeSet<(String, String)> = BTreeSet::new();
    for p in stations.for_kind(kind) {
        let Some(step) = &p.step else { continue };
        let Some(slug) = &step.slug else { continue };
        for key in &step.metadata_unmarked {
            out.insert((slug.clone(), key.clone()));
        }
    }
    out.into_iter().collect()
}

/// WALK EVERY PROTOCOL THE MAP PLACES: every active platform row, each
/// through the family [`family_of`] reads it into, in kind order.
///
/// EVERY PLATFORM KIND, not the families the map was drawn for (backlog
/// e23005c0). The live stations are judged over every open packet, so
/// any kind whose step waits at a station stands in marshalling — an
/// ops-request at `execute`, a chore at its task — and leaves it by its
/// own terminal. Walking only the named families left 217 ops-request
/// exits in one day with no route that named their terminal. A kind no
/// station ever holds walks to no crossing at all, which costs a few
/// states and draws nothing.
pub fn walk_all(specs: &[WorkflowSpec], stations: &Stations, now: Instant) -> Vec<Walk> {
    let active: Vec<WorkflowSpec> = specs
        .iter()
        .filter(|s| s.status == WorkflowStatus::Active)
        .cloned()
        .collect();
    let inbound = inbound_kinds(&active);
    let mut walks: Vec<Walk> = active
        .iter()
        .filter(|spec| spec.category == "platform")
        .map(|spec| {
            let family = family_of(&spec.kind, &inbound);
            walk(
                spec,
                &seeds_for(&spec.kind, stations),
                &markers_for(&spec.kind, stations),
                now,
                crate::regions::DEFAULT_WINDOW_HOURS,
                |job, steps, at| {
                    place_alone(
                        &(job.clone(), steps.to_vec()),
                        family,
                        stations,
                        at,
                        crate::regions::DEFAULT_WINDOW_HOURS,
                    )
                },
            )
        })
        .collect();
    walks.sort_by(|a, b| a.kind.cmp(&b.kind));
    walks
}

/// ONE DECLARED HAND-OFF (`infra/platform/yard/handoffs.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandOff {
    /// `rule:<name>`, `cadence:<name>` or `verb:<name>`.
    pub by: String,
    /// `kind@step` of the packet handed off; `None` for a filing onto
    /// the map from nowhere.
    #[serde(default)]
    pub from: Option<String>,
    /// `kind@step` of the packet that continues; `None` when the maker
    /// takes the one handed off off the map without a terminal —
    /// absorbed into a packet already standing, or released by a reading
    /// it writes onto it.
    #[serde(default)]
    pub to: Option<String>,
    pub why: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandOffsFile {
    #[serde(default, rename = "hand_off")]
    hand_offs: Vec<HandOff>,
}

/// The declarations, parsed. An unparseable file is an error the routes
/// read carries, never an empty list that reads as "nothing declared".
pub fn hand_offs(src: &str) -> Result<Vec<HandOff>, String> {
    toml::from_str::<HandOffsFile>(src)
        .map(|f| f.hand_offs)
        .map_err(|e| format!("infra/platform/yard/handoffs.toml does not parse: {e}"))
}

/// Where `kind@step` stands, by the walks: a region, or why it cannot
/// be resolved.
pub fn stands(walks: &[Walk], at: &str) -> Result<&'static str, String> {
    let (kind, step) = at
        .split_once('@')
        .ok_or_else(|| format!("{at}: not `kind@step`"))?;
    let walk = walks
        .iter()
        .find(|w| w.kind == kind)
        .ok_or_else(|| format!("{at}: `{kind}` is no kind the map places"))?;
    match walk.at.get(step) {
        Some(Placed::At(r)) => Ok(r),
        Some(other) => Err(format!(
            "{at}: while `{step}` is ready the packet stands on no region ({other:?})"
        )),
        None => Err(format!(
            "{at}: the walk of {kind} v{} never reaches `{step}`",
            walk.version
        )),
    }
}

/// What supports one route.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum Source {
    /// A protocol walk's crossing.
    Workflow {
        workflow: String,
        version: i32,
        step: String,
        via: String,
    },
    /// A declared hand-off.
    HandOff {
        by: String,
        from: Option<String>,
        to: Option<String>,
        why: String,
        /// The terminal it hands off, when `from` names one (a green in
        /// the garage); `None` when it takes the packet from a step
        /// before its close, whatever that close turns out to be.
        terminal: Option<String>,
    },
    /// The moves record: how many moves took it in the window. An exit
    /// is counted per kind and terminal ([`RouteCount`]); both are
    /// `None` on a move between regions, and on an exit whose packet
    /// the mover could not read back.
    Observed {
        moves: i64,
        last_at: Instant,
        kind: Option<String>,
        terminal: Option<String>,
    },
}

impl Source {
    fn declares(&self) -> bool {
        !matches!(self, Source::Observed { .. })
    }

    /// Whether this source declares a packet of `kind` leaving by
    /// `terminal`: a protocol crossing cited to that terminal of that
    /// kind, or a hand-off of that kind that names the terminal or
    /// takes the packet whatever it closes on.
    fn declares_exit(&self, kind: &str, terminal: &str) -> bool {
        match self {
            Source::Workflow { workflow, step, .. } => workflow == kind && step == terminal,
            Source::HandOff {
                from: Some(from),
                terminal: handed,
                ..
            } => {
                from.split_once('@').map(|(k, _)| k) == Some(kind)
                    && handed.as_deref().is_none_or(|t| t == terminal)
            }
            _ => false,
        }
    }
}

/// One route: from a region (or onto the map) to a region (or off it),
/// and everything that supports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Route {
    pub from: Option<String>,
    pub to: Option<String>,
    /// A protocol or a hand-off supports it. `false`: only moves did —
    /// observed, undeclared.
    pub declared: bool,
    pub sources: Vec<Source>,
}

/// A walk as the routes read reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Walked {
    pub kind: String,
    pub version: i32,
    pub states: usize,
    pub truncated: bool,
}

/// THE ROUTES: every route any source supports, in (from, to) order,
/// and anything a source declared that could not be resolved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouteMap {
    pub routes: Vec<Route>,
    pub walked: Vec<Walked>,
    /// Hand-offs that could not be resolved, each naming why. Empty on
    /// the tree (pinned); a live registry that has moved past it says so
    /// here rather than dropping the route in silence.
    pub refused: Vec<String>,
}

impl RouteMap {
    /// Whether a move from `from` to `to` took a declared route. A move
    /// within one region crosses no border and needs none.
    pub fn declares(&self, from: Option<&str>, to: Option<&str>) -> bool {
        from == to
            || self
                .routes
                .iter()
                .any(|r| r.declared && r.from.as_deref() == from && r.to.as_deref() == to)
    }

    /// Whether a move took a declared route, judged as finely as the
    /// move says (backlog e23005c0): an EXIT whose kind and terminal are
    /// known is declared only by a source naming that terminal of that
    /// kind ([`Source::declares_exit`]) — a region's exit declared for
    /// one protocol's terminals no longer vouches for another's. Every
    /// other move, and an exit whose packet could not be read back, is
    /// judged by its route ([`RouteMap::declares`]).
    pub fn judges(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        kind: Option<&str>,
        terminal: Option<&str>,
    ) -> bool {
        match (to, kind.filter(|k| !k.is_empty()), terminal) {
            (None, Some(kind), Some(terminal)) if from.is_some() => self
                .routes
                .iter()
                .filter(|r| r.from.as_deref() == from && r.to.is_none())
                .flat_map(|r| &r.sources)
                .any(|s| s.declares_exit(kind, terminal)),
            _ => self.declares(from, to),
        }
    }

    /// The observed counts no source declares ([`RouteMap::judges`]),
    /// each with its kind and terminal where it is an exit.
    pub fn undeclared(&self) -> Vec<RouteCount> {
        self.routes
            .iter()
            .flat_map(|r| {
                r.sources.iter().filter_map(|s| match s {
                    Source::Observed {
                        moves,
                        last_at,
                        kind,
                        terminal,
                    } => Some(RouteCount {
                        from: r.from.clone(),
                        to: r.to.clone(),
                        kind: kind.clone(),
                        terminal: terminal.clone(),
                        moves: *moves,
                        last_at: *last_at,
                    }),
                    _ => None,
                })
            })
            .filter(|c| !self.judges_count(c))
            .collect()
    }

    /// [`RouteMap::judges`] over one observed count.
    pub fn judges_count(&self, c: &RouteCount) -> bool {
        self.judges(
            c.from.as_deref(),
            c.to.as_deref(),
            c.kind.as_deref(),
            c.terminal.as_deref(),
        )
    }
}

/// DERIVE THE ROUTES from the three sources (see the module note).
/// `observed` is `None` when the moves record could not be read — the
/// declared routes are still served, with no counts beside them.
pub fn derive(walks: &[Walk], hand_offs: &[HandOff], observed: Option<&[RouteCount]>) -> RouteMap {
    type Key = (Option<String>, Option<String>);
    let mut by: BTreeMap<Key, Vec<Source>> = BTreeMap::new();
    let key = |a: End, b: End| (a.map(str::to_string), b.map(str::to_string));
    for w in walks {
        for c in &w.crossings {
            by.entry(key(c.from, c.to))
                .or_default()
                .push(Source::Workflow {
                    workflow: w.kind.clone(),
                    version: w.version,
                    step: c.step.clone(),
                    via: c.via.clone(),
                });
        }
    }
    let mut refused = Vec::new();
    for h in hand_offs {
        let end = |at: &Option<String>| at.as_ref().map(|a| stands(walks, a)).transpose();
        match (end(&h.from), end(&h.to)) {
            (Ok(None), Ok(None)) => refused.push(format!("{}: names neither end", h.by)),
            (Ok(a), Ok(b)) if a == b => {} // a hand-off inside one region crosses no border
            (Ok(a), Ok(b)) => by.entry(key(a, b)).or_default().push(Source::HandOff {
                by: h.by.clone(),
                from: h.from.clone(),
                to: h.to.clone(),
                why: h.why.clone(),
                terminal: h.from.as_ref().and_then(|at| {
                    let (kind, step) = at.split_once('@')?;
                    walks
                        .iter()
                        .find(|w| w.kind == kind)
                        .filter(|w| w.terminals.contains(step))
                        .map(|_| step.to_string())
                }),
            }),
            (Err(e), _) | (_, Err(e)) => refused.push(format!("{}: {e}", h.by)),
        }
    }
    for c in observed.into_iter().flatten() {
        if c.from == c.to {
            continue;
        }
        by.entry((c.from.clone(), c.to.clone()))
            .or_default()
            .push(Source::Observed {
                moves: c.moves,
                last_at: c.last_at,
                kind: c.kind.clone(),
                terminal: c.terminal.clone(),
            });
    }
    RouteMap {
        routes: by
            .into_iter()
            .map(|((from, to), sources)| Route {
                declared: sources.iter().any(Source::declares),
                from,
                to,
                sources,
            })
            .collect(),
        walked: walks
            .iter()
            .map(|w| Walked {
                kind: w.kind.clone(),
                version: w.version,
                states: w.states,
                truncated: w.truncated,
            })
            .collect(),
        refused,
    }
}

/// The whole derivation from registry rows: walk every placed protocol,
/// resolve the compiled-in hand-offs, and lay the observed counts over.
pub fn derive_from(
    specs: &[WorkflowSpec],
    stations: &[crate::stations::StationSpec],
    observed: Option<&[RouteCount]>,
    now: Instant,
) -> RouteMap {
    let walks = walk_all(specs, &Stations::of(stations), now);
    match hand_offs(HANDOFFS_TOML) {
        Ok(h) => derive(&walks, &h, observed),
        Err(e) => {
            let mut map = derive(&walks, &[], observed);
            map.refused.push(e);
            map
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        chrono::DateTime::parse_from_rfc3339("2026-09-26T05:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// The tree's protocols and stations, as a live instance seeded from
    /// it would hold them: the Workflow bundle, and the station bundle
    /// plus the stations the protocols project (the listing's own two
    /// sources, `http/stations.rs::effective_stations`).
    fn tree() -> (Vec<WorkflowSpec>, Vec<crate::stations::StationSpec>) {
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("the platform Workflow bundle parses");
        let mut stations =
            crate::seed_loader::load_stations(crate::station_seed::platform_stations_path())
                .expect("the platform station bundle parses");
        let names: Vec<String> = stations.iter().map(|s| s.name.clone()).collect();
        stations.extend(crate::station_projection::derived_stations(
            &specs,
            &names,
            now(),
        ));
        (specs, stations)
    }

    fn tree_routes() -> RouteMap {
        let (specs, stations) = tree();
        derive_from(&specs, &stations, None, now())
    }

    fn route<'a>(map: &'a RouteMap, from: Option<&str>, to: Option<&str>) -> Option<&'a Route> {
        map.routes
            .iter()
            .find(|r| r.from.as_deref() == from && r.to.as_deref() == to)
    }

    fn by_workflow(r: &Route, kind: &str) -> Vec<String> {
        r.sources
            .iter()
            .filter_map(|s| match s {
                Source::Workflow { workflow, step, .. } if workflow == kind => Some(step.clone()),
                _ => None,
            })
            .collect()
    }

    /// THE TRAIN'S OWN LINE comes out of pr-train's steps, with no hand
    /// drawing (design e765b3fc §2b): made up at the dock, into the gates
    /// when its PR opens and its train gate runs, onto the track when it
    /// merges, into arrivals when it arrives. The drawn border it
    /// replaces skipped the gates — dock straight to track — and no
    /// protocol state takes it.
    #[test]
    fn the_train_line_is_derived_from_the_pr_train_protocol() {
        let map = tree_routes();
        let train = |from: Option<&str>, to: Option<&str>| {
            route(&map, from, to)
                .map(|r| by_workflow(r, TRAIN))
                .unwrap_or_default()
        };
        const TRAIN: &str = crate::regions::TRAIN_KIND;
        assert!(
            !train(None, Some("dock")).is_empty(),
            "a train is made up at the dock: {:#?}",
            map.routes
        );
        assert_eq!(
            train(Some("dock"), Some("gates")),
            ["pr"],
            "its PR opens and its gate runs"
        );
        assert_eq!(
            train(Some("gates"), Some("track")),
            ["merged"],
            "it departs when it merges"
        );
        assert!(
            train(Some("track"), Some("arrivals")).contains(&"arrived".to_string()),
            "{:#?}",
            route(&map, Some("track"), Some("arrivals"))
        );
        assert!(
            train(Some("dock"), Some("track")).is_empty(),
            "no train goes from the dock to the track without its gate"
        );
        // And it leaves the map by its terminals: a train collected empty
        // is cancelled at the dock.
        assert!(
            train(Some("dock"), None).contains(&"cancelled".to_string()),
            "{:#?}",
            route(&map, Some("dock"), None)
        );
    }

    /// AN ARRIVED TRAIN LEAVES THE MAP BY A DRAWN ROUTE (backlog d085dc47;
    /// David's off-ramps, design e765b3fc): it closes on `arrived` into
    /// arrivals, and the placement drops it once its closing ages out of
    /// the window. Until this car the walk stopped at the closed state, so
    /// arrivals served no exit and every train that ever arrived simply
    /// vanished from the map. The exit is the walk's own finding — the
    /// closed state placed again past the window — cited to the terminal
    /// that closed it, never drawn by hand.
    #[test]
    fn an_arrived_train_leaves_arrivals_by_its_terminal_when_it_ages_out() {
        let map = tree_routes();
        let exit = route(&map, Some("arrivals"), None).expect("arrivals serves an exit");
        assert!(exit.declared, "{exit:#?}");
        assert_eq!(by_workflow(exit, crate::regions::TRAIN_KIND), ["arrived"]);
        assert!(
            exit.sources.iter().any(|s| matches!(
                s,
                Source::Workflow { via, .. } if via.contains("aged out of the window")
            )),
            "the exit says how it is taken: {exit:#?}"
        );
    }

    /// THE ARRIVALS -> PUBLISH CONNECTOR IS DECLARED by the rule that files
    /// the publish packet — `publish-to-github-daily` spawns it on
    /// `target = origin/main`, the main the arrived trains landed on — and
    /// both ends resolve through the walk: the train closed on `arrived`
    /// stands in arrivals, the publish packet reading its checks stands in
    /// publish. The design drew it grey; nothing served it (d085dc47).
    #[test]
    fn arrivals_connects_to_publish_by_the_rule_that_files_the_publish_packet() {
        let map = tree_routes();
        let publish =
            route(&map, Some("arrivals"), Some("publish")).expect("arrivals -> publish is served");
        assert!(publish.declared, "{publish:#?}");
        assert!(
            publish.sources.iter().any(
                |s| matches!(s, Source::HandOff { by, .. } if by == "rule:publish-to-github-daily")
            ),
            "{publish:#?}"
        );
    }

    /// CONSERVATION, PER WINDOW, FOR ARRIVALS (David 2026-09-25, design
    /// e765b3fc: "packets in = on the map + left by an off-ramp, per
    /// window"). Trains closed on `arrived` at instants spread over three
    /// windows, placed by the ONE placement function at the window's start
    /// and at its end: what stood there at the start plus what arrived
    /// during it equals what stands there at the end plus what left — and
    /// every train that left took a route the map declares.
    #[test]
    fn arrivals_conserves_every_train_per_window() {
        let (specs, stations) = tree();
        let map = derive_from(&specs, &stations, None, now());
        let spec = specs
            .iter()
            .find(|w| w.kind == crate::regions::TRAIN_KIND && w.status == WorkflowStatus::Active)
            .expect("the tree has an active pr-train");
        let hours = crate::regions::DEFAULT_WINDOW_HOURS;
        let end = now();
        let start = end - chrono::Duration::hours(hours);
        // One train driven to its arrival by the protocol itself: every
        // ready step completed until it closes.
        let mut arrived = admit(spec, &Value::Object(Map::new()), end);
        while arrived.open() {
            let i = arrived
                .steps
                .iter()
                .position(|s| s.status == StepStatus::Ready)
                .expect("an open train has a ready step");
            arrived = complete(spec, &arrived, i, &[], end);
        }
        assert_eq!(arrived.closed_on.as_deref(), Some("arrived"));
        let stations = Stations::of(&stations);
        let at = |closed: Instant, when: Instant| {
            let mut job = arrived.job.clone();
            set(
                &mut job.metadata,
                "closed_at",
                Value::String(closed.to_rfc3339()),
            );
            place_alone(
                &(job, arrived.steps.clone()),
                crate::regions::Family::Train,
                &stations,
                when,
                hours,
            )
        };
        let closings: Vec<Instant> = (1..=3 * hours)
            .step_by(5)
            .map(|h| end - chrono::Duration::hours(h) + chrono::Duration::minutes(30))
            .collect();
        let here = Placed::At("arrivals");
        let stood = closings.iter().filter(|c| at(**c, start) == here).count();
        let came = closings
            .iter()
            .filter(|c| **c > start && **c <= end)
            .count();
        let stands = closings.iter().filter(|c| at(**c, end) == here).count();
        let left: Vec<Placed> = closings
            .iter()
            .filter(|c| at(**c, start) == here && at(**c, end) != here)
            .map(|c| at(*c, end))
            .collect();
        assert!(
            stood > 0 && came > 0 && !left.is_empty(),
            "the fixture spans the window"
        );
        assert_eq!(stood + came, stands + left.len(), "in = on the map + left");
        for gone in &left {
            let to = match gone {
                Placed::At(r) => Some(*r),
                _ => None,
            };
            assert!(
                map.declares(Some("arrivals"), to),
                "a train left arrivals for {gone:?} by no declared route"
            );
        }
    }

    /// A CAR IS PLACED ON THE DOCK, HELD INTO THE GARAGE AND BACK, AND
    /// LEAVES BY ITS TERMINALS — and it never crosses from the dock to
    /// the shed by itself: it rides its train there, which the moves
    /// record draws from where the train stands and train-reconcile's
    /// hand-off declares. A one-packet walk that guessed that crossing
    /// would draw a line no packet takes.
    #[test]
    fn a_car_is_held_and_released_on_the_dock_and_never_walks_to_the_shed_alone() {
        let map = tree_routes();
        const CAR: &str = crate::regions::CAR_KIND;
        let car = |from: Option<&str>, to: Option<&str>| {
            route(&map, from, to)
                .map(|r| by_workflow(r, CAR))
                .unwrap_or_default()
        };
        assert_eq!(car(Some("dock"), Some("garage")), ["review"], "held");
        assert_eq!(car(Some("garage"), Some("dock")), ["review"], "released");
        assert!(car(Some("dock"), Some("shed")).is_empty());
        for exit in ["settled", "landed-twin", "abandoned"] {
            assert!(
                car(Some("dock"), None).contains(&exit.to_string()),
                "a parked car can leave the map {exit}: {:#?}",
                route(&map, Some("dock"), None)
            );
        }
        assert!(car(Some("shed"), None).contains(&"merged".to_string()));
        // The car peels off its train at the GATES: the conductor closes
        // every boarded car before it completes the train's `merged` step
        // (backlog 47880c86), so no car ever leaves from the track.
        let shed = route(&map, Some("gates"), Some("shed")).expect("gates -> shed is declared");
        assert!(
            shed.sources.iter().any(
                |s| matches!(s, Source::HandOff { by, .. } if by == "cadence:train-reconcile")
            ),
            "{shed:#?}"
        );
        assert!(
            route(&map, Some("track"), Some("shed")).is_none(),
            "no source draws track -> shed, which no car takes"
        );
    }

    /// EVERY ROUTE THE LIVE RECORD TOOK BETWEEN REGIONS, OR ONTO THE MAP,
    /// IS DECLARED (backlog 47880c86). The fixture is the moves record of
    /// 2026-09-26 06:26Z-21:57Z (`GET /api/yard/moves`, all 1233 rows),
    /// every route but the exits — which the record did not yet name by
    /// terminal and `every_exit_the_record_took_has_a_declared_route`
    /// pins — with how many took it. Four were undeclared on the live map:
    ///
    /// * gates -> shed x54: a real route, declared from the wrong end.
    ///   The conductor closes the cars before the train's `merged`, so
    ///   they leave the train at the gates (handoffs.toml).
    /// * receiving -> shop-floor x27: a real route nothing declared — a
    ///   run dispatched on an untriaged item's `triage` (handoffs.toml).
    /// * shed -> arrivals x66 and off the map -> garage x1: the MOVER'S
    ///   reading, not a route. A car that closes on `merged` in the shed
    ///   was drawn riding the arrived train that still names it, and a
    ///   held car filed from a green standing in the garage was drawn
    ///   filed from nowhere. Both are fixed in `moves::moves` (pinned
    ///   there) and stay undeclared here: no packet takes them.
    #[test]
    fn every_route_the_live_record_took_is_declared() {
        let map = tree_routes();
        const TAKEN: [(Option<&str>, &str, i64); 20] = [
            (None, "garage", 1),
            (None, "gates", 4),
            (None, "marshalling", 256),
            (None, "receiving", 27),
            (None, "shop-floor", 1),
            (Some("dock"), "garage", 2),
            (Some("dock"), "gates", 102),
            (Some("garage"), "dock", 9),
            (Some("garage"), "gates", 8),
            (Some("gates"), "dock", 45),
            (Some("gates"), "garage", 17),
            (Some("gates"), "shed", 54),
            (Some("gates"), "track", 17),
            (Some("marshalling"), "shop-floor", 40),
            (Some("receiving"), "marshalling", 42),
            (Some("receiving"), "shop-floor", 27),
            (Some("shed"), "arrivals", 66),
            (Some("shop-floor"), "gates", 56),
            (Some("shop-floor"), "marshalling", 6),
            (Some("track"), "arrivals", 18),
        ];
        const THE_MOVERS_READING: [(Option<&str>, &str); 2] =
            [(Some("shed"), "arrivals"), (None, "garage")];
        let undeclared: Vec<_> = TAKEN
            .iter()
            .filter(|(from, to, _)| !THE_MOVERS_READING.contains(&(*from, *to)))
            .filter(|(from, to, _)| !map.declares(*from, Some(to)))
            .collect();
        assert!(undeclared.is_empty(), "undeclared: {undeclared:#?}");
        for (from, to) in THE_MOVERS_READING {
            assert!(
                !map.declares(from, Some(to)),
                "{from:?} -> {to} is the mover's reading, fixed in moves::moves, not a route"
            );
        }
    }

    /// A BUILDER'S GATE THAT DIES IN LINE REACHES THE GARAGE FROM THE SHOP
    /// FLOOR (backlog b43042cf). Measured 2026-09-28: gate-run 40385690
    /// (feat/three-bounded-github-verbs) was filed by run 8403148a at
    /// 06:11:48, waited for a bay, its waiter's last queue beat at 06:29:33,
    /// and the conductor's reconcile settled it `lost` at 06:50:30. A
    /// queued gate-run occupies no bay, so it stands on no region
    /// (`yard::gates`); it first appears in the garage's limbo, and the
    /// mover draws it handed off from the run that filed it, still
    /// `building` on the shop floor. That was the one undeclared route on
    /// the live map, and it troubled the garage for an ordinary death.
    #[test]
    fn a_gate_that_dies_waiting_for_a_bay_reaches_the_garage_from_its_run() {
        let map = tree_routes();
        let r = route(&map, Some("shop-floor"), Some("garage"))
            .expect("shop-floor -> garage is a route");
        assert!(r.declared, "{r:#?}");
        assert!(
            r.sources.iter().any(|s| matches!(
                s,
                Source::HandOff { by, from, to, terminal, .. }
                    if by == "cadence:train-reconcile"
                        && from.as_deref() == Some("agent-run@building")
                        && to.as_deref() == Some("gate-run@lost")
                        && terminal.is_none()
            )),
            "declared by the reconcile that settles the orphan: {r:#?}"
        );
    }

    /// EVERY ROUTE IS SOURCED, AND EVERY SOURCE RESOLVES IN THE TREE: a
    /// protocol source names a workflow at the version the bundle holds
    /// and a step that workflow has; no walk was cut short; the pr-train
    /// line and the inbound pair are among them.
    #[test]
    fn every_route_is_sourced_and_every_source_resolves_in_the_tree() {
        let (specs, _) = tree();
        let map = tree_routes();
        assert!(!map.routes.is_empty());
        for r in &map.routes {
            assert!(
                r.declared,
                "{r:#?}: with no moves read, every route is declared"
            );
            assert!(!r.sources.is_empty(), "{r:#?}");
            for s in &r.sources {
                if let Source::Workflow {
                    workflow,
                    version,
                    step,
                    ..
                } = s
                {
                    let spec = specs
                        .iter()
                        .find(|w| &w.kind == workflow && w.version == *version)
                        .unwrap_or_else(|| panic!("{r:#?}: no {workflow} v{version} in the tree"));
                    assert!(
                        spec.steps.iter().any(|st| &st.title == step),
                        "{r:#?}: {workflow} v{version} has no step `{step}`"
                    );
                }
            }
        }
        for w in &map.walked {
            assert!(
                !w.truncated,
                "{} v{} was cut short at {} states",
                w.kind, w.version, w.states
            );
        }
        let kinds: Vec<&str> = map.walked.iter().map(|w| w.kind.as_str()).collect();
        for kind in [
            "pr-train",
            "ship-a-change",
            "gate-run",
            "agent-run",
            "backlog-item",
        ] {
            assert!(kinds.contains(&kind), "{kind} is walked: {kinds:?}");
        }
        let intake = route(&map, Some("receiving"), Some("marshalling"))
            .expect("an inbound packet is taken in");
        assert!(
            by_workflow(intake, "backlog-item").contains(&"triage".to_string()),
            "{intake:#?}"
        );
    }

    /// EVERY DECLARED HAND-OFF IS WALKED AND NAMES ITS MAKER: each row
    /// parses, both ends resolve through the walk to a region (so a
    /// renamed step or kind fails here, naming the row), and the rule or
    /// cadence it names has its file. The four the design requires are
    /// declared. (A `verb:` is held to boss-cli's subcommands by boss-cli's
    /// own test, which can see them.)
    #[test]
    fn every_declared_hand_off_is_walked_and_names_its_maker() {
        let rows = hand_offs(HANDOFFS_TOML).expect("handoffs.toml parses");
        assert!(!rows.is_empty());
        let map = tree_routes();
        assert!(map.refused.is_empty(), "{:#?}", map.refused);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        for h in &rows {
            assert!(!h.why.trim().is_empty(), "{}: a hand-off says why", h.by);
            let (kind, name) = h.by.split_once(':').unwrap_or(("", ""));
            let file = match kind {
                "rule" => Some(root.join(format!("infra/dispatcher/rules/{name}.toml"))),
                "cadence" => Some(root.join(format!("infra/platform/cadence/{name}.toml"))),
                "verb" => None,
                other => panic!("{}: `{other}` is not rule, cadence or verb", h.by),
            };
            if let Some(file) = file {
                assert!(file.exists(), "{}: {} does not exist", h.by, file.display());
            }
        }
        for required in [
            "rule:auto-park-on-gate-green",
            "cadence:train-board-on-dock-depth",
            "cadence:train-reconcile",
            "rule:an-abandoned-step-is-reclaimed-when-its-run-died",
        ] {
            assert!(
                rows.iter().any(|h| h.by == required),
                "{required} declares its hand-off (design e765b3fc §2b)"
            );
        }
        // A hand-off naming a kind the map does not place, or a step its
        // protocol never reaches, is refused by name.
        let walks = walk_all(&tree().0, &Stations::of(&tree().1), now());
        let bad = derive(
            &walks,
            &[HandOff {
                by: "verb:gate".into(),
                from: Some("gate-run@no-such-step".into()),
                to: Some("design-doc@review".into()),
                why: "a typo".into(),
            }],
            None,
        );
        assert_eq!(bad.refused.len(), 1, "{:#?}", bad.refused);
        assert!(
            bad.refused[0].contains("never reaches `no-such-step`"),
            "{}",
            bad.refused[0]
        );
    }

    /// A FIXTURE WORKFLOW WHOSE NEW STEP CREATES A ROUTE, WITH NO RUST
    /// CHANGE (the design's test for car R2): the tree's pr-train, and
    /// the same row with one more terminal — a merged train can be
    /// derailed. The new off-ramp from the track appears, sourced by the
    /// new step, and nothing else about the line changes.
    #[test]
    fn a_fixture_workflow_whose_new_step_creates_a_route_with_no_rust_change() {
        let (specs, stations) = tree();
        let before = derive_from(&specs, &stations, None, now());
        let derail = |r: &RouteMap| {
            route(r, Some("track"), None)
                .map(|r| by_workflow(r, "pr-train"))
                .unwrap_or_default()
        };
        assert!(!derail(&before).contains(&"derailed".to_string()));

        let mut specs = specs;
        let train = specs
            .iter_mut()
            .find(|w| w.kind == "pr-train")
            .expect("the tree has pr-train");
        let mut step = train
            .steps
            .iter()
            .find(|s| s.terminal.is_some())
            .expect("pr-train has a terminal")
            .clone();
        step.title = "derailed".into();
        step.ready_when = "steps.merged.done AND job.metadata.derailed = \"true\"".into();
        step.terminal = Some(crate::registry::Terminal {
            outcome: "derailed".into(),
        });
        train.steps.push(step);
        let after = derive_from(&specs, &stations, None, now());
        assert!(
            derail(&after).contains(&"derailed".to_string()),
            "{:#?}",
            route(&after, Some("track"), None)
        );
        let line = |r: &RouteMap| {
            r.routes
                .iter()
                .filter(|x| x.to.is_some())
                .map(|x| (x.from.clone(), x.to.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(line(&before), line(&after), "only the off-ramp is new");
    }

    /// EVERY EXIT THE RECORD TOOK HAS A DECLARED ROUTE, TERMINAL BY
    /// TERMINAL (backlog e23005c0). The fixture is the moves record of
    /// 2026-09-26 06:26Z-20:31Z (design exhibit f0313eda: 1105 moves),
    /// each exit read with the terminal its packet closed on — the
    /// region it left, its kind, its terminal, and how many took it. An
    /// exit is declared only by a source naming THAT terminal (a
    /// protocol's crossing cited to it) or a hand-off that absorbs the
    /// packet from that region; a route declared for one kind's
    /// terminal no longer vouches for another's.
    ///
    /// Three rows were undeclared against the tree before this car:
    /// ops-request `answered` from marshalling (the walk never walked
    /// ops-request), and gate-run `green` / `failed` from the garage
    /// (the auto-park and re-gate hand-offs were declared only from the
    /// gates). The fourth finding, agent-run `landed` x19 / `refused`
    /// x1 from marshalling, was the placement's error, not the map's —
    /// a read skew (`regions::partition`, the newer read wins) — so it
    /// stays undeclared here: a run leaves from the shop floor.
    #[test]
    fn every_exit_the_record_took_has_a_declared_route() {
        let map = tree_routes();
        const EXITS: [(&str, &str, &str, i64); 20] = [
            ("marshalling", "ops-request", "answered", 217),
            ("marshalling", "ops-request", "refused", 1),
            ("marshalling", "backlog-item", "closed", 42),
            ("marshalling", "backlog-item", "stale", 2),
            ("marshalling", "backlog-item", "duplicate", 2),
            ("shop-floor", "agent-run", "landed", 48),
            ("shop-floor", "agent-run", "unreported", 7),
            ("shop-floor", "agent-run", "died", 2),
            ("shop-floor", "agent-run", "refused", 1),
            ("gates", "gate-run", "green", 30),
            ("garage", "gate-run", "green", 5),
            ("garage", "gate-run", "failed", 2),
            ("garage", "ship-a-change", "landed-twin", 2),
            ("receiving", "backlog-item", "stale", 8),
            ("receiving", "backlog-item", "duplicate", 1),
            ("receiving", "user-feedback", "declined", 1),
            // And the ones the tree already declared, kept as the pin.
            ("dock", "ship-a-change", "settled", 0),
            ("shed", "ship-a-change", "merged", 0),
            ("track", "pr-train", "merge-lost", 0),
            ("arrivals", "pr-train", "arrived", 0),
        ];
        let undeclared: Vec<_> = EXITS
            .iter()
            .filter(|(from, kind, terminal, _)| {
                !map.judges(Some(from), None, Some(kind), Some(terminal))
            })
            .collect();
        assert!(undeclared.is_empty(), "undeclared exits: {undeclared:#?}");
        // The skew's exits stay undeclared: a run is on the shop floor
        // until it closes, and a closed run stands at no station.
        for terminal in ["landed", "refused"] {
            assert!(
                !map.judges(Some("marshalling"), None, Some("agent-run"), Some(terminal)),
                "agent-run {terminal} from marshalling is the read skew, not a route"
            );
        }
    }

    /// AN EXIT IS JUDGED BY THE TERMINAL IT LEFT BY: marshalling -> off
    /// the map is declared for backlog-item's terminals, and an exit from
    /// there by a terminal no source names is undeclared all the same —
    /// counted by that terminal, beside the declared ones.
    #[test]
    fn an_exit_is_judged_by_the_terminal_it_left_by() {
        let (specs, stations) = tree();
        let at = now();
        let exit = |kind: &str, terminal: &str, moves: i64| RouteCount {
            from: Some("marshalling".into()),
            to: None,
            kind: Some(kind.into()),
            terminal: Some(terminal.into()),
            moves,
            last_at: at,
        };
        let observed = [
            exit("backlog-item", "closed", 3),
            exit("backlog-item", "no-such-terminal", 2),
        ];
        let map = derive_from(&specs, &stations, Some(&observed), at);
        let route = route(&map, Some("marshalling"), None).unwrap();
        assert!(route.declared, "the region's exit is declared");
        assert_eq!(
            map.undeclared()
                .iter()
                .map(|c| (c.terminal.as_deref(), c.moves))
                .collect::<Vec<_>>(),
            [(Some("no-such-terminal"), 2)]
        );
        // A move whose terminal is unknown (a packet the mover could not
        // read back) is judged by the route alone, as before.
        assert!(map.judges(Some("marshalling"), None, None, None));
    }

    /// OPS-REQUEST IS WALKED (backlog e23005c0): it stands in marshalling
    /// while its `execute` step waits at its station, and leaves by
    /// `answered` or `refused` — the route the 217 answered requests of
    /// 2026-09-26 took with no source naming it.
    #[test]
    fn an_ops_request_is_walked_into_marshalling_and_out_by_its_terminals() {
        let map = tree_routes();
        assert!(map.walked.iter().any(|w| w.kind == "ops-request"));
        let out = route(&map, Some("marshalling"), None).expect("marshalling serves exits");
        let steps = by_workflow(out, "ops-request");
        for terminal in ["answered", "refused"] {
            assert!(steps.contains(&terminal.to_string()), "{out:#?}");
        }
    }

    /// THE OBSERVED SOURCE: a count on a declared route rides beside its
    /// declaration; a count on a route nothing declares is the one
    /// undeclared route, with its count.
    #[test]
    fn a_move_on_no_declared_route_is_observed_undeclared() {
        let (specs, stations) = tree();
        let at = now();
        let observed = [
            RouteCount {
                from: Some("gates".into()),
                to: Some("dock".into()),
                kind: None,
                terminal: None,
                moves: 4,
                last_at: at,
            },
            RouteCount {
                from: Some("shed".into()),
                to: Some("arrivals".into()),
                kind: None,
                terminal: None,
                moves: 1,
                last_at: at,
            },
        ];
        let map = derive_from(&specs, &stations, Some(&observed), at);
        let dock = route(&map, Some("gates"), Some("dock")).unwrap();
        assert!(dock.declared, "the auto-park hand-off declares it");
        assert!(
            dock.sources
                .iter()
                .any(|s| matches!(s, Source::Observed { moves: 4, .. }))
        );
        let shed = route(&map, Some("shed"), Some("arrivals")).unwrap();
        assert!(
            !shed.declared,
            "a car in the shed never walks to arrivals: it leaves by its proof"
        );
        assert_eq!(
            map.undeclared()
                .iter()
                .map(|c| (c.from.as_deref(), c.to.as_deref(), c.moves))
                .collect::<Vec<_>>(),
            [(Some("shed"), Some("arrivals"), 1)]
        );
        assert!(
            map.declares(Some("dock"), Some("dock")),
            "a move within one region crosses nothing"
        );
        assert!(!map.declares(Some("shed"), Some("arrivals")));
    }
}
