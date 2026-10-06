//! In-memory adapter for `AgentsRegistry` — the port-level test double.
//!
//! Mirrors the Pg invariants that matter here: an alias exists only
//! under a registered agent (`actor_aliases.actor_id` is a foreign key
//! into `agents`), so the builder takes the agent and its logins
//! together and there is no way to register a login to nothing; and a
//! publish inserts a row the registry lacks, KEEPS a held row under the
//! default insert-if-absent naming the fields that differ, UPDATES it
//! under take on the declared fields that differ (a declared alias
//! lands under the declared id, an undeclared alias is kept), and names
//! each change in the outcome.
//!
//! And it refuses what the schema refuses, as Postgres does: a batch
//! naming an id that is not `agent-<slug>` or a negative budget or run
//! cap (the table's CHECKs, which run on every declared row, held or
//! not), and — given a rate card (`with_rate_card`) — a model the card
//! does not price on a row the batch would WRITE (the foreign key, which
//! fires only on a written row), each refusing the whole batch before
//! anything lands. Until backlog be459ab9's adapters-agree suite
//! (2026-09-29) the double mirrored none of the three and landed every
//! such batch, so a door test through it measured a registry Postgres
//! would have refused.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use boss_core::event::Event;
use boss_core::publish::PublishMode;
use boss_core::publisher::EventStamp;

use super::automations::{AutomationActor, AutomationsSeedOutcome, classify, validate_all};
use super::port::{
    AgentsError, AgentsRegistry, automation_declared_event, declared_event, updated_event,
};
use super::types::{AgentInput, AgentRow, AgentsBatchOutcome, KeptRow, UpdatedRow, is_agent_id};

#[derive(Default)]
struct Rows {
    /// id -> the row (without its aliases).
    agents: BTreeMap<String, AgentInput>,
    /// login -> registered agent id.
    aliases: BTreeMap<String, String>,
}

impl Rows {
    fn row(&self, a: &AgentInput) -> AgentRow {
        AgentRow {
            id: a.id.clone(),
            display_name: a.display_name.clone(),
            default_model: a.default_model.clone(),
            role: a.role.clone(),
            department: a.department.clone(),
            hourly_budget_usd_micros: a.hourly_budget_usd_micros,
            max_concurrent_runs: a.max_concurrent_runs,
            aliases: self
                .aliases
                .iter()
                .filter(|(_, id)| **id == a.id)
                .map(|(alias, _)| alias.clone())
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct InMemoryAgents {
    rows: Mutex<Rows>,
    /// id -> the automation's row (`automation_actors`), byte-ordered
    /// as the Pg listing's `COLLATE "C"` is.
    automations: Mutex<BTreeMap<String, AutomationActor>>,
    events: Mutex<Vec<Event>>,
    rate_card: Option<BTreeSet<String>>,
}

impl InMemoryAgents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Price exactly `models`, as the `agent_rate_card` rows of a
    /// Postgres registry do: a batch that would write any other
    /// `default_model` is refused as `Unpriced`. Without one the double
    /// prices every model — it holds no card of its own to consult.
    pub fn with_rate_card<'a>(mut self, models: impl IntoIterator<Item = &'a str>) -> Self {
        self.rate_card = Some(models.into_iter().map(str::to_string).collect());
        self
    }

    /// The refusal Postgres makes of `declared`, row by row in the
    /// batch's order and each row's CHECKs before its foreign key, as
    /// the database meets them — or `Ok` when it makes none. Judged
    /// before anything is written, so a refused batch lands nothing, as
    /// its rolled-back transaction does there. The foreign key is
    /// checked only where the model is WRITTEN: an inserted row, or a
    /// take that changes a held row's model — a kept row, or a take that
    /// leaves the model as it was, writes no model to check.
    fn refusal(
        &self,
        held: &Rows,
        declared: &[AgentInput],
        mode: PublishMode,
    ) -> Result<(), AgentsError> {
        // Each id's model as the batch leaves it so far, over the held.
        let mut model: BTreeMap<&str, &str> = BTreeMap::new();
        for a in declared {
            let negative = a.hourly_budget_usd_micros.is_some_and(|n| n < 0)
                || a.max_concurrent_runs.is_some_and(|n| n < 0);
            if !is_agent_id(&a.id) || negative {
                return Err(AgentsError::Storage(format!(
                    "agent {}: a CHECK on the agents table refuses the row",
                    a.id
                )));
            }
            let current = model
                .get(a.id.as_str())
                .copied()
                .or_else(|| held.agents.get(&a.id).map(|h| h.default_model.as_str()));
            let written = match current {
                None => true,
                Some(m) => mode.is_take() && m != a.default_model,
            };
            let priced = self
                .rate_card
                .as_ref()
                .is_none_or(|card| card.contains(&a.default_model));
            if written && !priced {
                return Err(AgentsError::Unpriced(a.default_model.clone()));
            }
            if written {
                model.insert(&a.id, &a.default_model);
            }
        }
        Ok(())
    }

    /// Every event recorded through this adapter, in order — what a
    /// Pg deployment would find on the outbox.
    pub fn recorded_events(&self) -> Vec<Event> {
        self.events.lock().expect("events lock").clone()
    }

    /// Register `agent_id` with the logins it may sign as.
    pub fn with_agent<'a>(self, agent_id: &str, logins: impl IntoIterator<Item = &'a str>) -> Self {
        {
            let mut rows = self.rows.lock().expect("agents lock");
            rows.agents
                .entry(agent_id.to_string())
                .or_insert_with(|| AgentInput {
                    id: agent_id.to_string(),
                    display_name: agent_id.to_string(),
                    default_model: "opus-5".to_string(),
                    aliases: Vec::new(),
                    role: None,
                    department: None,
                    hourly_budget_usd_micros: None,
                    max_concurrent_runs: None,
                });
            for login in logins {
                rows.aliases.insert(login.to_string(), agent_id.to_string());
            }
        }
        self
    }
}

#[async_trait]
impl AgentsRegistry for InMemoryAgents {
    async fn resolve_login(&self, login: &str) -> Result<Option<String>, AgentsError> {
        Ok(self
            .rows
            .lock()
            .expect("agents lock")
            .aliases
            .get(login)
            .cloned())
    }

    async fn list(&self) -> Result<Vec<AgentRow>, AgentsError> {
        let rows = self.rows.lock().expect("agents lock");
        Ok(rows.agents.values().map(|a| rows.row(a)).collect())
    }

    async fn publish(
        &self,
        declared: &[AgentInput],
        mode: PublishMode,
        stamp: &EventStamp,
    ) -> Result<AgentsBatchOutcome, AgentsError> {
        let mut rows = self.rows.lock().expect("agents lock");
        self.refusal(&rows, declared, mode)?;
        let mut events = self.events.lock().expect("events lock");
        let mut inserted = 0usize;
        let mut updated = Vec::new();
        let mut kept = Vec::new();
        let mut unchanged = 0usize;
        for a in declared {
            let before = rows.agents.get(&a.id).map(|held| rows.row(held));
            let mut stored = a.clone();
            stored.aliases.clear();
            // Insert-if-absent keeps a held row as it is; take replaces
            // it (the Pg adapter's DO NOTHING / DO UPDATE).
            if before.is_none() || mode.is_take() {
                rows.agents.insert(a.id.clone(), stored);
            }
            // A declared alias signs as the declared id: landed when
            // nobody holds it, moved only under take when another
            // agent does; one the tenant did not declare stays where
            // it is.
            for alias in &a.aliases {
                if mode.is_take() || !rows.aliases.contains_key(alias) {
                    rows.aliases.insert(alias.clone(), a.id.clone());
                }
            }
            let Some(before) = before else {
                events.push(declared_event(stamp, a)?);
                inserted += 1;
                continue;
            };
            let after = rows.row(&rows.agents[&a.id]);
            let changes = before.changes_to(&after);
            let changed = !changes.is_empty();
            if changed {
                events.push(updated_event(stamp, &after, &changes)?);
                updated.push(UpdatedRow {
                    id: a.id.clone(),
                    changes,
                });
            }
            let differs = after.differs_from(a);
            if !differs.is_empty() {
                kept.push(KeptRow {
                    id: a.id.clone(),
                    differs,
                });
            } else if !changed {
                unchanged += 1;
            }
        }
        Ok(AgentsBatchOutcome {
            received: declared.len(),
            inserted,
            updated,
            kept,
            unchanged,
        })
    }

    async fn list_automations(&self) -> Result<Vec<AutomationActor>, AgentsError> {
        Ok(self
            .automations
            .lock()
            .expect("automations lock")
            .values()
            .cloned()
            .collect())
    }

    async fn declare_automations(
        &self,
        declared: &[AutomationActor],
        stamp: &EventStamp,
    ) -> Result<AutomationsSeedOutcome, AgentsError> {
        validate_all(declared).map_err(AgentsError::Storage)?;
        let mut held = self.automations.lock().expect("automations lock");
        // The table's CHECKs and the unique family, judged before
        // anything lands — a refused bundle lands nothing, as its
        // rolled-back transaction does in Postgres.
        let mut after: Vec<AutomationActor> = held.values().cloned().collect();
        after.extend(
            declared
                .iter()
                .filter(|d| !held.contains_key(&d.id))
                .cloned(),
        );
        if let Err(why) = validate_all(&after) {
            return Err(AgentsError::Storage(format!(
                "a constraint on automation_actors refuses the bundle: {why}"
            )));
        }
        let before: Vec<AutomationActor> = held.values().cloned().collect();
        let outcome = classify(&before, declared);
        let mut events = self.events.lock().expect("events lock");
        for d in declared.iter().filter(|d| outcome.inserted.contains(&d.id)) {
            held.insert(d.id.clone(), d.clone());
            events.push(automation_declared_event(stamp, d)?);
        }
        Ok(outcome)
    }
}
