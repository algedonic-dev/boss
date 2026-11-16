//! The claim door's budget reservation — design c87fb59b car 3
//! (backlog cb78818d).
//!
//! Car 1 put a `budget_usd` on the step's agent block and projected it
//! onto the packet as `agent_budget_usd`. `agent_runs` judges a run
//! against its actor's `hourly_budget_usd_micros` AT FINISH — a
//! refusal there is a fact the desk can act on, not a gate that stopped
//! the spend (that module's own "honest limit"). This is the gate. At
//! the claim, before the CAS, the door RESERVES the block's budget
//! against the actor's hour: the priced spend of the runs that
//! finished in the last hour ([`spent_in`]) plus what this step may
//! spend must fit under the row's cap, else the claim is refused with
//! the three numbers and the step stays in its queue.
//!
//! THE MONEY HALF IS A READING NOW (backlog e6b2066f, 2026-09-23). The
//! spend it measured was a run's FINAL context size priced at a blend —
//! about a fifth of what the run consumed. Priced from the transcript's
//! four counts, the same hour reads about five times higher, and the
//! claims it would then refuse are the building David's direction says
//! a budget must not limit. So an over-cap reservation admits the claim
//! and puts [`CLAIM_OVER_BUDGET`] on the log beside it, carrying the
//! same numbers the 409 did: a cost signal a protocol can read, not a
//! gate.
//!
//! `spent + budget <= cap` and not `spent >= cap`
//! ([`boss_core::agent::BudgetDecision::decide`]): the recorder judges
//! a run that already happened, so it asks whether the hour was
//! already exhausted; the door judges a run that has not started, so it
//! asks whether the hour has room for it. Two questions, two rules —
//! both answered as a [`BudgetDecision`] value so the refusal reads the
//! same way in both places.
//!
//! THE CONCURRENCY HALF IS RETIRED (backlog 3e927996; David, design
//! a3c5e58a Q3). It counted an actor's open `agent-run` packets
//! against its row's `agents.max_concurrent_runs` (backlog 57c108c2)
//! — the bound the money reading structurally cannot be. The station
//! tree carries it now: a run's `building` claim reserves a slot of
//! its `robots:<model>` pool, so the pool is the one bound on how many
//! runs a robot has going, and the column is a stored declaration
//! nothing judges.
//!
//! What it deliberately is NOT: a person's gate. A person has no
//! agents row and runs no model; the budget is the agent's, and a
//! person claiming an agent-blocked step is a person doing the work by
//! hand, which the protocol allows (`executor_for`'s "the human always
//! able to claim"). A row with no cap declared reserves nothing — an
//! unbudgeted agent is admitted with nothing to count down, the
//! boot-guard rule every cap reader here follows.

use std::sync::Arc;

use boss_core::agent::BudgetDecision;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::agent_runs::{ADMISSION_WINDOW, AgentRun, AgentRunLog, RunFilter};
use crate::agents::{AgentRow, AgentsRegistry};

/// Dollars to the micro-USD the caps are declared in. Half up, so a
/// budget of $0.0000005 is one micro, not none.
pub fn usd_to_micros(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * 1_000_000.0).round() as u64
}

/// The rule. `None` cap admits everything; otherwise the reservation
/// must fit: `spent + budget <= cap`.
pub fn reserve(
    cap_usd_micros: Option<u64>,
    spent_usd_micros: u64,
    budget_usd_micros: u64,
) -> BudgetDecision {
    let Some(cap) = cap_usd_micros else {
        return BudgetDecision::Allow {
            remaining_usd_micros: None,
        };
    };
    let asked = spent_usd_micros.saturating_add(budget_usd_micros);
    if asked > cap {
        return BudgetDecision::Deny {
            reason: format!(
                "hourly budget would be exceeded: {spent_usd_micros} usd_micros spent in the \
                 last hour + {budget_usd_micros} this step may spend = {asked}, over the cap \
                 of {cap}"
            ),
        };
    }
    BudgetDecision::Allow {
        remaining_usd_micros: Some(cap - asked),
    }
}

/// The hour-window spend: the priced cost of every run in `runs` — the
/// adapter has already narrowed them to the actor and the window. An
/// unpriced run adds nothing: it cannot add a number it does not have,
/// the same rule `measure_load` applies. Since design 91a9bfe7 a
/// total-only run on a model that declares a blend IS priced and DOES
/// count here — which is the point of that change: the desks were
/// enforcing against 2% of the spend.
pub fn spent_in(runs: &[AgentRun]) -> u64 {
    runs.iter()
        .filter_map(|r| r.usd_micros)
        .fold(0u64, u64::saturating_add)
}

/// What the door measured and decided, for the record either way.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reservation {
    pub actor: String,
    pub spent_usd_micros: u64,
    pub budget_usd_micros: u64,
    pub hourly_budget_usd_micros: Option<u64>,
    pub window_from: DateTime<Utc>,
    pub decision: BudgetDecision,
}

/// The reading an over-budget claim leaves on the log beside the claim
/// itself (backlog e6b2066f) — the payload that was a 409 body until
/// the door stopped refusing.
pub const CLAIM_OVER_BUDGET: &str = "agents.claim.over_budget";

impl Reservation {
    /// The [`CLAIM_OVER_BUDGET`] payload: every number the reading rests
    /// on, so the reader never re-derives what the door already
    /// measured. It was the 409 body until backlog e6b2066f turned the
    /// refusal into a reading.
    pub fn reading_body(&self) -> serde_json::Value {
        serde_json::json!({
            "reading": "claim over the agent's hourly budget",
            "actor": self.actor,
            "spent_usd_micros": self.spent_usd_micros,
            "budget_usd_micros": self.budget_usd_micros,
            "hourly_budget_usd_micros": self.hourly_budget_usd_micros,
            "window_from": self.window_from,
            "reason": match &self.decision {
                BudgetDecision::Deny { reason } => reason.clone(),
                BudgetDecision::Allow { .. } => String::new(),
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BudgetDoorError {
    #[error("agents registry: {0}")]
    Agents(#[from] crate::agents::AgentsError),
    #[error("agent runs: {0}")]
    Runs(#[from] crate::agent_runs::AgentRunError),
}

/// The door's two reads: the actor's row (its cap, its model — the
/// model is what the station capability compares) and the actor's
/// runs in the window.
pub struct BudgetDoor {
    pub agents: Arc<dyn AgentsRegistry>,
    pub runs: Arc<dyn AgentRunLog>,
}

impl BudgetDoor {
    /// The agents row a CALLER's spelling names: the row with that id,
    /// else the one row listing it among its aliases (a login that signs
    /// as the agent). Two rows claiming one login name nobody, as the
    /// CLI's `resolve_agent` already rules (review f7f0b689, N3). It
    /// answers which spellings are the caller itself (backlog 5487223c)
    /// and, at the claim door, whose row a holder is: an id-only reader
    /// beside it trusted the binary's login door to have rewritten an
    /// alias first, and a router served without that layer recorded a
    /// robot's login as a person (backlog 43ca3b3a), so it was deleted.
    pub async fn caller_row(&self, spelling: &str) -> Result<Option<AgentRow>, BudgetDoorError> {
        let rows = self.agents.list().await?;
        if let Some(row) = rows.iter().find(|a| a.id == spelling) {
            return Ok(Some(row.clone()));
        }
        let mut named = rows
            .into_iter()
            .filter(|a| a.aliases.iter().any(|l| l == spelling));
        Ok(match (named.next(), named.next()) {
            (Some(row), None) => Some(row),
            _ => None,
        })
    }

    /// Reserve `budget_usd` of `row`'s hour, measured at `now`.
    pub async fn reserve(
        &self,
        row: &AgentRow,
        budget_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<Reservation, BudgetDoorError> {
        self.reserve_with_inputs(row, budget_usd, now)
            .await
            .map(|(reading, _)| reading)
    }

    /// The historical advisory reading and the exact rows its query returned.
    /// This is evidence, not a stronger authorization or a new hard cap.
    pub async fn reserve_with_inputs(
        &self,
        row: &AgentRow,
        budget_usd: f64,
        now: DateTime<Utc>,
    ) -> Result<(Reservation, Vec<AgentRun>), BudgetDoorError> {
        let window_from = ADMISSION_WINDOW.cutoff(now);
        let runs = self
            .runs
            .list_runs(&RunFilter {
                actor_id: Some(row.id.clone()),
                since: Some(window_from),
                ..RunFilter::default()
            })
            .await?;
        let spent_usd_micros = spent_in(&runs);
        let budget_usd_micros = usd_to_micros(budget_usd);
        let cap = row
            .hourly_budget_usd_micros
            .and_then(|n| u64::try_from(n).ok());
        Ok((
            Reservation {
                actor: row.id.clone(),
                spent_usd_micros,
                budget_usd_micros,
                hourly_budget_usd_micros: cap,
                window_from,
                decision: reserve(cap, spent_usd_micros, budget_usd_micros),
            },
            runs,
        ))
    }
}

/// The key an `agent-run` packet spells its actor under — written by
/// `boss dispatch` when it opens the run.
pub const RUN_AGENT_KEY: &str = "agent";

/// The workflow kind a run packet is filed under.
pub const RUN_KIND: &str = "agent-run";

/// The key a run's `building` step carries its worker's start bound
/// under — `{ schema = 1, minutes = N }`, a `metadata_defaults` of
/// agent-run.toml (train #956). `boss dispatch --started` reads it to
/// know a receipt is owed, and the top board to know how long a run may
/// stand briefed before it reads as never started (backlog 57326714).
pub const WORKER_START_EXPECTATION: &str = "worker_start_expectation";

/// Schema 2 is a launcher observation, never a worker's self-claim. Only
/// newly admitted runs opt in with an exact observer and input digests.
pub fn observer_start_receipt_matches(
    expected: &serde_json::Value,
    receipt: &serde_json::Value,
    run: &str,
    actor: &str,
) -> bool {
    use serde_json::Value;
    let nonempty = |value: &Value| {
        value
            .as_str()
            .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
    };
    let digest = |value: &Value| {
        value.as_str().is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    let keys = receipt.as_object().is_some_and(|m| {
        m.len() == 8
            && [
                "schema",
                "run",
                "actor",
                "observer",
                "prompt_sha256",
                "snapshot_sha256",
                "binding",
                "acknowledgment",
            ]
            .iter()
            .all(|k| m.contains_key(*k))
    });
    let binding = &receipt["binding"];
    let ack = &receipt["acknowledgment"];
    observer_start_expectation_valid(expected, actor)
        && receipt["schema"] == 2
        && keys
        && expected["actor"] == actor
        && receipt["actor"] == actor
        && receipt["run"] == run
        && nonempty(&expected["observer"])
        && receipt["observer"] == expected["observer"]
        && expected["observer"] != actor
        && ["prompt_sha256", "snapshot_sha256"]
            .iter()
            .all(|k| digest(&expected[k]) && receipt[k] == expected[k])
        && binding.as_object().is_some_and(|b| {
            (3..=4).contains(&b.len())
                && b.keys()
                    .all(|k| ["job_uid", "pod", "pod_uid", "container_id"].contains(&k.as_str()))
        })
        && ["job_uid", "pod_uid", "container_id"]
            .iter()
            .all(|k| nonempty(&binding[k]))
        && ack.as_object().is_some_and(|a| a.len() == 5)
        && ack["schema"] == 1
        && ack["start_read"] == true
        && ack["run"] == run
        && ack["prompt_sha256"] == expected["prompt_sha256"]
        && ack["snapshot_sha256"] == expected["snapshot_sha256"]
}

pub fn observer_start_expectation_valid(expected: &serde_json::Value, actor: &str) -> bool {
    let text = |key| {
        expected[key]
            .as_str()
            .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
    };
    expected.as_object().is_some_and(|m| m.len() == 6)
        && expected["schema"] == 2
        && expected["minutes"].as_i64().is_some_and(|n| n > 0)
        && expected["actor"] == actor
        && text("observer")
        && expected["observer"] != actor
        && ["prompt_sha256", "snapshot_sha256"].iter().all(|k| {
            expected[k].as_str().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reservation_must_fit_under_the_cap_and_no_cap_reserves_nothing() {
        assert!(reserve(None, u64::MAX, u64::MAX).is_allowed());
        // At the cap exactly is admitted: <=, not <.
        assert_eq!(
            reserve(Some(3_000_000), 1_000_000, 2_000_000),
            BudgetDecision::Allow {
                remaining_usd_micros: Some(0)
            }
        );
        let BudgetDecision::Deny { reason } = reserve(Some(3_000_000), 1_000_870, 5_000_000) else {
            panic!("over the cap is a refusal")
        };
        assert!(reason.contains("1000870"), "{reason}");
        assert!(reason.contains("5000000"), "{reason}");
        assert!(reason.contains("3000000"), "{reason}");
        // A cap of zero refuses every positive reservation — declared,
        // not absent.
        assert!(!reserve(Some(0), 0, 1).is_allowed());
        assert!(reserve(Some(0), 0, 0).is_allowed());
        // Overflow saturates rather than wrapping into an admission.
        assert!(!reserve(Some(u64::MAX - 1), u64::MAX, 1).is_allowed());
    }

    #[test]
    fn dollars_become_micros_half_up_and_nothing_below_zero() {
        assert_eq!(usd_to_micros(5.0), 5_000_000);
        assert_eq!(usd_to_micros(0.0000005), 1);
        assert_eq!(usd_to_micros(0.0000004), 0);
        assert_eq!(usd_to_micros(-1.0), 0);
        assert_eq!(usd_to_micros(f64::NAN), 0);
    }

    #[test]
    fn the_reading_carries_every_number() {
        let r = Reservation {
            actor: "agent-claude".into(),
            spent_usd_micros: 1_000_870,
            budget_usd_micros: 5_000_000,
            hourly_budget_usd_micros: Some(3_000_000),
            window_from: "2026-09-18T18:00:00Z".parse().unwrap(),
            decision: reserve(Some(3_000_000), 1_000_870, 5_000_000),
        };
        let body = r.reading_body();
        assert_eq!(body["reading"], "claim over the agent's hourly budget");
        assert_eq!(body["actor"], "agent-claude");
        assert_eq!(body["spent_usd_micros"], 1_000_870);
        assert_eq!(body["budget_usd_micros"], 5_000_000);
        assert_eq!(body["hourly_budget_usd_micros"], 3_000_000);
        assert_eq!(body["window_from"], "2026-09-18T18:00:00Z");
        assert!(body["reason"].as_str().unwrap().contains("over the cap"));
    }
}
