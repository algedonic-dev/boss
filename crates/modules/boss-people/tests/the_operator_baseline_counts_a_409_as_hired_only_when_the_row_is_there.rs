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
    let diagnostic = err.to_string();
    assert!(
        diagnostic.contains("emp-op-2")
            && diagnostic.contains("POST")
            && diagnostic.contains("409")
            && diagnostic.contains("email"),
        "the returned error must retain the actual refused hire without a tracing subscriber: {diagnostic}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_post_retains_its_status_and_body_without_a_log_subscriber() {
    // The wire response is the evidence, even when nobody subscribed to tracing
    // (48b0a505). Exercise the same blocking seed client, not a formatted fake error.
    let app = axum::Router::new().route(
        "/api/people",
        axum::routing::get(|| async { axum::Json(vec![emp("emp-held", "held@suite.test")]) }).post(
            || async {
                (
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "diagnostic fixture: missing manager",
                )
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let target = format!("{base}/api/people");
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let diagnostic = seed(base, "op-baseline-post-diagnostic", HIRES)
        .await
        .expect_err("the refusal must fail the baseline")
        .to_string();
    server.abort();
    for expected in [
        "emp-op-1",
        "emp-op-2",
        "POST",
        &target,
        "422",
        "missing manager",
    ] {
        assert!(
            diagnostic.contains(expected),
            "missing {expected}: {diagnostic}"
        );
    }
    let evidence = post_evidence(&diagnostic);
    assert_eq!(evidence["inserted"], 0);
    assert_eq!(evidence["skipped"], 0);
    assert_eq!(evidence["failures"].as_array().unwrap().len(), 2);
    for failure in evidence["failures"].as_array().unwrap() {
        assert_eq!(failure["kind"], "response");
        assert_eq!(failure["status"], 422);
        assert_eq!(failure["body"], "diagnostic fixture: missing manager");
    }
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

/// Real HTTP headers followed by either no response or an incomplete body.
/// The GET supplies the held bootstrap identity; only the POST fails.
fn broken_post_server(response: &'static str) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            let length = request
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            if request.starts_with("GET ") {
                let body =
                    serde_json::to_string(&vec![emp("emp-held", "held@suite.test")]).unwrap();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            } else {
                socket.write_all(response.as_bytes()).unwrap();
            }
        }
    });
    (base, worker)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_post_transport_is_distinct_from_a_service_refusal() {
    let (base, worker) = broken_post_server("");
    let target = format!("{base}/api/people");
    let diagnostic = seed(base, "op-baseline-post-transport", HIRES)
        .await
        .unwrap_err()
        .to_string();
    worker.join().unwrap();
    for expected in ["emp-op-1", "emp-op-2", "POST", &target, "transport"] {
        assert!(
            diagnostic.contains(expected),
            "missing {expected}: {diagnostic}"
        );
    }
    for failure in post_evidence(&diagnostic)["failures"].as_array().unwrap() {
        assert_eq!(failure["kind"], "transport");
        assert!(failure["error"].as_str().unwrap().contains("connection"));
        assert!(failure.get("status").is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_response_body_is_not_reported_as_an_empty_body() {
    let (base, worker) = broken_post_server(
        "HTTP/1.1 422 Unprocessable Entity\r\nContent-Length: 99\r\nConnection: close\r\n\r\nshort",
    );
    let diagnostic = seed(base, "op-baseline-post-body-read", HIRES)
        .await
        .unwrap_err()
        .to_string();
    worker.join().unwrap();
    for expected in ["emp-op-1", "emp-op-2", "422", "body_read"] {
        assert!(
            diagnostic.contains(expected),
            "missing {expected}: {diagnostic}"
        );
    }
    for failure in post_evidence(&diagnostic)["failures"].as_array().unwrap() {
        assert_eq!(failure["kind"], "body_read");
        assert_eq!(failure["status"], 422);
        assert!(!failure["error"].as_str().unwrap().is_empty());
        assert!(
            failure.get("body").is_none(),
            "a failed read must not invent a body"
        );
    }
}

fn post_evidence(diagnostic: &str) -> serde_json::Value {
    serde_json::from_str(diagnostic.split_once("\nPOST failures: ").unwrap().1).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transport_failure_does_not_copy_url_authentication_into_evidence() {
    let (base, worker) = broken_post_server("");
    let mut url = reqwest::Url::parse(&base).unwrap();
    let fixture_secret = format!("fixture-private-{}", std::process::id());
    url.set_username("fixture-user").unwrap();
    url.set_password(Some(&fixture_secret)).unwrap();
    let diagnostic = seed(
        url.to_string().trim_end_matches('/').to_string(),
        "op-baseline-private-target",
        HIRES,
    )
    .await
    .unwrap_err()
    .to_string();
    worker.join().unwrap();
    assert!(
        !diagnostic.contains(&fixture_secret),
        "URL authentication must stay out of evidence"
    );
    assert!(!diagnostic.contains("fixture-user"));
    for failure in post_evidence(&diagnostic)["failures"].as_array().unwrap() {
        assert_eq!(failure["target"], format!("{base}/api/people"));
    }
}
