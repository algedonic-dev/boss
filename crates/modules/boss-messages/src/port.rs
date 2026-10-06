//! Port (trait) defining the message repository contract.

use async_trait::async_trait;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, DurationRound, Utc};

use crate::types::Message;

/// `at` as a TIMESTAMPTZ column keeps it: to the microsecond, the
/// nanoseconds below it truncated (as sqlx's bind truncates). The double
/// stores what the column stores (backlog be459ab9).
pub fn to_the_microsecond(at: DateTime<Utc>) -> DateTime<Utc> {
    at.duration_trunc(chrono::TimeDelta::microseconds(1))
        .unwrap_or(at)
}

#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    #[error("not found: {0}")]
    NotFound(String),
    /// A write the store cannot hold — a NUL byte in a field, or a
    /// reply to a message nobody sent. Both adapters refuse it before
    /// writing; until the adapters-agree suite (backlog be459ab9)
    /// Postgres answered with its encoding or foreign-key error as
    /// `Storage` (a 500) and the double stored it.
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// Refuse a message Postgres cannot store: a NUL byte in any of its
/// text fields (TEXT rejects one). Both adapters call this before
/// writing, so the refusal is `BadRequest` naming the field on each
/// (backlog be459ab9).
pub fn refuse_nul(msg: &Message) -> Result<(), MessageError> {
    let er = msg.entity_ref.as_ref();
    let fields: [(&str, Option<&str>); 10] = [
        ("id", Some(&msg.id)),
        ("sender_id", Some(&msg.sender_id)),
        ("recipient_id", Some(&msg.recipient_id)),
        ("subject", Some(&msg.subject)),
        ("body", Some(&msg.body)),
        ("kind", Some(msg.kind.as_str())),
        ("entity_type", er.map(|e| e.entity_type.as_str())),
        ("entity_id", er.map(|e| e.entity_id.as_str())),
        ("entity_path", er.and_then(|e| e.entity_path.as_deref())),
        ("reply_to", msg.reply_to.as_deref()),
    ];
    match fields
        .iter()
        .find(|(_, v)| v.is_some_and(|v| v.contains('\0')))
    {
        Some((f, _)) => Err(MessageError::BadRequest(format!(
            "{f} holds a NUL byte, which cannot be stored"
        ))),
        None => Ok(()),
    }
}

/// The refusal for a reply to a message nobody sent — the `reply_to`
/// column references `messages(id)` (backlog be459ab9).
pub fn no_such_parent(reply_to: &str) -> MessageError {
    MessageError::BadRequest(format!("reply_to names no message: {reply_to}"))
}

/// What one inbox read asks for: which of a recipient's messages, and
/// which page of them (backlog 74da899d, page audit 5477d9eb GAP 3).
/// The read was every row, unpaged, and the page filtered the lot in
/// the browser — 5,129 rows to one recipient in the audit window. Now
/// the narrowing is the store's and the answer is one page of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxQuery {
    /// Archived rows too (they have left the inbox; backlog 8578b91e).
    pub include_archived: bool,
    /// Only messages of this kind.
    pub kind: Option<String>,
    /// Only messages nobody has read.
    pub unread_only: bool,
    /// At most this many rows.
    pub limit: u32,
    /// After skipping this many, in the read's order.
    pub offset: u32,
}

impl InboxQuery {
    /// The first `limit` rows of the inbox, unnarrowed.
    pub fn first(limit: u32) -> Self {
        Self {
            include_archived: false,
            kind: None,
            unread_only: false,
            limit,
            offset: 0,
        }
    }
}

/// One page of an inbox read and how many rows its narrowing matches,
/// so `rows.len() < total` says there is more.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxPage {
    pub rows: Vec<Message>,
    pub total: u64,
}

/// How many of the rows an unnarrowed inbox read reads are of one kind,
/// and how many of those are unread. A page that shows one filtered page
/// still owes its header the whole inbox's numbers; per kind rather than
/// per named kind, because kinds are Class registry rows, not this
/// crate's list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KindCount {
    pub kind: String,
    pub all: u64,
    pub unread: u64,
}

/// OUTBOX (phase 2): every mutation records its domain event on the
/// transactional outbox INSIDE the adapter transaction via the stamp
/// (`boss_events::outbox::record_event_in_tx`); boss-event-relay
/// delivers to audit_log + NATS post-commit. Idempotency guards sit
/// AHEAD of the recording, so a collapsed replay records nothing.
#[async_trait]
pub trait MessageRepository: Send + Sync {
    /// A recipient's messages, newest first, messages sent in the same
    /// microsecond in BYTE order of id. Archived rows are left out
    /// unless `include_archived`: archiving is how a message leaves the
    /// inbox, and a read that still returned them handed the page a
    /// third kind it could not place — counted in All and Unread, drawn
    /// as a direct, offered Mark read (backlog 8578b91e, page audit
    /// 5477d9eb GAP 2). The archived rows are not deleted, so a reader
    /// that needs every row it ever sent (a tenant seed's idempotence
    /// check) asks for them.
    ///
    /// One PAGE of that list, narrowed by `query`, with the count of
    /// every row the narrowing matches (backlog 74da899d): `query.limit`
    /// rows at most, after `query.offset`, in the order above.
    async fn inbox(
        &self,
        recipient_id: &str,
        query: &InboxQuery,
    ) -> Result<InboxPage, MessageError>;
    /// The rows an unnarrowed inbox read reads (archived ones only when
    /// `include_archived`), counted per kind — all and unread — in BYTE
    /// order of kind. The sum of `all` is that read's `total`.
    async fn inbox_counts(
        &self,
        recipient_id: &str,
        include_archived: bool,
    ) -> Result<Vec<KindCount>, MessageError>;
    /// Unread messages for a recipient, optionally narrowed to one
    /// `kind`. The narrowing is what makes the count usable as a
    /// badge: an inbox holding 1,980 unread `signal` rows against 3
    /// unread `direct` ones renders the noise as a number unless the
    /// caller can ask the question the reader actually has, which is
    /// "is anything addressed to me?". Archived rows never count — the
    /// same inbox `inbox` returns, since the expire rule archives a
    /// signal without reading it — and that holds under a kind too:
    /// archiving keeps the kind (backlog 9bda9726), so an archived
    /// direct must not reach the `Some("direct")` badge.
    async fn unread_count(
        &self,
        recipient_id: &str,
        kind: Option<&str>,
    ) -> Result<u32, MessageError>;
    async fn message_by_id(&self, id: &str) -> Result<Option<Message>, MessageError>;
    /// Mark a message read at the given timestamp. Caller picks the
    /// timestamp so the same value can be carried in the
    /// `messages.message.read` event payload — letting a rebuild
    /// reconstruct the projection's `read_at` exactly.
    /// Records `messages.message.read` (`{id, read_at}`) in-tx —
    /// only when the row actually updated (a phantom id records
    /// nothing). Idempotent: marking a read message read again is Ok,
    /// changes nothing and records nothing — the first `read_at`
    /// stands (backlog 624e92eb).
    async fn mark_read(
        &self,
        id: &str,
        read_at: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.sent` (full row state) in-tx — only
    /// when the INSERT actually inserted (a redelivered notification
    /// collapses on ON CONFLICT (id) and records nothing). The row is
    /// stored as sent, a `read_at` or `archived_at` included, every
    /// time to the microsecond the column keeps. A NUL byte in any
    /// field, or a `reply_to` naming no message, is refused
    /// `BadRequest` and writes nothing.
    async fn send(&self, msg: &Message, stamp: &EventStamp) -> Result<(), MessageError>;
    /// Records `messages.message.deleted` (`{id, deleted_at}`) in-tx
    /// after the row actually deleted.
    async fn delete_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Records `messages.message.archived` (`{id, archived_at}`)
    /// in-tx for each row that actually moved, in BYTE order of id; a
    /// signal already archived does not move again. The prefix is
    /// literal — a `_` or `%` in it is a character, not a wildcard.
    /// Archive every UNREAD `signal` whose `entity_path` starts with
    /// `path_prefix`, returning how many moved. The expiry path for
    /// notifications about work that has finished (David, 2026-08-14:
    /// "a way to automatically expire inbox messages for jobs that
    /// have moved past relevancy").
    ///
    /// Three narrowings, each deliberate:
    ///
    /// - **`signal` only.** A `direct` is one person addressing
    ///   another; it does not stop being addressed to you because a
    ///   job closed, and auto-clearing it would delete the one
    ///   category the inbox's needs-you filter is built on.
    /// - **Unread only.** A read message has already done its job and
    ///   rewriting it would churn the log for no reader.
    /// - **Archived, not deleted.** `read_at` would claim someone read
    ///   it, which is false; deletion would lose the record. Archiving
    ///   says exactly what happened — it stopped being relevant.
    async fn expire_signals_under(
        &self,
        path_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError>;

    /// Archive every UNREAD notice under `path_prefix` whose id starts
    /// with `id_prefix`, of ANY kind, returning how many moved. The
    /// retirement path for the notices the machine sends about a step
    /// (backlog 0b2bac00, 2026-09-23): the dispatcher's notifier sends
    /// an assignee's ready/assigned notice as a `direct` with the id
    /// `notify:{step}:{recipient}`, and `expire_signals_under` above
    /// never touches a direct, so 83 of David's 90 direct notices
    /// pointed at completed steps while his badge counted them.
    ///
    /// The id prefix is the narrowing, where the kind is above. A
    /// person's direct carries a minted id and a `done:` announcement
    /// its own prefix, so neither moves; the caller names the prefix
    /// (a dispatcher rule's argument), so which notices retire is rule
    /// data rather than a list in this crate. Unread only, and
    /// archived rather than read or deleted, for the reasons above.
    async fn expire_notices_under(
        &self,
        path_prefix: &str,
        id_prefix: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<u32, MessageError>;

    /// Take one message out of the inbox by setting its `archived_at`
    /// BESIDE its kind, and record `messages.message.archived`
    /// (`{id, archived_at}`) in-tx. Idempotent (backlog 9bda9726): a
    /// message already archived is left as it is — Ok, no event, the
    /// first archive's time standing. NotFound only for a missing id.
    async fn archive_message(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), MessageError>;
    /// Return all messages in a thread (the root message + its direct
    /// replies), oldest first, a tie in BYTE order of id.
    async fn thread(&self, message_id: &str) -> Result<Vec<Message>, MessageError>;
}
