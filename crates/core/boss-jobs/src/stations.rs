//! Station registry — stations.md (Q1–Q4 ratified 2026-08-13).
//!
//! A **station** is an abstract priority queue that routes or holds
//! job-packet traffic until there is bandwidth or capability to
//! handle the packet. Everything about a station is registry data —
//! never a code path. Membership is DERIVED: the row's `predicate`
//! is evaluated over open Jobs at read time (see
//! [`crate::station_queue`]); there is no mutable current-station
//! field on the packet.
//!
//! Shape mirrors `StepPluginRegistry` / `WorkflowRegistry`:
//! append-only versioning + a status lifecycle (draft → active →
//! retired), a partial unique index enforcing at most one active row
//! per name, and every write recording its event atomically with the
//! row (the workflow-registry-events posture).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::WorkflowStatus;
use crate::station_queue::{DisciplineKey, StationPredicate, default_discipline};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The ratified station taxonomy (stations.md, and since 2026-11-03
/// the factory taxonomy's M2, docs/design/factory-taxonomy.md): all
/// rows, one registry. The variants are the spec's own closed
/// vocabulary — which flavors of queue exist — not a tenant-extensible
/// taxonomy (those go through the Class registry).
///
/// TWO FAMILIES. `actor`, `group` and `constraint` are the taxonomy's
/// GENERIC station, sorted by who serves it; `equipment` is a station
/// that is shared process equipment, operated by an automaton, whose
/// capacity is its `bays`. The fourth kind, `batch`, was DELETED (item 0a78f8f8,
/// design 4348b857 Q1, decided 2026-11-02): a batch station is an
/// equipment station whose automaton's program is a cadence, so the
/// loading dock became equipment and the review queues that also said
/// `batch` were always a group's. A row still spelling `batch` is
/// refused at every door — the viability lint at every publish, the
/// stored column's CHECK and its parse — and survives only as
/// [`StationKind::RetiredBatch`], the log's own spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StationKind {
    /// Every executor has one — the personal queue (My Day rendered).
    Actor,
    /// Served by a set of actors: departments, teams.
    Group,
    /// Membership defined by capability predicates (skills,
    /// authority, sign-off rights), not an enumerated roster.
    Constraint,
    /// Shared process equipment (the gates, the dock): operated by ONE
    /// automaton the row names, shared by many protocols, and bounded
    /// by the `bays` the row declares — the non-fungible constraint on
    /// top of the pools. Both are required on this kind and refused on
    /// every other ([`crate::station_lint`]).
    Equipment,
    /// NOT A KIND A ROW MAY HAVE: the deleted `batch`, as the record
    /// spelled it before item 0a78f8f8, kept so the record still reads
    /// and still hashes. Every station publication of the dock, the
    /// review queues and repair before 2026-11-03 says `batch`, and so
    /// does every move-diagnosis receipt (its `inputs.stations`), whose
    /// verification re-serializes the receipt and compares its hash —
    /// read as any other kind it would re-serialize differently and
    /// fail, and the registered-agent-handoff-v2 model that produced
    /// those receipts is pinned byte for byte and may not change to
    /// translate it. So the spelling round-trips here and nowhere else:
    /// the viability lint refuses it at every publish (seed bundle,
    /// authoring door, `publish_declared`), the stored column's CHECK
    /// refuses it, and [`std::str::FromStr`] — the stored row's parse —
    /// does not know it.
    #[serde(rename = "batch")]
    RetiredBatch,
}

impl StationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Actor => "actor",
            Self::Group => "group",
            Self::Constraint => "constraint",
            Self::Equipment => "equipment",
            Self::RetiredBatch => "batch",
        }
    }
}

impl std::str::FromStr for StationKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "actor" => Ok(Self::Actor),
            "group" => Ok(Self::Group),
            "constraint" => Ok(Self::Constraint),
            "equipment" => Ok(Self::Equipment),
            other => Err(format!("unknown station kind: {other}")),
        }
    }
}

/// The equipment station whose bays ARE the gate capacity — `boss
/// gate`'s bound, the resident supervisor's, and the slots the yard
/// draws. Declared at `infra/platform/stations/gates.toml`.
///
/// ONE HOME FOR THE NUMBER (CLAUDE.md §9a, item 0a78f8f8). Until this
/// car the capacity was `gate_max_concurrent` on the train-conductor
/// delivery policy; that column is dropped and every reader takes the
/// gates row's `bays` instead.
pub const GATES_STATION: &str = "gates";

/// The gate capacity a reader uses when the gates row cannot be read —
/// a dark registry must not stop a gate, so `boss gate` and the yard
/// fall back here. It is a fallback, not a copy: the pin
/// `the_compiled_gate_bays_are_the_gates_rows` (boss-jobs,
/// tests/platform_stations_bundle.rs) holds it equal to `gates.toml`'s
/// `bays`, and it is the ONE compiled fallback — the yard's and the
/// CLI's were two constants held equal across crates until this car
/// (`2` from delivery policy v11, backlog 9a460e74: two bays delivered
/// 2.62 gates/h on the saturated Kingston gate drive, three occupied
/// throughout 2.38; `3` since gates v4, backlog a082371d, design
/// 121f44a1 D2: the Micron 7400 that replaced it left a gate CPU-bound).
pub const COMPILED_GATE_BAYS: i32 = 3;

/// Who may claim a packet FROM this station — role slugs, and since
/// design c87fb59b car 3 the rate-card models an agent must run.
/// Checked at the claim CAS when the claim names its station. Absent =
/// any actor may claim.
///
/// THE ROLES ARE WHATEVER THE STEP'S SELECTOR SPELLS, and that is two
/// vocabularies, not one (backlog 4b103f0f). A projected row copies
/// the step's `authority_role`, which may be a PLATFORM role
/// (`platform-admin`, what a request asserts about itself) or a Class
/// code under `(employee, role)` (`engineering-agent`, what the roster
/// and the agents registry hold). The claim door therefore judges a
/// claimant over EVERY role spelling the record holds for it rather
/// than the one the request carries — see [`Self::admits_roles`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StationCapability {
    #[serde(default)]
    pub roles: Vec<String>,
    /// The models an AGENT claimant must run one of, as
    /// `agent_rate_card` spells them (`opus-5[1m]`). Written by the
    /// `(role, model)` projection (`station_projection::agent_stations`)
    /// so a station stands for "a `platform-admin`-role agent on
    /// opus-5[1m]"; an agents-registry row serves the station when its
    /// `default_model` is one of these. Empty gates no agent out, and
    /// a person — who runs no model — is gated by `roles` alone.
    /// Skipped on the wire when empty so every roles-only row the
    /// registry already holds reads back unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl StationCapability {
    /// Whether an actor holding `role` may claim from this station.
    /// An empty roles list gates nobody out (a declared-but-empty
    /// capability is a vacuous constraint, not a lockout).
    pub fn allows_role(&self, role: &str) -> bool {
        self.admits_roles(&[role])
    }

    /// Whether an actor may claim, judged over EVERY role the record
    /// holds for it: the one its request asserts, plus — for a
    /// registered agent — the Class code its `agents` row declares.
    ///
    /// ONE DECISION, TWO VOCABULARIES (backlog 4b103f0f). The roster
    /// the dispatcher nominates from reads `agents.role`, and a step
    /// addressed `audience = { role = X }` resolves to every HOLDER of
    /// X, agents included (migration 20260918022311). The claim door
    /// used to compare only `user.role`, which every named CLI caller
    /// carries as `platform-admin` — so an agent routed by the role it
    /// HOLDS was refused at the door by the role it ASSERTS, and the
    /// registry's `role` was read by nothing at the claim. Asking over
    /// both spellings is what reconciles them; an empty roles list
    /// still gates nobody out (a declared-but-empty capability is a
    /// vacuous constraint, not a lockout).
    pub fn admits_roles(&self, roles: &[&str]) -> bool {
        self.roles.is_empty()
            || roles
                .iter()
                .any(|held| self.roles.iter().any(|r| r == held))
    }

    /// Whether an actor that runs `models` (an agent's `default_model`;
    /// empty for a person) may claim from this station. Empty on
    /// either side gates nobody out: no models declared is a roles-only
    /// station, and no models run is not an agent.
    pub fn allows_model(&self, models: &[String]) -> bool {
        self.models.is_empty()
            || models.is_empty()
            || models.iter().any(|m| self.models.contains(m))
    }
}

/// The page context a surface needs to render this station whole.
///
/// The row declares it, the queue envelope echoes it, and the lens
/// renders whatever the row says with no frontend change — a page's
/// header and which supplementary panels it carries — so a surface
/// that already holds the queue holds everything it needs to draw
/// itself.
///
/// What it deliberately is NOT: the panels' *data*. A panel key names
/// a renderer the surface already has (the `step_plugins` idiom: the
/// row names the bundle, the bundle knows its own source). Putting
/// fetch URLs in the registry would let a row point a browser
/// anywhere, and putting the panel *contents* here would make
/// `boss-jobs` a client of every service a page reads from. A panel
/// fetches its own data; the station registry carries the queue and
/// how it is framed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationLens {
    /// Small type above the title (`System Model · Design review`).
    /// Optional: not every page sits under a section.
    #[serde(default)]
    pub eyebrow: Option<String>,
    /// The page's heading. The one required field — a lens with no
    /// title is a page with no name, and falling back to the
    /// station's `title` would silently publish an operator-facing
    /// heading that was written as a registry label.
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Supplementary panels this page carries, in render order, named
    /// by renderer key. Plain strings for the same reason
    /// `DisciplineKey`s are: the surface renders the vocabulary the
    /// registry declares, and a key it does not know is skipped
    /// rather than crashing the page. Empty (the default) = the queue
    /// is the whole page.
    #[serde(default)]
    pub panels: Vec<String>,
    /// Whether the queue envelope should carry each member's STEPS.
    ///
    /// Off by default and deliberately opt-in per station: it costs one
    /// `list_steps` per member, and most lenses render a row from the
    /// Job alone. A lens that places packets at the stop they have
    /// reached cannot — "where is this one" is a fact about its steps,
    /// and without them the surface either guesses or degrades to a
    /// list, which is what it was trying to stop being.
    ///
    /// Declared here rather than inferred from the predicate because
    /// the predicate answers who is IN the queue; this answers what the
    /// reader needs in order to draw it.
    #[serde(default)]
    pub with_steps: bool,
}

/// Optional authored grid cell; coordinates are cells, never hand-set pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloorPlacement {
    pub row: u16,
    pub col: u16,
}

/// Whether a station stands for good or is made for one claim — the two
/// modes of the ONE station of the station tree (T2 of design 4b27a65a,
/// backlog 3f58c823; handoff §2). A STANDING station is a row an operator
/// declares; an AD HOC one is instantiated under a standing parent when a
/// step needs a workspace and released with it. Nothing instantiates ad
/// hoc stations yet — resolution is T6 (baac0b65) — so the platform
/// bundle declares only standing rows; the mode is on the row so the
/// tree read can say which is which the day both exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StationMode {
    Standing,
    AdHoc,
}

impl StationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::AdHoc => "ad-hoc",
        }
    }
}

impl std::str::FromStr for StationMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "standing" => Ok(Self::Standing),
            "ad-hoc" => Ok(Self::AdHoc),
            other => Err(format!("unknown station mode: {other}")),
        }
    }
}

/// A full station row. Serializes directly to the `stations` columns
/// with the same names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StationSpec {
    pub name: String,
    pub version: i32,
    pub status: WorkflowStatus,
    pub title: String,
    pub kind: StationKind,
    /// The automaton that operates an `equipment` station, as the actor
    /// id it signs with (`automation:gate-runner`) — it must parse as
    /// `ActorId::Automation`, the set an automaton executor is drawn
    /// from. Required on `equipment`, refused on every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automaton: Option<String>,
    /// How many packets an `equipment` station serves at once — its
    /// bays (at least 1). Required on `equipment`, refused on every
    /// other kind: a generic station's bandwidth is its capability and
    /// reservations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bays: Option<i32>,
    /// The queue-membership predicate over packets — see
    /// [`StationPredicate`] for the documented JSON shape.
    pub predicate: StationPredicate,
    /// Queue ordering as data; `priority, then age` default.
    #[serde(default = "default_discipline")]
    pub discipline: Vec<DisciplineKey>,
    /// How long a *terminal* packet stays in this station's queue,
    /// counted from `closed_on`. `None` (the default, and every
    /// holding/routing station) = in-flight packets only, since a
    /// station that routes traffic has nothing to say about traffic
    /// that already left.
    ///
    /// Declared per station rather than baked into the predicate
    /// because it is a *retention* rule, not a membership rule: the
    /// predicate says which packets are this station's, the window
    /// says how long a departed one lingers on the board. Keeping it
    /// off the predicate also keeps
    /// [`StationPredicate::matches`](crate::station_queue::StationPredicate::matches)
    /// clockless.
    #[serde(default)]
    pub terminal_window_days: Option<u32>,
    /// Optional capability gate at the claim CAS.
    #[serde(default)]
    pub capability: Option<StationCapability>,
    /// Visual rollup grouping (team/department) — view-level clutter
    /// control, not a data-model claim.
    #[serde(default)]
    pub rollup_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<FloorPlacement>,
    /// The stations a packet goes to NEXT from this one — its outbound
    /// routes, by station name (pass item 4 of design ccfe87ff, backlog
    /// 1bb8c479). Declared only where the tree shows a packet taking the
    /// route; the floor lays its stations out along them. Every name is
    /// a station some active row declares: the seed refuses any other
    /// ([`validate_station_edges`]) before it publishes a row, and the
    /// live publish door refuses it too ([`gate_publish`]), and no
    /// name appears twice: the viability lint refuses that at the seed's
    /// load and at every publish ([`crate::station_lint`]). Empty (the
    /// default) is skipped on the wire, so a row declaring none — every
    /// row before this field — serializes, hashes and reads as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<String>,
    /// THE STATION TREE (T2 of design 4b27a65a, backlog 3f58c823). Six
    /// fields, each optional and each skipped on the wire when absent, so
    /// a row declaring none — every row before this car, and every row a
    /// protocol projects — serializes, hashes and reads exactly as
    /// before, and a build that predates them reads a row that has them
    /// by skipping the keys (StationSpec denies no unknown field; the
    /// rollback pin in tests/station_tree.rs holds that).
    ///
    /// `id` is the station's stable identity: it does not change when the
    /// row is versioned, re-parented or renamed, and it is what `parent`
    /// names. A row with no `id` is outside the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The `id` of the station this one stands in. `None` on a tree row is
    /// the ROOT, and the platform seed refuses a tree with any number of
    /// roots but one ([`crate::station_tree::validate_station_tree`]). The
    /// station's ADDRESS is derived from this chain of parents and never
    /// stored (handoff §3 rule 9: code never names an address), so
    /// re-parenting a station is this one field and a version bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Standing or ad hoc ([`StationMode`]). Declared by every tree row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<StationMode>,
    /// How many claims the station seats at once. On an `equipment` row it
    /// is the same number as `bays`, and the viability lint refuses a
    /// difference: `bays` retires in a later car and this takes its place
    /// (additive only, `bundle_decision_20261106_1944Z` on 4b27a65a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<i32>,
    /// The software provisioned into this station, serving it and
    /// everything beneath it, by the automation id each signs with
    /// (`automation:gate-runner`). A machine never claims a step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<String>,
    /// What this station holds from its parent, per pool name — reserved
    /// capacity. A child's allocations, summed per pool, never exceed what
    /// its parent holds; the root holds every pool whole, so a child of the
    /// root is judged against the pool's own capacity (handoff §3 rule 2,
    /// [`crate::station_tree::validate_station_tree`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub allocation: BTreeMap<String, crate::resources::Quantity>,
    /// The line the station's card rail draws, as a Design token code
    /// (`it` draws `--map-line-it`): identity, never state. Declared on the
    /// root and the stations under it; every station below inherits the
    /// nearest one up its parents (design c4854fd4, backlog 85cb2103).
    /// Skipped on the wire when absent, like the tree fields above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    // No `upstream`: the walk upstream is the parent station's page,
    // derived from the tree (design deba3000), and the field, its
    // column and its envelope echo retired together (backlog
    // d2cb38af). A body or an older event payload that still carries
    // the key decodes, because this struct does not deny unknown
    // fields, and the key is never written back.
    /// Optional page context for a surface that renders this station
    /// whole. `None` = no page claims this station, which is the
    /// common case: most stations are queues read by a lens that has
    /// its own identity.
    #[serde(default)]
    pub lens: Option<StationLens>,
    pub created_at: DateTime<Utc>,
}

impl StationSpec {
    /// Convenience constructor for tests + seeds.
    ///
    /// `now` is the caller's clock reading, taken as a parameter for
    /// the same reason every write method on [`StationRegistry`] takes
    /// one: only the clock service reads wall time, so a row's
    /// `created_at` is stamped with whatever `now` the caller was
    /// handed — sim-time under a sim deploy, wall time otherwise. A
    /// constructor that called `Utc::now()` itself would stamp wall
    /// time regardless of clock-api mode (`infra/lint/no-wallclock.sh`).
    pub fn draft(
        name: impl Into<String>,
        title: impl Into<String>,
        kind: StationKind,
        predicate: StationPredicate,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            name: name.into(),
            version: 1,
            status: WorkflowStatus::Draft,
            title: title.into(),
            kind,
            automaton: None,
            bays: None,
            predicate,
            discipline: default_discipline(),
            terminal_window_days: None,
            capability: None,
            rollup_parent: None,
            floor: None,
            edges: Vec::new(),
            id: None,
            parent: None,
            mode: None,
            capacity: None,
            machines: Vec::new(),
            allocation: BTreeMap::new(),
            line: None,
            lens: None,
            created_at: now,
        }
    }

    /// This row with its predicate bound to `actor` — the row a read
    /// edge actually evaluates.
    ///
    /// `None` means the row declares a per-actor queue
    /// ([`crate::station_queue::SELF`]) and this caller is not an
    /// identified actor, so the station has no queue for them. The
    /// caller renders an empty queue; it must never fall back to the
    /// unbound row.
    pub fn bind_self(&self, actor: Option<&str>) -> Option<StationSpec> {
        Some(StationSpec {
            predicate: self.predicate.bind_self(actor)?,
            ..self.clone()
        })
    }
}

/// Every edge of `rows` naming a station no ACTIVE row of `among`
/// declares, one refusal each, naming the station that declares the edge
/// and the one it could not find (pass item 4 of design ccfe87ff, backlog
/// 1bb8c479). The ONE definition of the question, asked at both doors a
/// station row is published through: the seed on the tree's bundle
/// against itself (`rows == among`), before any publish — the same place
/// and the same rows a pool's station is judged against
/// ([`crate::pools::validate_pool_stations`]) — and the live authoring
/// door, on the promoted draft against the active rows
/// ([`gate_publish`]; until backlog 5b80b9f4 that door never asked it).
/// The tree-wide pin runs it on the platform bundle. An edge named twice
/// is refused per row by the viability lint (`station_lint::check_edges`).
pub fn validate_station_edges(
    rows: &[StationSpec],
    among: &[StationSpec],
) -> Vec<crate::station_lint::StationLintError> {
    rows.iter()
        .flat_map(|from| from.edges.iter().map(move |to| (from, to)))
        .filter(|(_, to)| {
            !among
                .iter()
                .any(|s| &s.name == *to && s.status == WorkflowStatus::Active)
        })
        .map(|(from, to)| crate::station_lint::StationLintError {
            station: from.name.clone(),
            field: "edges".into(),
            reason: format!("declares an edge to `{to}`, which no active station row declares"),
        })
        .collect()
}

/// Every equipment row of `rows` whose automaton is no REGISTERED
/// automation — no id of `registered`, the automations registry
/// (`automation_actors`, published from `infra/platform/automations/`) —
/// one refusal each, naming the station, the field and the automaton
/// (backlog d0cf2cdc). The automaton an equipment station names decides
/// which actor may operate its bays, and the viability lint checks only
/// that it is SPELLED as an automation id: `automation:anything` parses.
///
/// The ONE definition of the question, asked wherever a station row is
/// published: the live door and the dry run through [`gate_publish`],
/// the platform seed on the whole bundle before any publish, and the
/// tree-wide pin on the bundle against the automations bundle. An
/// automaton the lint already refuses (not spelled as an automation id)
/// is that lint's to name and is not named twice here.
pub fn validate_station_automatons(
    rows: &[StationSpec],
    registered: &[String],
) -> Vec<crate::station_lint::StationLintError> {
    rows.iter()
        .filter(|s| s.kind == StationKind::Equipment)
        .filter_map(|s| s.automaton.as_deref().map(|a| (s, a)))
        .filter(|(_, a)| crate::station_lint::is_automation_id(a))
        .filter(|(_, a)| !registered.iter().any(|r| r == a))
        .map(|(s, a)| crate::station_lint::StationLintError {
            station: s.name.clone(),
            field: "automaton".into(),
            reason: format!(
                "automaton `{a}` is not a registered automation: no row of the \
                 automations registry (infra/platform/automations/) holds that id, so \
                 nothing operates this station's bays. Register the automation first, \
                 or name one that is."
            ),
        })
        .collect()
}

/// The publish door's viability gate: `spec`'s own lint
/// ([`crate::station_lint::gate_active`]), its edges judged against
/// `active`, the registry's active rows, with `spec` taking its name's
/// place among them as the promotion would leave it, and its automaton
/// judged against `registered`, the automation ids the automations
/// registry holds ([`validate_station_automatons`]). Both adapters'
/// `publish` run it on the draft their write actually promotes, and the
/// dry run (`POST /api/stations/_validate`) runs it on the spec it is
/// handed, so a clean dry run publishes and a refusal reads the same.
///
/// Only `spec`'s OWN edges are judged: another active row's broken edge
/// (to a station retired since) is not this publish's to answer for.
/// Backlog 5b80b9f4: the door ran only the row-alone lint, so a row
/// naming an edge to no active station published by hand at /system
/// while the seed refused the same row. Backlog d0cf2cdc: the same for
/// an automaton no registered automation answers to.
///
/// `registered` need hold only the ids `spec` names — an adapter reads
/// the one row ([`StationRegistry::registered_automatons`]).
pub fn gate_publish(
    spec: &StationSpec,
    active: &[StationSpec],
    registered: &[String],
) -> Result<(), Vec<crate::station_lint::StationLintError>> {
    let promoted = StationSpec {
        status: WorkflowStatus::Active,
        ..spec.clone()
    };
    let after: Vec<StationSpec> = active
        .iter()
        .filter(|s| s.name != spec.name)
        .cloned()
        .chain(std::iter::once(promoted))
        .collect();
    let problems: Vec<_> = crate::station_lint::gate_active(spec)
        .err()
        .unwrap_or_default()
        .into_iter()
        .chain(validate_station_edges(std::slice::from_ref(spec), &after))
        .chain(validate_station_automatons(
            std::slice::from_ref(spec),
            registered,
        ))
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StationError {
    #[error("station not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
    /// The spec parsed and is well-formed — it just describes a queue
    /// that cannot behave as declared. Distinct from `Invalid` because
    /// the caller gets a problem *list* to render, not a message, and
    /// because it leaves as 422 rather than 400. See
    /// [`crate::station_lint`].
    #[error("station is not viable: {} problem(s)", .0.len())]
    Unviable(Vec<crate::station_lint::StationLintError>),
    #[error("storage error: {0}")]
    Storage(String),
}

/// `terminal_window_days` as the `stations.terminal_window_days` INT
/// column holds it, or `Invalid` naming the station when it cannot.
///
/// The row carries a `u32` and Postgres has no unsigned INT, so a
/// window past `i32::MAX` days has no stored spelling. The Pg adapter
/// used to clamp it to `i32::MAX` on the way in: the write answered the
/// window it was handed, recorded that window on its fact, and stored a
/// different one, while the double stored it as handed — so the row,
/// the fact and the other adapter each said something else (backlog
/// be459ab9, found by the adapters-agree suite on 2026-09-29). Both
/// adapters now refuse it before writing anything, through this one
/// check.
pub(crate) fn window_column(spec: &StationSpec) -> Result<Option<i32>, StationError> {
    spec.terminal_window_days
        .map(|days| {
            i32::try_from(days).map_err(|_| {
                StationError::Invalid(format!(
                    "station {}: terminal_window_days {days} exceeds the widest window \
                     a row can hold ({} days)",
                    spec.name,
                    i32::MAX
                ))
            })
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

pub trait StationSelectionRows {
    fn rows(&self) -> &[StationSpec];
}

struct LockedStationSelectionRows<'a> {
    _guard: std::sync::MutexGuard<'a, HashMap<(String, i32), StationSpec>>,
    active: Vec<StationSpec>,
}

impl StationSelectionRows for LockedStationSelectionRows<'_> {
    fn rows(&self) -> &[StationSpec] {
        &self.active
    }
}

#[async_trait]
pub trait StationRegistry: Send + Sync {
    /// The automation ids `spec` names that the automations registry
    /// holds — empty when it names none, or names one no row answers
    /// to. The registered half of [`gate_publish`], which the dry run
    /// reads here and `publish` reads inside its own write (backlog
    /// d0cf2cdc). An adapter with no registry to read refuses a spec
    /// that names an automaton rather than answering empty, which would
    /// read as "unregistered" for a registry it never asked.
    async fn registered_automatons(&self, spec: &StationSpec) -> Result<Vec<String>, StationError> {
        if spec.automaton.is_some() {
            return Err(StationError::Storage(
                "automation registry unavailable".into(),
            ));
        }
        Ok(Vec::new())
    }
    fn try_lock_active_selection_rows(
        &self,
    ) -> Result<Option<Box<dyn StationSelectionRows + '_>>, crate::port::JobsError> {
        Err(crate::port::JobsError::Storage(
            "station adapter has no nonblocking selection fence".into(),
        ))
    }
    /// Acquire the actual local writer guard before any selection effects.
    fn lock_active_selection_rows(
        &self,
    ) -> Result<Box<dyn StationSelectionRows + '_>, crate::port::JobsError> {
        Err(crate::port::JobsError::Storage(
            "station adapter has no local selection fence".into(),
        ))
    }
    /// Currently-active spec for `name`.
    async fn get_active(&self, name: &str) -> Result<StationSpec, StationError>;

    /// Specific historical version.
    async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError>;

    /// Every active station, name-ordered byte for byte (`-` before a
    /// letter) — never by a database's locale.
    async fn list_active(&self) -> Result<Vec<StationSpec>, StationError>;

    /// Every version of one name (oldest first). Includes drafts +
    /// retired.
    async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError>;

    /// Every name this registry has ever held a row of, any status,
    /// sorted byte for byte — the recovery export's walk
    /// (`crate::registry_export`, backlog c26aacaa). Refuses rather
    /// than answering a short list when the adapter cannot enumerate.
    async fn lineage_names(&self) -> Result<Vec<String>, StationError> {
        Err(StationError::Storage(
            "station adapter cannot enumerate its lineages".into(),
        ))
    }

    /// Append a new draft row. Version = max(version)+1 or 1.
    ///
    /// Every write method takes `actor` + `now` — a station edit is a
    /// network configuration change, so the adapter builds the
    /// corresponding event via `events::station_registry_event` and
    /// records it atomically with the row. Records
    /// `jobs.station.draft_saved` (payload = the stored draft).
    ///
    /// `Invalid`, writing nothing, when the row cannot be stored as
    /// written (a `terminal_window_days` past `i32::MAX`); `publish_declared`
    /// refuses the same.
    async fn create_draft(
        &self,
        spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError>;

    /// Flip the latest draft to active and demote the previous
    /// active. Transactional; records `jobs.station.published`
    /// (payload = the promoted spec) with the row flips.
    async fn publish(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError>;

    /// Flip the active row to retired. Idempotent if already retired
    /// — and silent then too: only a write that touched a row records
    /// `jobs.station.retired` (payload = the retired spec).
    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), StationError>;

    /// Publish `spec` at ITS OWN declared version — the platform
    /// bundle's write (`crate::station_seed`, backlog 393d3234).
    ///
    /// `create_draft` + `publish` assign `max(version) + 1`, which is
    /// right for an author and wrong for a bundle: a bundle row
    /// carries its version (a version bump is the edit path), and a
    /// fresh deployment must land `loading-dock` at v3 — the version
    /// the migrations produced and every in-flight reference names —
    /// not at v1 with two versions of history nobody wrote.
    ///
    /// In one transaction: the viability gate, retire any active row
    /// of the same name, INSERT the row active at `spec.version` with
    /// `created_at = now`, record `jobs.station.published` (payload =
    /// the row written). `Conflict`, writing nothing, when
    /// `spec.version` is not above the newest version the name holds in
    /// any status — an existing row is never overwritten, and a live
    /// row is never retired for a lower one; the caller's classify is a
    /// read taken before this write, so the floor lives here (backlog
    /// df793bd7, `crate::declared_version`).
    async fn publish_declared(
        &self,
        spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError>;
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

pub struct InMemoryStations {
    rows: Arc<Mutex<HashMap<(String, i32), StationSpec>>>,
    /// The automations registry an equipment station's automaton is
    /// judged against at publish (backlog d0cf2cdc) — the double's
    /// stand-in for the `automation_actors` table the Pg adapter reads.
    automations: Option<Arc<dyn crate::agents::AgentsRegistry>>,
    /// What the Pg adapter records into `event_outbox` inside the
    /// row transaction, this adapter collects here — same events at
    /// the same write points, so tests assert the event contract
    /// through the port without a database.
    recorded: Arc<Mutex<Vec<boss_core::event::Event>>>,
}

impl Default for InMemoryStations {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStations {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
            automations: None,
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_automations(mut self, automations: Arc<dyn crate::agents::AgentsRegistry>) -> Self {
        self.automations = Some(automations);
        self
    }

    /// Every event a write method recorded, in write order — the
    /// in-memory stand-in for `SELECT ... FROM event_outbox`.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().unwrap().clone()
    }

    fn record(&self, event: boss_core::event::Event) {
        self.recorded.lock().unwrap().push(event);
    }

    /// Seed helper for tests. Inserts as-is without versioning logic.
    pub fn seed(&self, spec: StationSpec) -> Result<(), StationError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.name.clone(), spec.version)) {
            return Err(StationError::Conflict(format!(
                "row already exists: {}@{}",
                spec.name, spec.version
            )));
        }
        rows.insert((spec.name.clone(), spec.version), spec);
        Ok(())
    }

    /// Every automation id the wired registry holds, or why it cannot be
    /// read — never an empty list standing in for a registry not wired.
    async fn automation_ids(&self) -> Result<Vec<String>, StationError> {
        let registry = self
            .automations
            .as_ref()
            .ok_or_else(|| StationError::Storage("automation registry unavailable".into()))?;
        let rows = registry
            .list_automations()
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(|a| a.id).collect())
    }

    fn snapshot(&self) -> Vec<StationSpec> {
        self.rows.lock().unwrap().values().cloned().collect()
    }

    fn max_version(&self, name: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl StationRegistry for InMemoryStations {
    async fn registered_automatons(&self, spec: &StationSpec) -> Result<Vec<String>, StationError> {
        let Some(automaton) = spec.automaton.as_deref() else {
            return Ok(Vec::new());
        };
        Ok(self
            .automation_ids()
            .await?
            .into_iter()
            .filter(|id| id == automaton)
            .collect())
    }
    fn try_lock_active_selection_rows(
        &self,
    ) -> Result<Option<Box<dyn StationSelectionRows + '_>>, crate::port::JobsError> {
        let guard = match self.rows.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(crate::port::JobsError::Storage(
                    "station selection lock poisoned".into(),
                ));
            }
        };
        let mut active: Vec<_> = guard
            .values()
            .filter(|row| row.status == WorkflowStatus::Active)
            .cloned()
            .collect();
        active.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Some(Box::new(LockedStationSelectionRows {
            _guard: guard,
            active,
        })))
    }
    fn lock_active_selection_rows(
        &self,
    ) -> Result<Box<dyn StationSelectionRows + '_>, crate::port::JobsError> {
        let guard = self.rows.lock().map_err(|_| {
            crate::port::JobsError::Storage("station selection lock poisoned".into())
        })?;
        let mut active: Vec<_> = guard
            .values()
            .filter(|row| row.status == WorkflowStatus::Active)
            .cloned()
            .collect();
        active.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Box::new(LockedStationSelectionRows {
            _guard: guard,
            active,
        }))
    }
    async fn get_active(&self, name: &str) -> Result<StationSpec, StationError> {
        self.snapshot()
            .into_iter()
            .find(|r| r.name == name && r.status == WorkflowStatus::Active)
            .ok_or_else(|| StationError::NotFound(format!("no active station: {name}")))
    }

    async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError> {
        self.rows
            .lock()
            .unwrap()
            .get(&(name.to_string(), version))
            .cloned()
            .ok_or_else(|| StationError::NotFound(format!("{name}@v{version}")))
    }

    async fn list_active(&self) -> Result<Vec<StationSpec>, StationError> {
        let mut rows: Vec<StationSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == WorkflowStatus::Active)
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError> {
        let mut rows: Vec<StationSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.name == name)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn lineage_names(&self) -> Result<Vec<String>, StationError> {
        let names: std::collections::BTreeSet<String> =
            self.snapshot().into_iter().map(|r| r.name).collect();
        Ok(names.into_iter().collect())
    }

    async fn create_draft(
        &self,
        mut spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        window_column(&spec)?;
        let next = self.max_version(&spec.name).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = WorkflowStatus::Draft;
        spec.created_at = now;
        self.rows
            .lock()
            .unwrap()
            .insert((spec.name.clone(), spec.version), spec.clone());
        self.record(crate::events::station_registry_event(
            crate::events::STATION_DRAFT_SAVED,
            actor,
            &spec,
        ));
        Ok(spec)
    }

    async fn publish(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        // Read the automations registry before taking the station writer
        // mutex; judge the actual promoted draft below (backlog d0cf2cdc).
        let automations = self.automation_ids().await;
        let mut rows = self.rows.lock().unwrap();
        let latest_draft = rows
            .values()
            .filter(|r| r.name == name && r.status == WorkflowStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                StationError::NotFound(format!("no draft to publish for station: {name}"))
            })?;

        // The viability gate, against the row this call actually
        // promotes — not a copy re-read by the caller, which could
        // race a concurrent author. Same placement as the Workflow
        // registry's, for the same reason. Its edges are judged against
        // the active rows under the same lock (backlog 5b80b9f4).
        let active: Vec<StationSpec> = rows
            .values()
            .filter(|r| r.status == WorkflowStatus::Active)
            .cloned()
            .collect();
        let registered = if latest_draft.automaton.is_some() {
            automations?
        } else {
            Vec::new()
        };
        gate_publish(&latest_draft, &active, &registered).map_err(StationError::Unviable)?;

        for ((n, _), row) in rows.iter_mut() {
            if n == name && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        let key = (latest_draft.name.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = WorkflowStatus::Active;
        let promoted = row.clone();
        drop(rows);
        self.record(crate::events::station_registry_event(
            crate::events::STATION_PUBLISHED,
            actor,
            &promoted,
        ));
        Ok(promoted)
    }

    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<(), StationError> {
        let mut rows = self.rows.lock().unwrap();
        let any_active = rows
            .values()
            .any(|r| r.name == name && r.status == WorkflowStatus::Active);
        if !any_active {
            // Idempotent — nothing to do, so nothing to record.
            return Ok(());
        }
        let mut retired: Option<StationSpec> = None;
        for ((n, _), row) in rows.iter_mut() {
            if n == name && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
                retired = Some(row.clone());
            }
        }
        drop(rows);
        if let Some(spec) = retired {
            self.record(crate::events::station_registry_event(
                crate::events::STATION_RETIRED,
                actor,
                &spec,
            ));
        }
        Ok(())
    }

    async fn publish_declared(
        &self,
        mut spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        window_column(&spec)?;
        crate::station_lint::gate_active(&spec).map_err(StationError::Unviable)?;
        let mut rows = self.rows.lock().unwrap();
        let key = (spec.name.clone(), spec.version);
        // The floor, under the one lock every write takes — the Pg
        // adapter's rule (backlog df793bd7).
        let newest = crate::declared_version::newest(
            rows.keys()
                .filter(|(n, _)| *n == spec.name)
                .map(|(_, v)| *v),
        );
        if spec.version <= newest {
            return Err(StationError::Conflict(crate::declared_version::not_above(
                &spec.name,
                spec.version,
                newest,
            )));
        }
        for ((n, _), row) in rows.iter_mut() {
            if *n == spec.name && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        rows.insert(key, spec.clone());
        drop(rows);
        self.record(crate::events::station_registry_event(
            crate::events::STATION_PUBLISHED,
            actor,
            &spec,
        ));
        Ok(spec)
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    /// The automation id `spec` names, if a row of `automation_actors`
    /// holds it — read FOR SHARE inside the publish,
    /// so the row it was judged against stands until the flip commits
    /// (backlog d0cf2cdc).
    async fn registered_automatons_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        spec: &StationSpec,
    ) -> Result<Vec<String>, StationError> {
        let Some(automaton) = &spec.automaton else {
            return Ok(Vec::new());
        };
        let held: Option<String> =
            sqlx::query_scalar("SELECT id FROM automation_actors WHERE id = $1 FOR SHARE")
                .bind(automaton)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
        Ok(held.into_iter().collect())
    }

    pub struct PgStations {
        pool: PgPool,
    }

    pub(crate) async fn active_rows_in<'e, E>(exec: E) -> Result<Vec<StationSpec>, StationError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        // BYTE order, `COLLATE "C"`: the port promises "name-ordered",
        // and a bare `ORDER BY name` sorts by the database's locale,
        // which ignores `-` at first level — `suite-ab` listed before
        // `suite-a-z` here and after it in the double. Byte order is
        // the one both adapters can hold (backlog be459ab9, found by
        // the adapters-agree suite; the Workflow and credentials
        // registries were fixed the same way).
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "{SELECT} WHERE status = 'active' ORDER BY name COLLATE \"C\""
        ))
        .fetch_all(exec)
        .await
        .map_err(|e| StationError::Storage(e.to_string()))?;
        rows.into_iter().map(row_to_spec).collect()
    }

    impl PgStations {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        name: String,
        version: i32,
        status: String,
        title: String,
        kind: String,
        automaton: Option<String>,
        bays: Option<i32>,
        predicate: serde_json::Value,
        discipline: serde_json::Value,
        terminal_window_days: Option<i32>,
        capability: Option<serde_json::Value>,
        rollup_parent: Option<String>,
        floor: Option<serde_json::Value>,
        edges: Vec<String>,
        id: Option<String>,
        parent: Option<String>,
        mode: Option<String>,
        capacity: Option<i32>,
        machines: Vec<String>,
        allocation: serde_json::Value,
        line: Option<String>,
        lens: Option<serde_json::Value>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<StationSpec, StationError> {
        let status = r
            .status
            .parse::<WorkflowStatus>()
            .map_err(StationError::Storage)?;
        let kind = r
            .kind
            .parse::<StationKind>()
            .map_err(StationError::Storage)?;
        let predicate: StationPredicate = serde_json::from_value(r.predicate)
            .map_err(|e| StationError::Storage(format!("stations.predicate: {e}")))?;
        let discipline: Vec<DisciplineKey> = serde_json::from_value(r.discipline)
            .map_err(|e| StationError::Storage(format!("stations.discipline: {e}")))?;
        let capability: Option<StationCapability> = r
            .capability
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| StationError::Storage(format!("stations.capability: {e}")))?;
        let lens: Option<StationLens> = r
            .lens
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| StationError::Storage(format!("stations.lens: {e}")))?;
        Ok(StationSpec {
            name: r.name,
            version: r.version,
            status,
            title: r.title,
            kind,
            automaton: r.automaton,
            bays: r.bays,
            predicate,
            discipline,
            // The column is INT (Postgres has no unsigned); a negative
            // value would be a nonsense window, so it reads as "no
            // window" rather than panicking a cast.
            terminal_window_days: r.terminal_window_days.and_then(|d| u32::try_from(d).ok()),
            capability,
            rollup_parent: r.rollup_parent,
            floor: r
                .floor
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| StationError::Storage(format!("stations.floor: {e}")))?,
            edges: r.edges,
            id: r.id,
            parent: r.parent,
            mode: r
                .mode
                .map(|m| m.parse::<StationMode>())
                .transpose()
                .map_err(StationError::Storage)?,
            capacity: r.capacity,
            machines: r.machines,
            allocation: serde_json::from_value(r.allocation)
                .map_err(|e| StationError::Storage(format!("stations.allocation: {e}")))?,
            line: r.line,
            lens,
            created_at: r.created_at,
        })
    }

    const SELECT: &str = "SELECT name, version, status, title, kind, predicate, discipline, \
                          terminal_window_days, capability, rollup_parent, \
                          lens, created_at, floor, automaton, bays, \
                          edges, id, parent, mode, capacity, machines, allocation, line \
                          FROM stations";

    /// `allocation` as its JSONB column holds it: an object of pool name to
    /// exact decimal string, `{}` for a row that allocates nothing (T2 of
    /// design 4b27a65a, backlog 3f58c823).
    fn allocation_column(spec: &StationSpec) -> serde_json::Value {
        serde_json::to_value(&spec.allocation).unwrap_or_default()
    }

    #[async_trait]
    impl StationRegistry for PgStations {
        async fn registered_automatons(
            &self,
            spec: &StationSpec,
        ) -> Result<Vec<String>, StationError> {
            if spec.automaton.is_none() {
                return Ok(Vec::new());
            }
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            registered_automatons_in(&mut tx, spec).await
        }
        async fn get_active(&self, name: &str) -> Result<StationSpec, StationError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND status = 'active'"))
                    .bind(name)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("no active station: {name}")))
        }

        async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND version = $2"))
                    .bind(name)
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("{name}@v{version}")))
        }

        async fn list_active(&self) -> Result<Vec<StationSpec>, StationError> {
            active_rows_in(&self.pool).await
        }

        async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError> {
            let rows: Vec<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 ORDER BY version"))
                    .bind(name)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn lineage_names(&self) -> Result<Vec<String>, StationError> {
            sqlx::query_scalar(
                "SELECT name FROM stations GROUP BY name ORDER BY name COLLATE \"C\"",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))
        }

        async fn create_draft(
            &self,
            mut spec: StationSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StationSpec, StationError> {
            let window = window_column(&spec)?;
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM stations WHERE name = $1")
                    .bind(&spec.name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            spec.version = max.0.map(|v| v + 1).unwrap_or(1);
            spec.status = WorkflowStatus::Draft;
            spec.created_at = now;

            sqlx::query(
                "INSERT INTO stations
                    (name, version, status, title, kind, predicate, discipline,
                     terminal_window_days, capability, rollup_parent,
                     lens, created_at, floor, automaton, bays, edges,
                     id, parent, mode, capacity, machines, allocation, line)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                         $15, $16, $17, $18, $19, $20, $21, $22)",
            )
            .bind(&spec.name)
            .bind(spec.version)
            .bind(&spec.title)
            .bind(spec.kind.as_str())
            .bind(serde_json::to_value(&spec.predicate).unwrap_or_default())
            .bind(serde_json::to_value(&spec.discipline).unwrap_or_default())
            .bind(window)
            .bind(
                spec.capability
                    .as_ref()
                    .map(|c| serde_json::to_value(c).unwrap_or_default()),
            )
            .bind(&spec.rollup_parent)
            .bind(
                spec.lens
                    .as_ref()
                    .map(|l| serde_json::to_value(l).unwrap_or_default()),
            )
            .bind(spec.created_at)
            .bind(
                spec.floor
                    .as_ref()
                    .map(|f| serde_json::json!({"row": f.row, "col": f.col})),
            )
            .bind(&spec.automaton)
            .bind(spec.bays)
            .bind(&spec.edges)
            .bind(&spec.id)
            .bind(&spec.parent)
            .bind(spec.mode.map(|m| m.as_str()))
            .bind(spec.capacity)
            .bind(&spec.machines)
            .bind(allocation_column(&spec))
            .bind(&spec.line)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            let event = crate::events::station_registry_event(
                crate::events::STATION_DRAFT_SAVED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<StationSpec, StationError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            // Read the full draft row inside the tx — the published
            // event's payload is the promoted spec, and it must
            // describe the row THIS transaction flips (a post-commit
            // re-fetch could race a concurrent writer and record a
            // spec the flip never produced).
            let draft: Option<Row> = sqlx::query_as(&format!(
                "{SELECT} WHERE name = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1"
            ))
            .bind(name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            let mut promoted = draft
                .map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("no draft to publish: {name}")))?;

            // The viability gate, inside the transaction, against the
            // row the flip below actually promotes. `?` here rolls the
            // tx back untouched — an unviable draft never occupies the
            // ACTIVE slot even for the length of a transaction. Its
            // edges are judged against the active rows this transaction
            // reads (backlog 5b80b9f4), and its automaton against the
            // automations registry the same transaction reads (backlog
            // d0cf2cdc).
            let active = active_rows_in(&mut *tx).await?;
            let registered = registered_automatons_in(&mut tx, &promoted).await?;
            gate_publish(&promoted, &active, &registered).map_err(StationError::Unviable)?;

            sqlx::query(
                "UPDATE stations SET status = 'retired'
                 WHERE name = $1 AND status = 'active'",
            )
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            sqlx::query(
                "UPDATE stations SET status = 'active'
                 WHERE name = $1 AND version = $2",
            )
            .bind(name)
            .bind(promoted.version)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            promoted.status = WorkflowStatus::Active;

            let event = crate::events::station_registry_event(
                crate::events::STATION_PUBLISHED,
                actor,
                &promoted,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            Ok(promoted)
        }

        async fn retire(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            _now: DateTime<Utc>,
        ) -> Result<(), StationError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            // Read the active row first — the event payload is the
            // retired spec, and the idempotent no-op path (nothing
            // active) must record nothing. At most one row can be
            // active per name (partial unique index).
            let active: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND status = 'active'"))
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;

            let Some(row) = active else {
                // Idempotent — nothing to do, so nothing to record.
                return Ok(());
            };
            let mut spec = row_to_spec(row)?;

            sqlx::query(
                "UPDATE stations SET status = 'retired'
                 WHERE name = $1 AND status = 'active'",
            )
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            spec.status = WorkflowStatus::Retired;

            let event =
                crate::events::station_registry_event(crate::events::STATION_RETIRED, actor, &spec);
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            Ok(())
        }

        async fn publish_declared(
            &self,
            mut spec: StationSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StationSpec, StationError> {
            // The viability gate before the transaction opens: an
            // unviable bundle row never occupies the ACTIVE slot, not
            // even for the length of a transaction.
            let window = window_column(&spec)?;
            crate::station_lint::gate_active(&spec).map_err(StationError::Unviable)?;

            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            // THE FLOOR, inside the write (backlog df793bd7): the
            // newest version of the name, any status, read after every
            // other declared write of it has committed or waits behind
            // this one (`crate::declared_version`).
            let newest = crate::declared_version::lock_and_read_newest(
                &mut tx, "stations", "name", &spec.name,
            )
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            if spec.version <= newest {
                return Err(StationError::Conflict(crate::declared_version::not_above(
                    &spec.name,
                    spec.version,
                    newest,
                )));
            }

            // RETIRE FIRST, THEN INSERT: `stations_one_active_per_name`
            // is a plain partial unique index, enforced per statement,
            // so the order is load-bearing (130 and 133 each reddened
            // a train by getting it the other way round).
            sqlx::query(
                "UPDATE stations SET status = 'retired'
                 WHERE name = $1 AND status = 'active'",
            )
            .bind(&spec.name)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            spec.status = WorkflowStatus::Active;
            spec.created_at = now;
            sqlx::query(
                "INSERT INTO stations
                    (name, version, status, title, kind, predicate, discipline,
                     terminal_window_days, capability, rollup_parent,
                     lens, created_at, floor, automaton, bays, edges,
                     id, parent, mode, capacity, machines, allocation, line)
                 VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                         $15, $16, $17, $18, $19, $20, $21, $22)",
            )
            .bind(&spec.name)
            .bind(spec.version)
            .bind(&spec.title)
            .bind(spec.kind.as_str())
            .bind(serde_json::to_value(&spec.predicate).unwrap_or_default())
            .bind(serde_json::to_value(&spec.discipline).unwrap_or_default())
            .bind(window)
            .bind(
                spec.capability
                    .as_ref()
                    .map(|c| serde_json::to_value(c).unwrap_or_default()),
            )
            .bind(&spec.rollup_parent)
            .bind(
                spec.lens
                    .as_ref()
                    .map(|l| serde_json::to_value(l).unwrap_or_default()),
            )
            .bind(spec.created_at)
            .bind(
                spec.floor
                    .as_ref()
                    .map(|f| serde_json::json!({"row": f.row, "col": f.col})),
            )
            .bind(&spec.automaton)
            .bind(spec.bays)
            .bind(&spec.edges)
            .bind(&spec.id)
            .bind(&spec.parent)
            .bind(spec.mode.map(|m| m.as_str()))
            .bind(spec.capacity)
            .bind(&spec.machines)
            .bind(allocation_column(&spec))
            .bind(&spec.line)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            let event = crate::events::station_registry_event(
                crate::events::STATION_PUBLISHED,
                actor,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            Ok(spec)
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgStations;
#[cfg(feature = "postgres")]
pub(crate) use pg::active_rows_in as active_station_rows_in;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> StationSpec {
        StationSpec::draft(
            name,
            format!("Test {name}"),
            StationKind::Group,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
            Utc::now(),
        )
    }

    /// The `batch` kind is gone from the stored row's parse (item
    /// 0a78f8f8 — the lint refuses it at every publish, pinned in
    /// station_lint), and an equipment row reads back whole with the two
    /// columns that make one.
    #[test]
    fn batch_is_no_station_kind_and_equipment_is_one() {
        assert!("batch".parse::<StationKind>().is_err());
        assert_eq!(
            "equipment".parse::<StationKind>(),
            Ok(StationKind::Equipment)
        );
        let mut row = serde_json::to_value(sample("dock")).unwrap();
        row["kind"] = serde_json::json!("equipment");
        row["automaton"] = serde_json::json!("automation:train-conductor");
        row["bays"] = serde_json::json!(1);
        let equipment: StationSpec = serde_json::from_value(row).unwrap();
        assert_eq!(equipment.kind, StationKind::Equipment);
        assert_eq!(
            equipment.automaton.as_deref(),
            Some("automation:train-conductor")
        );
        assert_eq!(equipment.bays, Some(1));
        let generic = serde_json::to_value(sample("generic")).unwrap();
        assert!(
            generic.get("automaton").is_none() && generic.get("bays").is_none(),
            "a generic row's wire shape is unchanged: {generic}"
        );
    }

    /// The record keeps what it recorded, byte for byte. A publication
    /// or a move-diagnosis receipt from before the kind was deleted
    /// reads, and re-serializes to the same canonical bytes — so the
    /// receipt's hash still verifies — as `RetiredBatch`, spelled
    /// `batch` and nothing else.
    #[test]
    fn a_recorded_batch_station_round_trips_byte_for_byte() {
        let mut payload = serde_json::to_value(sample("loading-dock")).unwrap();
        payload["kind"] = serde_json::json!("batch");
        let read: StationSpec = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(read.kind, StationKind::RetiredBatch);
        assert_eq!(read.kind.as_str(), "batch");
        let again = serde_json::to_value(&read).unwrap();
        assert_eq!(
            boss_core::job::canonical_json_bytes(&again),
            boss_core::job::canonical_json_bytes(&payload),
            "a recorded batch station re-serializes to the bytes it was hashed as"
        );
    }

    /// Shared write-path actor: every registry write records an
    /// event, and the event needs a who. Tests are exempt from the
    /// no-wallclock lint, so `Utc::now()` rides along at call sites.
    fn test_actor() -> boss_core::actor::ActorId {
        boss_core::actor::ActorId::Human("emp-test".into())
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryStations::new();
        let v1 = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, WorkflowStatus::Draft);
        let v2 = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
    }

    #[tokio::test]
    async fn publish_retires_previous_active() {
        let reg = InMemoryStations::new();
        reg.create_draft(sample("dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        let active = reg
            .publish("dock", &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(active.status, WorkflowStatus::Active);

        reg.create_draft(sample("dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("dock", &test_actor(), Utc::now())
            .await
            .unwrap();

        let v1 = reg.get_version("dock", 1).await.unwrap();
        assert_eq!(v1.status, WorkflowStatus::Retired);
        let cur = reg.get_active("dock").await.unwrap();
        assert_eq!(cur.version, 2);
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryStations::new();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_active_is_name_ordered() {
        let reg = InMemoryStations::new();
        let mut b = sample("b-station");
        b.status = WorkflowStatus::Active;
        reg.seed(b).unwrap();
        let mut a = sample("a-station");
        a.status = WorkflowStatus::Active;
        reg.seed(a).unwrap();
        let mut d = sample("d-draft");
        d.status = WorkflowStatus::Draft;
        reg.seed(d).unwrap();

        let names: Vec<String> = reg
            .list_active()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            vec!["a-station".to_string(), "b-station".to_string()]
        );
    }

    // -----------------------------------------------------------
    // Upstream — retired (backlog d2cb38af, design deba3000).
    // -----------------------------------------------------------

    #[test]
    fn a_spec_that_still_carries_an_upstream_parses_and_never_writes_it_back() {
        // The walk upstream is the parent station's page, derived from
        // the tree (deba3000), so a station row no longer declares one.
        // Every `jobs.station.published` payload and every older CLI's
        // body written before backlog d2cb38af still carries the key,
        // and audit_log is the system of record: such a spec keeps
        // decoding, and the key it carried is read past, never echoed.
        let json = serde_json::json!({
            "name": "old-station",
            "version": 1,
            "status": "active",
            "title": "Older than the retirement",
            "kind": "group",
            "predicate": {},
            "upstream": { "label": "FEEDBACK", "href": "/system/feedback" },
            "created_at": "2026-08-01T00:00:00Z",
        });
        let spec: StationSpec = serde_json::from_value(json).unwrap();
        let back = serde_json::to_value(&spec).unwrap();
        assert!(back.get("upstream").is_none(), "{back}");
    }

    // -----------------------------------------------------------
    // Lens — the page context a surface needs to render this station.
    // -----------------------------------------------------------

    #[test]
    fn a_station_declares_no_lens_by_default() {
        // Most stations are queues nobody renders a whole page for.
        assert_eq!(sample("dock").lens, None);
    }

    #[test]
    fn lens_round_trips_with_its_declared_panels() {
        let mut spec = sample("design-review");
        spec.lens = Some(StationLens {
            eyebrow: Some("System Model · Design review".into()),
            title: "Design review".into(),
            subtitle: Some("Design docs waiting on a decision".into()),
            panels: vec!["queue".into(), "flow-strip".into()],
            with_steps: false,
        });
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["lens"]["title"], "Design review");
        assert_eq!(json["lens"]["panels"][0], "queue");
        let back: StationSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.lens, spec.lens);
    }

    #[test]
    fn a_lens_is_a_title_plus_whatever_panels_the_row_names() {
        // Only `title` is required: a lens with no panels is a page
        // that renders the queue and nothing else, which is the
        // common case and must not need three nulls to say so.
        let lens: StationLens = serde_json::from_value(serde_json::json!({
            "title": "Loading dock",
        }))
        .unwrap();
        assert_eq!(lens.title, "Loading dock");
        assert_eq!(lens.eyebrow, None);
        assert_eq!(lens.subtitle, None);
        assert!(lens.panels.is_empty());
    }

    #[test]
    fn a_row_written_before_the_lens_column_existed_still_parses() {
        let json = serde_json::json!({
            "name": "old-station",
            "version": 1,
            "status": "active",
            "title": "Older than the field",
            "kind": "group",
            "predicate": {},
            "created_at": "2026-08-01T00:00:00Z",
        });
        let spec: StationSpec = serde_json::from_value(json).unwrap();
        assert_eq!(spec.lens, None);
    }

    #[test]
    fn binding_self_carries_the_lens_through() {
        // The read edge renders the envelope from the BOUND row, so a
        // per-actor station would lose its whole page context here.
        let mut spec = sample("my-watchlist");
        spec.lens = Some(StationLens {
            eyebrow: None,
            title: "My watchlist".into(),
            subtitle: None,
            panels: vec![],
            with_steps: false,
        });
        let bound = spec.bind_self(Some("emp-r")).expect("binds");
        assert_eq!(bound.lens, spec.lens);
    }

    #[tokio::test]
    async fn capability_allows_role() {
        let cap = StationCapability {
            roles: vec!["head-brewer".into(), "brewer".into()],
            ..Default::default()
        };
        assert!(cap.allows_role("brewer"));
        assert!(!cap.allows_role("bookkeeper"));
        // Declared-but-empty gates nobody out.
        assert!(StationCapability::default().allows_role("anyone"));
    }

    /// The role half over BOTH vocabularies (backlog 4b103f0f): a
    /// claimant is admitted when ANY role the record holds for it is
    /// named — the platform role its request asserts, or the Class
    /// code its agents row declares. One spelling is the person's
    /// case, and `allows_role` is that case by definition.
    #[tokio::test]
    async fn capability_admits_any_role_the_record_holds() {
        let cap = StationCapability {
            roles: vec!["engineering-agent".into()],
            ..Default::default()
        };
        // The asserted role alone is not admitted; the row's is.
        assert!(!cap.admits_roles(&["platform-admin"]));
        assert!(cap.admits_roles(&["platform-admin", "engineering-agent"]));
        // Neither spelling named is still a refusal.
        assert!(!cap.admits_roles(&["platform-admin", "bookkeeper"]));
        // Declared-but-empty gates nobody out, and no spelling at all
        // (an anonymous claim) is refused by a declared list.
        assert!(StationCapability::default().admits_roles(&[]));
        assert!(!cap.admits_roles(&[]));
        // One spelling IS `allows_role`.
        assert_eq!(
            cap.admits_roles(&["engineering-agent"]),
            cap.allows_role("engineering-agent")
        );
    }

    /// The model half of a capability (design c87fb59b car 3): an agent
    /// serves a station whose `models` name one it runs; a person
    /// (no models) is gated by `roles` alone; a station with no models
    /// declared gates no agent out. The wire shape of a roles-only
    /// capability is unchanged — every row the registry already holds
    /// reads back byte-for-byte.
    #[tokio::test]
    async fn capability_allows_model() {
        let cap = StationCapability {
            roles: vec!["platform-admin".into()],
            models: vec!["opus-5[1m]".into()],
        };
        assert!(cap.allows_model(&["opus-5[1m]".to_string()]));
        assert!(cap.allows_model(&["haiku-4-5".to_string(), "opus-5[1m]".to_string()]));
        assert!(!cap.allows_model(&["haiku-4-5".to_string()]));
        assert!(
            cap.allows_model(&[]),
            "a claimant with no model is not an agent; roles decide"
        );
        assert!(StationCapability::default().allows_model(&["haiku-4-5".to_string()]));

        let roles_only = StationCapability {
            roles: vec!["bookkeeper".into()],
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&roles_only).unwrap(),
            serde_json::json!({ "roles": ["bookkeeper"] })
        );
        let read: StationCapability =
            serde_json::from_value(serde_json::json!({ "roles": ["bookkeeper"] })).unwrap();
        assert_eq!(read, roles_only);
        assert_eq!(
            serde_json::to_value(&cap).unwrap(),
            serde_json::json!({ "roles": ["platform-admin"], "models": ["opus-5[1m]"] })
        );
    }

    // -----------------------------------------------------------
    // Registry events — every write records an outbox event with
    // the row (the workflow-registry posture). The InMemory adapter
    // collects what the Pg adapter records inside the row
    // transaction; `recorded_events()` is the test window.
    // -----------------------------------------------------------

    #[tokio::test]
    async fn create_draft_records_draft_saved() {
        let reg = InMemoryStations::new();
        let draft = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();

        let events = reg.recorded_events();
        assert_eq!(events.len(), 1, "one write, one event");
        assert_eq!(events[0].kind, crate::events::STATION_DRAFT_SAVED);
        assert_eq!(events[0].payload["name"], "loading-dock");
        assert_eq!(events[0].payload["version"], draft.version);
        assert_eq!(events[0].payload["status"], "draft");
    }

    #[tokio::test]
    async fn publish_records_exactly_one_published_event() {
        let reg = InMemoryStations::new();
        let actor = test_actor();
        reg.create_draft(sample("loading-dock"), &actor, Utc::now())
            .await
            .unwrap();
        let published = reg
            .publish("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        let events: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STATION_PUBLISHED)
            .collect();
        assert_eq!(
            events.len(),
            1,
            "publish must record exactly one jobs.station.published"
        );
        let payload = &events[0].payload;
        // The actor rides as `_actor` in EventStamp's exact shape —
        // a Human serializes as the bare employee id.
        assert_eq!(payload["_actor"], "emp-test");
        // The payload IS the promoted spec.
        assert_eq!(payload["name"], "loading-dock");
        assert_eq!(payload["version"], published.version);
        assert_eq!(payload["status"], "active");
    }

    #[tokio::test]
    async fn retire_records_once_and_stays_silent_when_already_retired() {
        let reg = InMemoryStations::new();
        let actor = test_actor();
        reg.create_draft(sample("loading-dock"), &actor, Utc::now())
            .await
            .unwrap();
        reg.publish("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        reg.retire("loading-dock", &actor, Utc::now())
            .await
            .unwrap();
        // Second retire is the idempotent no-op path: nothing
        // active, so no event — the log records what happened, and
        // nothing happened.
        reg.retire("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        let retired: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STATION_RETIRED)
            .collect();
        assert_eq!(
            retired.len(),
            1,
            "idempotent retire must not record a second event"
        );
        assert_eq!(retired[0].payload["name"], "loading-dock");
        assert_eq!(retired[0].payload["status"], "retired");
    }

    #[test]
    fn a_declared_floor_survives_the_station_record() {
        let mut declared = serde_json::to_value(sample("department-floor")).unwrap();
        declared["floor"] = serde_json::json!({"row": 2, "col": 3});
        let station: StationSpec = serde_json::from_value(declared).unwrap();
        let recorded = serde_json::to_value(station).unwrap();
        assert_eq!(recorded["floor"], serde_json::json!({"row": 2, "col": 3}));
    }

    #[test]
    fn floor_coordinates_are_bounded_cells_and_legacy_rows_still_parse() {
        let mut declared = serde_json::to_value(sample("floor-cells")).unwrap();
        declared["floor"] = serde_json::json!({"row": 65536, "col": 0});
        assert!(serde_json::from_value::<StationSpec>(declared).is_err());
        let legacy = serde_json::to_value(sample("legacy-floor")).unwrap();
        let station: StationSpec = serde_json::from_value(legacy).unwrap();
        assert_eq!(station.floor, None);
    }

    /// Pass item 4 of design ccfe87ff (backlog 1bb8c479): a station may
    /// name the stations a packet goes to next. A row that declares none
    /// carries no `edges` key at all, so every stored row, published
    /// event and move-diagnosis receipt that copied one serializes
    /// exactly as it did before the field existed — and reads back with
    /// none.
    #[test]
    fn declared_edges_survive_the_record_and_a_row_without_them_reads_as_before() {
        let legacy = serde_json::to_value(sample("legacy-edges")).unwrap();
        assert!(
            legacy.get("edges").is_none(),
            "a row with no edges serializes as before: {legacy}"
        );
        let station: StationSpec = serde_json::from_value(legacy).unwrap();
        assert!(station.edges.is_empty());
        let mut declared = serde_json::to_value(sample("with-edges")).unwrap();
        declared["edges"] = serde_json::json!(["loading-dock", "repair"]);
        let station: StationSpec = serde_json::from_value(declared).unwrap();
        assert_eq!(station.edges, vec!["loading-dock", "repair"]);
        assert_eq!(
            serde_json::to_value(station).unwrap()["edges"],
            serde_json::json!(["loading-dock", "repair"])
        );
    }

    /// Backlog d0cf2cdc: an equipment station's automaton is judged
    /// against the registered ids by EXACT id, once per row; an
    /// automaton the row-alone lint already refuses (not spelled as an
    /// automation id, or on a row that is not equipment) is that lint's
    /// to name, never named twice.
    #[test]
    fn an_equipment_automaton_is_judged_against_the_registered_ids() {
        let equipment = |name: &str, automaton: &str| StationSpec {
            kind: StationKind::Equipment,
            automaton: Some(automaton.into()),
            bays: Some(1),
            ..sample(name)
        };
        let registered = vec!["automation:gate-runner".to_string()];
        let rows = vec![
            equipment("gates", "automation:gate-runner"),
            equipment("kettle", "automation:anything"),
            // A family member of a registered signer is no row of its own.
            equipment("vat", "automation:gate-runner:host"),
            equipment("misspelt", "emp-david"),
            StationSpec {
                automaton: Some("automation:anything".into()),
                ..sample("generic")
            },
        ];
        let problems = validate_station_automatons(&rows, &registered);
        assert_eq!(
            problems
                .iter()
                .map(|p| (p.station.as_str(), p.field.as_str()))
                .collect::<Vec<_>>(),
            vec![("kettle", "automaton"), ("vat", "automaton")]
        );
        assert!(problems[0].to_string().contains("`automation:anything`"));

        let mut spec = equipment("kettle", "automation:anything");
        assert!(gate_publish(&spec, &[], &[]).is_err());
        spec.automaton = Some("automation:gate-runner".into());
        assert_eq!(gate_publish(&spec, &[], &registered), Ok(()));
    }
}
