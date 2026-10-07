//! G2: preserve the holders a People mutation would remove (47aed706).
//! Policy and workflow facts arrive through a read port; the local
//! roster and key state are judged at the mutation's commit boundary.
//!
//! WHICH WRITES READ THE BASIS (review c3b96c09 F1, 2026-10-06). The
//! basis is HTTP — the policy table and the workflow registry — and it
//! was read before every guarded write, one override list per employee:
//! 407 requests and 153 ms a write at the demo tenant's 405 employees,
//! about 410,000 requests across one fresh boot. Most writes cannot take
//! a holder away whatever the table says, and [`can_orphan`] names them
//! from the local roster and keys alone. Only a write it does not clear
//! reads the basis, and that read is two requests, not N+2.

use std::collections::BTreeSet;

use async_trait::async_trait;
use boss_policy_client::coverage::{self, Control, Key, Person};
use boss_policy_client::types::{PolicyRule, UserOverride};

use crate::port::PeopleError;
use crate::types::Employee;

/// The complete external basis of one judgement. Identities name the
/// override reads that succeeded, including those answering no rows.
#[derive(Clone)]
pub struct Standing {
    pub controls: Vec<Control>,
    pub rules: Vec<PolicyRule>,
    pub overrides: Vec<UserOverride>,
    pub employee_ids: BTreeSet<String>,
}

/// Read the actual active policy and workflow declarations. A read
/// error is not an empty set of grants or overrides.
#[async_trait]
pub trait CoverageRead: Send + Sync {
    async fn standing(&self, employee_ids: &[String]) -> Result<Standing, PeopleError>;
}

pub fn people(roster: &[Employee]) -> Vec<Person> {
    roster
        .iter()
        .map(|e| Person {
            id: e.id.clone(),
            role: e.role.clone(),
            active: e.status.as_deref() == Some("active"),
            hire_date: e.hire_date,
        })
        .collect()
}

fn real_ids(roster: &[Person], keys: &[Key]) -> BTreeSet<String> {
    coverage::real_people(roster, keys)
        .into_iter()
        .map(|p| p.id.clone())
        .collect()
}

fn holds_operator_key(keys: &[Key], id: &str) -> bool {
    keys.iter()
        .any(|k| k.employee_id == id && k.access_tier == boss_policy_client::AccessTier::Operator)
}

/// Whether a change of roster and keys CAN take a control from its last
/// real holder, read from the local facts alone — no rule, override or
/// workflow consulted. `false` is a proof, not a guess:
///
/// - with no real person before the write nothing is held, so nothing
///   can be newly orphaned (the fresh instance seeding its founder);
/// - otherwise, `coverage::holds` reads only the control, the person's
///   own row, whether they are the platform owner, and whether they
///   hold an operator-tier key. So when every real person before the
///   write is still real after it with the same row, has lost no
///   operator-tier standing, and the owner is the same person, every
///   holder before is a holder after, under ANY table.
///
/// `true` only means the basis must be read and the write judged. A
/// grant is not cleared for being a grant: an earlier-hired keyless
/// platform-admin changes who the owner is, and is judged.
pub fn can_orphan(
    before: &[Person],
    before_keys: &[Key],
    after: &[Person],
    after_keys: &[Key],
) -> bool {
    let real = coverage::real_people(before, before_keys);
    if real.is_empty() {
        return false;
    }
    let owner = |roster: &[Person]| coverage::platform_owner(roster).map(|p| p.id.clone());
    if owner(before) != owner(after) {
        return true;
    }
    let kept = coverage::real_people(after, after_keys);
    real.into_iter().any(|person| {
        !kept.contains(&person)
            || (holds_operator_key(before_keys, &person.id)
                && !holds_operator_key(after_keys, &person.id))
    })
}

/// The basis for one proposed change, read BEFORE the transaction, or
/// `None` when [`can_orphan`] clears it. The ids handed to the reader
/// are the real people of either state: `coverage::holds` is asked of
/// nobody else, so nobody else's overrides can change the judgement.
pub async fn standing_for(
    source: &dyn CoverageRead,
    before: &[Person],
    before_keys: &[Key],
    after: &[Person],
    after_keys: &[Key],
) -> Result<Option<Standing>, PeopleError> {
    if !can_orphan(before, before_keys, after, after_keys) {
        return Ok(None);
    }
    let mut ids = real_ids(before, before_keys);
    ids.extend(real_ids(after, after_keys));
    let ids: Vec<String> = ids.into_iter().collect();
    source.standing(&ids).await.map(Some)
}

/// The judgement at the commit boundary, over the roster and keys read
/// under the guard's lock. The clearance is decided again here, on the
/// locked facts: a write cleared before the transaction that the locked
/// state no longer clears (a key enrolled in between) is REFUSED as
/// unjudgeable rather than committed unjudged — the basis is HTTP and
/// is never read while the lock is held.
pub fn judge_at_commit(
    standing: Option<&Standing>,
    before: &[Person],
    before_keys: &[Key],
    after: &[Person],
    after_keys: &[Key],
) -> Result<(), PeopleError> {
    if !can_orphan(before, before_keys, after, after_keys) {
        return Ok(());
    }
    match standing {
        Some(standing) => standing.judge_people(before, before_keys, after, after_keys),
        None => Err(PeopleError::Unavailable(
            "coverage cannot be judged: the roster or its keys changed while this write was \
             being prepared; retry it"
                .into(),
        )),
    }
}

/// One employee write, as the guard sees it.
pub enum Change<'a> {
    Create(&'a Employee),
    Update(&'a Employee),
    Delete,
}

/// The roster the write would leave, or `None` when the write's own
/// existence check will answer instead — a create of a row that is
/// there, an update or delete of one that is not. Those change nothing,
/// so they are never judged and never read the basis: a replayed create
/// answers "already exists", not the guard (review c3b96c09 F6).
pub fn roster_after(before: &[Person], id: &str, change: &Change<'_>) -> Option<Vec<Person>> {
    let exists = before.iter().any(|p| p.id == id);
    let written = match change {
        Change::Create(_) if exists => return None,
        Change::Update(_) | Change::Delete if !exists => return None,
        Change::Create(row) | Change::Update(row) => Some(*row),
        Change::Delete => None,
    };
    let mut after: Vec<Person> = before.iter().filter(|p| p.id != id).cloned().collect();
    if let Some(row) = written {
        after.extend(people(std::slice::from_ref(row)));
    }
    Some(after)
}

/// The keys one removal would leave, or `None` when that employee holds
/// no key of that tier.
pub fn keys_without(
    keys: &[Key],
    employee_id: &str,
    tier: &boss_policy_client::AccessTier,
) -> Option<Vec<Key>> {
    let index = keys
        .iter()
        .position(|key| key.employee_id == employee_id && key.access_tier == *tier)?;
    let mut after = keys.to_vec();
    after.remove(index);
    Some(after)
}

impl Standing {
    /// The same policy basis judges both states; only newly orphaned
    /// controls refuse. Existing gaps must not block their own repair.
    pub fn judge_people(
        &self,
        before: &[Person],
        before_keys: &[Key],
        after: &[Person],
        after_keys: &[Key],
    ) -> Result<(), PeopleError> {
        if self.controls.is_empty() {
            return Err(PeopleError::Unavailable(
                "coverage cannot be judged: no controls were read".into(),
            ));
        }
        // Overrides must have been read for every REAL person of either
        // state — the only people `coverage::holds` is asked about. A
        // keyless or inactive employee holds nothing under any override,
        // so theirs are not read (review c3b96c09 F1: this used to be one
        // request per employee on the roster).
        let mut judged = real_ids(before, before_keys);
        judged.extend(real_ids(after, after_keys));
        if let Some(id) = judged.iter().find(|id| !self.employee_ids.contains(*id)) {
            return Err(PeopleError::Unavailable(format!(
                "coverage cannot be judged: overrides for employee {id} were not read"
            )));
        }
        let existing: BTreeSet<String> = coverage::coverage(
            &self.controls,
            &self.rules,
            &self.overrides,
            before,
            before_keys,
        )
        .into_iter()
        .map(|o| o.control)
        .collect();
        let taken: Vec<String> = coverage::coverage(
            &self.controls,
            &self.rules,
            &self.overrides,
            after,
            after_keys,
        )
        .into_iter()
        .filter(|o| !existing.contains(&o.control))
        .map(|o| format!("{}: {}", o.control, o.wants))
        .collect();
        if taken.is_empty() {
            Ok(())
        } else {
            Err(PeopleError::Conflict(format!(
                "this write would remove the last real holder: {}; add a holder first",
                taken.join("; ")
            )))
        }
    }
}

/// Production read adapter. The URLs come from the service registry
/// at construction; requests carry the existing People machine identity.
pub struct HttpCoverageRead {
    policy_base: String,
    jobs_base: String,
    http: boss_core::machine_token::Client,
    user: String,
}

impl HttpCoverageRead {
    pub fn new(policy_base: impl Into<String>, jobs_base: impl Into<String>) -> Self {
        let (policy_base, http) = boss_core::http_client::base(policy_base);
        Self {
            policy_base,
            jobs_base: jobs_base.into().trim_end_matches('/').to_owned(),
            http,
            user: serde_json::json!(boss_policy_client::User::service("people")).to_string(),
        }
    }

    async fn read<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        source: &str,
        shape: &str,
    ) -> Result<T, PeopleError> {
        let response = self
            .http
            .get(url)
            .header("x-boss-user", &self.user)
            .send()
            .await
            .map_err(|_| PeopleError::Unavailable(format!("{source} is unreachable")))?;
        if !response.status().is_success() {
            return Err(PeopleError::Unavailable(format!(
                "{source} answered {}",
                response.status()
            )));
        }
        response
            .json::<T>()
            .await
            .map_err(|_| PeopleError::Unavailable(format!("{source} answered no {shape}")))
    }

    /// The native registry reads answer whole bare arrays. A counted
    /// envelope, a partial page or an error object is not silently
    /// accepted as the complete registry.
    async fn rows<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        source: &str,
    ) -> Result<Vec<T>, PeopleError> {
        self.read(url, source, "complete typed row array").await
    }

    /// One employee's live overrides — the single read for a real person
    /// the bulk snapshot does not carry.
    async fn overrides_of(&self, id: &str) -> Result<Vec<UserOverride>, PeopleError> {
        let mut url =
            reqwest::Url::parse(&format!("{}/api/policy/user-overrides/", self.policy_base))
                .map_err(|_| PeopleError::Unavailable("policy base URL cannot be used".into()))?;
        url.path_segments_mut()
            .map_err(|_| PeopleError::Unavailable("policy base has no path".into()))?
            .pop_if_empty()
            .push(id);
        let rows: Vec<UserOverride> = self.rows(url.as_str(), "employee policy overrides").await?;
        if rows.iter().any(|row| row.user_id != id) {
            return Err(PeopleError::Unavailable(format!(
                "policy overrides answered another employee for {id}"
            )));
        }
        Ok(rows)
    }
}

#[async_trait]
impl CoverageRead for HttpCoverageRead {
    /// Two requests, whatever the roster's size (review c3b96c09 F1; it
    /// was one override read per employee). The policy service's coverage
    /// snapshot (`coverage::SNAPSHOT_PATH`, landed with the publish
    /// guard) answers the rules and the live overrides of every active
    /// employee it reads committed, in one typed body. A real person it
    /// does not carry — someone this very write hires or re-activates —
    /// costs one override read each. The snapshot's own roster and keys
    /// are NOT judged here: this guard judges the rows it reads under
    /// its lock; the snapshot's roster only names whose overrides it
    /// answered.
    async fn standing(&self, employee_ids: &[String]) -> Result<Standing, PeopleError> {
        let snapshot: coverage::CoverageSnapshot = self
            .read(
                &format!("{}{}", self.policy_base, coverage::SNAPSHOT_PATH),
                "policy coverage snapshot",
                "complete typed snapshot",
            )
            .await?;
        snapshot.validate().map_err(PeopleError::Unavailable)?;
        let mut read: BTreeSet<String> = snapshot.roster.iter().map(|p| p.id.clone()).collect();
        if let Some(stray) = snapshot
            .overrides
            .iter()
            .find(|row| !read.contains(&row.user_id))
        {
            return Err(PeopleError::Unavailable(format!(
                "policy coverage snapshot answered an override for {}, who is not on its roster",
                stray.user_id
            )));
        }
        let workflows: Vec<serde_json::Value> = self
            .rows(
                &format!("{}/api/workflows", self.jobs_base),
                "workflow registry",
            )
            .await?;
        if workflows.is_empty() {
            return Err(PeopleError::Unavailable(
                "workflow registry answered no workflow".into(),
            ));
        }
        let workflows = workflows
            .into_iter()
            .map(|row| {
                // Native active rows always contain their complete steps.
                // WorkflowFacts has defaults for thin consumers; a guard
                // must not mistake omission for a workflow with no controls.
                if !row.get("steps").is_some_and(serde_json::Value::is_array) {
                    return Err(PeopleError::Unavailable(
                        "workflow registry omitted the steps array".into(),
                    ));
                }
                serde_json::from_value::<coverage::WorkflowFacts>(row).map_err(|_| {
                    PeopleError::Unavailable("workflow registry row cannot be judged".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut overrides = snapshot.overrides;
        for id in employee_ids {
            if read.contains(id) {
                continue;
            }
            overrides.extend(self.overrides_of(id).await?);
            read.insert(id.clone());
        }
        Ok(Standing {
            controls: coverage::controls(&workflows),
            rules: snapshot.rules,
            overrides,
            employee_ids: read,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get};
    use boss_policy_client::AccessTier;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The policy and jobs services in one stub: the coverage snapshot
    /// over `roster`, the workflow registry, and the single-employee
    /// override read — counting every request it answers.
    async fn server_with(
        roster: Vec<String>,
        snapshot_overrides: Value,
        workflows: Value,
        overrides: Value,
        status: axum::http::StatusCode,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let snapshot = json!({
            "rules": [],
            "overrides": snapshot_overrides,
            "roster": roster.iter().map(|id| json!({"id": id, "role": "service-tech", "active": true})).collect::<Vec<_>>(),
            "keys": [],
        });
        let (a, b, c) = (requests.clone(), requests.clone(), requests.clone());
        let app = Router::new()
            .route(
                coverage::SNAPSHOT_PATH,
                get(move || {
                    a.fetch_add(1, Ordering::SeqCst);
                    let value = snapshot.clone();
                    async move { Json(value) }
                }),
            )
            .route(
                "/api/workflows",
                get(move || {
                    b.fetch_add(1, Ordering::SeqCst);
                    let value = workflows.clone();
                    async move { (status, Json(value)) }
                }),
            )
            .route(
                "/api/policy/user-overrides/{id}",
                get(move || {
                    c.fetch_add(1, Ordering::SeqCst);
                    let value = overrides.clone();
                    async move { Json(value) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), task, requests)
    }

    async fn server(
        workflows: Value,
        overrides: Value,
        status: axum::http::StatusCode,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let (base, task, _) = server_with(
            vec!["emp-founder".into()],
            json!([]),
            workflows,
            overrides,
            status,
        )
        .await;
        (base, task)
    }

    #[tokio::test]
    async fn the_native_reader_refuses_missing_steps_dark_empty_and_partial_registries() {
        for workflows in [
            json!([]),
            json!([{"kind":"real-work","status":"active"}]),
            json!({"data":[{"kind":"real-work","steps":[]}],"total":2}),
            json!({"error":"unavailable"}),
        ] {
            let (base, task) = server(workflows, json!([]), axum::http::StatusCode::OK).await;
            let source = HttpCoverageRead::new(&base, &base);
            assert!(matches!(
                source.standing(&["emp-founder".into()]).await,
                Err(PeopleError::Unavailable(_))
            ));
            task.abort();
        }
        let (base, task) = server(
            json!([{"kind":"real-work","steps":[]}]),
            json!([]),
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
        assert!(matches!(
            HttpCoverageRead::new(&base, &base)
                .standing(&["emp-founder".into()])
                .await,
            Err(PeopleError::Unavailable(_))
        ));
        task.abort();
    }

    #[tokio::test]
    async fn the_native_reader_keeps_dynamic_presence_controls_and_valid_empty_override_reads() {
        let workflows = json!([{"kind":"review-work","status":"active","steps":[{"title":"Approve","assurance_required":"presence","audience":{"individual":"emp-founder"}}]}]);
        let (base, task) = server(workflows, json!([]), axum::http::StatusCode::OK).await;
        let standing = HttpCoverageRead::new(&base, &base)
            .standing(&["emp-founder".into()])
            .await
            .unwrap();
        assert!(standing.employee_ids.contains("emp-founder"));
        assert!(standing.overrides.is_empty());
        assert!(standing.controls.iter().any(
            |c| matches!(c, Control::Presence { individual: Some(id), .. } if id == "emp-founder")
        ));
        task.abort();
    }

    /// The single read for a real person the snapshot does not carry:
    /// another employee's rows are not that person's empty answer.
    #[tokio::test]
    async fn a_different_employees_override_response_is_not_an_empty_newcomer_override_read() {
        let row = json!({"id":"ov-other","user_id":"emp-other","resource":"job","action":"read","scope":"none","reason":"fixture","expires_at":null});
        let (base, task) = server(
            json!([{"kind":"real-work","steps":[]}]),
            json!([row]),
            axum::http::StatusCode::OK,
        )
        .await;
        let result = HttpCoverageRead::new(&base, &base)
            .standing(&["emp-newcomer".into()])
            .await;
        match result {
            Err(PeopleError::Unavailable(message)) => {
                assert!(message.contains("another employee"), "{message}")
            }
            _ => panic!("another employee's override rows must refuse"),
        }
        task.abort();
    }

    /// THE COST (review c3b96c09 F1): the basis was N+2 sequential
    /// requests — 407 at the demo tenant's 405 employees, 153 ms a write
    /// against a stub. It is two, and three when the write brings a real
    /// person the snapshot has not seen.
    #[tokio::test]
    async fn the_basis_is_two_requests_at_405_employees_and_one_more_per_unseen_real_person() {
        let roster: Vec<String> = (0..405).map(|n| format!("emp-{n:04}")).collect();
        let (base, task, requests) = server_with(
            roster.clone(),
            json!([]),
            json!([{"kind":"real-work","steps":[]}]),
            json!([]),
            axum::http::StatusCode::OK,
        )
        .await;
        let source = HttpCoverageRead::new(&base, &base);
        source.standing(&roster).await.unwrap();
        let started = std::time::Instant::now();
        let rounds = 20;
        requests.store(0, Ordering::SeqCst);
        for _ in 0..rounds {
            let standing = source.standing(&roster).await.unwrap();
            assert_eq!(standing.employee_ids.len(), 405);
        }
        eprintln!(
            "G2 basis read n=405: {:?} per judged write (stub; {} HTTP requests each)",
            started.elapsed() / rounds,
            requests.load(Ordering::SeqCst) / rounds as usize
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2 * rounds as usize);

        requests.store(0, Ordering::SeqCst);
        let mut with_newcomer = roster.clone();
        with_newcomer.push("emp-newcomer".into());
        let standing = source.standing(&with_newcomer).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        assert!(standing.employee_ids.contains("emp-newcomer"));
        task.abort();
    }

    /// A snapshot is taken whole or not at all: an override for someone
    /// off its own roster, an unknown field, or a refusal is not a basis.
    #[tokio::test]
    async fn a_snapshot_that_is_not_whole_is_not_a_basis() {
        let stray = json!([{"id":"ov-stray","user_id":"emp-elsewhere","resource":"job","action":"read","scope":"none","reason":"fixture","expires_at":null}]);
        let (base, task, _) = server_with(
            vec!["emp-founder".into()],
            stray,
            json!([{"kind":"real-work","steps":[]}]),
            json!([]),
            axum::http::StatusCode::OK,
        )
        .await;
        match HttpCoverageRead::new(&base, &base)
            .standing(&["emp-founder".into()])
            .await
        {
            Err(PeopleError::Unavailable(message)) => {
                assert!(message.contains("not on its roster"), "{message}")
            }
            _ => panic!("a stray override must refuse"),
        }
        task.abort();

        for (status, body) in [
            (axum::http::StatusCode::BAD_GATEWAY, json!({"error":"dark"})),
            (
                axum::http::StatusCode::OK,
                json!({"rules":[],"overrides":[],"roster":[],"keys":[]}),
            ),
            (
                axum::http::StatusCode::OK,
                json!({"rules":[],"overrides":[],"roster":[{"id":"emp-founder","role":null,"active":true}],"keys":[],"page":1}),
            ),
            (axum::http::StatusCode::OK, json!([])),
        ] {
            let app = Router::new()
                .route(
                    coverage::SNAPSHOT_PATH,
                    get(move || {
                        let value = body.clone();
                        async move { (status, Json(value)) }
                    }),
                )
                .route(
                    "/api/workflows",
                    get(|| async { Json(json!([{"kind":"real-work","steps":[]}])) }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            assert!(matches!(
                HttpCoverageRead::new(&base, &base)
                    .standing(&["emp-founder".into()])
                    .await,
                Err(PeopleError::Unavailable(_))
            ));
            task.abort();
        }
    }

    // -----------------------------------------------------------------
    // can_orphan: which changes are cleared without a basis.
    // -----------------------------------------------------------------

    fn person(id: &str, role: &str, active: bool, hired: (i32, u32, u32)) -> Person {
        Person {
            id: id.into(),
            role: Some(role.into()),
            active,
            hire_date: chrono::NaiveDate::from_ymd_opt(hired.0, hired.1, hired.2),
        }
    }

    fn key(id: &str, access_tier: AccessTier) -> Key {
        Key {
            employee_id: id.into(),
            access_tier,
        }
    }

    fn owner() -> Person {
        person("emp-owner", "platform-admin", true, (2020, 1, 1))
    }

    #[test]
    fn changes_that_keep_every_real_holder_and_the_owner_are_cleared() {
        let before = vec![
            owner(),
            person("emp-tech", "service-tech", true, (2021, 1, 1)),
        ];
        let keys = vec![
            key("emp-owner", AccessTier::Operator),
            key("emp-owner", AccessTier::User),
        ];
        let with = |extra: Person| {
            let mut after = before.clone();
            after.push(extra);
            after
        };
        // A keyless hire who is not a platform-admin.
        assert!(!can_orphan(
            &before,
            &keys,
            &with(person("emp-new", "service-tech", true, (2019, 1, 1))),
            &keys
        ));
        // A platform-admin hired AFTER the owner: the owner does not move.
        assert!(!can_orphan(
            &before,
            &keys,
            &with(person(
                "emp-delegate",
                "platform-admin",
                true,
                (2026, 10, 1)
            )),
            &keys
        ));
        // An inactive earlier-hired platform-admin is not the owner.
        assert!(!can_orphan(
            &before,
            &keys,
            &with(person("emp-old", "platform-admin", false, (2019, 1, 1))),
            &keys
        ));
        // Nothing changed at all.
        assert!(!can_orphan(&before, &keys, &before, &keys));
        // A keyless employee retired, re-roled or removed.
        assert!(!can_orphan(
            &before,
            &keys,
            &[
                owner(),
                person("emp-tech", "service-tech", false, (2021, 1, 1))
            ],
            &keys
        ));
        assert!(!can_orphan(
            &before,
            &keys,
            &[owner(), person("emp-tech", "sales", true, (2021, 1, 1))],
            &keys
        ));
        assert!(!can_orphan(&before, &keys, &[owner()], &keys));
        // A user key removed, the operator key kept.
        assert!(!can_orphan(
            &before,
            &keys,
            &before,
            &[key("emp-owner", AccessTier::Operator)]
        ));
        // A key added.
        let mut more = keys.clone();
        more.push(key("emp-tech", AccessTier::User));
        assert!(!can_orphan(&before, &keys, &before, &more));
    }

    #[test]
    fn changes_that_touch_a_real_holder_or_move_the_owner_are_judged() {
        let tech = person("emp-tech", "service-tech", true, (2021, 1, 1));
        let before = vec![owner(), tech.clone()];
        let keys = vec![
            key("emp-owner", AccessTier::Operator),
            key("emp-owner", AccessTier::User),
            key("emp-tech", AccessTier::User),
        ];
        // The real holder retired, re-roled, re-dated or removed.
        for changed in [
            person("emp-owner", "platform-admin", false, (2020, 1, 1)),
            person("emp-owner", "service-tech", true, (2020, 1, 1)),
            person("emp-owner", "platform-admin", true, (2022, 1, 1)),
        ] {
            assert!(can_orphan(&before, &keys, &[changed, tech.clone()], &keys));
        }
        assert!(can_orphan(
            &before,
            &keys,
            std::slice::from_ref(&tech),
            &keys
        ));
        // A real non-admin holder touched: their role may be what held a pair.
        assert!(can_orphan(
            &before,
            &keys,
            &[owner(), person("emp-tech", "sales", true, (2021, 1, 1))],
            &keys
        ));
        // An earlier-hired platform-admin, keyless or not, becomes the owner.
        let mut usurped = before.clone();
        usurped.push(person("emp-earlier", "platform-admin", true, (2019, 6, 1)));
        assert!(can_orphan(&before, &keys, &usurped, &keys));
        // A grant of the role to an earlier hire moves the owner too.
        let early_tech = person("emp-early", "service-tech", true, (2019, 6, 1));
        let granted = person("emp-early", "platform-admin", true, (2019, 6, 1));
        assert!(can_orphan(
            &[owner(), early_tech],
            &keys,
            &[owner(), granted],
            &keys
        ));
        // The operator key removed while a user key keeps them real.
        assert!(can_orphan(
            &before,
            &keys,
            &before,
            &[
                key("emp-owner", AccessTier::User),
                key("emp-tech", AccessTier::User)
            ]
        ));
        // A real person's only key removed.
        assert!(can_orphan(
            &before,
            &keys,
            &before,
            &[
                key("emp-owner", AccessTier::Operator),
                key("emp-owner", AccessTier::User)
            ]
        ));
    }

    #[test]
    fn with_no_real_person_nothing_is_held_so_nothing_is_judged() {
        let founder = owner();
        assert!(!can_orphan(&[], &[], std::slice::from_ref(&founder), &[]));
        // Keyless roster: even retiring the only platform-admin takes nothing.
        assert!(!can_orphan(std::slice::from_ref(&founder), &[], &[], &[]));
    }

    /// A write cleared before the transaction that the locked facts no
    /// longer clear is refused as unjudgeable, never committed unjudged.
    #[test]
    fn a_change_the_locked_facts_do_not_clear_refuses_without_a_basis() {
        let keys = vec![key("emp-owner", AccessTier::User)];
        assert!(matches!(
            judge_at_commit(None, &[owner()], &keys, &[], &keys),
            Err(PeopleError::Unavailable(_))
        ));
        assert!(judge_at_commit(None, &[owner()], &keys, &[owner()], &keys).is_ok());
    }

    #[test]
    fn a_write_its_own_existence_check_answers_is_never_judged() {
        let roster = vec![owner()];
        let row: Employee = serde_json::from_value(json!({
            "id": "emp-owner", "skills": [], "certifications": [],
        }))
        .unwrap();
        assert!(roster_after(&roster, "emp-owner", &Change::Create(&row)).is_none());
        assert!(roster_after(&roster, "emp-absent", &Change::Update(&row)).is_none());
        assert!(roster_after(&roster, "emp-absent", &Change::Delete).is_none());
        assert_eq!(
            roster_after(&roster, "emp-owner", &Change::Delete),
            Some(vec![])
        );
    }

    // The clearance is a claim about EVERY table, so it is tested against
    // generated ones: whenever can_orphan clears a change, the full
    // judgement over the same change must find nothing newly orphaned —
    // for every control kind, under generated rules and overrides.
    mod clearance {
        use super::*;
        use boss_policy_client::types::{Action, Resource, Scope};
        use proptest::prelude::*;

        const IDS: [&str; 4] = ["emp-a", "emp-b", "emp-c", "emp-d"];
        const ROLES: [&str; 3] = ["platform-admin", "service-tech", "sales"];

        fn roster() -> impl Strategy<Value = Vec<Person>> {
            proptest::collection::vec(
                (
                    0..3usize,
                    any::<bool>(),
                    proptest::option::of(2018..2024i32),
                ),
                4,
            )
            .prop_flat_map(|rows| {
                proptest::collection::vec(any::<bool>(), 4).prop_map(move |present| {
                    rows.iter()
                        .zip(IDS)
                        .zip(present)
                        .filter(|(_, present)| *present)
                        .map(|(((role, active, year), id), _)| Person {
                            id: id.into(),
                            role: Some(ROLES[*role].into()),
                            active: *active,
                            hire_date: year.and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1)),
                        })
                        .collect()
                })
            })
        }

        fn keys() -> impl Strategy<Value = Vec<Key>> {
            proptest::collection::vec((0..4usize, any::<bool>()), 0..5).prop_map(|rows| {
                rows.into_iter()
                    .map(|(who, operator)| Key {
                        employee_id: IDS[who].into(),
                        access_tier: if operator {
                            AccessTier::Operator
                        } else {
                            AccessTier::User
                        },
                    })
                    .collect()
            })
        }

        fn scope() -> impl Strategy<Value = Scope> {
            prop_oneof![Just(Scope::All), Just(Scope::None)]
        }

        fn rules() -> impl Strategy<Value = Vec<PolicyRule>> {
            proptest::collection::vec((0..3usize, any::<bool>(), scope(), any::<bool>()), 0..5)
                .prop_map(|rows| {
                    rows.into_iter()
                        .map(|(role, signoff, scope, active)| {
                            let (resource, action) = if signoff {
                                (Resource::new("step-signoff:sales"), Action::SignOff)
                            } else {
                                (Resource::new("job"), Action::Read)
                            };
                            let mut rule = PolicyRule::new(ROLES[role], resource, action, scope);
                            rule.active = active;
                            rule
                        })
                        .collect()
                })
        }

        fn overrides() -> impl Strategy<Value = Vec<UserOverride>> {
            proptest::collection::vec((0..4usize, scope()), 0..3).prop_map(|rows| {
                rows.into_iter()
                    .enumerate()
                    .map(|(n, (who, scope))| UserOverride {
                        id: format!("ov-{n}"),
                        user_id: IDS[who].into(),
                        resource: Resource::new("job"),
                        action: Action::Read,
                        scope,
                        reason: "generated".into(),
                        expires_at: None,
                    })
                    .collect()
            })
        }

        fn controls() -> Vec<Control> {
            vec![
                Control::PlatformOwner,
                Control::OperatorTier,
                Control::Authority {
                    role: "sales".into(),
                },
                Control::Policy {
                    action: Action::Read,
                    resource: "job".into(),
                    all_only: false,
                },
                Control::Presence {
                    workflow: "generated".into(),
                    step: "generated".into(),
                    roles: vec!["service-tech".into()],
                    individual: Some("emp-b".into()),
                },
            ]
        }

        proptest! {
            #[test]
            fn a_cleared_change_orphans_nothing_under_any_table(
                before in roster(), before_keys in keys(),
                after in roster(), after_keys in keys(),
                rules in rules(), overrides in overrides(),
            ) {
                prop_assume!(!can_orphan(&before, &before_keys, &after, &after_keys));
                let standing = Standing {
                    controls: controls(),
                    rules,
                    overrides,
                    employee_ids: IDS.iter().map(|id| id.to_string()).collect(),
                };
                let judged = standing.judge_people(&before, &before_keys, &after, &after_keys);
                prop_assert!(judged.is_ok(), "cleared, yet the judgement refuses: {judged:?}");
            }
        }
    }
}
