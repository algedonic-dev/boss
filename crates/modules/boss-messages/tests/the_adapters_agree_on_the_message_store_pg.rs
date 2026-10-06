//! The message store answers the same on both `MessageRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), the next MODULE port of the
//! census after the Equipment knowledge base (backlog be459ab9). The
//! HTTP unit tests and the dispatcher's notifier tests are answered by
//! `InMemoryMessages`, while production is answered by `PgMessages`.
//!
//! WHY IT EXISTS. The double was stated by its own unit tests and the
//! Postgres adapter by the audit-log, batch and rebuild tests, never
//! against each other; this file is the one statement of every method of
//! the port, each case holding each adapter to the same stated answer,
//! and every write also judged by the facts it leaves — the double's
//! `recorded_events` and Postgres's `event_outbox`, staged in the
//! write's own transaction.
//!
//! WHAT ITS FIRST RUN FOUND (2026-10-01), each fixed in the adapter the
//! port's own words make wrong:
//! - ORDER. Messages sent in the same instant came back in insertion
//!   order from the double and in no stated order from Postgres; the
//!   inbox and a thread now break a tie on `sent_at` by BYTE order of id
//!   (`COLLATE "C"`, backlog 2987fb2d's class), and the archive doors
//!   record their per-row facts in that order too.
//! - THE ROW. The double kept `sent_at`, `read_at` and `archived_at` to
//!   the nanosecond; the columns keep microseconds. It now keeps what
//!   the column keeps. Postgres's send dropped a `read_at` or
//!   `archived_at` the message carried, while its own sent fact (the
//!   full row state) and the double kept them, and its rebuild replays
//!   `read_at` from that fact — so a rebuild changed the row. Send now
//!   stores both, and the rebuild replays both.
//! - A PREFIX IS A PREFIX. Both expire doors matched `entity_path` with
//!   `LIKE prefix || '%'` on Postgres, so a `_` or `%` in the prefix was
//!   a wildcard; the port says "starts with", which the double does. It
//!   is now `starts_with` on both.
//! - A REPLY'S PARENT. The `reply_to` column references `messages(id)`
//!   ON DELETE SET NULL: Postgres refused a reply to a message nobody
//!   sent with its foreign-key error as `Storage` (a 500), and cleared a
//!   reply's `reply_to` when its parent was deleted. The double stored
//!   the orphan and kept the dangling pointer. Both now refuse the orphan
//!   `BadRequest` naming `reply_to`, and the double clears the pointer.
//! - A NUL BYTE. A send carrying one was refused by Postgres with its
//!   encoding error as `Storage` and stored by the double; a read, mark,
//!   delete or archive keyed by one was a 500 on Postgres. Both now
//!   refuse the send `BadRequest` naming the field, and a key holding one
//!   is the miss it is.
//!
//! NOT CHANGED, because the port says it: a send on an id already held
//! is an idempotent no-op (a redelivered notification collapses), not a
//! `Conflict` — both adapters already agreed, and the dispatcher's
//! notifier relies on the deterministic id collapsing.
//!
//! The world: every row is written THROUGH the port, so each adapter
//! seeds itself the way production does, and every recipient, id and
//! fact is `suite-` scoped. The fixtures hold case and punctuation pairs
//! (`suite-B`, `suite-a-z`, `suite-ab`) — the only shape that catches the
//! database's locale disagreeing with the double's byte order.

use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use boss_messages::events::{MESSAGE_ARCHIVED, MESSAGE_DELETED, MESSAGE_READ, MESSAGE_SENT};
use boss_messages::{
    EntityRef, InMemoryMessages, InboxQuery, Message, MessageError, MessageKind, MessageRepository,
    PgMessages,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

/// The store under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: MessageRepository;
    fn repo(&self) -> &Self::R;
    /// Every `messages.*` fact about a `suite-` id, as `(kind, payload)`,
    /// in the order recorded.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("messages.")
        && payload["id"]
            .as_str()
            .is_some_and(|v| v.starts_with("suite-"))
}

struct InMemory(InMemoryMessages);

impl World for InMemory {
    type R = InMemoryMessages;
    fn repo(&self) -> &InMemoryMessages {
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
    repo: PgMessages,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgMessages;
    fn repo(&self) -> &PgMessages {
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

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryMessages::new(vec![])), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres {
                repo: PgMessages::new(db.pool.clone()),
                pool: db.pool.clone(),
            };
            (world, db)
        },
    }
    cases {
        an_inbox_lists_newest_first_ties_in_byte_order_of_id_and_leaves_archived_out_unless_asked,
        an_inbox_page_is_a_slice_of_the_narrowed_list_and_says_how_many_the_narrowing_holds,
        the_inbox_counts_each_kind_all_and_unread_in_byte_order_of_kind,
        a_message_reads_back_as_sent_to_the_microsecond_and_a_stranger_is_none,
        a_send_records_the_message_as_sent_and_a_redelivery_changes_nothing,
        a_message_sent_read_or_archived_keeps_that_state,
        the_unread_count_leaves_out_read_and_archived_and_narrows_by_kind,
        a_mark_read_sets_the_time_and_records_it_and_a_phantom_records_nothing,
        a_delete_removes_the_message_clears_its_replies_pointer_and_records_it,
        an_archive_keeps_the_kind_and_a_repeat_is_a_no_op,
        expiring_signals_moves_only_unread_signals_under_a_literal_prefix,
        expiring_notices_moves_only_unread_ids_under_both_literal_prefixes,
        a_thread_is_the_root_and_its_direct_replies_oldest_first,
        a_reply_to_a_message_nobody_sent_is_refused_naming_reply_to,
        a_nul_byte_is_refused_naming_its_field_and_misses_on_every_key,
    }
}

// ----- fixtures ------------------------------------------------------------

/// An instant `secs` after a fixed origin, carrying nanoseconds below
/// the microsecond the column keeps.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_789)
        .single()
        .expect("a representable instant")
}

/// `at(secs)` as the column keeps it: to the microsecond.
fn kept(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 123_456_000)
        .single()
        .expect("a representable instant")
}

fn stamp(secs: i64) -> EventStamp {
    EventStamp::new("messages", ActorId::Automation("suite".into())).with_timestamp(at(secs))
}

/// The fact `stamp(secs)` builds for `kind` — the payload as each
/// adapter must record it, `_actor` included.
fn fact(secs: i64, kind: &str, payload: Value) -> (String, Value) {
    (kind.to_string(), stamp(secs).event(kind, payload).payload)
}

fn sent_fact(secs: i64, m: &Message) -> (String, Value) {
    fact(
        secs,
        MESSAGE_SENT,
        serde_json::to_value(m).expect("a message serialises"),
    )
}

fn archived_fact(secs: i64, id: &str, reason: bool) -> (String, Value) {
    let mut p = json!({"id": id, "archived_at": at(secs)});
    if reason {
        p["reason"] = json!("entity-past-relevancy");
    }
    fact(secs, MESSAGE_ARCHIVED, p)
}

/// A bare direct to `suite-r` sent at `at(secs)`.
fn message(id: &str, secs: i64) -> Message {
    Message {
        id: id.into(),
        sender_id: "suite-sender".into(),
        recipient_id: "suite-r".into(),
        subject: format!("Subject {id}"),
        body: format!("Body {id}"),
        entity_ref: None,
        kind: MessageKind::direct(),
        sent_at: at(secs),
        read_at: None,
        reply_to: None,
        archived_at: None,
    }
}

/// An unread message of `kind` about `path`.
fn about(id: &str, kind: &str, path: &str, secs: i64) -> Message {
    Message {
        kind: MessageKind::new(kind),
        entity_ref: Some(EntityRef {
            entity_type: "job".into(),
            entity_id: "suite-job".into(),
            entity_path: Some(path.into()),
        }),
        ..message(id, secs)
    }
}

/// `m` as the store reads it back: every time to the microsecond.
fn read_back(m: &Message) -> Message {
    let trunc = |t: DateTime<Utc>| {
        Utc.timestamp_opt(t.timestamp(), t.timestamp_subsec_micros() * 1_000)
            .single()
            .expect("a representable instant")
    };
    Message {
        sent_at: trunc(m.sent_at),
        read_at: m.read_at.map(trunc),
        archived_at: m.archived_at.map(trunc),
        ..m.clone()
    }
}

async fn send<W: World>(w: &W, adapter: &str, m: &Message, secs: i64) {
    w.repo()
        .send(m, &stamp(secs))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: send {:?}: {e:?}", m.id));
}

async fn get<W: World>(w: &W, adapter: &str, id: &str) -> Option<Message> {
    w.repo()
        .message_by_id(id)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: message_by_id {id:?}: {e:?}"))
}

/// The whole inbox in one page: more rows than any case sends.
async fn inbox_ids<W: World>(w: &W, adapter: &str, who: &str, archived: bool) -> Vec<String> {
    let q = InboxQuery {
        include_archived: archived,
        ..InboxQuery::first(1000)
    };
    page_of(w, adapter, who, &q).await.0
}

/// One page's ids and the total its narrowing matches.
async fn page_of<W: World>(w: &W, adapter: &str, who: &str, q: &InboxQuery) -> (Vec<String>, u64) {
    let page = w
        .repo()
        .inbox(who, q)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: inbox {who:?} {q:?}: {e:?}"));
    (page.rows.into_iter().map(|m| m.id).collect(), page.total)
}

/// The per-kind counts as `(kind, all, unread)`.
async fn counts<W: World>(
    w: &W,
    adapter: &str,
    who: &str,
    archived: bool,
) -> Vec<(String, u64, u64)> {
    w.repo()
        .inbox_counts(who, archived)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: inbox_counts {who:?}: {e:?}"))
        .into_iter()
        .map(|c| (c.kind, c.all, c.unread))
        .collect()
}

async fn unread<W: World>(w: &W, adapter: &str, who: &str, kind: Option<&str>) -> u32 {
    w.repo()
        .unread_count(who, kind)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: unread_count {who:?} {kind:?}: {e:?}"))
}

async fn thread_ids<W: World>(w: &W, adapter: &str, id: &str) -> Vec<String> {
    w.repo()
        .thread(id)
        .await
        .unwrap_or_else(|e| panic!("{adapter}: thread {id:?}: {e:?}"))
        .into_iter()
        .map(|m| m.id)
        .collect()
}

async fn archived<W: World>(w: &W, adapter: &str, id: &str) -> bool {
    get(w, adapter, id)
        .await
        .unwrap_or_else(|| panic!("{adapter}: {id} is held"))
        .archived_at
        .is_some()
}

// ----- cases ---------------------------------------------------------------

/// The inbox lists one recipient's messages newest first; messages sent
/// in the same instant — to the microsecond the column keeps, so a
/// nanosecond apart is the same instant — list in BYTE order of id
/// (`suite-B` < `suite-a-z` < `suite-ab`, which a locale orders the other
/// way round). An archived message is left out unless asked for.
async fn an_inbox_lists_newest_first_ties_in_byte_order_of_id_and_leaves_archived_out_unless_asked<
    W: World,
>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &message("suite-old", 1), 1).await;
    for id in ["suite-ab", "suite-B", "suite-a-z"] {
        send(w, adapter, &message(id, 3), 2).await;
    }
    // A nanosecond later than suite-x1, in the same microsecond.
    let mut x0 = message("suite-x0", 5);
    x0.sent_at = Utc.timestamp_opt(1_790_000_005, 123_456_999).unwrap();
    send(w, adapter, &message("suite-x1", 5), 3).await;
    send(w, adapter, &x0, 3).await;
    send(w, adapter, &message("suite-gone", 4), 4).await;
    let mut elsewhere = message("suite-elsewhere", 6);
    elsewhere.recipient_id = "suite-other".into();
    send(w, adapter, &elsewhere, 4).await;
    w.repo()
        .archive_message("suite-gone", at(7), &stamp(7))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: archive: {e:?}"));

    assert_eq!(
        inbox_ids(w, adapter, "suite-r", false).await,
        [
            "suite-x0",
            "suite-x1",
            "suite-B",
            "suite-a-z",
            "suite-ab",
            "suite-old"
        ],
        "{adapter}"
    );
    assert_eq!(
        inbox_ids(w, adapter, "suite-r", true).await,
        [
            "suite-x0",
            "suite-x1",
            "suite-gone",
            "suite-B",
            "suite-a-z",
            "suite-ab",
            "suite-old"
        ],
        "{adapter}"
    );
    assert_eq!(
        inbox_ids(w, adapter, "suite-nobody", true).await,
        Vec::<String>::new(),
        "{adapter}"
    );
}

/// An inbox read is ONE PAGE (backlog 74da899d): the list above narrowed
/// by kind and by unread, `limit` rows of it after `offset`, and the
/// count of every row the narrowing matches — the page's size never, so
/// a reader holding a full page can tell it from the whole. An offset
/// past the end is an empty page that still says the total; a kind
/// nobody sent, or one holding a NUL, is an empty narrowing.
async fn an_inbox_page_is_a_slice_of_the_narrowed_list_and_says_how_many_the_narrowing_holds<
    W: World,
>(
    w: &W,
    adapter: &str,
) {
    let read = |id: &str, kind: &str, secs: i64| {
        let mut m = about(id, kind, "/jobs/x", secs);
        m.read_at = Some(at(secs));
        m
    };
    send(w, adapter, &message("suite-d1", 6), 1).await;
    send(w, adapter, &about("suite-s1", "signal", "/jobs/x", 5), 1).await;
    send(w, adapter, &read("suite-d2", "direct", 4), 1).await;
    send(w, adapter, &read("suite-s2", "signal", 3), 1).await;
    send(w, adapter, &message("suite-d3", 2), 1).await;
    send(w, adapter, &message("suite-gone", 1), 1).await;
    let mut elsewhere = message("suite-elsewhere", 7);
    elsewhere.recipient_id = "suite-other".into();
    send(w, adapter, &elsewhere, 1).await;
    w.repo()
        .archive_message("suite-gone", at(8), &stamp(8))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: archive: {e:?}"));

    let q = |kind: Option<&str>, unread_only: bool, limit: u32, offset: u32| InboxQuery {
        include_archived: false,
        kind: kind.map(String::from),
        unread_only,
        limit,
        offset,
    };
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    for (query, want, total) in [
        (q(None, false, 2, 0), ids(&["suite-d1", "suite-s1"]), 5),
        (q(None, false, 2, 2), ids(&["suite-d2", "suite-s2"]), 5),
        (q(None, false, 2, 4), ids(&["suite-d3"]), 5),
        (q(None, false, 2, 9), ids(&[]), 5),
        (
            q(Some("direct"), false, 10, 0),
            ids(&["suite-d1", "suite-d2", "suite-d3"]),
            3,
        ),
        (
            q(None, true, 10, 0),
            ids(&["suite-d1", "suite-s1", "suite-d3"]),
            3,
        ),
        (
            q(Some("direct"), true, 10, 0),
            ids(&["suite-d1", "suite-d3"]),
            2,
        ),
        (q(Some("direct"), true, 1, 1), ids(&["suite-d3"]), 2),
        (q(Some("signal"), true, 1, 0), ids(&["suite-s1"]), 1),
        (q(Some("suite-kind"), false, 10, 0), ids(&[]), 0),
        (q(Some("direct\0"), false, 10, 0), ids(&[]), 0),
        (
            InboxQuery {
                include_archived: true,
                ..q(Some("direct"), true, 10, 0)
            },
            ids(&["suite-d1", "suite-d3", "suite-gone"]),
            3,
        ),
    ] {
        assert_eq!(
            page_of(w, adapter, "suite-r", &query).await,
            (want, total),
            "{adapter}: {query:?}"
        );
    }
}

/// The counts are the rows an unnarrowed read reads, per kind — all and
/// unread — in BYTE order of kind (`Suite-K` before `direct`, which a
/// locale orders the other way round). Archived rows count only when
/// asked for, as the read leaves them out; a recipient with no messages
/// has no kinds.
async fn the_inbox_counts_each_kind_all_and_unread_in_byte_order_of_kind<W: World>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &message("suite-d1", 1), 1).await;
    let mut d2 = message("suite-d2", 2);
    d2.read_at = Some(at(3));
    send(w, adapter, &d2, 1).await;
    send(w, adapter, &about("suite-s1", "signal", "/jobs/x", 4), 1).await;
    let mut k = about("suite-k", "Suite-K", "/jobs/x", 5);
    k.read_at = Some(at(6));
    send(w, adapter, &k, 1).await;
    send(w, adapter, &message("suite-gone", 7), 1).await;
    let mut elsewhere = about("suite-elsewhere", "signal", "/jobs/x", 8);
    elsewhere.recipient_id = "suite-other".into();
    send(w, adapter, &elsewhere, 1).await;
    w.repo()
        .archive_message("suite-gone", at(9), &stamp(9))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: archive: {e:?}"));

    let c = |k: &str, all: u64, unread: u64| (k.to_string(), all, unread);
    assert_eq!(
        counts(w, adapter, "suite-r", false).await,
        [c("Suite-K", 1, 0), c("direct", 2, 1), c("signal", 1, 1)],
        "{adapter}"
    );
    assert_eq!(
        counts(w, adapter, "suite-r", true).await,
        [c("Suite-K", 1, 0), c("direct", 3, 2), c("signal", 1, 1)],
        "{adapter}"
    );
    assert_eq!(
        counts(w, adapter, "suite-nobody", true).await,
        vec![],
        "{adapter}"
    );
    assert_eq!(
        counts(w, adapter, "suite-r\0", true).await,
        vec![],
        "{adapter}"
    );
}

/// A message reads back whole — its entity, path, kind and reply — with
/// every time to the microsecond the column keeps. An id nobody sent is
/// `None`, not an error.
async fn a_message_reads_back_as_sent_to_the_microsecond_and_a_stranger_is_none<W: World>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &message("suite-root", 1), 1).await;
    let mut whole = about("suite-whole", "signal", "/jobs/suite-job/steps/s1", 2);
    whole.reply_to = Some("suite-root".into());
    send(w, adapter, &whole, 2).await;
    assert_eq!(
        get(w, adapter, "suite-whole").await,
        Some(read_back(&whole)),
        "{adapter}"
    );
    assert_eq!(
        get(w, adapter, "suite-whole").await.map(|m| m.sent_at),
        Some(kept(2)),
        "{adapter}"
    );
    assert_eq!(get(w, adapter, "suite-stranger").await, None, "{adapter}");
}

/// A send records `messages.message.sent` carrying the message AS SENT.
/// A second send on a held id is the redelivery the port names: Ok,
/// nothing changed, nothing recorded.
async fn a_send_records_the_message_as_sent_and_a_redelivery_changes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let first = about("suite-m", "signal", "/jobs/suite-job", 1);
    send(w, adapter, &first, 1).await;
    let mut again = message("suite-m", 2);
    again.subject = "Second".into();
    send(w, adapter, &again, 2).await;
    assert_eq!(
        get(w, adapter, "suite-m").await,
        Some(read_back(&first)),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![sent_fact(1, &first)], "{adapter}");
}

/// A message sent already read or already archived keeps that state,
/// as its sent fact (the full row state) says it was.
async fn a_message_sent_read_or_archived_keeps_that_state<W: World>(w: &W, adapter: &str) {
    let mut done = message("suite-done", 1);
    done.read_at = Some(at(2));
    done.archived_at = Some(at(3));
    send(w, adapter, &done, 4).await;
    assert_eq!(
        get(w, adapter, "suite-done").await,
        Some(read_back(&done)),
        "{adapter}"
    );
    assert_eq!(w.facts().await, vec![sent_fact(4, &done)], "{adapter}");
}

/// The unread count is the recipient's unread, unarchived messages,
/// narrowed to one kind when asked — an archived direct keeps its kind
/// and still does not count under `direct`.
async fn the_unread_count_leaves_out_read_and_archived_and_narrows_by_kind<W: World>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &message("suite-d1", 1), 1).await;
    send(w, adapter, &message("suite-d2", 2), 1).await;
    send(w, adapter, &about("suite-s1", "signal", "/jobs/x", 3), 1).await;
    let mut read = message("suite-read", 4);
    read.read_at = Some(at(5));
    send(w, adapter, &read, 1).await;
    let mut other = message("suite-other", 4);
    other.recipient_id = "suite-other".into();
    send(w, adapter, &other, 1).await;
    w.repo()
        .archive_message("suite-d2", at(6), &stamp(6))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: archive: {e:?}"));

    assert_eq!(unread(w, adapter, "suite-r", None).await, 2, "{adapter}");
    assert_eq!(
        unread(w, adapter, "suite-r", Some("direct")).await,
        1,
        "{adapter}"
    );
    assert_eq!(
        unread(w, adapter, "suite-r", Some("signal")).await,
        1,
        "{adapter}"
    );
    assert_eq!(
        unread(w, adapter, "suite-r", Some("suite-kind")).await,
        0,
        "{adapter}"
    );
    assert_eq!(
        unread(w, adapter, "suite-nobody", None).await,
        0,
        "{adapter}"
    );
}

/// A mark sets `read_at` (to the microsecond) and records
/// `messages.message.read` carrying the time given. A repeat mark of a
/// read message is Ok, records nothing, and the first time stands
/// (backlog 624e92eb). A phantom id is Ok and records nothing.
async fn a_mark_read_sets_the_time_and_records_it_and_a_phantom_records_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let m = message("suite-m", 1);
    send(w, adapter, &m, 1).await;
    for secs in [2, 4] {
        w.repo()
            .mark_read("suite-m", at(secs), &stamp(secs))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: mark_read at {secs}: {e:?}"));
    }
    w.repo()
        .mark_read("suite-phantom", at(3), &stamp(3))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: mark_read phantom: {e:?}"));
    assert_eq!(
        get(w, adapter, "suite-m").await.and_then(|m| m.read_at),
        Some(kept(2)),
        "{adapter}"
    );
    assert_eq!(unread(w, adapter, "suite-r", None).await, 0, "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![
            sent_fact(1, &m),
            fact(2, MESSAGE_READ, json!({"id": "suite-m", "read_at": at(2)})),
        ],
        "{adapter}"
    );
}

/// A delete removes the message and records `messages.message.deleted`
/// carrying the id and the instant. A reply to it stays, its `reply_to`
/// cleared (the column's ON DELETE SET NULL), so its thread is itself
/// alone. An id nobody sent is refused `NotFound` naming it, and records
/// nothing. A deleted id may be sent afresh.
async fn a_delete_removes_the_message_clears_its_replies_pointer_and_records_it<W: World>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &message("suite-root", 1), 1).await;
    let mut reply = message("suite-reply", 2);
    reply.reply_to = Some("suite-root".into());
    send(w, adapter, &reply, 2).await;
    w.repo()
        .delete_message("suite-root", at(3), &stamp(3))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: delete: {e:?}"));
    match w
        .repo()
        .delete_message("suite-stranger", at(4), &stamp(4))
        .await
    {
        Err(MessageError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: delete a stranger: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-root").await, None, "{adapter}");
    assert_eq!(
        get(w, adapter, "suite-reply").await.map(|m| m.reply_to),
        Some(None),
        "{adapter}: the reply's pointer is cleared"
    );
    assert_eq!(
        thread_ids(w, adapter, "suite-reply").await,
        ["suite-reply"],
        "{adapter}"
    );
    assert_eq!(
        w.facts().await.last(),
        Some(&fact(
            3,
            MESSAGE_DELETED,
            json!({"id": "suite-root", "deleted_at": at(3)})
        )),
        "{adapter}"
    );
    send(w, adapter, &message("suite-root", 5), 5).await;
    assert_eq!(
        get(w, adapter, "suite-root").await,
        Some(read_back(&message("suite-root", 5))),
        "{adapter}"
    );
}

/// An archive sets `archived_at` beside the kind and records
/// `messages.message.archived`. A repeat is Ok, records nothing, and the
/// first time stands. An id nobody sent is refused `NotFound` naming it.
async fn an_archive_keeps_the_kind_and_a_repeat_is_a_no_op<W: World>(w: &W, adapter: &str) {
    let m = about("suite-s", "signal", "/jobs/x", 1);
    send(w, adapter, &m, 1).await;
    for secs in [2, 3] {
        w.repo()
            .archive_message("suite-s", at(secs), &stamp(secs))
            .await
            .unwrap_or_else(|e| panic!("{adapter}: archive at {secs}: {e:?}"));
    }
    match w
        .repo()
        .archive_message("suite-stranger", at(4), &stamp(4))
        .await
    {
        Err(MessageError::NotFound(m)) => assert!(m.contains("suite-stranger"), "{adapter}: {m}"),
        other => panic!("{adapter}: archive a stranger: {other:?}"),
    }
    let read = get(w, adapter, "suite-s").await.expect("held");
    assert_eq!(read.kind.as_str(), "signal", "{adapter}");
    assert_eq!(read.archived_at, Some(kept(2)), "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![sent_fact(1, &m), archived_fact(2, "suite-s", false)],
        "{adapter}"
    );
}

/// `expire_signals_under` archives every UNREAD, unarchived `signal`
/// whose path STARTS WITH the prefix — read literally, so `_` and `%` are
/// characters, not wildcards — and records one fact per row moved, in
/// BYTE order of id. A second pass moves nothing.
async fn expiring_signals_moves_only_unread_signals_under_a_literal_prefix<W: World>(
    w: &W,
    adapter: &str,
) {
    let job = "/jobs/suite_1";
    for m in [
        about("suite-ab", "signal", job, 1),
        about("suite-B", "signal", &format!("{job}/steps/s1"), 1),
        about("suite-a-z", "signal", job, 1),
        about("suite-direct", "direct", job, 1),
        about("suite-wild", "signal", "/jobs/suiteX1", 1),
        about("suite-sibling", "signal", "/jobs/suite_10", 1),
        message("suite-none", 1),
    ] {
        send(w, adapter, &m, 1).await;
    }
    let mut read = about("suite-read", "signal", job, 1);
    read.read_at = Some(at(1));
    send(w, adapter, &read, 1).await;
    let before = w.facts().await;

    let n = w
        .repo()
        .expire_signals_under(job, at(2), &stamp(2))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: expire: {e:?}"));
    // `/jobs/suite_1` is a prefix of `/jobs/suite_10`, as it says.
    assert_eq!(n, 4, "{adapter}");
    for (id, moved) in [
        ("suite-ab", true),
        ("suite-B", true),
        ("suite-a-z", true),
        ("suite-sibling", true),
        ("suite-direct", false),
        ("suite-wild", false),
        ("suite-none", false),
        ("suite-read", false),
    ] {
        assert_eq!(archived(w, adapter, id).await, moved, "{adapter}: {id}");
    }
    let wild = w
        .repo()
        .expire_signals_under("/jobs/%", at(3), &stamp(3))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: expire %: {e:?}"));
    assert_eq!(wild, 0, "{adapter}: % is a character");
    let again = w
        .repo()
        .expire_signals_under(job, at(4), &stamp(4))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: expire again: {e:?}"));
    assert_eq!(again, 0, "{adapter}");

    let mut want = before;
    for id in ["suite-B", "suite-a-z", "suite-ab", "suite-sibling"] {
        want.push(archived_fact(2, id, true));
    }
    assert_eq!(w.facts().await, want, "{adapter}");
}

/// `expire_notices_under` archives every UNREAD, unarchived message of
/// ANY kind whose path starts with the path prefix and whose id starts
/// with the id prefix — both read literally — recording one fact per row
/// in BYTE order of id.
async fn expiring_notices_moves_only_unread_ids_under_both_literal_prefixes<W: World>(
    w: &W,
    adapter: &str,
) {
    let step = "/jobs/j/steps/s_1";
    for m in [
        about("suite-n_B", "direct", step, 1),
        about("suite-n_a", "signal", step, 1),
        about("suite-nXa", "direct", step, 1),
        about("suite-human", "direct", step, 1),
        about("suite-n_other", "direct", "/jobs/j/steps/sX1", 1),
    ] {
        send(w, adapter, &m, 1).await;
    }
    let mut read = about("suite-n_read", "direct", step, 1);
    read.read_at = Some(at(1));
    send(w, adapter, &read, 1).await;
    let before = w.facts().await;

    let n = w
        .repo()
        .expire_notices_under(step, "suite-n_", at(2), &stamp(2))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: expire notices: {e:?}"));
    assert_eq!(n, 2, "{adapter}");
    for (id, moved) in [
        ("suite-n_B", true),
        ("suite-n_a", true),
        ("suite-nXa", false),
        ("suite-human", false),
        ("suite-n_other", false),
        ("suite-n_read", false),
    ] {
        assert_eq!(archived(w, adapter, id).await, moved, "{adapter}: {id}");
    }
    assert_eq!(
        get(w, adapter, "suite-n_B").await.map(|m| m.kind),
        Some(MessageKind::direct()),
        "{adapter}"
    );
    let mut want = before;
    for id in ["suite-n_B", "suite-n_a"] {
        want.push(archived_fact(2, id, true));
    }
    assert_eq!(w.facts().await, want, "{adapter}");
}

/// A thread is the message and its DIRECT replies, oldest first, ties in
/// BYTE order of id. A reply's reply is not in its grandparent's thread.
/// An id nobody sent has an empty thread.
async fn a_thread_is_the_root_and_its_direct_replies_oldest_first<W: World>(w: &W, adapter: &str) {
    send(w, adapter, &message("suite-root", 1), 1).await;
    for (id, secs) in [
        ("suite-ab", 3),
        ("suite-B", 3),
        ("suite-a-z", 3),
        ("suite-first", 2),
    ] {
        let mut r = message(id, secs);
        r.reply_to = Some("suite-root".into());
        send(w, adapter, &r, 1).await;
    }
    let mut deep = message("suite-deep", 4);
    deep.reply_to = Some("suite-first".into());
    send(w, adapter, &deep, 1).await;
    assert_eq!(
        thread_ids(w, adapter, "suite-root").await,
        [
            "suite-root",
            "suite-first",
            "suite-B",
            "suite-a-z",
            "suite-ab"
        ],
        "{adapter}"
    );
    assert_eq!(
        thread_ids(w, adapter, "suite-first").await,
        ["suite-first", "suite-deep"],
        "{adapter}"
    );
    assert_eq!(
        thread_ids(w, adapter, "suite-stranger").await,
        Vec::<String>::new(),
        "{adapter}"
    );
}

/// A reply to a message nobody sent is refused `BadRequest` naming
/// `reply_to`, writes nothing and records nothing.
async fn a_reply_to_a_message_nobody_sent_is_refused_naming_reply_to<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut orphan = message("suite-orphan", 1);
    orphan.reply_to = Some("suite-stranger".into());
    match w.repo().send(&orphan, &stamp(1)).await {
        Err(MessageError::BadRequest(m)) => assert!(m.contains("reply_to"), "{adapter}: {m}"),
        other => panic!("{adapter}: an orphan reply: {other:?}"),
    }
    assert_eq!(get(w, adapter, "suite-orphan").await, None, "{adapter}");
    assert_eq!(w.facts().await, vec![], "{adapter}");
}

/// A NUL byte cannot be stored in TEXT. A send carrying one in any field
/// is refused `BadRequest` naming the field and writes nothing. Every
/// read, mark, delete, archive or expire keyed by one is the miss it is.
async fn a_nul_byte_is_refused_naming_its_field_and_misses_on_every_key<W: World>(
    w: &W,
    adapter: &str,
) {
    send(w, adapter, &about("suite-held", "signal", "/jobs/x", 1), 1).await;
    let before = w.facts().await;
    let nul = |f: &dyn Fn(&mut Message)| {
        let mut m = about("suite-new", "signal", "/jobs/x", 2);
        f(&mut m);
        m
    };
    let cases = [
        ("id", nul(&|m| m.id = "suite-n\0".into())),
        ("sender_id", nul(&|m| m.sender_id = "s\0".into())),
        ("recipient_id", nul(&|m| m.recipient_id = "r\0".into())),
        ("subject", nul(&|m| m.subject = "s\0".into())),
        ("body", nul(&|m| m.body = "b\0".into())),
        ("kind", nul(&|m| m.kind = MessageKind::new("k\0"))),
        (
            "entity_type",
            nul(&|m| m.entity_ref.as_mut().unwrap().entity_type = "t\0".into()),
        ),
        (
            "entity_id",
            nul(&|m| m.entity_ref.as_mut().unwrap().entity_id = "i\0".into()),
        ),
        (
            "entity_path",
            nul(&|m| m.entity_ref.as_mut().unwrap().entity_path = Some("/p\0".into())),
        ),
        (
            "reply_to",
            nul(&|m| m.reply_to = Some("suite-held\0".into())),
        ),
    ];
    for (field, m) in &cases {
        match w.repo().send(m, &stamp(2)).await {
            Err(MessageError::BadRequest(msg)) => {
                assert!(msg.contains(field), "{adapter}: {msg}")
            }
            other => panic!("{adapter}: a NUL {field}: {other:?}"),
        }
    }
    let key = "suite-held\0";
    assert_eq!(get(w, adapter, key).await, None, "{adapter}");
    assert_eq!(
        inbox_ids(w, adapter, "suite-r\0", true).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    assert_eq!(unread(w, adapter, "suite-r\0", None).await, 0, "{adapter}");
    assert_eq!(
        unread(w, adapter, "suite-r", Some("signal\0")).await,
        0,
        "{adapter}"
    );
    assert_eq!(
        thread_ids(w, adapter, key).await,
        Vec::<String>::new(),
        "{adapter}"
    );
    w.repo()
        .mark_read(key, at(3), &stamp(3))
        .await
        .unwrap_or_else(|e| panic!("{adapter}: mark_read a NUL: {e:?}"));
    match w.repo().delete_message(key, at(3), &stamp(3)).await {
        Err(MessageError::NotFound(_)) => {}
        other => panic!("{adapter}: delete a NUL: {other:?}"),
    }
    match w.repo().archive_message(key, at(3), &stamp(3)).await {
        Err(MessageError::NotFound(_)) => {}
        other => panic!("{adapter}: archive a NUL: {other:?}"),
    }
    for (path, id) in [("/jobs/x\0", None), ("/jobs/x", Some("suite-\0"))] {
        let n = match id {
            None => w.repo().expire_signals_under(path, at(3), &stamp(3)).await,
            Some(id) => {
                w.repo()
                    .expire_notices_under(path, id, at(3), &stamp(3))
                    .await
            }
        }
        .unwrap_or_else(|e| panic!("{adapter}: expire {path:?} {id:?}: {e:?}"));
        assert_eq!(n, 0, "{adapter}");
    }
    assert_eq!(
        inbox_ids(w, adapter, "suite-r", true).await,
        ["suite-held"],
        "{adapter}"
    );
    assert!(!archived(w, adapter, "suite-held").await, "{adapter}");
    assert_eq!(w.facts().await, before, "{adapter}: nothing recorded");
}
