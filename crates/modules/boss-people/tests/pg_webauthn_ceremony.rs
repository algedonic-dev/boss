//! The presence ceremony's storage half, end to end against Postgres.
//!
//! 10-people.sql shipped webauthn_credentials + webauthn_challenges as
//! dormant intent on 2026-08-10; 151-presence-challenge-binding.sql
//! wakes them. These tests pin the storage contract the gateway's
//! ceremony depends on: credential round-trip, single-use challenge
//! consumption (atomic, evidence-leaving), and the presence flow's
//! step binding riding the row.

use axum::http::StatusCode;
use boss_people::webauthn::webauthn_router;
use boss_testing::{TestDb, TestRequest};
use serde_json::json;
use std::sync::Arc;

async fn app() -> (TestDb, axum::Router) {
    let db = TestDb::new().await;
    let router = webauthn_router(
        db.pool.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    (db, router)
}

// b64url("test-credential-id") and a fake COSE key, stable across tests.
const CRED_ID: &str = "dGVzdC1jcmVkZW50aWFsLWlk";
const PUB_KEY: &str = "cHVibGljLWtleS1ieXRlcw";

async fn seed_employee(db: &TestDb, id: &str) {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role) VALUES ($1, $1, $2, 'platform-admin')",
    )
    .bind(id)
    .bind(format!("{id}@test.invalid"))
    .execute(&db.pool)
    .await
    .expect("seed employee");
}

#[tokio::test]
async fn credential_round_trip_and_duplicate_conflict() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-1").await;

    TestRequest::post("/api/people/emp-wa-1/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY, "label": "yubikey-a"}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    // Same credential_id again is a conflict, not an upsert: a
    // credential is bound to one authenticator forever.
    TestRequest::post("/api/people/emp-wa-1/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CONFLICT);

    let resp = TestRequest::get("/api/people/emp-wa-1/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let creds: serde_json::Value = resp.assert_json();
    let list = creds.as_array().expect("array of credentials");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["credential_id"], CRED_ID);
    assert_eq!(list[0]["public_key"], PUB_KEY);
    assert_eq!(list[0]["label"], "yubikey-a");
    assert_eq!(list[0]["access_tier"], "user");
    assert_eq!(list[0]["sign_count"], 0);
    assert!(list[0]["last_used_at"].is_null());
}

#[tokio::test]
async fn recording_a_use_advances_sign_count_and_last_used() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-2").await;

    TestRequest::post("/api/people/emp-wa-2/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/webauthn-credentials/used")
        .json(&json!({"credential_id": CRED_ID, "sign_count": 7}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let resp = TestRequest::get("/api/people/emp-wa-2/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(creds[0]["sign_count"], 7);
    assert!(!creds[0]["last_used_at"].is_null());
}

#[tokio::test]
async fn presence_challenge_consumes_exactly_once_with_binding() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-3").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-1",
            "employee_id": "emp-wa-3",
            "challenge": PUB_KEY,
            "flow": "presence",
            "step_id": "step-42",
            "shape_hash": "abc123",
            "nonce": "6e6f6e6365"
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    let resp = TestRequest::post("/api/people/presence-challenges/ch-1/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let row: serde_json::Value = resp.assert_json();
    assert_eq!(row["employee_id"], "emp-wa-3");
    assert_eq!(row["challenge"], PUB_KEY);
    assert_eq!(row["flow"], "presence");
    assert_eq!(row["step_id"], "step-42");
    assert_eq!(row["shape_hash"], "abc123");
    assert_eq!(row["nonce"], "6e6f6e6365");

    // Second consumption: the row exists but is spent — 410, not 404.
    TestRequest::post("/api/people/presence-challenges/ch-1/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn expired_challenge_is_gone_and_unknown_is_not_found() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-4").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-exp",
            "employee_id": "emp-wa-4",
            "challenge": PUB_KEY,
            "flow": "authenticate",
            "ttl_seconds": 0
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::post("/api/people/presence-challenges/ch-exp/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::GONE);

    TestRequest::post("/api/people/presence-challenges/ch-nope/consume")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let _ = db; // TestDb teardown on drop
}

#[tokio::test]
async fn bad_flow_and_bad_base64_are_rejected_before_the_db() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-5").await;

    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({
            "id": "ch-bad",
            "employee_id": "emp-wa-5",
            "challenge": PUB_KEY,
            "flow": "vibes"
        }))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    TestRequest::post("/api/people/emp-wa-5/webauthn-credentials")
        .json(&json!({"credential_id": "not!!base64", "public_key": PUB_KEY}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let _ = db;
}

#[tokio::test]
async fn an_ordinary_session_is_refused_at_the_storage_door() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-6").await;

    // The gateway proxies /api/people/{*rest} for every session, so
    // these paths are browser-reachable — and a credential row planted
    // for someone else is an account takeover. Ordinary roles stop
    // here; the ceremony's session-to-employee binding lives at the
    // gateway's /api/auth/passkey surface.
    TestRequest::post("/api/people/emp-wa-6/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY}))
        .as_user("emp-wa-6", "qa-lead")
        .send(&router)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    TestRequest::get("/api/people/emp-wa-6/webauthn-credentials")
        .as_user("emp-wa-6", "qa-lead")
        .send(&router)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

// Backlog e199c02d (2026-09-28). The storage door used to admit any
// caller whose ROLE was platform-admin — and the gateway's
// /api/people/{*rest} proxy forwards the session's own role, so the
// owner's user-tier browser cookie, and every agent (which signs as
// platform-admin, 91971374), could store, remove or rekey a passkey and
// mint or spend challenges around the ceremony. The caller is judged by
// ID now, the way the promote door judges it: only the gateway's own
// actor passes. Each of the six handlers is refused for a platform-admin
// that is not the gateway, and the refusal writes nothing.
#[tokio::test]
async fn a_platform_admin_that_is_not_the_gateway_is_refused_on_every_handler() {
    const GATEWAY: &str = boss_core::actor::GATEWAY_ACTOR_ID;
    const BACKUP_ID: &str = "YmFja3VwLWNyZWRlbnRpYWwtaWQ";
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-8").await;
    for id in [CRED_ID, BACKUP_ID] {
        TestRequest::post("/api/people/emp-wa-8/webauthn-credentials")
            .json(&json!({"credential_id": id, "public_key": PUB_KEY}))
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::CREATED);
    }
    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({"id": "ch-8", "employee_id": "emp-wa-8",
                      "challenge": PUB_KEY, "flow": "presence"}))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);

    const NEW_ID: &str = "cGxhbnRlZC1jcmVkZW50aWFsLWlk";
    let attempts: [(TestRequest, &str); 6] = [
        (
            TestRequest::get("/api/people/emp-wa-8/webauthn-credentials"),
            "list",
        ),
        (
            TestRequest::post("/api/people/emp-wa-8/webauthn-credentials")
                .json(&json!({"credential_id": NEW_ID, "public_key": PUB_KEY})),
            "register",
        ),
        (
            TestRequest::delete(format!(
                "/api/people/emp-wa-8/webauthn-credentials/{CRED_ID}"
            )),
            "remove",
        ),
        (
            TestRequest::post("/api/people/webauthn-credentials/used")
                .json(&json!({"credential_id": CRED_ID, "sign_count": 99})),
            "used",
        ),
        (
            TestRequest::post("/api/people/presence-challenges").json(&json!({
                "id": "ch-planted", "employee_id": "emp-wa-8",
                "challenge": PUB_KEY, "flow": "presence"})),
            "mint",
        ),
        (
            TestRequest::post("/api/people/presence-challenges/ch-8/consume"),
            "consume",
        ),
    ];
    for (req, handler) in attempts {
        // The owner's own id, carrying the role his cookie carries.
        let resp = req
            .as_user("emp-wa-8", "platform-admin")
            .send(&router)
            .await;
        resp.assert_status(StatusCode::FORBIDDEN);
        let text = resp.body_text();
        assert!(
            text.contains("only the gateway") && text.contains("whatever its role"),
            "{handler}: the refusal names the rule in the promote door's terms: {text}"
        );
    }

    // Nothing moved: both keys stand, the use was not recorded, no
    // challenge was planted, and the live one is still unspent.
    let creds: Vec<(Vec<u8>, i32)> = sqlx::query_as(
        "SELECT credential_id, sign_count FROM webauthn_credentials
          WHERE employee_id = 'emp-wa-8' ORDER BY credential_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(creds.len(), 2, "no key was removed or planted: {creds:?}");
    assert!(
        creds.iter().all(|(_, n)| *n == 0),
        "no use was recorded: {creds:?}"
    );
    let challenges: Vec<(String, bool)> = sqlx::query_as(
        "SELECT id, used_at IS NOT NULL FROM webauthn_challenges
          WHERE employee_id = 'emp-wa-8' ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(
        challenges,
        vec![("ch-8".to_string(), false)],
        "no challenge was minted or spent"
    );

    // The same six calls, signed as the gateway, pass.
    let resp = TestRequest::get("/api/people/emp-wa-8/webauthn-credentials")
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    TestRequest::post("/api/people/emp-wa-8/webauthn-credentials")
        .json(&json!({"credential_id": NEW_ID, "public_key": PUB_KEY}))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::delete(format!(
        "/api/people/emp-wa-8/webauthn-credentials/{CRED_ID}"
    ))
    .as_user(GATEWAY, "platform-admin")
    .send(&router)
    .await
    .assert_status(StatusCode::NO_CONTENT);
    TestRequest::post("/api/people/webauthn-credentials/used")
        .json(&json!({"credential_id": BACKUP_ID, "sign_count": 3}))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    TestRequest::post("/api/people/presence-challenges")
        .json(&json!({"id": "ch-8b", "employee_id": "emp-wa-8",
                      "challenge": PUB_KEY, "flow": "presence"}))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);
    TestRequest::post("/api/people/presence-challenges/ch-8/consume")
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
}

// Backlog 1d9970d1 (2026-09-28). The POST used to take `access_tier`
// from its body, and its only gate is the platform-admin ROLE — which the
// owner's user-tier browser session carries through the gateway's
// /api/people proxy. So that cookie could store an OPERATOR-tier key of
// its own making and then elevate with it: H1 (review of car 0bde9b99)
// reached one layer below the ceremony. The body no longer names a tier.
// A body that tries is REFUSED, not quietly ignored, so a caller that
// still believes it can choose learns otherwise loudly — and nothing is
// stored.
#[tokio::test]
async fn a_body_cannot_choose_the_tier_a_credential_is_stored_at() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-7").await;

    for tier in ["operator", "user"] {
        TestRequest::post("/api/people/emp-wa-7/webauthn-credentials")
            .json(&json!({
                "credential_id": CRED_ID,
                "public_key": PUB_KEY,
                "access_tier": tier,
            }))
            .as_user("automation:gateway", "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    }

    let resp = TestRequest::get("/api/people/emp-wa-7/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(
        creds.as_array().map(Vec::len),
        Some(0),
        "a refused body stores nothing: {creds}"
    );

    // The ordinary enrolment — what the gateway's register_finish sends —
    // still stores, and what it stores is user tier.
    TestRequest::post("/api/people/emp-wa-7/webauthn-credentials")
        .json(&json!({"credential_id": CRED_ID, "public_key": PUB_KEY, "label": "macbook"}))
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CREATED);
    let resp = TestRequest::get("/api/people/emp-wa-7/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(creds.as_array().map(Vec::len), Some(1));
    assert_eq!(creds[0]["access_tier"], "user");
}

// The pin behind the test above: no request body in the credential
// storage module can carry the tier at all. The only writer of an
// operator-tier row is to be a separate, recorded promotion act (design
// answering 2a228d0c), never a field on a body. A body struct that grows
// the field, or a handler that takes an untyped JSON body (which would
// carry any key it liked), fails here and names itself.
#[test]
fn no_request_body_in_the_credential_store_names_the_tier() {
    let src = include_str!("../src/webauthn.rs");
    let bodies: Vec<&str> = src
        .match_indices("#[derive(Deserialize")
        .map(|(at, _)| {
            let rest = &src[at..];
            let end = rest.find("\n}\n").map_or(rest.len(), |e| e + 3);
            &rest[..end]
        })
        .collect();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains("struct RegisterCredentialBody")),
        "the pin must see the credential POST's body struct; it found: {bodies:#?}"
    );
    for body in &bodies {
        assert!(
            !body.contains("access_tier"),
            "a request body in webauthn.rs carries access_tier — a caller could \
             choose the tier its credential is stored at (backlog 1d9970d1):\n{body}"
        );
    }
    for untyped in ["Json<serde_json::Value>", "Json<Value>"] {
        assert!(
            !src.contains(untyped),
            "webauthn.rs takes an untyped `{untyped}` body, which can carry \
             access_tier past the struct pin above"
        );
    }
}

// David, feedback 16414d99 (2026-09-10): "Let me manage the passkeys on
// my account… Important so users can add a backup key, but don't let
// them delete all their keys." Removal exists so a lost or retired
// authenticator can go; the LAST one stays, because a person with no
// passkey cannot pass a presence-gated step and the surface that let
// them do that would be the one that locked them out.
#[tokio::test]
async fn a_passkey_can_be_removed_but_never_the_last_one() {
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-5").await;
    const BACKUP_ID: &str = "YmFja3VwLWNyZWRlbnRpYWwtaWQ";

    for (id, label) in [(CRED_ID, "yubikey-a"), (BACKUP_ID, "backup")] {
        TestRequest::post("/api/people/emp-wa-5/webauthn-credentials")
            .json(&json!({"credential_id": id, "public_key": PUB_KEY, "label": label}))
            .as_user("automation:gateway", "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::CREATED);
    }

    // Two keys: one may go.
    TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{CRED_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    // One key: it stays, and the refusal says why in the user's terms.
    let resp = TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{BACKUP_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await;
    resp.assert_status(StatusCode::CONFLICT);
    let text = resp.body_text();
    assert!(
        text.contains("last passkey") && text.contains("backup"),
        "the refusal names the rule and the way out: {text}"
    );

    let resp = TestRequest::get("/api/people/emp-wa-5/webauthn-credentials")
        .as_user("automation:gateway", "platform-admin")
        .send(&router)
        .await;
    let creds: serde_json::Value = resp.assert_json();
    assert_eq!(creds.as_array().map(Vec::len), Some(1));
    assert_eq!(creds[0]["credential_id"], BACKUP_ID);

    // A credential that is not there, or is somebody else's, is 404 —
    // never a silent 204 that reads as "removed".
    TestRequest::delete(format!(
        "/api/people/emp-wa-5/webauthn-credentials/{CRED_ID}"
    ))
    .as_user("automation:gateway", "platform-admin")
    .send(&router)
    .await
    .assert_status(StatusCode::NOT_FOUND);
}

// Design 1c4e42e1 (backlog 47aed706): the coverage read needs who holds
// a key and at which tier — a real person is an active employee with a
// bound passkey, and the operator tier is a platform-admin with an
// operator-tier one. The tier counts answer exactly that and nothing
// else, to a caller at the operator or auditor tier: machinery, and the
// owner's own ELEVATED session (which the gateway signs at operator) —
// never a user-tier session, and a header claim until 2710c8fc.
#[tokio::test]
async fn the_tier_counts_answer_machinery_with_counts_and_no_key_material() {
    const BACKUP_ID: &str = "YmFja3VwLWNyZWRlbnRpYWwtaWQ";
    let (db, router) = app().await;
    seed_employee(&db, "emp-wa-9").await;
    seed_employee(&db, "emp-wa-10").await;
    for id in [CRED_ID, BACKUP_ID] {
        TestRequest::post("/api/people/emp-wa-9/webauthn-credentials")
            .json(&json!({"credential_id": id, "public_key": PUB_KEY}))
            .as_user(boss_core::actor::GATEWAY_ACTOR_ID, "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::CREATED);
    }
    // Asked by the contract crate's spelling of the path, which is what
    // the policy service reads: a router literal that drifted from it
    // answers 404 here. (An operator-tier row is written only by the
    // promotion's own transaction, and only that module spells its mark;
    // the tier split is pinned on the reading side, boss-policy
    // `keys_from_row`.)
    use boss_policy_client::coverage::TIER_COUNTS_PATH;
    let machinery =
        r#"{"id":"automation:policy-coverage","role":"platform-admin","access_tier":"operator"}"#;
    let resp = TestRequest::get(TIER_COUNTS_PATH)
        .header("x-boss-user", machinery)
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();
    assert_eq!(
        body["total"], 1,
        "the keyless employee is not listed: {body}"
    );
    assert_eq!(
        body["data"][0],
        json!({"employee_id": "emp-wa-9", "user": 2, "operator": 0}),
        "counts only — no credential id, key, label or time: {body}"
    );
    let auditor = r#"{"id":"automation:audit","role":"audit-readonly","access_tier":"auditor"}"#;
    TestRequest::get(TIER_COUNTS_PATH)
        .header("x-boss-user", auditor)
        .send(&router)
        .await
        .assert_status(StatusCode::OK);

    // A user-tier session is refused, whatever its role — the owner's
    // unelevated cookie carries platform-admin at the user tier.
    for who in [
        ("emp-wa-9", "platform-admin"),
        (boss_core::actor::GATEWAY_ACTOR_ID, "platform-admin"),
    ] {
        TestRequest::get(TIER_COUNTS_PATH)
            .as_user(who.0, who.1)
            .send(&router)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
}
