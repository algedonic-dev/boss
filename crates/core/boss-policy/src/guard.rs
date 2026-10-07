//! The policy-write guard: no rule or override write may leave a control
//! held by no real person (car 3 of design 1c4e42e1, backlog 47aed706;
//! G1 of design b08725c2, 2026-09-30).
//!
//! WHY. David, 2026-09-28: "guards to make sure users, especially the
//! platform-admin can't create a policy or security configuration that
//! permanently locks any controls". The triage measured the lockout it
//! names: platform-admin could set its own `policy-rule:update` to scope
//! none — `judge_rule` skips its holds check for a write that grants
//! nothing — and `DELETE /api/policy/rules/{id}` was judged by nobody.
//! One real person held all 60 pairs a door asks for, so either write
//! would have left the table unrepairable except through break-glass.
//!
//! WHAT IT DECIDES. Every rule write (POST, PUT, DELETE) and override
//! write (POST, DELETE) is judged inside the adapter's transaction, on
//! the row it changes, by `boss_policy_client::coverage::orphaned_by`:
//! the table before the write against the table the write leaves, over
//! the roster, the passkeys and the active workflows read here BEFORE
//! the transaction opens — the read-before, decide-inside split
//! `crate::authority` already takes. A write that takes a control from
//! its last real holder is REFUSED 409, naming each control and whom it
//! would take it from. A gap there before and after is not the write's
//! (decision 3), so the operator-tier gap never blocks its own repair.
//!
//! NO OVERRIDE, AND NO ROLE IS EXEMPT (decision 4). Platform-admin is
//! bound like everyone: it is the actor most able to lock itself out.
//! The way past a refusal is a second holder first. Break-glass is
//! exempt only when it restores a rule core ships exactly — the one rule
//! write `authority::judge_rule` lets it make.
//!
//! A DENY'S RETIREMENT NEEDS NO READ (adversarial review 782257de, B1).
//! Retiring a scope-none override can never take a holder away: the user
//! falls back to their role rule, the widest-rule bar reads rules only,
//! and no other kind of control reads overrides. So [`retires_a_deny`]
//! passes it on the row the adapter read inside the transaction, with no
//! cross-service read — for the founder and for break-glass alike, with
//! every source dark. The first build read the sources for it, so a deny
//! on the guard's own reader (which made `/api/workflows` answer 403)
//! turned every human policy write, its own repair included, into a 503.
//! That override is now refused at the door ([`is_the_guards_reader`]).
//!
//! A READ THAT CANNOT BE TRUSTED IS A REFUSAL, NOT A PASS. A people or
//! jobs service that errors leaves the write unjudgeable, so it is
//! refused 503 naming the source, and nothing is written. A roster that
//! answers no active employee, or no bound passkey, is read the same way
//! (review 782257de, N1): a wrong target answers instead of erroring, so
//! an empty answer cannot be told from a fresh instance by the answer.
//! The fresh instance is recognised by what its seeding WRITES instead:
//! `boss tenant publish` posts policy before the roster exists and before
//! the founder's passkey ceremony, and every write it makes is a NEW rule
//! — which takes no one's hold (only a rule's narrowing or retirement,
//! or an override, can). So with no real person readable, a new rule
//! passes, named as such, and every other write is refused 503.
//!
//! NOT ON THE BOOT PATH. `bootstrap_reconcile` writes through the port
//! with no door and no judge, as it did; this guard sits only on the
//! HTTP write doors, so a restart never waits on the people or jobs API.
//!
//! LIMITS, STATED. The rules and overrides are read before the
//! transaction and only the written row inside it, so two concurrent
//! writes each safe alone can together orphan a control — the backstop
//! (`every-control-has-a-real-person-hourly`) files it on its next tick.
//! A new rule can still raise a pair's widest-rule bar (review N2), which
//! the blind path does not see. And a people write (a role change, a
//! termination, a revoked key) is not judged here: that is the people
//! guard, car 4 (G2 of b08725c2).
//!
//! ROLLBACK. A revert car: the train writes no policy rows, so a
//! misjudging guard cannot stop its own removal from boarding. While the
//! jobs API is dark the train cannot run, and the road is the LAN kube
//! road: roll boss-policy to the last converged build before this car.

use chrono::{DateTime, Utc};

use boss_policy_client::coverage::{self, Control, Held, Key, Person, Table};
use boss_policy_client::port::PolicyRepository;
use boss_policy_client::types::{PolicyRule, Scope, UserOverride};

use crate::coverage::{COVERAGE_READER_ID, CoverageSources};

/// Everything the guard reads before a write's transaction opens.
#[derive(Debug, Clone)]
pub struct Standing {
    controls: Vec<Control>,
    roster: Vec<Person>,
    keys: Vec<Key>,
    rules: Vec<PolicyRule>,
    overrides: Vec<UserOverride>,
    /// Why no real person could be read, when none could: an empty
    /// roster, or one with no bound passkey (review 782257de, N1).
    blind: Option<String>,
}

/// A source the guard could not read, named for the 503.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dark(pub String);

/// Why the guard refused a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The write takes each of these controls from its last real holder
    /// — answered 409.
    Taken(Vec<Held>),
    /// The guard could not see whom the write would take a control from
    /// — answered 503, nothing written.
    Blind(String),
}

/// True when the write retires a scope-none override — `existing` as
/// the adapter read it inside the transaction. It can take no holder
/// away, so it is judged on the row alone, with no read (B1).
pub fn retires_a_deny(existing: Option<&UserOverride>, written: Option<&UserOverride>) -> bool {
    written.is_none() && existing.is_some_and(|o| o.scope == Scope::None)
}

/// True when an override names the identity the guard reads as. An
/// override there could narrow the guard's own reads until every write
/// is unjudgeable (review 782257de, B1), so the door refuses one.
pub fn is_the_guards_reader(ov: &UserOverride) -> bool {
    ov.user_id == COVERAGE_READER_ID
}

impl Standing {
    /// Read the roster, the passkeys, the active workflows and the policy
    /// table. With no real person readable, the workflows and overrides
    /// are not read, and the standing is blind.
    pub async fn read<R: PolicyRepository>(
        repo: &R,
        sources: &dyn CoverageSources,
    ) -> Result<Self, Dark> {
        let dark = |what: &str, why: String| Dark(format!("{what} {why}"));
        let roster: Vec<Person> = sources
            .roster()
            .await
            .map_err(|e| dark("the people roster", e))?
            .into_iter()
            .filter(|p| p.active)
            .collect();
        let keys = if roster.is_empty() {
            Vec::new()
        } else {
            sources
                .keys()
                .await
                .map_err(|e| dark("the passkey tier counts", e))?
        };
        let rules = repo
            .list_rules()
            .await
            .map_err(|e| dark("the policy rules", e.to_string()))?;
        let blind = if roster.is_empty() {
            Some("the people roster answered no active employee".to_string())
        } else if coverage::real_people(&roster, &keys).is_empty() {
            Some(format!(
                "the passkey tier counts answered no bound key for any of the {} active \
                 employee(s)",
                roster.len()
            ))
        } else {
            None
        };
        if blind.is_some() {
            return Ok(Self {
                controls: Vec::new(),
                roster,
                keys,
                rules,
                overrides: Vec::new(),
                blind,
            });
        }
        let workflows = sources
            .workflows()
            .await
            .map_err(|e| dark("the workflow registry", e))?;
        if workflows.is_empty() {
            // Every instance has its platform protocols, so an empty
            // registry is a dark read, and the sign-off and authority
            // controls it declares would go unjudged.
            return Err(dark("the workflow registry", "answered no workflow".into()));
        }
        let mut overrides = Vec::new();
        for p in &roster {
            overrides.extend(
                repo.list_user_overrides(&p.id)
                    .await
                    .map_err(|e| dark("the user overrides", e.to_string()))?,
            );
        }
        Ok(Self {
            controls: coverage::controls(&workflows),
            roster,
            keys,
            rules,
            overrides,
            blind: None,
        })
    }

    /// The table before the write against the table it leaves.
    fn judge(&self, before: Table<'_>, after: Table<'_>) -> Result<(), Refusal> {
        let taken = coverage::orphaned_by(&self.controls, &self.roster, &self.keys, before, after);
        if taken.is_empty() {
            Ok(())
        } else {
            Err(Refusal::Taken(taken))
        }
    }

    fn blind(&self, what: &str) -> Refusal {
        Refusal::Blind(format!(
            "{} — {what} could take a control's last holder away, and no real person can be read \
             to judge it (a new rule is the one write that passes unread: it takes no hold)",
            self.blind.as_deref().unwrap_or("no real person was read")
        ))
    }

    /// A rule write: `existing` is the row under its id as the adapter
    /// read it inside the transaction, `written` what the write leaves
    /// there (a retirement leaves it inactive).
    pub fn rule_write(
        &self,
        existing: Option<&PolicyRule>,
        written: &PolicyRule,
    ) -> Result<(), Refusal> {
        if self.blind.is_some() {
            // A rule with no row under its id grants or denies what no
            // rule did before; it narrows nobody (the fresh instance's
            // tenant publish writes only these).
            return match existing {
                None => Ok(()),
                Some(_) => Err(self.blind("rewriting or retiring a rule")),
            };
        }
        let with = |row: Option<&PolicyRule>| -> Vec<PolicyRule> {
            self.rules
                .iter()
                .filter(|r| r.id != written.id)
                .chain(row.filter(|r| r.active))
                .cloned()
                .collect()
        };
        self.judge(
            Table {
                rules: &with(existing),
                overrides: &self.overrides,
            },
            Table {
                rules: &with(Some(written)),
                overrides: &self.overrides,
            },
        )
    }

    /// An override write: `existing` is the row on its (user, resource,
    /// action) as the adapter read it inside the transaction, `written`
    /// the row the write leaves — `None` for a retirement.
    pub fn override_write(
        &self,
        existing: Option<&UserOverride>,
        written: Option<&UserOverride>,
        now: DateTime<Utc>,
    ) -> Result<(), Refusal> {
        if retires_a_deny(existing, written) {
            return Ok(());
        }
        let Some(key) = written.or(existing) else {
            return Ok(());
        };
        if self.blind.is_some() {
            return Err(self.blind("an override"));
        }
        let same = |o: &UserOverride| {
            o.user_id == key.user_id && o.resource == key.resource && o.action == key.action
        };
        let with = |row: Option<&UserOverride>| -> Vec<UserOverride> {
            self.overrides
                .iter()
                .filter(|o| !same(o))
                .chain(row.filter(|o| o.is_active_at(now)))
                .cloned()
                .collect()
        };
        self.judge(
            Table {
                rules: &self.rules,
                overrides: &with(existing),
            },
            Table {
                rules: &self.rules,
                overrides: &with(written),
            },
        )
    }
}

/// Fixed sources for the guard's tests and the write doors' tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use async_trait::async_trait;
    use boss_policy_client::coverage::{Key, Person, WorkflowFacts};
    use boss_policy_client::types::AccessTier;

    use crate::coverage::CoverageSources;

    pub(crate) struct Fixed {
        pub(crate) roster: Result<Vec<Person>, String>,
        pub(crate) keys: Result<Vec<Key>, String>,
        pub(crate) workflows: Result<Vec<WorkflowFacts>, String>,
    }

    #[async_trait]
    impl CoverageSources for Fixed {
        async fn roster(&self) -> Result<Vec<Person>, String> {
            self.roster.clone()
        }
        async fn keys(&self) -> Result<Vec<Key>, String> {
            self.keys.clone()
        }
        async fn workflows(&self) -> Result<Vec<WorkflowFacts>, String> {
            self.workflows.clone()
        }
    }

    fn person(id: &str, role: &str) -> Person {
        Person {
            id: id.into(),
            role: Some(role.into()),
            active: true,
            hire_date: None,
        }
    }

    fn key(id: &str) -> Key {
        Key {
            employee_id: id.into(),
            access_tier: AccessTier::User,
        }
    }

    impl Fixed {
        /// The instance's shape on 2026-09-30: one real person, the
        /// founder, platform-admin, with a user-tier key — the sole
        /// holder of every pair a door asks for — and one workflow whose
        /// step the founder's role signs off and claims.
        pub(crate) fn founder_only() -> Self {
            Self {
                roster: Ok(vec![person("emp-founder", "platform-admin")]),
                keys: Ok(vec![key("emp-founder")]),
                workflows: Ok(serde_json::from_value(serde_json::json!([
                    {"kind": "design-doc", "status": "active", "steps": [
                        {"title": "review", "authority_role": "platform-admin",
                         "sign_offs_required": ["platform-admin"]}]}
                ]))
                .unwrap_or_default()),
            }
        }

        /// The founder plus an emergency delegate with a bound key, whose
        /// role holds nothing at rest (DR readiness 62dac114 item 4).
        pub(crate) fn founder_and_delegate() -> Self {
            let mut me = Self::founder_only();
            me.roster = Ok(vec![
                person("emp-founder", "platform-admin"),
                person("emp-delegate", "emergency-delegate"),
            ]);
            me.keys = Ok(vec![key("emp-founder"), key("emp-delegate")]);
            me
        }

        /// One real person who holds nothing: every control is a gap
        /// before any write, so no write can take one, and a door test
        /// judges authority alone.
        pub(crate) fn bystander() -> Self {
            Self {
                roster: Ok(vec![person("emp-bystander", "bystander")]),
                keys: Ok(vec![key("emp-bystander")]),
                workflows: Ok(serde_json::from_value(serde_json::json!([
                    {"kind": "chore", "status": "active", "steps": []}
                ]))
                .unwrap_or_default()),
            }
        }

        /// Every source dark.
        pub(crate) fn all_dark() -> Self {
            Self {
                roster: Err("GET /api/people: connection refused".into()),
                keys: Err("GET tiers: connection refused".into()),
                workflows: Err("GET /api/workflows: connection refused".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::Fixed;
    use super::*;
    use boss_policy_client::in_memory::InMemoryPolicy;
    use boss_policy_client::types::{Action, Resource, Scope};

    fn founder_only() -> Fixed {
        Fixed::founder_only()
    }

    async fn defaults() -> InMemoryPolicy {
        InMemoryPolicy::with_rules(boss_policy_client::defaults::default_rules())
    }

    fn update_policy_rule(scope: Scope, active: bool) -> PolicyRule {
        let mut r = PolicyRule::new(
            "platform-admin",
            Resource::policy_rule(),
            Action::Update,
            scope,
        );
        r.active = active;
        r
    }

    fn taken(r: Result<(), Refusal>) -> Vec<Held> {
        match r {
            Err(Refusal::Taken(t)) => t,
            other => panic!("expected a taken control, got {other:?}"),
        }
    }

    fn deny(user: &str) -> UserOverride {
        UserOverride {
            id: format!("ov-{user}"),
            user_id: user.into(),
            resource: Resource::policy_rule(),
            action: Action::Update,
            scope: Scope::None,
            reason: "lockout".into(),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn the_founders_own_lockout_is_refused_naming_the_control() {
        let repo = defaults().await;
        let standing = Standing::read(&repo, &founder_only()).await.unwrap();
        let existing = update_policy_rule(Scope::All, true);
        for written in [
            update_policy_rule(Scope::None, true),
            update_policy_rule(Scope::All, false),
        ] {
            let taken = taken(standing.rule_write(Some(&existing), &written));
            assert_eq!(taken[0].control, "policy:update:policy-rule");
        }
        // A rule about a role no real person holds changes nothing.
        let clerk = PolicyRule::new("clerk", Resource::job(), Action::Read, Scope::None);
        assert!(standing.rule_write(None, &clerk).is_ok());
    }

    /// The dynamic controls are read off the workflows: retiring the
    /// founder's sign-off grant would leave the design review's sign-off
    /// held by no one.
    #[tokio::test]
    async fn a_workflow_sign_off_is_a_control_the_guard_keeps() {
        let repo = defaults().await;
        repo.upsert_rule(
            &PolicyRule::new(
                "platform-admin",
                Resource::new("step-signoff:platform-admin"),
                Action::SignOff,
                Scope::All,
            ),
            "test",
        )
        .await
        .unwrap();
        let standing = Standing::read(&repo, &founder_only()).await.unwrap();
        let id = "platform-admin:step-signoff:platform-admin:sign-off";
        let existing = repo.rule_for(id).await.unwrap().expect("the grant");
        let mut retired = existing.clone();
        retired.active = false;
        let taken = taken(standing.rule_write(Some(&existing), &retired));
        assert_eq!(
            taken[0].control,
            "policy:sign-off:step-signoff:platform-admin"
        );
    }

    #[tokio::test]
    async fn a_scope_none_override_on_the_one_holder_is_refused_and_its_retirement_passes() {
        let repo = defaults().await;
        let standing = Standing::read(&repo, &founder_only()).await.unwrap();
        let now = Utc::now();
        let deny = deny("emp-founder");
        assert_eq!(
            taken(standing.override_write(None, Some(&deny), now))[0].control,
            "policy:update:policy-rule"
        );

        // The lockout already in the table: retiring it repairs it.
        repo.upsert_user_override(&deny, "test").await.unwrap();
        let locked = Standing::read(&repo, &founder_only()).await.unwrap();
        assert!(locked.override_write(Some(&deny), None, now).is_ok());
    }

    /// Review 782257de, B1: a deny's retirement is decided on its row —
    /// with no standing at all, since nothing can be read to need one.
    #[test]
    fn retiring_a_deny_is_decided_on_the_row_alone() {
        let d = deny("emp-founder");
        assert!(retires_a_deny(Some(&d), None));
        let grant = UserOverride {
            scope: Scope::All,
            ..d.clone()
        };
        assert!(!retires_a_deny(Some(&grant), None), "a grant's retirement");
        assert!(!retires_a_deny(Some(&d), Some(&grant)), "a rewrite");
        assert!(!retires_a_deny(None, None));
    }

    #[test]
    fn the_guards_own_reader_is_named() {
        assert!(is_the_guards_reader(&deny(COVERAGE_READER_ID)));
        assert!(!is_the_guards_reader(&deny("emp-founder")));
        assert!(
            crate::coverage::COVERAGE_READER.contains(&format!("\"id\":\"{COVERAGE_READER_ID}\"")),
            "the reader's header carries the id the door refuses overrides on"
        );
    }

    /// Review 782257de, N1: an empty roster, and a roster with no bound
    /// key, are blind — not "nothing held". Only a NEW rule passes blind
    /// (the fresh instance's seeding); a narrowing, a retirement and any
    /// override are refused, naming what could not be read.
    #[tokio::test]
    async fn a_roster_with_no_real_person_is_blind_and_passes_only_a_new_rule() {
        let repo = defaults().await;
        let empty = Fixed {
            roster: Ok(vec![]),
            keys: Err("not read with no roster".into()),
            workflows: Err("not read blind".into()),
        };
        let keyless = Fixed {
            keys: Ok(vec![]),
            workflows: Err("not read blind".into()),
            ..founder_only()
        };
        let now = Utc::now();
        for (sources, named) in [(empty, "roster"), (keyless, "passkey")] {
            let standing = Standing::read(&repo, &sources).await.unwrap();
            let narrowed = standing.rule_write(
                Some(&update_policy_rule(Scope::All, true)),
                &update_policy_rule(Scope::None, true),
            );
            match narrowed {
                Err(Refusal::Blind(why)) => assert!(why.contains(named), "{why}"),
                other => panic!("{named}: a narrowing read blind must refuse: {other:?}"),
            }
            let fresh = PolicyRule::new("ceo", Resource::job(), Action::Read, Scope::All);
            assert!(standing.rule_write(None, &fresh).is_ok(), "{named}");
            assert!(matches!(
                standing.override_write(None, Some(&deny("emp-founder")), now),
                Err(Refusal::Blind(_))
            ));
            // And a deny's retirement still needs nothing read.
            let d = deny("emp-founder");
            assert!(standing.override_write(Some(&d), None, now).is_ok());
        }
    }

    /// Review 782257de, N3 (the policy half of Q2(a) of design b08725c2):
    /// with the delegate a real person and the founder still a holder,
    /// granting the delegate's role a pair and retiring that grant both
    /// pass, and so does a user override on the delegate and its end.
    #[tokio::test]
    async fn granting_and_retiring_the_delegates_hold_passes() {
        let repo = defaults().await;
        let standing = Standing::read(&repo, &Fixed::founder_and_delegate())
            .await
            .unwrap();
        let grant = PolicyRule::new(
            "emergency-delegate",
            Resource::class(),
            Action::Create,
            Scope::All,
        );
        assert!(standing.rule_write(None, &grant).is_ok());
        repo.upsert_rule(&grant, "test").await.unwrap();
        let held = Standing::read(&repo, &Fixed::founder_and_delegate())
            .await
            .unwrap();
        let retired = PolicyRule {
            active: false,
            ..grant.clone()
        };
        assert!(held.rule_write(Some(&grant), &retired).is_ok());

        let now = Utc::now();
        let cover = UserOverride {
            scope: Scope::All,
            ..deny("emp-delegate")
        };
        assert!(standing.override_write(None, Some(&cover), now).is_ok());
        assert!(standing.override_write(Some(&cover), None, now).is_ok());
    }

    #[tokio::test]
    async fn a_dark_source_is_a_refusal_never_a_pass() {
        let repo = defaults().await;
        for (sources, named) in [
            (
                Fixed {
                    roster: Err("connection refused".into()),
                    ..founder_only()
                },
                "roster",
            ),
            (
                Fixed {
                    keys: Err("answered 403".into()),
                    ..founder_only()
                },
                "passkey",
            ),
            (
                Fixed {
                    workflows: Err("answered 502".into()),
                    ..founder_only()
                },
                "workflow",
            ),
            (
                Fixed {
                    workflows: Ok(vec![]),
                    ..founder_only()
                },
                "workflow",
            ),
        ] {
            let Dark(why) = Standing::read(&repo, &sources).await.unwrap_err();
            assert!(why.contains(named), "{why}");
        }
    }
}
