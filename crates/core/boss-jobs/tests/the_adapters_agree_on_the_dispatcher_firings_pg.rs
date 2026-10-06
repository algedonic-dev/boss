//! The dispatcher's firing record answers the same on both adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next port in the census
//! order after the cadence registry and the delivery policy (backlog
//! be459ab9): `DispatcherFiringsRepository`, which
//! `InMemoryDispatcherFirings` and `PgDispatcherFirings` implement.
//!
//! WHY THIS PORT. It is what the IT world map and the rules list read to
//! say when a dispatcher rule last fired and how often it dead-lettered
//! on a topic naming no packet — the reading that tells a stalled
//! `auto-park-on-gate-green` from an idle one. Every door test of those
//! surfaces (`yard_borders_http.rs`, `yard_rule_firings_http.rs`, the
//! dispatcher's schedule door) asks `InMemoryDispatcherFirings`;
//! production is answered by `PgDispatcherFirings`. `dispatcher_firings_pg.rs`
//! states what the one table holds and `dispatcher_firings::tests` what
//! the double does, each in its own words; this file is the one statement
//! both are held to — every method of the port.
//!
//! THE PORT IS READ-ONLY. The writer is the rules runner in
//! `boss-dispatcher`, which depends on this crate and so cannot be driven
//! from here. Each world therefore RECORDS a row the way that writer
//! does — the Postgres world as the row the runner inserts, the in-memory
//! world as the declaration the double is built from — and the case reads
//! it back through the port.
//!
//! The first run found two disagreements (4 of 16 adapter cases red),
//! fixed in this car in production code:
//! - A TIE. Two firings of one rule at the same instant (a clock tick
//!   and an event, or two events stamped by one clock reading) left
//!   "the newest" unstated: Postgres answered whichever tied row its
//!   plan met first (`ORDER BY fired_at DESC LIMIT 1`, and `DISTINCT ON`
//!   the same) — measured, the first inserted — while the double's
//!   `last_firing` answered the LAST one declared and its `last_firings`
//!   the FIRST, so the double disagreed with itself and the world map
//!   and the rules list could name two topics for one rule. The newest
//!   is now `(fired_at, firing_id)`, the id in byte order, greatest
//!   wins: `dispatcher_firings::recency`, which both of the double's
//!   reads order by, and its SQL spelling `NEWEST_FIRST`, which both
//!   Postgres reads end their `ORDER BY` with. The primary key makes it
//!   a total order. The cadence record has the same class (backlog
//!   af532492). Case `a_tie_at_one_instant_is_broken_by_the_firing_id`.
//! - ORDER. `last_firings` and `unrouted_dead_letters` promise "ordered
//!   by rule name". Postgres sorted by the database's locale, which
//!   ignores `-` at first level, so `suite-ab` served before `suite-a-z`
//!   there and after it in memory. Both Postgres reads now sort
//!   `COLLATE "C"` — byte order, the double's map order — over the
//!   one-row-per-rule result, so the recency index still serves the
//!   scan; the Workflow, credentials, station, department and cadence
//!   suites found and fixed the same defect the same way. Cases
//!   `every_rules_newest_firing_is_name_ordered_in_byte_order` and
//!   `unrouted_dead_letters_are_name_ordered_in_byte_order`.
//!
//! THE CHECK. `dispatcher_firings.outcome` carries
//! `CHECK (outcome IN ('fired', 'dead-letter'))`, and every read filters
//! by `OUTCOME_FIRED` / `OUTCOME_DEAD_LETTER`. A third outcome added to
//! the table alone would be a row neither read sees; a constant respelled
//! alone would read nothing. `the_outcome_constants_are_the_tables_check`
//! holds the two to the live table.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal. Every rule this file records
//! starts `suite-`; the migrations seed no firing, and both adapters
//! start empty.

use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, InMemoryDispatcherFirings, LastFiring, OUTCOME_DEAD_LETTER,
    OUTCOME_FIRED, PgDispatcherFirings, RuleLastFiring, UnroutedDeadLetters,
};
use chrono::{DateTime, TimeZone, Utc};
use std::collections::BTreeSet;
use std::sync::Mutex;

/// One row of the record, as the rules runner writes it.
#[derive(Clone)]
struct Row {
    firing_id: String,
    rule: String,
    fired_on: String,
    fired_at: DateTime<Utc>,
    outcome: &'static str,
}

/// The port, and the one way into it a read-only port has: record a row
/// the way its writer does.
trait World {
    type R: DispatcherFiringsRepository;
    async fn record(&self, row: Row);
    /// The adapter over everything recorded so far.
    fn repo(&self) -> Self::R;
}

/// The double is built from its declarations, so this world keeps them
/// and builds the adapter on each read.
struct InMemory(Mutex<Vec<Row>>);

impl World for InMemory {
    type R = InMemoryDispatcherFirings;
    async fn record(&self, row: Row) {
        self.0.lock().expect("rows").push(row);
    }
    fn repo(&self) -> InMemoryDispatcherFirings {
        let rows = self.0.lock().expect("rows").clone();
        let firings = rows
            .iter()
            .filter(|r| r.outcome == OUTCOME_FIRED)
            .map(|r| {
                (
                    r.rule.clone(),
                    LastFiring {
                        firing_id: r.firing_id.clone(),
                        fired_on: r.fired_on.clone(),
                        fired_at: r.fired_at,
                    },
                )
            })
            .collect();
        let dead = rows
            .iter()
            .filter(|r| r.outcome == OUTCOME_DEAD_LETTER)
            .map(|r| (r.rule.clone(), r.fired_at))
            .collect();
        InMemoryDispatcherFirings::new(firings).with_unrouted_dead_letters(dead)
    }
}

struct Postgres(sqlx::PgPool);

impl World for Postgres {
    type R = PgDispatcherFirings;
    async fn record(&self, row: Row) {
        sqlx::query(
            "INSERT INTO dispatcher_firings \
             (firing_id, rule_name, fired_on, fired_at, detail, outcome) \
             VALUES ($1, $2, $3, $4, '{}'::jsonb, $5)",
        )
        .bind(&row.firing_id)
        .bind(&row.rule)
        .bind(&row.fired_on)
        .bind(row.fired_at)
        .bind(row.outcome)
        .execute(&self.0)
        .await
        .expect("record a row");
    }
    fn repo(&self) -> PgDispatcherFirings {
        PgDispatcherFirings::new(self.0.clone())
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(Mutex::new(Vec::new())), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (Postgres(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_record_answers_nothing_and_no_error,
        last_firing_is_the_newest_firing_of_its_own_rule,
        every_rules_newest_firing_is_one_row_per_rule,
        every_rules_newest_firing_is_name_ordered_in_byte_order,
        a_tie_at_one_instant_is_broken_by_the_firing_id,
        a_dead_letter_is_never_read_as_a_firing,
        unrouted_dead_letters_count_inside_the_window_inclusive,
        unrouted_dead_letters_are_name_ordered_in_byte_order,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
}

fn fired(id: &str, rule: &str, fired_on: &str, fired_at: DateTime<Utc>) -> Row {
    Row {
        firing_id: id.into(),
        rule: rule.into(),
        fired_on: fired_on.into(),
        fired_at,
        outcome: OUTCOME_FIRED,
    }
}

fn dead_letter(id: &str, rule: &str, fired_at: DateTime<Utc>) -> Row {
    Row {
        firing_id: id.into(),
        rule: rule.into(),
        fired_on: "commerce.invoice.issued".into(),
        fired_at,
        outcome: OUTCOME_DEAD_LETTER,
    }
}

async fn last<W: World>(w: &W, rule: &str) -> Option<LastFiring> {
    w.repo()
        .last_firing(rule)
        .await
        .expect("last_firing answers")
}

async fn every<W: World>(w: &W) -> Vec<RuleLastFiring> {
    w.repo().last_firings().await.expect("last_firings answers")
}

async fn unrouted<W: World>(w: &W, since: DateTime<Utc>) -> Vec<UnroutedDeadLetters> {
    w.repo()
        .unrouted_dead_letters(since)
        .await
        .expect("unrouted_dead_letters answers")
}

fn newest(rule: &str, fired_on: &str, fired_at: DateTime<Utc>) -> RuleLastFiring {
    RuleLastFiring {
        rule: rule.into(),
        fired_on: fired_on.into(),
        fired_at,
    }
}

// ----- cases ---------------------------------------------------------------

/// Nothing recorded: no rule has fired, the every-rule read is empty and
/// no rule has dead-lettered — each an answer, never an error.
async fn an_empty_record_answers_nothing_and_no_error<W: World>(w: &W, adapter: &str) {
    assert_eq!(last(w, "suite-never").await, None, "{adapter}");
    assert!(every(w).await.is_empty(), "{adapter}");
    assert!(unrouted(w, at(0, 0)).await.is_empty(), "{adapter}");
}

/// `last_firing` is the newest firing BY ITS INSTANT, not by the order
/// recorded, whole — id, topic, instant — and only of the rule asked; a
/// rule that never fired is `None`.
async fn last_firing_is_the_newest_firing_of_its_own_rule<W: World>(w: &W, adapter: &str) {
    for row in [
        fired("suite-a-10", "suite-a", "jobs.gate.green", at(10, 0)),
        fired("suite-a-12", "suite-a", "step.done.review", at(12, 0)),
        fired("suite-a-11", "suite-a", "jobs.gate.green", at(11, 0)),
        fired("suite-b-13", "suite-b", "clock.day", at(13, 0)),
    ] {
        w.record(row).await;
    }
    assert_eq!(
        last(w, "suite-a").await,
        Some(LastFiring {
            firing_id: "suite-a-12".into(),
            fired_on: "step.done.review".into(),
            fired_at: at(12, 0),
        }),
        "{adapter}"
    );
    assert_eq!(
        last(w, "suite-b").await.map(|l| l.firing_id),
        Some("suite-b-13".into()),
        "{adapter}: a newer firing of another rule does not leak across names"
    );
    assert_eq!(last(w, "suite-c").await, None, "{adapter}");
}

/// The rules list's read: one entry per rule, each its newest firing's
/// topic and instant — never a page of firings.
async fn every_rules_newest_firing_is_one_row_per_rule<W: World>(w: &W, adapter: &str) {
    for row in [
        fired("suite-park-1", "suite-park", "jobs.gate.green", at(9, 0)),
        fired("suite-park-3", "suite-park", "jobs.gate.red", at(11, 0)),
        fired("suite-park-2", "suite-park", "jobs.gate.green", at(10, 0)),
        fired("suite-mark-1", "suite-mark", "step.ready", at(8, 0)),
    ] {
        w.record(row).await;
    }
    assert_eq!(
        every(w).await,
        vec![
            newest("suite-mark", "step.ready", at(8, 0)),
            newest("suite-park", "jobs.gate.red", at(11, 0)),
        ],
        "{adapter}"
    );
}

/// "Ordered by rule name" is BYTE order (`-` sorts before a letter) —
/// the order the double's map holds and the only one both adapters can:
/// a rules list whose order depends on the database's locale answers two
/// questions.
async fn every_rules_newest_firing_is_name_ordered_in_byte_order<W: World>(w: &W, adapter: &str) {
    for rule in ["suite-b", "suite-ab", "suite-a-z"] {
        w.record(fired(&format!("{rule}-1"), rule, "t", at(12, 0)))
            .await;
    }
    let names: Vec<String> = every(w).await.into_iter().map(|f| f.rule).collect();
    assert_eq!(
        names,
        vec!["suite-a-z", "suite-ab", "suite-b"],
        "{adapter}: every name byte for byte"
    );
}

/// Two firings of one rule at the SAME instant: the newest is the one
/// whose firing id is greatest in byte order — whichever was recorded
/// first, and on BOTH reads, so the world map and the rules list name the
/// same topic. The ids differ only in case and punctuation, so a locale
/// order and byte order would disagree on them too.
async fn a_tie_at_one_instant_is_broken_by_the_firing_id<W: World>(w: &W, adapter: &str) {
    // Recorded in both orders across two rules, so neither "first
    // declared" nor "last declared" can pass by accident.
    for row in [
        fired("dispatcher:suite-t:B", "suite-t", "clock.tick", at(12, 0)),
        fired(
            "dispatcher:suite-t:a",
            "suite-t",
            "jobs.gate.green",
            at(12, 0),
        ),
        fired("dispatcher:suite-t:0", "suite-t", "older", at(11, 0)),
        fired(
            "dispatcher:suite-u:a",
            "suite-u",
            "jobs.gate.green",
            at(12, 0),
        ),
        fired("dispatcher:suite-u:B", "suite-u", "clock.tick", at(12, 0)),
    ] {
        w.record(row).await;
    }
    // Byte order: 'a' (0x61) > 'B' (0x42); a locale puts b after a.
    for rule in ["suite-t", "suite-u"] {
        assert_eq!(
            last(w, rule).await,
            Some(LastFiring {
                firing_id: format!("dispatcher:{rule}:a"),
                fired_on: "jobs.gate.green".into(),
                fired_at: at(12, 0),
            }),
            "{adapter}: {rule}"
        );
    }
    assert_eq!(
        every(w).await,
        vec![
            newest("suite-t", "jobs.gate.green", at(12, 0)),
            newest("suite-u", "jobs.gate.green", at(12, 0)),
        ],
        "{adapter}: the every-rule read breaks the tie the same way"
    );
}

/// A dead-letter row is NOT a firing (4b175523): neither firing read
/// answers with it, however much newer it is, or a rule failing every
/// event would read "fired a minute ago"; a rule with only dead-letters
/// has never fired.
async fn a_dead_letter_is_never_read_as_a_firing<W: World>(w: &W, adapter: &str) {
    w.record(fired(
        "suite-inv-1",
        "suite-inv",
        "commerce.invoice.issued",
        at(9, 0),
    ))
    .await;
    w.record(dead_letter("dl-suite-inv-2", "suite-inv", at(10, 0)))
        .await;
    w.record(dead_letter("dl-suite-only-1", "suite-only", at(10, 0)))
        .await;
    assert_eq!(
        last(w, "suite-inv").await.map(|l| l.firing_id),
        Some("suite-inv-1".into()),
        "{adapter}: the newest FIRING, not the newer dead-letter"
    );
    assert_eq!(last(w, "suite-only").await, None, "{adapter}");
    assert_eq!(
        every(w).await,
        vec![newest("suite-inv", "commerce.invoice.issued", at(9, 0))],
        "{adapter}"
    );
}

/// The dead-letter read counts each rule's dead-letters at or after
/// `since` — the boundary instant INCLUDED — with the newest, and never
/// counts a firing.
async fn unrouted_dead_letters_count_inside_the_window_inclusive<W: World>(w: &W, adapter: &str) {
    for row in [
        dead_letter("dl-1", "suite-inv", at(8, 0)),
        dead_letter("dl-2", "suite-inv", at(10, 0)),
        dead_letter("dl-3", "suite-inv", at(9, 0)),
        dead_letter("dl-4", "suite-inv", at(7, 59)),
        dead_letter("dl-5", "suite-old", at(7, 0)),
    ] {
        w.record(row).await;
    }
    w.record(fired("f-1", "suite-inv", "t", at(11, 0))).await;
    assert_eq!(
        unrouted(w, at(8, 0)).await,
        vec![UnroutedDeadLetters {
            rule: "suite-inv".into(),
            count: 3,
            newest_at: at(10, 0),
        }],
        "{adapter}: 08:00 is inside a window from 08:00; 07:59 and the firing are not"
    );
}

/// The dead-letter read is ordered by rule name in BYTE order, as the
/// firing read is.
async fn unrouted_dead_letters_are_name_ordered_in_byte_order<W: World>(w: &W, adapter: &str) {
    for rule in ["suite-b", "suite-ab", "suite-a-z"] {
        w.record(dead_letter(&format!("dl-{rule}"), rule, at(12, 0)))
            .await;
    }
    let names: Vec<String> = unrouted(w, at(0, 0))
        .await
        .into_iter()
        .map(|d| d.rule)
        .collect();
    assert_eq!(
        names,
        vec!["suite-a-z", "suite-ab", "suite-b"],
        "{adapter}: every name byte for byte"
    );
}

// ----- the pin: the outcome constants ARE the table's check -----------------

/// A FACT THAT LIVES TWICE (CLAUDE.md §9a): which outcomes a row can hold
/// is stated by the table's CHECK and by the two constants every read
/// filters on. Held to the live table the migrations build: the CHECK's
/// literals are exactly `{OUTCOME_FIRED, OUTCOME_DEAD_LETTER}`, so an
/// outcome added or respelled on one side alone goes red naming it.
#[tokio::test(flavor = "multi_thread")]
async fn the_outcome_constants_are_the_tables_check() {
    let db = boss_testing::TestDb::new().await;
    let defs: Vec<String> = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint \
         WHERE conrelid = 'dispatcher_firings'::regclass AND contype = 'c' \
         AND pg_get_constraintdef(oid) LIKE '%outcome%'",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read pg_constraint");
    assert_eq!(defs.len(), 1, "one CHECK on outcome: {defs:?}");
    let literals: BTreeSet<String> = defs[0]
        .split('\'')
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|pair| match pair {
            [_, lit] => Some((*lit).to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        literals,
        [OUTCOME_FIRED, OUTCOME_DEAD_LETTER]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "the table's outcomes are the constants every read filters on: {}",
        defs[0]
    );
}
