//! The employee roster answers the same on both `PeopleRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the first MODULE port of
//! the census (backlog be459ab9). Every door test of `/api/people`
//! (`tests/common`, `http::tests`) is answered by `InMemoryPeople`,
//! while production is answered by `PgPeople`.
//!
//! WHY IT EXISTS. The port was stated for the double alone
//! (`in_memory::tests`) and for Postgres alone (`pg_people_roundtrip`),
//! each in its own words; this file is the one statement of every
//! method of the port, each case holding each adapter to the same
//! answer, and every write also judged by the `people.employee.*` facts
//! it leaves — the double's `recorded_events` and Postgres's
//! `event_outbox`, staged in the write's own transaction.
//!
//! The world: the same `suite-` rows, INSERTed into Postgres the way a
//! rebuild lands them and handed to the double's constructor. Postgres
//! uses `PgPeople::new` — no registry clients — because the double holds
//! no Class, department or Location registry; registry validation is
//! `PgPeople::with_registries`' and is pinned in `postgres::tests`. The
//! migrations may seed rows of their own, so every read is judged over
//! `suite-` ids.
//!
//! ORDER IS COMPARED, because the port now promises it: the roster by
//! `id`, direct reports by `name` (nameless last), an employee's skills
//! and certifications in one stated order — all in BYTE order. The
//! fixture holds a case pair and a punctuation pair (`suite-B`,
//! `suite-a-z`, `suite-ab`) — the only shape that catches the
//! database's locale disagreeing with the double's byte order (backlog
//! 2987fb2d).

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_people::events::{EMPLOYEE_CREATED, EMPLOYEE_DELETED, EMPLOYEE_UPDATED};
use boss_people::{
    Certification, Employee, InMemoryPeople, PeopleError, PeopleRepository, PgPeople,
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

/// The roster under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: PeopleRepository;
    fn repo(&self) -> &Self::R;
    /// Every `people.employee.*` fact about a `suite-` row, as
    /// `(kind, payload)`, in the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("people.employee.")
        && payload["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("suite-"))
}

struct InMemory(InMemoryPeople);

impl World for InMemory {
    type R = InMemoryPeople;
    fn repo(&self) -> &InMemoryPeople {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| is_suite_fact(&e.kind, &e.payload))
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgPeople,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgPeople;
    fn repo(&self) -> &PgPeople {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> =
            sqlx::query_as("SELECT kind, payload FROM event_outbox ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .expect("read the outbox");
        rows.into_iter()
            .filter(|(k, p)| is_suite_fact(k, p))
            .collect()
    }
}

/// Land the suite's rows in a fresh database — managers first, one
/// statement per row and per satellite, satellites in the order given.
async fn seed(pool: &sqlx::PgPool) {
    for e in rows() {
        sqlx::query(
            "INSERT INTO employees (id, name, email, role, department, skill_level,
                hire_date, location, manager_id, employment_type, status,
                annual_salary_cents)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&e.id)
        .bind(&e.name)
        .bind(&e.email)
        .bind(&e.role)
        .bind(&e.department)
        .bind(e.skill_level.map(i16::from))
        .bind(e.hire_date)
        .bind(&e.location)
        .bind(&e.manager_id)
        .bind(&e.employment_type)
        .bind(&e.status)
        .bind(e.annual_salary_cents)
        .execute(pool)
        .await
        .expect("a suite employee lands");
        for s in &e.skills {
            sqlx::query("INSERT INTO employee_skills (employee_id, skill) VALUES ($1, $2)")
                .bind(&e.id)
                .bind(s)
                .execute(pool)
                .await
                .expect("a suite skill lands");
        }
        for c in &e.certifications {
            sqlx::query(
                "INSERT INTO employee_certifications
                    (employee_id, name, issuing_body, issued_on, expires_on)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(&e.id)
            .bind(&c.name)
            .bind(&c.issuing_body)
            .bind(c.issued_on)
            .bind(c.expires_on)
            .execute(pool)
            .await
            .expect("a suite certification lands");
        }
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryPeople::new(rows())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            seed(&db.pool).await;
            let world = Postgres { repo: PgPeople::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        all_employees_is_every_row_whole_in_byte_order_of_id,
        employee_by_id_answers_a_row_whole_and_none_for_a_stranger,
        direct_reports_is_one_level_in_byte_order_of_name_nameless_last,
        a_created_employee_reads_back_whole_and_records_the_row_as_sent,
        a_create_on_a_held_id_is_a_conflict_and_records_nothing,
        a_create_the_roster_cannot_hold_is_a_conflict_and_lands_nothing,
        an_update_replaces_the_row_and_its_satellites_and_records_it,
        an_update_the_roster_cannot_hold_is_refused_and_changes_nothing,
        a_delete_removes_the_row_and_records_who_and_when,
        a_delete_of_a_stranger_or_a_manager_is_refused_and_records_nothing,
    }
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn day(m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, m, d).expect("a real day")
}

fn cert(name: &str, issued_on: NaiveDate) -> Certification {
    Certification {
        name: name.into(),
        issuing_body: "Suite Board".into(),
        issued_on,
        expires_on: None,
    }
}

/// An employee carrying only what the suite needs to tell it apart.
fn emp(id: &str, name: Option<&str>, manager: Option<&str>) -> Employee {
    Employee {
        id: id.into(),
        name: name.map(String::from),
        email: Some(format!("{id}@suite.test")),
        role: None,
        department: None,
        skill_level: None,
        skills: vec![],
        hire_date: None,
        location: None,
        manager_id: manager.map(String::from),
        employment_type: None,
        status: None,
        certifications: vec![],
        annual_salary_cents: None,
    }
}

/// A fully-described manager whose skills and certifications are given
/// OUT of the order the port answers them in.
fn boss() -> Employee {
    let mut b = emp("suite-boss", Some("Suite Boss"), None);
    b.role = Some("service-tech".into());
    b.department = Some("service".into());
    b.skill_level = Some(5);
    b.skills = vec![
        "brazing".into(),
        "Welding".into(),
        "ab".into(),
        "a-z".into(),
    ];
    b.hire_date = Some(day(1, 2));
    b.location = Some("loc-hq".into());
    b.employment_type = Some("full-time".into());
    b.status = Some("active".into());
    b.certifications = vec![
        cert("b", day(3, 1)),
        cert("A", day(1, 1)),
        cert("ab", day(3, 1)),
        cert("a-z", day(3, 1)),
    ];
    b.certifications[1].expires_on = Some(day(12, 31));
    b.annual_salary_cents = Some(9_000_000);
    b
}

/// The suite's rows, managers first: a manager with four direct
/// reports whose names are a case pair, a punctuation pair and a
/// nameless one; a grandchild; and an id-only record.
fn rows() -> Vec<Employee> {
    let mut solo = emp("suite-solo", None, None);
    solo.email = None;
    let mut anon = emp("suite-anon", None, Some("suite-boss"));
    anon.email = None;
    vec![
        boss(),
        emp("suite-ab", Some("Suite ab"), Some("suite-boss")),
        emp("suite-a-z", Some("Suite a-z"), Some("suite-boss")),
        emp("suite-B", Some("Suite B"), Some("suite-boss")),
        anon,
        emp("suite-zz", Some("Suite Aardvark"), Some("suite-ab")),
        solo,
    ]
}

/// A row as every read answers it: skills in byte order, certifications
/// by `issued_on` then `name` in byte order.
fn whole(mut e: Employee) -> Employee {
    e.skills.sort();
    e.certifications
        .sort_by(|a, b| (a.issued_on, &a.name).cmp(&(b.issued_on, &b.name)));
    e
}

fn row(id: &str) -> Employee {
    rows()
        .into_iter()
        .find(|e| e.id == id)
        .expect("a suite row")
}

fn stamp() -> EventStamp {
    EventStamp::new(
        "people",
        ActorId::Automation("rule:adapters-agree-suite".into()),
    )
}

/// The `suite-` ids among `rows`, in the order answered.
fn ids(rows: &[Employee]) -> Vec<&str> {
    rows.iter()
        .map(|e| e.id.as_str())
        .filter(|id| id.starts_with("suite-"))
        .collect()
}

/// A fact with its `_`-prefixed envelope keys removed.
fn stripped((kind, payload): &(String, Value)) -> (String, Value) {
    let mut p = payload.clone();
    if let Value::Object(m) = &mut p {
        m.retain(|k, _| !k.starts_with('_'));
    }
    (kind.clone(), p)
}

fn fact(kind: &str, payload: Value) -> (String, Value) {
    (kind.into(), payload)
}

fn as_sent(e: &Employee) -> Value {
    serde_json::to_value(e).expect("an employee serialises")
}

// ----- cases -----------------------------------------------------------------

/// The roster is every row, whole, in BYTE order of id — `suite-B`
/// before `suite-a-z` before `suite-ab`.
async fn all_employees_is_every_row_whole_in_byte_order_of_id<W: World>(w: &W, adapter: &str) {
    let all = w
        .repo()
        .all_employees()
        .await
        .expect("all_employees answers");
    assert_eq!(
        ids(&all),
        vec![
            "suite-B",
            "suite-a-z",
            "suite-ab",
            "suite-anon",
            "suite-boss",
            "suite-solo",
            "suite-zz",
        ],
        "{adapter}: id in byte order"
    );
    for e in all.iter().filter(|e| e.id.starts_with("suite-")) {
        assert_eq!(*e, whole(row(&e.id)), "{adapter}: {} listed whole", e.id);
    }
}

/// `employee_by_id` answers a row whole — satellites in the stated
/// order — and `None`, not an error, for an id no row carries.
async fn employee_by_id_answers_a_row_whole_and_none_for_a_stranger<W: World>(
    w: &W,
    adapter: &str,
) {
    for id in ["suite-boss", "suite-solo", "suite-anon"] {
        assert_eq!(
            w.repo().employee_by_id(id).await.expect("answers"),
            Some(whole(row(id))),
            "{adapter}: {id} reads back whole"
        );
    }
    let b = w
        .repo()
        .employee_by_id("suite-boss")
        .await
        .expect("answers")
        .expect("held");
    assert_eq!(
        b.skills,
        vec!["Welding", "a-z", "ab", "brazing"],
        "{adapter}: skills in byte order"
    );
    assert_eq!(
        b.certifications
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["A", "a-z", "ab", "b"],
        "{adapter}: certifications by issued_on, then name in byte order"
    );
    assert_eq!(
        w.repo()
            .employee_by_id("suite-stranger")
            .await
            .expect("answers"),
        None,
        "{adapter}"
    );
}

/// Direct reports are one level — a grandchild is not a report — by
/// `name` in byte order with a nameless report last, whole; a leaf or
/// a stranger has none.
async fn direct_reports_is_one_level_in_byte_order_of_name_nameless_last<W: World>(
    w: &W,
    adapter: &str,
) {
    let reports = w
        .repo()
        .direct_reports("suite-boss")
        .await
        .expect("direct_reports answers");
    assert_eq!(
        ids(&reports),
        vec!["suite-B", "suite-a-z", "suite-ab", "suite-anon"],
        "{adapter}: name in byte order, nameless last, no grandchild"
    );
    for e in &reports {
        assert_eq!(*e, whole(row(&e.id)), "{adapter}: {} whole", e.id);
    }
    assert_eq!(
        ids(&w.repo().direct_reports("suite-ab").await.expect("answers")),
        vec!["suite-zz"],
        "{adapter}"
    );
    for leaf in ["suite-zz", "suite-stranger"] {
        assert!(
            w.repo()
                .direct_reports(leaf)
                .await
                .expect("answers")
                .is_empty(),
            "{adapter}: {leaf}"
        );
    }
}

/// A create answers the id, reads back whole — the satellites in the
/// read order whatever order they were sent in — and records ONE
/// `people.employee.created` carrying the row as sent.
async fn a_created_employee_reads_back_whole_and_records_the_row_as_sent<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut hire = boss();
    hire.id = "suite-hire".into();
    hire.name = Some("Suite Hire".into());
    hire.email = Some("suite-hire@suite.test".into());
    hire.manager_id = Some("suite-boss".into());
    let id = w
        .repo()
        .create_employee_at(&hire, at(0), &stamp())
        .await
        .expect("the hire lands");
    assert_eq!(id, "suite-hire", "{adapter}");
    assert_eq!(
        w.repo()
            .employee_by_id("suite-hire")
            .await
            .expect("answers"),
        Some(whole(hire.clone())),
        "{adapter}: reads back whole"
    );
    let id_only = emp("suite-new", None, None);
    w.repo()
        .create_employee_at(&id_only, at(1), &stamp())
        .await
        .expect("an id-only record lands");
    assert_eq!(
        w.facts().await.iter().map(stripped).collect::<Vec<_>>(),
        vec![
            fact(EMPLOYEE_CREATED, as_sent(&hire)),
            fact(EMPLOYEE_CREATED, as_sent(&id_only)),
        ],
        "{adapter}: one fact per create, the row as sent"
    );
}

/// A create on an id the roster holds is a `Conflict` naming it; the
/// held row is untouched and nothing is recorded.
async fn a_create_on_a_held_id_is_a_conflict_and_records_nothing<W: World>(w: &W, adapter: &str) {
    let mut again = emp("suite-ab", Some("Suite ab again"), None);
    again.email = Some("suite-ab-again@suite.test".into());
    let got = w.repo().create_employee_at(&again, at(0), &stamp()).await;
    assert!(
        matches!(got, Err(PeopleError::Conflict(ref m)) if m.contains("suite-ab")),
        "{adapter}: {got:?}"
    );
    assert_eq!(
        w.repo().employee_by_id("suite-ab").await.expect("answers"),
        Some(whole(row("suite-ab"))),
        "{adapter}: the held row is untouched"
    );
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}

/// What the roster cannot hold is refused as a `Conflict` naming the
/// offending value, before anything lands: an email another employee
/// holds (case-insensitively — credentials key on it), a manager the
/// roster does not hold, a skill level outside 1..=5, a skill listed
/// twice. Until this suite (backlog be459ab9) Postgres refused each as
/// a `Storage` error — a 500 — and the double accepted every one.
async fn a_create_the_roster_cannot_hold_is_a_conflict_and_lands_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut taken = emp("suite-x1", Some("X1"), None);
    taken.email = Some("SUITE-AB@Suite.Test".into());
    let mut orphan = emp("suite-x2", Some("X2"), Some("suite-nobody"));
    orphan.email = None;
    let mut level = emp("suite-x3", Some("X3"), None);
    level.skill_level = Some(6);
    let mut twice = emp("suite-x4", Some("X4"), None);
    twice.skills = vec!["brazing".into(), "brazing".into()];
    for (e, names) in [
        (&taken, "SUITE-AB@Suite.Test"),
        (&orphan, "suite-nobody"),
        (&level, "6"),
        (&twice, "brazing"),
    ] {
        let got = w.repo().create_employee_at(e, at(0), &stamp()).await;
        assert!(
            matches!(got, Err(PeopleError::Conflict(ref m)) if m.contains(names)),
            "{adapter}: {} must be a Conflict naming {names}: {got:?}",
            e.id
        );
        assert_eq!(
            w.repo().employee_by_id(&e.id).await.expect("answers"),
            None,
            "{adapter}: {} did not land",
            e.id
        );
    }
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}

/// An update replaces the row whole — satellites included, so a skill
/// or certification left out is gone — keeps the employee's own email
/// in another case, and records ONE `people.employee.updated` carrying
/// the row as sent.
async fn an_update_replaces_the_row_and_its_satellites_and_records_it<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut next = boss();
    next.name = Some("Suite Boss Renamed".into());
    next.email = Some("Suite-Boss@SUITE.test".into());
    next.skills = vec!["zz".into(), "Aa".into()];
    next.certifications = vec![cert("only", day(6, 6))];
    next.skill_level = Some(1);
    next.location = None;
    w.repo()
        .update_employee_at("suite-boss", &next, at(0), &stamp())
        .await
        .expect("the update lands");
    assert_eq!(
        w.repo()
            .employee_by_id("suite-boss")
            .await
            .expect("answers"),
        Some(whole(next.clone())),
        "{adapter}: replaced whole"
    );
    assert_eq!(
        w.facts().await.iter().map(stripped).collect::<Vec<_>>(),
        vec![fact(EMPLOYEE_UPDATED, as_sent(&next))],
        "{adapter}: one fact, the row as sent"
    );
}

/// An update of an id no row carries is `NotFound`; a body whose id is
/// not the id named, an email another employee holds, or a manager the
/// roster does not hold is a `Conflict` naming it. None of them changes
/// a row or records a fact. Until this suite a body naming another id
/// replaced the named row's identity in the double and wrote the BODY's
/// row in Postgres — the same call, two different employees changed.
async fn an_update_the_roster_cannot_hold_is_refused_and_changes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let got = w
        .repo()
        .update_employee_at(
            "suite-stranger",
            &emp("suite-stranger", None, None),
            at(0),
            &stamp(),
        )
        .await;
    assert!(
        matches!(got, Err(PeopleError::NotFound(ref m)) if m.contains("suite-stranger")),
        "{adapter}: {got:?}"
    );

    let mut other = row("suite-ab");
    other.id = "suite-a-z".into();
    let got = w
        .repo()
        .update_employee_at("suite-ab", &other, at(0), &stamp())
        .await;
    assert!(
        matches!(got, Err(PeopleError::Conflict(ref m)) if m.contains("suite-a-z")),
        "{adapter}: a body naming another id: {got:?}"
    );

    let mut taken = row("suite-ab");
    taken.email = Some("suite-boss@SUITE.TEST".into());
    let mut orphan = row("suite-ab");
    orphan.manager_id = Some("suite-nobody".into());
    for (e, names) in [(&taken, "suite-boss@SUITE.TEST"), (&orphan, "suite-nobody")] {
        let got = w
            .repo()
            .update_employee_at("suite-ab", e, at(0), &stamp())
            .await;
        assert!(
            matches!(got, Err(PeopleError::Conflict(ref m)) if m.contains(names)),
            "{adapter}: must be a Conflict naming {names}: {got:?}"
        );
    }
    for id in ["suite-ab", "suite-a-z"] {
        assert_eq!(
            w.repo().employee_by_id(id).await.expect("answers"),
            Some(whole(row(id))),
            "{adapter}: {id} unchanged"
        );
    }
    assert_eq!(
        w.repo()
            .employee_by_id("suite-stranger")
            .await
            .expect("answers"),
        None,
        "{adapter}"
    );
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}

/// A delete removes the row — it leaves the roster and its manager's
/// reports — and records ONE `people.employee.deleted` carrying the id
/// and the instant.
async fn a_delete_removes_the_row_and_records_who_and_when<W: World>(w: &W, adapter: &str) {
    w.repo()
        .delete_employee_at("suite-zz", at(7), &stamp())
        .await
        .expect("the delete lands");
    assert_eq!(
        w.repo().employee_by_id("suite-zz").await.expect("answers"),
        None,
        "{adapter}"
    );
    assert!(
        w.repo()
            .direct_reports("suite-ab")
            .await
            .expect("answers")
            .is_empty(),
        "{adapter}: no longer a report"
    );
    assert!(
        !ids(&w.repo().all_employees().await.expect("answers")).contains(&"suite-zz"),
        "{adapter}: not on the roster"
    );
    assert_eq!(
        w.facts().await.iter().map(stripped).collect::<Vec<_>>(),
        vec![fact(
            EMPLOYEE_DELETED,
            json!({"id": "suite-zz", "deleted_at": at(7)})
        )],
        "{adapter}"
    );
}

/// A delete of an id no row carries is `NotFound`; a delete of someone
/// who still manages a report is a `Conflict` naming the report — the
/// roster never holds a manager it does not hold. Until this suite
/// Postgres refused that as a `Storage` error and the double deleted
/// the manager and left its reports pointing at nobody.
async fn a_delete_of_a_stranger_or_a_manager_is_refused_and_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let got = w
        .repo()
        .delete_employee_at("suite-stranger", at(0), &stamp())
        .await;
    assert!(
        matches!(got, Err(PeopleError::NotFound(ref m)) if m.contains("suite-stranger")),
        "{adapter}: {got:?}"
    );
    let got = w
        .repo()
        .delete_employee_at("suite-ab", at(0), &stamp())
        .await;
    assert!(
        matches!(got, Err(PeopleError::Conflict(ref m)) if m.contains("suite-zz")),
        "{adapter}: a manager of suite-zz: {got:?}"
    );
    assert_eq!(
        w.repo().employee_by_id("suite-ab").await.expect("answers"),
        Some(whole(row("suite-ab"))),
        "{adapter}: the manager stays"
    );
    assert!(w.facts().await.is_empty(), "{adapter}: no fact");
}
