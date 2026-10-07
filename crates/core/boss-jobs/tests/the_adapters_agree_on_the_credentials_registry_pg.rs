//! The credentials registry answers the same on both
//! `CredentialsRegistry` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), on the
//! sixth port it reaches (backlog be459ab9; after the jobs store's list
//! filters, the agent-run log, the Workflow registry, the Class registry
//! and the policy store).
//!
//! WHY THIS PORT NEXT. It is the trust boundary: the registry says where
//! every credential lives, who reads it and when it last changed, and
//! the rotation path's four phases are recorded through it. Every door
//! test of `/api/credentials` asks `InMemoryCredentials`; production is
//! answered by `PgCredentials`. `credentials_pg.rs` and the in-memory
//! module's own tests each state the contract in their own words; this
//! file is the one statement both are held to.
//!
//! The first run found two disagreements, each fixed in this car:
//! - ORDER. `list` promises rows "ordered by id". Postgres sorted by the
//!   database's locale, which ignores `-` at first level, so `suite-ab`
//!   listed before `suite-a-z` there and after it in memory. The Pg
//!   adapter now sorts `COLLATE "C"` — byte order, the order the double
//!   holds and the only one both can: the double cannot reproduce a
//!   server's locale, and a listing whose order depends on the server it
//!   ran against answers two questions. The Workflow registry's suite
//!   found and fixed the same defect the same way. Case
//!   `a_declaration_lands_whole_and_lists_in_byte_order_of_id`.
//! - REFUSAL. The schema admits `rotation_policy` in on-demand |
//!   scheduled and nothing else, checked before the conflict (so a held
//!   id is refused too), and the batch is one transaction. The in-memory
//!   adapter took any spelling, so a batch Postgres refused whole landed
//!   there. It now refuses the batch before writing anything, naming the
//!   row, against `ROTATION_POLICIES` — the one list `validate_credential`
//!   reads too. Case `a_rotation_policy_the_schema_refuses_lands_nothing`,
//!   whose Pg leg is also what holds that list equal to the SQL CHECK.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one;
//! every write is also judged by the FACTS it leaves — the
//! `credential.*` events the in-memory adapter collects and Postgres
//! stages on `event_outbox` in the row's own transaction — because a
//! registry row and its fact are one record and the port promises both.
//! Stamps carry whole-second instants, so Postgres's microseconds and the
//! double's nanoseconds compare equal. A fresh Postgres holds no
//! credential row (backlog ee368d0c), so both adapters start empty.
//!
//! No credential VALUE appears anywhere in this file: the registry holds
//! locations, and every fixture is a location.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_jobs::credentials::types::{
    CREDENTIAL_DECLARED, Consumer, CredentialInput, CredentialRow, ROTATION_POLICIES, RotationPhase,
};
use boss_jobs::credentials::{
    CredentialsError, CredentialsRegistry, InMemoryCredentials, PgCredentials,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: CredentialsRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `credential.*` fact recorded so far, in the order recorded:
    /// `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
}

struct InMemory(InMemoryCredentials);

impl World for InMemory {
    type R = InMemoryCredentials;
    fn repo(&self) -> &InMemoryCredentials {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind.starts_with("credential."))
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgCredentials,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgCredentials;
    fn repo(&self) -> &PgCredentials {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'credential.%' ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .expect("read the outbox")
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryCredentials::default()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgCredentials::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_fresh_registry_lists_nothing_and_answers_none,
        a_declaration_lands_whole_and_lists_in_byte_order_of_id,
        a_held_row_is_kept_as_the_rotation_left_it,
        a_batch_naming_one_id_twice_inserts_it_once,
        only_the_install_phase_stamps_rotated_at,
        a_rotation_of_an_unknown_credential_records_nothing,
        a_rotation_policy_the_schema_refuses_lands_nothing,
        an_original_observation_replays_exactly_and_conflicts_leave_no_new_fact,
    }
}

async fn an_original_observation_replays_exactly_and_conflicts_leave_no_new_fact<W: World>(
    w: &W,
    adapter: &str,
) {
    use boss_jobs::credentials::receipt::RotationOutcome;
    w.repo()
        .publish("suite", &[declared("suite-replay")], &stamp(0))
        .await
        .unwrap();
    let command = json!({"observation_id":"original-attempt","measured":9.0});
    let recorded = w
        .repo()
        .record_rotation(
            "suite-replay",
            RotationPhase::Installed,
            command.clone(),
            &stamp(1),
        )
        .await
        .unwrap();
    let RotationOutcome::Recorded { receipt: original } = recorded else {
        panic!("{adapter}: fresh observation is recorded");
    };
    let facts = w.facts().await;
    let replay = w
        .repo()
        .record_rotation(
            "suite-replay",
            RotationPhase::Installed,
            command.clone(),
            &stamp(2),
        )
        .await
        .unwrap();
    assert!(
        matches!(replay,RotationOutcome::Replayed { receipt } if receipt==original),
        "{adapter}: original actor/time/event/full bytes retained"
    );
    assert_eq!(w.facts().await, facts, "{adapter}: no second event");
    let changed = json!({"observation_id":"original-attempt","measured":9});
    assert!(
        matches!(
            w.repo()
                .record_rotation("suite-replay", RotationPhase::Installed, changed, &stamp(3))
                .await,
            Err(CredentialsError::ObservationConflict)
        ),
        "{adapter}: even changed scalar spelling conflicts"
    );
    let foreign = EventStamp::new("jobs", ActorId::automation("foreign")).with_timestamp(at(4));
    assert!(
        matches!(
            w.repo()
                .record_rotation("suite-replay", RotationPhase::Installed, command, &foreign)
                .await,
            Err(CredentialsError::ObservationConflict)
        ),
        "{adapter}: actor cannot retrofit provenance"
    );
    for invalid in [
        Value::Null,
        json!(""),
        json!("x".repeat(129)),
        json!(42),
        json!("a b"),
    ] {
        assert!(
            w.repo()
                .record_rotation(
                    "suite-replay",
                    RotationPhase::Installed,
                    json!({"observation_id":invalid}),
                    &stamp(5)
                )
                .await
                .is_err(),
            "{adapter}: malformed control before mutation"
        );
    }
    assert_eq!(
        w.repo()
            .get("suite-replay")
            .await
            .unwrap()
            .unwrap()
            .rotated_at,
        Some(at(1))
    );
    assert_eq!(w.facts().await, facts);
    for _ in 0..2 {
        assert!(matches!(
            w.repo()
                .record_rotation("suite-replay", RotationPhase::Minted, json!({}), &stamp(6))
                .await
                .unwrap(),
            RotationOutcome::LegacyRecorded
        ));
    }
    assert_eq!(
        w.facts().await.len(),
        facts.len() + 2,
        "{adapter}: legacy calls deliberately append"
    );
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new(
        "jobs",
        ActorId::Automation("rule:adapters-agree-suite".into()),
    )
    .with_timestamp(at(secs))
}

/// A declaration of `id`: every field a location or a name, never a value.
fn declared(id: &str) -> CredentialInput {
    CredentialInput {
        id: id.into(),
        kind: "forgejo-access-token".into(),
        issuer: "the forge".into(),
        principal: format!("the principal of {id}"),
        scopes: vec!["write:repository".into(), "read:user".into()],
        storage_location: format!("k8s Secret suite/{id} key token"),
        consumers: vec![Consumer {
            kind: "secret-mount".into(),
            location: format!("/etc/suite/{id}.token"),
        }],
        rotation_policy: "on-demand".into(),
        notes: format!("{id} as first declared"),
    }
}

/// The row a declaration of `id` lands as: the declaration, the two
/// lists as JSON arrays, and no rotation instant.
fn row_of(c: &CredentialInput) -> CredentialRow {
    CredentialRow {
        id: c.id.clone(),
        kind: c.kind.clone(),
        issuer: c.issuer.clone(),
        principal: c.principal.clone(),
        scopes: json!(c.scopes),
        storage_location: c.storage_location.clone(),
        consumers: serde_json::to_value(&c.consumers).expect("consumers serialise"),
        rotation_policy: c.rotation_policy.clone(),
        rotated_at: None,
        notes: c.notes.clone(),
    }
}

async fn ids<W: World>(w: &W) -> Vec<String> {
    w.repo()
        .list()
        .await
        .expect("list answers")
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// `(kind, the id it annotates)` for each fact — the declared fact
/// carries the row's `id`; a rotation fact carries the caller's
/// evidence, which every case here keys by `credential_id`.
async fn fact_ids<W: World>(w: &W) -> Vec<(String, String)> {
    w.facts()
        .await
        .into_iter()
        .map(|(kind, p)| {
            let id = p
                .get("id")
                .or_else(|| p.get("credential_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (kind, id)
        })
        .collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

// ----- cases -----------------------------------------------------------------

/// Nothing declared: an empty listing, `None` for any id (an answer, not
/// an error — the door owns the 404), and no fact.
async fn a_fresh_registry_lists_nothing_and_answers_none<W: World>(w: &W, adapter: &str) {
    assert!(ids(w).await.is_empty(), "{adapter}");
    assert_eq!(
        w.repo().get("suite-nothing").await.expect("get answers"),
        None,
        "{adapter}"
    );
    assert!(w.facts().await.is_empty(), "{adapter}");
}

/// A declaration lands every field as declared, reads back the same
/// through `get` and `list`, lists in BYTE order of id (`-` sorts before
/// a letter), and leaves one `credential.declared` per row — carrying the
/// declarer and the tenant, never a value.
async fn a_declaration_lands_whole_and_lists_in_byte_order_of_id<W: World>(w: &W, adapter: &str) {
    let batch = [
        declared("suite-b"),
        declared("suite-ab"),
        declared("suite-a-z"),
    ];
    let out = w
        .repo()
        .publish("acme", &batch, &stamp(0))
        .await
        .expect("a declaration lands");
    assert_eq!((out.received, out.inserted), (3, 3), "{adapter}");

    assert_eq!(
        ids(w).await,
        vec!["suite-a-z", "suite-ab", "suite-b"],
        "{adapter}: ordered by id, byte for byte"
    );
    for c in &batch {
        assert_eq!(
            w.repo().get(&c.id).await.expect("get answers"),
            Some(row_of(c)),
            "{adapter}: {} reads back as declared",
            c.id
        );
    }
    let listed = w.repo().list().await.expect("list answers");
    assert!(
        listed.iter().all(|r| batch.iter().any(|c| row_of(c) == *r)),
        "{adapter}: the listing carries the same rows as get: {listed:?}"
    );

    let facts = w.facts().await;
    assert_eq!(
        fact_ids(w).await,
        pairs(&[
            (CREDENTIAL_DECLARED, "suite-b"),
            (CREDENTIAL_DECLARED, "suite-ab"),
            (CREDENTIAL_DECLARED, "suite-a-z"),
        ]),
        "{adapter}: one fact per inserted row, in the batch's order"
    );
    for (_, p) in &facts {
        assert_eq!(p["tenant_id"], "acme", "{adapter}");
        assert_eq!(
            p["declared_by"], "automation:rule:adapters-agree-suite",
            "{adapter}"
        );
        assert_eq!(p["declared_by"], p["_actor"], "{adapter}: one actor");
        assert!(p.get("value").is_none(), "{adapter}: never a value");
    }
}

/// A second declaration of a held id inserts nothing and records
/// nothing, and the held row keeps what the rotation path wrote — its
/// `rotated_at` and its notes — because a declaration is knowledge about
/// the credential, not its history. A new id in the same batch lands.
async fn a_held_row_is_kept_as_the_rotation_left_it<W: World>(w: &W, adapter: &str) {
    w.repo()
        .publish("acme", &[declared("suite-held")], &stamp(0))
        .await
        .expect("the first declaration lands");
    w.repo()
        .record_rotation(
            "suite-held",
            RotationPhase::Installed,
            json!({ "credential_id": "suite-held", "value_length": 40 }),
            &stamp(60),
        )
        .await
        .expect("the install records");

    let mut again = declared("suite-held");
    again.notes = "a later declaration with different prose".into();
    again.scopes = vec!["admin".into()];
    let out = w
        .repo()
        .publish("acme", &[again, declared("suite-new")], &stamp(120))
        .await
        .expect("the second declaration lands");
    assert_eq!((out.received, out.inserted), (2, 1), "{adapter}");

    let held = w.repo().get("suite-held").await.expect("get answers");
    let mut expected = row_of(&declared("suite-held"));
    expected.rotated_at = Some(at(60));
    assert_eq!(held, Some(expected), "{adapter}: the held row is untouched");

    let out = w
        .repo()
        .publish("acme", &[declared("suite-new")], &stamp(180))
        .await
        .expect("a republish answers");
    assert_eq!((out.received, out.inserted), (1, 0), "{adapter}");
    assert_eq!(
        fact_ids(w).await,
        pairs(&[
            (CREDENTIAL_DECLARED, "suite-held"),
            ("credential.installed", "suite-held"),
            (CREDENTIAL_DECLARED, "suite-new"),
        ]),
        "{adapter}: no fact for a kept row"
    );
}

/// One batch naming an id twice: the first declaration lands, the second
/// is a kept row like any other.
async fn a_batch_naming_one_id_twice_inserts_it_once<W: World>(w: &W, adapter: &str) {
    let first = declared("suite-twice");
    let mut second = declared("suite-twice");
    second.notes = "the second spelling in one batch".into();
    let out = w
        .repo()
        .publish("acme", &[first.clone(), second], &stamp(0))
        .await
        .expect("the batch lands");
    assert_eq!((out.received, out.inserted), (2, 1), "{adapter}");
    assert_eq!(
        w.repo().get("suite-twice").await.expect("get answers"),
        Some(row_of(&first)),
        "{adapter}: the first spelling is the row"
    );
    assert_eq!(
        fact_ids(w).await,
        pairs(&[(CREDENTIAL_DECLARED, "suite-twice")]),
        "{adapter}"
    );
}

/// Every phase records its own event kind carrying the caller's evidence
/// and the stamp's actor, and only the install moves `rotated_at` — to
/// the install's own instant, the one its event carries.
async fn only_the_install_phase_stamps_rotated_at<W: World>(w: &W, adapter: &str) {
    w.repo()
        .publish("acme", &[declared("suite-rotated")], &stamp(0))
        .await
        .expect("the declaration lands");
    let mut want_rotated_at = None;
    for (n, phase) in RotationPhase::ALL.into_iter().enumerate() {
        let secs = 60 * (n as i64 + 1);
        w.repo()
            .record_rotation(
                "suite-rotated",
                phase,
                json!({ "credential_id": "suite-rotated", "phase_no": n }),
                &stamp(secs),
            )
            .await
            .expect("the phase records");
        if phase == RotationPhase::Installed {
            want_rotated_at = Some(at(secs));
        }
        let row = w
            .repo()
            .get("suite-rotated")
            .await
            .expect("get answers")
            .expect("the row is held");
        assert_eq!(
            row.rotated_at,
            want_rotated_at,
            "{adapter}: after {}",
            phase.as_str()
        );
    }

    let facts = w.facts().await;
    let kinds: Vec<&str> = facts.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            CREDENTIAL_DECLARED,
            "credential.minted",
            "credential.installed",
            "credential.verified",
            "credential.revoked",
        ],
        "{adapter}"
    );
    for (n, (kind, p)) in facts.iter().skip(1).enumerate() {
        assert_eq!(p["phase_no"], n, "{adapter}: {kind} carries its evidence");
        assert_eq!(
            p["_actor"], "automation:rule:adapters-agree-suite",
            "{adapter}: {kind}"
        );
    }
}

/// A rotation naming an id the registry does not hold is refused as
/// `UnknownCredential` naming it, in every phase, and leaves no fact —
/// no event may detach from the row it annotates.
async fn a_rotation_of_an_unknown_credential_records_nothing<W: World>(w: &W, adapter: &str) {
    w.repo()
        .publish("acme", &[declared("suite-known")], &stamp(0))
        .await
        .expect("the declaration lands");
    for phase in RotationPhase::ALL {
        let err = w
            .repo()
            .record_rotation(
                "suite-ghost",
                phase,
                json!({ "credential_id": "suite-ghost" }),
                &stamp(60),
            )
            .await
            .expect_err("an unknown credential is refused");
        assert!(
            matches!(&err, CredentialsError::UnknownCredential(id) if id == "suite-ghost"),
            "{adapter}: {}: {err:?}",
            phase.as_str()
        );
    }
    assert_eq!(ids(w).await, vec!["suite-known"], "{adapter}");
    assert_eq!(
        fact_ids(w).await,
        pairs(&[(CREDENTIAL_DECLARED, "suite-known")]),
        "{adapter}"
    );
}

/// Every rotation policy the registry names is admitted; any other
/// spelling refuses the WHOLE batch — the rows before it too, and a held
/// id as well as a new one (the schema's CHECK runs before the conflict
/// is looked at) — and leaves no fact.
async fn a_rotation_policy_the_schema_refuses_lands_nothing<W: World>(w: &W, adapter: &str) {
    let admitted: Vec<CredentialInput> = ROTATION_POLICIES
        .iter()
        .map(|policy| {
            let mut c = declared(&format!("suite-{policy}"));
            c.rotation_policy = (*policy).to_string();
            c
        })
        .collect();
    let out = w
        .repo()
        .publish("acme", &admitted, &stamp(0))
        .await
        .expect("every named policy is admitted");
    assert_eq!(
        (out.received, out.inserted),
        (admitted.len(), admitted.len()),
        "{adapter}"
    );
    let before = ids(w).await;
    let facts_before = fact_ids(w).await;

    let refused_new = {
        let mut c = declared("suite-refused");
        c.rotation_policy = "whenever".into();
        c
    };
    let refused_held = {
        let mut c = admitted[0].clone();
        c.rotation_policy = "whenever".into();
        c
    };
    for bad in [refused_new, refused_held] {
        let err = w
            .repo()
            .publish(
                "acme",
                &[declared("suite-alongside"), bad.clone()],
                &stamp(60),
            )
            .await
            .expect_err("a policy the schema refuses refuses the batch");
        assert!(
            matches!(err, CredentialsError::Storage(_)),
            "{adapter}: {}: {err:?}",
            bad.id
        );
        assert_eq!(ids(w).await, before, "{adapter}: {} landed nothing", bad.id);
        assert_eq!(
            fact_ids(w).await,
            facts_before,
            "{adapter}: {} left no fact",
            bad.id
        );
    }
}
