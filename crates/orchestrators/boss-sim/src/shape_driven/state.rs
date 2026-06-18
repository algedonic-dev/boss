//! `ShapeDrivenState` — the in-memory bookkeeping the day-loop engine
//! carries forward. Tracks the active Subject pool, RNG-shock windows,
//! per-tenant run counters, and a roster reference — but NO job/step
//! state. The live system owns jobs + steps; the sim posts them and the
//! workforce executor reads them back through the public API.
//!
//! Tenant-neutral: every entity it tracks is keyed by
//! `(subject_kind, id)`, with no domain types embedded.

use std::collections::HashMap;

use chrono::NaiveDate;

/// In-memory state for a shape-driven sim run. Tracks only what the
/// sim *generates* — the Subject pool, RNG-shock windows, run counters,
/// and a roster reference. It holds NO job/step/inventory mirror: the
/// live system owns that state; the sim posts jobs and the workforce
/// executor reads jobs/steps/inventory back through the public API.
#[derive(Debug, Default)]
pub struct ShapeDrivenState {
    /// Pool of Subjects available to be picked as Job targets.
    /// Keyed by SubjectKind slug ("account", "vendor", "location",
    /// …). Each entry is a list of Subject ids currently active for
    /// that kind. Populated by the seed loader; grown by Subject birth.
    pub subjects: HashMap<String, Vec<String>>,
    /// Day-by-day counters used by the run summary.
    pub counters: RunCounters,
    /// Random shocks that are currently in their active window.
    /// Engine rolls fresh shocks at quarter starts (probabilistic);
    /// expired shocks (end_date < day) get pruned at the top of
    /// `simulate_day`. Multipliers stack — two concurrent shocks on
    /// the same JobKind multiply together, which is correct for
    /// "supply shortage hits during a viral-bar surge."
    pub active_shocks: Vec<ActiveShock>,
    /// Employee-id pool keyed by role slug, populated at init from the
    /// tenant roster. A roster reference only — step assignment +
    /// completion happen in the workforce executor, which resolves
    /// employees from the live people-api.
    pub employees_by_role: HashMap<String, Vec<String>>,
}

/// A shock that has rolled into its active window. Carried in
/// `ShapeDrivenState` between ticks so the multiplier persists for
/// the configured duration without re-rolling each day.
#[derive(Debug, Clone)]
pub struct ActiveShock {
    /// Authoring name (`"viral-bar"`, `"supply-shortage"`). Used
    /// for logs + the per-shock counter; not load-bearing for the
    /// rate math.
    pub name: String,
    /// What this shock multiplies. See `ShockTarget` for the
    /// supported shapes.
    pub target: crate::shape_driven::tenant::ShockTarget,
    /// Multiplier applied to the matched rate(s). Values <1 dampen,
    /// >1 surge. Stacks with weekday/month multipliers.
    pub rate_multiplier: f64,
    /// Last day (inclusive) the shock applies. Pruned the first
    /// time `simulate_day` runs with `today > end_date`.
    pub end_date: NaiveDate,
}

/// Cumulative counters for a sim run.
#[derive(Debug, Default, Clone)]
pub struct RunCounters {
    pub days_simulated: u64,
    pub jobs_created: u64,
    pub jobs_skipped_no_subject: u64,
    pub jobs_skipped_unknown_kind: u64,
    /// `open_job_from_request` got an open_job payload missing
    /// one of the required fields (job_kind, subject_kind,
    /// subject_id). Each fire is logged at WARN with the missing
    /// field + the offending payload so a misconfigured
    /// `[periodic.*]` open_job spec (e.g. one missing
    /// `action.subject_kind` / `action.subject_id`) can't silently
    /// drop every fire.
    pub jobs_skipped_missing_field: u64,
    pub steps_completed: u64,
    pub jobs_closed: u64,
    pub subjects_born: u64,
    pub anomalies_fired: u64,
    pub triggered_jobs_spawned: u64,
    /// Per-JobKind creation count, for spot-checking that rates
    /// roughly track the tenant config.
    pub jobs_created_by_kind: HashMap<String, u64>,
    /// Per-SubjectKind birth count.
    pub subjects_born_by_kind: HashMap<String, u64>,
    /// Counter for synthesizing unique Subject ids — used by the
    /// shape-driven engine when birthing new Subjects from
    /// tenant.subject_rates. Keyed by SubjectKind so each kind has
    /// its own monotonic id sequence.
    pub subject_id_counter: HashMap<String, u64>,
    /// Per-anomaly-name fire count, keyed as
    /// `"<jobkind>:<anomaly-name>"` so the same anomaly across
    /// JobKinds doesn't collide.
    pub anomalies_fired_by_kind: HashMap<String, u64>,
}

/// One day's slice of activity. Returned by `engine::simulate_day`.
#[derive(Debug, Default, Clone)]
pub struct DaySummary {
    pub day: Option<NaiveDate>,
    pub jobs_created: u64,
    pub jobs_skipped_no_subject: u64,
    pub jobs_skipped_unknown_kind: u64,
    pub steps_completed: u64,
    pub jobs_closed: u64,
    pub subjects_born: u64,
    pub anomalies_fired: u64,
    pub triggered_jobs_spawned: u64,
    /// Per-JobKind count for this day.
    pub by_kind: HashMap<String, u64>,
    /// Side-effect handler failures captured during this day. Each
    /// entry is `(handler_name, error_summary)`. Empty in the happy
    /// path. The engine binary's `--hard-fail` mode aborts when this
    /// is non-empty so a missing-metadata side effect surfaces at the
    /// failing step instead of 30 sim-days later, when a downstream
    /// counterparty PUTs against an invoice/account/etc that was
    /// never created.
    pub side_effect_failures: Vec<(String, String)>,
}

impl ShapeDrivenState {
    /// Construct an empty state. Callers add Subjects via
    /// `seed_subjects` before the first `simulate_day` so JobKinds
    /// targeting "account" / "vendor" / etc. have something to
    /// attach to.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one Subject under its kind. Idempotent — duplicates
    /// are de-duplicated.
    pub fn seed_subject(&mut self, kind: &str, id: &str) {
        let bucket = self.subjects.entry(kind.to_string()).or_default();
        if !bucket.iter().any(|e| e == id) {
            bucket.push(id.to_string());
        }
    }

    /// Bulk seed.
    pub fn seed_subjects(&mut self, kind: &str, ids: impl IntoIterator<Item = impl Into<String>>) {
        for id in ids {
            self.seed_subject(kind, &id.into());
        }
    }

    /// Count of active Subjects for a given kind.
    pub fn subject_count(&self, kind: &str) -> usize {
        self.subjects.get(kind).map(|v| v.len()).unwrap_or(0)
    }

    /// Register one Employee in the role-keyed pool. Idempotent
    /// per (role, id) pair. The tenant engine seeds this from its
    /// roster so the engine's actor stamping picks from the same
    /// roster the people projection holds.
    pub fn register_employee(&mut self, role: &str, id: &str) {
        let bucket = self.employees_by_role.entry(role.to_string()).or_default();
        if !bucket.iter().any(|e| e == id) {
            bucket.push(id.to_string());
        }
    }

    /// Pick one Employee id from the role pool. Returns None when
    /// the role has no registered Employees — caller falls back to
    /// the generic automation slug. RNG-keyed for replay
    /// determinism.
    pub fn pick_employee_for_role(&self, role: &str, rng: &mut crate::rng::Rng) -> Option<String> {
        let pool = self.employees_by_role.get(role)?;
        if pool.is_empty() {
            return None;
        }
        let pick = (rng.next() * pool.len() as f64) as usize;
        Some(pool[pick.min(pool.len() - 1)].clone())
    }

    /// Fallback for step kinds whose StepType doesn't declare a
    /// `required_roles`. Pick uniformly from the union of all
    /// registered Employees so every step transition still
    /// attributes to a real person; without this, the
    /// 33-of-38-StepTypes that have empty roles would all stamp
    /// the generic `brewery-sim` actor.
    pub fn pick_any_employee(&self, rng: &mut crate::rng::Rng) -> Option<String> {
        let mut all: Vec<&String> = self.employees_by_role.values().flatten().collect();
        if all.is_empty() {
            return None;
        }
        // Sort for replay determinism — HashMap iteration order
        // varies across runs.
        all.sort();
        let pick = (rng.next() * all.len() as f64) as usize;
        Some(all[pick.min(all.len() - 1)].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_subject_dedupes() {
        let mut state = ShapeDrivenState::new();
        state.seed_subject("vendor", "vnd-001");
        state.seed_subject("vendor", "vnd-001");
        state.seed_subject("vendor", "vnd-002");
        assert_eq!(state.subject_count("vendor"), 2);
    }

    #[test]
    fn bulk_seed_partitions_by_kind() {
        let mut state = ShapeDrivenState::new();
        state.seed_subjects("location", ["loc-hq", "loc-warehouse"]);
        state.seed_subjects("vendor", ["vnd-001"]);
        assert_eq!(state.subject_count("location"), 2);
        assert_eq!(state.subject_count("vendor"), 1);
        assert_eq!(state.subject_count("account"), 0);
    }
}
