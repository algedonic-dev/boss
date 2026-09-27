//! Default policy rules seeded at service startup.
//!
//! Core ships only the **platform-level** rules every BOSS deployment
//! needs to run:
//!
//! - `platform-admin` — the operator who owns the deployment. Broad
//!   Read across every shipped resource + Create/Update/Publish/
//!   Retire/Delete on the registry resources (`policy-rule`,
//!   `workflow`, `step-plugin`) that govern how the platform behaves.
//! - `audit-readonly` — the external auditor, and the anonymous guest
//!   on an instance that opts in to the system-audit read
//!   (`BOSS_GUEST_ACCESS=audit`, the playground). Strictly Read on
//!   every shipped resource.
//! - `smoke-tester` — fixture role for the boss-testing harness.
//!   Read-only mirror of `audit-readonly`; isolated so a misconfigured
//!   test can't accidentally drift external-auditor expectations.
//! - `guest` — the unauth landing surface. Strictly
//!   `workflow` Read; no other resource.
//! - `visitor` — the anonymous guest on an install offering basic
//!   guest access, the OSS default (design 2830b6b7). Read on
//!   `workflow` and `step-plugin` — the operating model the install
//!   has published; drafts are read only by their authors — and
//!   nothing else until the install grants more.
//!
//! Tenant role grants (sales-rep, service-tech, controllers, the
//! C-suite, department managers, …) live in **tenant seed data**, not
//! here. The 2026-05-24 tier-purity pass moved the prior ~365-line
//! used-device-shop org chart out of core — it was wrong on every
//! non-device-shop deployment (e.g. the brewery's role set never got
//! these grants), and it tied core's release cadence to one tenant's
//! HR model. Tenants seed their role matrix at first boot via the
//! `boss-policy-bootstrap` binary, which reads
//! `examples/<tenant>/seeds/policy_rules.toml` and POSTs each rule
//! to `/api/policy/rules`. See [`crate::seed_loader`] for the TOML
//! schema.
//!
//! Operators can edit any rule via the admin API and their changes
//! survive restarts: `bootstrap_reconcile` only refreshes rows whose
//! `updated_by = 'bootstrap'`. Operator-tuned rows are preserved.

use crate::types::{Action, PolicyRule as Rule, Resource, Scope};

/// The 13 resources the platform's `default_rules` enumerate Read
/// access over. Modules and tenants introduce their own via
/// `Resource::new("specimen")` and seed grants through the admin API.
pub fn shipped_resources() -> Vec<Resource> {
    vec![
        Resource::job(),
        Resource::step(),
        Resource::account(),
        Resource::employee(),
        Resource::invoice(),
        Resource::agreement(),
        Resource::asset(),
        Resource::shipment(),
        Resource::part(),
        Resource::purchase_order(),
        Resource::policy_rule(),
        Resource::workflow(),
        Resource::step_plugin(),
        // Result-set access to the log and to identity. Adding them
        // here is what makes the search/View gates data-driven: every
        // platform role's grant below is generated from this list, so
        // `audit-readonly` picks up Read on both without a special
        // case, and a role with no grant is denied by default.
        Resource::event(),
        Resource::subject(),
        // The finance read surface. Platform-admin gets everything and
        // audit-readonly gets Read from the loops below; tenants grant
        // it to their finance roles.
        Resource::ledger(),
    ]
}

pub fn default_rules() -> Vec<Rule> {
    use Action::*;
    let mut rules = Vec::new();
    let resources = shipped_resources();

    // ------------------------------------------------------------------
    // Platform admin — the operator running the BOSS deployment itself.
    // Broad **every-action** grant across every shipped resource. This
    // is the deploy-time superuser: it walks `workflow-design` meta-
    // Jobs to register tenant Workflows, runs `boss-policy-bootstrap`
    // to seed tenant role grants, runs `boss-brewery-data-seed` to
    // populate Subject rosters, and tunes any policy-rule after launch.
    //
    // Day-to-day business writes (a brewing batch's repair step, a
    // refurb-tech's job closure) still come from real employees with
    // their tenant roles. Those grants live in
    // `examples/<tenant>/seeds/policy_rules.toml`, not here.
    // ------------------------------------------------------------------
    for r in &resources {
        for action in [
            Read, Create, Update, Close, SignOff, Publish, Retire, Delete,
        ] {
            rules.push(Rule::new("platform-admin", r.clone(), action, Scope::All));
        }
    }

    // Step sign-off authority is enforced through policy against a
    // role-scoped `step-signoff:<role>` resource (see
    // `boss-jobs::http::update_step`). The deploy superuser keeps SignOff
    // on `step-signoff:platform-admin` (the `design-doc-review` approval
    // step still requires it) AND on `step-signoff:workflow-approver` —
    // the operational-leadership capability the `workflow-design` approval
    // step now requires. Tenants grant `workflow-approver` to their
    // C-suite/COO/dept-heads in `examples/<tenant>/seeds/policy_rules.toml`
    // so authoring a work-type isn't gated solely on the deploy operator;
    // the bare `step` grant above does NOT cover these role-scoped
    // resources.
    for authority in ["platform-admin", "workflow-approver"] {
        rules.push(Rule::new(
            "platform-admin",
            Resource::new(format!("step-signoff:{authority}")),
            SignOff,
            Scope::All,
        ));
    }

    // Starting a step for someone else (design 611fbffd, Q1 answered
    // 2026-09-26): the claim door installs a holder other than its
    // caller only for the step's declared executor or a holder of this
    // authority — platform-admin today. Not shipped, so the read-only
    // roles do not inherit it.
    rules.push(Rule::new(
        "platform-admin",
        Resource::step_assign(),
        Update,
        Scope::All,
    ));

    // Pay (backlog c7484d0e, 2026-09-23). Not a shipped resource, so
    // none of the read-only roles below inherit it — the auditor role
    // is also what an anonymous visitor carries. The deploy superuser
    // reads it here; tenants grant their HR and finance roles in
    // `examples/<tenant>/seeds/policy_rules.toml`.
    rules.push(Rule::new(
        "platform-admin",
        Resource::compensation(),
        Read,
        Scope::All,
    ));

    // Schedules (backlog a621d091, 2026-09-25) — not shipped, for the
    // same reason as pay: every employee reads their own without a
    // grant, the deploy superuser reads all of them here, and tenants
    // grant their managers in their own seed.
    rules.push(Rule::new(
        "platform-admin",
        Resource::schedule(),
        Read,
        Scope::All,
    ));
    // Writing one. Booking PTO is Create (backlog dda8fd97, 2026-09-27:
    // `POST /api/people/pto` took no caller); so is reserving time on the
    // calendar or adding availability, an assignment or a shift, a status
    // is Update, and a cancel or a removal is Delete (backlog 11721a25:
    // the calendar's reservations and the scheduling writes took no
    // caller either). The deploy superuser holds them so the routes are
    // not dead on an install that has written no HR or manager grants,
    // and so a sibling service (the jobs API's step hook, the people
    // API's PTO) signs as it after authorising its own caller; tenants
    // grant their HR roles and managers, and an install offering
    // self-service grants `self`.
    for action in [Create, Update, Delete] {
        rules.push(Rule::new(
            "platform-admin",
            Resource::schedule(),
            action,
            Scope::All,
        ));
    }

    // ------------------------------------------------------------------
    // Audit-readonly — external auditors / the seeded `emp-audit`
    // login / the anonymous guest on an instance opted in to the
    // system-audit read (the OSS default guest is `visitor`, below). Read on every shipped resource;
    // never Create/Update/Close/Publish/Retire/SignOff.
    //
    // The audit_log's own tail + integrity checkpoints are still
    // out-of-band (boss-events tail-http + journal export). What IS
    // gated now is being handed log rows back as a result set —
    // `Resource::event()` above — because global search and Views
    // both do exactly that, and both shipped doing it for anyone who
    // asked.
    // ------------------------------------------------------------------
    for r in &resources {
        rules.push(Rule::new("audit-readonly", r.clone(), Read, Scope::All));
    }

    // ------------------------------------------------------------------
    // Smoke-tester — fixture role for the boss-testing harness.
    // Reserved for `emp-smoke` (seeded by the schema). Mirrors
    // audit-readonly's rule matrix; isolated as a separate role so a
    // misconfigured smoke test can't accidentally drift production
    // external-auditor expectations.
    // ------------------------------------------------------------------
    for r in &resources {
        rules.push(Rule::new("smoke-tester", r.clone(), Read, Scope::All));
    }

    // ------------------------------------------------------------------
    // Guest — the unauth landing surface. The gateway forwards
    // `GET /api/workflows*` without a session; the
    // jobs-api then sees role `guest`, and this rule lets it answer.
    // Strictly read-only, strictly workflow.
    // ------------------------------------------------------------------
    rules.push(Rule::new("guest", Resource::workflow(), Read, Scope::All));

    // ------------------------------------------------------------------
    // Visitor — the anonymous guest on an install offering BASIC guest
    // access, the OSS default (design 2830b6b7, decision 4, 2026-09-25).
    // It reads the operating model — the protocols (`workflow`) and the
    // step surfaces that render them (`step-plugin`) — and nothing of the
    // company's record: no packets, people, accounts, books, events,
    // subjects or policy rules. "The operating model" is the protocols
    // this install PUBLISHED, active and retired. It is not "already
    // public in the repo": an install's own protocols are not in any
    // repo, and its unpublished drafts of workflows and stations are its
    // authors' workspace, which Read does not open — a draft needs
    // Create, Update or Publish on `workflow` (backlog 1a4a4d03,
    // `boss-jobs` http/kinds.rs `may_read_drafts`). Nor does Read on
    // `step-plugin` count anyone's work: the in-flight count asks Read
    // on `step` at scope all. The install widens it with `visitor` rows in its own
    // `policy_rules.toml`; widening is one row, and a stranger's read
    // cannot be taken back, so the default starts narrow. An instance
    // wanting the full audit read mints `audit-readonly` instead
    // (`BOSS_GUEST_ACCESS=audit`), not a wider `visitor`.
    // ------------------------------------------------------------------
    for r in [Resource::workflow(), Resource::step_plugin()] {
        rules.push(Rule::new(
            boss_core::roles::VISITOR_ROLE,
            r,
            Read,
            Scope::All,
        ));
    }

    // ------------------------------------------------------------------
    // Break-glass — the emergency session minted by the gateway's
    // hardware-key ceremony (docs/design/break-glass-is-a-key-you-
    // hold.md, Q4: NARROW). Exactly three levers, mapped onto the
    // resources that express them today:
    //
    // - **Deploy rollback** → Create/Update/Close on `job` +
    //   Update on `step`: file and drive a rollback / regenerate-
    //   deployment packet, claim and complete its steps, cancel a
    //   wedged train. (The kube-apiserver half of this lever is the
    //   PIV applet on the same key — outside BOSS policy by design.)
    // - **Merge approval** → SignOff on `step-signoff:platform-admin`:
    //   the platform-authority stamp an emergency merge-approval step
    //   requires. Honestly noted: this is the nearest real resource
    //   and is wider than merge-only — it satisfies ANY step whose
    //   sign-off names platform-admin authority. There is no
    //   narrower merge-approval resource today; minting a dead
    //   `step-signoff:break-glass` no workflow requires would grant
    //   nothing.
    // - **Auth administration** → Create/Update on `policy-rule`:
    //   repair a broken grant that caused the lockout. The gateway-
    //   side half (onboard credentials, issue resets) is
    //   `boss_core::roles::can_administer_auth`, which names this
    //   role explicitly.
    //
    // Reads are the working set for those three verbs and nothing
    // more: job/step (the packets being driven), workflow (what the
    // packets instantiate), event (the audit trail during an
    // incident), policy-rule (what auth administration edits). No
    // ledger, no accounts, no employees — an emergency key is a door
    // key, not a data key.
    // ------------------------------------------------------------------
    for r in [
        Resource::job(),
        Resource::step(),
        Resource::workflow(),
        Resource::event(),
        Resource::policy_rule(),
    ] {
        rules.push(Rule::new("break-glass", r, Read, Scope::All));
    }
    for action in [Create, Update, Close] {
        rules.push(Rule::new(
            "break-glass",
            Resource::job(),
            action,
            Scope::All,
        ));
    }
    rules.push(Rule::new(
        "break-glass",
        Resource::step(),
        Update,
        Scope::All,
    ));
    rules.push(Rule::new(
        "break-glass",
        Resource::new("step-signoff:platform-admin"),
        SignOff,
        Scope::All,
    ));
    for action in [Create, Update] {
        rules.push(Rule::new(
            "break-glass",
            Resource::policy_rule(),
            action,
            Scope::All,
        ));
    }

    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_rule_has_unique_id() {
        let rules = default_rules();
        boss_testing::assert_roster_floor!(
            rules,
            100,
            "core's default policy rules (175 on 2026-09-11)"
        );
        let mut ids = std::collections::HashSet::new();
        for r in &rules {
            assert!(
                ids.insert(r.id.clone()),
                "duplicate default rule id: {}",
                r.id
            );
        }
    }

    #[test]
    fn platform_admin_reads_every_resource() {
        let rules = default_rules();
        let reads: Vec<_> = rules
            .iter()
            .filter(|r| r.role == "platform-admin" && r.action == Action::Read)
            .collect();
        assert!(
            reads.len() >= 13,
            "platform-admin should have Read on every projection resource, got {}",
            reads.len()
        );
        for r in reads {
            assert_eq!(r.scope, Scope::All, "platform-admin reads are unrestricted");
        }
    }

    #[test]
    fn audit_readonly_only_has_read_grants() {
        let rules = default_rules();
        // The FILTERED set is what this ranges over, so that is what needs
        // the floor: a role whose grants all vanished would satisfy
        // "never has a non-Read action" perfectly.
        let granted: Vec<_> = rules
            .iter()
            .filter(|r| r.role == "audit-readonly")
            .collect();
        boss_testing::assert_roster_floor!(
            granted,
            10,
            "audit-readonly's grants (16 on 2026-09-11)"
        );
        for r in granted {
            assert_eq!(
                r.action,
                Action::Read,
                "audit-readonly must never have non-Read actions; got {:?} on {:?}",
                r.action,
                r.resource
            );
        }
    }

    #[test]
    fn no_tenant_role_grants_in_core() {
        // The 2026-05-24 tier-purity pass moved the C-suite and the
        // department/IC role grants out of core. Pin that.
        let rules = default_rules();
        // "No tenant role appears" is true of no rules at all, which is
        // the one way this could go quiet.
        boss_testing::assert_roster_floor!(
            rules,
            100,
            "core's default policy rules (175 on 2026-09-11)"
        );
        let banned = [
            "ceo",
            "coo",
            "cto",
            "cfo",
            "vp-sales",
            "sales-mgr",
            "sales-rep",
            "service-mgr",
            "service-tech",
            "refurb-supervisor",
            "refurb-tech",
            "qa-lead",
            "qa-tech",
            "warehouse-mgr",
            "warehouse-clerk",
            "parts-buyer",
            "controller",
            "ap-specialist",
            "hr-generalist",
            "recruiter",
            "support-specialist",
            "it-manager",
        ];
        for role in banned {
            let leak: Vec<_> = rules.iter().filter(|r| r.role == role).collect();
            assert!(
                leak.is_empty(),
                "tenant role `{role}` leaked into core defaults — move to tenant seed"
            );
        }
    }

    /// Q4 (break-glass-is-a-key-you-hold): the emergency role's grant
    /// set is EXACTLY the three levers plus their working-set reads.
    /// This is an equality pin, not a floor — a new grant sneaking in
    /// here is precisely the drift the narrow-role decision exists to
    /// prevent, so the test names the full set.
    #[test]
    fn break_glass_grants_are_exactly_the_three_levers() {
        let rules = default_rules();
        let mut got: Vec<String> = rules
            .iter()
            .filter(|r| r.role == "break-glass")
            .map(|r| format!("{}:{}", r.resource.as_str(), r.action.as_str()))
            .collect();
        got.sort();
        let mut want = vec![
            // working-set reads
            "job:read".to_string(),
            "step:read".to_string(),
            "workflow:read".to_string(),
            "event:read".to_string(),
            "policy-rule:read".to_string(),
            // deploy rollback
            "job:create".to_string(),
            "job:update".to_string(),
            "job:close".to_string(),
            "step:update".to_string(),
            // merge approval
            "step-signoff:platform-admin:sign-off".to_string(),
            // auth administration
            "policy-rule:create".to_string(),
            "policy-rule:update".to_string(),
        ];
        want.sort();
        assert_eq!(got, want, "break-glass grants drifted from Q4's narrow set");
        for r in rules.iter().filter(|r| r.role == "break-glass") {
            assert_eq!(r.scope, Scope::All, "break-glass scopes are All: {}", r.id);
        }
    }

    /// The narrow role must never touch the data surfaces: no ledger,
    /// no accounts, no employees, and no Delete anywhere. A break-
    /// glass key opens doors; it does not read the books.
    #[test]
    fn break_glass_never_reaches_data_surfaces_or_delete() {
        let rules = default_rules();
        let granted: Vec<_> = rules.iter().filter(|r| r.role == "break-glass").collect();
        boss_testing::assert_roster_floor!(granted, 8, "break-glass's grants (12 on 2026-09-11)");
        for r in granted {
            for banned in ["ledger", "account", "employee", "invoice", "subject"] {
                assert_ne!(
                    r.resource.as_str(),
                    banned,
                    "break-glass gained a data-surface grant: {}",
                    r.id
                );
            }
            assert_ne!(
                r.action,
                Action::Delete,
                "break-glass must never Delete: {}",
                r.id
            );
        }
    }

    #[test]
    fn guest_only_reads_workflows() {
        let rules = default_rules();
        let guest: Vec<_> = rules.iter().filter(|r| r.role == "guest").collect();
        assert_eq!(guest.len(), 1);
        assert_eq!(guest[0].resource, Resource::workflow());
        assert_eq!(guest[0].action, Action::Read);
    }

    /// Design 2830b6b7, decision 4 (2026-09-25): the OSS default guest
    /// reads the operating model and nothing else — Read on `workflow`
    /// and `step-plugin`, the protocols the install has published (its
    /// drafts stay with their authors, backlog 1a4a4d03).
    /// An equality pin, not a floor: every other grant is the
    /// install's to add as a `visitor` row in its own seed, because a
    /// stranger's read cannot be taken back, and `policy-rule` above
    /// all stays out — the rule table is the install's security map.
    #[test]
    fn visitor_reads_only_the_operating_model() {
        let rules = default_rules();
        let mut got: Vec<String> = rules
            .iter()
            .filter(|r| r.role == boss_core::roles::VISITOR_ROLE)
            .map(|r| format!("{}:{}", r.resource.as_str(), r.action.as_str()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec!["step-plugin:read".to_string(), "workflow:read".to_string()],
            "the basic guest's default grant drifted from the operating model"
        );
        for r in rules
            .iter()
            .filter(|r| r.role == boss_core::roles::VISITOR_ROLE)
        {
            assert_eq!(r.scope, Scope::All, "visitor scopes are All: {}", r.id);
        }
    }

    /// Pay is read by grant (backlog c7484d0e, 2026-09-23). The deploy
    /// superuser holds it; the auditor role — which is also what an
    /// audit-opted instance's guest and the seeded `emp-audit` login carry —
    /// does not, which is why `compensation` is kept OUT of
    /// `shipped_resources`, whose Read every read-only role inherits.
    #[test]
    fn only_platform_admin_reads_compensation_by_default() {
        let holders: Vec<_> = default_rules()
            .into_iter()
            .filter(|r| r.resource == Resource::compensation())
            .map(|r| (r.role, r.action, r.scope))
            .collect();
        assert_eq!(
            holders,
            vec![("platform-admin".to_string(), Action::Read, Scope::All)]
        );
        assert!(!shipped_resources().contains(&Resource::compensation()));
    }

    /// Schedules the same way (backlog a621d091, 2026-09-25): the
    /// guest session read every employee's week while the scheduling
    /// reads asked no policy, and shipping the resource would hand it
    /// straight back to the read-only roles it inherits to. The writes —
    /// booking PTO (backlog dda8fd97), reserving time, availability,
    /// assignments, shift patterns (backlog 11721a25) — are the deploy
    /// superuser's alone the same way until a tenant grants its HR roles
    /// and managers, and it is what a sibling service signs as when it
    /// writes for a caller it has already authorised. Each held ONCE: two
    /// cars each added Create in the same hunk on 2026-09-27.
    #[test]
    fn only_platform_admin_holds_schedules_by_default() {
        let holders: Vec<_> = default_rules()
            .into_iter()
            .filter(|r| r.resource == Resource::schedule())
            .map(|r| (r.role, r.action, r.scope))
            .collect();
        assert_eq!(
            holders,
            vec![
                ("platform-admin".to_string(), Action::Read, Scope::All),
                ("platform-admin".to_string(), Action::Create, Scope::All),
                ("platform-admin".to_string(), Action::Update, Scope::All),
                ("platform-admin".to_string(), Action::Delete, Scope::All),
            ]
        );
        assert!(!shipped_resources().contains(&Resource::schedule()));
    }
}
