//! The agent-run log port — three operations on the record, and two on
//! the work profile that sits beside it.
//!
//! The profile pair (backlog 2f23f4c6) is TELEMETRY and records no
//! event: it lives in `agent_run_profiles`, keyed on the run id, and
//! is never a source a rebuilder reads (see `super::profile`). It is
//! on this port rather than a port of its own because it describes the
//! same runs, through the same door, for the same readers.
//!
//! UNLIKE the cadence registry next door, this surface DOES record an
//! event, and for a reason cadence names: `cadence_firings` is its own
//! measurement record and a parallel event stream would duplicate it.
//! Here the row is a PROJECTION — `agents.run.recorded` carries the
//! full run state, the row is rebuilt from it
//! (`super::rebuild::rebuild_agent_runs`), and the write is one
//! transaction so the log and the projection commit together. That is
//! the audit-log-is-the-system-of-record contract, not a second copy.

use async_trait::async_trait;
use boss_core::agent::{AgentCaps, AgentLoad, BudgetDecision};

use chrono::{DateTime, Utc};

use super::profile::{RunProfile, WorkProfile};
use super::types::{AgentRun, NewAgentRun, RateCardRow, RunFilter, TokenUsage};

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

/// What the `agents` row says about the actor a run names — the two
/// columns the recorder reads. `None` when the actor has no row: a
/// legacy colon-form id, or a registered id nobody registered (which
/// `resolve_model` refuses on its own grounds when the run names no
/// model either).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisteredAgent {
    pub default_model: String,
    pub caps: AgentCaps,
}

/// The admission step both adapters run between resolving the model
/// and writing anything: the actor's caps against its measured load,
/// through the one rule (`BudgetDecision::decide`). An actor with no
/// row is unbudgeted — `AgentCaps::default()`, every cap `None` — and
/// is admitted with nothing to count down; a missing row must not stop
/// the stack.
pub fn admit(agent: Option<&RegisteredAgent>, load: AgentLoad) -> BudgetDecision {
    BudgetDecision::decide(agent.map(|a| a.caps).unwrap_or_default(), load)
}

/// What a record attempt did. `recorded: false` means this `run_id` was
/// already on the log — a retried report, not a failure — and the
/// returned run is the one already held, so the caller sees the
/// authoritative record either way.
///
/// `replaced: true` (always with `recorded: true`) means the row this
/// `run_id` already held carried no count, and this record took its
/// place — see [`replaces`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRun {
    pub recorded: bool,
    pub replaced: bool,
    pub run: AgentRun,
}

/// The `detail` key a replacing record carries: what the row it
/// replaced held. One spelling, because the recorder writes it and the
/// rebuilder reads it to know the event supersedes a row rather than
/// collapsing onto it (CLAUDE.md §9a).
pub const REPLACED_KEY: &str = "replaced";

/// May `new` take the place of the row `held` holds for the same run?
///
/// THE ONE EXCEPTION TO INSERT-ONCE, and why it is safe (backlog
/// b5a3a174, measured 2026-10-08). A run's row was written the moment
/// the run LANDED — by the landing rule, from whatever stood on the
/// packet — and that was routinely before the agent's own report: 21 of
/// that day's 56 rows held no count (19) or a metered zero at $0 (2),
/// and each then refused the report that carried the run's real usage
/// (10,703,524 tokens on run ca00400b; $16.50 on af8c0cd7). The cost
/// record said $0 for a third of the day's work.
///
/// A row with no count prices nothing — it is a placeholder for a cost,
/// not a cost — so the first record that DOES carry one replaces it.
/// Exactly once, by construction rather than by a flag: only a row
/// without a count is replaceable, and only a record with one replaces,
/// so the row that results can never be replaced again. A row that
/// holds a count still stands against every later report; correcting a
/// recorded cost is backlog b4fd594e's decision, not this one.
pub fn replaces(held: &AgentRun, new: &NewAgentRun) -> bool {
    let counted = |t: &TokenUsage| t.total().is_some_and(|n| n > 0);
    !counted(&held.run.tokens) && counted(&new.tokens)
}

/// `new` as it is recorded when it replaces `held`: itself, with what
/// it replaced under [`REPLACED_KEY`] in its `detail` — so the row says
/// it is a replacement, the event says so, and a rebuild replays it as
/// one. `null` for the count or the price the old row did not have.
pub fn replacement(held: &AgentRun, new: &NewAgentRun) -> NewAgentRun {
    let mut detail = match &new.detail {
        serde_json::Value::Object(m) => m.clone(),
        serde_json::Value::Null => serde_json::Map::new(),
        // `detail` is free-form: keep a non-object whole rather than
        // drop it to make room for the key.
        other => serde_json::Map::from_iter([("detail".to_string(), other.clone())]),
    };
    detail.insert(
        REPLACED_KEY.to_string(),
        serde_json::json!({
            "recorded_at": held.recorded_at,
            "finished_at": held.run.finished_at,
            "outcome": held.run.outcome.as_str(),
            "total_tokens": held.run.tokens.total(),
            "usd_micros": held.usd_micros,
            "why": "the row held no count, so it priced nothing; the first record carrying \
                    one replaces it, once (backlog b5a3a174)",
        }),
    );
    NewAgentRun {
        detail: serde_json::Value::Object(detail),
        ..new.clone()
    }
}

/// Does a recorded run say it replaced a row? The rebuilder's question.
pub fn is_replacement(run: &NewAgentRun) -> bool {
    run.detail.get(REPLACED_KEY).is_some_and(|v| v.is_object())
}

#[async_trait]
pub trait AgentRunLog: Send + Sync {
    /// Record one FINISHED agent run, pricing it off the rate card in
    /// the same transaction that writes the row and the event.
    ///
    /// Finished, deliberately: this surface is the record of what a run
    /// cost, which is not knowable until it ends. Making the work
    /// visible WHILE it happens is a different fact with a different
    /// home (backlog be025b44 — the builder opens its car at build
    /// start); a half-filled row here would answer "what did it cost"
    /// with a number that was still moving.
    ///
    /// Idempotent on `run_id`, so the reporter can retry a failed
    /// report without inventing a second run. One exception: a held
    /// row that carries no count is replaced by the first record that
    /// carries one ([`replaces`]), and that replacement is a second
    /// `agents.run.recorded` fact naming what it replaced.
    ///
    /// Judged against the actor's budget (backlog 7dd9f28c): the
    /// actor's registry caps against its priced spend in the hour
    /// before the run started and its runs in flight at that instant
    /// ([`super::types::measure_load`]), judged by [`admit`]. The
    /// judgement rides the row and the event as `budget`, `Allow` or
    /// `Deny` — a READING either way, never a refusal (backlog
    /// e6b2066f): the run has happened, and a record that dropped the
    /// over-cap ones would understate exactly the spend a cap is about.
    ///
    /// `recorded_by` is who FILED the record — usually the dispatching
    /// session, sometimes the agent itself. It rides the event as
    /// `_actor` and is not the same thing as the run's own `actor_id`,
    /// which is the CPU that did the work.
    async fn record_run(
        &self,
        run: &NewAgentRun,
        recorded_by: &boss_core::actor::ActorId,
    ) -> Result<RecordedRun, AgentRunError>;

    /// Matching runs, newest finish first.
    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<AgentRun>, AgentRunError>;

    /// How many runs match `filter`, its `limit` aside — the listing's
    /// `total`, so a reader can tell a page from the whole answer
    /// (backlog 11a0998a). A limit is not a filter.
    async fn count_runs(&self, filter: &RunFilter) -> Result<u64, AgentRunError>;

    /// The rate card, model-ordered. Read-only on purpose — see
    /// `super::mod`'s doc comment for why a price is a tree change.
    async fn rate_card(&self) -> Result<Vec<RateCardRow>, AgentRunError>;

    /// Hold `profile` as run `run_id`'s work profile, read at `at`,
    /// REPLACING any profile already held for it: a report retried
    /// after the run went on re-reads a longer transcript, and the
    /// whole transcript is the truer reading. No event — telemetry.
    /// No check that the run is recorded: a report records the profile
    /// even for a run that has reached no terminal yet, and the two
    /// are joined on the id when read.
    async fn record_profile(
        &self,
        run_id: &str,
        profile: &WorkProfile,
        at: DateTime<Utc>,
    ) -> Result<RunProfile, AgentRunError>;

    /// Every profile read in `[since, until)`, oldest first.
    async fn list_profiles(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<RunProfile>, AgentRunError>;
}

/// The one refusal both adapters give a profile write that cannot be
/// held, and a window that cannot hold one.
pub fn validate_profile(run_id: &str) -> Result<(), AgentRunError> {
    if run_id.trim().is_empty() {
        return Err(AgentRunError::BadRequest(
            "run_id is required — a profile describes one recorded run".into(),
        ));
    }
    Ok(())
}

/// `since >= until` is a bad request, not an empty answer: an empty
/// list would read as a quiet week (the `surface_opens` rule).
pub fn validate_window(since: DateTime<Utc>, until: DateTime<Utc>) -> Result<(), AgentRunError> {
    if since >= until {
        return Err(AgentRunError::BadRequest(format!(
            "since ({since}) is not before until ({until}) — the window holds nothing"
        )));
    }
    Ok(())
}

/// Reject what cannot be a run before anything is written. Shared by
/// both adapters so a Postgres caller and an in-memory caller are
/// refused for the same reasons, and each refusal names its fix.
pub fn validate(run: &NewAgentRun) -> Result<(), AgentRunError> {
    if run.run_id.trim().is_empty() {
        return Err(AgentRunError::BadRequest(
            "run_id is required — it is the idempotency key, so the caller must mint it".into(),
        ));
    }
    if run.finished_at < run.started_at {
        return Err(AgentRunError::BadRequest(format!(
            "finished_at ({}) is before started_at ({}) — a run cannot end before it starts",
            run.finished_at, run.started_at
        )));
    }
    if !run.actor_id.is_agent() {
        return Err(AgentRunError::BadRequest(format!(
            "actor_id {:?} is not an agent — an agent run must name its CPU as a \
             registered agent id (e.g. \"agent-claude\", with the model in the body's \
             `model` key or defaulted from the agents registry) or in the legacy \
             <mode>:<model> form (e.g. \"claude:opus-5\")",
            run.actor_id.to_string()
        )));
    }
    if run.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
        return Err(AgentRunError::BadRequest(
            "model is blank — name the model this run ran on (as agent_rate_card spells \
             it, e.g. \"opus-5[1m]\"), or leave the key out to take the agent's default"
                .into(),
        ));
    }
    Ok(())
}

/// The model a run is recorded with — the resolve half of the rule
/// [`NewAgentRun::model`] reads: the report's own word first; else
/// what the actor id already says (the model half of a legacy colon
/// form — a registered id says nothing); else the registered agent's
/// `default_model` (`registry_default`, read by the adapter from the
/// `agents` row for `run.actor_id`). Resolved ONCE, here, and
/// written to the row and the event, so a rebuild replays the model
/// the run was recorded with rather than re-asking a registry whose
/// default may since have changed — the same reason the price is
/// replayed and not recomputed.
///
/// A run that names no model by any of the three is refused, naming
/// both fixes: a run that cannot say what it ran on is not a record of
/// what it cost, and a registered id no `agents` row backs is the
/// other half of the same gap.
pub fn resolve_model(
    run: &NewAgentRun,
    registry_default: Option<&str>,
) -> Result<String, AgentRunError> {
    run.model()
        .or(registry_default)
        .map(str::to_string)
        .ok_or_else(|| {
            AgentRunError::BadRequest(format!(
                "actor {:?} names no model and no agents row supplies a default — send \
                 `model` in the report, or register the agent (an `agents` row with a \
                 default_model) so a run that does not say can take its default",
                run.actor_id.to_string()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runs::types::RunOutcome;
    use boss_core::actor::ActorId;

    fn ok_run() -> NewAgentRun {
        NewAgentRun {
            run_id: "run-1".into(),
            actor_id: ActorId::agent("claude", "opus-5"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
            model: None,
            tokens: crate::agent_runs::types::TokenUsage::Split {
                input: 1,
                output: 1,
            },
            tool_calls: 1,
            job_id: None,
            branch: None,
            detail: serde_json::Value::Null,
        }
    }

    fn registered_run(model: Option<&str>) -> NewAgentRun {
        NewAgentRun {
            actor_id: ActorId::RegisteredAgent("agent-claude".into()),
            model: model.map(str::to_string),
            ..ok_run()
        }
    }

    #[test]
    fn a_well_formed_run_passes() {
        assert!(validate(&ok_run()).is_ok());
    }

    #[test]
    fn a_registered_agent_is_a_well_formed_actor() {
        assert!(validate(&registered_run(None)).is_ok());
    }

    #[test]
    fn a_blank_model_is_refused_and_names_the_two_fixes() {
        let err = validate(&registered_run(Some("  ")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("blank"), "{err}");
        assert!(err.contains("leave the key out"), "{err}");
    }

    // -- resolve_model: the one rule, in the order it is applied --------

    #[test]
    fn the_reports_own_word_wins() {
        let model =
            resolve_model(&registered_run(Some("haiku-4-5")), Some("opus-5")).expect("resolves");
        assert_eq!(
            model, "haiku-4-5",
            "the default is a fallback, not an override"
        );
    }

    #[test]
    fn a_registered_agent_that_does_not_say_takes_the_registry_default() {
        let model = resolve_model(&registered_run(None), Some("opus-5")).expect("resolves");
        assert_eq!(model, "opus-5");
    }

    #[test]
    fn a_legacy_colon_form_actor_resolves_from_its_own_id() {
        // No registry row is consulted for the colon form; the id says.
        let model = resolve_model(&ok_run(), None).expect("resolves");
        assert_eq!(model, "opus-5");
    }

    #[test]
    fn a_run_that_names_no_model_by_any_rule_is_refused_naming_both_fixes() {
        let mut run = registered_run(None);
        run.actor_id = ActorId::RegisteredAgent("agent-nobody".into());
        let err = resolve_model(&run, None).unwrap_err().to_string();
        assert!(err.contains("agent-nobody"), "{err}");
        assert!(err.contains("send `model`"), "{err}");
        assert!(err.contains("register the agent"), "{err}");
    }

    #[test]
    fn an_empty_run_id_is_refused_and_says_why() {
        let mut run = ok_run();
        run.run_id = "   ".into();
        let err = validate(&run).unwrap_err().to_string();
        assert!(err.contains("run_id"), "{err}");
        assert!(err.contains("idempotency"), "{err}");
    }

    #[test]
    fn a_run_that_ends_before_it_starts_is_refused() {
        let mut run = ok_run();
        run.finished_at = run.started_at - chrono::Duration::seconds(1);
        assert!(validate(&run).is_err());
    }

    #[test]
    fn a_zero_length_run_is_allowed() {
        // A run that started and finished inside the same second is
        // odd but real (an immediate refusal, a cancelled dispatch).
        let mut run = ok_run();
        run.finished_at = run.started_at;
        assert!(validate(&run).is_ok());
    }

    #[test]
    fn a_non_agent_actor_is_refused_and_names_both_agent_forms() {
        let mut run = ok_run();
        run.actor_id = ActorId::automation("train-conductor");
        let err = validate(&run).unwrap_err().to_string();
        assert!(err.contains("agent-claude"), "{err}");
        assert!(err.contains("<mode>:<model>"), "{err}");
    }

    #[test]
    fn a_human_actor_is_refused_too() {
        let mut run = ok_run();
        run.actor_id = ActorId::human("emp-032");
        assert!(validate(&run).is_err());
    }

    #[test]
    fn ports_are_object_safe() {
        fn takes<T: ?Sized>() {}
        takes::<dyn AgentRunLog>();
    }
}
