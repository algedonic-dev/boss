//! Wire types for the cadence surface.
//!
//! `CadenceRuleRow` is deliberately the RAW registry row — nullable
//! basis-specific columns and all — not a parsed rule. The conductor
//! owns the parse because it owns the consequence: a malformed row is
//! skipped loudly in the conductor's journal every tick, which is
//! where an operator reads it. An API that parsed and 500'd would turn
//! one bad registry row into a dead cadence loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Does a cadence verb put a train on the track? `board` assembles and
/// departs one; `run` is reconcile-then-board. Exactly these — a
/// reconcile or a packet-open never needs a clear track.
///
/// ONE DEFINITION (CLAUDE.md §9a). The conductor decides serialization
/// and idle-firing outcomes on it, and the yard decides whether a
/// `basis=clock` row is a BOARDING trigger worth describing to the
/// operator ("Boards at 06:05 / 18:05 UTC") on it. It lived only in the
/// conductor until 634a475b, so the yard selected clock rows by basis
/// alone and would have named a clock row with any other verb as a
/// boarding window — latent while the only live clock row is
/// `train-window` with verb `run`. Tier 1 owns it because Tier 1 cannot
/// read the orchestrator, and both readers can read here.
pub fn departs_a_train(verb: &str) -> bool {
    matches!(verb, "board" | "run")
}

/// The `boss train` verbs a rule may fire — the conductor's allowlist
/// (`boss-cli`'s cadence loop spawns nothing else) and the literal half
/// of the table's `cadence_rules_verb_check` (20260925200737), spelled
/// once. The other half is [`OPEN_VERB_PREFIX`].
pub const CONDUCTOR_VERBS: [&str; 5] = ["preflight", "reconcile", "board", "run", "refresh"];

/// A rule may instead open a packet of a workflow kind: `open:<kind>`,
/// the kind in kebab-case — the table's `verb ~ '^open:[a-z0-9-]+$'`.
pub const OPEN_VERB_PREFIX: &str = "open:";

/// Every basis, the columns it REQUIRES, and the columns it MAY carry
/// beyond those — `cadence_rules_basis_check` and
/// `cadence_rules_params_check` (202608282135) spelled once. A column in
/// neither list must be absent for that basis; an INT column present
/// must be above 0; and a calendar rule's `at_times` holds exactly one
/// time-of-day (the cadence chooses the days, the time when on them).
pub const BASIS_COLUMNS: [(&str, &[&str], &[&str]); 4] = [
    ("wall", &["every_minutes"], &[]),
    ("clock", &["at_times"], &[]),
    ("queue-depth", &["min_dock_depth", "cooldown_minutes"], &[]),
    (
        "calendar",
        &["cadence", "anchor_date", "at_times"],
        &["business_calendar"],
    ),
];

/// Would `cadence_rules` admit this row? The table's CHECKs on the verb,
/// the basis and each basis's parameter group, as one Rust check, so a
/// declaration is refused the same way over either adapter.
///
/// WHY IT EXISTS (backlog be459ab9, found by the adapters-agree suite,
/// 2026-09-29). Postgres enforced the CHECKs and `InMemoryCadence`
/// enforced none, so a publish the database refused as a 500 naming a
/// constraint landed in the double and answered 200 — every door test
/// of `POST /api/cadence/rules/{name}/publish` runs on the double. Both
/// adapters now run this before writing and answer
/// `CadenceError::BadRequest` naming the rule and the column.
/// `the_rule_check_is_the_tables_check` (in
/// `the_adapters_agree_on_the_cadence_registry_pg.rs`) holds it to the
/// live table BOTH ways: the verb and basis lists equal the constraint's
/// own, and a corpus of rows gets the same verdict from each.
///
/// `cadence_rules_regate_hold_check` has no half here: the row lost
/// `regate_hold_minutes` in backlog d1d4275d, so no adapter writes it.
/// The conductor's parse (`boss-cli` `rule_from_row`) is stricter still
/// — it reads the times and refuses a business calendar it cannot
/// resolve — and is the conductor's own judgement, not the registry's.
pub fn check_rule(row: &CadenceRuleRow) -> Result<(), String> {
    let name = &row.name;
    let verb = row.verb.as_str();
    let is_kind = |k: &str| {
        !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    let verb_ok =
        CONDUCTOR_VERBS.contains(&verb) || verb.strip_prefix(OPEN_VERB_PREFIX).is_some_and(is_kind);
    if !verb_ok {
        return Err(format!(
            "cadence rule {name}: verb `{verb}` is neither a conductor verb ({}) nor \
             {OPEN_VERB_PREFIX}<kind> with the kind in kebab-case",
            CONDUCTOR_VERBS.join(" | ")
        ));
    }
    let basis = row.basis.as_str();
    let Some((_, required, may)) = BASIS_COLUMNS.iter().find(|(b, _, _)| *b == basis) else {
        let bases: Vec<&str> = BASIS_COLUMNS.iter().map(|(b, _, _)| *b).collect();
        return Err(format!(
            "cadence rule {name}: basis `{basis}` is not one of {}",
            bases.join(" | ")
        ));
    };
    let int = |v: Option<i32>| (v.is_some(), v);
    let columns: [(&str, (bool, Option<i32>)); 7] = [
        ("every_minutes", int(row.every_minutes)),
        ("at_times", (row.at_times.is_some(), None)),
        ("min_dock_depth", int(row.min_dock_depth)),
        ("cooldown_minutes", int(row.cooldown_minutes)),
        ("cadence", (row.cadence.is_some(), None)),
        ("anchor_date", (row.anchor_date.is_some(), None)),
        ("business_calendar", (row.business_calendar.is_some(), None)),
    ];
    for (column, (present, value)) in columns {
        if required.contains(&column) && !present {
            return Err(format!(
                "cadence rule {name}: {column} is required for basis `{basis}`"
            ));
        }
        if present && !required.contains(&column) && !may.contains(&column) {
            return Err(format!(
                "cadence rule {name}: {column} is not a column of basis `{basis}` — leave it absent"
            ));
        }
        if let Some(v) = value.filter(|v| *v <= 0) {
            return Err(format!(
                "cadence rule {name}: {column} must be above 0, got {v}"
            ));
        }
    }
    let one_time = row
        .at_times
        .as_ref()
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| a.len() == 1);
    if basis == "calendar" && !one_time {
        return Err(format!(
            "cadence rule {name}: at_times holds exactly one time-of-day for basis `calendar` \
             — the cadence chooses the days, at_times when on them"
        ));
    }
    Ok(())
}

/// The `detail` a claim lands with: an object as sent, JSON null as `{}`,
/// and anything else refused — one rule for both adapters.
///
/// WHY (backlog be459ab9, found by the adapters-agree suite,
/// 2026-09-29). `NewFiring::detail` defaults to null on the wire, and
/// Postgres stored that null: `record_outcome`'s `detail || $2` then
/// built `[null, {"rc": 0, …}]`, so the rc just recorded read back as
/// "no outcome yet" for ever — the state evaluation must NOT mistake for
/// a finished run — while the double replaced the null and read it. An
/// outcome merges into an object, and an array or a scalar has no merge
/// both adapters share, so a claim carrying one is a bad request.
pub fn claim_detail(detail: &serde_json::Value) -> Result<serde_json::Value, String> {
    match detail {
        serde_json::Value::Null => Ok(serde_json::json!({})),
        serde_json::Value::Object(_) => Ok(detail.clone()),
        other => Err(format!(
            "a firing's detail is a JSON object (or absent), got {other}"
        )),
    }
}

/// One row of `cadence_rules`, unparsed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadenceRuleRow {
    pub name: String,
    pub verb: String,
    pub basis: String,
    #[serde(default)]
    pub every_minutes: Option<i32>,
    #[serde(default)]
    pub at_times: Option<serde_json::Value>,
    #[serde(default)]
    pub min_dock_depth: Option<i32>,
    #[serde(default)]
    pub cooldown_minutes: Option<i32>,
    /// Calendar basis: which days the rule fires on
    /// (`daily|weekly|monthly|...` — parsed by `boss_core::calendar` in
    /// the conductor, deliberately not here; the row stays raw).
    #[serde(default)]
    pub cadence: Option<String>,
    /// Calendar basis: the date the recurrence is anchored to.
    #[serde(default)]
    pub anchor_date: Option<chrono::NaiveDate>,
    /// Calendar basis: optional business-calendar code; absent means
    /// every day is a business day.
    #[serde(default)]
    pub business_calendar: Option<String>,
}

/// One row of `cadence_rules` as DECLARED — the wire row plus the
/// two columns the conductor never reads (`version`, `status`) and
/// the one the seed stamps (`created_at`). This is what the platform
/// bundle (`infra/platform/cadence/<name>.toml`) declares and what
/// `CadenceRegistry::live_versions` reads back, so the equality pin
/// compares the same shape on both sides. The columns live ONCE, in
/// [`CadenceRuleRow`], flattened here rather than repeated (CLAUDE.md
/// §9a): a basis column added to the row is added to the declaration
/// by construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CadenceRuleSpec {
    pub version: i32,
    pub status: crate::registry::WorkflowStatus,
    #[serde(flatten)]
    pub row: CadenceRuleRow,
    /// When the deployment was built — stamped by the seed's clock on
    /// the row it writes; never part of the declaration. Defaults on
    /// the wire (to the epoch) for the same reason: a publish body
    /// (`POST /api/cadence/rules/{name}/publish`) is a declaration,
    /// and the door stamps its own clock over whatever was sent.
    #[serde(default)]
    pub created_at: DateTime<Utc>,
}

impl CadenceRuleSpec {
    pub fn name(&self) -> &str {
        &self.row.name
    }
}

/// The most recent recorded firing of a rule — what the conductor's
/// evaluation compares a candidate window against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastFiring {
    pub firing_id: String,
    pub fired_at: DateTime<Utc>,
    /// The verb's exit code, once `record_outcome` has merged it into the
    /// firing's `detail`. `None` means no outcome is recorded yet — the run
    /// is still in flight, or it was cut off without one — which is NOT the
    /// same as a failure and must not be read as one. Evaluation needs this
    /// to tell a firing that did its work from one that died on its first
    /// syscall: without it, a board that boarded nothing still consumed its
    /// whole cooldown (2026-09-04, two hours of a threshold-met dock).
    #[serde(default)]
    pub rc: Option<i32>,
    /// What a BOARD firing decided (backlog 96f02540) — merged into its
    /// `detail` with the outcome, `None` for every other verb, for a
    /// firing with no outcome yet, and for one recorded before the field
    /// existed. The yard's boarding hold states it rather than re-deriving
    /// the board's decision from the cadence rows (`yard::boarding_hold`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_decision: Option<crate::board_decision::BoardDecision>,
}

/// A claim request. `fired_at` is supplied BY THE CALLER and bound as
/// a parameter — it is boss-clock time, never the database's
/// wallclock. Sim runs depend on this: a sim-dated conductor tick must
/// record a sim-dated firing, and `NOW()` would silently overwrite it
/// with real time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewFiring {
    pub firing_id: String,
    pub rule_name: String,
    pub verb: String,
    pub basis: String,
    pub fired_at: DateTime<Utc>,
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// Result of a claim. `claimed: false` means the window was already
/// taken — by a concurrent conductor, or by this one before a crash
/// mid-verb. The caller must not run the verb.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClaimResult {
    pub claimed: bool,
}

/// What the verb cost, merged into the firing's `detail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiringOutcome {
    pub rc: i32,
    pub runtime_secs: u64,
    /// A board's decision (backlog 96f02540), read by the cadence loop off
    /// the verb it ran. Absent for every other verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_decision: Option<crate::board_decision::BoardDecision>,
}

impl FiringOutcome {
    /// The object an adapter merges into the firing's `detail` — the one
    /// shape both adapters write, so the decision rides under the key the
    /// readers look for (`board_decision::FIRING_KEY`).
    pub fn detail_patch(&self) -> serde_json::Value {
        let mut patch = serde_json::json!({"rc": self.rc, "runtime_secs": self.runtime_secs});
        if let Some(d) = &self.board_decision {
            patch[crate::board_decision::FIRING_KEY] = serde_json::json!(d);
        }
        patch
    }
}
