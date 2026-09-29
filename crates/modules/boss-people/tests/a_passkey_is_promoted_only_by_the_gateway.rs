//! A passkey reaches the operator tier through ONE door, the gateway's
//! (design 2cb6256f D5-D7, David 2026-09-28; backlog 2a228d0c).
//!
//! `POST /api/people/{id}/webauthn-credentials/{credential_id}/promote`
//! is the only write in boss-people that turns a stored credential into
//! an operator-tier key, and an operator-tier key is what raises the
//! platform owner's session to operator. These tests pin, end to end
//! against Postgres:
//! - the caller is judged by ACTOR ID, not role (a copied owner cookie
//!   carries platform-admin);
//! - the id is not enough: the door reads the named packet and refuses,
//!   writing nothing, unless the owner approved THIS key with a live
//!   presence stamp and the promotion is unspent (adversarial review of
//!   this car, findings 1-2; every check is also pinned one by one on
//!   the pure judge in `src/passkey_promotion.rs`);
//! - the flip and `auth.passkey.promoted` land in one transaction (D6);
//! - a replay under the same packet is harmless, under another is
//!   refused (D7, finding 7);
//! - the table refuses operator, and a rekey of an operator row, from
//!   any writer but the promotion (F4 of car 1d9970d1, finding 3);
//! - a key deleted and re-registered under the approved credential id
//!   promotes nothing (review of car 293d5dc1).
//!
//! - nothing is promoted without a promotion ticket the gateway signed
//!   with the session key for THIS packet, employee and key, the voucher
//!   is read from that ticket and never from the body, and a ticket is
//!   spent once (car 2 of the design; review of car 293d5dc1).
//!
//! These tests mount `promotion_router` themselves, with a fixed key;
//! the service binary mounts it with the gateway's key file — pinned by
//! `the_service_mounts_the_promote_door_with_its_ticket_key` in
//! `src/passkey_promotion.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::StatusCode;
use boss_people::passkey_promotion::{
    AUTHORISE_STEP, PROMOTE_STEP, PROMOTION_KIND, PromotionPackets, PromotionTicket, TicketKey,
    promotion_router,
};
use boss_people::webauthn::webauthn_router;
use boss_testing::{TestDb, TestRequest};
use serde_json::{Value, json};
use uuid::Uuid;

const GATEWAY: &str = boss_core::actor::GATEWAY_ACTOR_ID;
const OWNER: &str = "emp-owner";
const PACKET: &str = "7a1d2c3b-0000-4000-8000-00000000c0de";
const OTHER_PACKET: &str = "7a1d2c3b-0000-4000-8000-0000000c0de2";

// b64url("promote-credential-id") and a fake COSE key.
const CRED_ID: &str = "cHJvbW90ZS1jcmVkZW50aWFsLWlk";
const PUB_KEY: &str = "cHVibGljLWtleS1ieXRlcw";
// b64url("attacker-key-bytes").
const OTHER_PUB_KEY: &str = "YXR0YWNrZXIta2V5LWJ5dGVz";
const REGISTERED_AT: &str = "2026-09-28T09:00:00Z";
/// Stands in for the gateway's session key, which signs the ticket.
const SESSION_KEY: &[u8] = b"people-promotion-test-session-key";

/// The jobs API held in memory: packets by id, and how often one was
/// read (a refusal before the read must not read at all).
#[derive(Default)]
struct MemoryPackets {
    packets: Mutex<HashMap<Uuid, Value>>,
}

impl MemoryPackets {
    fn put(&self, p: Value) {
        let id = Uuid::parse_str(p["id"].as_str().unwrap()).unwrap();
        self.packets.lock().unwrap().insert(id, p);
    }
}

#[async_trait]
impl PromotionPackets for MemoryPackets {
    async fn packet(&self, job_id: Uuid) -> Result<Option<Value>, String> {
        Ok(self.packets.lock().unwrap().get(&job_id).cloned())
    }
}

/// A `passkey-promotion` packet the owner approved with his passkey for
/// THIS key, `promote` waiting — the one state that authorises.
fn approved(id: &str) -> Value {
    let mut p = json!({
        "id": id,
        "kind": PROMOTION_KIND,
        "status": "open",
        "steps": [
            {
                "id": Uuid::new_v4(),
                "job_id": id,
                "spec_slug": AUTHORISE_STEP,
                "kind": "sign-off",
                "title": "Authorise making yubikey-5c an operator key",
                "status": "completed",
                "metadata": {
                    "employee_id": OWNER,
                    "credential_id": CRED_ID,
                    "label": "yubikey-5c",
                    "registered_at": REGISTERED_AT,
                    "public_key_sha256":
                        boss_core::presence::public_key_fingerprint(&base64_decode(PUB_KEY)),
                    "decision": "approved",
                },
                "sign_offs": [],
            },
            {
                "id": Uuid::new_v4(),
                "job_id": id,
                "spec_slug": PROMOTE_STEP,
                "kind": "task",
                "title": "Finish on /me",
                "status": "ready",
                "metadata": {},
            },
        ],
    });
    let step = &mut p["steps"][0];
    let hash = boss_core::job::step_shape_hash(step["title"].as_str().unwrap(), &step["metadata"]);
    step["sign_offs"] = json!([{
        "authority_id": OWNER,
        "role": "platform-admin",
        "stamped_at": "2026-09-28T10:00:01Z",
        "shape_hash": hash,
        "assurance": "presence",
        "presence_nonce": "n0nce",
    }]);
    p
}

async fn app() -> (TestDb, axum::Router, Arc<MemoryPackets>) {
    let db = TestDb::new().await;
    let packets = Arc::new(MemoryPackets::default());
    packets.put(approved(PACKET));
    let router = webauthn_router(
        db.pool.clone(),
        Arc::new(boss_clock_client::WallClockClient),
    )
    .merge(promotion_router(
        db.pool.clone(),
        packets.clone(),
        Arc::new(TicketKey::Fixed(SESSION_KEY.to_vec())),
    ));
    (db, router, packets)
}

/// An employee holding one user-tier key, written straight to the row so
/// these tests do not depend on the shape of the enrolment body.
async fn seed_key(db: &TestDb, employee: &str) {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role) VALUES ($1, $1, $2, 'platform-admin')",
    )
    .bind(employee)
    .bind(format!("{employee}@test.invalid"))
    .execute(&db.pool)
    .await
    .expect("seed employee");
    register(db, employee, PUB_KEY, "yubikey-5c", REGISTERED_AT).await;
}

/// Register credential `CRED_ID` for `employee` with the given key
/// material, label and enrolment time — what enrolment writes.
async fn register(db: &TestDb, employee: &str, public_key: &str, label: &str, at: &str) {
    sqlx::query(
        "INSERT INTO webauthn_credentials
           (employee_id, credential_id, public_key, label, registered_at)
         VALUES ($1, $2, $3, $4, $5::timestamptz)",
    )
    .bind(employee)
    .bind(base64_decode(CRED_ID))
    .bind(base64_decode(public_key))
    .bind(label)
    .bind(at)
    .execute(&db.pool)
    .await
    .expect("register a user-tier key");
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .expect("fixture is base64url")
}

async fn tier(db: &TestDb) -> String {
    sqlx::query_scalar("SELECT access_tier FROM webauthn_credentials WHERE credential_id = $1")
        .bind(base64_decode(CRED_ID))
        .fetch_one(&db.pool)
        .await
        .expect("the key's row")
}

async fn promoted_events(db: &TestDb) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = 'auth.passkey.promoted' ORDER BY timestamp",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read the outbox")
}

fn promote_path(employee: &str) -> String {
    format!("/api/people/{employee}/webauthn-credentials/{CRED_ID}/promote")
}

/// The gateway's signed word that the ceremony for `packet` finished:
/// the owner's key asserted and the `primary` break-glass key vouched.
/// A fresh nonce each time, as each ceremony mints one.
fn ticket_for(packet: &str) -> String {
    PromotionTicket {
        p: packet.into(),
        i: OWNER.into(),
        c: CRED_ID.into(),
        v: "primary".into(),
        n: Uuid::new_v4().to_string(),
        e: boss_core::presence::now_epoch() + 60,
    }
    .encode(SESSION_KEY)
    .expect("a ticket encodes")
}

fn body_for(packet: &str) -> Value {
    json!({ "packet_id": packet, "ticket": ticket_for(packet) })
}

fn body() -> Value {
    body_for(PACKET)
}

async fn assert_nothing_written(db: &TestDb) {
    assert_eq!(tier(db).await, "user", "a refused promote writes nothing");
    assert!(
        promoted_events(db).await.is_empty(),
        "a refused promote records nothing"
    );
}

/// The owner's own proxied session carries `platform-admin`, and that
/// is exactly the caller that must NOT promote: a copied cookie is the
/// attacker the door exists to refuse. Nor may any other automation.
#[tokio::test]
async fn promote_refuses_every_caller_but_the_gateway() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    for (actor, role) in [
        (OWNER, "platform-admin"),
        ("agent-claude", "platform-admin"),
        ("automation:people", "platform-admin"),
        ("automation:gateway-imposter", "platform-admin"),
        ("anonymous", "guest"),
    ] {
        TestRequest::post(promote_path(OWNER))
            .json(&body())
            .as_user(actor, role)
            .send(&router)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
    assert_nothing_written(&db).await;
}

/// Findings 1-2: a caller that CLAIMS to be the gateway — anyone who can
/// reach the port while the machine gate is off — still promotes nothing
/// without an approved packet for this key. Each case is one thing wrong
/// with the packet; the pure judge's tests pin every check singly.
#[tokio::test]
async fn the_gateway_id_promotes_nothing_without_an_approved_packet_for_this_key() {
    let (db, router, packets) = app().await;
    seed_key(&db, OWNER).await;

    // No such packet at all.
    TestRequest::post(promote_path(OWNER))
        .json(&body_for(OTHER_PACKET))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::CONFLICT);

    let mut cases: Vec<(&str, Value, StatusCode)> = Vec::new();
    let mut p = approved(OTHER_PACKET);
    p["status"] = json!("cancelled");
    cases.push(("not open", p, StatusCode::CONFLICT));
    let mut p = approved(OTHER_PACKET);
    p["steps"][0]["status"] = json!("active");
    cases.push(("not approved yet", p, StatusCode::CONFLICT));
    let mut p = approved(OTHER_PACKET);
    p["steps"][0]["sign_offs"] = json!([]);
    cases.push(("no stamp", p, StatusCode::FORBIDDEN));
    let mut p = approved(OTHER_PACKET);
    p["steps"][0]["metadata"]["credential_id"] = json!("b3RoZXIta2V5");
    cases.push(("another key", p, StatusCode::FORBIDDEN));
    let mut p = approved(OTHER_PACKET);
    p["steps"][1]["status"] = json!("completed");
    cases.push(("spent", p, StatusCode::CONFLICT));

    for (why, p, status) in cases {
        packets.put(p);
        let resp = TestRequest::post(promote_path(OWNER))
            .json(&body_for(OTHER_PACKET))
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await;
        assert_eq!(resp.status, status, "{why}: {}", resp.body_text());
    }
    assert_nothing_written(&db).await;
}

/// The flip and its fact land together, and the fact names the key,
/// the packet that authorised it and the break-glass key that vouched.
#[tokio::test]
async fn the_gateway_promotes_the_key_and_the_fact_rides_with_the_row() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    let resp = TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let out: Value = resp.assert_json();
    assert_eq!(out["promoted"], true);
    assert_eq!(out["access_tier"], "operator");

    assert_eq!(tier(&db).await, "operator");
    let events = promoted_events(&db).await;
    assert_eq!(events.len(), 1, "one promotion, one fact");
    let e = &events[0];
    assert_eq!(e["employee_id"], OWNER);
    assert_eq!(e["credential_label"], "yubikey-5c");
    assert_eq!(e["credential_id"], CRED_ID);
    assert_eq!(e["packet_id"], PACKET);
    assert_eq!(e["vouched_by"], "primary");
    assert!(
        e["registered_at"].is_string(),
        "the fact says when the key was enrolled"
    );
    assert_eq!(e["_actor"], GATEWAY, "the promote is the gateway's act");
    assert!(
        e.get("public_key").is_none(),
        "never key material on the record"
    );
}

/// D7: the gateway promotes, THEN completes its step. A replay after a
/// failed step write re-asks under the same packet, and it is harmless:
/// 200, nothing written, no second fact. Finding 7: the same ask under a
/// DIFFERENT approved packet is refused, naming the packet that promoted
/// the key.
#[tokio::test]
async fn a_replay_is_harmless_only_under_the_packet_that_promoted() {
    let (db, router, packets) = app().await;
    seed_key(&db, OWNER).await;
    packets.put(approved(OTHER_PACKET));

    for _ in 0..2 {
        TestRequest::post(promote_path(OWNER))
            .json(&body())
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await
            .assert_status(StatusCode::OK);
    }
    let resp = TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let out: Value = resp.assert_json();
    assert_eq!(out["promoted"], false, "already operator: nothing to do");

    let resp = TestRequest::post(promote_path(OWNER))
        .json(&body_for(OTHER_PACKET))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await;
    resp.assert_status(StatusCode::CONFLICT);
    assert!(
        resp.body_text().contains(PACKET),
        "the refusal names the packet that promoted the key: {}",
        resp.body_text()
    );
    assert_eq!(promoted_events(&db).await.len(), 1);
}

/// The key is named by its owner AND its id: an approved packet for the
/// owner's key does not reach another employee's row, nor an unknown
/// key. Never a 200 that reads as "promoted".
#[tokio::test]
async fn promote_names_the_owner_and_the_key_or_finds_nothing() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    // The packet names emp-owner, so asking for emp-other is refused.
    TestRequest::post(promote_path("emp-other"))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // The packet's key, on an account that does not hold it: 404.
    sqlx::query("DELETE FROM webauthn_credentials")
        .execute(&db.pool)
        .await
        .expect("clear the key");
    TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(promoted_events(&db).await.is_empty());
}

/// The fact must name its authorisation, and its voucher comes from the
/// ticket, so a promote without them — or one that names a voucher or a
/// tier itself — is refused before anything is read or written.
#[tokio::test]
async fn promote_refuses_a_request_that_names_no_packet_or_no_ticket() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    for bad in [
        json!({ "ticket": ticket_for(PACKET) }),
        json!({ "packet_id": PACKET }),
        json!({ "packet_id": PACKET, "ticket": "  " }),
        json!({ "packet_id": "", "ticket": ticket_for(PACKET) }),
        json!({ "packet_id": "not-a-uuid", "ticket": ticket_for(PACKET) }),
        // The car-1 body: a free-text voucher is refused, never read —
        // who vouched is what the gateway signed.
        json!({ "packet_id": PACKET, "vouched_by": "primary" }),
        json!({ "packet_id": PACKET, "ticket": ticket_for(PACKET), "vouched_by": "backup" }),
        // A body naming a tier is refused, never read or ignored: the
        // promotion writes one tier and takes no say in it.
        json!({ "packet_id": PACKET, "ticket": ticket_for(PACKET), "access_tier": "operator" }),
    ] {
        let status = TestRequest::post(promote_path(OWNER))
            .json(&bad)
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await
            .status;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{bad} answered {status}"
        );
    }
    assert_nothing_written(&db).await;
}

/// THE REVIEW OF CAR 293d5dc1, closed. A caller that claims to be the
/// gateway AND holds a packet the owner approved for this key — what a
/// copied cookie with LAN reach can assemble with a software key it
/// enrolled itself — still promotes nothing without a ticket signed with
/// the session key. Each case is a ticket such a caller could make.
#[tokio::test]
async fn an_approved_packet_promotes_nothing_without_the_gateways_ticket() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    let forged = |key: &[u8], packet: &str, cred: &str, e_offset: i64| {
        PromotionTicket {
            p: packet.into(),
            i: OWNER.into(),
            c: cred.into(),
            v: "primary".into(),
            n: Uuid::new_v4().to_string(),
            e: (boss_core::presence::now_epoch() as i64 + e_offset) as u64,
        }
        .encode(key)
        .unwrap()
    };
    for (why, ticket) in [
        ("free text", "primary".to_string()),
        (
            "signed with a guessed key",
            forged(b"guessed-key-guessed-key-guessed!", PACKET, CRED_ID, 60),
        ),
        ("expired", forged(SESSION_KEY, PACKET, CRED_ID, -1)),
        (
            "for another packet",
            forged(SESSION_KEY, OTHER_PACKET, CRED_ID, 60),
        ),
        (
            "for another key",
            forged(SESSION_KEY, PACKET, "b3RoZXIta2V5", 60),
        ),
    ] {
        let resp = TestRequest::post(promote_path(OWNER))
            .json(&json!({ "packet_id": PACKET, "ticket": ticket }))
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await;
        assert_eq!(
            resp.status,
            StatusCode::FORBIDDEN,
            "{why}: {}",
            resp.body_text()
        );
    }
    assert_nothing_written(&db).await;
}

/// A ticket is spent once: the same ticket presented again — a copy,
/// replayed inside its expiry — is refused, while the gateway's own
/// re-ask (a fresh ceremony, a fresh ticket) stays the harmless 200.
#[tokio::test]
async fn a_ticket_is_spent_once() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    let once = json!({ "packet_id": PACKET, "ticket": ticket_for(PACKET) });
    TestRequest::post(promote_path(OWNER))
        .json(&once)
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    let again = TestRequest::post(promote_path(OWNER))
        .json(&once)
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body_text());
    assert!(
        again.body_text().contains("already spent"),
        "{}",
        again.body_text()
    );
    TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    let spent: i64 = sqlx::query_scalar("SELECT count(*) FROM passkey_promotion_tickets")
        .fetch_one(&db.pool)
        .await
        .expect("count spent tickets");
    assert_eq!(spent, 2, "each accepted ticket is on the ledger once");
    assert_eq!(
        promoted_events(&db).await.len(),
        1,
        "one promotion, one fact"
    );
}

/// The one-writer rule holds in the DATABASE, not only in this crate's
/// source (review F4 of car 1d9970d1): a statement that writes `operator`
/// outside the promotion's transaction is refused by the table itself.
/// What stays allowed: an operator key's sign count moving, and a key
/// going back to `user`.
#[tokio::test]
async fn the_table_refuses_operator_from_any_writer_but_the_promotion() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;

    let planted = sqlx::query(
        "INSERT INTO webauthn_credentials (employee_id, credential_id, public_key, label, access_tier)
         VALUES ('emp-owner', '\\x706c616e746564', '\\x6b6579', 'planted', 'operator')",
    )
    .execute(&db.pool)
    .await;
    assert!(planted.is_err(), "an INSERT of an operator key is refused");
    let flipped = sqlx::query(
        "UPDATE webauthn_credentials SET access_tier = 'operator' WHERE credential_id = $1",
    )
    .bind(base64_decode(CRED_ID))
    .execute(&db.pool)
    .await;
    assert!(flipped.is_err(), "a hand UPDATE to operator is refused");
    assert_eq!(tier(&db).await, "user");

    // Through the door it lands, and the promoted key keeps working.
    TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
    TestRequest::post("/api/people/webauthn-credentials/used")
        .json(&json!({"credential_id": CRED_ID, "sign_count": 3}))
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(tier(&db).await, "operator");

    // Back to user is not a grant, and needs no door.
    sqlx::query("UPDATE webauthn_credentials SET access_tier = 'user' WHERE credential_id = $1")
        .bind(base64_decode(CRED_ID))
        .execute(&db.pool)
        .await
        .expect("a key may always drop to user");
}

/// Finding 3: an operator row REKEYED — its public key, its credential
/// id, or its owner swapped for an attacker's, with the tier untouched —
/// is a planted elevating key just the same, and the table refuses it.
#[tokio::test]
async fn the_table_refuses_a_rekey_of_an_operator_key() {
    let (db, router, _) = app().await;
    seed_key(&db, OWNER).await;
    sqlx::query(
        "INSERT INTO employees (id, name, email, role)
         VALUES ('emp-mallory', 'emp-mallory', 'mallory@test.invalid', 'platform-admin')",
    )
    .execute(&db.pool)
    .await
    .expect("seed a second employee");
    TestRequest::post(promote_path(OWNER))
        .json(&body())
        .as_user(GATEWAY, "platform-admin")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);

    for (what, sql) in [
        (
            "public key",
            "UPDATE webauthn_credentials SET public_key = '\\x61747461636b6572' WHERE access_tier = 'operator'",
        ),
        (
            "credential id",
            "UPDATE webauthn_credentials SET credential_id = '\\x61747461636b6572' WHERE access_tier = 'operator'",
        ),
        (
            "owner",
            "UPDATE webauthn_credentials SET employee_id = 'emp-mallory' WHERE access_tier = 'operator'",
        ),
        (
            "public key and credential id",
            "UPDATE webauthn_credentials SET public_key = '\\x61', credential_id = '\\x62' WHERE access_tier = 'operator'",
        ),
    ] {
        let rekeyed = sqlx::query(sql).execute(&db.pool).await;
        assert!(
            rekeyed.is_err(),
            "a rekey of an operator row's {what} is refused"
        );
    }
    let (key, owner): (Vec<u8>, String) = sqlx::query_as(
        "SELECT public_key, employee_id FROM webauthn_credentials WHERE access_tier = 'operator'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("the operator key");
    assert_eq!(key, base64_decode(PUB_KEY));
    assert_eq!(owner, OWNER);
}

/// KEY SUBSTITUTION (review of car 293d5dc1). While an approved packet
/// for credential C waits unspent, a platform-admin deletes row C and
/// registers C again with other key material. The door matches rows by
/// credential id, so without the comparison the owner's approval would
/// promote the attacker's key. Each case re-registers C differently;
/// every one is refused and writes nothing.
#[tokio::test]
async fn a_key_re_registered_under_the_approved_credential_id_promotes_nothing() {
    for (what, public_key, label, at) in [
        (
            "other key material, all else equal",
            OTHER_PUB_KEY,
            "yubikey-5c",
            REGISTERED_AT,
        ),
        (
            "the same key material, enrolled again later",
            PUB_KEY,
            "yubikey-5c",
            "2026-09-28T11:00:00Z",
        ),
        (
            "the same key material, relabelled",
            PUB_KEY,
            "yubikey-5c-new",
            REGISTERED_AT,
        ),
    ] {
        let (db, router, _) = app().await;
        seed_key(&db, OWNER).await;
        sqlx::query("DELETE FROM webauthn_credentials")
            .execute(&db.pool)
            .await
            .expect("delete row C");
        register(&db, OWNER, public_key, label, at).await;

        let resp = TestRequest::post(promote_path(OWNER))
            .json(&body())
            .as_user(GATEWAY, "platform-admin")
            .send(&router)
            .await;
        assert_eq!(
            resp.status,
            StatusCode::CONFLICT,
            "{what}: {}",
            resp.body_text()
        );
        assert!(
            resp.body_text().contains("is not the key packet"),
            "{what}: the refusal says why: {}",
            resp.body_text()
        );
        assert_nothing_written(&db).await;
    }
}
