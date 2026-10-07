//! Every control a door checks is held by at least one real person —
//! the coverage invariant, measured (design 1c4e42e1, David 2026-09-28;
//! backlog 47aed706).
//!
//! WHY. David, 2026-09-28: "make sure that all required permissions to
//! operate exist in the context of at least one real user". Measured the
//! same day: one real person, 60 policy pairs a door asks for, all held
//! by him through platform-admin — and ZERO people holding the operator
//! tier, because elevation needs an operator-tier passkey and no person
//! had one. Nothing in the tree could say so; the triage re-derived it by
//! hand from 135 `Action::` sites and six live reads. This module is that
//! measurement as one pure function, so the read, the backstop alarm and
//! (later) the guards all answer the question the same way.
//!
//! A CONTROL is anything a door asks for before it acts, of four kinds:
//! - a policy pair — static ones declared as the consts every door asks
//!   through ([`crate::controls`]), dynamic ones
//!   read off every active workflow: `sign-off` on `step-signoff:<role>`
//!   for each required sign-off, and the claim door's `authority_role`
//!   ([`Control::Authority`], held the three ways the claim door admits);
//! - the operator tier — a real person with platform-admin and an
//!   operator-tier passkey, what the gateway's `elevation.rs` requires;
//! - the platform owner — a real person with platform-admin, what
//!   `platform_owner` resolves;
//! - presence — every step declaring `assurance_required = "presence"`
//!   has a real person in its audience.
//!
//! A REAL PERSON is an active employee with at least one bound passkey
//! (decision 1): a fact the record already holds, so a system account
//! (emp-audit, 0 keys), the deploy superuser and `automation:*` actors
//! never count.
//!
//! A PAIR IS HELD AT THE WIDEST SCOPE ANY ACTIVE RULE GRANTS FOR IT
//! (decision 2): authority rule 1 lets a person re-grant only within
//! their own scope, so a narrower holder cannot restore it. A pair some
//! door admits only at scope `all` ([`Pair::all_only`]) is held only at
//! `all`, whatever the rules grant — the release review of car 1 (LOW):
//! a tenant granting Read on `estate` only at `territory` read HELD
//! while the door refused every caller.
//!
//! WHAT REFUSES ON IT. The coverage core, its read and the backstop only
//! report. The guards call [`orphaned_by`], released one per train after
//! DR readiness (62dac114, design b08725c2): the policy-write guard
//! (`boss-policy` `guard.rs`, car 3) first; the people guard and the
//! workflow-publish guard (cars 4 and 5) after it.

use serde::{Deserialize, Serialize};

pub use crate::controls::{CONTROLS, Pair};
use crate::types::{AccessTier, Action, PolicyRule, Scope, UserOverride, rule_id};
use Action::{SignOff, Update};
use boss_core::roles::PLATFORM_ADMIN_ROLE;

// ---------------------------------------------------------------------------
// The static controls — declared once, as the consts every door asks
// through (`crate::controls`).
// ---------------------------------------------------------------------------

/// The static pairs — every [`CONTROLS`] const, ordered by resource then
/// action. Until 2026-09-29 this read a hand-kept `DOORS` table, one
/// entry per door file, pinned by a count-and-digest scan (car 1 of
/// design 1c4e42e1); every door now asks through a const, so the list is
/// the consts and cannot miss a door that asks one.
pub fn static_pairs() -> Vec<Pair> {
    let mut out: Vec<Pair> = CONTROLS.to_vec();
    out.sort_by_key(|p| (p.resource_name(), p.action().as_str()));
    out
}

// ---------------------------------------------------------------------------
// Controls, and what holds each.
// ---------------------------------------------------------------------------

/// Something a door asks for before it acts. See the module doc for the
/// four kinds; [`Control::Authority`] is the dynamic half of the policy
/// kind that is not a single pair, so it carries a kind of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Control {
    /// A door asks policy for `action` on `resource`; `all_only` when
    /// the door admits it only at scope `all` ([`Pair::all_only`]).
    Policy {
        action: Action,
        resource: String,
        #[serde(default)]
        all_only: bool,
    },
    /// A step whose `authority_role` is `role` is claimed by a holder of
    /// that role, of `sign-off` on `step-signoff:<role>`, or of `update`
    /// on `step-assign` — the claim door's three routes
    /// (`boss-jobs` `claimant_holds_authority`).
    Authority { role: String },
    /// A person can raise a session to operator: the platform owner
    /// ([`platform_owner`]) holding an operator-tier passkey — the only
    /// session `boss-gateway` `elevation.rs` elevates.
    OperatorTier,
    /// The platform owner ([`platform_owner`]) is a real person.
    PlatformOwner,
    /// A presence-assured step has a real person in its audience: one of
    /// `roles` (its authority role and its required sign-offs), or the
    /// `individual` it names.
    Presence {
        workflow: String,
        step: String,
        roles: Vec<String>,
        individual: Option<String>,
    },
}

/// The remedy the operator-tier gap names (design 1c4e42e1 decision 5):
/// the one road to an operator key is the promotion door, since the
/// register door stores user tier only.
pub const OPERATOR_TIER_REMEDY: &str = "promote one of the platform owner's passkeys to the \
     operator tier through a passkey-promotion packet (backlog 2a228d0c) — the register door \
     stores every new key at user tier (backlog 1d9970d1), so promotion is the one road to an \
     operator key";

/// The remedy an unheld platform owner names: the owner is resolved by
/// hire order, so the fix is the owner binding a key, not another admin.
pub const PLATFORM_OWNER_REMEDY: &str = "the platform owner (the first hire among the active \
     platform-admins) must bind a passkey through the gateway's ceremony at /me — a key on a \
     later-hired admin does not make them the owner";

impl Control {
    /// The stable id an orphan is named and an alarm is deduped by.
    pub fn id(&self) -> String {
        match self {
            Self::Policy {
                action, resource, ..
            } => format!("policy:{}:{resource}", action.as_str()),
            Self::Authority { role } => format!("authority:{role}"),
            Self::OperatorTier => "operator-tier".to_string(),
            Self::PlatformOwner => "platform-owner".to_string(),
            Self::Presence { workflow, step, .. } => format!("presence:{workflow}:{step}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Policy { .. } => "policy",
            Self::Authority { .. } => "authority",
            Self::OperatorTier => "operator-tier",
            Self::PlatformOwner => "platform-owner",
            Self::Presence { .. } => "presence",
        }
    }

    /// What a holder must have, in words a reader acts on.
    pub fn wants(&self) -> String {
        match self {
            Self::Policy {
                action,
                resource,
                all_only: true,
            } => format!(
                "a real person granted {} on {resource} at scope all — the door admits no \
                 narrower scope",
                action.as_str()
            ),
            Self::Policy {
                action, resource, ..
            } => format!(
                "a real person granted {} on {resource} at the widest scope any active rule \
                 grants it",
                action.as_str()
            ),
            Self::Authority { role } => format!(
                "a real person with role {role}, or granted sign-off on step-signoff:{role}, or \
                 update on step-assign — who may claim a step whose authority_role is {role}"
            ),
            Self::OperatorTier => format!(
                "the platform owner — the first hire among the active {PLATFORM_ADMIN_ROLE}s, the \
                 only person the gateway elevates — a real person holding an operator-tier passkey"
            ),
            Self::PlatformOwner => format!(
                "the platform owner — the first hire among the active {PLATFORM_ADMIN_ROLE}s, whom \
                 the platform-owner resolution names — a real person with a bound passkey"
            ),
            Self::Presence {
                workflow,
                step,
                roles,
                individual,
            } => {
                let mut who: Vec<String> = roles.iter().map(|r| format!("role {r}")).collect();
                who.extend(individual.iter().map(|i| format!("the person {i}")));
                let who = if who.is_empty() {
                    "no role or person — the step's audience names none a person can be in"
                        .to_string()
                } else {
                    who.join(" or ")
                };
                format!(
                    "a real person in the audience of {workflow} `{step}`, which requires \
                     presence: {who}"
                )
            }
        }
    }

    /// The write that would cover this control when no one holds it, if
    /// the remedy is more specific than granting it.
    pub fn remedy(&self) -> Option<&'static str> {
        match self {
            Self::OperatorTier => Some(OPERATOR_TIER_REMEDY),
            Self::PlatformOwner => Some(PLATFORM_OWNER_REMEDY),
            _ => None,
        }
    }
}

/// One row of the roster: an employee, their role, whether they are
/// active (`status = active`), and when they were hired — the order the
/// platform owner is resolved by ([`platform_owner`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub role: Option<String>,
    pub active: bool,
    #[serde(default)]
    pub hire_date: Option<chrono::NaiveDate>,
}

/// The platform owner as the gateway resolves it: the FIRST HIRE among
/// the active platform-admins (`boss_core::platform_owner::first_hire`,
/// the one ordering every adapter uses) — whether or not they hold a
/// key. The gateway elevates only this person (`elevation.rs`,
/// `Refusal::NotTheOwner`), so a second platform-admin, however many
/// keys they hold, holds neither the platform owner nor the operator
/// tier (adversarial review of this car, H1: counting any admin made
/// both controls read HELD in a state where nobody could elevate).
///
/// The emergency delegate (DR readiness 62dac114, item 4) changes
/// nothing here, as the fold of design 1c4e42e1 says: a delegate hired
/// later is never the first hire, so the owner does not move; the
/// delegate holds exactly the policy pairs their standing grants hold.
///
/// `BOSS_PLATFORM_OWNER` plays no part, rightly: elevation does NOT
/// honour it (`elevation.rs`: "an environment variable must not decide
/// trust") — it changes only who a machine-filed packet is owned by, so
/// the owner these controls bind to is the roster's first hire alone.
pub fn platform_owner(roster: &[Person]) -> Option<&Person> {
    let admins: Vec<boss_core::platform_owner::Holder> = roster
        .iter()
        .filter(|p| p.active && p.role.as_deref() == Some(PLATFORM_ADMIN_ROLE))
        .map(|p| boss_core::platform_owner::Holder {
            id: p.id.clone(),
            hire_date: p.hire_date,
        })
        .collect();
    let owner = boss_core::platform_owner::first_hire(&admins)?;
    roster.iter().find(|p| p.id == owner)
}

/// Where the people service answers every employee's key counts by
/// tier (`{data: [{employee_id, user, operator}], total}`) — the one
/// read of passkey storage that is not the gateway's, and all the
/// coverage read needs of it. Spelled once, here in the contract crate
/// both sides already depend on: `boss-people` serves it, `boss-policy`
/// reads it.
pub const TIER_COUNTS_PATH: &str = "/api/people/webauthn-credentials/tiers";

/// One bound passkey: whose, and at which tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub employee_id: String,
    pub access_tier: AccessTier,
}

/// One control and the real people who hold it, in id order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub control: String,
    pub kind: String,
    pub wants: String,
    pub holders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
    /// A guard refusal's first future transition, absent on the current
    /// coverage report and on a control taken immediately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_expiry: Option<chrono::DateTime<chrono::Utc>>,
}

/// A control no real person holds. Named the way a refusal and an alarm
/// both read it: the control, its kind, what a holder needs, and the
/// remedy when it is more than a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orphan {
    pub control: String,
    pub kind: String,
    pub wants: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

/// The real people on a roster: active, with at least one bound key.
pub fn real_people<'a>(roster: &'a [Person], keys: &[Key]) -> Vec<&'a Person> {
    roster
        .iter()
        .filter(|p| {
            p.active
                && p.id
                    .parse::<boss_core::actor::ActorId>()
                    .is_ok_and(|actor| actor.is_human())
                && keys.iter().any(|k| k.employee_id == p.id)
        })
        .collect()
}

/// The scope `person` holds `action` on `resource` at: a live override
/// for that pair wins (a `none` override is a denial), else their role's
/// active rule. `None` when nothing grants it. `overrides` are the LIVE
/// ones — the repository lists only unexpired overrides, the same set
/// the engine consults.
fn held_scope(
    person: &Person,
    action: Action,
    resource: &str,
    rules: &[PolicyRule],
    overrides: &[UserOverride],
) -> Option<Scope> {
    let granted = |s: &Scope| (*s != Scope::None).then(|| s.clone());
    if let Some(ov) = overrides
        .iter()
        .find(|o| o.user_id == person.id && o.action == action && o.resource.as_str() == resource)
    {
        return granted(&ov.scope);
    }
    let role = person.role.as_deref()?;
    let id = rule_id(role, &crate::types::Resource::new(resource), action);
    rules
        .iter()
        .find(|r| r.id == id && r.active)
        .and_then(|r| granted(&r.scope))
}

/// Whether `person` holds the pair at the widest scope any active rule
/// grants for it (decision 2). With no active rule granting it, any
/// grant at all holds it — an override is then the only road. A pair
/// whose door admits only scope `all` is held only at `all`: the door
/// refuses anything narrower, whatever the widest rule is.
fn holds_pair(
    person: &Person,
    action: Action,
    resource: &str,
    all_only: bool,
    rules: &[PolicyRule],
    overrides: &[UserOverride],
) -> bool {
    let Some(scope) = held_scope(person, action, resource, rules, overrides) else {
        return false;
    };
    if all_only {
        return scope == Scope::All;
    }
    rules
        .iter()
        .filter(|r| r.active && r.action == action && r.resource.as_str() == resource)
        .filter(|r| r.scope != Scope::None)
        .all(|r| scope.contains(&r.scope))
}

fn holds(
    control: &Control,
    person: &Person,
    owner: Option<&str>,
    rules: &[PolicyRule],
    overrides: &[UserOverride],
    keys: &[Key],
) -> bool {
    let role = person.role.as_deref();
    let is_owner = owner == Some(person.id.as_str());
    match control {
        Control::Policy {
            action,
            resource,
            all_only,
        } => holds_pair(person, *action, resource, *all_only, rules, overrides),
        Control::Authority { role: wanted } => {
            role == Some(wanted.as_str())
                || held_scope(
                    person,
                    SignOff,
                    &format!("step-signoff:{wanted}"),
                    rules,
                    overrides,
                )
                .is_some()
                || held_scope(person, Update, "step-assign", rules, overrides).is_some()
        }
        Control::OperatorTier => {
            is_owner
                && keys
                    .iter()
                    .any(|k| k.employee_id == person.id && k.access_tier == AccessTier::Operator)
        }
        Control::PlatformOwner => is_owner,
        Control::Presence {
            roles, individual, ..
        } => {
            individual.as_deref() == Some(person.id.as_str())
                || role.is_some_and(|r| roles.iter().any(|x| x == r))
        }
    }
}

/// Every control and the real people who hold it, in the order given.
pub fn report(
    controls: &[Control],
    rules: &[PolicyRule],
    overrides: &[UserOverride],
    roster: &[Person],
    keys: &[Key],
) -> Vec<Held> {
    let people = real_people(roster, keys);
    let owner = platform_owner(roster).map(|p| p.id.as_str());
    controls
        .iter()
        .map(|c| Held {
            control: c.id(),
            kind: c.kind().to_string(),
            wants: c.wants(),
            holders: people
                .iter()
                .filter(|p| holds(c, p, owner, rules, overrides, keys))
                .map(|p| p.id.clone())
                .collect(),
            remedy: c.remedy().map(str::to_string),
            after_expiry: None,
        })
        .collect()
}

/// THE invariant, as one pure function (design 1c4e42e1 decision 7):
/// every control no real person holds. Empty is the covered state.
pub fn coverage(
    controls: &[Control],
    rules: &[PolicyRule],
    overrides: &[UserOverride],
    roster: &[Person],
    keys: &[Key],
) -> Vec<Orphan> {
    report(controls, rules, overrides, roster, keys)
        .into_iter()
        .filter(|h| h.holders.is_empty())
        .map(|h| Orphan {
            control: h.control,
            kind: h.kind,
            wants: h.wants,
            remedy: h.remedy,
        })
        .collect()
}

/// External holder facts for one proposed publication. Workflows are
/// deliberately absent: jobs supplies its own transaction's active rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageSnapshot {
    pub rules: Vec<PolicyRule>,
    pub overrides: Vec<UserOverride>,
    pub roster: Vec<Person>,
    pub keys: Vec<Key>,
}

impl CoverageSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if !self.roster.iter().any(|p| p.active) {
            return Err("coverage snapshot answered no active employee".into());
        }
        let mut people = std::collections::BTreeSet::new();
        if self
            .roster
            .iter()
            .any(|p| p.id.is_empty() || !people.insert(&p.id))
        {
            return Err("coverage snapshot has missing or duplicate employee identity".into());
        }
        Ok(())
    }

    /// Refuse new gaps, preserving an unrelated gap that existed already.
    pub fn judge_workflow(
        &self,
        before: &[WorkflowFacts],
        candidate: &WorkflowFacts,
    ) -> Result<(), String> {
        self.validate()?;
        let prior: std::collections::BTreeSet<_> = coverage(
            &controls(before),
            &self.rules,
            &self.overrides,
            &self.roster,
            &self.keys,
        )
        .into_iter()
        .map(|o| o.control)
        .collect();
        let mut after: Vec<_> = before
            .iter()
            .filter(|w| w.kind != candidate.kind)
            .cloned()
            .collect();
        let mut proposed = candidate.clone();
        proposed.status = Some("active".into());
        after.push(proposed);
        let added: Vec<_> = coverage(
            &controls(&after),
            &self.rules,
            &self.overrides,
            &self.roster,
            &self.keys,
        )
        .into_iter()
        .filter(|o| !prior.contains(&o.control))
        .collect();
        if added.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "workflow publication would orphan: {}. Add a real holder before publishing",
                added
                    .iter()
                    .map(|o| format!("{} ({})", o.control, o.wants))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

/// A whole external snapshot is acquired before any registry write lock.
#[async_trait::async_trait]
pub trait CoverageSnapshotSource: Send + Sync {
    async fn snapshot(&self) -> Result<CoverageSnapshot, String>;
}

pub const SNAPSHOT_PATH: &str = "/api/policy/coverage/snapshot";

pub struct HttpCoverageSnapshotSource {
    base: String,
    http: boss_core::machine_token::Client,
}

impl HttpCoverageSnapshotSource {
    pub fn new(base: impl Into<String>) -> Self {
        let (base, http) = boss_core::http_client::base(base);
        Self { base, http }
    }
}

#[async_trait::async_trait]
impl CoverageSnapshotSource for HttpCoverageSnapshotSource {
    async fn snapshot(&self) -> Result<CoverageSnapshot, String> {
        let mut response = self
            .http
            .get(format!("{}{SNAPSHOT_PATH}", self.base))
            .header(
                "x-boss-user",
                serde_json::to_string(&crate::User::service("jobs")).map_err(|e| e.to_string())?,
            )
            .send()
            .await
            .map_err(|e| format!("coverage snapshot unavailable: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("coverage snapshot answered {}", response.status()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err("coverage snapshot exceeds 2MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let snapshot: CoverageSnapshot = serde_json::from_slice(&bytes)
            .map_err(|e| format!("coverage snapshot malformed: {e}"))?;
        snapshot.validate()?;
        Ok(snapshot)
    }
}

// ---------------------------------------------------------------------------
// The guard: which controls a write takes the last holder from (car 3 of
// design 1c4e42e1; G1 of design b08725c2, 2026-09-30).
// ---------------------------------------------------------------------------

/// The policy table as it stands before a write, or as the write leaves
/// it: the active rules and the LIVE overrides of the roster's people.
#[derive(Debug, Clone, Copy)]
pub struct Table<'a> {
    pub rules: &'a [PolicyRule],
    pub overrides: &'a [UserOverride],
}

/// The live table at one expiry transition. All overrides sharing that
/// instant lapse together; a permanent deny still applies. No wall clock
/// belongs here: the caller hands over the overrides live at its snapshot.
fn overrides_after(
    table: Table<'_>,
    expiry: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<UserOverride> {
    table
        .overrides
        .iter()
        .filter(|o| expiry.is_none_or(|t| o.expires_at.is_none_or(|ends| ends > t)))
        .cloned()
        .collect()
}

/// Decision 3, as one pure function: every control a write would leave
/// held by no real person that some real person holds BEFORE the write
/// at that same instant — `orphaned(after) ⊄ orphaned(before)`.
/// Compare now and every distinct expiry in either table, in time order.
/// Checking only the endpoints missed a gap between a grant's expiry and
/// a later deny's expiry (review 9b8bacb5, 2026-10-02); combining gaps
/// across time would let an old gap excuse a new one at another stage.
/// Each refusal names the first offending transition and its prior holders.
/// Empty: the write may go. A gap that is there before the write and
/// after it is not the write's, so the operator-tier gap never blocks
/// its own repair.
pub fn orphaned_by(
    controls: &[Control],
    roster: &[Person],
    keys: &[Key],
    before: Table<'_>,
    after: Table<'_>,
) -> Vec<Held> {
    let expiries: std::collections::BTreeSet<_> = before
        .overrides
        .iter()
        .chain(after.overrides)
        .filter_map(|o| o.expires_at)
        .collect();
    let mut taken = std::collections::BTreeMap::new();
    for expiry in std::iter::once(None).chain(expiries.into_iter().map(Some)) {
        let was = overrides_after(before, expiry);
        let will = overrides_after(after, expiry);
        let missing: std::collections::BTreeSet<_> =
            coverage(controls, after.rules, &will, roster, keys)
                .into_iter()
                .map(|o| o.control)
                .collect();
        for mut held in report(controls, before.rules, &was, roster, keys) {
            if !held.holders.is_empty() && missing.contains(&held.control) {
                held.after_expiry = expiry;
                taken.entry(held.control.clone()).or_insert(held);
            }
        }
    }
    taken.into_values().collect()
}

/// The refusal a guard answers with, naming each control, whom the write
/// would take it from, and what a holder needs. There is no override
/// flag (decision 4): the way past is a second holder first, so the
/// refusal says so rather than naming a switch.
pub fn refusal(taken: &[Held]) -> String {
    let each: Vec<String> = taken
        .iter()
        .map(|h| {
            let from = if h.holders.is_empty() {
                "nobody whose hold outlasts its overrides".to_string()
            } else {
                h.holders.join(", ")
            };
            let remedy = h
                .remedy
                .as_deref()
                .map(|r| format!("; remedy: {r}"))
                .unwrap_or_default();
            let stage = h.after_expiry.map_or_else(
                || "now".to_string(),
                |t| format!("after expiry {}", t.to_rfc3339()),
            );
            format!(
                "{} ({stage}; held before the write by {from}; it wants {}{remedy})",
                h.control, h.wants
            )
        })
        .collect();
    format!(
        "this write would leave {} control(s) held by no real person, and no write may take a \
         control's last holder away (design 1c4e42e1): {}. There is no override: grant each \
         control to a second real person first — a rule on their role, or a user override \
         that never expires — and then make this write",
        taken.len(),
        each.join("; ")
    )
}

// ---------------------------------------------------------------------------
// The controls, derived: static pairs plus what the workflows declare.
// ---------------------------------------------------------------------------

/// One workflow as `GET /api/workflows` answers it — only what the
/// controls are read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowFacts {
    pub kind: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepFacts>,
}

/// One step of a workflow — the keys that make a control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepFacts {
    pub title: String,
    #[serde(default)]
    pub authority_role: Option<String>,
    #[serde(default)]
    pub sign_offs_required: Vec<String>,
    #[serde(default)]
    pub assurance_required: Option<String>,
    /// Externally tagged, as the registry writes it: `{"role": …}`,
    /// `{"individual": …}`, `{"station": …}`, `{"department": …}`.
    #[serde(default)]
    pub audience: Option<serde_json::Value>,
}

impl StepFacts {
    /// The role this step is for: its audience's, else its legacy
    /// `authority_role` — `StepSpec::selectors`' own order.
    fn role(&self) -> Option<String> {
        match &self.audience {
            Some(a) => a.get("role").and_then(|v| v.as_str()).map(str::to_string),
            None => self.authority_role.clone(),
        }
    }

    fn individual(&self) -> Option<String> {
        self.audience
            .as_ref()
            .and_then(|a| a.get("individual"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

/// Every control there is: the static pairs, the operator tier, the
/// platform owner, and what each ACTIVE workflow's steps declare — a
/// `sign-off` on `step-signoff:<role>` per required sign-off, an
/// authority per claimed role, a presence per presence-assured step.
/// Once each, in id order.
pub fn controls(workflows: &[WorkflowFacts]) -> Vec<Control> {
    let mut out: Vec<Control> = static_pairs()
        .into_iter()
        .map(|p| Control::Policy {
            action: p.action(),
            resource: p.resource_name().to_string(),
            all_only: p.all_only(),
        })
        .collect();
    out.push(Control::OperatorTier);
    out.push(Control::PlatformOwner);
    let active = workflows
        .iter()
        .filter(|w| w.status.as_deref().is_none_or(|s| s == "active"));
    for w in active {
        for s in &w.steps {
            out.extend(s.sign_offs_required.iter().map(|r| Control::Policy {
                action: SignOff,
                resource: format!("step-signoff:{r}"),
                all_only: false,
            }));
            let role = s.role();
            out.extend(role.iter().map(|r| Control::Authority { role: r.clone() }));
            if s.assurance_required.as_deref() == Some("presence") {
                let mut roles: Vec<String> = role.into_iter().collect();
                for r in &s.sign_offs_required {
                    if !roles.contains(r) {
                        roles.push(r.clone());
                    }
                }
                out.push(Control::Presence {
                    workflow: w.kind.clone(),
                    step: s.title.clone(),
                    roles,
                    individual: s.individual(),
                });
            }
        }
    }
    out.sort_by_key(Control::id);
    out.dedup_by_key(|c| c.id());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Resource;

    fn person(id: &str, role: &str) -> Person {
        Person {
            id: id.into(),
            role: Some(role.into()),
            active: true,
            hire_date: None,
        }
    }

    fn hired(id: &str, role: &str, on: &str) -> Person {
        Person {
            hire_date: Some(on.parse().unwrap()),
            ..person(id, role)
        }
    }

    fn key(id: &str, tier: AccessTier) -> Key {
        Key {
            employee_id: id.into(),
            access_tier: tier,
        }
    }

    fn pair(action: Action, resource: &str) -> Control {
        Control::Policy {
            action,
            resource: resource.into(),
            all_only: false,
        }
    }

    /// The control a door asking `p` declares — the same derivation
    /// [`controls`] makes of every const.
    fn declared(p: Pair) -> Control {
        Control::Policy {
            action: p.action(),
            resource: p.resource_name().into(),
            all_only: p.all_only(),
        }
    }

    fn rule(role: &str, resource: &str, action: Action, scope: Scope) -> PolicyRule {
        PolicyRule::new(role, Resource::new(resource), action, scope)
    }

    fn ids(orphans: &[Orphan]) -> Vec<&str> {
        orphans.iter().map(|o| o.control.as_str()).collect()
    }

    /// Decision 1: a real person is active AND holds a key. The system
    /// audit account (0 keys) and a terminated admin never count.
    #[test]
    fn a_real_person_is_active_with_a_bound_key() {
        let mut gone = person("emp-gone", PLATFORM_ADMIN_ROLE);
        gone.active = false;
        let roster = vec![
            person("emp-founder", PLATFORM_ADMIN_ROLE),
            person("emp-audit", "audit-readonly"),
            gone,
        ];
        let keys = vec![
            key("emp-founder", AccessTier::User),
            key("emp-gone", AccessTier::User),
        ];
        let real: Vec<&str> = real_people(&roster, &keys)
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(real, vec!["emp-founder"]);

        // So a control only the keyless account's role holds is orphaned.
        let rules = vec![rule("audit-readonly", "ledger", Action::Read, Scope::All)];
        let orphans = coverage(&[pair(Action::Read, "ledger")], &rules, &[], &roster, &keys);
        assert_eq!(ids(&orphans), vec!["policy:read:ledger"]);
    }

    /// Decision 2: held only at the widest scope any active rule grants.
    #[test]
    fn a_narrower_holder_does_not_hold_the_pair() {
        let roster = vec![person("emp-a", "rep")];
        let keys = vec![key("emp-a", AccessTier::User)];
        let c = [pair(Action::Read, "account")];
        let narrow = vec![
            rule("rep", "account", Action::Read, Scope::Territory),
            rule("director", "account", Action::Read, Scope::All),
        ];
        assert_eq!(
            ids(&coverage(&c, &narrow, &[], &roster, &keys)),
            vec!["policy:read:account"],
            "a territory holder cannot restore the all-scope grant"
        );
        // An inactive or scope-none wider rule does not raise the bar.
        let mut retired = rule("director", "account", Action::Read, Scope::All);
        retired.active = false;
        let equal = vec![
            rule("rep", "account", Action::Read, Scope::Territory),
            retired,
            rule("nobody", "account", Action::Read, Scope::None),
        ];
        assert!(coverage(&c, &equal, &[], &roster, &keys).is_empty());
    }

    /// The release review of car 1 (LOW, 2026-09-29): the estate door
    /// admits Read only at scope `all`. A tenant granting it only at
    /// `territory` has a widest rule of `territory`, so decision 2 alone
    /// read the territory holder as HELD while the door refused them.
    /// Held only at `all` now — and still held there.
    #[test]
    fn a_scope_all_door_is_held_only_at_scope_all() {
        use crate::controls::{READ_ACCOUNT, READ_ESTATE};
        let roster = vec![person("emp-a", "site-lead")];
        let keys = vec![key("emp-a", AccessTier::User)];
        let c = [declared(READ_ESTATE)];
        let narrow = vec![rule("site-lead", "estate", Action::Read, Scope::Territory)];
        let orphans = coverage(&c, &narrow, &[], &roster, &keys);
        assert_eq!(ids(&orphans), vec!["policy:read:estate"]);
        assert!(
            orphans[0].wants.contains("scope all"),
            "the orphan says why: {}",
            orphans[0].wants
        );

        let wide = vec![rule("site-lead", "estate", Action::Read, Scope::All)];
        assert!(coverage(&c, &wide, &[], &roster, &keys).is_empty());

        // A pair whose door admits any scope keeps decision 2.
        let accounts = vec![rule("site-lead", "account", Action::Read, Scope::Territory)];
        assert!(coverage(&[declared(READ_ACCOUNT)], &accounts, &[], &roster, &keys).is_empty());
    }

    /// Every scope-all const reaches coverage as scope-all.
    #[test]
    fn the_static_controls_carry_their_scope_all_flag() {
        let all_only: Vec<String> = controls(&[])
            .iter()
            .filter(|c| matches!(c, Control::Policy { all_only: true, .. }))
            .map(Control::id)
            .collect();
        let want: Vec<String> = CONTROLS
            .iter()
            .filter(|p| p.all_only())
            .map(|p| declared(*p).id())
            .collect();
        assert_eq!(all_only.len(), want.len());
        assert!(all_only.contains(&"policy:read:estate".to_string()));
    }

    /// A live override decides before the role rule, both ways.
    #[test]
    fn an_override_grants_or_denies_before_the_role_rule() {
        let roster = vec![person("emp-a", PLATFORM_ADMIN_ROLE)];
        let keys = vec![key("emp-a", AccessTier::User)];
        let c = [pair(Action::Update, "policy-rule")];
        let rules = vec![rule(
            PLATFORM_ADMIN_ROLE,
            "policy-rule",
            Action::Update,
            Scope::All,
        )];
        let deny = UserOverride {
            id: "ov-1".into(),
            user_id: "emp-a".into(),
            resource: Resource::policy_rule(),
            action: Action::Update,
            scope: Scope::None,
            reason: "lockout".into(),
            expires_at: None,
        };
        assert_eq!(
            ids(&coverage(
                &c,
                &rules,
                std::slice::from_ref(&deny),
                &roster,
                &keys
            )),
            vec!["policy:update:policy-rule"],
            "a scope-none override on the only holder orphans the pair"
        );
        let grant = UserOverride {
            scope: Scope::All,
            ..deny
        };
        assert!(coverage(&c, &[], &[grant], &roster, &keys).is_empty());
    }

    /// Decision 5: the operator tier is a control before any operator
    /// key exists — reported with 0 holders, naming the remedy.
    #[test]
    fn the_operator_tier_needs_an_admin_with_an_operator_key() {
        let roster = vec![
            person("emp-founder", PLATFORM_ADMIN_ROLE),
            person("emp-ops", "clerk"),
        ];
        let user_keys = vec![
            key("emp-founder", AccessTier::User),
            key("emp-ops", AccessTier::Operator),
        ];
        let orphans = coverage(&[Control::OperatorTier], &[], &[], &roster, &user_keys);
        assert_eq!(ids(&orphans), vec!["operator-tier"]);
        let remedy = orphans[0].remedy.as_deref().unwrap_or_default();
        assert!(remedy.contains("2a228d0c") && remedy.contains("1d9970d1"));

        let promoted = vec![key("emp-founder", AccessTier::Operator)];
        assert!(coverage(&[Control::OperatorTier], &[], &[], &roster, &promoted).is_empty());
    }

    /// Review H1, case 1: the founder (first hire, the one person the
    /// gateway elevates) holds only a user-tier key, and a platform-admin
    /// hired later holds an operator key. Nobody can reach operator, so
    /// the operator tier is ORPHANED — it read HELD before the fix.
    #[test]
    fn an_operator_key_on_a_later_admin_does_not_hold_the_operator_tier() {
        let roster = vec![
            hired("emp-founder", PLATFORM_ADMIN_ROLE, "2024-01-01"),
            hired("emp-second", PLATFORM_ADMIN_ROLE, "2026-09-01"),
        ];
        let keys = vec![
            key("emp-founder", AccessTier::User),
            key("emp-second", AccessTier::Operator),
        ];
        let c = [Control::OperatorTier, Control::PlatformOwner];
        assert_eq!(
            ids(&coverage(&c, &[], &[], &roster, &keys)),
            vec!["operator-tier"],
            "the owner holds the platform owner; only the owner's key covers the tier"
        );
        let promoted = vec![
            key("emp-founder", AccessTier::Operator),
            key("emp-second", AccessTier::Operator),
        ];
        let held = report(&c, &[], &[], &roster, &promoted);
        assert_eq!(
            held[0].holders,
            vec!["emp-founder"],
            "the owner, and only the owner"
        );
    }

    /// Review H1, case 2: the founder holds no key at all and a later
    /// admin does. `platform_owner` resolves the founder, who is not a
    /// real person, so the platform owner is ORPHANED — it read HELD.
    #[test]
    fn a_keyless_founder_leaves_the_platform_owner_orphaned() {
        let roster = vec![
            hired("emp-founder", PLATFORM_ADMIN_ROLE, "2024-01-01"),
            hired("emp-second", PLATFORM_ADMIN_ROLE, "2026-09-01"),
        ];
        let keys = vec![key("emp-second", AccessTier::User)];
        let orphans = coverage(&[Control::PlatformOwner], &[], &[], &roster, &keys);
        assert_eq!(ids(&orphans), vec!["platform-owner"]);
        assert!(
            orphans[0]
                .remedy
                .as_deref()
                .unwrap_or_default()
                .contains("first hire")
        );
    }

    /// The owner is the first HIRE, not the first id, and an undated hire
    /// sorts after every dated one — `first_hire`'s own ordering.
    #[test]
    fn the_owner_is_resolved_by_hire_order() {
        let roster = vec![
            hired("emp-b", PLATFORM_ADMIN_ROLE, "2020-01-01"),
            hired("emp-a", PLATFORM_ADMIN_ROLE, "2025-01-01"),
            person("emp-0", PLATFORM_ADMIN_ROLE),
            hired("emp-clerk", "clerk", "2010-01-01"),
        ];
        assert_eq!(
            platform_owner(&roster).map(|p| p.id.as_str()),
            Some("emp-b")
        );
        let mut gone = roster.clone();
        gone[0].active = false;
        assert_eq!(platform_owner(&gone).map(|p| p.id.as_str()), Some("emp-a"));
        assert!(platform_owner(&[hired("emp-clerk", "clerk", "2010-01-01")]).is_none());
    }

    /// The emergency delegate (62dac114 item 4), as the fold of design
    /// 1c4e42e1 accounts for it: a real person once their key is bound,
    /// holding exactly what their standing grants hold. Emergency-only
    /// (a role granted nothing) leaves the founder the sole holder of
    /// everything; a delegate with standing platform-admin is a second
    /// holder of the policy pairs, and still not the owner.
    #[test]
    fn the_emergency_delegate_holds_what_its_grants_hold_and_never_the_owner() {
        let rules = vec![rule(
            PLATFORM_ADMIN_ROLE,
            "class",
            Action::Create,
            Scope::All,
        )];
        let keys = vec![
            key("emp-founder", AccessTier::Operator),
            key("emp-delegate", AccessTier::User),
        ];
        let c = [
            pair(Action::Create, "class"),
            Control::OperatorTier,
            Control::PlatformOwner,
        ];
        for (role, class_holders) in [
            ("emergency-delegate", vec!["emp-founder"]),
            (PLATFORM_ADMIN_ROLE, vec!["emp-founder", "emp-delegate"]),
        ] {
            let roster = vec![
                hired("emp-founder", PLATFORM_ADMIN_ROLE, "2024-01-01"),
                hired("emp-delegate", role, "2026-09-29"),
            ];
            let held = report(&c, &rules, &[], &roster, &keys);
            assert_eq!(held[0].holders, class_holders, "{role}");
            assert_eq!(
                held[1].holders,
                vec!["emp-founder"],
                "{role}: operator tier"
            );
            assert_eq!(
                held[2].holders,
                vec!["emp-founder"],
                "{role}: platform owner"
            );
        }
    }

    #[test]
    fn the_claim_authority_is_held_three_ways() {
        let c = [Control::Authority {
            role: "workflow-approver".into(),
        }];
        let keys = vec![key("emp-a", AccessTier::User)];
        let as_role = vec![person("emp-a", "workflow-approver")];
        assert!(coverage(&c, &[], &[], &as_role, &keys).is_empty());
        let admin = vec![person("emp-a", PLATFORM_ADMIN_ROLE)];
        assert_eq!(
            ids(&coverage(&c, &[], &[], &admin, &keys)),
            vec!["authority:workflow-approver"]
        );
        for grant in [
            rule(
                PLATFORM_ADMIN_ROLE,
                "step-signoff:workflow-approver",
                Action::SignOff,
                Scope::All,
            ),
            rule(
                PLATFORM_ADMIN_ROLE,
                "step-assign",
                Action::Update,
                Scope::All,
            ),
        ] {
            assert!(coverage(&c, &[grant], &[], &admin, &keys).is_empty());
        }
    }

    /// The live shape of the four presence steps on 2026-09-29:
    /// authority platform-admin, sign-off platform-admin, no audience.
    #[test]
    fn a_workflow_declares_its_sign_off_authority_and_presence_controls() {
        let wf: Vec<WorkflowFacts> = serde_json::from_value(serde_json::json!([
            {"kind": "passkey-promotion", "status": "active", "steps": [
                {"title": "filed", "sign_offs_required": [], "authority_role": null},
                {"title": "authorise", "authority_role": "platform-admin",
                 "sign_offs_required": ["platform-admin"], "assurance_required": "presence"}]},
            {"kind": "design-doc", "status": "active", "steps": [
                {"title": "review", "audience": {"role": "workflow-approver"},
                 "sign_offs_required": ["workflow-approver"]}]},
            {"kind": "old", "status": "retired", "steps": [
                {"title": "x", "authority_role": "ghost-role"}]}
        ]))
        .unwrap();
        let all: Vec<String> = controls(&wf).iter().map(Control::id).collect();
        for want in [
            "authority:platform-admin",
            "authority:workflow-approver",
            "policy:sign-off:step-signoff:platform-admin",
            "policy:sign-off:step-signoff:workflow-approver",
            "presence:passkey-promotion:authorise",
            "operator-tier",
            "platform-owner",
            "policy:create:class",
        ] {
            assert!(all.iter().any(|c| c == want), "{want} missing from {all:?}");
        }
        assert!(
            !all.iter().any(|c| c.contains("ghost-role")),
            "a retired workflow declares nothing"
        );
        let mut sorted = all.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(all, sorted, "once each, in id order");

        // The presence control is held by a real person in its audience.
        let presence: Vec<Control> = controls(&wf)
            .into_iter()
            .filter(|c| c.kind() == "presence")
            .collect();
        let keys = vec![key("emp-a", AccessTier::User)];
        assert!(
            coverage(
                &presence,
                &[],
                &[],
                &[person("emp-a", PLATFORM_ADMIN_ROLE)],
                &keys
            )
            .is_empty()
        );
        assert_eq!(
            coverage(&presence, &[], &[], &[person("emp-a", "clerk")], &keys).len(),
            1
        );
    }

    /// The fresh instance: one founder, platform-admin, one user-tier
    /// key, the shipped defaults. Every control a door asks for is held
    /// by the founder — except the operator tier, which no default can
    /// give (it is a key, not a rule). A door that asks for a pair the
    /// defaults never grant platform-admin fails HERE, naming it: the
    /// 432f0eb4 gap (posting-rule create, absent until a rollout) was
    /// exactly that, found by hand.
    #[test]
    fn a_fresh_instance_founder_holds_every_control_but_the_operator_tier() {
        let founder = vec![person("emp-founder", PLATFORM_ADMIN_ROLE)];
        let keys = vec![key("emp-founder", AccessTier::User)];
        let wf: Vec<WorkflowFacts> = serde_json::from_value(serde_json::json!([
            {"kind": "design-doc", "status": "active", "steps": [
                {"title": "review", "authority_role": "workflow-approver",
                 "sign_offs_required": ["workflow-approver", "platform-admin"]}]}
        ]))
        .unwrap();
        let orphans = coverage(
            &controls(&wf),
            &crate::defaults::default_rules(),
            &[],
            &founder,
            &keys,
        );
        assert_eq!(ids(&orphans), vec!["operator-tier"], "{orphans:#?}");
    }

    // ----- the guard's judgement (car 3 of design 1c4e42e1, G1 of
    // design b08725c2): which controls a write takes the last holder from.

    fn table<'a>(rules: &'a [PolicyRule], overrides: &'a [UserOverride]) -> Table<'a> {
        Table { rules, overrides }
    }

    fn deny(user: &str, resource: &str, action: Action) -> UserOverride {
        UserOverride {
            id: format!("ov-{user}-{resource}-{}", action.as_str()),
            user_id: user.into(),
            resource: Resource::new(resource),
            action,
            scope: Scope::None,
            reason: "test".into(),
            expires_at: None,
        }
    }

    fn founder() -> (Vec<Person>, Vec<Key>) {
        (
            vec![person("emp-founder", PLATFORM_ADMIN_ROLE)],
            vec![key("emp-founder", AccessTier::User)],
        )
    }

    fn update_policy_rule() -> [Control; 1] {
        [pair(Action::Update, "policy-rule")]
    }

    fn admin_update(scope: Scope) -> PolicyRule {
        rule(PLATFORM_ADMIN_ROLE, "policy-rule", Action::Update, scope)
    }

    /// THE LOCKOUT the triage measured (47aed706): platform-admin setting
    /// its own `policy-rule:update` to scope none. The founder is the one
    /// holder, so the write takes the control from its last holder, and
    /// the judgement names the control and who holds it now.
    #[test]
    fn a_write_taking_the_last_holder_is_named() {
        let (roster, keys) = founder();
        let before = [admin_update(Scope::All)];
        let after = [admin_update(Scope::None)];
        let taken = orphaned_by(
            &update_policy_rule(),
            &roster,
            &keys,
            table(&before, &[]),
            table(&after, &[]),
        );
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].control, "policy:update:policy-rule");
        assert_eq!(taken[0].holders, vec!["emp-founder"], "who holds it now");
        let why = refusal(&taken);
        assert!(why.contains("policy:update:policy-rule"), "{why}");
        assert!(why.contains("emp-founder"), "{why}");
        assert!(
            why.contains("second"),
            "the way past is a second holder: {why}"
        );

        // Retiring the rule is the same lockout, and so is a scope-none
        // override on the founder.
        let mut retired = admin_update(Scope::All);
        retired.active = false;
        assert_eq!(
            orphaned_by(
                &update_policy_rule(),
                &roster,
                &keys,
                table(&before, &[]),
                table(&[retired], &[]),
            )
            .len(),
            1
        );
        let denied = [deny("emp-founder", "policy-rule", Action::Update)];
        assert_eq!(
            orphaned_by(
                &update_policy_rule(),
                &roster,
                &keys,
                table(&before, &[]),
                table(&before, &denied),
            )
            .len(),
            1
        );
    }

    /// Decision 3: a write that leaves an existing gap as it was passes —
    /// or the operator-tier gap would block its own repair — and a write
    /// that keeps a second holder passes.
    #[test]
    fn a_write_that_leaves_a_holder_or_an_old_gap_passes() {
        let (mut roster, mut keys) = founder();
        let c = [pair(Action::Update, "policy-rule"), Control::OperatorTier];
        let before = [admin_update(Scope::All)];
        // The operator tier is orphaned before AND after: not this write's.
        assert!(
            orphaned_by(&c, &roster, &keys, table(&before, &[]), table(&before, &[])).is_empty()
        );
        // A second real person holding it through an override of their
        // own: the founder may now be denied it.
        roster.push(person("emp-second", "clerk"));
        keys.push(key("emp-second", AccessTier::User));
        let second = UserOverride {
            scope: Scope::All,
            ..deny("emp-second", "policy-rule", Action::Update)
        };
        let denied = [
            second.clone(),
            deny("emp-founder", "policy-rule", Action::Update),
        ];
        assert!(
            orphaned_by(
                &c,
                &roster,
                &keys,
                table(&before, std::slice::from_ref(&second)),
                table(&before, &denied),
            )
            .is_empty(),
            "a second holder makes the founder's denial safe"
        );
    }

    /// With no real person at all — a fresh instance before its founder's
    /// passkey ceremony — nothing is held, so nothing can be taken: the
    /// tenant publish that seeds the rules is never refused by the guard.
    #[test]
    fn with_no_real_person_no_write_orphans_anything() {
        let roster = vec![person("emp-founder", PLATFORM_ADMIN_ROLE)];
        let before = [admin_update(Scope::All)];
        let after = [admin_update(Scope::None)];
        assert!(
            orphaned_by(
                &update_policy_rule(),
                &roster,
                &[],
                table(&before, &[]),
                table(&after, &[]),
            )
            .is_empty()
        );
    }

    /// A holder the write leaves only through an override that EXPIRES
    /// is not a holder the guard counts: the lockout would arrive by the
    /// clock, not by a write, and nothing would refuse it then. So a rule
    /// narrowed to none behind an hour's grant to the founder is refused,
    /// and so is a deny that lapses in a hundred years.
    #[test]
    fn a_holder_kept_only_until_an_override_lapses_is_not_kept() {
        let (roster, keys) = founder();
        let before = [admin_update(Scope::All)];
        let soon = chrono::Utc::now() + chrono::Duration::hours(1);
        let hours_grant = UserOverride {
            scope: Scope::All,
            expires_at: Some(soon),
            ..deny("emp-founder", "policy-rule", Action::Update)
        };
        let taken = orphaned_by(
            &update_policy_rule(),
            &roster,
            &keys,
            table(&before, &[]),
            table(&[admin_update(Scope::None)], &[hours_grant]),
        );
        assert_eq!(taken.len(), 1, "held for an hour is not held");

        let long_deny = UserOverride {
            expires_at: Some(chrono::Utc::now() + chrono::Duration::days(36_500)),
            ..deny("emp-founder", "policy-rule", Action::Update)
        };
        assert_eq!(
            orphaned_by(
                &update_policy_rule(),
                &roster,
                &keys,
                table(&before, &[]),
                table(&before, &[long_deny]),
            )
            .len(),
            1,
            "denied until an expiry is denied now"
        );
    }

    /// Lifting the founder's own lockout is a write the guard must let
    /// through: the control is orphaned before and held after.
    #[test]
    fn repairing_a_lockout_passes() {
        let (roster, keys) = founder();
        let rules = [admin_update(Scope::All)];
        let denied = [deny("emp-founder", "policy-rule", Action::Update)];
        assert!(
            orphaned_by(
                &update_policy_rule(),
                &roster,
                &keys,
                table(&rules, &denied),
                table(&rules, &[]),
            )
            .is_empty()
        );
    }

    #[test]
    fn the_static_pairs_are_the_doors_asks_once_each() {
        let pairs = static_pairs();
        boss_testing::assert_roster_floor!(pairs, 50);
        for (action, resource) in [
            (Action::Create, "class"),
            (Action::Create, "posting-rule"),
            (Action::Delete, "policy-rule"),
            (Action::Read, "compensation"),
        ] {
            assert!(
                pairs
                    .iter()
                    .any(|p| p.action() == action && p.resource_name() == resource),
                "{} {resource}",
                action.as_str()
            );
        }
        let mut seen = pairs.clone();
        seen.dedup();
        assert_eq!(seen.len(), pairs.len());
    }
}
#[cfg(test)]
mod expiry_guard_tests {
    use super::*;
    use chrono::{Duration, Utc};
    fn standing() -> (Vec<Person>, Vec<Key>, Vec<PolicyRule>, Vec<UserOverride>) {
        let roster = vec![
            Person {
                id: "emp-founder".into(),
                role: Some("platform-admin".into()),
                active: true,
                hire_date: None,
            },
            Person {
                id: "emp-second".into(),
                role: Some("second-holder".into()),
                active: true,
                hire_date: None,
            },
        ];
        let keys = roster
            .iter()
            .map(|p| Key {
                employee_id: p.id.clone(),
                access_tier: AccessTier::User,
            })
            .collect();
        let rules = vec![
            PolicyRule::new(
                "platform-admin",
                crate::types::Resource::policy_rule(),
                Action::Update,
                Scope::All,
            ),
            PolicyRule::new(
                "second-holder",
                crate::types::Resource::policy_rule(),
                Action::Update,
                Scope::All,
            ),
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let overrides = vec![
            UserOverride {
                id: "grant-one-hour".into(),
                user_id: "emp-founder".into(),
                resource: crate::types::Resource::policy_rule(),
                action: Action::Update,
                scope: Scope::All,
                reason: "handover".into(),
                expires_at: Some(now + Duration::hours(1)),
            },
            UserOverride {
                id: "deny-two-hours".into(),
                user_id: "emp-second".into(),
                resource: crate::types::Resource::policy_rule(),
                action: Action::Update,
                scope: Scope::None,
                reason: "handover".into(),
                expires_at: Some(now + Duration::hours(2)),
            },
        ];
        (roster, keys, rules, overrides)
    }
    #[test]
    fn a_write_must_not_create_a_gap_between_distinct_override_expirations() {
        let (roster, keys, before, overrides) = standing();
        let control = [Control::Policy {
            action: Action::Update,
            resource: "policy-rule".into(),
            all_only: true,
        }];
        let mut after = before.clone();
        after[0].scope = Scope::None;
        assert!(coverage(&control, &before, &overrides, &roster, &keys).is_empty());
        assert!(
            coverage(&control, &after, &overrides, &roster, &keys).is_empty(),
            "founder temporarily holds it now"
        );
        assert!(
            coverage(&control, &after, &[], &roster, &keys).is_empty(),
            "second holder recovers after both expirations"
        );
        let middle: Vec<_> = overrides
            .iter()
            .filter(|o| o.is_active_at(overrides[0].expires_at.unwrap() + Duration::minutes(30)))
            .cloned()
            .collect();
        assert!(
            coverage(&control, &before, &middle, &roster, &keys).is_empty(),
            "before the write the founder's permanent grant prevents the gap"
        );
        let missing = coverage(&control, &after, &middle, &roster, &keys);
        assert_eq!(missing.len(), 1, "one-hour gap follows the accepted write");
        let taken = orphaned_by(
            &control,
            &roster,
            &keys,
            Table {
                rules: &before,
                overrides: &overrides,
            },
            Table {
                rules: &after,
                overrides: &overrides,
            },
        );
        assert!(
            !taken.is_empty(),
            "guard accepts both endpoints but leaves policy:update:policy-rule unheld between the 1h grant expiry and 2h deny expiry"
        );
    }
    #[test]
    fn a_handover_with_equal_expirations_has_no_intermediate_gap() {
        let (roster, keys, before, mut overrides) = standing();
        overrides[1].expires_at = overrides[0].expires_at;
        let control = [Control::Policy {
            action: Action::Update,
            resource: "policy-rule".into(),
            all_only: true,
        }];
        let mut after = before.clone();
        after[0].scope = Scope::None;
        assert!(coverage(&control, &after, &overrides, &roster, &keys).is_empty());
        assert!(coverage(&control, &after, &[], &roster, &keys).is_empty());
        assert!(
            orphaned_by(
                &control,
                &roster,
                &keys,
                Table {
                    rules: &before,
                    overrides: &overrides
                },
                Table {
                    rules: &after,
                    overrides: &overrides
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_preexisting_gap_at_another_stage_does_not_hide_a_new_gap() {
        let (roster, keys, before, mut overrides) = standing();
        overrides[0].scope = Scope::None;
        let control = [Control::Policy {
            action: Action::Update,
            resource: "policy-rule".into(),
            all_only: true,
        }];
        let mut after = before.clone();
        after[0].scope = Scope::None;
        assert_eq!(
            coverage(&control, &before, &overrides, &roster, &keys).len(),
            1
        );
        let taken = orphaned_by(
            &control,
            &roster,
            &keys,
            Table {
                rules: &before,
                overrides: &overrides,
            },
            Table {
                rules: &after,
                overrides: &overrides,
            },
        );
        assert_eq!(
            taken.len(),
            1,
            "the old gap now must not excuse the new gap after the first expiry"
        );
        let text = refusal(&taken);
        assert!(text.contains("policy:update:policy-rule"), "{text}");
        assert!(
            text.contains(&overrides[0].expires_at.unwrap().to_rfc3339()),
            "the receipt must name the first offending expiry: {text}"
        );
        assert!(
            text.contains("emp-founder"),
            "the holder before that expiry must be named: {text}"
        );
        let mut reversed = overrides.clone();
        reversed.reverse();
        assert_eq!(
            taken,
            orphaned_by(
                &control,
                &roster,
                &keys,
                Table {
                    rules: &before,
                    overrides: &reversed
                },
                Table {
                    rules: &after,
                    overrides: &reversed
                }
            ),
            "expiry enumeration and receipt order are deterministic"
        );
    }

    #[test]
    fn an_unchanged_gap_and_its_partial_repair_still_pass() {
        let (roster, keys, mut before, mut overrides) = standing();
        before[0].scope = Scope::None;
        overrides[0].scope = Scope::None;
        let control = [Control::Policy {
            action: Action::Update,
            resource: "policy-rule".into(),
            all_only: true,
        }];
        let original = Table {
            rules: &before,
            overrides: &overrides,
        };
        assert!(orphaned_by(&control, &roster, &keys, original, original).is_empty());
        let mut repaired = before.clone();
        repaired[0].scope = Scope::All;
        assert!(
            orphaned_by(
                &control,
                &roster,
                &keys,
                original,
                Table {
                    rules: &repaired,
                    overrides: &overrides
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn shortening_a_temporary_grant_compares_both_tables_expirations() {
        let (roster, keys, mut rules, before) = standing();
        rules[0].scope = Scope::None;
        let control = [Control::Policy {
            action: Action::Update,
            resource: "policy-rule".into(),
            all_only: true,
        }];
        let mut after = before.clone();
        after[0].expires_at = before[0].expires_at.map(|t| t - Duration::minutes(30));
        let taken = orphaned_by(
            &control,
            &roster,
            &keys,
            Table {
                rules: &rules,
                overrides: &before,
            },
            Table {
                rules: &rules,
                overrides: &after,
            },
        );
        assert_eq!(
            taken.len(),
            1,
            "a later preexisting gap must not hide the new earlier gap"
        );
        assert!(refusal(&taken).contains(&after[0].expires_at.unwrap().to_rfc3339()));
    }
}
