//! `ActorId` — the named origin of every transition.
//!
//! Boss is a human-powered state machine (see
//! `docs/design/human-powered-state-machine.md`). Invariant **I-2**
//! says every transition has a named CPU — so every event and every
//! write needs an actor, full stop. Before this type existed, event
//! authors reached for `actor_id: None` when no human was involved.
//!
//! Every transition is fired by one of exactly three classes of CPU:
//!
//!   - **Human** — an employee took the action (a human CPU). Wire
//!     form is the bare `employees.id` (`emp-032`).
//!   - **Automation** — a *named* authority fired it: a dispatch rule
//!     (`automation:rule:<name>`), a scheduler, or the emitting
//!     service itself (`automation:<service>`).
//!   - **Agent** — an LLM session executed the transition. Wire form
//!     is `<agent-mode>:<model>` (`claude:opus-5`, `claude:fable`),
//!     per David's directive of 2026-08-13: *"I like
//!     [agent-mode]:[model] as the actor_id config and agent mode can
//!     be claude for interactive Claude sessions like Claude Code."*
//!     `claude` is the mode for an interactive Claude session (Claude
//!     Code); the model half is the vendor's model string, kept whole.
//!
//!     The same class has a second, REGISTRY spelling since design
//!     6fda05ae (decided 2026-09-15): `agent-<slug>` (`agent-claude`),
//!     the id of a row in the `agents` registry, bare on the wire the
//!     way an employee id is. That decision moved the model off the
//!     actor and onto the run (`agent_runs.model`) — one registered
//!     agent runs different models over time, and cost is priced per
//!     run — so the registry form carries no model half. The run
//!     carries its model in a column of its own since 2026-09-15
//!     (`agent_runs.model`, backlog 7dd9f28c); the `<mode>:<model>`
//!     form survives on the rows and events written before that, and
//!     the [`ActorId::Agent`] arm survives to read them. The
//!     [`ActorId::RegisteredAgent`] arm is where the class is heading
//!     and the [`ActorId::Agent`] arm is the one to retire once no
//!     reporter sends it.
//!
//! Agents are CPUs in the same machine, not a separate system — but
//! they are not *people*. Before this variant existed, an agent
//! stamping `claude:fable` parsed as `Human("claude:fable")`: the SPA
//! rendered it as an unknown employee and every human-vs-automation
//! census counted agent work as staff work. Ask "is this a person?"
//! with [`ActorId::is_human`], which is false for both non-human
//! classes; ask "which class" by matching the variant.
//!
//! **Retro consequence of the directive:** the model is now a
//! groupable dimension of `actor_id` *alone*. "How much work did
//! opus-5 do this week" is a group-by on the parsed actor — there is
//! no separate `_model` payload key to add, and none should be added.
//!
//! There is deliberately no anonymous "system" actor. A system has no
//! inherent autonomy to do anything; attributing a transition to a
//! bare `"system"` masks the real authority that granted it. So every
//! automated transition references its explicit grant — the same way
//! the dispatcher already attributes side-effects to the rule that
//! fired them. "No one did it" is not a representable state, and
//! neither is "the system did it".
//!
//! One concept, four deliberate spellings — do not flatten:
//!   - `ActorId` — this Rust type;
//!   - `actor` — the publisher-parameter name in `EventPublisher`;
//!   - `_actor` — the audit_log payload key (the `_actor` /
//!     `_simulated` / `_source` metadata family);
//!   - `actor_id` — the serialized field on SQL columns, HTTP
//!     bodies, and TS types (the dominant spelling at boundaries).

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Who (or what) fired a transition. Every event carries one.
///
/// Wire format:
///   - `ActorId::Human("emp-032")` → `"emp-032"` (bare; the SPA
///     consumes `actor_id` as an employee id when present).
///   - `ActorId::Automation("shipping-agent")` → `"automation:shipping-agent"`
///   - `ActorId::agent("claude", "opus-5")` → `"claude:opus-5"`
///
/// The Human case serializes as a bare string (no `human:` prefix)
/// because the SPA treats `actor_id` as an opaque employee-id lookup
/// in many places (e.g. `empNames.get(actor_id)`). Automation uses the
/// `automation:` prefix so its kind is unambiguous on the wire and a
/// stale frontend doesn't render `"automation:cron"` as
/// "Employee automation:cron".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActorId {
    /// An Boss employee. The string is the `employees.id` foreign key.
    Human(String),
    /// A named automated process — schedulers, cron jobs, dispatch
    /// rules, bus subscribers. The string is a stable slug for the
    /// program (e.g. `"shipping-agent"`, `"escalation-router"`,
    /// `"rule:bill-approve"`). These slugs are free-form for
    /// now; if we need a registry of known automations later, that's
    /// a separate design.
    Automation(String),
    /// An LLM session. `mode` is the agent harness (`claude` for an
    /// interactive Claude session such as Claude Code); `model` is the
    /// vendor's model string (`opus-5`, `fable`, `claude-opus-5.1`),
    /// kept whole so the model stays a groupable dimension of the
    /// actor id itself. Neither half is validated against a registry —
    /// same free-form stance as automation slugs.
    Agent { mode: String, model: String },
    /// A registered agent, by the id of its `agents` registry row —
    /// `agent-<slug>`, e.g. `agent-claude`. The address it logs in
    /// with (`claude@algedonic.dev`) is an ALIAS resolved at the jobs
    /// API's door before any write is signed, the way a human's login
    /// resolves to an `emp-*` id in boss-gateway's oidc.rs — which is
    /// why there is deliberately no address arm here: an address
    /// surviving to a step means resolution did not happen, not that a
    /// fourth kind of actor exists. Wire form is the bare id, because
    /// the SPA treats colon-free ids as opaque lookups and the `agent-`
    /// prefix is what tells it (and [`ActorId::is_human`]) this one is
    /// not staff.
    RegisteredAgent(String),
}

impl ActorId {
    /// Short-hand for a human actor from an employee id string.
    pub fn human(emp_id: impl Into<String>) -> Self {
        Self::Human(emp_id.into())
    }

    /// Short-hand for a named automation.
    pub fn automation(name: impl Into<String>) -> Self {
        Self::Automation(name.into())
    }

    /// Short-hand for an agent session — `agent("claude", "opus-5")`.
    pub fn agent(mode: impl Into<String>, model: impl Into<String>) -> Self {
        Self::Agent {
            mode: mode.into(),
            model: model.into(),
        }
    }

    /// True if a *person* was the CPU on this transition. False for
    /// both machine classes — an agent is a CPU, not staff.
    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human(_))
    }

    /// True if an LLM session was the CPU on this transition — in
    /// either spelling of the class.
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. } | Self::RegisteredAgent(_))
    }

    /// The underlying id / slug for display. Human, Automation and
    /// RegisteredAgent return their inner string (`emp-032`,
    /// `rule:bill-approve` — note the automation slug *without* its
    /// `automation:` prefix — `agent-claude`); Agent returns the full
    /// `<mode>:<model>`, because neither half alone identifies the CPU.
    /// Use [`Display`](fmt::Display) when you want the wire form for
    /// every variant.
    pub fn as_slug(&self) -> Cow<'_, str> {
        match self {
            Self::Human(id) | Self::Automation(id) | Self::RegisteredAgent(id) => {
                Cow::Borrowed(id.as_str())
            }
            Self::Agent { mode, model } => Cow::Owned(format!("{mode}:{model}")),
        }
    }

    /// True when this actor's text form parses back to this same actor.
    ///
    /// Every actor that came off the wire does — `FromStr` only builds
    /// values that print back to themselves — but the variants are public,
    /// so Rust code can build one that does not: `Human("system")` reads
    /// back as the `platform` automation, a Human or RegisteredAgent id
    /// holding a colon as an agent, `Human("agent-x")` as a registered
    /// agent, `RegisteredAgent("emp-1")` as a human, and an Agent whose
    /// mode is `automation` as an automation. A store that keeps the text
    /// form (a TEXT column) would then hand a DIFFERENT actor back than
    /// one that keeps the value — the credit for the transition changes
    /// on the way through, which is the misattribution this type exists
    /// to refuse. A store's write door refuses such an actor with this
    /// (backlog b8099caf); it is not refused at construction because the
    /// variants are the type's public shape, built in hundreds of places.
    pub fn text_form_round_trips(&self) -> bool {
        self.to_string().parse::<Self>().as_ref() == Ok(self)
    }

    /// The kind of executor this actor is (factory taxonomy M1). Total and
    /// one-to-one over the arms, decided 2026-11-03 on backlog 3ff2ef60:
    /// a human is the Human arm, an automaton the Automation arm, and a
    /// robot either agent spelling. This is the answer the claim door
    /// records for every holder, by its canonical id. `agents` carries an
    /// `executor_kind` column (default `robot`, the backfill), but no
    /// door writes any other value into it and no reader consults it, so
    /// the column and this answer cannot disagree today (station tree
    /// T1, 050284c2). A machine identity that claims a step is spelled
    /// `automation:<name>` and is an automaton here.
    ///
    /// Two spellings are read past their arm (backlog 8e8d0b1a), so every
    /// executor-kind question asks here rather than at an id prefix: a
    /// legacy bare `rule:<name>` parses as an agent of mode `rule` but is
    /// the dispatcher's, an automaton; and an address still in the record
    /// parses Human but is an unresolved session login, which on this
    /// deployment is a robot (the gateway resolves a person's login to an
    /// `emp-*` id before any write is signed; apps/web's `isHumanActor`
    /// reads it the same way).
    pub fn executor_kind(&self) -> ExecutorKind {
        match self {
            Self::Human(id) if id.contains('@') => ExecutorKind::Robot,
            Self::Human(_) => ExecutorKind::Human,
            Self::Automation(_) => ExecutorKind::Automaton,
            Self::Agent { mode, .. } if mode == "rule" => ExecutorKind::Automaton,
            Self::Agent { .. } | Self::RegisteredAgent(_) => ExecutorKind::Robot,
        }
    }
}

/// What KIND of executor ran a step — the factory taxonomy's three
/// (design 4348b857, decided by David 2026-11-02; v2 in
/// `docs/design/factory-taxonomy-v2.md`, primitive 5). A **human** brings
/// judgment and is stateful; a **robot** brings judgment, is stochastic
/// and stateless between claims; an **automaton** is deterministic
/// software executing a step.
///
/// A CLOSED enum, not Class-registry data, though CLAUDE.md §9 sends
/// taxonomies to the registry: §9 is about TENANT-EXTENSIBLE categories,
/// and these three are core vocabulary the design fixes — its rule 3 is
/// that there is no fourth kind, and the one-kind check that holds every
/// executor id to exactly one of them only means something over a
/// closed set. Roles, model classes and equipment kinds — the part a
/// tenant does extend — stay registry data.
///
/// Wire form is a lowercase word, and any word but those below is
/// refused, never read as the nearest kind.
///
/// EXPAND NOW, CONTRACT LATER (station tree T1, backlog 050284c2). v2
/// calls the staff kind `human`, and that is this variant's name. The
/// record still WRITES `person`, the word every earlier build reads: the
/// last-converged build a watchdog rollback lands on refuses `human` in
/// its claims fold, so a claim written as `human` would leave the console
/// unreadable on the build the rollback chose. This build READS both
/// words. The retirement car flips the written word to `human` once a
/// build that reads both has converged. Pinned by
/// `every_kind_is_written_in_a_word_the_last_build_reads`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorKind {
    #[serde(rename = "person", alias = "human")]
    Human,
    Robot,
    Automaton,
}

impl ExecutorKind {
    /// Every kind there is.
    pub const ALL: [Self; 3] = [Self::Human, Self::Robot, Self::Automaton];

    /// The word the record carries for this kind: what this build
    /// writes, and what every earlier build reads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "person",
            Self::Robot => "robot",
            Self::Automaton => "automaton",
        }
    }

    /// The v2 word for this kind (factory-taxonomy-v2.md), which this
    /// build reads beside [`Self::as_str`] and does not yet write.
    pub fn v2_word(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Robot => "robot",
            Self::Automaton => "automaton",
        }
    }
}

impl fmt::Display for ExecutorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ExecutorKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s || kind.v2_word() == s)
            .ok_or_else(|| {
                format!(
                    "{s:?} is not an executor kind (human, robot or automaton, and person for \
                     human; there is no fourth)"
                )
            })
    }
}

impl fmt::Display for ActorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Mirrors the Serialize impl: Human and RegisteredAgent are
        // bare; Automation uses the `automation:` prefix; Agent is
        // `<mode>:<model>`.
        match self {
            Self::Human(id) | Self::RegisteredAgent(id) => f.write_str(id),
            Self::Automation(name) => write!(f, "automation:{name}"),
            Self::Agent { mode, model } => write!(f, "{mode}:{model}"),
        }
    }
}

impl FromStr for ActorId {
    type Err = std::convert::Infallible;

    /// Branch order is the contract, and the tests pin it:
    ///
    /// 1. `automation:` prefix → [`Self::Automation`] with everything
    ///    after the prefix kept whole. FIRST, because the rest may
    ///    itself contain colons (`automation:rule:bill-approve` is the
    ///    dispatcher's stamp on every side-effect) and the agent split
    ///    below would otherwise claim it.
    /// 2. Literal `system` → the named `platform` automation.
    /// 3. Contains a `:` → [`Self::Agent`], split on the FIRST colon.
    ///    Safe as a catch-all because no employee id carries a colon —
    ///    verified across the seeds and fixtures (`emp-aa-001`,
    ///    `emp-bootstrap-admin`).
    /// 4. `agent-` prefix with a non-empty slug →
    ///    [`Self::RegisteredAgent`]. Safe because no employee id
    ///    carries it (`emp-*`) and the prefix was already the
    ///    convention for agent ids (the retired boss-observability's demo agents,
    ///    `agent-inventory-reorder-advisor`).
    /// 5. Otherwise → [`Self::Human`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some(rest) = s.strip_prefix("automation:") {
            Self::Automation(rest.to_string())
        } else if s == "system" {
            // Map the bare `system` actor to a typed, named catch-all
            // rather than a fake human, so every transition is
            // attributed to a real CPU.
            Self::Automation("platform".to_string())
        } else if let Some((mode, model)) = s.split_once(':') {
            // `split_once` keeps the model half whole: a model string
            // may carry dots and further colons, and all of it names
            // the model.
            Self::Agent {
                mode: mode.to_string(),
                model: model.to_string(),
            }
        } else if s
            .strip_prefix(REGISTERED_AGENT_PREFIX)
            .is_some_and(|slug| !slug.is_empty())
        {
            Self::RegisteredAgent(s.to_string())
        } else {
            Self::Human(s.to_string())
        })
    }
}

/// The prefix a registered agent's id carries (`agent-claude`). One
/// definition: the parse above reads it, and the `agents` registry's
/// CHECK constraint spells the same thing in SQL.
pub const REGISTERED_AGENT_PREFIX: &str = "agent-";

/// The gateway's own service identity on the wire: the actor its
/// server-side calls sign as (`boss_gateway::passkey::GATEWAY_ACTOR` is
/// this constant). Spelled once (CLAUDE.md §9a) because two crates
/// decide trust on it and neither may depend on the other: the gateway
/// signs as it, and boss-people's passkey promote accepts no other
/// caller (design 2cb6256f D5).
pub const GATEWAY_ACTOR_ID: &str = "automation:gateway";

impl Serialize for ActorId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Human(id) | Self::RegisteredAgent(id) => s.serialize_str(id),
            Self::Automation(name) => s.serialize_str(&format!("automation:{name}")),
            Self::Agent { mode, model } => s.serialize_str(&format!("{mode}:{model}")),
        }
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either a string or `null`. A `null` (an event with
        // no recorded actor) maps to the `platform` automation —
        // every transition is attributed, never anonymous.
        let opt: Option<String> = Option::deserialize(d)?;
        Ok(match opt {
            Some(s) => s
                .parse()
                .unwrap_or_else(|_| Self::Automation("platform".to_string())),
            None => Self::Automation("platform".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_roundtrip_uses_bare_string() {
        let a = ActorId::Human("emp-032".into());
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"emp-032\"");
        let back: ActorId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn automation_roundtrip_uses_prefix() {
        let a = ActorId::Automation("warranty-expiry".into());
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"automation:warranty-expiry\"");
        let back: ActorId = serde_json::from_str(&j).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn legacy_system_string_maps_to_platform_automation() {
        // The `System` actor was removed in v1.1.0. A stale `"system"`
        // on the wire is read as the named `platform` automation, never
        // a fake human.
        let back: ActorId = serde_json::from_str("\"system\"").unwrap();
        assert_eq!(back, ActorId::Automation("platform".into()));
    }

    #[test]
    fn null_deserializes_to_platform_automation() {
        let a: ActorId = serde_json::from_str("null").unwrap();
        assert_eq!(a, ActorId::Automation("platform".into()));
    }

    // -- Agent (the third CPU class) ------------------------------------

    /// The four `FromStr` branches, in the order they are tried. The
    /// order is the contract, not an implementation detail: `automation:`
    /// must win before the colon-split, or `automation:rule:bill-approve`
    /// (which exists in the wild) would read as an agent in `automation`
    /// mode running the `rule:bill-approve` model.
    #[test]
    fn parse_order_is_automation_then_system_then_agent_then_human() {
        let cases = [
            (
                "automation:rule:bill-approve",
                ActorId::Automation("rule:bill-approve".into()),
            ),
            (
                "automation:dispatcher",
                ActorId::Automation("dispatcher".into()),
            ),
            ("system", ActorId::Automation("platform".into())),
            ("claude:fable", ActorId::agent("claude", "fable")),
            ("emp-032", ActorId::Human("emp-032".into())),
        ];
        for (wire, want) in cases {
            assert_eq!(wire.parse::<ActorId>().unwrap(), want, "parsing {wire}");
        }
    }

    #[test]
    fn agent_roundtrips_through_the_wire_form() {
        for (mode, model) in [("claude", "fable"), ("claude", "opus-5")] {
            let a = ActorId::agent(mode, model);
            let j = serde_json::to_string(&a).unwrap();
            assert_eq!(j, format!("\"{mode}:{model}\""));
            assert_eq!(a.to_string(), format!("{mode}:{model}"));
            assert_eq!(serde_json::from_str::<ActorId>(&j).unwrap(), a);
        }
    }

    /// The model half is kept whole — it is a vendor string, not a
    /// path. Dots and further colons belong to the model.
    #[test]
    fn agent_model_keeps_dots_and_further_colons() {
        assert_eq!(
            "claude:claude-opus-5".parse::<ActorId>().unwrap(),
            ActorId::agent("claude", "claude-opus-5")
        );
        assert_eq!(
            "claude:claude-opus-5.1".parse::<ActorId>().unwrap(),
            ActorId::agent("claude", "claude-opus-5.1")
        );
        let nested = ActorId::agent("claude", "opus-5:1m");
        assert_eq!("claude:opus-5:1m".parse::<ActorId>().unwrap(), nested);
        assert_eq!(nested.to_string(), "claude:opus-5:1m");
    }

    /// `automation:rule:<name>` must survive the round trip untouched —
    /// the dispatcher stamps every side-effect with one of these.
    #[test]
    fn automation_with_colons_in_the_slug_roundtrips() {
        let a = ActorId::Automation("rule:bill-approve".into());
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"automation:rule:bill-approve\"");
        assert_eq!(serde_json::from_str::<ActorId>(&j).unwrap(), a);
    }

    #[test]
    fn is_human_and_is_agent_truth_table() {
        let human = ActorId::human("emp-032");
        let automation = ActorId::automation("dispatcher");
        let agent = ActorId::agent("claude", "opus-5");
        assert!(human.is_human() && !human.is_agent());
        assert!(!automation.is_human() && !automation.is_agent());
        // An agent is a CPU, but not a person: it must never be
        // counted as staff by a human-vs-machine census.
        assert!(!agent.is_human() && agent.is_agent());
    }

    #[test]
    fn as_slug_is_the_id_for_humans_and_automations_and_mode_model_for_agents() {
        assert_eq!(ActorId::human("emp-032").as_slug(), "emp-032");
        assert_eq!(
            ActorId::automation("rule:bill-approve").as_slug(),
            "rule:bill-approve"
        );
        assert_eq!(ActorId::agent("claude", "fable").as_slug(), "claude:fable");
    }

    // -- RegisteredAgent (the registry form of the same class) -------

    /// `agent-<slug>` is a registered agent's canonical id (design
    /// 6fda05ae): bare on the wire like an employee id, colon-free, and
    /// NOT a person. Before this branch existed it parsed as `Human`,
    /// which is exactly the census defect the `Agent` arm was added to
    /// stop, wearing a new spelling.
    #[test]
    fn a_registered_agent_id_parses_bare_and_is_not_a_person() {
        let a: ActorId = "agent-claude".parse().unwrap();
        assert_eq!(a, ActorId::RegisteredAgent("agent-claude".into()));
        assert!(a.is_agent() && !a.is_human());
        assert_eq!(a.as_slug(), "agent-claude");
        assert_eq!(a.to_string(), "agent-claude");
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(j, "\"agent-claude\"");
        assert_eq!(serde_json::from_str::<ActorId>(&j).unwrap(), a);
    }

    /// The prefix is the whole test, and it is `agent-` with the hyphen
    /// and something after it: an automation slug that merely contains
    /// the word (`shipping-agent`), an id that starts with `agent` and
    /// no hyphen, or the bare prefix stay what they were.
    #[test]
    fn only_the_agent_hyphen_prefix_is_a_registered_agent() {
        assert_eq!(
            "agentsmith".parse::<ActorId>().unwrap(),
            ActorId::Human("agentsmith".into())
        );
        assert_eq!(
            "automation:shipping-agent".parse::<ActorId>().unwrap(),
            ActorId::Automation("shipping-agent".into())
        );
        assert_eq!(
            "agent-".parse::<ActorId>().unwrap(),
            ActorId::Human("agent-".into()),
            "a bare prefix names no agent"
        );
    }

    /// The actors Rust can build whose text form reads back as someone
    /// else, and one of each class that reads back as itself
    /// (backlog b8099caf).
    #[test]
    fn text_form_round_trips_is_false_exactly_where_the_parse_names_someone_else() {
        let drift = [
            ActorId::Human("system".into()),
            ActorId::Human("emp:032".into()),
            ActorId::Human("agent-claude".into()),
            ActorId::Human("automation:cron".into()),
            ActorId::RegisteredAgent("emp-032".into()),
            ActorId::RegisteredAgent("agent-x:y".into()),
            ActorId::agent("automation", "cron"),
            ActorId::agent("claude:x", "opus-5"),
        ];
        for a in drift {
            assert!(!a.text_form_round_trips(), "{a:?} reads back as itself");
        }
        let whole = [
            ActorId::human("emp-032"),
            ActorId::human("agent-"),
            ActorId::automation("rule:bill-approve"),
            ActorId::automation("platform"),
            ActorId::agent("claude", "opus-5:1m"),
            ActorId::RegisteredAgent("agent-claude".into()),
        ];
        for a in whole {
            assert!(a.text_form_round_trips(), "{a:?} reads back otherwise");
        }
    }

    // -- ExecutorKind (factory taxonomy M1, design 4348b857) ----------

    /// Every arm of the actor maps to exactly one kind, one-to-one with
    /// the decision of 2026-11-03: Human → human, Automation →
    /// automaton, both agent spellings → robot.
    #[test]
    fn every_actor_class_is_exactly_one_executor_kind() {
        let cases = [
            ("emp-032", ExecutorKind::Human),
            ("automation:dispatcher", ExecutorKind::Automaton),
            ("automation:rule:bill-approve", ExecutorKind::Automaton),
            ("system", ExecutorKind::Automaton),
            ("claude:opus-5", ExecutorKind::Robot),
            ("agent-claude", ExecutorKind::Robot),
        ];
        for (wire, want) in cases {
            let actor: ActorId = wire.parse().unwrap();
            assert_eq!(actor.executor_kind(), want, "{wire}");
        }
    }

    /// The two spellings the arms alone read wrongly (backlog 8e8d0b1a):
    /// a legacy bare `rule:<name>` is the dispatcher's, an automaton, not
    /// an agent of mode `rule`; and an address that reached the record
    /// unresolved is a SESSION LOGIN, which the gateway never leaves for a
    /// person (it resolves every human login to an `emp-*` id before a
    /// write is signed), so on this deployment it is a robot. The web's
    /// `isHumanActor` (apps/web/src/data/actor.ts) already reads it so.
    #[test]
    fn a_legacy_rule_is_an_automaton_and_an_unresolved_login_is_a_robot() {
        for (wire, want) in [
            ("rule:bill-approve", ExecutorKind::Automaton),
            ("claude@algedonic.dev", ExecutorKind::Robot),
            ("emp-bootstrap-admin", ExecutorKind::Human),
        ] {
            let actor: ActorId = wire.parse().unwrap();
            assert_eq!(actor.executor_kind(), want, "{wire}");
        }
    }

    /// The written spelling is the lowercase word, both ways, and there
    /// is no fourth kind: `machine`, `agent`, a capitalised word and the
    /// empty string are refused rather than read as one of the three.
    /// (`human` is the v2 word for the first kind, not a fourth:
    /// `the_v2_word_human_reads_as_the_human_kind`.)
    #[test]
    fn the_executor_kind_has_three_words_and_refuses_a_fourth() {
        for (kind, word) in [
            (ExecutorKind::Human, "person"),
            (ExecutorKind::Robot, "robot"),
            (ExecutorKind::Automaton, "automaton"),
        ] {
            assert_eq!(kind.as_str(), word);
            assert_eq!(kind.to_string(), word);
            assert_eq!(word.parse::<ExecutorKind>(), Ok(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), word);
            assert_eq!(
                serde_json::from_value::<ExecutorKind>(serde_json::json!(word)).unwrap(),
                kind
            );
        }
        assert_eq!(ExecutorKind::ALL.len(), 3);
        for fourth in ["machine", "agent", "Human", "Robot", ""] {
            assert!(fourth.parse::<ExecutorKind>().is_err(), "{fourth:?}");
            assert!(
                serde_json::from_value::<ExecutorKind>(serde_json::json!(fourth)).is_err(),
                "{fourth:?}"
            );
        }
    }

    /// Station tree T1 (backlog 050284c2, factory-taxonomy-v2.md): the
    /// v2 word for the kind a staff member is is `human`. Every build
    /// from this one on reads it, so the retirement car that WRITES it
    /// rolls back onto a build that can read what it wrote.
    #[test]
    fn the_v2_word_human_reads_as_the_human_kind() {
        let human = ActorId::Human("emp-032".into()).executor_kind();
        assert_eq!("human".parse::<ExecutorKind>(), Ok(human));
        assert_eq!(
            serde_json::from_value::<ExecutorKind>(serde_json::json!("human")).unwrap(),
            human
        );
    }

    /// The rollback pin. The last-converged build a watchdog rollback
    /// lands on predates the v2 word and refuses it: its claims fold
    /// deserializes `_executor_kind` strictly and its `FromStr` knows
    /// three words. So every word this build WRITES is one of those
    /// three, and `human` is not written until a contract car flips it,
    /// once a build that reads both has converged.
    #[test]
    fn every_kind_is_written_in_a_word_the_last_build_reads() {
        const READ_BY_THE_LAST_BUILD: [&str; 3] = ["person", "robot", "automaton"];
        for kind in ExecutorKind::ALL {
            let written = serde_json::to_value(kind).unwrap();
            assert!(
                READ_BY_THE_LAST_BUILD.contains(&written.as_str().unwrap()),
                "{kind:?} is written as {written}, which the last build refuses"
            );
            assert_eq!(kind.to_string(), written.as_str().unwrap());
        }
    }

    /// Model is a groupable dimension off `actor_id` alone — the retro
    /// consequence of the directive. No `_model` payload key needed.
    #[test]
    fn model_is_recoverable_from_the_actor_id_alone() {
        let ActorId::Agent { mode, model } = "claude:opus-5".parse::<ActorId>().unwrap() else {
            panic!("expected an agent");
        };
        assert_eq!((mode.as_str(), model.as_str()), ("claude", "opus-5"));
    }
}
