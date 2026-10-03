//! The calendar answers the same on both `CalendarClient` adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next core port in the
//! census after the location registry (backlog be459ab9):
//! `CalendarClient`, which `InMemoryCalendar` and `PgCalendar`
//! implement.
//!
//! WHY THIS PORT. It holds the one race-free guarantee the platform
//! makes about time — one hard reservation per subject per window — and
//! the business calendars the dispatcher's timing triggers count
//! business days from. Every door test of the calendar
//! (`boss-calendar/src/http.rs`) and every unit test beside the double
//! asks `InMemoryCalendar`; production is answered by `PgCalendar`, and
//! the double's header says it "mirrors the GIST exclusion-constraint
//! semantics" — a claim with no test until this file.
//!
//! The first run found four disagreements, fixed in this car in
//! production code:
//!
//! - RESERVABILITY. Postgres refuses to reserve a subject whose kind the
//!   subject_kinds registry does not flag `calendar_reservable`; the
//!   double reserved any kind at all, so a door test could reserve a
//!   `location` that production refuses. The double now holds the
//!   migration's v1 reservable set (`employee`, `asset`, `account`) and
//!   refuses the rest with the Postgres adapter's words. Case
//!   `only_a_reservable_kind_can_be_reserved`.
//! - LIST ORDER. `list` promised nothing; Postgres answered `ORDER BY
//!   start_ts` and the double answered insertion order, and Postgres
//!   broke a tie on the start (two soft rows can share one) by whatever
//!   the plan returned. Both now answer by start, then id. Case
//!   `the_list_is_every_active_row_overlapping_the_window_by_start_then_id`.
//! - CONFLICT ORDER. The rows a `Conflict` carries, same shape: Postgres
//!   by start, the double by insertion. Case
//!   `a_hard_overlap_is_refused_naming_each_active_hard_row_it_overlaps`.
//! - BUSINESS CALENDAR ORDER. `list_business_calendars` promises "sorted
//!   by `code`"; Postgres sorted by the database's locale, which ignores
//!   `-` at first level and folds case, and the double by `String::cmp`
//!   (backlog 2987fb2d's class). Postgres now sorts `COLLATE "C"`. Case
//!   `business_calendars_list_in_byte_order_of_code`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds and every write passes its own `now`, so
//! Postgres's microseconds and the double's nanoseconds compare equal.
//! The migrations seed no reservation and no business calendar, and
//! both adapters start empty.

use boss_calendar::{CalendarClient, CalendarError, InMemoryCalendar, PgCalendar};
use boss_core::actor::ActorId;
use boss_core::calendar::{
    BusinessCalendar, Reservation, ReservationId, ReservationRequest, ReservationStrength,
    TimeWindow,
};
use boss_core::job::Subject;
use boss_core::publish::{FieldChange, KeptRow, PublishMode, UpdatedRow};
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryCalendar::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgCalendar::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_calendar_answers_nothing,
        a_reservation_reads_back_as_written,
        only_a_reservable_kind_can_be_reserved,
        a_window_that_holds_nothing_is_refused,
        a_hard_overlap_is_refused_naming_each_active_hard_row_it_overlaps,
        touching_windows_other_subjects_and_soft_rows_do_not_conflict,
        the_list_is_every_active_row_overlapping_the_window_by_start_then_id,
        a_cancel_flips_once_and_frees_the_slot,
        a_cancel_of_an_unknown_reservation_is_not_found,
        a_cascade_cancels_every_active_row_of_its_reason_and_counts_the_flips,
        a_business_calendar_publishes_insert_if_absent_and_reads_back,
        a_take_replaces_a_held_business_calendar_naming_each_change,
        business_calendars_list_in_byte_order_of_code,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, h, m, 0).unwrap()
}

fn window(from: DateTime<Utc>, to: DateTime<Utc>) -> TimeWindow {
    TimeWindow::new(from, to).unwrap()
}

fn stamp() -> EventStamp {
    EventStamp::new("calendar", ActorId::Human("emp-suite".into()))
}

fn emp(id: &str) -> Subject {
    Subject::new("employee", id)
}

fn request(
    subject: Subject,
    w: TimeWindow,
    strength: ReservationStrength,
    reason_ref: &str,
) -> ReservationRequest {
    ReservationRequest {
        subject,
        window: w,
        reason_kind: "job-step".into(),
        reason_ref_id: reason_ref.into(),
        strength,
        notes: None,
        created_by: "emp-suite".into(),
    }
}

/// Reserve at a whole-second `now`, expecting success.
async fn reserve<C: CalendarClient>(c: &C, req: ReservationRequest) -> ReservationId {
    c.reserve_at(req, at(8, 0), &stamp())
        .await
        .expect("reserve answers")
}

async fn hard<C: CalendarClient>(c: &C, who: &str, w: TimeWindow, reason: &str) -> ReservationId {
    reserve(c, request(emp(who), w, ReservationStrength::Hard, reason)).await
}

async fn soft<C: CalendarClient>(c: &C, who: &str, w: TimeWindow, reason: &str) -> ReservationId {
    reserve(c, request(emp(who), w, ReservationStrength::Soft, reason)).await
}

fn refs(rows: &[Reservation]) -> Vec<&str> {
    rows.iter().map(|r| r.reason_ref_id.as_str()).collect()
}

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn calendar(code: &str, name: &str, weekend: &[u8], closed: &[&str]) -> BusinessCalendar {
    BusinessCalendar {
        code: code.into(),
        name: name.into(),
        weekend: weekend.iter().copied().collect(),
        closed: closed.iter().map(|d| day(d)).collect(),
    }
}

// ----- cases: reservations -------------------------------------------------

/// Nothing written: a list is empty, an id is absent, a code is absent
/// and the business calendars are none — answers, never errors.
async fn an_empty_calendar_answers_nothing<C: CalendarClient>(c: &C, adapter: &str) {
    let rows = c
        .list(&emp("emp-a"), window(at(0, 0), at(23, 0)))
        .await
        .expect("list");
    assert!(rows.is_empty(), "{adapter}: {rows:?}");
    assert_eq!(
        c.get(ReservationId::new()).await.expect("get"),
        None,
        "{adapter}"
    );
    assert_eq!(
        c.get_business_calendar("us-banking").await.expect("get"),
        None,
        "{adapter}"
    );
    assert_eq!(
        c.list_business_calendars().await.expect("list"),
        vec![],
        "{adapter}"
    );
}

/// `get` answers the row as written — every field the request carried,
/// the id the reserve answered, the `now` it was written at — and
/// `list` answers the same row.
async fn a_reservation_reads_back_as_written<C: CalendarClient>(c: &C, adapter: &str) {
    let req = ReservationRequest {
        subject: Subject::new("asset", "kettle-1"),
        window: window(at(10, 0), at(12, 30)),
        reason_kind: "pm-schedule".into(),
        reason_ref_id: "pm-7".into(),
        strength: ReservationStrength::Soft,
        notes: Some("descale".into()),
        created_by: "emp-suite".into(),
    };
    let id = c
        .reserve_at(req.clone(), at(9, 15), &stamp())
        .await
        .expect("reserve");
    let want = Reservation {
        id,
        subject: req.subject.clone(),
        window: req.window,
        reason_kind: req.reason_kind.clone(),
        reason_ref_id: req.reason_ref_id.clone(),
        strength: req.strength,
        notes: req.notes.clone(),
        created_by: req.created_by.clone(),
        created_at: at(9, 15),
        cancelled_at: None,
    };
    assert_eq!(
        c.get(id).await.expect("get"),
        Some(want.clone()),
        "{adapter}"
    );
    assert_eq!(
        c.list(&req.subject, window(at(0, 0), at(23, 0)))
            .await
            .expect("list"),
        vec![want],
        "{adapter}"
    );
}

/// Only a kind the registry flags `calendar_reservable` may be
/// reserved: the migration flags `employee`, `asset` and `account`. A
/// kind it does not flag (`location`) and one it does not hold at all
/// are refused as an invalid request, naming the kind, and leave no row.
async fn only_a_reservable_kind_can_be_reserved<C: CalendarClient>(c: &C, adapter: &str) {
    for kind in ["employee", "asset", "account"] {
        let subject = Subject::new(kind, "x-1");
        reserve(
            c,
            request(
                subject,
                window(at(10, 0), at(11, 0)),
                ReservationStrength::Hard,
                kind,
            ),
        )
        .await;
    }
    for kind in ["location", "no-such-kind"] {
        let subject = Subject::new(kind, "x-1");
        let err = c
            .reserve_at(
                request(
                    subject.clone(),
                    window(at(10, 0), at(11, 0)),
                    ReservationStrength::Hard,
                    kind,
                ),
                at(8, 0),
                &stamp(),
            )
            .await
            .expect_err("an unreservable kind");
        match err {
            CalendarError::Invalid(msg) => assert_eq!(
                msg,
                format!("subject kind `{kind}` is not calendar-reservable"),
                "{adapter}"
            ),
            other => panic!("{adapter}: {kind} answered {other:?}"),
        }
        assert!(
            c.list(&subject, window(at(0, 0), at(23, 0)))
                .await
                .expect("list")
                .is_empty(),
            "{adapter}: {kind} left a row"
        );
    }
}

/// A window whose end is not after its start — built past the
/// constructor, whose fields are public — is refused on both.
async fn a_window_that_holds_nothing_is_refused<C: CalendarClient>(c: &C, adapter: &str) {
    for (start, end) in [(at(10, 0), at(10, 0)), (at(11, 0), at(10, 0))] {
        let w = TimeWindow { start, end };
        let err = c
            .reserve_at(
                request(emp("emp-a"), w, ReservationStrength::Soft, "r"),
                at(8, 0),
                &stamp(),
            )
            .await
            .expect_err("an empty window");
        assert!(
            matches!(err, CalendarError::Invalid(_)),
            "{adapter}: [{start}, {end}): {err:?}"
        );
    }
}

/// A hard request overlapping active hard rows on the same subject is
/// refused, the conflict naming EACH of them, by start — not a soft row
/// it overlaps, not a cancelled one — and the refusal writes nothing.
async fn a_hard_overlap_is_refused_naming_each_active_hard_row_it_overlaps<C: CalendarClient>(
    c: &C,
    adapter: &str,
) {
    // Written later-first, so an insertion order answers backwards.
    hard(c, "emp-a", window(at(11, 0), at(12, 0)), "later").await;
    hard(c, "emp-a", window(at(10, 0), at(11, 0)), "earlier").await;
    soft(c, "emp-a", window(at(10, 30), at(12, 0)), "soft").await;
    let gone = hard(c, "emp-a", window(at(9, 0), at(10, 0)), "cancelled").await;
    c.cancel_at(gone, "emp-suite", at(8, 30), &stamp())
        .await
        .expect("cancel");

    let err = c
        .reserve_at(
            request(
                emp("emp-a"),
                window(at(9, 30), at(11, 30)),
                ReservationStrength::Hard,
                "new",
            ),
            at(8, 0),
            &stamp(),
        )
        .await
        .expect_err("a hard overlap");
    match err {
        CalendarError::Conflict { existing } => {
            assert_eq!(refs(&existing), vec!["earlier", "later"], "{adapter}");
        }
        other => panic!("{adapter}: expected Conflict, got {other:?}"),
    }
    let rows = c
        .list(&emp("emp-a"), window(at(0, 0), at(23, 0)))
        .await
        .expect("list");
    assert_eq!(refs(&rows), vec!["earlier", "soft", "later"], "{adapter}");
}

/// The windows are half-open: one ending as the next starts does not
/// conflict. Another subject's hard row does not conflict, and a soft
/// row overlaps anything, hard or soft.
async fn touching_windows_other_subjects_and_soft_rows_do_not_conflict<C: CalendarClient>(
    c: &C,
    adapter: &str,
) {
    hard(c, "emp-a", window(at(10, 0), at(11, 0)), "a1").await;
    hard(c, "emp-a", window(at(11, 0), at(12, 0)), "a2").await;
    hard(c, "emp-a", window(at(9, 0), at(10, 0)), "a0").await;
    hard(c, "emp-b", window(at(10, 0), at(12, 0)), "b").await;
    reserve(
        c,
        request(
            Subject::new("asset", "emp-a"),
            window(at(10, 0), at(12, 0)),
            ReservationStrength::Hard,
            "same-id-other-kind",
        ),
    )
    .await;
    soft(c, "emp-a", window(at(9, 30), at(11, 30)), "s1").await;
    soft(c, "emp-a", window(at(9, 30), at(11, 30)), "s2").await;
    let rows = c
        .list(&emp("emp-a"), window(at(0, 0), at(23, 0)))
        .await
        .expect("list");
    assert_eq!(rows.len(), 5, "{adapter}: {:?}", refs(&rows));
}

/// `list` answers every ACTIVE row on the subject whose window overlaps
/// the asked one — a row touching its edge is out — ordered by start,
/// and by id where two starts are equal (soft rows may share one).
async fn the_list_is_every_active_row_overlapping_the_window_by_start_then_id<C: CalendarClient>(
    c: &C,
    adapter: &str,
) {
    soft(c, "emp-a", window(at(14, 0), at(15, 0)), "afternoon").await;
    soft(c, "emp-a", window(at(8, 0), at(9, 0)), "ends-at-edge").await;
    let x = soft(c, "emp-a", window(at(10, 0), at(11, 0)), "tie-x").await;
    let y = soft(c, "emp-a", window(at(10, 0), at(12, 0)), "tie-y").await;
    soft(c, "emp-a", window(at(9, 30), at(10, 30)), "early").await;
    soft(c, "emp-a", window(at(17, 0), at(18, 0)), "starts-at-edge").await;
    soft(c, "emp-b", window(at(10, 0), at(11, 0)), "other-subject").await;
    let gone = soft(c, "emp-a", window(at(9, 0), at(16, 0)), "cancelled").await;
    c.cancel_at(gone, "emp-suite", at(8, 30), &stamp())
        .await
        .expect("cancel");

    let rows = c
        .list(&emp("emp-a"), window(at(9, 0), at(17, 0)))
        .await
        .expect("list");
    let (first, second) = if x.inner().as_uuid() < y.inner().as_uuid() {
        ("tie-x", "tie-y")
    } else {
        ("tie-y", "tie-x")
    };
    assert_eq!(
        refs(&rows),
        vec!["early", first, second, "afternoon"],
        "{adapter}"
    );
}

/// A cancel stamps `cancelled_at` with its `now` and frees the slot; a
/// second cancel is a no-op that keeps the first stamp; the row still
/// reads back by id, cancelled.
async fn a_cancel_flips_once_and_frees_the_slot<C: CalendarClient>(c: &C, adapter: &str) {
    let id = hard(c, "emp-a", window(at(10, 0), at(12, 0)), "first").await;
    c.cancel_at(id, "emp-suite", at(9, 0), &stamp())
        .await
        .expect("cancel");
    c.cancel_at(id, "emp-suite", at(9, 30), &stamp())
        .await
        .expect("cancel again");
    let row = c.get(id).await.expect("get").expect("still there");
    assert_eq!(row.cancelled_at, Some(at(9, 0)), "{adapter}");
    hard(c, "emp-a", window(at(10, 0), at(12, 0)), "second").await;
    let rows = c
        .list(&emp("emp-a"), window(at(0, 0), at(23, 0)))
        .await
        .expect("list");
    assert_eq!(refs(&rows), vec!["second"], "{adapter}");
}

/// Cancelling an id the calendar never held is NotFound, naming it.
async fn a_cancel_of_an_unknown_reservation_is_not_found<C: CalendarClient>(c: &C, adapter: &str) {
    let id = ReservationId::new();
    match c.cancel_at(id, "emp-suite", at(9, 0), &stamp()).await {
        Err(CalendarError::NotFound(named)) => assert_eq!(named, id, "{adapter}"),
        other => panic!("{adapter}: expected NotFound, got {other:?}"),
    }
}

/// The cascade cancels every active row carrying its (kind, ref) pair,
/// across subjects, and counts only the rows it flipped: a row already
/// cancelled, a row of another ref, and a row of another KIND with the
/// same ref are untouched. A second cascade flips none.
async fn a_cascade_cancels_every_active_row_of_its_reason_and_counts_the_flips<
    C: CalendarClient,
>(
    c: &C,
    adapter: &str,
) {
    hard(c, "emp-a", window(at(10, 0), at(11, 0)), "step-1").await;
    hard(c, "emp-b", window(at(10, 0), at(11, 0)), "step-1").await;
    let early = hard(c, "emp-c", window(at(10, 0), at(11, 0)), "step-1").await;
    c.cancel_at(early, "emp-suite", at(8, 30), &stamp())
        .await
        .expect("cancel");
    hard(c, "emp-d", window(at(10, 0), at(11, 0)), "step-2").await;
    let mut other_kind = request(
        emp("emp-e"),
        window(at(10, 0), at(11, 0)),
        ReservationStrength::Hard,
        "step-1",
    );
    other_kind.reason_kind = "pto".into();
    let pto = reserve(c, other_kind).await;

    let n = c
        .cancel_by_reason_at("job-step", "step-1", "emp-suite", at(9, 0), &stamp())
        .await
        .expect("cascade");
    assert_eq!(n, 2, "{adapter}");
    assert_eq!(
        c.cancel_by_reason_at("job-step", "step-1", "emp-suite", at(9, 30), &stamp())
            .await
            .expect("cascade again"),
        0,
        "{adapter}"
    );
    let early_row = c.get(early).await.expect("get").expect("held");
    assert_eq!(early_row.cancelled_at, Some(at(8, 30)), "{adapter}");
    let pto_row = c.get(pto).await.expect("get").expect("held");
    assert_eq!(pto_row.cancelled_at, None, "{adapter}");
    let d = c
        .list(&emp("emp-d"), window(at(0, 0), at(23, 0)))
        .await
        .expect("list");
    assert_eq!(refs(&d), vec!["step-2"], "{adapter}");
}

// ----- cases: business calendars ------------------------------------------

/// The default publish inserts an absent code and KEEPS a held one,
/// naming the fields the declaration differs on; a restatement is
/// unchanged. The held calendar reads back as first written — header,
/// weekend and closed-day set.
async fn a_business_calendar_publishes_insert_if_absent_and_reads_back<C: CalendarClient>(
    c: &C,
    adapter: &str,
) {
    let first = calendar("acme", "Acme", &[4, 5], &["2026-12-25", "2026-01-01"]);
    let out = c
        .publish_business_calendars(
            &[first.clone(), calendar("empty-week", "Open", &[], &[])],
            PublishMode::InsertIfAbsent,
            &stamp(),
        )
        .await
        .expect("publish");
    assert_eq!((out.received, out.inserted), (2, 2), "{adapter}: {out:?}");

    let edited = calendar("acme", "Acme Co", &[5, 6], &["2026-12-25"]);
    let out = c
        .publish_business_calendars(
            &[edited, first.clone()],
            PublishMode::InsertIfAbsent,
            &stamp(),
        )
        .await
        .expect("publish again");
    assert_eq!(out.inserted, 0, "{adapter}");
    assert_eq!(
        out.kept,
        vec![KeptRow {
            id: "acme".into(),
            differs: vec!["name".into(), "weekend".into(), "closed".into()],
        }],
        "{adapter}"
    );
    assert_eq!(out.unchanged, 1, "{adapter}");
    assert_eq!(
        c.get_business_calendar("acme").await.expect("get"),
        Some(first),
        "{adapter}"
    );
    assert_eq!(
        c.get_business_calendar("empty-week").await.expect("get"),
        Some(calendar("empty-week", "Open", &[], &[])),
        "{adapter}"
    );
}

/// `Take` replaces a held calendar wholesale — header and closed days —
/// and names each change from → to; a take restating the held row is
/// unchanged.
async fn a_take_replaces_a_held_business_calendar_naming_each_change<C: CalendarClient>(
    c: &C,
    adapter: &str,
) {
    let held = calendar("acme", "Acme", &[5, 6], &["2026-12-25", "2026-11-26"]);
    c.publish_business_calendars(
        std::slice::from_ref(&held),
        PublishMode::InsertIfAbsent,
        &stamp(),
    )
    .await
    .expect("publish");
    let taken = calendar("acme", "Acme", &[5, 6], &["2026-07-03"]);
    let out = c
        .publish_business_calendars(std::slice::from_ref(&taken), PublishMode::Take, &stamp())
        .await
        .expect("take");
    assert_eq!(
        out.updated,
        vec![UpdatedRow {
            id: "acme".into(),
            changes: vec![FieldChange::new("closed", &held.closed, &taken.closed)],
        }],
        "{adapter}"
    );
    assert_eq!(
        c.get_business_calendar("acme").await.expect("get"),
        Some(taken.clone()),
        "{adapter}"
    );
    let again = c
        .publish_business_calendars(&[taken], PublishMode::Take, &stamp())
        .await
        .expect("take again");
    assert_eq!((again.unchanged, again.updated.len()), (1, 0), "{adapter}");
}

/// Every held calendar, with its closed days, in BYTE order of code
/// (`-` before a letter, upper before lower) — the only order both
/// adapters can hold. A locale order would answer two lists.
async fn business_calendars_list_in_byte_order_of_code<C: CalendarClient>(c: &C, adapter: &str) {
    let cals: Vec<BusinessCalendar> = ["suite-ab", "suite-B", "suite-a-z", "suite-b"]
        .into_iter()
        .map(|code| calendar(code, code, &[5, 6], &["2026-12-25"]))
        .collect();
    c.publish_business_calendars(&cals, PublishMode::InsertIfAbsent, &stamp())
        .await
        .expect("publish");
    let listed = c.list_business_calendars().await.expect("list");
    let codes: Vec<&str> = listed.iter().map(|b| b.code.as_str()).collect();
    assert_eq!(
        codes,
        vec!["suite-B", "suite-a-z", "suite-ab", "suite-b"],
        "{adapter}"
    );
    assert!(
        listed.iter().all(|b| b.closed.contains(&day("2026-12-25"))),
        "{adapter}: every calendar carries its closed days"
    );
}
