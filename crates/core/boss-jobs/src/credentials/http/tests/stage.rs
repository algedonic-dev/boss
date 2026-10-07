//! The rotation door behind the broker-stage door (design 6e28ed42;
//! backlog 1e50e66b). The first test is the reproduction the design asks
//! for — a forged label records a runner stage — and it stays, because
//! with the door off that is still what happens: the decision authorized
//! no enforcement flip.

use super::*;
use crate::credentials::broker_stage::tests::{
    Packets, claims_at, cluster, keys, rotation, stranger, terms,
};
use crate::credentials::broker_stage::{self, Door, Refusal};
use crate::credentials::{CredentialRow, InMemoryCredentials};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const RUNNER: &str = "ops-runner-credential-forge";
const OTHER: &str = "boss-dev-forge-token";

fn row(id: &str, kind: &str) -> CredentialRow {
    CredentialRow {
        id: id.into(),
        kind: kind.into(),
        issuer: "test".into(),
        principal: "runner:ops".into(),
        scopes: json!([]),
        storage_location: "secret boss/test".into(),
        consumers: json!([]),
        rotation_policy: "on-demand".into(),
        rotated_at: None,
        notes: String::new(),
    }
}

/// A header a holder of the machine token can type: operator tier, and
/// any rule label it likes.
fn forged_label() -> String {
    json!({"id":"rule:broker-rotates-the-forge-ops-runner-credential","role":"platform-admin",
           "access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],
           "department":"platform"})
    .to_string()
}

const FORGED_ACTOR: &str = "automation:rule:broker-rotates-the-forge-ops-runner-credential";

struct World {
    app: Router,
    registry: Arc<InMemoryCredentials>,
    packet: uuid::Uuid,
    foreign: uuid::Uuid,
    closed: uuid::Uuid,
}

fn on() -> Door {
    Door::On {
        terms: terms(),
        keys: Arc::new(keys()),
    }
}

fn world_over(registry: Arc<InMemoryCredentials>, door: Door, packet: uuid::Uuid) -> World {
    let (foreign, closed) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let door = Arc::new(door);
    let packets: Arc<dyn broker_stage::StagePackets> = Arc::new(Packets(vec![
        (packet, rotation(RUNNER, true)),
        (foreign, rotation("ops-runner-credential-boss-gcp", true)),
        (closed, rotation(RUNNER, false)),
    ]));
    let app =
        router(CredentialsApiState::new(registry.clone()).with_stage(door.clone(), Some(packets)));
    World {
        app: broker_stage::mount(app, door),
        registry,
        packet,
        foreign,
        closed,
    }
}

fn world(door: Door) -> World {
    let registry = Arc::new(InMemoryCredentials::new(vec![
        row(RUNNER, broker_stage::RUNNER_KIND),
        row(OTHER, "forgejo-access-token"),
    ]));
    world_over(registry, door, uuid::Uuid::new_v4())
}

fn token() -> String {
    cluster().sign(&claims_at(chrono::Utc::now().timestamp()))
}

fn command(job: uuid::Uuid, phase: &str) -> Value {
    json!({"job_id": job.to_string(), "host": "forge",
           "observation_id": format!("0b5d4f0e-1111-4222-8333-444455556666:{phase}")})
}

async fn post(
    app: &Router,
    id: &str,
    phase: &str,
    body: &Value,
    tokens: &[&str],
) -> (StatusCode, String) {
    let mut request = Request::post(format!("/api/credentials/{id}/rotation/{phase}"))
        .header("x-boss-user", forged_label())
        .header("content-type", "application/json");
    for token in tokens {
        request = request.header(broker_stage::HEADER, *token);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn actors(registry: &InMemoryCredentials) -> Vec<String> {
    registry
        .recorded_events()
        .unwrap()
        .iter()
        .map(|e| e.payload["_actor"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// THE REPRODUCTION, and the inactive behaviour this car must not change:
/// with the door off, a typed label records a runner credential's stage.
#[tokio::test]
async fn with_the_door_off_a_forged_label_still_records_a_runner_stage() {
    let w = world(Door::Off);
    let (status, _) = post(&w.app, RUNNER, "minted", &command(w.packet, "minted"), &[]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(actors(&w.registry), [FORGED_ACTOR]);
}

#[tokio::test]
async fn with_the_door_on_a_forged_label_records_nothing() {
    let w = world(on());
    for phase in RotationPhase::ALL {
        let phase = phase.as_str();
        let (status, said) = post(&w.app, RUNNER, phase, &command(w.packet, phase), &[]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{phase}");
        assert!(said.contains(&Refusal::NonePresented.to_string()), "{said}");
    }
    assert!(actors(&w.registry).is_empty());
}

#[tokio::test]
async fn a_verified_stage_is_recorded_as_the_workload_whatever_label_was_typed() {
    let w = world(on());
    for phase in RotationPhase::ALL {
        let phase = phase.as_str();
        let (status, said) = post(
            &w.app,
            RUNNER,
            phase,
            &command(w.packet, phase),
            &[&token()],
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{phase}: {said}");
        let answer: Value = serde_json::from_str(&said).unwrap();
        assert_eq!(
            answer["observation"]["receipt"]["actor"],
            broker_stage::ACTOR
        );
    }
    assert_eq!(actors(&w.registry), [broker_stage::ACTOR; 4]);
}

#[tokio::test]
async fn every_token_that_is_not_the_stage_workloads_is_a_named_refusal() {
    let now = chrono::Utc::now().timestamp();
    let with = |patch: Value| {
        let mut claims = claims_at(now);
        for (key, value) in patch.as_object().unwrap() {
            claims[key] = value.clone();
        }
        claims
    };
    let good = token();
    let bound = |ns: &str, sa: &str| json!({"namespace":ns,"serviceaccount":{"name":sa}});
    let cases: Vec<(&str, Vec<String>, Refusal)> = vec![
        (
            "the cluster's own audience",
            vec![cluster().sign(&with(json!({"aud":[terms().issuer]})))],
            Refusal::Audience,
        ),
        (
            "expired",
            vec![cluster().sign(&with(json!({"iat":now-900,"nbf":now-900,"exp":now-300})))],
            Refusal::Expired,
        ),
        (
            "an hour's lifetime",
            vec![cluster().sign(&with(json!({"exp":now+3540})))],
            Refusal::Lifetime,
        ),
        (
            "another namespace's workload",
            vec![
                cluster().sign(&with(json!({"sub":"system:serviceaccount:playground:boss",
                "kubernetes.io":bound("playground","boss")}))),
            ],
            Refusal::Workload,
        ),
        (
            "another ServiceAccount",
            vec![
                cluster().sign(&with(json!({"sub":"system:serviceaccount:boss:default",
                "kubernetes.io":bound("boss","default")}))),
            ],
            Refusal::Workload,
        ),
        (
            "another issuer",
            vec![cluster().sign(&with(json!({"iss":"https://other.test"})))],
            Refusal::Issuer,
        ),
        (
            "a key the cluster never published",
            vec![stranger().sign(&claims_at(now))],
            Refusal::Signature,
        ),
        (
            "not a token",
            vec!["opaque-bearer".into()],
            Refusal::Malformed,
        ),
        ("an empty header", vec![String::new()], Refusal::Malformed),
        (
            "two tokens",
            vec![good.clone(), good.clone()],
            Refusal::MultiplePresented,
        ),
    ];
    let w = world(on());
    for (name, tokens, refusal) in cases {
        let presented: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let (status, said) = post(
            &w.app,
            RUNNER,
            "minted",
            &command(w.packet, "minted"),
            &presented,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
        assert!(said.contains(&refusal.to_string()), "{name}: {said}");
        for token in tokens.iter().filter(|t| !t.is_empty()) {
            assert!(
                !said.contains(token.as_str()),
                "{name}: the answer echoes the token"
            );
            let signature = token.rsplit('.').next().unwrap();
            assert!(!said.contains(signature), "{name}");
        }
    }
    assert!(actors(&w.registry).is_empty(), "a refusal records nothing");
}

#[tokio::test]
async fn a_door_that_cannot_verify_refuses_a_presented_token_and_a_broken_one_refuses_all() {
    // Off: a presented identity is never quietly ignored on a runner row.
    let w = world(Door::Off);
    let (status, said) = post(
        &w.app,
        RUNNER,
        "minted",
        &command(w.packet, "minted"),
        &[&token()],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(said.contains(&Refusal::NotConfigured.to_string()), "{said}");
    assert!(actors(&w.registry).is_empty());

    // Partly configured is not off.
    let w = world(Door::Broken("BOSS_BROKER_STAGE_KEY_SET unset".into()));
    for tokens in [vec![], vec![token()]] {
        let presented: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let (status, said) = post(
            &w.app,
            RUNNER,
            "minted",
            &command(w.packet, "minted"),
            &presented,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(said.contains("BOSS_BROKER_STAGE_KEY_SET unset"), "{said}");
    }
    assert!(actors(&w.registry).is_empty());
}

#[tokio::test]
async fn a_verified_stage_reaches_only_the_open_packet_on_its_own_credential() {
    let w = world(on());
    for (name, job) in [
        ("another credential's packet", json!(w.foreign.to_string())),
        ("a closed packet", json!(w.closed.to_string())),
        (
            "a packet nobody filed",
            json!(uuid::Uuid::new_v4().to_string()),
        ),
        ("no packet", Value::Null),
    ] {
        let mut body = command(w.packet, "minted");
        body["job_id"] = job;
        let (status, said) = post(&w.app, RUNNER, "minted", &body, &[&token()]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}");
        assert!(
            said.contains(&Refusal::Packet.to_string()),
            "{name}: {said}"
        );
    }
    // No packet reader behind an enforcing door is a refusal, not a pass.
    let door = Arc::new(on());
    let app = broker_stage::mount(
        router(CredentialsApiState::new(w.registry.clone()).with_stage(door.clone(), None)),
        door,
    );
    let (status, said) = post(
        &app,
        RUNNER,
        "minted",
        &command(w.packet, "minted"),
        &[&token()],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        said.contains(&Refusal::PacketUnavailable.to_string()),
        "{said}"
    );
    assert!(actors(&w.registry).is_empty());
}

/// Design 5840's legacy phase behaviour: every credential that is not a
/// runner's is recorded exactly as before, door on or off, and a stage
/// token — good or bad — neither helps nor hinders it.
#[tokio::test]
async fn another_credentials_phases_are_unchanged_and_a_token_confers_nothing_there() {
    for door in [Door::Off, on(), Door::Broken("x".into())] {
        let w = world(door);
        let body = json!({"job_id": w.packet.to_string(), "token_id": 101});
        for tokens in [vec![], vec![token()], vec!["garbage".to_owned()]] {
            let presented: Vec<&str> = tokens.iter().map(String::as_str).collect();
            let (status, _) = post(&w.app, OTHER, "minted", &body, &presented).await;
            assert_eq!(status, StatusCode::ACCEPTED);
        }
        assert_eq!(actors(&w.registry), [FORGED_ACTOR; 3]);
    }
}

/// A REPLAY ACROSS STAGES, three ways: the stage token at the host's
/// delivery door, the stage token beside a host's runner credential, and
/// the recorded command replayed under a typed label.
#[tokio::test]
async fn a_stage_token_is_not_delivery_authority_and_a_host_credential_is_not_stage_authority() {
    let w = world(on());
    let dir = boss_testing::scratch_dir("broker-stage-across-doors");
    let host_value = "fake-host-runner-generation-for-the-stage-control";
    std::fs::write(dir.join("forge.next"), host_value).unwrap();
    let app = crate::runner_credential::mount(w.app.clone(), dir);

    let delivery = app
        .clone()
        .oneshot(
            Request::post(format!("/api/credentials/{RUNNER}/delivery"))
                .header("x-boss-user", forged_label())
                .header(broker_stage::HEADER, token())
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"job_id":w.packet.to_string(),
                           "attempt":uuid::Uuid::new_v4().to_string(),"secret_uid":"uid-1"})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delivery.status(), StatusCode::FORBIDDEN);

    let both = app
        .oneshot(
            Request::post(format!("/api/credentials/{RUNNER}/rotation/minted"))
                .header("x-boss-user", forged_label())
                .header(broker_stage::HEADER, token())
                .header(crate::runner_credential::HEADER, host_value)
                .header("content-type", "application/json")
                .body(Body::from(command(w.packet, "minted").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(both.status(), StatusCode::FORBIDDEN);
    assert!(actors(&w.registry).is_empty());
}

#[tokio::test]
async fn a_replay_is_bound_to_the_verified_actor_and_the_original_is_conserved() {
    let w = world(on());
    let body = command(w.packet, "installed");
    let (status, first) = post(&w.app, RUNNER, "installed", &body, &[&token()]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let first: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["observation"]["outcome"], "recorded");

    // A lost acknowledgment, replayed by the workload with a LATER token.
    let (status, again) = post(&w.app, RUNNER, "installed", &body, &[&token()]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let again: Value = serde_json::from_str(&again).unwrap();
    assert_eq!(again["observation"]["outcome"], "replayed");
    assert_eq!(
        again["observation"]["receipt"],
        first["observation"]["receipt"]
    );

    // The same command under a typed label, through a door that is off
    // (the pre-activation deployment): the original is not rewritten.
    let off = world_over(w.registry.clone(), Door::Off, w.packet);
    let (status, _) = post(&off.app, RUNNER, "installed", &body, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // And a changed command under the verified actor is a conflict too.
    let mut changed = body.clone();
    changed["host"] = json!("boss-gcp");
    let (status, _) = post(&w.app, RUNNER, "installed", &changed, &[&token()]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(actors(&w.registry), [broker_stage::ACTOR]);
}

/// IN-FLIGHT, stated rather than inferred: a phase recorded under a rule
/// label before activation keeps its original actor, and the verified
/// workload's replay of it is a conflict — never a rewrite. An activation
/// therefore needs no runner rotation open (there has never been one).
#[tokio::test]
async fn a_phase_recorded_before_activation_keeps_its_original_actor() {
    let before = world(Door::Off);
    let body = command(before.packet, "minted");
    let (status, _) = post(&before.app, RUNNER, "minted", &body, &[]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let after = world_over(before.registry.clone(), on(), before.packet);
    let (status, _) = post(&after.app, RUNNER, "minted", &body, &[&token()]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(actors(&before.registry), [FORGED_ACTOR]);
}

/// The token is taken off the request at the door, whatever was made of
/// it: no handler, log or proxy downstream holds it.
#[tokio::test]
async fn the_token_never_reaches_a_handler() {
    for door in [Door::Off, on()] {
        let app = broker_stage::mount(
            Router::new().route(
                "/echo",
                get(|headers: axum::http::HeaderMap| async move {
                    headers.contains_key(broker_stage::HEADER).to_string()
                }),
            ),
            Arc::new(door),
        );
        for tokens in [
            vec![token()],
            vec![token(), token()],
            vec!["garbage".to_owned()],
        ] {
            let mut request = Request::get("/echo");
            for token in &tokens {
                request = request.header(broker_stage::HEADER, token);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&bytes[..], b"false");
        }
    }
}
