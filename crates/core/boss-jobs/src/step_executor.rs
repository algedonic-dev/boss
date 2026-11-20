//! What a step's `executor` names (factory taxonomy M3, item 0a78f8f8,
//! docs/design/factory-taxonomy-v2.md).
//!
//! ONE FIELD, TWO GRAINS. `StepSpec::executor` has named a credential
//! PRINCIPAL since design f623e425 (`runner:ops` on ops-request: only a
//! server-resolved credential for that principal may execute the step).
//! M3 asks the same field the same question — who executes this step —
//! at the grain of a KIND: `role:<slug>` (a person holding an
//! employee-kind role Class), `model:<class>` (a robot running a model
//! class the `robots:<class>` pool declares) or `equipment:<station>`
//! (the automaton operating an equipment station row). The prefix
//! implies the [`ExecutorKind`]; a declaration with none of the three
//! prefixes is a principal and stays the credential door's
//! ([`crate::credential_executor`]), judged by the viability lint's
//! resolvable list exactly as before. A second field answering "who
//! executes" beside the first would be two answers to one question.
//!
//! THE DECLARATION (T2b) parses the field and holds the lint that refuses
//! a kind no registry declares. THE CLAIM (T2c, backlog 4839d6a1) reads
//! it: a claim on a step whose pinned spec names a kind clears only when
//! the holder's [`ExecutorKind`] — off its CANONICAL id, the agents row
//! the door already reads (43ca3b3a) — equals the implied kind, AND its
//! capability matches ([`judge_kind`], [`judge_capability`]). A refusal
//! names which half failed and what was demanded against what was found
//! ([`Ineligible`]). A step without `executor`, or naming a principal,
//! keeps the claim door's predicate exactly as it was, and an in-flight
//! packet stays pinned to the version it was admitted under, so nothing
//! is demanded retroactively.

use std::collections::BTreeSet;

use serde::Serialize;

use boss_core::actor::ExecutorKind;

use crate::pools::PoolSpec;
use crate::registry::{WorkflowSpec, WorkflowStatus};
use crate::stations::{StationKind, StationSpec};

/// An executor declaration of a kind, parsed from `executor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorDemand {
    /// `role:<slug>` — a person holding that employee role Class.
    Role(String),
    /// `model:<class>[|<class>...]` — a robot running ANY of these model
    /// classes, in declared order (backlog 446ab559: a review is read by
    /// either family, `model:opus|sol`). One class is the set of one.
    Model(Vec<String>),
    /// `equipment:<station>` — the automaton of that equipment station.
    Equipment(String),
}

impl ExecutorDemand {
    /// `Ok(None)` for a declaration that names no kind — a credential
    /// principal (`runner:ops`), the credential door's to judge. `Err`
    /// for a kind prefix whose name no registry could declare (empty,
    /// or carrying whitespace or a further `:`), so a typo is refused
    /// rather than read as a principal.
    pub fn parse(declared: &str) -> Result<Option<Self>, String> {
        let named = |name: &str| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        };
        let refused = |what: &str| {
            Err(format!(
                "executor `{declared}` names no {}: after the prefix comes {what} of letters, \
                 digits, `-`, `.` or `_`",
                Self::noun_of(declared)
            ))
        };
        // A model declaration takes a SET: classes joined by `|`, each a
        // name, none twice (backlog 446ab559). A person or a station is
        // one name.
        if let Some(set) = declared.strip_prefix("model:") {
            let classes: Vec<String> = set.split('|').map(str::to_string).collect();
            let distinct: BTreeSet<&String> = classes.iter().collect();
            if !classes.iter().all(|c| named(c)) || distinct.len() != classes.len() {
                return refused("one name, or several distinct names joined by `|`, each");
            }
            return Ok(Some(Self::Model(classes)));
        }
        let (make, name): (fn(String) -> Self, &str) =
            if let Some(name) = declared.strip_prefix("role:") {
                (Self::Role, name)
            } else if let Some(name) = declared.strip_prefix("equipment:") {
                (Self::Equipment, name)
            } else {
                return Ok(None);
            };
        if !named(name) {
            return refused("one name");
        }
        Ok(Some(make(name.to_string())))
    }

    /// The classes a model declaration admits, or `None` for a person
    /// or a station.
    pub fn model_classes(&self) -> Option<&[String]> {
        match self {
            Self::Model(classes) => Some(classes),
            _ => None,
        }
    }

    /// The executor kind the prefix implies: role -> human, model ->
    /// robot, equipment -> automaton.
    pub fn kind(&self) -> ExecutorKind {
        match self {
            Self::Role(_) => ExecutorKind::Human,
            Self::Model(_) => ExecutorKind::Robot,
            Self::Equipment(_) => ExecutorKind::Automaton,
        }
    }

    fn noun_of(declared: &str) -> &'static str {
        match declared.split_once(':').map(|(p, _)| p) {
            Some("role") => "role",
            Some("model") => "model class",
            _ => "equipment station",
        }
    }
}

/// Is `declared` an executor KIND, as opposed to a credential principal?
/// A malformed kind counts as one: it is refused by the lint, never
/// handed to the credential door as a principal.
pub fn names_a_kind(declared: &str) -> bool {
    !matches!(ExecutorDemand::parse(declared), Ok(None))
}

/// Where a role demand can be judged. Roles are rows of the live
/// `classes` table — some declared through its door and in no migration
/// (engineering-agent, 20260928094618) — so the tree cannot answer.
#[derive(Debug, Clone, Copy)]
pub enum RoleVocabulary<'a> {
    /// The live employee role codes ([`live_role_codes`]).
    Read(&'a BTreeSet<String>),
    /// This reader holds no Class registry (the tree-wide pin): role
    /// demands are judged by the seed, before any publish. Said, not
    /// silently passed or refused.
    NotReadHere,
}

/// The registries a step's declarations are judged against: the pools a
/// `resources` block and a `model:` executor name, the stations an
/// `equipment:` executor names, and the roles a `role:` executor names.
#[derive(Debug, Clone, Copy)]
pub struct Declarations<'a> {
    pub pools: &'a [PoolSpec],
    pub stations: &'a [StationSpec],
    pub roles: RoleVocabulary<'a>,
}

impl Declarations<'_> {
    /// `Err` naming the missing declaration when no registry declares
    /// the kind this executor names; `Ok` for a principal (not this
    /// lint's) and for a declared kind.
    pub fn judge_executor(&self, declared: &str) -> Result<(), String> {
        let Some(demand) = ExecutorDemand::parse(declared)? else {
            return Ok(());
        };
        match &demand {
            ExecutorDemand::Role(role) => match self.roles {
                RoleVocabulary::Read(roles) if !roles.contains(role) => Err(format!(
                    "executor `{declared}` names no employee role: `{role}` is not an active \
                     Class (employee, role)"
                )),
                _ => Ok(()),
            },
            // Every class of the set is carried, as one class must be: a
            // class nobody declares strands the robots of the others less
            // visibly than a lone one, but it is the same typo.
            ExecutorDemand::Model(classes) => {
                let missing: Vec<String> = classes
                    .iter()
                    .map(|class| crate::resources::robot_pool(class))
                    .filter(|pool| {
                        !self
                            .pools
                            .iter()
                            .any(|p| &p.name == pool && p.status == WorkflowStatus::Active)
                    })
                    .map(|pool| format!("`{pool}`"))
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "executor `{declared}` names an undeclared model class: no active pool \
                         {}",
                        missing.join(", ")
                    ))
                }
            }
            ExecutorDemand::Equipment(name) => {
                match self
                    .stations
                    .iter()
                    .find(|s| &s.name == name && s.status == WorkflowStatus::Active)
                {
                    Some(s) if s.kind == StationKind::Equipment => Ok(()),
                    Some(s) => Err(format!(
                        "executor `{declared}` names `{name}`, which is not an equipment station \
                         (its kind is `{}`)",
                        s.kind.as_str()
                    )),
                    None => Err(format!(
                        "executor `{declared}` names no station: no active station row `{name}`"
                    )),
                }
            }
        }
    }
}

/// Does any step of these rows declare a `role:` executor? The seed
/// reads the Class registry only then, so a bundle declaring none adds
/// no read to the boot path.
pub fn declares_a_role(workflows: &[crate::registry::WorkflowSpec]) -> bool {
    workflows.iter().flat_map(|w| &w.steps).any(|s| {
        s.executor
            .as_deref()
            .is_some_and(|e| matches!(ExecutorDemand::parse(e), Ok(Some(ExecutorDemand::Role(_)))))
    })
}

/// The kind a step's PINNED spec demands of its executor: the step by
/// its `spec_slug` (the materialiser's key, as
/// [`crate::active_holder::declared_executor`] reads it). `Ok(None)` for
/// a step with no spec, no `executor`, or a principal — the claim door's
/// predicate is then exactly what it was. `Err` for a malformed kind,
/// which publish refuses; a row that carries one anyway admits nobody.
pub fn demanded_by(
    pinned: &WorkflowSpec,
    spec_slug: Option<&str>,
) -> Result<Option<ExecutorDemand>, String> {
    let Some(step) = spec_slug.and_then(|slug| pinned.steps.iter().find(|s| s.title == slug))
    else {
        return Ok(None);
    };
    match step.executor.as_deref() {
        Some(declared) => ExecutorDemand::parse(declared),
        None => Ok(None),
    }
}

impl ExecutorDemand {
    /// The declaration as the step spells it.
    pub fn declared(&self) -> String {
        match self {
            Self::Role(role) => format!("role:{role}"),
            Self::Model(classes) => format!("model:{}", classes.join("|")),
            Self::Equipment(station) => format!("equipment:{station}"),
        }
    }
}

/// Which half of the eligibility predicate a claim failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Half {
    /// The holder is not the kind the declaration implies.
    Kind,
    /// The holder is that kind, without the capability named.
    Capability,
}

/// A claim the step's executor declaration does not admit: the half that
/// failed, and what was demanded against what the door found — so the
/// refusal is a verdict, not a prompt to re-derive one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ineligible {
    pub half: Half,
    pub demanded: String,
    pub found: String,
}

impl Ineligible {
    /// The claim door's 403 body.
    pub fn refusal_body(&self, step_id: &str, executor: &str, holder: &str) -> serde_json::Value {
        serde_json::json!({
            "error": "the claimant is not the executor this step names",
            "half": self.half,
            "step_id": step_id,
            "executor": executor,
            "holder": holder,
            "demanded": self.demanded,
            "found": self.found,
        })
    }

    /// Every bay of `station` occupied. The words the claim door's read
    /// before the CAS and the adapters' count inside the claim's write
    /// (backlog 5a571b1a) both answer in, so a claim refused by either
    /// reads the same.
    pub fn no_free_bay(station: &str, occupied: u32, bays: i32) -> Self {
        Self {
            half: Half::Capability,
            demanded: format!("a free bay at {station}"),
            found: format!("no free bay: {occupied} of {bays} occupied"),
        }
    }
}

/// The first half: the holder's kind against the kind the prefix implies.
pub fn judge_kind(demand: &ExecutorDemand, holder_kind: ExecutorKind) -> Result<(), Ineligible> {
    if demand.kind() == holder_kind {
        return Ok(());
    }
    Err(Ineligible {
        half: Half::Kind,
        demanded: demand.kind().to_string(),
        found: holder_kind.to_string(),
    })
}

/// What the claim door read about the holder for one demand — the
/// second half's evidence, gathered by the door, judged here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// `role:` — does the roster list the holder among the role's
    /// active holders (`employees.role`, an `(employee, role)` Class)?
    Role { holds: bool },
    /// `model:` — the model class the holder's registry row records, and
    /// whether an active `robots:<class>` pool declares it. `None`
    /// is a row that records none: not eligible, never guessed from the
    /// model id it runs.
    Model { class: Option<String>, known: bool },
    /// `equipment:` — the station's row, or `None` when no active
    /// equipment station has that name.
    Equipment(Option<Bays>),
}

/// An equipment station's automaton and bays, and how many of those
/// bays the automaton's active holds already occupy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bays {
    pub automaton: String,
    pub bays: i32,
    pub occupied: u32,
}

/// The second half: the capability the declaration names, for `holder`
/// (its canonical id).
pub fn judge_capability(
    demand: &ExecutorDemand,
    holder: &str,
    found: &Capability,
) -> Result<(), Ineligible> {
    let refuse = |demanded: String, found: String| {
        Err(Ineligible {
            half: Half::Capability,
            demanded,
            found,
        })
    };
    match (demand, found) {
        (ExecutorDemand::Role(_), Capability::Role { holds: true }) => Ok(()),
        (ExecutorDemand::Role(role), Capability::Role { holds: false }) => refuse(
            format!("role {role}"),
            format!("{holder} does not hold {role}: the roster lists it among no active holders"),
        ),
        (ExecutorDemand::Model(classes), Capability::Model { class: held, known }) => {
            let class = classes.join(" or ");
            match (held.as_deref(), *known) {
                (None, _) => refuse(
                    format!("model class {class}"),
                    format!("the agents registry records no model class for {holder}"),
                ),
                (Some(held), false) => refuse(
                    format!("model class {class}"),
                    format!(
                        "{holder} records model class {held}, which no active pool \
                         robots:{held} declares"
                    ),
                ),
                (Some(held), true) if classes.iter().any(|c| c == held) => Ok(()),
                (Some(held), true) => refuse(
                    format!("model class {class}"),
                    format!("{holder} runs model class {held}"),
                ),
            }
        }
        (ExecutorDemand::Equipment(station), Capability::Equipment(row)) => {
            let demanded = format!("the automaton of equipment station {station}");
            match row {
                None => refuse(demanded, format!("no active equipment station {station}")),
                Some(b) if b.automaton != holder => refuse(
                    demanded,
                    format!("{station} is operated by {}, not {holder}", b.automaton),
                ),
                Some(b) if !bay_free(b.occupied, b.bays) => {
                    Err(Ineligible::no_free_bay(station, b.occupied, b.bays))
                }
                Some(_) => Ok(()),
            }
        }
        // The door reads the evidence the demand names; a mismatched
        // pair is a door defect, and it admits nobody.
        (demand, found) => refuse(
            demand.declared(),
            format!("evidence of another kind was read: {found:?}"),
        ),
    }
}

/// IS THIS HOLDER ELIGIBLE FOR THIS STEP — the one predicate (backlog
/// c17dec0f, cluster_20261109). Three readers ask it: the claim door
/// (`http::steps`, "THE STEP NAMES ITS EXECUTOR'S KIND"), the
/// executor-refused release (`jobs.release_executor_refused`) and the
/// matchmaker (`boss-dispatcher`). Each used to compose the two halves
/// itself, and two composed them short: the release left a person on a
/// `role:` step standing on kind alone, and the matchmaker judged only
/// a `model:` class — so a holder the door refuses was kept or
/// nominated. Here the composition lives once, as the door has it:
/// the kind is read off `holder` — its CANONICAL id, the agents row's
/// when one speaks for it (43ca3b3a) — through the one mapping
/// [`boss_core::actor::ActorId`] owns, and judged FIRST.
///
/// The capability half is reachable only through the [`KindAdmitted`]
/// this returns, so no reader can judge a capability for a holder of
/// the wrong kind, and none can stop at the kind: a reader has to say
/// what it read for every demand. WHAT each reader reads stays its own,
/// because each reads a different place (the door its repositories,
/// the release and the matchmaker the APIs); whether a fact read is
/// the holder's is this predicate's.
pub fn judge_holder<'a>(
    demand: &'a ExecutorDemand,
    holder: &'a str,
) -> Result<KindAdmitted<'a>, Ineligible> {
    let kind = match holder.parse::<boss_core::actor::ActorId>() {
        Ok(actor) => actor.executor_kind(),
        Err(never) => match never {},
    };
    judge_kind(demand, kind)?;
    Ok(KindAdmitted { demand, holder })
}

/// A holder whose executor KIND its step's demand admits — the only
/// road to the capability half ([`judge_holder`]). Read the evidence
/// [`KindAdmitted::demand`] names, then [`KindAdmitted::judge`] it.
#[derive(Debug, Clone, Copy)]
#[must_use = "a holder the kind admits is not yet eligible: judge its capability"]
pub struct KindAdmitted<'a> {
    demand: &'a ExecutorDemand,
    holder: &'a str,
}

impl KindAdmitted<'_> {
    /// The demand whose capability evidence is to be read.
    pub fn demand(&self) -> &ExecutorDemand {
        self.demand
    }

    /// The capability half, over the evidence read for this holder.
    pub fn judge(self, found: &Capability) -> Result<(), Ineligible> {
        judge_capability(self.demand, self.holder, found)
    }
}

/// How many bays of `station` its automaton's holds occupy: the
/// automaton's ACTIVE steps, other than `except` (the step being
/// claimed), whose PINNED spec demands `equipment:<station>`. One
/// assignment read and one registry read per pinned version.
///
/// A hold whose pinned version the registry no longer has declares
/// nothing and occupies no bay; any other failed read is an `Err` — a
/// bay count that could not be made is not a free bay. A read that
/// fills [`BAY_SCAN_LIMIT`] is refused rather than counted short: a
/// limit is not a filter.
pub async fn occupied_bays<R: crate::port::JobsRepository + ?Sized>(
    jobs: &R,
    workflows: &dyn crate::registry::WorkflowRegistry,
    automaton: &str,
    station: &str,
    except: &boss_core::job::StepId,
) -> Result<u32, String> {
    let rows = jobs
        .list_assignments(Some(automaton), &[], BAY_SCAN_LIMIT)
        .await
        .map_err(|e| format!("{automaton}'s holds could not be read: {e}"))?;
    if i64::try_from(rows.len()).unwrap_or(i64::MAX) >= BAY_SCAN_LIMIT {
        return Err(format!(
            "{automaton} holds {BAY_SCAN_LIMIT} or more steps; its bays at {station} cannot be \
             counted from one read"
        ));
    }
    let mut pinned: std::collections::BTreeMap<(String, i32), Option<WorkflowSpec>> =
        std::collections::BTreeMap::new();
    let mut occupied = 0u32;
    for row in rows
        .iter()
        .filter(|r| r.step.status == boss_core::job::StepStatus::Active && r.step.id != *except)
    {
        let key = (row.workflow.clone(), row.workflow_version);
        if !pinned.contains_key(&key) {
            let spec = match workflows.get_version(&key.0, key.1).await {
                Ok(spec) => Some(spec),
                Err(crate::registry::WorkflowError::NotFound(_)) => None,
                Err(e) => return Err(format!("{}@{}: {e}", key.0, key.1)),
            };
            pinned.insert(key.clone(), spec);
        }
        if occupies_bay(
            pinned.get(&key).and_then(Option::as_ref),
            row.step.spec_slug.as_deref(),
            station,
        ) {
            occupied = occupied.saturating_add(1);
        }
    }
    Ok(occupied)
}

// ---------------------------------------------------------------------------
// THE BAY IS TAKEN IN THE CLAIM'S OWN WRITE (backlog 5a571b1a). The claim
// door's `occupied_bays` above is a plain read before the CAS: two
// concurrent claims by one automaton could each read the last bay free
// and both be admitted, so a station ran over its bays. Each adapter now
// counts again INSIDE the claim's write, with these pieces — the Pg one
// under the station's lineage lock (`pg_advisory_xact_lock`, the one a
// station publish takes, `crate::declared_version`), the in-memory one
// under its state lock — and refuses with `JobsError::NoFreeBay`. The
// door's read stays: it answers the uncontended claim before the race,
// in the same words.
// ---------------------------------------------------------------------------

/// The equipment station a step's pinned spec demands, if any. A
/// version the registry no longer holds (`None`) declares nothing, and
/// a malformed declaration names no station — the claim door refuses
/// that on its own read.
pub fn equipment_demanded(
    pinned: Option<&WorkflowSpec>,
    spec_slug: Option<&str>,
) -> Option<String> {
    match pinned.and_then(|spec| demanded_by(spec, spec_slug).ok().flatten()) {
        Some(ExecutorDemand::Equipment(station)) => Some(station),
        _ => None,
    }
}

/// Whether one hold occupies a bay of `station`: its pinned spec demands
/// `equipment:<station>` of the step at `spec_slug`. The one predicate
/// [`occupied_bays`] and both adapters' claim writes count by.
pub fn occupies_bay(pinned: Option<&WorkflowSpec>, spec_slug: Option<&str>, station: &str) -> bool {
    equipment_demanded(pinned, spec_slug).as_deref() == Some(station)
}

/// The bays a claim by `holder` takes a share of at this station row:
/// `Some` only for an equipment station `holder` operates that declares
/// bays. Any other holder takes no bay — the claim door refuses it on
/// who it is, not on capacity.
pub fn bays_for(row: &StationSpec, holder: &str) -> Option<i32> {
    match (&row.kind, row.automaton.as_deref(), row.bays) {
        (StationKind::Equipment, Some(automaton), Some(bays)) if automaton == holder => Some(bays),
        _ => None,
    }
}

/// Whether one more hold fits: `occupied` holds other than the claimed
/// step, against `bays`.
pub fn bay_free(occupied: u32, bays: i32) -> bool {
    i64::from(occupied) < i64::from(bays)
}

/// The adapters' verdict on the count they made inside the claim's write.
pub fn take_bay(station: &str, occupied: u32, bays: i32) -> Result<(), crate::port::JobsError> {
    if bay_free(occupied, bays) {
        return Ok(());
    }
    Err(crate::port::JobsError::NoFreeBay {
        station: station.to_string(),
        occupied,
        bays,
    })
}

/// The station lineage acquired before a claim's population and row locks.
/// Construction is kept at the transaction's early lock edge; a later demand
/// change refuses rather than acquiring a second lineage under those locks.
#[cfg(feature = "postgres")]
pub struct StationBayLock {
    station: Option<String>,
}

#[cfg(feature = "postgres")]
impl StationBayLock {
    pub(crate) fn none() -> Self {
        Self { station: None }
    }
}

#[cfg(feature = "postgres")]
async fn demanded_station_in_tx(
    tx: &mut sqlx::PgConnection,
    step: &boss_core::job::Step,
    budget: Option<boss_events::statement_budget::StatementBudget>,
) -> Result<Option<String>, crate::port::JobsError> {
    use crate::port::JobsError;
    use boss_events::statement_budget::refresh;
    type RawRow = sqlx::types::Json<Box<serde_json::value::RawValue>>;
    let storage = |e: String| JobsError::Storage(e);
    let spec = |raw: RawRow| crate::resource_history::workflow(raw.0.get());

    refresh(tx, budget).await.map_err(storage)?;
    let pinned: Option<RawRow> = sqlx::query_scalar(
        "SELECT to_jsonb(w) FROM workflows w JOIN jobs j \
           ON j.kind = w.kind AND j.workflow_version = w.version WHERE j.id = $1",
    )
    .bind(*step.job_id.inner().as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| storage(e.to_string()))?;
    let pinned = pinned.map(spec).transpose().map_err(storage)?;
    Ok(equipment_demanded(
        pinned.as_ref(),
        step.spec_slug.as_deref(),
    ))
}

/// Every claimant and declared station publisher takes the lineage before
/// population/table or step-row locks. The demand is checked again after
/// those locks; a repin or step move refuses instead of taking another lock
/// in the opposite order (f4479975).
#[cfg(feature = "postgres")]
pub(crate) async fn lock_bay_in_tx(
    tx: &mut sqlx::PgConnection,
    step: &boss_core::job::Step,
    budget: Option<boss_events::statement_budget::StatementBudget>,
) -> Result<StationBayLock, crate::port::JobsError> {
    let station = demanded_station_in_tx(tx, step, budget).await?;
    if let Some(station) = &station {
        crate::declared_version::lock_and_read_newest_with_budget(
            tx, "stations", "name", station, budget,
        )
        .await
        .map_err(|error| crate::port::JobsError::Storage(error.to_string()))?;
    }
    Ok(StationBayLock { station })
}

#[cfg(feature = "postgres")]
/// Count the pinned station's active holds under its already held lineage.
/// Fresh statements see the previous holder's commits, without a list limit.
pub(crate) async fn take_bay_in_tx(
    tx: &mut sqlx::PgConnection,
    step: &boss_core::job::Step,
    holder: &str,
    locked: &StationBayLock,
    budget: Option<boss_events::statement_budget::StatementBudget>,
) -> Result<(), crate::port::JobsError> {
    use crate::port::JobsError;
    use boss_events::statement_budget::refresh;
    type RawRow = sqlx::types::Json<Box<serde_json::value::RawValue>>;
    let storage = |e: String| JobsError::Storage(e);
    let spec = |raw: RawRow| crate::resource_history::workflow(raw.0.get());
    let current = demanded_station_in_tx(tx, step, budget).await?;
    if current != locked.station {
        return Err(JobsError::SelectionConflict(
            "equipment demand changed after station lineage acquisition".into(),
        ));
    }
    let Some(station) = &locked.station else {
        return Ok(());
    };
    refresh(tx, budget).await.map_err(storage)?;
    let rows = crate::stations::active_station_rows_in(&mut *tx)
        .await
        .map_err(|e| storage(e.to_string()))?;
    let Some(bays) = rows
        .iter()
        .find(|row| row.name == *station)
        .and_then(|row| bays_for(row, holder))
    else {
        return Ok(());
    };

    refresh(tx, budget).await.map_err(storage)?;
    let holds: Vec<(Option<String>, String, i32)> = sqlx::query_as(
        "SELECT s.spec_slug, j.kind, j.workflow_version FROM steps s \
           JOIN jobs j ON j.id = s.job_id \
          WHERE s.assignee_id = $1 AND s.status = 'active' AND j.status = 'open' AND s.id <> $2",
    )
    .bind(holder)
    .bind(*step.id.inner().as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| storage(e.to_string()))?;
    let mut specs: std::collections::BTreeMap<(String, i32), Option<WorkflowSpec>> =
        std::collections::BTreeMap::new();
    let mut occupied = 0u32;
    for (slug, kind, version) in holds {
        let key = (kind, version);
        if !specs.contains_key(&key) {
            refresh(tx, budget).await.map_err(storage)?;
            let raw: Option<RawRow> = sqlx::query_scalar(
                "SELECT to_jsonb(w) FROM workflows w WHERE kind = $1 AND version = $2",
            )
            .bind(&key.0)
            .bind(key.1)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| storage(e.to_string()))?;
            specs.insert(key.clone(), raw.map(spec).transpose().map_err(storage)?);
        }
        if occupies_bay(
            specs.get(&key).and_then(Option::as_ref),
            slug.as_deref(),
            station,
        ) {
            occupied = occupied.saturating_add(1);
        }
    }
    take_bay(station, occupied, bays)
}

/// The most holds [`occupied_bays`] reads in one pass. An automaton
/// holding this many active steps is past anything a bay count was
/// sized for; the door refuses rather than count a prefix.
pub const BAY_SCAN_LIMIT: i64 = 1_000;

/// The live employee ROLE codes: `classes` rows of subject kind
/// `employee` whose member attribute is `role` and which are not
/// retired — the vocabulary `employees.role` and `agents.role` validate
/// against.
#[cfg(feature = "postgres")]
pub async fn live_role_codes(pool: &sqlx::PgPool) -> Result<BTreeSet<String>, sqlx::Error> {
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM classes \
          WHERE subject_kind = 'employee' AND member_attribute = 'role' AND retired_at IS NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(codes.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_prefix_implies_its_executor_kind() {
        assert_eq!(
            ExecutorDemand::parse("role:platform-admin"),
            Ok(Some(ExecutorDemand::Role("platform-admin".into())))
        );
        assert_eq!(
            ExecutorDemand::parse("model:opus").map(|d| d.map(|d| d.kind())),
            Ok(Some(ExecutorKind::Robot))
        );
        assert_eq!(
            ExecutorDemand::parse("role:ceo").map(|d| d.map(|d| d.kind())),
            Ok(Some(ExecutorKind::Human))
        );
        assert_eq!(
            ExecutorDemand::parse("equipment:gates").map(|d| d.map(|d| d.kind())),
            Ok(Some(ExecutorKind::Automaton))
        );
    }

    #[test]
    fn a_principal_names_no_kind_and_a_malformed_kind_is_refused() {
        assert_eq!(ExecutorDemand::parse("runner:ops"), Ok(None));
        assert!(!names_a_kind("runner:ops"));
        for malformed in ["role:", "model: opus", "equipment:", "role:a:b"] {
            let refused = ExecutorDemand::parse(malformed).unwrap_err();
            assert!(refused.contains(malformed), "{refused}");
            assert!(names_a_kind(malformed), "{malformed}");
        }
    }

    /// The credential door reads `credential_executor` off the step's
    /// materialised metadata and admits only a caller holding that
    /// principal — so a kind projected there would lock the step against
    /// every caller. Only a principal is projected.
    #[test]
    fn only_a_principal_is_materialised_as_the_credential_executor() {
        let step = |executor: &str| crate::registry::StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            executor: Some(executor.into()),
            ..Default::default()
        };
        let defaults = serde_json::json!({});
        let principal = crate::registry::merge_metadata(&defaults, &step("runner:ops"));
        assert_eq!(
            principal.get(crate::credential_executor::KEY),
            Some(&serde_json::json!("runner:ops"))
        );
        for kind in ["role:platform-admin", "model:opus", "equipment:gates"] {
            let merged = crate::registry::merge_metadata(&defaults, &step(kind));
            assert!(
                merged.get(crate::credential_executor::KEY).is_none(),
                "{kind}: {merged}"
            );
        }
    }

    /// The pure predicate's every arm, the model arms included, pinned
    /// against the decision it implements (conductor decision
    /// 20261104T0939: no class, or a class no pool declares, is not
    /// eligible). The claim route reaches each model arm through the
    /// class a robot's agents row records since T2d (backlog 06d8988b,
    /// `a_model_step_clears_for_a_robot_whose_row_records_that_class`).
    #[test]
    fn each_half_names_what_was_demanded_and_what_was_found() {
        let role = ExecutorDemand::Role("auditor".into());
        let model = ExecutorDemand::Model(vec!["opus".into()]);
        let kettle = ExecutorDemand::Equipment("kettle".into());

        let kind = judge_kind(&role, ExecutorKind::Robot).unwrap_err();
        assert_eq!(
            (kind.half, kind.demanded.as_str(), kind.found.as_str()),
            (Half::Kind, "person", "robot")
        );
        assert_eq!(judge_kind(&model, ExecutorKind::Robot), Ok(()));
        assert_eq!(judge_kind(&kettle, ExecutorKind::Automaton), Ok(()));

        assert_eq!(
            judge_capability(&role, "emp-1", &Capability::Role { holds: true }),
            Ok(())
        );
        let refused =
            judge_capability(&role, "emp-1", &Capability::Role { holds: false }).unwrap_err();
        assert_eq!(refused.half, Half::Capability);

        let model_of = |class: Option<&str>, known: bool| Capability::Model {
            class: class.map(str::to_string),
            known,
        };
        assert_eq!(
            judge_capability(&model, "agent-x", &model_of(Some("opus"), true)),
            Ok(())
        );
        for (found, says) in [
            (model_of(None, false), "records no model class"),
            (model_of(Some("haiku"), false), "no active pool"),
            (model_of(Some("sonnet"), true), "runs model class sonnet"),
        ] {
            let refused = judge_capability(&model, "agent-x", &found).unwrap_err();
            assert_eq!(refused.half, Half::Capability);
            assert!(refused.found.contains(says), "{refused:?}");
        }

        let bays = |automaton: &str, bays: i32, occupied: u32| {
            Capability::Equipment(Some(Bays {
                automaton: automaton.into(),
                bays,
                occupied,
            }))
        };
        let runner = "automation:kettle-runner";
        assert_eq!(
            judge_capability(&kettle, runner, &bays(runner, 2, 1)),
            Ok(())
        );
        for (found, says) in [
            (Capability::Equipment(None), "no active equipment station"),
            (
                bays("automation:other", 2, 0),
                "operated by automation:other",
            ),
            (bays(runner, 2, 2), "no free bay: 2 of 2 occupied"),
        ] {
            let refused = judge_capability(&kettle, runner, &found).unwrap_err();
            assert_eq!(refused.half, Half::Capability);
            assert!(refused.found.contains(says), "{refused:?}");
        }

        // Evidence of the wrong kind admits nobody.
        assert!(judge_capability(&role, "emp-1", &bays(runner, 2, 0)).is_err());
    }

    /// ONE PREDICATE, THREE READERS (backlog c17dec0f, cluster_20261109).
    /// The claim door, the executor-refused release and the matchmaker
    /// each answer "is this holder eligible for this step" through
    /// [`judge_holder`]: the kind off the holder's canonical id, judged
    /// FIRST, and the capability half reachable only through what the
    /// kind half admitted — so a reader that has not read the capability
    /// cannot skip it, and a robot never costs a roster read. The holders
    /// are the claim door's own fixture's
    /// (`a_claim_clears_only_for_the_executor_a_step_names.rs`).
    #[test]
    fn judge_holder_judges_the_kind_off_the_id_before_any_capability() {
        let role = ExecutorDemand::Role("auditor".into());
        for robot in ["agent-codex", "claude@algedonic.dev"] {
            let refused = judge_holder(&role, robot).map(|_| ()).unwrap_err();
            assert_eq!(
                (
                    refused.half,
                    refused.demanded.as_str(),
                    refused.found.as_str()
                ),
                (Half::Kind, "person", "robot"),
                "{robot}"
            );
        }
        let refused = judge_holder(&role, "emp-clerk")
            .expect("a person passes the kind half")
            .judge(&Capability::Role { holds: false })
            .unwrap_err();
        assert_eq!(refused.half, Half::Capability);
        assert!(
            refused.found.contains("emp-clerk does not hold auditor"),
            "{refused:?}"
        );
        assert_eq!(
            judge_holder(&role, "emp-auditor")
                .and_then(|admitted| admitted.judge(&Capability::Role { holds: true })),
            Ok(())
        );
        let kettle = ExecutorDemand::Equipment("kettle".into());
        let runner = "automation:kettle-runner";
        assert_eq!(
            judge_holder(&kettle, "emp-auditor")
                .map(|_| ())
                .unwrap_err()
                .half,
            Half::Kind
        );
        assert_eq!(
            judge_holder(&kettle, runner).and_then(|admitted| {
                admitted.judge(&Capability::Equipment(Some(Bays {
                    automaton: runner.into(),
                    bays: 1,
                    occupied: 0,
                })))
            }),
            Ok(())
        );
    }

    #[test]
    fn the_demand_is_read_off_the_pinned_step_by_its_slug() {
        let step = |title: &str, executor: Option<&str>| crate::registry::StepSpec {
            title: title.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            executor: executor.map(str::to_string),
            ..Default::default()
        };
        let pinned = WorkflowSpec::platform_seed(
            "fixture",
            "Fixture",
            "test",
            vec!["custom".into()],
            vec![
                step("audit", Some("role:auditor")),
                step("run", Some("runner:ops")),
                step("free", None),
            ],
        );
        assert_eq!(
            demanded_by(&pinned, Some("audit")),
            Ok(Some(ExecutorDemand::Role("auditor".into())))
        );
        for slug in [Some("run"), Some("free"), Some("absent"), None] {
            assert_eq!(demanded_by(&pinned, slug), Ok(None), "{slug:?}");
        }
    }

    /// The count each adapter makes inside the claim's write (backlog
    /// 5a571b1a) reads the same pieces the door's read does, and refuses
    /// in the door's words: only the station's own automaton takes a
    /// bay, only a hold demanding THAT station occupies one, and the
    /// last bay admits one more hold and no second.
    #[test]
    fn the_count_inside_the_claim_reads_and_refuses_as_the_door_does() {
        let step = |title: &str, executor: Option<&str>| crate::registry::StepSpec {
            title: title.into(),
            kind: "task".into(),
            ready_when: "true".into(),
            executor: executor.map(str::to_string),
            ..Default::default()
        };
        let pinned = WorkflowSpec::platform_seed(
            "fixture",
            "Fixture",
            "test",
            vec!["custom".into()],
            vec![
                step("boil", Some("equipment:kettle")),
                step("press", Some("equipment:press")),
                step("free", None),
            ],
        );
        assert!(occupies_bay(Some(&pinned), Some("boil"), "kettle"));
        assert!(!occupies_bay(Some(&pinned), Some("press"), "kettle"));
        assert!(!occupies_bay(Some(&pinned), Some("free"), "kettle"));
        assert!(
            !occupies_bay(None, Some("boil"), "kettle"),
            "a version the registry no longer holds declares nothing"
        );

        let kettle = StationSpec {
            automaton: Some("automation:kettle-runner".into()),
            bays: Some(2),
            ..StationSpec::draft(
                "kettle",
                "Kettle",
                StationKind::Equipment,
                crate::StationPredicate::default(),
                chrono::Utc::now(),
            )
        };
        assert_eq!(bays_for(&kettle, "automation:kettle-runner"), Some(2));
        assert_eq!(bays_for(&kettle, "automation:other"), None);

        assert!(take_bay("kettle", 1, 2).is_ok());
        let refused = take_bay("kettle", 2, 2).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "equipment station kettle has no free bay: 2 of 2 occupied"
        );
        let door = Ineligible::no_free_bay("kettle", 2, 2);
        assert!(refused.to_string().ends_with(&door.found), "{door:?}");
        assert_eq!(door.demanded, "a free bay at kettle");
    }

    /// A model declaration names a SET of classes, `model:opus|sol`
    /// (backlog 446ab559; conductor decision 20261105_1714Z: the two
    /// reviews stay undeclared until the declaration takes a set). It is
    /// one robot kind, and a robot of ANY class in the set clears its
    /// capability half; the refusal names every class demanded.
    #[test]
    fn a_model_declaration_names_a_set_and_any_class_in_it_clears() {
        let either = ExecutorDemand::parse("model:opus|sol")
            .expect("a set of classes parses")
            .expect("it names a kind");
        assert_eq!(either.kind(), ExecutorKind::Robot);
        assert_eq!(either.declared(), "model:opus|sol");
        assert_eq!(
            ExecutorDemand::parse("model:opus").map(|d| d.map(|d| d.declared())),
            Ok(Some("model:opus".to_string())),
            "one class is the set of one, spelled as before"
        );

        let robot = |class: &str| Capability::Model {
            class: Some(class.into()),
            known: true,
        };
        assert_eq!(judge_capability(&either, "agent-a", &robot("opus")), Ok(()));
        assert_eq!(judge_capability(&either, "agent-b", &robot("sol")), Ok(()));
        let refused = judge_capability(&either, "agent-c", &robot("haiku")).unwrap_err();
        assert_eq!(refused.half, Half::Capability);
        assert_eq!(refused.demanded, "model class opus or sol");
        assert!(
            refused.found.contains("runs model class haiku"),
            "{refused:?}"
        );
    }

    /// A set is spelled exactly: no empty member, no repeat, and only a
    /// model declaration takes one — a person or a station is one name.
    #[test]
    fn a_malformed_set_is_refused_and_only_a_model_takes_one() {
        for malformed in [
            "model:opus|",
            "model:|sol",
            "model:opus||sol",
            "model:opus|opus",
            "model:opus| sol",
            "role:auditor|ceo",
            "equipment:gates|dock",
        ] {
            let refused = ExecutorDemand::parse(malformed).unwrap_err();
            assert!(refused.contains(malformed), "{refused}");
            assert!(names_a_kind(malformed), "{malformed}");
        }
    }

    /// Every class of a set must be carried by an active
    /// `robots:<class>` pool, as one class must be: a set naming a
    /// class nobody declares is refused naming that class's pool.
    #[test]
    fn every_class_of_a_set_needs_its_model_slot_pool() {
        let pool = |name: &str| -> PoolSpec {
            serde_json::from_value(serde_json::json!({
                "name": name, "version": 1, "status": "active", "unit": "slot",
                "kind": "flow", "capacity": {"state": "unknown"}, "models": []
            }))
            .expect("a pool row")
        };
        let both = [pool("robots:opus"), pool("robots:sol")];
        let declarations = |pools| Declarations {
            pools,
            stations: &[],
            roles: RoleVocabulary::NotReadHere,
        };
        assert_eq!(
            declarations(&both[..]).judge_executor("model:opus|sol"),
            Ok(())
        );
        let refused = declarations(&both[..1])
            .judge_executor("model:opus|sol")
            .unwrap_err();
        assert!(refused.contains("robots:sol"), "{refused}");
        assert!(!refused.contains("robots:opus`"), "{refused}");
    }
}
