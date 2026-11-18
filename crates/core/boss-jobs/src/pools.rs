//! Versioned pool declarations and roster observations; reservations live in the ledger.
use crate::resources::Quantity;
use crate::{
    bundle_seed::{BundleRegistry, Declared},
    registry::WorkflowStatus,
    resources::ResourceBill,
    seed_loader::SeedLoaderError,
};
use async_trait::async_trait;
use boss_core::{actor::ActorId, event::Event};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Mutex};
pub const PUBLISHED: &str = "jobs.pool.published";
/// A pool's active version taken out of service through the live pools
/// door (backlog f399458f); the payload is the row it retired.
pub const RETIRED: &str = "jobs.pool.retired";

#[cfg(test)]
mod legacy_pool_tests {
    #[test]
    fn historical_validation_keeps_other_pool_invariants_and_authored_models_strict() {
        let original = serde_json::json!({
            "name": "model-slot:opus", "version": 2, "status": "active",
            "unit": "slot", "kind": "flow", "capacity": {"state": "unknown"},
            "models": ["historical-model"]
        });
        let decode = |value| serde_json::from_value::<super::PoolSpec>(value).unwrap();
        let legacy = decode(original.clone());
        assert!(legacy.validate_history().is_ok());
        assert!(legacy.validate().is_err());
        assert_eq!(
            serde_json::to_value(&legacy).unwrap()["name"],
            original["name"]
        );
        for (key, value) in [
            ("name", serde_json::json!("model-slot:from-run")),
            ("name", serde_json::json!("model-slot:")),
            ("name", serde_json::json!("model-slot:bad_class")),
            ("name", serde_json::json!("host:opus")),
            ("kind", serde_json::json!("stock")),
            ("version", serde_json::json!(0)),
            ("models", serde_json::json!(["duplicate", "duplicate"])),
            ("models", serde_json::json!([" invalid "])),
        ] {
            let mut invalid = original.clone();
            invalid[key] = value;
            assert!(
                decode(invalid.clone()).validate_history().is_err(),
                "{invalid}"
            );
        }
        let mut current = original;
        current["name"] = serde_json::json!("robots:opus");
        assert!(decode(current).validate().is_ok());
    }
}

/// Unknown is deliberately different from an explicitly configured zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PoolCapacity {
    Unknown {},
    Configured { amount: Quantity },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PoolKind {
    Flow,
    Stock,
}

/// The three pool shapes of the station tree (T5 of design 4b27a65a,
/// backlog 3f58c823; handoff §2): CAPACITY is held and given back (vCPU,
/// seats), STOCK is consumed and recovered by a protocol (disk), RATE is
/// spent per unit of time (dollars an hour). `kind` stays the reservation
/// mechanics — a capacity or a rate pool is a flow, a stock pool a stock —
/// and the shape names which of the three the pool is, which `kind` alone
/// cannot say of a rate. Optional and skipped on the wire when absent, so
/// a build that predates it reads a row carrying it by skipping the key
/// (PoolSpec is open; backlog fb893bba).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PoolShape {
    Capacity,
    Stock,
    Rate,
}

/// OPEN to unknown keys, and so are [`RosterDeclaration`] and
/// [`RosterBasis`] (backlog fb893bba). A row in resource_pools is read by
/// every build that may run against it, including the named last-converged
/// build a watchdog rollback lands on, which predates any field added
/// since. Closed, one new optional field (5ece507d's `reclaimer_kind`)
/// made every read of the table refuse on that build. The stored-row
/// reader (`resource_history::pool`) warns naming each key it read past;
/// an AUTHORED row is still refused for one, by [`load_pools`], because
/// only the build that ships a file reads it, so its unknown key is a typo.
/// [`PoolCapacity`] and `ClassRequirement` stay closed: dropping a key
/// there changes what the reader computes (a capacity, an eligibility
/// predicate), and failing to read is the safer answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolSpec {
    pub name: String,
    pub version: i32,
    pub status: WorkflowStatus,
    pub unit: String,
    pub kind: PoolKind,
    /// Capacity, stock or rate ([`PoolShape`]); must agree with `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<PoolShape>,
    pub capacity: PoolCapacity,
    /// Derived roster capacity keeps its Class predicate and audit basis on
    /// the same append-only declaration as the amount claims consume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<RosterDeclaration>,
    #[serde(default)]
    pub reclaimer: Option<String>,
    /// The packet kind each run of the reclaimer files — the key its
    /// record is read by. `reclaimer` names a script (v1) or an actor,
    /// and neither is a kind: the estate band asked GET
    /// /api/jobs?kind=infra/forge/disk-floor-sweep.sh, answered total 0,
    /// and drew "not read" for a reclaimer that runs hourly, because only
    /// the host's systemd unit tied the script to its packet kind
    /// (backlog 5ece507d). A stock's row says it here; the tree-wide pin
    /// `every_stock_reclaimer_names_the_kind_its_runs_file` holds it to a
    /// declared Workflow and to the wrap call of whatever runs the script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimer_kind: Option<String>,
    /// The executor is WHO; this path describes its program, never authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_verifier: Option<String>,
    /// Explicit unpaid identity binding. It cannot authorize a return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_pin_unenforced: Option<String>,
    /// Reserved for later department pools; this car admits factory pools only.
    #[serde(default)]
    pub department: Option<String>,
    /// The station whose work this pool serves, by its station row's
    /// `name` (pass item 3 of design ccfe87ff, backlog 56efd9c2): the
    /// mechanism behind the operations floor's binding per station, read
    /// off this row on the claims read and never inferred by the client.
    /// Unset where no station's work is what consumes the pool. A name is
    /// judged against the station bundle before any publish
    /// ([`validate_pool_stations`]); a label only, it changes no
    /// reservation, so it is not part of the roster contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    /// The station that KEEPS this pool, by its station row's `name`
    /// (backlog 42cd9a83): the mandate to keep the box running, which
    /// station-tree.md section 3 separates from both `station` (whose work
    /// consumes the pool) and an allocation (who owns a share of it) —
    /// "owning capacity is an allocation; keeping the box is a mandate".
    /// Every `host:` row names `operations` (the pin
    /// `every_host_pool_is_kept_by_operations`), and the station page band
    /// reads it rather than saying no read names the keeper. Judged
    /// against the station bundle beside `station`
    /// ([`validate_pool_stations`]); a label only, it changes no
    /// reservation, so it is not part of the roster contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeper: Option<String>,
    /// The model ids a `robots:<class>` pool meters, exactly as a
    /// run's `model` spells them (backlog af5dd547): how a bill whose
    /// `model` is `from-run` reads its class off the run, so the
    /// model-to-class fact is registry data rather than a parse of the
    /// id. Only a robot pool may list any, and a model listed by
    /// two active pools names no class. Empty, the pool meters no run
    /// by its model — every row in the tree until the deliberate publish
    /// of allocation item 5.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Where this row's capacity number comes from, in a sentence: the
    /// file, registry or observation that decides it, and for an
    /// `unknown` the reader that does not exist yet (T5 of the station
    /// tree, backlog 84f570a1; station-tree.md §3's last column). Prose
    /// for the console to show beside the number, never parsed; a label
    /// only, so it changes no reservation and is not part of the roster
    /// contract. Every authored row declares it (the pin
    /// `every_pool_row_says_where_its_capacity_comes_from`); a stored row
    /// from a build before it reads without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_from: Option<String>,
    #[serde(default)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterDeclaration {
    pub eligibility: Vec<boss_core::roster_capacity::ClassRequirement>,
    pub basis: Option<RosterBasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterBasis {
    pub revision: Option<i64>,
    /// The last qualified count and sequence survive an unavailable read;
    /// capacity remains Unknown and must never display this stale count.
    pub count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("invalid pool: {0}")]
    Invalid(String),
    #[error("pool version conflict: {0}")]
    Conflict(String),
    #[error("pool storage: {0}")]
    Storage(String),
}

impl PoolSpec {
    pub fn validate(&self) -> Result<(), PoolError> {
        self.validate_with_history(false)
    }

    /// Immutable pool facts retain the model-slot spelling used before T5.
    /// This exception belongs to replay, never to an authoring door (b1538d2d).
    pub(crate) fn validate_history(&self) -> Result<(), PoolError> {
        self.validate_with_history(true)
    }

    fn validate_with_history(&self, historical: bool) -> Result<(), PoolError> {
        if let Some(roster) = &self.roster {
            boss_core::roster_capacity::validate_eligibility(&roster.eligibility)
                .map_err(PoolError::Invalid)?;
            if self.kind != PoolKind::Flow {
                return Err(PoolError::Invalid(
                    "roster capacity requires a flow pool".into(),
                ));
            }
            let role = self
                .name
                .strip_prefix("people:")
                .filter(|role| !role.is_empty());
            if !roster.eligibility.iter().any(|r| {
                r.attribute == "role" && r.codes.len() == 1 && Some(r.codes[0].as_str()) == role
            }) {
                return Err(PoolError::Invalid(
                    "roster pool must bind its people:<Class role> eligibility".into(),
                ));
            }
            if roster
                .unknown_reason
                .as_ref()
                .is_some_and(|r| r.trim().is_empty() || r.len() > 512)
            {
                return Err(PoolError::Invalid(
                    "roster unknown reason must be nonempty and bounded".into(),
                ));
            }
            let count = roster.basis.as_ref().and_then(|basis| basis.count);
            if roster.basis.as_ref().is_some_and(|basis| {
                basis.revision.is_some_and(|seq| seq < 1)
                    || (basis.count.is_some_and(|n| n > 0) && basis.revision.is_none())
            }) {
                return Err(PoolError::Invalid(
                    "roster basis has no valid people audit sequence".into(),
                ));
            }
            match (&self.capacity, count) {
                (PoolCapacity::Unknown {}, _)
                    if roster.basis.is_none() || roster.unknown_reason.is_some() => {}
                (PoolCapacity::Configured { amount }, Some(count))
                    if roster.unknown_reason.is_none()
                        && roster.basis.as_ref().is_some_and(|b| b.as_of.is_some())
                        && amount.amount().map_err(PoolError::Invalid)?
                            == Quantity::from(count).amount().map_err(PoolError::Invalid)? => {}
                _ => {
                    return Err(PoolError::Invalid(
                        "capacity must equal its roster observation; unavailable is not zero"
                            .into(),
                    ));
                }
            }
        }
        if self.name.is_empty()
            || self.name.len() > 128
            || self
                .name
                .bytes()
                .any(|b| !b.is_ascii_lowercase() && !b.is_ascii_digit() && !b"-:_".contains(&b))
        {
            return Err(PoolError::Invalid(
                "name must be a nonempty lowercase pool key".into(),
            ));
        }
        if self.version < 1 || self.unit.trim().is_empty() || self.department.is_some() {
            return Err(PoolError::Invalid(
                "positive version and unit required; department must remain unconfigured".into(),
            ));
        }
        if let PoolCapacity::Configured { amount } = &self.capacity
            && amount.amount().is_err()
        {
            return Err(PoolError::Invalid(
                "configured capacity must be finite and nonnegative".into(),
            ));
        }
        if !matches!(
            (self.kind, self.shape),
            (_, None)
                | (PoolKind::Stock, Some(PoolShape::Stock))
                | (PoolKind::Flow, Some(PoolShape::Capacity | PoolShape::Rate))
        ) {
            return Err(PoolError::Invalid(
                "a stock pool's shape is stock; a flow pool's is capacity or rate".into(),
            ));
        }
        match (self.kind, self.reclaimer.as_deref()) {
            (PoolKind::Stock, Some(name)) if !name.trim().is_empty() => (),
            (PoolKind::Flow, None) => (),
            _ => {
                return Err(PoolError::Invalid(
                    "a stock names its reclaimer; a flow has none".into(),
                ));
            }
        }
        if self
            .station
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.trim() != s)
        {
            return Err(PoolError::Invalid(
                "station must name a station row: nonempty, no surrounding whitespace".into(),
            ));
        }
        if self
            .keeper
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.trim() != s)
        {
            return Err(PoolError::Invalid(
                "keeper must name a station row: nonempty, no surrounding whitespace".into(),
            ));
        }
        if self
            .capacity_from
            .as_ref()
            .is_some_and(|s| s.trim().is_empty() || s.len() > 1024)
        {
            return Err(PoolError::Invalid(
                "capacity_from must say where the capacity comes from: nonempty, at most 1024 bytes"
                    .into(),
            ));
        }
        // A packet kind, spelled as one: a script path here would repeat
        // the wrong target this field exists to replace (5ece507d).
        if let Some(kind) = self.reclaimer_kind.as_deref()
            && (self.kind != PoolKind::Stock
                || kind.is_empty()
                || kind.len() > 128
                || !kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
        {
            return Err(PoolError::Invalid(
                "a stock's reclaimer_kind names the packet kind its runs file; a flow has none"
                    .into(),
            ));
        }
        // The model a run is metered by (backlog af5dd547). The sentinel
        // is a bill's word, never a class: a pool spelled with it would
        // meter every older build's unresolved from-run claim.
        // Both spellings: a build older than T5 (backlog 84f570a1) bills
        // its unresolved claim under the old prefix.
        if self.name == crate::resources::robot_pool(crate::resources::MODEL_FROM_RUN)
            || self.name
                == format!(
                    "{}{}",
                    crate::resources::MODEL_SLOT_PREFIX_BEFORE_T5,
                    crate::resources::MODEL_FROM_RUN
                )
        {
            return Err(PoolError::Invalid(format!(
                "{} is the from-run bill's spelling, not a model class",
                self.name
            )));
        }
        // A robot pool's suffix is the class a bill bills, so it is judged
        // by the bill's own grammar (backlog 3901fc49): a class no bill can
        // carry would publish, and a from-run claim resolving to it would
        // fail its own bill as a 500.
        if let Some(class) = self.name.strip_prefix(crate::resources::ROBOTS_PREFIX)
            && !crate::resources::is_robot_class(class)
        {
            return Err(PoolError::Invalid(format!(
                "{} names no robot class: a class is nonempty lowercase letters, digits and '-'",
                self.name
            )));
        }
        if !self.models.is_empty() {
            let legacy = historical
                && self
                    .name
                    .strip_prefix(crate::resources::MODEL_SLOT_PREFIX_BEFORE_T5)
                    .is_some_and(crate::resources::is_robot_class);
            if !(self.name.starts_with(crate::resources::ROBOTS_PREFIX) || legacy)
                || self.kind != PoolKind::Flow
            {
                return Err(PoolError::Invalid(
                    "only a robots:<class> flow pool lists the models it meters".into(),
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            if self
                .models
                .iter()
                .any(|m| m.is_empty() || m.len() > 128 || m.trim() != m || !seen.insert(m.as_str()))
            {
                return Err(PoolError::Invalid(
                    "models must be distinct, nonempty model ids with no surrounding whitespace"
                        .into(),
                ));
            }
        }
        if [
            &self.program,
            &self.return_verifier,
            &self.identity_pin_unenforced,
        ]
        .into_iter()
        .any(|v| v.as_ref().is_some_and(|s| s.trim().is_empty()))
            || (self.kind == PoolKind::Flow
                && (self.program.is_some()
                    || self.return_verifier.is_some()
                    || self.identity_pin_unenforced.is_some()))
            || ((self.return_verifier.is_some() || self.identity_pin_unenforced.is_some())
                && self.program.is_none())
        {
            return Err(PoolError::Invalid(
                "stock return declarations require a named program; empty declarations refuse"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// Every key `given` carries that `read` (the parsed spec, serialized back)
/// does not, as dotted paths: the keys this build read past. Sound because
/// no field of these types is renamed, aliased or flattened, so a field's
/// serialized name is the name it was read from. A null is never one: it
/// carries nothing, and an absent Option may skip serializing.
pub(crate) fn unknown_keys(given: &serde_json::Value, read: &serde_json::Value) -> Vec<String> {
    fn walk(given: &serde_json::Value, read: &serde_json::Value, at: &str) -> Vec<String> {
        use serde_json::Value::{Array, Object};
        match (given, read) {
            (Object(given), Object(read)) => given
                .iter()
                .filter(|(_, value)| !value.is_null())
                .flat_map(|(key, value)| {
                    let path = if at.is_empty() {
                        key.clone()
                    } else {
                        format!("{at}.{key}")
                    };
                    match read.get(key) {
                        Some(kept) => walk(value, kept, &path),
                        None => vec![path],
                    }
                })
                .collect(),
            (Array(given), Array(read)) => given
                .iter()
                .zip(read)
                .enumerate()
                .flat_map(|(i, (given, read))| walk(given, read, &format!("{at}[{i}]")))
                .collect(),
            _ => Vec::new(),
        }
    }
    walk(given, read, "")
}

pub fn validate_bill(bill: &ResourceBill, pools: &[PoolSpec]) -> Result<(), String> {
    bill.validate()?;
    // The host a run draws on is the claim's choice from its owner's
    // floating share (backlog 5608c1a2), never a row's: a row naming one
    // would pin every run of the protocol to one box.
    if bill.host.is_some() {
        return Err("resources.host is written by the claim, never declared on a row".into());
    }
    // A from-run bill names its class only at the claim (backlog
    // af5dd547), so what a declaration can be judged on is that SOME
    // active robot pool meters a model by name; with none, every
    // claim on the step would be refused. This is the publish order item
    // 5 owes: the pools' `models` go live before a bill that reads them.
    let from_run = bill
        .takes_model_from_run()
        .then(|| crate::resources::robot_pool(crate::resources::MODEL_FROM_RUN));
    if from_run.is_some()
        && !pools.iter().any(|p| {
            p.status == WorkflowStatus::Active
                && p.name.starts_with(crate::resources::ROBOTS_PREFIX)
                && !p.models.is_empty()
        })
    {
        return Err(
            "resources.model = \"from-run\" needs an active robot pool that lists its models"
                .into(),
        );
    }
    for (name, _) in bill.pools() {
        if from_run.as_ref() == Some(&name) {
            continue;
        }
        if !pools
            .iter()
            .any(|p| p.name == name && p.status == WorkflowStatus::Active)
        {
            return Err(format!("resources names undeclared active pool `{name}`"));
        }
    }
    Ok(())
}

/// Bundle lint, shared by the seed's pre-write check and the tree-wide pin.
/// This validates declarations; it does not reserve resources at a claim.
///
/// A step's two registry-backed declarations, judged by ONE lint: its
/// `resources` block names declared pools, and since factory taxonomy M3
/// (item 0a78f8f8) its `executor`, when it names a kind, names a declared
/// one — an employee role Class, a `robots:<class>` pool, or an
/// equipment station row ([`crate::step_executor::Declarations`]).
pub fn validate_step_declarations(
    workflows: &[crate::registry::WorkflowSpec],
    declared: &crate::step_executor::Declarations<'_>,
) -> Vec<String> {
    workflows
        .iter()
        .flat_map(|w| {
            w.steps.iter().flat_map(move |s| {
                let bill = s
                    .resources
                    .as_ref()
                    .and_then(|bill| validate_bill(bill, declared.pools).err());
                let executor = s
                    .executor
                    .as_deref()
                    .and_then(|e| declared.judge_executor(e).err());
                [bill, executor]
                    .into_iter()
                    .flatten()
                    .map(move |why| format!("[{}] step `{}`: {why}", w.kind, s.title))
            })
        })
        .collect()
}

/// Every pool naming a station no ACTIVE row of `stations` declares, one
/// refusal each, naming the pool, the field and the station (pass item 3
/// of design ccfe87ff, backlog 56efd9c2). The seed runs it on the tree's
/// bundles before any publish, the same place and the same rows as an
/// `equipment:` executor is judged, the pools door on a live change
/// ([`registry_change_failures`]), and the tree-wide pin on the platform
/// bundles. Both names a row carries are judged: the `station` it serves
/// and the `keeper` that keeps it (backlog 42cd9a83).
pub fn validate_pool_stations(
    pools: &[PoolSpec],
    stations: &[crate::stations::StationSpec],
) -> Vec<String> {
    pools
        .iter()
        .flat_map(|p| {
            [("station", &p.station), ("keeper", &p.keeper)]
                .into_iter()
                .filter_map(move |(field, named)| named.as_deref().map(|s| (p, field, s)))
        })
        .filter(|(_, _, station)| {
            !stations
                .iter()
                .any(|s| s.name == *station && s.status == WorkflowStatus::Active)
        })
        .map(|(p, field, station)| {
            format!(
                "pool `{}` names {field} `{station}`, which no active station row declares",
                p.name
            )
        })
        .collect()
}

pub fn pools_beside(workflows: &Path) -> std::path::PathBuf {
    workflows
        .parent()
        .map(|p| p.join("pools"))
        .unwrap_or_else(|| "pools".into())
}

pub fn platform_pools_path() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../../infra/platform/pools")
}

pub fn load_pools(path: impl AsRef<Path>) -> Result<Vec<PoolSpec>, SeedLoaderError> {
    fn file(path: &Path) -> Result<Vec<PoolSpec>, SeedLoaderError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Bundle {
            pool: Vec<PoolSpec>,
        }
        let name = path.display().to_string();
        let text = std::fs::read_to_string(path)
            .map_err(|e| SeedLoaderError::Io(name.clone(), e.to_string()))?;
        let mut value: toml::Value = toml::from_str(&text)
            .map_err(|e| SeedLoaderError::Parse(name.clone(), e.to_string()))?;
        if let Some(rows) = value.get_mut("pool").and_then(toml::Value::as_array_mut) {
            for (i, row) in rows.iter_mut().enumerate() {
                if let Some(amount) = row.get_mut("capacity").and_then(|c| c.get_mut("amount")) {
                    crate::resources::toml_amount(amount, &format!("pool[{i}].capacity.amount"))
                        .map_err(|e| SeedLoaderError::Parse(name.clone(), e))?;
                }
            }
        }
        // Read before the typed parse consumes it: PoolSpec is open to an
        // unknown key, so a typo in an AUTHORED row is refused here, by
        // name, at any depth (backlog fb893bba; dropping only the
        // attribute let `reclaimr` load silently, measured by
        // an_authored_pool_file_naming_a_key_this_build_does_not_know_refuses).
        let authored = serde_json::to_value(&value)
            .map_err(|e| SeedLoaderError::Parse(name.clone(), e.to_string()))?;
        let bundle: Bundle = value
            .try_into()
            .map_err(|e: toml::de::Error| SeedLoaderError::Parse(name.clone(), e.to_string()))?;
        for (i, p) in bundle.pool.iter().enumerate() {
            let read = serde_json::to_value(p)
                .map_err(|e| SeedLoaderError::Parse(name.clone(), e.to_string()))?;
            let unknown = unknown_keys(&authored["pool"][i], &read);
            if !unknown.is_empty() {
                return Err(SeedLoaderError::Parse(
                    name.clone(),
                    format!(
                        "pool[{i}] names key(s) this build does not know: {}",
                        unknown.join(", ")
                    ),
                ));
            }
        }
        for p in &bundle.pool {
            p.validate()
                .map_err(|e| SeedLoaderError::Parse(name.clone(), e.to_string()))?;
            if p.status != WorkflowStatus::Active {
                return Err(SeedLoaderError::Parse(
                    name.clone(),
                    "pool bundles declare active rows; live retirement is not a seed".into(),
                ));
            }
        }
        Ok(bundle.pool)
    }
    let path = path.as_ref();
    if path.is_dir() {
        crate::seed_loader::load_bundle_dir(path, "pool", "name", file, |p| &p.name)
    } else {
        file(path)
    }
}

#[async_trait]
pub trait PoolRegistry: Send + Sync {
    async fn list_versions(&self, name: &str) -> Result<Vec<PoolSpec>, PoolError>;
    async fn list_active(&self) -> Result<Vec<PoolSpec>, PoolError>;
    /// Every name this registry has ever held a row of, any status,
    /// sorted byte for byte — the recovery export's walk
    /// (`crate::registry_export`, backlog c26aacaa). Refuses rather
    /// than answering a short list when the adapter cannot enumerate.
    async fn lineage_names(&self) -> Result<Vec<String>, PoolError> {
        Err(PoolError::Storage(
            "pool adapter cannot enumerate its lineages".into(),
        ))
    }
    async fn publish_declared(
        &self,
        spec: PoolSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError>;
    /// [`Self::publish_declared`] held to the live version the publish
    /// was signed over: the write lands only if the newest version the
    /// lineage holds, read under the SAME lock that guards the write, is
    /// exactly `over`; otherwise [`PoolError::Conflict`]
    /// ([`crate::declared_version::moved`]) and nothing is written.
    ///
    /// Review R1 of car 4019ba69 (2026-11-08): the approval road's CLI
    /// checked `--over` against a lineage GET and then POSTed the row
    /// alone, so a v2 committed between that read and the write was
    /// replaced by a v3 whose approval was a diff against v1. No read a
    /// client takes before its write can close that window; only the
    /// write's own transaction can.
    async fn publish_over(
        &self,
        spec: PoolSpec,
        over: i32,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError>;
    /// Take the active version of `name` out of service and answer the
    /// row retired, recording one [`RETIRED`] fact in the same write.
    /// `None` when nothing of the name is active, which writes nothing.
    /// Refused while the pool still holds an outstanding allocation: the
    /// held amount would be owed to a pool nothing serves.
    async fn retire(
        &self,
        name: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<Option<PoolSpec>, PoolError>;
    /// Observe an already-declared roster through this owner's lineage lock.
    /// Foreign adapters remain unavailable rather than publishing stale data.
    async fn observe_roster(
        &self,
        _expected: &PoolSpec,
        _observation: Result<
            boss_core::roster_capacity::RosterSnapshot,
            boss_core::roster_capacity::RosterSnapshotError,
        >,
        _actor: &ActorId,
        _now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        Err(PoolError::Storage(
            "adapter does not support roster observations".into(),
        ))
    }
}

#[cfg(all(test, feature = "postgres"))]
#[path = "pools/tests/roster_observation.rs"]
mod roster_observation_tests;

impl Declared for PoolSpec {
    fn name(&self) -> &str {
        &self.name
    }
    fn version(&self) -> i32 {
        self.version
    }
    fn status(&self) -> WorkflowStatus {
        self.status
    }
}
#[async_trait]
impl BundleRegistry for dyn PoolRegistry + '_ {
    type Spec = PoolSpec;
    type Error = PoolError;
    const LABEL: &'static str = "platform-pool-seed";
    const BUNDLE: &'static str = "infra/platform/pools/<name>.toml";
    const VERSIONED: bool = true;
    async fn live_versions(&self, name: &str) -> Result<Vec<PoolSpec>, PoolError> {
        self.list_versions(name).await
    }
    async fn publish_declared(
        &self,
        spec: PoolSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        PoolRegistry::publish_declared(self, spec, actor, now).await
    }
}
pub async fn seed_pools(
    registry: &dyn PoolRegistry,
    specs: &[PoolSpec],
    actor: &ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<crate::bundle_seed::SeedReport, crate::bundle_seed::BundleSeedError<PoolError>> {
    crate::bundle_seed::seed_bundle(registry, specs, actor, now, dry_run).await
}

fn event(spec: &PoolSpec, actor: &ActorId, now: DateTime<Utc>) -> Result<Event, PoolError> {
    event_of(PUBLISHED, spec, actor, now)
}

fn event_of(
    kind: &str,
    spec: &PoolSpec,
    actor: &ActorId,
    now: DateTime<Utc>,
) -> Result<Event, PoolError> {
    let payload = serde_json::to_value(spec).map_err(|e| PoolError::Invalid(e.to_string()))?;
    Ok(Event::new(
        "jobs",
        kind,
        boss_core::publisher::inject_actor(payload, actor),
        now,
    ))
}

/// One pool row as an author sent it to the live door (backlog
/// f399458f), held to the checks the seed's loader holds a file to: a
/// key this build does not know is a typo and is refused by name,
/// [`PoolSpec::validate`], and only an active row publishes — retiring
/// is its own door. `created_at` is the door's to stamp.
pub fn authored_row(given: &serde_json::Value) -> Result<PoolSpec, PoolError> {
    let spec: PoolSpec =
        serde_json::from_value(given.clone()).map_err(|e| PoolError::Invalid(e.to_string()))?;
    let read = serde_json::to_value(&spec).map_err(|e| PoolError::Invalid(e.to_string()))?;
    let unknown = unknown_keys(given, &read);
    if !unknown.is_empty() {
        return Err(PoolError::Invalid(format!(
            "the row names key(s) this build does not know: {}",
            unknown.join(", ")
        )));
    }
    spec.validate()?;
    if spec.status != WorkflowStatus::Active {
        return Err(PoolError::Invalid(format!(
            "a published row is active; this one declares `{}` — retiring is \
             POST /api/pools/{}/retire",
            spec.status.as_str(),
            spec.name
        )));
    }
    Ok(spec)
}

/// The seed's three cross-registry lints, judged on a live change rather
/// than on the tree (backlog f399458f): a pool's `station` names an
/// active station row ([`validate_pool_stations`]), the station tree
/// holds ([`crate::station_tree::validate_station_tree`]), and every
/// step bill of the given protocols names an active pool
/// ([`validate_bill`]). Run on the active pools `before` and `after` the
/// change, it answers only the failures the change ADDS: a failure the
/// live registry already holds is not this publish's to answer for, and
/// refusing on it would make every row unpublishable until it is fixed.
pub fn registry_change_failures(
    before: &[PoolSpec],
    after: &[PoolSpec],
    stations: &[crate::stations::StationSpec],
    workflows: &[crate::registry::WorkflowSpec],
) -> Vec<String> {
    let lint = |pools: &[PoolSpec]| -> Vec<String> {
        let bills = workflows.iter().flat_map(|w| {
            w.steps.iter().filter_map(move |s| {
                s.resources
                    .as_ref()
                    .and_then(|bill| validate_bill(bill, pools).err())
                    .map(|why| format!("[{}] step `{}`: {why}", w.kind, s.title))
            })
        });
        validate_pool_stations(pools, stations)
            .into_iter()
            .chain(crate::station_tree::validate_station_tree(stations, pools))
            .chain(bills)
            .collect()
    };
    let held = lint(before);
    lint(after)
        .into_iter()
        .filter(|failure| !held.contains(failure))
        .collect()
}

pub(crate) fn same_roster_contract(before: &PoolSpec, after: &PoolSpec) -> bool {
    before.name == after.name
        && before.kind == after.kind
        && before.unit == after.unit
        && before.reclaimer == after.reclaimer
        && before.department == after.department
        && before.roster.as_ref().map(|r| &r.eligibility)
            == after.roster.as_ref().map(|r| &r.eligibility)
}

fn roster_candidate(
    expected: &PoolSpec,
    observation: Result<
        boss_core::roster_capacity::RosterSnapshot,
        boss_core::roster_capacity::RosterSnapshotError,
    >,
    now: DateTime<Utc>,
) -> Result<PoolSpec, PoolError> {
    expected.validate()?;
    if expected.status != WorkflowStatus::Active || expected.roster.is_none() {
        return Err(PoolError::Invalid(
            "roster observation requires an active derived declaration".into(),
        ));
    }
    let mut next = expected.clone();
    let roster = next
        .roster
        .as_mut()
        .ok_or_else(|| PoolError::Invalid("missing roster declaration".into()))?;
    match observation {
        Ok(snapshot) => {
            snapshot.validate().map_err(PoolError::Invalid)?;
            if snapshot.eligibility != roster.eligibility {
                return Err(PoolError::Conflict(
                    "roster eligibility differs from its pinned declaration".into(),
                ));
            }
            if let Some(previous) = &roster.basis {
                if snapshot.revision < previous.revision
                    || (snapshot.revision == previous.revision
                        && previous.count != Some(snapshot.count))
                {
                    return Err(PoolError::Conflict(
                        "roster observation is stale or disagrees at one audit revision".into(),
                    ));
                }
                if snapshot.revision == previous.revision && roster.unknown_reason.is_none() {
                    return Ok(expected.clone());
                }
            }
            next.capacity = PoolCapacity::Configured {
                amount: Quantity::from(snapshot.count),
            };
            roster.basis = Some(RosterBasis {
                revision: snapshot.revision,
                count: Some(snapshot.count),
                as_of: Some(now),
            });
            roster.unknown_reason = None;
        }
        Err(boss_core::roster_capacity::RosterSnapshotError::Unavailable(reason))
        | Err(boss_core::roster_capacity::RosterSnapshotError::Invalid(reason)) => {
            if roster.unknown_reason.as_ref() == Some(&reason)
                && matches!(next.capacity, PoolCapacity::Unknown {})
            {
                return Ok(expected.clone());
            }
            next.capacity = PoolCapacity::Unknown {};
            roster.unknown_reason = Some(reason);
        }
    }
    next.version = expected
        .version
        .checked_add(1)
        .ok_or_else(|| PoolError::Conflict("roster version exhausted".into()))?;
    next.validate()?;
    Ok(next)
}

/// The neutral source and existing pool owner are explicit dependencies. The
/// owner rechecks the exact declaration after the external snapshot read.
pub async fn refresh_roster(
    registry: &dyn PoolRegistry,
    source: &dyn boss_core::roster_capacity::RosterCapacitySnapshot,
    expected: &PoolSpec,
    actor: &ActorId,
    now: DateTime<Utc>,
) -> Result<PoolSpec, PoolError> {
    let roster = expected
        .roster
        .as_ref()
        .ok_or_else(|| PoolError::Invalid("not a derived roster pool".into()))?;
    let observation = source.snapshot(&roster.eligibility).await;
    registry
        .observe_roster(expected, observation, actor, now)
        .await
}
#[derive(Default)]
pub(crate) struct State {
    pub(crate) rows: BTreeMap<(String, i32), PoolSpec>,
    events: Vec<Event>,
    pub(crate) claims: BTreeMap<String, (crate::flow_resources::Claim, bool)>,
    pub(crate) identities: BTreeMap<uuid::Uuid, crate::resource_leases::IdentityBinding>,
    pub(crate) obligations: BTreeMap<(uuid::Uuid, String), crate::resource_leases::StockObligation>,
    pub(crate) return_evidence: BTreeMap<uuid::Uuid, (uuid::Uuid, String)>,
    pub(crate) lease_expiry: std::collections::BTreeSet<(DateTime<Utc>, String)>,
}
#[derive(Default)]
pub struct InMemoryPools {
    state: Mutex<State>,
}
impl InMemoryPools {
    pub(crate) fn resource_lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, State>, crate::port::JobsError> {
        self.state
            .lock()
            .map_err(|_| crate::port::JobsError::Storage("resource-pool owner unavailable".into()))
    }
    pub(crate) fn try_resource_lock(
        &self,
    ) -> Result<Option<std::sync::MutexGuard<'_, State>>, crate::port::JobsError> {
        match self.state.try_lock() {
            Ok(guard) => Ok(Some(guard)),
            Err(std::sync::TryLockError::WouldBlock) => Ok(None),
            Err(_) => Err(crate::port::JobsError::Storage(
                "resource-pool owner unavailable".into(),
            )),
        }
    }
    pub fn recorded_events(&self) -> Result<Vec<Event>, PoolError> {
        Ok(self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?
            .events
            .clone())
    }
}
#[async_trait]
impl PoolRegistry for InMemoryPools {
    async fn list_versions(&self, name: &str) -> Result<Vec<PoolSpec>, PoolError> {
        Ok(self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?
            .rows
            .values()
            .filter(|p| p.name == name)
            .cloned()
            .collect())
    }
    async fn lineage_names(&self) -> Result<Vec<String>, PoolError> {
        let names: std::collections::BTreeSet<String> = self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?
            .rows
            .keys()
            .map(|(name, _)| name.clone())
            .collect();
        Ok(names.into_iter().collect())
    }
    async fn list_active(&self) -> Result<Vec<PoolSpec>, PoolError> {
        Ok(self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?
            .rows
            .values()
            .filter(|p| p.status == WorkflowStatus::Active)
            .cloned()
            .collect())
    }
    async fn publish_declared(
        &self,
        spec: PoolSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(spec, None, None, actor, now).await
    }
    async fn publish_over(
        &self,
        spec: PoolSpec,
        over: i32,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(spec, None, Some(over), actor, now)
            .await
    }
    async fn retire(
        &self,
        name: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<Option<PoolSpec>, PoolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?;
        let Some(mut retired) = state
            .rows
            .values()
            .find(|p| p.name == name && p.status == WorkflowStatus::Active)
            .cloned()
        else {
            return Ok(None);
        };
        let held = crate::flow_resources_memory::held(&state, name).map_err(PoolError::Storage)?;
        if !held.is_zero() {
            return Err(PoolError::Conflict(format!(
                "{name} still holds {held} {} in outstanding allocations — retire it once \
                 they are released",
                retired.unit
            )));
        }
        retired.status = WorkflowStatus::Retired;
        let fact = event_of(RETIRED, &retired, actor, now)?;
        state
            .rows
            .insert((retired.name.clone(), retired.version), retired.clone());
        state.events.push(fact);
        Ok(Some(retired))
    }
    async fn observe_roster(
        &self,
        expected: &PoolSpec,
        observation: Result<
            boss_core::roster_capacity::RosterSnapshot,
            boss_core::roster_capacity::RosterSnapshotError,
        >,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(
            roster_candidate(expected, observation, now)?,
            Some(expected),
            None,
            actor,
            now,
        )
        .await
    }
}
impl InMemoryPools {
    async fn publish_checked(
        &self,
        mut spec: PoolSpec,
        expected: Option<&PoolSpec>,
        over: Option<i32>,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        spec.validate()?;
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        let fact = event(&spec, actor, now)?;
        let mut state = self
            .state
            .lock()
            .map_err(|e| PoolError::Storage(e.to_string()))?;
        let newest = crate::declared_version::newest(
            state
                .rows
                .values()
                .filter(|p| p.name == spec.name)
                .map(|p| p.version),
        );
        // The signed baseline, judged under the same lock as the write
        // (`publish_over`; review R1 of car 4019ba69).
        if let Some(over) = over
            && newest != over
        {
            return Err(PoolError::Conflict(crate::declared_version::moved(
                &spec.name, over, newest,
            )));
        }
        if let Some(expected) = expected {
            let current = state
                .rows
                .values()
                .find(|p| p.name == spec.name && p.status == WorkflowStatus::Active);
            if current != Some(expected) {
                return Err(PoolError::Conflict(
                    "roster declaration or observation changed during snapshot read".into(),
                ));
            }
            if spec.version == expected.version {
                return Ok(expected.clone());
            }
        }
        crate::flow_resources_memory::publication_fence(&state, &spec, expected.is_some())
            .map_err(PoolError::Conflict)?;
        if spec.version <= newest {
            return Err(PoolError::Conflict(crate::declared_version::not_above(
                &spec.name,
                spec.version,
                newest,
            )));
        }
        for row in state
            .rows
            .values_mut()
            .filter(|p| p.name == spec.name && p.status == WorkflowStatus::Active)
        {
            row.status = WorkflowStatus::Retired;
        }
        state
            .rows
            .insert((spec.name.clone(), spec.version), spec.clone());
        state.events.push(fact);
        Ok(spec)
    }
}

#[cfg(feature = "postgres")]
pub struct PgPools {
    pool: sqlx::PgPool,
}
#[cfg(feature = "postgres")]
impl PgPools {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}
#[cfg(feature = "postgres")]
#[async_trait]
impl PoolRegistry for PgPools {
    async fn list_versions(&self, name: &str) -> Result<Vec<PoolSpec>, PoolError> {
        let rows:Vec<sqlx::types::Json<Box<serde_json::value::RawValue>>>=sqlx::query_scalar("SELECT spec || jsonb_build_object('status',status) FROM resource_pools WHERE name=$1 ORDER BY version").bind(name).fetch_all(&self.pool).await.map_err(|e|PoolError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| crate::resource_history::pool(r.0.get()).map_err(PoolError::Storage))
            .collect()
    }
    async fn lineage_names(&self) -> Result<Vec<String>, PoolError> {
        sqlx::query_scalar(
            "SELECT name FROM resource_pools GROUP BY name ORDER BY name COLLATE \"C\"",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PoolError::Storage(e.to_string()))
    }
    async fn list_active(&self) -> Result<Vec<PoolSpec>, PoolError> {
        let rows: Vec<sqlx::types::Json<Box<serde_json::value::RawValue>>> = sqlx::query_scalar(
            "SELECT spec FROM resource_pools WHERE status='active' ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PoolError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| crate::resource_history::pool(r.0.get()).map_err(PoolError::Storage))
            .collect()
    }
    async fn publish_declared(
        &self,
        spec: PoolSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(spec, None, None, actor, now).await
    }
    async fn publish_over(
        &self,
        spec: PoolSpec,
        over: i32,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(spec, None, Some(over), actor, now)
            .await
    }
    async fn retire(
        &self,
        name: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<Option<PoolSpec>, PoolError> {
        let storage = |e: sqlx::Error| PoolError::Storage(e.to_string());
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let result: Result<Option<PoolSpec>, PoolError> = async {
            // The lineage lock every publish of this name takes, so a
            // retire and a publish of one pool never interleave.
            crate::declared_version::lock_and_read_newest(&mut tx, "resource_pools", "name", name)
                .await
                .map_err(storage)?;
            let active: Option<sqlx::types::Json<Box<serde_json::value::RawValue>>> =
                sqlx::query_scalar(
                    "SELECT spec || jsonb_build_object('status',status) FROM resource_pools \
                     WHERE name=$1 AND status='active'",
                )
                .bind(name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?;
            let Some(active) = active else {
                return Ok(None);
            };
            let mut retired =
                crate::resource_history::pool(active.0.get()).map_err(PoolError::Storage)?;
            let held = crate::flow_resources_pg::held(&mut tx, name).await?;
            if !held.is_zero() {
                return Err(PoolError::Conflict(format!(
                    "{name} still holds {held} {} in outstanding allocations — retire it once \
                     they are released",
                    retired.unit
                )));
            }
            sqlx::query(
                "UPDATE resource_pools SET status='retired' WHERE name=$1 AND status='active'",
            )
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            retired.status = WorkflowStatus::Retired;
            let fact = event_of(RETIRED, &retired, actor, now)?;
            boss_events::outbox::record_event_in_tx(&mut tx, &fact)
                .await
                .map_err(PoolError::Storage)?;
            Ok(Some(retired))
        }
        .await;
        match result {
            Ok(retired) => {
                tx.commit().await.map_err(|e| {
                    PoolError::Storage(format!(
                        "pool retire COMMIT acknowledgement failed; effect uncertain: {e}"
                    ))
                })?;
                Ok(retired)
            }
            Err(error) => match tx.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(PoolError::Storage(format!(
                    "{error}; rollback acknowledgement failed: {rollback}"
                ))),
            },
        }
    }
    async fn observe_roster(
        &self,
        expected: &PoolSpec,
        observation: Result<
            boss_core::roster_capacity::RosterSnapshot,
            boss_core::roster_capacity::RosterSnapshotError,
        >,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        self.publish_checked(
            roster_candidate(expected, observation, now)?,
            Some(expected),
            None,
            actor,
            now,
        )
        .await
    }
}
#[cfg(feature = "postgres")]
impl PgPools {
    async fn publish_checked(
        &self,
        mut spec: PoolSpec,
        expected: Option<&PoolSpec>,
        over: Option<i32>,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<PoolSpec, PoolError> {
        spec.validate()?;
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        let fact = event(&spec, actor, now)?;
        let value = serde_json::to_value(&spec).map_err(|e| PoolError::Invalid(e.to_string()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PoolError::Storage(e.to_string()))?;
        let result:Result<bool,PoolError>=async {
            let newest=crate::declared_version::lock_and_read_newest(&mut tx,"resource_pools","name",&spec.name).await.map_err(|e|PoolError::Storage(e.to_string()))?;
            // The signed baseline, judged in the statement AFTER the
            // lineage's advisory lock is held — the same transaction
            // that inserts the row, so no writer can land between this
            // read and that write (`publish_over`; review R1 of car
            // 4019ba69).
            if let Some(over) = over
                && newest != over
            {
                return Err(PoolError::Conflict(crate::declared_version::moved(&spec.name, over, newest)));
            }
            if let Some(expected) = expected {
                let current: Option<sqlx::types::Json<Box<serde_json::value::RawValue>>> = sqlx::query_scalar("SELECT spec FROM resource_pools WHERE name=$1 AND status='active'")
                    .bind(&spec.name).fetch_optional(&mut *tx).await.map_err(|e| PoolError::Storage(e.to_string()))?;
                let current = current.map(|j| crate::resource_history::pool(j.0.get()).map_err(PoolError::Storage)).transpose()?;
                if current.as_ref() != Some(expected) { return Err(PoolError::Conflict("roster declaration or observation changed during snapshot read".into())); }
                if spec.version == expected.version { return Ok(false); }
            }
            if spec.version<=newest {return Err(PoolError::Conflict(crate::declared_version::not_above(&spec.name,spec.version,newest)));}
            crate::flow_resources_pg::publication_fence(&mut tx,&spec,expected.is_some()).await?;
            sqlx::query("UPDATE resource_pools SET status='retired' WHERE name=$1 AND status='active'").bind(&spec.name).execute(&mut *tx).await.map_err(|e|PoolError::Storage(e.to_string()))?;
            sqlx::query("INSERT INTO resource_pools(name,version,status,spec,created_at) VALUES($1,$2,'active',$3,$4)").bind(&spec.name).bind(spec.version).bind(value).bind(now).execute(&mut *tx).await.map_err(|e|PoolError::Storage(e.to_string()))?;
            boss_events::outbox::record_event_in_tx(&mut tx,&fact).await.map_err(PoolError::Storage)?;
            Ok(true)
        }.await;
        match result {
            Ok(changed) => {
                tx.commit().await.map_err(|e| {
                    PoolError::Storage(format!(
                        "pool COMMIT acknowledgement failed; effect uncertain: {e}"
                    ))
                })?;
                Ok(if changed {
                    spec
                } else {
                    expected.cloned().ok_or_else(|| {
                        PoolError::Storage("unchanged roster lacks expected declaration".into())
                    })?
                })
            }
            Err(error) => match tx.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(PoolError::Storage(format!(
                    "{error}; rollback acknowledgement failed: {rollback}"
                ))),
            },
        }
    }
}

/// A stored row is read by every build that might run against it, and an
/// authored row only by the build that ships it (backlog fb893bba).
#[cfg(test)]
mod unknown_key_tests {
    use super::*;

    /// A stock row published by a later build carries a field this one
    /// does not know (as 5ece507d's `reclaimer_kind` was, to the builds
    /// before it). A build that old is the one a watchdog rollback lands
    /// on, and it must still read resource_pools.
    #[test]
    fn a_stored_pool_row_carrying_a_key_this_build_does_not_know_still_reads() {
        let stock = r#"{"name":"disk:ci-workspaces","version":2,"status":"active",
            "unit":"GiB","kind":"stock","capacity":{"state":"unknown"},
            "reclaimer":"infra/forge/reap-dead-ci-jobs.sh",
            "reclaimer_kind":"maintenance-reap-ci-jobs","a_later_field":"later",
            "department":null,
            "created_at":"2026-11-05T12:00:00Z"}"#;
        let read = crate::resource_history::pool(stock);
        assert!(read.is_ok(), "a newer build's stock row refused: {read:?}");
        let read = read.unwrap();
        assert_eq!(
            read.reclaimer.as_deref(),
            Some("infra/forge/reap-dead-ci-jobs.sh")
        );
        read.validate().expect("the known fields still validate");

        let roster = r#"{"name":"people:fixture-role","version":3,"status":"active",
            "unit":"people","kind":"flow","capacity":{"state":"unknown"},
            "roster":{"eligibility":[{"attribute":"role","codes":["fixture-role"]}],
                "basis":{"revision":4,"count":2,"as_of":"2026-11-05T12:00:00Z",
                    "observed_by":"a-later-field"},
                "unknown_reason":"people registry unavailable",
                "unknown_since":"a-later-field"},
            "reclaimer":null,"department":null,"created_at":"2026-11-05T12:00:00Z"}"#;
        let read = crate::resource_history::pool(roster);
        assert!(read.is_ok(), "a newer build's roster row refused: {read:?}");
        let basis = read.unwrap().roster.and_then(|r| r.basis);
        assert_eq!(basis.and_then(|b| b.revision), Some(4));
    }

    /// The keys read past are named, at any depth, and only those: a known
    /// key, a null, and an Option the spec skips serializing are not.
    #[test]
    fn the_keys_a_stored_row_was_read_past_are_named_by_path() {
        let given = serde_json::json!({"name":"people:fixture-role","version":3,
            "status":"active","unit":"people","kind":"flow","capacity":{"state":"unknown"},
            "roster":{"eligibility":[{"attribute":"role","codes":["fixture-role"]}],
                "basis":{"revision":4,"count":2,"observed_by":"later"},
                "unknown_reason":"unavailable","unknown_since":"later"},
            "program":null,"a_later_field":"later","created_at":"2026-11-05T12:00:00Z"});
        let spec: PoolSpec = serde_json::from_value(given.clone()).expect("tolerant read");
        let read = serde_json::to_value(&spec).expect("wire");
        let mut named = unknown_keys(&given, &read);
        named.sort();
        assert_eq!(
            named,
            [
                "a_later_field",
                "roster.basis.observed_by",
                "roster.unknown_since"
            ]
        );
        assert!(unknown_keys(&read, &read).is_empty());
    }

    /// An authored file is read only by the build it ships in, so a key that
    /// build does not know is a typo, at the top level or nested.
    #[test]
    fn an_authored_pool_file_naming_a_key_this_build_does_not_know_refuses() {
        let directory = tempfile::tempdir().expect("own temp dir");
        let file = directory.path().join("fixture.toml");
        let stock = "[[pool]]\nname='disk:fixture'\nversion=1\nstatus='active'\nunit='GiB'\n\
                     kind='stock'\nreclaimer='infra/forge/reap-dead-ci-jobs.sh'\n\
                     capacity={state='unknown'}\n";
        std::fs::write(&file, stock).expect("write fixture");
        load_pools(&file).expect("the control row loads");
        for (typo, key) in [
            ("reclaimr='infra/forge/disk-floor-sweep.sh'\n", "reclaimr"),
            ("progam='infra/forge/x.sh'\n", "progam"),
        ] {
            std::fs::write(&file, format!("{stock}{typo}")).expect("write fixture");
            let error = load_pools(&file)
                .expect_err("a typo must refuse")
                .to_string();
            assert!(error.contains(key), "refusal must name `{key}`: {error}");
        }
        let roster = "[[pool]]\nname='people:fixture-role'\nversion=1\nstatus='active'\n\
                      unit='people'\nkind='flow'\ncapacity={state='unknown'}\n\
                      [pool.roster]\neligibility=[{attribute='role',codes=['fixture-role']}]\n";
        std::fs::write(&file, roster).expect("write fixture");
        load_pools(&file).expect("the control roster row loads");
        std::fs::write(&file, format!("{roster}unknown_reasn='typo'\n")).expect("write fixture");
        let error = load_pools(&file)
            .expect_err("a nested typo must refuse")
            .to_string();
        assert!(error.contains("unknown_reasn"), "{error}");
    }

    /// Pass item 3 of design ccfe87ff (backlog 56efd9c2): a pool row may
    /// name the station it serves. The name is a key this build knows, it
    /// travels on the row's wire (the published fact, and so /api/claims),
    /// and a blank one is refused rather than read as a station.
    #[test]
    fn an_authored_pool_row_may_name_the_station_it_serves() {
        let directory = tempfile::tempdir().expect("own temp dir");
        let file = directory.path().join("fixture.toml");
        let stock = "[[pool]]\nname='disk:fixture'\nversion=1\nstatus='active'\nunit='GiB'\n\
                     kind='stock'\nreclaimer='infra/forge/reap-dead-ci-jobs.sh'\n\
                     capacity={state='unknown'}\n";
        std::fs::write(&file, format!("{stock}station='deploy'\n")).expect("write fixture");
        let pools = load_pools(&file).expect("a pool row may name its station");
        let wire = serde_json::to_value(&pools[0]).expect("wire");
        assert_eq!(wire["station"], "deploy", "{wire}");
        std::fs::write(&file, stock).expect("write fixture");
        let wire = serde_json::to_value(&load_pools(&file).expect("control")[0]).expect("wire");
        assert!(
            wire.get("station").is_none(),
            "an unnamed station is absent: {wire}"
        );
        std::fs::write(&file, format!("{stock}station='  '\n")).expect("write fixture");
        let error = load_pools(&file)
            .expect_err("a blank station must refuse")
            .to_string();
        assert!(error.contains("station"), "{error}");
    }

    /// Backlog 42cd9a83: a pool row may name the station that KEEPS it —
    /// the mandate to keep the box, which is not the work that consumes
    /// it (`station`) and not the capacity a station owns (an allocation;
    /// station-tree.md section 3). The key travels on the row's wire for
    /// the station page to name, and a blank one is refused rather than
    /// read as a keeper.
    #[test]
    fn an_authored_pool_row_may_name_the_station_that_keeps_it() {
        let directory = tempfile::tempdir().expect("own temp dir");
        let file = directory.path().join("fixture.toml");
        let flow = "[[pool]]\nname='host:fixture'\nversion=1\nstatus='active'\nunit='vCPU'\n\
                    kind='flow'\ncapacity={state='configured',amount=4}\n";
        std::fs::write(&file, format!("{flow}keeper='operations'\n")).expect("write fixture");
        let pools = load_pools(&file).expect("a pool row may name its keeper");
        assert_eq!(pools[0].keeper.as_deref(), Some("operations"));
        assert_eq!(pools[0].station, None, "keeping is not serving");
        let wire = serde_json::to_value(&pools[0]).expect("wire");
        assert_eq!(wire["keeper"], "operations", "{wire}");
        std::fs::write(&file, flow).expect("write fixture");
        let wire = serde_json::to_value(&load_pools(&file).expect("control")[0]).expect("wire");
        assert!(
            wire.get("keeper").is_none(),
            "an unnamed keeper is absent: {wire}"
        );
        for blank in ["''", "' operations'"] {
            std::fs::write(&file, format!("{flow}keeper={blank}\n")).expect("write fixture");
            let error = load_pools(&file)
                .expect_err("a blank or padded keeper must refuse")
                .to_string();
            assert!(error.contains("keeper"), "{error}");
        }
    }

    /// T5 (backlog 84f570a1): a row says where its capacity number comes
    /// from, the key travels on the wire for the console to show, and a
    /// blank one is refused rather than shown as a source.
    #[test]
    fn an_authored_pool_row_says_where_its_capacity_comes_from() {
        let directory = tempfile::tempdir().expect("own temp dir");
        let file = directory.path().join("fixture.toml");
        let flow = "[[pool]]\nname='robots:fixture'\nversion=1\nstatus='active'\nunit='slot'\n\
                    kind='flow'\ncapacity={state='unknown'}\n";
        std::fs::write(
            &file,
            format!("{flow}capacity_from='the agents registry'\n"),
        )
        .expect("write fixture");
        let wire =
            serde_json::to_value(&load_pools(&file).expect("a sourced row")[0]).expect("wire");
        assert_eq!(wire["capacity_from"], "the agents registry", "{wire}");
        std::fs::write(&file, format!("{flow}capacity_from=' '\n")).expect("write fixture");
        let error = load_pools(&file)
            .expect_err("a blank source must refuse")
            .to_string();
        assert!(error.contains("capacity_from"), "{error}");
    }
}
