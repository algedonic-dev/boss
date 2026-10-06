//! The sensor registry answers the same on both `Sensors` adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next port in the census
//! order after the dispatcher's firing record (backlog be459ab9):
//! `Sensors`, which `InMemorySensors` and `PgSensors` implement.
//!
//! WHY THIS PORT. It is what the five-minute platform cadence polls
//! from, what a Stripe charge or a page view is recorded into, and what
//! a retro reads a sensor's window from. Every door test of those
//! surfaces (`sensors/http.rs`, the poller's tests) asks
//! `InMemorySensors`; production is answered by `PgSensors`. The two
//! stated their contract each in their own words — the double's own
//! module tests and the Postgres adapter's SQL — and nothing held one to
//! the other. This file is that statement, for every method of the port.
//!
//! The first run found the collation class the earlier suites found
//! (backlog 2987fb2d names `sensors (id, external_id)` among the tables
//! still disagreeing), fixed in this car in production code: three reads
//! promise an order over a text column and Postgres sorted each by the
//! database's locale, which ignores `-` at first level and folds case —
//! so `suite-ab` served before `suite-a-z` there and after it in memory.
//! `list` (by id), `unstamped` (a tie on `observed_at` broken by the
//! external id) and `window`'s `packets` (sorted and distinct) now sort
//! `COLLATE "C"`, byte order, the double's `Vec::sort` / `BTreeSet`
//! order. Cases `the_registry_is_listed_in_byte_order`,
//! `an_instant_tie_among_owed_readings_is_broken_in_byte_order` and
//! `a_windows_packets_are_distinct_and_in_byte_order`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal. `published_at` is each adapter's
//! own clock (a column default, a wall read) and is the one field no
//! case compares. Every sensor this file declares starts `suite-`; the
//! migrations seed no sensor, and both adapters start empty.

use boss_jobs::sensors::{
    BatchOutcome, InMemorySensors, NewReading, PgSensors, PollStamp, Reading, ReadingsWindow,
    SensorInput, SensorRow, Sensors, SensorsError,
};
use chrono::{DateTime, TimeZone, Utc};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemorySensors::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgSensors::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_registry_answers_nothing_and_no_error,
        a_publish_is_insert_if_absent_and_keeps_the_held_row,
        the_registry_is_listed_in_byte_order,
        a_reading_is_insert_if_absent_per_sensor,
        a_reading_against_no_sensor_is_refused,
        owed_readings_are_oldest_first,
        an_instant_tie_among_owed_readings_is_broken_in_byte_order,
        the_first_stamp_wins_and_an_unknown_reading_is_refused,
        a_window_is_half_open_and_counts_what_arrived_and_what_was_stamped,
        a_windows_packets_are_distinct_and_in_byte_order,
        a_window_on_no_sensor_is_refused_not_zero,
        a_poll_stamp_moves_the_cursor_only_forward,
        a_poll_stamp_on_no_sensor_is_refused,
        the_sweep_keeps_what_is_owed_and_ages_out_the_rest,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
}

/// A polled sensor, valid by `validate_sensor`.
fn polled(id: &str) -> SensorInput {
    SensorInput {
        id: id.into(),
        source: "stripe".into(),
        credential: "stripe-restricted-read".into(),
        every_minutes: 15,
        opens: "receive-a-sponsorship".into(),
        subject_kind: "custom".into(),
        enabled: true,
        selector: None,
    }
}

/// A push-only sensor (`PUSH_ONLY_SOURCES`): no credential, no period.
fn push_only(id: &str) -> SensorInput {
    SensorInput {
        source: "site".into(),
        credential: String::new(),
        every_minutes: 0,
        ..polled(id)
    }
}

fn reading(external_id: &str, observed_at: DateTime<Utc>) -> NewReading {
    NewReading {
        external_id: external_id.into(),
        observed_at,
        payload: serde_json::json!({ "id": external_id }),
    }
}

/// A row with the one field each adapter stamps from its own clock
/// fixed, so the rest compares whole.
fn unclocked(mut row: SensorRow) -> SensorRow {
    row.published_at = at(0, 0);
    row
}

async fn list<S: Sensors>(s: &S) -> Vec<SensorRow> {
    s.list()
        .await
        .expect("list answers")
        .into_iter()
        .map(unclocked)
        .collect()
}

async fn owed_ids<S: Sensors>(s: &S, sensor: &str) -> Vec<String> {
    s.unstamped(sensor)
        .await
        .expect("unstamped answers")
        .into_iter()
        .map(|r| r.external_id)
        .collect()
}

async fn declare<S: Sensors>(s: &S, sensors: &[SensorInput]) {
    s.publish("acme", sensors).await.expect("publish answers");
}

// ----- cases ---------------------------------------------------------------

/// Nothing declared: the list is empty and a sensor that does not exist
/// owes nothing — each an answer, never an error.
async fn an_empty_registry_answers_nothing_and_no_error<S: Sensors>(s: &S, adapter: &str) {
    assert!(list(s).await.is_empty(), "{adapter}");
    assert!(owed_ids(s, "suite-never").await.is_empty(), "{adapter}");
}

/// A publish inserts what is absent and nothing else: a second publish
/// naming a held id, even with a different declaration, keeps the held
/// row whole — its source, its tenant and the cursor its poller moved.
async fn a_publish_is_insert_if_absent_and_keeps_the_held_row<S: Sensors>(s: &S, adapter: &str) {
    let first = s
        .publish("acme", &[polled("suite-a")])
        .await
        .expect("first publish");
    assert_eq!(
        first,
        BatchOutcome {
            received: 1,
            inserted: 1
        },
        "{adapter}"
    );
    s.mark_polled(
        "suite-a",
        &PollStamp {
            polled_at: at(10, 0),
            cursor_at: Some(at(9, 0)),
        },
    )
    .await
    .expect("mark_polled");
    let again = s
        .publish(
            "other",
            &[
                SensorInput {
                    every_minutes: 60,
                    enabled: false,
                    ..polled("suite-a")
                },
                push_only("suite-b"),
            ],
        )
        .await
        .expect("second publish");
    assert_eq!(
        again,
        BatchOutcome {
            received: 2,
            inserted: 1
        },
        "{adapter}"
    );
    assert_eq!(
        list(s).await,
        vec![
            SensorRow {
                id: "suite-a".into(),
                source: "stripe".into(),
                credential: "stripe-restricted-read".into(),
                every_minutes: 15,
                opens_kind: "receive-a-sponsorship".into(),
                subject_kind: "custom".into(),
                enabled: true,
                selector: None,
                tenant_id: "acme".into(),
                published_at: at(0, 0),
                last_polled_at: Some(at(10, 0)),
                cursor_at: Some(at(9, 0)),
            },
            SensorRow {
                id: "suite-b".into(),
                source: "site".into(),
                credential: String::new(),
                every_minutes: 0,
                opens_kind: "receive-a-sponsorship".into(),
                subject_kind: "custom".into(),
                enabled: true,
                selector: None,
                tenant_id: "other".into(),
                published_at: at(0, 0),
                last_polled_at: None,
                cursor_at: None,
            },
        ],
        "{adapter}"
    );
}

/// "Ordered by id" is BYTE order (`-` sorts before a letter) — the order
/// the double sorts by and the only one both adapters can hold.
async fn the_registry_is_listed_in_byte_order<S: Sensors>(s: &S, adapter: &str) {
    declare(
        s,
        &[polled("suite-b"), polled("suite-ab"), polled("suite-a-z")],
    )
    .await;
    let ids: Vec<String> = list(s).await.into_iter().map(|r| r.id).collect();
    assert_eq!(
        ids,
        vec!["suite-a-z", "suite-ab", "suite-b"],
        "{adapter}: every id byte for byte"
    );
}

/// A reading is new once per `(sensor, external id)`: a redelivered one
/// is counted received and not inserted, and keeps the first payload;
/// the same external id under another sensor is a different reading.
async fn a_reading_is_insert_if_absent_per_sensor<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a"), polled("suite-b")]).await;
    let first = s
        .record(
            "suite-a",
            &[reading("ch_1", at(9, 0)), reading("ch_2", at(9, 1))],
        )
        .await
        .expect("record");
    assert_eq!((first.received, first.inserted), (2, 2), "{adapter}");
    let again = s
        .record(
            "suite-a",
            &[
                NewReading {
                    payload: serde_json::json!({ "id": "changed" }),
                    ..reading("ch_1", at(9, 5))
                },
                reading("ch_3", at(9, 2)),
            ],
        )
        .await
        .expect("record again");
    assert_eq!((again.received, again.inserted), (2, 1), "{adapter}");
    let other = s
        .record("suite-b", &[reading("ch_1", at(9, 0))])
        .await
        .expect("record under another sensor");
    assert_eq!(other.inserted, 1, "{adapter}: another sensor's ch_1");
    assert_eq!(
        s.unstamped("suite-a").await.expect("unstamped"),
        vec![
            Reading {
                sensor_id: "suite-a".into(),
                external_id: "ch_1".into(),
                observed_at: at(9, 0),
                payload: serde_json::json!({ "id": "ch_1" }),
                packet_id: None,
            },
            Reading {
                sensor_id: "suite-a".into(),
                external_id: "ch_2".into(),
                observed_at: at(9, 1),
                payload: serde_json::json!({ "id": "ch_2" }),
                packet_id: None,
            },
            Reading {
                sensor_id: "suite-a".into(),
                external_id: "ch_3".into(),
                observed_at: at(9, 2),
                payload: serde_json::json!({ "id": "ch_3" }),
                packet_id: None,
            },
        ],
        "{adapter}: the first delivery's instant and payload stand"
    );
}

/// A reading against a sensor the registry does not hold is
/// `UnknownSensor` — loud, never a silent insert of nothing.
async fn a_reading_against_no_sensor_is_refused<S: Sensors>(s: &S, adapter: &str) {
    let err = s
        .record("suite-none", &[reading("ch_1", at(9, 0))])
        .await
        .expect_err("a reading against nothing");
    assert!(
        matches!(err, SensorsError::UnknownSensor(ref id) if id == "suite-none"),
        "{adapter}: {err}"
    );
}

/// The owed readings are oldest OBSERVATION first, whatever order they
/// were recorded in, and a stamped reading owes nothing.
async fn owed_readings_are_oldest_first<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a")]).await;
    s.record(
        "suite-a",
        &[
            reading("late", at(11, 0)),
            reading("early", at(9, 0)),
            reading("done", at(8, 0)),
            reading("mid", at(10, 0)),
        ],
    )
    .await
    .expect("record");
    s.stamp("suite-a", "done", "pkt-1").await.expect("stamp");
    assert_eq!(
        owed_ids(s, "suite-a").await,
        vec!["early", "mid", "late"],
        "{adapter}"
    );
}

/// Two owed readings observed at ONE instant are ordered by external id
/// in BYTE order — the poller opens packets in this order, so a locale
/// order would open them differently on the two adapters.
async fn an_instant_tie_among_owed_readings_is_broken_in_byte_order<S: Sensors>(
    s: &S,
    adapter: &str,
) {
    declare(s, &[polled("suite-a")]).await;
    s.record(
        "suite-a",
        &[
            reading("ch-b", at(9, 0)),
            reading("ch-ab", at(9, 0)),
            reading("ch-a-z", at(9, 0)),
            reading("ch-B", at(9, 0)),
        ],
    )
    .await
    .expect("record");
    // Byte order: 'B' (0x42) < 'a' (0x61); '-' (0x2d) < 'b'.
    assert_eq!(
        owed_ids(s, "suite-a").await,
        vec!["ch-B", "ch-a-z", "ch-ab", "ch-b"],
        "{adapter}: every external id byte for byte"
    );
}

/// A reading has one packet: the first stamp wins and a second is a
/// no-op, not an error. Stamping a reading that is not held is
/// `UnknownSensor`, naming the sensor and the external id.
async fn the_first_stamp_wins_and_an_unknown_reading_is_refused<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a")]).await;
    s.record("suite-a", &[reading("ch_1", at(9, 0))])
        .await
        .expect("record");
    s.stamp("suite-a", "ch_1", "pkt-first")
        .await
        .expect("stamp");
    s.stamp("suite-a", "ch_1", "pkt-second")
        .await
        .expect("a second stamp is a no-op");
    let w = s
        .window("suite-a", at(0, 0), at(23, 0))
        .await
        .expect("window");
    assert_eq!(w.packets, vec!["pkt-first"], "{adapter}");
    let err = s
        .stamp("suite-a", "ch_404", "pkt-x")
        .await
        .expect_err("an unknown reading");
    assert!(
        matches!(err, SensorsError::UnknownSensor(ref id) if id == "suite-a/ch_404"),
        "{adapter}: {err}"
    );
}

/// The window is `[since, until)` over `observed_at` — the start
/// instant in, the end instant out — counts only its own sensor, and
/// splits what arrived into stamped and unstamped.
async fn a_window_is_half_open_and_counts_what_arrived_and_what_was_stamped<S: Sensors>(
    s: &S,
    adapter: &str,
) {
    declare(s, &[polled("suite-a"), polled("suite-b")]).await;
    s.record(
        "suite-a",
        &[
            reading("before", at(7, 59)),
            reading("start", at(8, 0)),
            reading("inside", at(9, 0)),
            reading("owed", at(9, 30)),
            reading("end", at(10, 0)),
        ],
    )
    .await
    .expect("record");
    s.record("suite-b", &[reading("elsewhere", at(9, 0))])
        .await
        .expect("record b");
    for (ext, pkt) in [("start", "pkt-1"), ("inside", "pkt-2"), ("end", "pkt-3")] {
        s.stamp("suite-a", ext, pkt).await.expect("stamp");
    }
    assert_eq!(
        s.window("suite-a", at(8, 0), at(10, 0))
            .await
            .expect("window"),
        ReadingsWindow {
            sensor_id: "suite-a".into(),
            since: at(8, 0),
            until: at(10, 0),
            arrived: 3,
            stamped: 2,
            unstamped: 1,
            packets: vec!["pkt-1".into(), "pkt-2".into()],
        },
        "{adapter}"
    );
}

/// A window's packets are sorted and DISTINCT, in byte order: two
/// readings stamped with one packet name it once.
async fn a_windows_packets_are_distinct_and_in_byte_order<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a")]).await;
    s.record(
        "suite-a",
        &[
            reading("r1", at(9, 0)),
            reading("r2", at(9, 1)),
            reading("r3", at(9, 2)),
            reading("r4", at(9, 3)),
            reading("r5", at(9, 4)),
        ],
    )
    .await
    .expect("record");
    for (ext, pkt) in [
        ("r1", "pkt-b"),
        ("r2", "pkt-ab"),
        ("r3", "pkt-a-z"),
        ("r4", "pkt-B"),
        ("r5", "pkt-ab"),
    ] {
        s.stamp("suite-a", ext, pkt).await.expect("stamp");
    }
    let w = s
        .window("suite-a", at(0, 0), at(23, 0))
        .await
        .expect("window");
    assert_eq!((w.arrived, w.stamped), (5, 5), "{adapter}");
    assert_eq!(
        w.packets,
        vec!["pkt-B", "pkt-a-z", "pkt-ab", "pkt-b"],
        "{adapter}: distinct, every packet byte for byte"
    );
}

/// A window on a sensor the registry does not hold is `UnknownSensor`,
/// never a count of zero — a retro reading zero would read a quiet
/// week off a misspelling.
async fn a_window_on_no_sensor_is_refused_not_zero<S: Sensors>(s: &S, adapter: &str) {
    let err = s
        .window("suite-none", at(0, 0), at(23, 0))
        .await
        .expect_err("a window on nothing");
    assert!(
        matches!(err, SensorsError::UnknownSensor(ref id) if id == "suite-none"),
        "{adapter}: {err}"
    );
}

/// A poll stamp always takes its `polled_at`, moves the cursor only
/// FORWARD, and leaves it where it is when the poll saw nothing.
async fn a_poll_stamp_moves_the_cursor_only_forward<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a")]).await;
    let stamps = [
        (at(10, 0), None, None),
        (at(10, 15), Some(at(10, 5)), Some(at(10, 5))),
        (at(10, 30), Some(at(9, 0)), Some(at(10, 5))),
        (at(10, 45), None, Some(at(10, 5))),
        (at(11, 0), Some(at(10, 50)), Some(at(10, 50))),
    ];
    for (polled_at, cursor_at, want) in stamps {
        s.mark_polled(
            "suite-a",
            &PollStamp {
                polled_at,
                cursor_at,
            },
        )
        .await
        .expect("mark_polled");
        let row = &list(s).await[0];
        assert_eq!(
            (row.last_polled_at, row.cursor_at),
            (Some(polled_at), want),
            "{adapter}: after the stamp at {polled_at}"
        );
    }
}

/// A poll stamp on a sensor the registry does not hold is
/// `UnknownSensor`, not an update of nothing.
async fn a_poll_stamp_on_no_sensor_is_refused<S: Sensors>(s: &S, adapter: &str) {
    let err = s
        .mark_polled(
            "suite-none",
            &PollStamp {
                polled_at: at(10, 0),
                cursor_at: None,
            },
        )
        .await
        .expect_err("a stamp on nothing");
    assert!(
        matches!(err, SensorsError::UnknownSensor(ref id) if id == "suite-none"),
        "{adapter}: {err}"
    );
}

/// The sweep deletes readings observed strictly BEFORE the cutoff that
/// owe nothing: a polled sensor's once stamped, a push-only sensor's by
/// age alone. A polled sensor's reading still owed a packet is kept at
/// any age, and the cutoff instant itself is kept. Answers how many.
async fn the_sweep_keeps_what_is_owed_and_ages_out_the_rest<S: Sensors>(s: &S, adapter: &str) {
    declare(s, &[polled("suite-a"), push_only("suite-site")]).await;
    s.record(
        "suite-a",
        &[
            reading("old-stamped", at(7, 0)),
            reading("old-owed", at(7, 0)),
            reading("edge-stamped", at(8, 0)),
            reading("new-stamped", at(9, 0)),
        ],
    )
    .await
    .expect("record a");
    s.record(
        "suite-site",
        &[
            reading("view-old", at(7, 0)),
            reading("view-edge", at(8, 0)),
        ],
    )
    .await
    .expect("record site");
    for ext in ["old-stamped", "edge-stamped", "new-stamped"] {
        s.stamp("suite-a", ext, "pkt-1").await.expect("stamp");
    }
    assert_eq!(
        s.sweep(at(8, 0)).await.expect("sweep"),
        2,
        "{adapter}: old-stamped and view-old"
    );
    assert_eq!(owed_ids(s, "suite-a").await, vec!["old-owed"], "{adapter}");
    let a = s
        .window("suite-a", at(0, 0), at(23, 0))
        .await
        .expect("window a");
    assert_eq!((a.arrived, a.stamped), (3, 2), "{adapter}");
    let site = s
        .window("suite-site", at(0, 0), at(23, 0))
        .await
        .expect("window site");
    assert_eq!(site.arrived, 1, "{adapter}: view-edge stays");
}
