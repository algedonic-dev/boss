//! The operator-baseline seed counts a 409 from `POST /api/people` as
//! "already hired" only when the row is there.
//!
//! WHY IT EXISTS. Since the people roster's adapters were held to one
//! suite (backlog be459ab9) both answer a taken email, an absent
//! manager or a malformed row as a `Conflict` — a 409 naming the value —
//! where Postgres used to answer a 500. The seed counted EVERY 409 as
//! "operator already hired, skipping", so the same refusal that failed
//! the run loudly as a 500 would have passed it silently as a 409, and a
//! founding operator would be missing with the seed green. It already
//! did that for a role or location the registries refuse (409 since
//! they were wired). `boss tenant publish` learned the same lesson on
//! 2026-09-16 (backlog 0d2d7daa): a 409 is "already there" only when a
//! GET finds the row.

use std::sync::Arc;

use boss_people::http::{PeopleApiState, router};
use boss_people::{Employee, InMemoryPeople};
use boss_policy_client::{PermissivePolicyClient, PolicyClient};

fn emp(id: &str, email: &str) -> Employee {
    Employee {
        id: id.into(),
        name: Some(id.into()),
        email: Some(email.into()),
        role: None,
        department: None,
        skill_level: None,
        skills: vec![],
        hire_date: None,
        location: None,
        manager_id: None,
        employment_type: None,
        status: None,
        certifications: vec![],
        annual_salary_cents: None,
    }
}

/// The people router over a double holding `roster`, on an ephemeral
/// port.
async fn serve(roster: Vec<Employee>) -> (String, Arc<InMemoryPeople>) {
    let people = Arc::new(InMemoryPeople::new(roster));
    let app = router(PeopleApiState {
        people: people.clone(),
        publisher: None,
        policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
        subject_kinds: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (base, people)
}

/// Run the seed over `hires` against `base`. The bootstrap email names
/// a row the roster already holds, so the seed injects nothing of its
/// own.
async fn seed(
    base: String,
    name: &str,
    hires: &str,
) -> anyhow::Result<boss_people::operator_baseline::Summary> {
    // SAFETY: a process-wide env var, set to the one value every case
    // in this file wants, before the only reader (the seed) runs.
    unsafe { std::env::set_var("BOSS_BOOTSTRAP_ADMIN_EMAIL", "held@suite.test") };
    let dir = boss_testing::scratch_dir(name);
    let path = dir.join("operator_hires.toml");
    std::fs::write(&path, hires).expect("write the seed file");
    tokio::task::spawn_blocking(move || {
        let client = boss_core::machine_token::BlockingClient::build_with_source(
            reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(15)),
            Arc::new(boss_core::machine_token::Source::fixed(None)),
        )
        .expect("a blocking client");
        boss_people::operator_baseline::seed(&client, &base, &path, None)
    })
    .await
    .expect("the seed thread")
}

const HIRES: &str = r#"
[[hire]]
id = "emp-op-1"
name = "Op One"
email = "op1@suite.test"
skills = []
certifications = []

[[hire]]
id = "emp-op-2"
name = "Op Two"
email = "HELD@suite.test"
skills = []
certifications = []
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_409_for_a_row_that_is_not_there_fails_the_seed() {
    let (base, people) = serve(vec![
        emp("emp-held", "held@suite.test"),
        emp("emp-op-1", "op1@suite.test"),
    ])
    .await;
    let got = seed(base, "op-baseline-409-not-there", HIRES).await;
    let err = got.expect_err("a refused hire must fail the seed");
    assert!(
        err.to_string().contains("1 operator-baseline POSTs failed")
            && err.to_string().contains("skipped=1"),
        "the held id is skipped, the refused one fails: {err}"
    );
    assert!(
        !hired(&people).iter().any(|id| id == "emp-op-2"),
        "the refused hire did not land"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_409_for_a_row_that_is_there_is_already_hired() {
    let (base, _people) = serve(vec![
        emp("emp-held", "held@suite.test"),
        emp("emp-op-1", "op1@suite.test"),
    ])
    .await;
    let hires = HIRES.replace("HELD@suite.test", "op2@suite.test");
    let summary = seed(base, "op-baseline-409-there", &hires)
        .await
        .expect("the seed lands");
    assert_eq!(
        (summary.inserted, summary.skipped),
        (1, 1),
        "emp-op-2 hired, emp-op-1 already there"
    );
}

/// The ids the double recorded a create for.
fn hired(people: &InMemoryPeople) -> Vec<String> {
    people
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_people::events::EMPLOYEE_CREATED)
        .filter_map(|e| e.payload["id"].as_str().map(String::from))
        .collect()
}
