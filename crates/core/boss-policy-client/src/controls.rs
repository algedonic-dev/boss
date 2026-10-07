//! Every static control a door asks for, declared once — and the only
//! way a door asks for one (design 1c4e42e1 decision 8; backlog
//! 47aed706).
//!
//! WHY CONSTS. Car 1 of the design declared the static pair list by
//! hand, one `Door` per source file, and pinned it with a scan that
//! counted and hashed every ask line — a holding action (CLAUDE.md §9a),
//! because a door's resource usually rode a helper no scan could
//! resolve. This is the collapse the design named next: a door asks
//! through a [`Pair`] const declared here, and the `controls!` line that
//! declares the const is the same line that puts it in [`CONTROLS`], the
//! list coverage reads. So a door cannot ask for a static pair the
//! coverage read does not know about — to ask for a new one it must add
//! a line here, and that line IS the declaration. The compiler is the
//! pin: [`Pair`] has no public constructor, so a const from this module
//! is the only `Pair` there is.
//!
//! What still asks with a raw `Action` is a door whose resource is data
//! — `step-signoff:<role>`, `job:<kind>`, a View's source — plus the
//! policy service's own judge of a rule write, whose verb is the
//! write's; `tests/every_door_is_a_declared_control.rs` holds that
//! residue to a short, reasoned list, and refuses a static pair asked
//! raw.
//!
//! SCOPE-ALL DOORS (the release review of car 1, LOW, 2026-09-29). Some
//! doors admit a grant only at scope `all`, because what they act on has
//! no owner a narrower scope could match: the estate, a policy rule, the
//! commerce and asset writes that carry no row predicate, every registry
//! row written through `writes::require_registry_write`, and the
//! schedule materialiser, which writes every employee's week. Coverage
//! counted a holder at the widest scope any rule grants, so a tenant
//! granting Read on `estate` only at `territory` read as HELD while the
//! door refused every caller. A const declared `all` tells coverage so,
//! and coverage then holds the pair only at scope `all`.
//!
//! The flag DECLARES; it never decides. Each door still refuses a
//! narrower grant the way it did — its own `Scope::All` match, or the
//! registry ladder's — so no edit to a flag can widen a door; the worst
//! a missing flag does is the LOW this closes, a control read as held
//! that no one can use. `the_scope_all_doors_are_declared_scope_all`
//! below holds the set, naming the door that makes each one so. When a
//! pair is asked by several doors, the strictest decides: `create` on
//! `schedule` is `all` because the materialiser is, though a person's
//! own availability takes `self`. There is no exception to that rule,
//! and three pairs read as one until the release review of car 2
//! (005eb5b4, LOW-2) named them: `read` on `step` and on `policy-rule`
//! are `all` although the file and audit doors (boss-content) admit any
//! scope, because the in-flight count and the rule-table reads admit
//! only `all`; and `read` on `job` is `all` although every listing
//! filters by scope, because the moves record serves only `all`.

use crate::types::{Action, Resource};

/// One `(action, resource)` pair a door asks policy for, and whether some
/// door admits it only at scope `all`. Only a const in this module is
/// one: there is no public constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pair {
    action: Action,
    resource: &'static str,
    all_only: bool,
}

impl Pair {
    const fn declare(action: Action, resource: &'static str, all_only: bool) -> Self {
        Self {
            action,
            resource,
            all_only,
        }
    }

    pub const fn action(&self) -> Action {
        self.action
    }

    /// The resource, spelled as the rule table stores it.
    pub const fn resource_name(&self) -> &'static str {
        self.resource
    }

    pub fn resource(&self) -> Resource {
        Resource::new(self.resource)
    }

    /// True when some door asking this pair admits a grant only at
    /// scope `all` — and so coverage holds it only at `all`.
    pub const fn all_only(&self) -> bool {
        self.all_only
    }
}

/// Declares each control as a `pub const` AND lists it in [`CONTROLS`],
/// from one line: `NAME: Verb "resource";`, or `NAME: Verb "resource",
/// all;` for a pair some door admits only at scope `all`.
macro_rules! controls {
    ($($(#[$doc:meta])* $name:ident: $action:ident $resource:literal $(, $all:ident)?;)*) => {
        $(
            $(#[$doc])*
            pub const $name: Pair = Pair::declare(
                Action::$action,
                $resource,
                controls!(@all $($all)?),
            );
        )*
        /// Every static control, in declaration order — each const above,
        /// once. Coverage reads this list; nothing else declares one.
        pub const CONTROLS: &[Pair] = &[$($name),*];
    };
    (@all) => { false };
    (@all all) => { true };
}

controls! {
    // account — a file attached to a Subject is read and written as
    // `account` (boss-content files).
    READ_ACCOUNT: Read "account";
    UPDATE_ACCOUNT: Update "account";

    // agreement and invoice — commerce writes with no row predicate, so
    // a narrower grant is refused (backlog f922edea).
    CREATE_AGREEMENT: Create "agreement", all;
    UPDATE_AGREEMENT: Update "agreement", all;
    CREATE_INVOICE: Create "invoice", all;
    UPDATE_INVOICE: Update "invoice", all;

    // asset — the same, for the asset write (backlog f922edea).
    UPDATE_ASSET: Update "asset", all;

    CREATE_BUSINESS_CALENDAR: Create "business-calendar", all;
    UPDATE_BUSINESS_CALENDAR: Update "business-calendar", all;

    CREATE_CLASS: Create "class", all;
    UPDATE_CLASS: Update "class", all;
    RETIRE_CLASS: Retire "class", all;

    READ_COMPENSATION: Read "compensation";

    CREATE_DISPATCHER_RULE: Create "dispatcher-rule", all;
    PUBLISH_DISPATCHER_RULE: Publish "dispatcher-rule", all;
    RETIRE_DISPATCHER_RULE: Retire "dispatcher-rule", all;

    READ_EMPLOYEE: Read "employee";
    CREATE_EMPLOYEE: Create "employee";
    UPDATE_EMPLOYEE: Update "employee";
    DELETE_EMPLOYEE: Delete "employee";

    /// Raw audit-log rows as a result set: global search reads them only
    /// on an unrestricted grant (boss-search `unrestricted_read`). This
    /// and READ_SUBJECT were missing from car 1's hand table, which read
    /// search's two asks as data-named facets; asking through a const
    /// is what surfaced them (2026-09-29).
    READ_EVENT: Read "event", all;

    /// No estate row has an owner a narrower scope could match, so a
    /// scoped read would answer an empty estate (train #805).
    READ_ESTATE: Read "estate", all;

    /// Every packet listing filters by the reader's scope, but the moves
    /// record (boss-jobs `moves.rs` `refused`) names every packet that
    /// moved and serves only a reader at scope `all` — so, the strictest
    /// deciding, this is `all` (review 005eb5b4 of car 2, LOW-2).
    READ_JOB: Read "job", all;
    /// The ALL-KINDS grant opening a packet takes first; `job:<kind>`
    /// is the per-kind grant beside it (design 222fc982), so a holder of
    /// this holds every kind.
    CREATE_JOB: Create "job";
    UPDATE_JOB: Update "job";
    CLOSE_JOB: Close "job";

    READ_LEDGER: Read "ledger";
    CREATE_LEDGER: Create "ledger", all;
    UPDATE_LEDGER: Update "ledger", all;
    CREATE_LEDGER_ACCOUNT: Create "ledger-account", all;
    CREATE_LEDGER_PERIOD: Create "ledger-period", all;
    UPDATE_LEDGER_PERIOD: Update "ledger-period", all;
    CLOSE_LEDGER_PERIOD: Close "ledger-period", all;

    CREATE_LOCATION: Create "location", all;

    /// A policy rule belongs to the whole deployment — no owner, team,
    /// territory or department — so only scope `all` reads or writes one
    /// (`boss-policy` `authority::may`).
    READ_POLICY_RULE: Read "policy-rule", all;
    CREATE_POLICY_RULE: Create "policy-rule", all;
    UPDATE_POLICY_RULE: Update "policy-rule", all;
    DELETE_POLICY_RULE: Delete "policy-rule", all;

    CREATE_POSTING_RULE: Create "posting-rule", all;

    READ_SCHEDULE: Read "schedule";
    CREATE_SCHEDULE: Create "schedule", all;
    UPDATE_SCHEDULE: Update "schedule";
    DELETE_SCHEDULE: Delete "schedule", all;

    /// The in-flight count spans every packet's steps, so that door
    /// reads steps only at scope `all`; a file on a step reads it too.
    READ_STEP: Read "step", all;
    UPDATE_STEP: Update "step";

    /// The claim door's third route to a step (`claimant_holds_authority`).
    UPDATE_STEP_ASSIGN: Update "step-assign";

    READ_STEP_PLUGIN: Read "step-plugin";
    CREATE_STEP_PLUGIN: Create "step-plugin";
    UPDATE_STEP_PLUGIN: Update "step-plugin";
    PUBLISH_STEP_PLUGIN: Publish "step-plugin";
    RETIRE_STEP_PLUGIN: Retire "step-plugin";

    /// Subjects as a search result: all-or-nothing, like events.
    READ_SUBJECT: Read "subject", all;

    UPDATE_SUBJECT_KIND: Update "subject-kind", all;

    CREATE_TAX_REGIME: Create "tax-regime", all;

    READ_WORKFLOW: Read "workflow";
    CREATE_WORKFLOW: Create "workflow";
    UPDATE_WORKFLOW: Update "workflow";
    PUBLISH_WORKFLOW: Publish "workflow";
    RETIRE_WORKFLOW: Retire "workflow";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two consts for one pair would be one control counted twice, and
    /// could disagree on whether it is scope-all.
    #[test]
    fn every_control_is_declared_once() {
        boss_testing::assert_roster_floor!(CONTROLS, 50);
        let mut seen: Vec<(Action, &str)> = Vec::new();
        for c in CONTROLS {
            let key = (c.action(), c.resource_name());
            assert!(
                !seen.contains(&key),
                "{} {} is declared twice",
                c.action().as_str(),
                c.resource_name()
            );
            seen.push(key);
        }
    }

    /// The scope-all set, measured 2026-09-29 (the release review of car
    /// 1, LOW), each with the door that refuses a narrower Allow. A pair
    /// leaves this list only when its door stops refusing — never to make
    /// a control read as held.
    #[test]
    fn the_scope_all_doors_are_declared_scope_all() {
        let doors: [(Pair, &str); 34] = [
            (READ_ESTATE, "boss-jobs estate_read_refusal"),
            (READ_JOB, "boss-jobs the moves record (moves.rs refused)"),
            (READ_POLICY_RULE, "boss-policy authority::may"),
            (CREATE_POLICY_RULE, "boss-policy authority::may"),
            (UPDATE_POLICY_RULE, "boss-policy authority::may"),
            (DELETE_POLICY_RULE, "boss-policy authority::may"),
            (UPDATE_ASSET, "boss-assets require_asset_update_on"),
            (CREATE_INVOICE, "boss-commerce require_on"),
            (UPDATE_INVOICE, "boss-commerce require_on"),
            (CREATE_AGREEMENT, "boss-commerce require_on"),
            (UPDATE_AGREEMENT, "boss-commerce require_on"),
            (READ_STEP, "boss-jobs the in-flight plugin count"),
            (CREATE_CLASS, "the registry ladder: boss-classes"),
            (UPDATE_CLASS, "the registry ladder: boss-classes"),
            (RETIRE_CLASS, "the registry ladder: boss-classes"),
            (CREATE_LOCATION, "the registry ladder: boss-locations"),
            (
                UPDATE_SUBJECT_KIND,
                "the registry ladder: boss-subject-kinds",
            ),
            (
                CREATE_BUSINESS_CALENDAR,
                "the registry ladder: boss-calendar",
            ),
            (
                UPDATE_BUSINESS_CALENDAR,
                "the registry ladder: boss-calendar",
            ),
            (
                CREATE_DISPATCHER_RULE,
                "the registry ladder: boss-dispatcher",
            ),
            (
                PUBLISH_DISPATCHER_RULE,
                "the registry ladder: boss-dispatcher",
            ),
            (
                RETIRE_DISPATCHER_RULE,
                "the registry ladder: boss-dispatcher",
            ),
            (CREATE_LEDGER, "the registry ladder: boss-ledger"),
            (UPDATE_LEDGER, "the registry ladder: boss-ledger"),
            (CREATE_POSTING_RULE, "the registry ladder: boss-ledger"),
            (CREATE_TAX_REGIME, "the registry ladder: boss-ledger"),
            (CREATE_LEDGER_ACCOUNT, "the registry ladder: boss-ledger"),
            (
                CREATE_LEDGER_PERIOD,
                "the registry ladder: boss-ledger periods",
            ),
            (
                UPDATE_LEDGER_PERIOD,
                "the registry ladder: boss-ledger periods",
            ),
            (
                CLOSE_LEDGER_PERIOD,
                "the registry ladder: boss-ledger periods",
            ),
            (CREATE_SCHEDULE, "boss-jobs the schedule materialiser"),
            (DELETE_SCHEDULE, "boss-calendar cancel-by-reason"),
            (READ_EVENT, "boss-search unrestricted_read"),
            (READ_SUBJECT, "boss-search unrestricted_read"),
        ];
        for (pair, door) in doors {
            assert!(
                pair.all_only(),
                "{} {} is refused below scope all by {door}, and is not declared all",
                pair.action().as_str(),
                pair.resource_name()
            );
        }
        let declared = CONTROLS.iter().filter(|c| c.all_only()).count();
        assert_eq!(
            declared,
            doors.len(),
            "a const is declared all that no door listed here refuses below all"
        );
    }

    #[test]
    fn a_const_is_the_pair_it_names() {
        assert_eq!(CREATE_CLASS.action(), Action::Create);
        assert_eq!(CREATE_CLASS.resource(), Resource::class());
        assert_eq!(UPDATE_STEP_ASSIGN.resource(), Resource::step_assign());
        assert_eq!(READ_POLICY_RULE.resource(), Resource::policy_rule());
    }
}
