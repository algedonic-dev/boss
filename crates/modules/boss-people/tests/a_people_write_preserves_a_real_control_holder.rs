//! G2 of the approved coverage guard: direct repository callers, not
//! only the HTTP door, must preserve the last real holder (47aed706).

mod common;

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_people::{InMemoryPeople, PeopleError, PeopleRepository};
use boss_policy_client::AccessTier;
use boss_policy_client::coverage::{self, Control, Key, Person};

struct Fixed;

#[async_trait::async_trait]
impl boss_people::coverage_guard::CoverageRead for Fixed {
    async fn standing(
        &self,
        ids: &[String],
    ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
        Ok(boss_people::coverage_guard::Standing {
            controls: vec![Control::PlatformOwner, Control::OperatorTier],
            rules: vec![],
            overrides: vec![],
            employee_ids: ids.iter().cloned().collect(),
        })
    }
}

#[tokio::test]
async fn postgres_deletion_of_the_only_real_platform_owner_is_refused_before_outbox() {
    let db = boss_testing::TestDb::new().await;
    sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ('emp-founder', 'platform-admin', 'active', '2024-01-15')")
        .execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ('emp-founder', $1, $2, 'user')")
        .bind(b"founder-user-key".as_slice()).bind(b"fixture-public-key".as_slice())
        .execute(&db.pool).await.unwrap();
    let repo =
        boss_people::PgPeople::new(db.pool.clone()).with_coverage(std::sync::Arc::new(Fixed));
    let now = chrono::Utc::now();
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    let result = repo.delete_employee_at("emp-founder", now, &stamp).await;
    assert!(
        matches!(result, Err(PeopleError::Conflict(_))),
        "the native People transaction must refuse the owner removal before rows or outbox: {result:?}"
    );
    assert!(repo.employee_by_id("emp-founder").await.unwrap().is_some());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'people.employee.deleted'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn deleting_the_only_real_platform_owner_is_refused_before_row_or_event() {
    let mut founder = common::employee_fixture("emp-founder");
    founder.role = Some("platform-admin".into());
    let keys = vec![Key {
        employee_id: founder.id.clone(),
        access_tier: AccessTier::Operator,
    }];
    let roster = vec![Person {
        id: founder.id.clone(),
        role: founder.role.clone(),
        active: true,
        hire_date: founder.hire_date,
    }];
    let controls = vec![Control::PlatformOwner, Control::OperatorTier];
    assert!(coverage::coverage(&controls, &[], &[], &roster, &keys).is_empty());
    assert_eq!(coverage::coverage(&controls, &[], &[], &[], &keys).len(), 2);
    let repo =
        InMemoryPeople::new(vec![founder.clone()]).with_coverage(std::sync::Arc::new(Fixed), keys);
    let now = chrono::Utc::now();
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    let result = repo.delete_employee_at(&founder.id, now, &stamp).await;
    assert!(
        matches!(result, Err(PeopleError::Conflict(_))),
        "a direct repository removal must refuse the newly orphaned owner and operator controls: {result:?}"
    );
    assert_eq!(
        repo.employee_by_id(&founder.id).await.unwrap(),
        Some(founder)
    );
    assert!(repo.recorded_events().is_empty());
}

fn founder(id: &str) -> boss_people::Employee {
    let mut person = common::employee_fixture(id);
    person.role = Some("platform-admin".into());
    person
}

fn operator_key(id: &str) -> Key {
    Key {
        employee_id: id.into(),
        access_tier: AccessTier::Operator,
    }
}

#[tokio::test]
async fn role_and_status_removal_of_a_sole_real_holder_refuse_without_events() {
    for (role, status) in [
        (Some("service-tech"), Some("active")),
        (None, Some("active")),
        (Some("platform-admin"), Some("terminated")),
        (Some("platform-admin"), None),
    ] {
        let owner = founder("emp-owner");
        let repo = InMemoryPeople::new(vec![owner.clone()])
            .with_coverage(std::sync::Arc::new(Fixed), vec![operator_key(&owner.id)]);
        let mut written = owner.clone();
        written.role = role.map(str::to_owned);
        written.status = status.map(str::to_owned);
        let now = chrono::Utc::now();
        let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
        assert!(matches!(
            repo.update_employee_at(&owner.id, &written, now, &stamp)
                .await,
            Err(PeopleError::Conflict(_))
        ));
        assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
        assert!(repo.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn an_existing_operator_gap_does_not_block_its_own_repair_or_an_unrelated_change() {
    let owner = founder("emp-owner");
    let repo = InMemoryPeople::new(vec![owner.clone()]).with_coverage(
        std::sync::Arc::new(Fixed),
        vec![Key {
            employee_id: owner.id.clone(),
            access_tier: AccessTier::User,
        }],
    );
    let mut written = owner.clone();
    written.name = Some("Updated name".into());
    let now = chrono::Utc::now();
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    repo.update_employee_at(&owner.id, &written, now, &stamp)
        .await
        .unwrap();
    assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(written));
    assert_eq!(repo.recorded_events().len(), 1);
}

#[tokio::test]
async fn two_concurrent_retirements_cannot_each_consume_the_other_persons_hold() {
    struct Concurrent(tokio::sync::Barrier);
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Concurrent {
        async fn standing(
            &self,
            ids: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            self.0.wait().await;
            Fixed.standing(ids).await
        }
    }
    let first = founder("emp-first");
    let second = founder("emp-second");
    let repo = InMemoryPeople::new(vec![first.clone(), second.clone()]).with_coverage(
        std::sync::Arc::new(Concurrent(tokio::sync::Barrier::new(2))),
        vec![operator_key(&first.id), operator_key(&second.id)],
    );
    let mut first_after = first.clone();
    first_after.status = Some("terminated".into());
    let mut second_after = second.clone();
    second_after.status = Some("terminated".into());
    let now = chrono::Utc::now();
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    let (a, b) = tokio::join!(
        repo.update_employee_at(&first.id, &first_after, now, &stamp),
        repo.update_employee_at(&second.id, &second_after, now, &stamp)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        matches!(a, Err(PeopleError::Conflict(_))) || matches!(b, Err(PeopleError::Conflict(_)))
    );
    assert_eq!(
        repo.all_employees()
            .await
            .unwrap()
            .iter()
            .filter(|e| e.status.as_deref() == Some("active"))
            .count(),
        1
    );
    assert_eq!(repo.recorded_events().len(), 1);
}

#[tokio::test]
async fn unread_policy_or_unread_employee_overrides_refuse_instead_of_an_empty_basis() {
    struct Dark(bool);
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Dark {
        async fn standing(
            &self,
            _: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            if self.0 {
                return Err(PeopleError::Unavailable("policy read failed".into()));
            }
            Ok(boss_people::coverage_guard::Standing {
                controls: vec![Control::PlatformOwner],
                rules: vec![],
                overrides: vec![],
                employee_ids: Default::default(),
            })
        }
    }
    for dark in [true, false] {
        let owner = founder("emp-owner");
        let repo = InMemoryPeople::new(vec![owner.clone()]).with_coverage(
            std::sync::Arc::new(Dark(dark)),
            vec![operator_key(&owner.id)],
        );
        let now = chrono::Utc::now();
        let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
        assert!(matches!(
            repo.delete_employee_at(&owner.id, now, &stamp).await,
            Err(PeopleError::Unavailable(_))
        ));
        assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
        assert!(repo.recorded_events().is_empty());
    }
}

#[tokio::test]
async fn concurrent_postgres_retirements_preserve_one_real_holder_and_one_outbox_fact() {
    struct Concurrent(tokio::sync::Barrier);
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Concurrent {
        async fn standing(
            &self,
            ids: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            self.0.wait().await;
            Fixed.standing(ids).await
        }
    }
    let db = boss_testing::TestDb::new().await;
    for id in ["emp-first", "emp-second"] {
        sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ($1, 'platform-admin', 'active', '2024-01-15')")
            .bind(id).execute(&db.pool).await.unwrap();
        sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ($1, $2, $3, 'user')")
            .bind(id).bind(id.as_bytes()).bind(b"fixture-public-key".as_slice()).execute(&db.pool).await.unwrap();
    }
    let repo = boss_people::PgPeople::new(db.pool.clone()).with_coverage(std::sync::Arc::new(
        Concurrent(tokio::sync::Barrier::new(2)),
    ));
    let mut first = repo.employee_by_id("emp-first").await.unwrap().unwrap();
    let mut second = repo.employee_by_id("emp-second").await.unwrap().unwrap();
    first.status = Some("terminated".into());
    second.status = Some("terminated".into());
    let now = chrono::Utc::now();
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        tokio::join!(
            repo.update_employee_at(&first.id, &first, now, &stamp),
            repo.update_employee_at(&second.id, &second, now, &stamp)
        )
    })
    .await
    .expect("the local guarded writes must terminate");
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert!(
        matches!(a, Err(PeopleError::Conflict(_))) || matches!(b, Err(PeopleError::Conflict(_)))
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM employees WHERE status = 'active'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'people.employee.updated'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 1);
}

#[tokio::test]
async fn offboarding_the_last_holder_reports_conflict_without_row_change_or_events() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let db = boss_testing::TestDb::new().await;
    let owner = founder("emp-offboarding-owner");
    let repo = std::sync::Arc::new(
        InMemoryPeople::new(vec![owner.clone()])
            .with_coverage(std::sync::Arc::new(Fixed), vec![operator_key(&owner.id)]),
    );
    let app = boss_people::workflows::workflow_router(
        db.pool.clone(),
        repo.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );
    let user = serde_json::to_string(&boss_policy_client::User::service("people")).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/people/{}/offboard", owner.id))
                .header("x-boss-user", user)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a deliberate coverage refusal is a conflict, not a storage failure"
    );
    assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
    assert!(repo.recorded_events().is_empty());
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM employee_changes")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(changes, 0);
}

#[tokio::test]
async fn offboarding_with_unread_coverage_reports_unavailable_without_changes() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    struct Unread;
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Unread {
        async fn standing(
            &self,
            _: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            Err(PeopleError::Unavailable(
                "coverage could not be read".into(),
            ))
        }
    }
    let db = boss_testing::TestDb::new().await;
    let owner = founder("emp-unread-owner");
    let repo = std::sync::Arc::new(
        InMemoryPeople::new(vec![owner.clone()])
            .with_coverage(std::sync::Arc::new(Unread), vec![operator_key(&owner.id)]),
    );
    let app = boss_people::workflows::workflow_router(
        db.pool.clone(),
        repo.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );
    let user = serde_json::to_string(&boss_policy_client::User::service("people")).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/people/{}/offboard", owner.id))
                .header("x-boss-user", user)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
    assert!(repo.recorded_events().is_empty());
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM employee_changes")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(changes, 0);
}

#[tokio::test]
async fn enrolling_then_revoking_a_nonholder_preserves_the_original_real_owner_in_memory() {
    use boss_people::coverage_guard::CoverageRead;
    let owner = founder("emp-existing-owner");
    let newcomer = common::employee_fixture("emp-nonholder");
    let keys = vec![
        operator_key(&owner.id),
        Key {
            employee_id: newcomer.id.clone(),
            access_tier: AccessTier::User,
        },
    ];
    let repo = InMemoryPeople::new(vec![owner.clone()])
        .with_coverage(std::sync::Arc::new(Fixed), keys.clone());
    let stamp = EventStamp::new("people", ActorId::Automation("agent-codex".into()));
    let now = chrono::Utc::now();
    assert!(
        coverage::coverage(
            &Fixed
                .standing(std::slice::from_ref(&owner.id))
                .await
                .unwrap()
                .controls,
            &[],
            &[],
            &boss_people::coverage_guard::people(std::slice::from_ref(&owner)),
            &keys
        )
        .is_empty()
    );
    repo.create_employee_at(&newcomer, now, &stamp)
        .await
        .unwrap();
    assert_eq!(
        repo.employee_by_id(&newcomer.id).await.unwrap(),
        Some(newcomer.clone())
    );
    repo.delete_employee_at(&newcomer.id, now, &stamp)
        .await
        .unwrap();
    assert!(repo.employee_by_id(&newcomer.id).await.unwrap().is_none());
    assert_eq!(
        repo.employee_by_id(&owner.id).await.unwrap(),
        Some(owner.clone())
    );
    let facts = repo.recorded_events();
    assert_eq!(
        facts.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["people.employee.created", "people.employee.deleted"]
    );
    assert!(facts.iter().all(|e| e.source == "people"));
    assert!(
        coverage::coverage(
            &[Control::PlatformOwner, Control::OperatorTier],
            &[],
            &[],
            &boss_people::coverage_guard::people(&[owner]),
            &keys
        )
        .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Which writes read the coverage basis (review c3b96c09, F1 and F6,
// 2026-10-06). The basis is two HTTP reads; a write that cannot take a
// holder away must not pay for them, and a write that can must never
// skip them. Each skip below is pinned by the count of basis reads.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counted(std::sync::atomic::AtomicUsize);

impl Counted {
    fn reads(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl boss_people::coverage_guard::CoverageRead for Counted {
    async fn standing(
        &self,
        ids: &[String],
    ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Fixed.standing(ids).await
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("people", ActorId::Automation("agent-claude".into()))
}

fn user_key(id: &str) -> Key {
    Key {
        employee_id: id.into(),
        access_tier: AccessTier::User,
    }
}

/// The founder as production holds it: an active platform-admin with a
/// bound key, so every control in `Fixed` has a real holder to lose.
async fn seed_real_founder(pool: &sqlx::PgPool, id: &str, hired: &str) {
    sqlx::query("INSERT INTO employees (id, role, status, hire_date) VALUES ($1, 'platform-admin', 'active', $2::date)")
        .bind(id).bind(hired).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, access_tier) VALUES ($1, $2, $3, 'user')")
        .bind(id).bind(id.as_bytes()).bind(b"fixture-public-key".as_slice())
        .execute(pool).await.unwrap();
}

/// THE COST the review measured: the demo tenant's boot POSTs 405
/// employees, links their managers and then replays the 405 POSTs, and
/// each of those writes read the basis (N+2 requests) before anything
/// else. None of them can take a holder away — the hires carry no key
/// and are not platform-admins — so none may read it.
#[tokio::test]
async fn a_boot_of_405_keyless_hires_their_manager_links_and_its_replay_read_no_basis_on_postgres()
{
    let db = boss_testing::TestDb::new().await;
    seed_real_founder(&db.pool, "emp-founder", "2020-01-01").await;
    let counted = std::sync::Arc::new(Counted::default());
    let repo = boss_people::PgPeople::new(db.pool.clone()).with_coverage(counted.clone());
    let now = chrono::Utc::now();
    let started = std::time::Instant::now();
    let hires: Vec<_> = (0..405)
        .map(|n| common::employee_fixture(&format!("emp-boot-{n:03}")))
        .collect();
    for hire in &hires {
        repo.create_employee_at(hire, now, &stamp()).await.unwrap();
    }
    let created = started.elapsed();
    for hire in hires.iter().skip(1) {
        let mut linked = hire.clone();
        linked.manager_id = Some(hires[0].id.clone());
        repo.update_employee_at(&hire.id, &linked, now, &stamp())
            .await
            .unwrap();
    }
    let linked = started.elapsed();
    for hire in &hires {
        match repo.create_employee_at(hire, now, &stamp()).await {
            Err(PeopleError::Conflict(message)) => {
                assert!(message.contains("already exists"), "{message}")
            }
            other => panic!("a replayed hire answers already exists: {other:?}"),
        }
    }
    eprintln!(
        "G2 boot at n=405 on Postgres: 405 creates {created:?}, +404 manager links {:?}, +405 replays {:?}; basis reads {}",
        linked - created,
        started.elapsed() - linked,
        counted.reads()
    );
    assert_eq!(
        counted.reads(),
        0,
        "no write of a keyless non-admin hire can take a holder away"
    );
}

/// A replayed create answers for the row that is there, in the write's
/// own terms, and reads nothing — whoever the row is (review F6: the
/// guard used to judge first, so a replay could answer the guard's 409).
#[tokio::test]
async fn a_replayed_create_of_a_real_holder_answers_already_exists_and_reads_no_basis() {
    let mut replay = founder("emp-founder");
    replay.role = Some("service-tech".into());
    let now = chrono::Utc::now();

    let counted = std::sync::Arc::new(Counted::default());
    let memory = InMemoryPeople::new(vec![founder("emp-founder")])
        .with_coverage(counted.clone(), vec![operator_key("emp-founder")]);
    match memory.create_employee_at(&replay, now, &stamp()).await {
        Err(PeopleError::Conflict(message)) => {
            assert!(message.contains("already exists"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(counted.reads(), 0);

    let db = boss_testing::TestDb::new().await;
    seed_real_founder(&db.pool, "emp-founder", "2020-01-01").await;
    let counted = std::sync::Arc::new(Counted::default());
    let repo = boss_people::PgPeople::new(db.pool.clone()).with_coverage(counted.clone());
    match repo.create_employee_at(&replay, now, &stamp()).await {
        Err(PeopleError::Conflict(message)) => {
            assert!(message.contains("already exists"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(counted.reads(), 0);
}

/// A create is NOT coverage-neutral (review c3b96c09's live table): a
/// keyless platform-admin hired EARLIER than the owner becomes the first
/// hire, so the owner the gateway elevates is now someone with no key.
/// That create reads the basis and is refused, in both adapters, before
/// a row or a fact exists.
#[tokio::test]
async fn creating_an_earlier_hired_keyless_platform_admin_reads_the_basis_and_is_refused() {
    let mut usurper = founder("emp-earlier");
    usurper.hire_date = chrono::NaiveDate::from_ymd_opt(2019, 6, 1);
    let now = chrono::Utc::now();

    let mut owner = founder("emp-founder");
    owner.hire_date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1);
    let counted = std::sync::Arc::new(Counted::default());
    let memory = InMemoryPeople::new(vec![owner])
        .with_coverage(counted.clone(), vec![operator_key("emp-founder")]);
    let refused = memory.create_employee_at(&usurper, now, &stamp()).await;
    assert!(
        matches!(&refused, Err(PeopleError::Conflict(m)) if m.contains("last real holder")),
        "{refused:?}"
    );
    assert_eq!(counted.reads(), 1);
    assert!(
        memory
            .employee_by_id("emp-earlier")
            .await
            .unwrap()
            .is_none()
    );
    assert!(memory.recorded_events().is_empty());

    let db = boss_testing::TestDb::new().await;
    seed_real_founder(&db.pool, "emp-founder", "2020-01-01").await;
    let counted = std::sync::Arc::new(Counted::default());
    let repo = boss_people::PgPeople::new(db.pool.clone()).with_coverage(counted.clone());
    let refused = repo.create_employee_at(&usurper, now, &stamp()).await;
    assert!(
        matches!(&refused, Err(PeopleError::Conflict(m)) if m.contains("last real holder")),
        "{refused:?}"
    );
    assert_eq!(counted.reads(), 1);
    assert!(repo.employee_by_id("emp-earlier").await.unwrap().is_none());
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'people.employee.created'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(facts, 0);
}

/// The same grant by an UPDATE: an earlier-hired keyless employee given
/// the platform-admin role takes the owner's place. A role grant is not
/// coverage-neutral either.
#[tokio::test]
async fn granting_platform_admin_to_an_earlier_hired_keyless_employee_is_refused() {
    let mut owner = founder("emp-founder");
    owner.hire_date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1);
    let mut audit = common::employee_fixture("emp-audit");
    audit.hire_date = chrono::NaiveDate::from_ymd_opt(2019, 6, 1);
    let counted = std::sync::Arc::new(Counted::default());
    let repo = InMemoryPeople::new(vec![owner, audit.clone()])
        .with_coverage(counted.clone(), vec![operator_key("emp-founder")]);
    let mut granted = audit.clone();
    granted.role = Some("platform-admin".into());
    let refused = repo
        .update_employee_at(&audit.id, &granted, chrono::Utc::now(), &stamp())
        .await;
    assert!(
        matches!(&refused, Err(PeopleError::Conflict(_))),
        "{refused:?}"
    );
    assert_eq!(counted.reads(), 1);
    assert_eq!(repo.employee_by_id(&audit.id).await.unwrap(), Some(audit));
}

/// The writes that leave every real holder exactly as they were, with
/// the same owner, read nothing: a later-hired platform-admin delegate,
/// an edit of a real holder that changes none of role, status or hire
/// date, the retirement of a keyless employee, and an update that
/// changes nothing at all.
#[tokio::test]
async fn writes_that_leave_every_real_holder_and_the_owner_in_place_read_no_basis() {
    let mut owner = founder("emp-founder");
    owner.hire_date = chrono::NaiveDate::from_ymd_opt(2020, 1, 1);
    let bystander = common::employee_fixture("emp-bystander");
    let counted = std::sync::Arc::new(Counted::default());
    let repo = InMemoryPeople::new(vec![owner.clone(), bystander.clone()])
        .with_coverage(counted.clone(), vec![operator_key("emp-founder")]);
    let now = chrono::Utc::now();

    let mut delegate = founder("emp-delegate");
    delegate.hire_date = chrono::NaiveDate::from_ymd_opt(2026, 10, 1);
    repo.create_employee_at(&delegate, now, &stamp())
        .await
        .unwrap();

    let mut renamed = owner.clone();
    renamed.name = Some("A new name".into());
    repo.update_employee_at(&owner.id, &renamed, now, &stamp())
        .await
        .unwrap();
    repo.update_employee_at(&owner.id, &renamed, now, &stamp())
        .await
        .unwrap();

    let mut retired = bystander.clone();
    retired.status = Some("terminated".into());
    repo.update_employee_at(&bystander.id, &retired, now, &stamp())
        .await
        .unwrap();
    repo.delete_employee_at(&bystander.id, now, &stamp())
        .await
        .unwrap();

    assert_eq!(counted.reads(), 0);
}

/// With no real person at all nothing is held, so nothing can be taken:
/// a fresh instance seeds its founder — an active platform-admin, the
/// owner-to-be — without a basis to read. The policy snapshot refuses an
/// empty roster, so a guard that asked here could never be answered.
#[tokio::test]
async fn a_fresh_instance_seeds_its_founder_without_a_basis_to_read() {
    struct Dark;
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Dark {
        async fn standing(
            &self,
            _: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            Err(PeopleError::Unavailable("no active employee".into()))
        }
    }
    let now = chrono::Utc::now();
    let memory = InMemoryPeople::new(vec![]).with_coverage(std::sync::Arc::new(Dark), vec![]);
    memory
        .create_employee_at(&founder("emp-founder"), now, &stamp())
        .await
        .unwrap();
    let db = boss_testing::TestDb::new().await;
    let repo = boss_people::PgPeople::new(db.pool.clone()).with_coverage(std::sync::Arc::new(Dark));
    repo.create_employee_at(&founder("emp-founder"), now, &stamp())
        .await
        .unwrap();
}

/// `PUT /api/people/{id}/status` answers the guard in the guard's terms:
/// 409 for a refusal, never the 500 a storage failure gets (review F6,
/// mutant M11 — only the offboard door was pinned).
#[tokio::test]
async fn the_status_door_reports_the_guards_refusal_as_a_conflict() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let db = boss_testing::TestDb::new().await;
    let owner = founder("emp-status-owner");
    let repo = std::sync::Arc::new(
        InMemoryPeople::new(vec![owner.clone()])
            .with_coverage(std::sync::Arc::new(Fixed), vec![operator_key(&owner.id)]),
    );
    let app = boss_people::workflows::workflow_router(
        db.pool.clone(),
        repo.clone(),
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    );
    let user = serde_json::to_string(&boss_policy_client::User::service("people")).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/people/{}/status", owner.id))
                .header("x-boss-user", user)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"terminated"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
    assert!(repo.recorded_events().is_empty());
}

/// A basis with no control in it is a read that failed, not a table in
/// which nothing can be orphaned (review F6, mutant M9 — the HTTP reader
/// cannot produce one, so nothing reached this refusal).
#[tokio::test]
async fn a_basis_with_no_controls_refuses_instead_of_passing_everything() {
    struct Empty;
    #[async_trait::async_trait]
    impl boss_people::coverage_guard::CoverageRead for Empty {
        async fn standing(
            &self,
            ids: &[String],
        ) -> Result<boss_people::coverage_guard::Standing, PeopleError> {
            Ok(boss_people::coverage_guard::Standing {
                controls: vec![],
                rules: vec![],
                overrides: vec![],
                employee_ids: ids.iter().cloned().collect(),
            })
        }
    }
    let owner = founder("emp-owner");
    let repo = InMemoryPeople::new(vec![owner.clone()])
        .with_coverage(std::sync::Arc::new(Empty), vec![user_key(&owner.id)]);
    let refused = repo
        .delete_employee_at(&owner.id, chrono::Utc::now(), &stamp())
        .await;
    assert!(
        matches!(refused, Err(PeopleError::Unavailable(_))),
        "{refused:?}"
    );
    assert_eq!(repo.employee_by_id(&owner.id).await.unwrap(), Some(owner));
}
