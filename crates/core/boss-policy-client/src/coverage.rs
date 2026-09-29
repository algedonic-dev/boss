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
//! - a policy pair — static ones declared here in [`DOORS`], dynamic ones
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
//! their own scope, so a narrower holder cannot restore it.
//!
//! THIS CAR REFUSES NOTHING. The coverage core, its read and the backstop
//! ship now and only report; the guards that would refuse a write that
//! orphans a control wait for DR readiness (62dac114) — "a refusal with
//! no recovery path is worse than the gap".

use serde::{Deserialize, Serialize};

use crate::types::{AccessTier, Action, PolicyRule, Scope, UserOverride, rule_id};
use boss_core::roles::PLATFORM_ADMIN_ROLE;

// ---------------------------------------------------------------------------
// The static controls — what each door file asks policy for.
// ---------------------------------------------------------------------------

/// One `(action, resource)` pair a door asks policy for, spelled so a
/// `const` can hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pair {
    pub action: Action,
    pub resource: &'static str,
}

const fn ask(action: Action, resource: &'static str) -> Pair {
    Pair { action, resource }
}

/// One source file that asks policy, how many asks the pin's scan sees
/// in it, and the pairs those asks resolve to.
///
/// THE STATIC PAIR LIST IS DECLARED ONCE, HERE (design 1c4e42e1
/// decision 8, CLAUDE.md §9a). A door's resource usually rides a helper
/// (`authorize(&state, &user, Action::Update)` means `class` in
/// boss-classes), so no scan can resolve it and the list is written by
/// hand — and pinned: `tests/every_door_is_a_declared_control.rs` reruns
/// the triage's scan (every `Action::<verb>`, `.scope_predicate(` and
/// `asks!(` outside tests and comments) and names every file whose count
/// differs from `mentions` or whose ask lines — and the resource tokens
/// beside them — no longer hash to `digest`, or that asks and is not
/// listed. Either means a door was added, moved, removed or changed:
/// resolve what it asks, correct `asks`, set the two values the failure
/// prints. The collapse the design names next —
/// every door asking through a `Control` const, so the compiler is the
/// pin — is a later car; until then this table is the holding action.
///
/// Measured on origin/main 10bddfaf (2026-09-29): 150 mentions in 40
/// files; the triage's resolution (run 60497b5d, at f04f7cf7) re-read
/// for the four files that moved since.
#[derive(Debug, Clone, Copy)]
pub struct Door {
    pub file: &'static str,
    pub mentions: usize,
    /// FNV-1a of the file's ask lines and the resource tokens beside
    /// them, as the pin computes it — so an ask swapped in place, which
    /// leaves `mentions` unchanged, still fails (review of this car, L1).
    pub digest: &'static str,
    pub asks: &'static [Pair],
}

use Action::{Close, Create, Delete, Publish, Read, Retire, SignOff, Update};

/// Every door file, and what it asks. A file whose asks are EMPTY is
/// plumbing (the policy client's own helpers) or asks for a resource
/// only data names (a View, a search facet) — the design's "named as
/// unmeasured, not covered".
pub const DOORS: &[Door] = &[
    Door {
        file: "crates/core/boss-calendar/src/http.rs",
        mentions: 5,
        digest: "8697fbf37dc33397",
        asks: &[
            ask(Create, "schedule"),
            ask(Delete, "schedule"),
            ask(Create, "business-calendar"),
            ask(Update, "business-calendar"),
        ],
    },
    Door {
        file: "crates/core/boss-classes/src/http.rs",
        mentions: 5,
        digest: "1d1994bab11f9e20",
        asks: &[
            ask(Create, "class"),
            ask(Update, "class"),
            ask(Retire, "class"),
        ],
    },
    // A file's target decides the resource: job, step, or a Subject
    // (read as `account`); the audit read is `policy-rule`.
    Door {
        file: "crates/core/boss-content/src/files/http.rs",
        mentions: 7,
        digest: "0388b6e49998a6e9",
        asks: &[
            ask(Read, "job"),
            ask(Update, "job"),
            ask(Read, "step"),
            ask(Update, "step"),
            ask(Read, "account"),
            ask(Update, "account"),
            ask(Read, "policy-rule"),
        ],
    },
    Door {
        file: "crates/core/boss-dispatcher/src/http.rs",
        mentions: 4,
        digest: "d37815044a2940c4",
        asks: &[
            ask(Read, "job"),
            ask(Create, "dispatcher-rule"),
            ask(Publish, "dispatcher-rule"),
            ask(Retire, "dispatcher-rule"),
        ],
    },
    Door {
        file: "crates/core/boss-jobs/src/cadence/http.rs",
        mentions: 2,
        digest: "4d18e5eadbe35367",
        asks: &[ask(Publish, "workflow"), ask(Retire, "workflow")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/jobs.rs",
        mentions: 11,
        digest: "14d3c0fca00aae18",
        asks: &[
            ask(Read, "job"),
            ask(Update, "job"),
            ask(Close, "job"),
            ask(Read, "workflow"),
            ask(Update, "workflow"),
            ask(Publish, "workflow"),
            // The estate readers' gate, `estate_read_refusal` (train #805).
            ask(Read, "estate"),
        ],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/kinds.rs",
        mentions: 13,
        digest: "d86917ced6d28f7a",
        asks: &[
            ask(Read, "workflow"),
            ask(Create, "workflow"),
            ask(Update, "workflow"),
            ask(Publish, "workflow"),
            ask(Retire, "workflow"),
        ],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/mod.rs",
        mentions: 1,
        digest: "30ac134c2198b71d",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/moves.rs",
        mentions: 1,
        digest: "3f676098b4ccc06c",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/plugins.rs",
        mentions: 11,
        digest: "18e51c9da866d2eb",
        asks: &[
            ask(Read, "step-plugin"),
            ask(Create, "step-plugin"),
            ask(Update, "step-plugin"),
            ask(Publish, "step-plugin"),
            ask(Retire, "step-plugin"),
            ask(Read, "step"),
        ],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/queue_age.rs",
        mentions: 1,
        digest: "772f551d423b2bf1",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/regions.rs",
        mentions: 1,
        digest: "4ece82220886201f",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/routes.rs",
        mentions: 1,
        digest: "34b9c59c486d4cc8",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/rule_firings.rs",
        mentions: 1,
        digest: "772f551d423b2bf1",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/stations.rs",
        mentions: 7,
        digest: "744e7af11c14f8f5",
        asks: &[
            ask(Read, "job"),
            ask(Read, "workflow"),
            ask(Create, "workflow"),
            ask(Update, "workflow"),
        ],
    },
    // The sign-off door and the claim door also ask `sign-off` on
    // `step-signoff:<role>` for a role only a workflow names — those are
    // the dynamic controls [`controls`] reads off the workflows.
    Door {
        file: "crates/core/boss-jobs/src/http/steps.rs",
        mentions: 9,
        digest: "fe47ef85b7d61bf6",
        asks: &[
            ask(Update, "step"),
            ask(Update, "job"),
            ask(Update, "step-assign"),
        ],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/terminal_report.rs",
        mentions: 1,
        digest: "64f478af82c31b4d",
        asks: &[ask(Read, "workflow")],
    },
    Door {
        file: "crates/core/boss-jobs/src/http/yard.rs",
        mentions: 1,
        digest: "772f551d423b2bf1",
        asks: &[ask(Read, "job")],
    },
    // `create` on `job` is the all-kinds grant the door takes first;
    // `job:<kind>` is the per-kind grant a tenant may add beside it, so
    // a holder of `create job` holds every kind (design 222fc982).
    Door {
        file: "crates/core/boss-jobs/src/open_authority.rs",
        mentions: 2,
        digest: "5a1b0049c3673c9d",
        asks: &[ask(Create, "job")],
    },
    Door {
        file: "crates/core/boss-jobs/src/scheduling/access.rs",
        mentions: 1,
        digest: "0f69460ef8f67877",
        asks: &[ask(Read, "schedule")],
    },
    Door {
        file: "crates/core/boss-jobs/src/scheduling/http.rs",
        mentions: 7,
        digest: "7738844a4568702e",
        asks: &[
            ask(Create, "schedule"),
            ask(Update, "schedule"),
            ask(Delete, "schedule"),
        ],
    },
    Door {
        file: "crates/core/boss-locations/src/http.rs",
        mentions: 1,
        digest: "11fd26fd8b9b00ed",
        asks: &[ask(Create, "location")],
    },
    // The client's own helpers: `scope_predicate` is `check(Read)` on
    // the resource its CALLER names, and the caller's file is the door.
    Door {
        file: "crates/core/boss-policy-client/src/engine.rs",
        mentions: 1,
        digest: "0107313527a6731e",
        asks: &[],
    },
    Door {
        file: "crates/core/boss-policy-client/src/lib.rs",
        mentions: 3,
        digest: "b4e502ff07b66680",
        asks: &[],
    },
    Door {
        file: "crates/core/boss-policy/src/authority.rs",
        mentions: 14,
        digest: "413dc6e85edf631b",
        asks: &[
            ask(Read, "policy-rule"),
            ask(Create, "policy-rule"),
            ask(Update, "policy-rule"),
            ask(Delete, "policy-rule"),
        ],
    },
    Door {
        file: "crates/core/boss-policy/src/http.rs",
        mentions: 5,
        digest: "28e13194622ca93d",
        asks: &[ask(Read, "policy-rule"), ask(Delete, "policy-rule")],
    },
    // Search reads `job`, and a facet's resource is data (unmeasured).
    Door {
        file: "crates/core/boss-search/src/query.rs",
        mentions: 2,
        digest: "0c434e51df38fbc0",
        asks: &[ask(Read, "job")],
    },
    Door {
        file: "crates/core/boss-subject-kinds/src/http.rs",
        mentions: 1,
        digest: "f7b1fe1d7fb7a83c",
        asks: &[ask(Update, "subject-kind")],
    },
    // A View names its own resource: data, unmeasured.
    Door {
        file: "crates/core/boss-views/src/query.rs",
        mentions: 1,
        digest: "3c00822ae37dab92",
        asks: &[],
    },
    Door {
        file: "crates/modules/boss-assets/src/http.rs",
        mentions: 1,
        digest: "c1d85f61a1a028e8",
        asks: &[ask(Update, "asset")],
    },
    Door {
        file: "crates/modules/boss-commerce/src/agreements.rs",
        mentions: 2,
        digest: "3ee72b0181519d34",
        asks: &[ask(Create, "agreement"), ask(Update, "agreement")],
    },
    Door {
        file: "crates/modules/boss-commerce/src/http.rs",
        mentions: 6,
        digest: "74fbd2d6d5d922e5",
        asks: &[ask(Create, "invoice"), ask(Update, "invoice")],
    },
    // The four `asks!` extractors, the router-wide read, and the
    // declaration helper the chart (`accounts.rs`) and the tax registry
    // (`tax_registry.rs`) call with their own resource.
    Door {
        file: "crates/modules/boss-ledger/src/http.rs",
        mentions: 6,
        digest: "6661d9267eaa6dc3",
        asks: &[
            ask(Read, "ledger"),
            ask(Create, "ledger"),
            ask(Update, "ledger"),
            ask(Create, "posting-rule"),
            ask(Create, "tax-regime"),
            ask(Create, "ledger-account"),
        ],
    },
    Door {
        file: "crates/modules/boss-ledger/src/http/periods.rs",
        mentions: 4,
        digest: "2f83d8a0ddcf1708",
        asks: &[
            ask(Create, "ledger-period"),
            ask(Update, "ledger-period"),
            ask(Close, "ledger-period"),
        ],
    },
    Door {
        file: "crates/modules/boss-people/src/employee_changes.rs",
        mentions: 1,
        digest: "2f24dced9ac3bc17",
        asks: &[ask(Update, "employee")],
    },
    Door {
        file: "crates/modules/boss-people/src/grants.rs",
        mentions: 3,
        digest: "0a4562c0393e423f",
        asks: &[ask(Read, "employee"), ask(Read, "compensation")],
    },
    Door {
        file: "crates/modules/boss-people/src/http.rs",
        mentions: 3,
        digest: "9c2ea39630e83810",
        asks: &[
            ask(Create, "employee"),
            ask(Update, "employee"),
            ask(Delete, "employee"),
        ],
    },
    Door {
        file: "crates/modules/boss-people/src/pto.rs",
        mentions: 1,
        digest: "e8e69b4dcc1ab753",
        asks: &[ask(Create, "schedule")],
    },
    Door {
        file: "crates/modules/boss-people/src/requisitions.rs",
        mentions: 1,
        digest: "7c1522e49d8fe9c2",
        asks: &[ask(Create, "employee")],
    },
    Door {
        file: "crates/modules/boss-people/src/workflows.rs",
        mentions: 4,
        digest: "38452f602d897705",
        asks: &[ask(Update, "employee")],
    },
];

/// The static pairs — every pair any [`DOORS`] entry asks, once each,
/// ordered by resource then action.
pub fn static_pairs() -> Vec<Pair> {
    let mut out: Vec<Pair> = Vec::new();
    for pair in DOORS.iter().flat_map(|d| d.asks.iter()) {
        if !out.contains(pair) {
            out.push(*pair);
        }
    }
    out.sort_by_key(|p| (p.resource, p.action.as_str()));
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
    /// A door asks policy for `action` on `resource`.
    Policy { action: Action, resource: String },
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
            Self::Policy { action, resource } => format!("policy:{}:{resource}", action.as_str()),
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
            Self::Policy { action, resource } => format!(
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
        .filter(|p| p.active && keys.iter().any(|k| k.employee_id == p.id))
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
/// grant at all holds it — an override is then the only road.
fn holds_pair(
    person: &Person,
    action: Action,
    resource: &str,
    rules: &[PolicyRule],
    overrides: &[UserOverride],
) -> bool {
    let Some(scope) = held_scope(person, action, resource, rules, overrides) else {
        return false;
    };
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
        Control::Policy { action, resource } => {
            holds_pair(person, *action, resource, rules, overrides)
        }
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
            action: p.action,
            resource: p.resource.to_string(),
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
                pairs.contains(&ask(action, resource)),
                "{} {resource}",
                action.as_str()
            );
        }
        let mut seen = pairs.clone();
        seen.dedup();
        assert_eq!(seen.len(), pairs.len());
    }
}
