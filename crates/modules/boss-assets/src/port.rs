//! Hexagonal port: `AssetsRepository` defines what the domain needs from
//! persistence. Adapters (in-memory for tests, Postgres for prod) implement
//! this trait.

use async_trait::async_trait;

use crate::types::{AssetCurrentState, AssetEvent, AssetEventKind, AssetId, AssetsSummary};
use boss_core::actor::ActorId;

#[derive(Debug, thiserror::Error)]
pub enum AssetsError {
    #[error("unknown asset: {0}")]
    UnknownSystem(AssetId),
    #[error("duplicate event id: {0}")]
    DuplicateEvent(String),
    /// The event names a catalog model (`sku`) the store does not hold.
    /// Until the adapters-agree suite (backlog be459ab9) Postgres
    /// answered its foreign-key error as `Storage` (a 500) and the
    /// double stored the event.
    #[error("unknown asset model: {0}")]
    UnknownModel(String),
    /// A write or a page the store cannot hold: a NUL byte, a negative
    /// limit or offset. Refused by both adapters, naming the field.
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("storage failure: {0}")]
    Storage(String),
}

/// The catalog model an event names, if it names one — the `sku` a
/// `Received` or an `Identified` carries. Both adapters refuse an event
/// whose model the store does not hold, judged on the EVENT, so a
/// refusal does not depend on whether the projection happens to take
/// the sku up (backlog be459ab9).
pub fn model_named(kind: &AssetEventKind) -> Option<&str> {
    match kind {
        AssetEventKind::Received { sku: Some(s), .. } | AssetEventKind::Identified { sku: s } => {
            Some(s)
        }
        _ => None,
    }
}

/// Refuse an event Postgres cannot store: a NUL byte in its id, its
/// asset id, its actor or any string of its payload (TEXT and JSONB
/// reject one). Both adapters call this before writing, so the refusal
/// is `Invalid` naming the field on each — until the adapters-agree
/// suite (backlog be459ab9) Postgres answered with its encoding error
/// as `Storage` (a 500) and the double stored the byte.
pub fn refuse_nul_in_event(event: &AssetEvent) -> Result<(), AssetsError> {
    let nul = |field: &str| {
        Err(AssetsError::Invalid(format!(
            "{field} holds a NUL byte, which cannot be stored"
        )))
    };
    if event.id.0.contains('\0') {
        return nul("id");
    }
    if event.asset_id.0.contains('\0') {
        return nul("asset_id");
    }
    if event.actor_id.to_string().contains('\0') {
        return nul("actor_id");
    }
    let payload =
        serde_json::to_value(&event.kind).map_err(|e| AssetsError::Storage(e.to_string()))?;
    if let Some(fields) = payload.as_object()
        && let Some((field, _)) = fields
            .iter()
            .find(|(k, v)| k.contains('\0') || v.as_str().is_some_and(|s| s.contains('\0')))
    {
        return nul(field);
    }
    Ok(())
}

/// Refuse an event whose actor's text form parses back as ANOTHER actor
/// (`Human("system")`, a human id holding a colon — see
/// `ActorId::text_form_round_trips`). Postgres stores the text form and
/// the double the value, so until backlog b8099caf the two read such an
/// event back crediting different actors. Only an actor built in Rust
/// can be one: an id that came off the wire was parsed from its text.
pub fn refuse_an_actor_that_reads_back_as_another(event: &AssetEvent) -> Result<(), AssetsError> {
    if event.actor_id.text_form_round_trips() {
        return Ok(());
    }
    Err(AssetsError::Invalid(format!(
        "actor_id {:?} is written {:?}, which reads back as {:?}",
        event.actor_id,
        event.actor_id.to_string(),
        event.actor_id.to_string().parse::<ActorId>(),
    )))
}

/// Refuse a page Postgres cannot read: a negative limit or offset (its
/// LIMIT and OFFSET reject one, answered as `Storage` until backlog
/// be459ab9, while the double wrapped it to a huge unsigned number).
pub fn refuse_negative_page(limit: i64, offset: i64) -> Result<(), AssetsError> {
    if limit < 0 {
        return Err(AssetsError::Invalid(format!("limit {limit} is negative")));
    }
    if offset < 0 {
        return Err(AssetsError::Invalid(format!("offset {offset} is negative")));
    }
    Ok(())
}

/// Result of a `batch_append` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchAppendStats {
    pub inserted: u64,
    pub duplicates: u64,
}

/// Persistence port for the assets event log + current-state projection.
///
/// Implementations MUST maintain the invariant that the event log is
/// append-only: once stored, an event's payload never changes. The
/// current-state projection is either stored alongside (and updated as
/// events are appended) or computed on demand from the log.
#[async_trait]
pub trait AssetsRepository: Send + Sync {
    /// Append one event to a asset's log. Creates the asset's log on
    /// first event. Refuses, in this order and storing nothing: `Invalid`
    /// for a NUL byte (`refuse_nul_in_event`); `DuplicateEvent` if the
    /// event id already exists, so a redelivery is a duplicate whatever
    /// it names; `UnknownModel` if the event names a catalog model the
    /// store does not hold (`model_named`).
    async fn append(&self, event: AssetEvent) -> Result<(), AssetsError>;

    /// Append a batch of events. Implementations are free to choose a
    /// faster bulk path; the contract is "same observable result as
    /// calling `append` once per event in the input order, except that
    /// duplicates do not error out — they're counted and skipped" — a
    /// second copy of an id WITHIN the batch included: the first copy is
    /// stored and applied, the second counted. It is all or nothing: a
    /// refusal (`UnknownModel`, `Invalid`) stores none of the batch.
    ///
    /// Default implementation loops `append` for adapters that don't
    /// have a real bulk path; it is NOT all or nothing, so both adapters
    /// here override it.
    async fn batch_append(&self, events: Vec<AssetEvent>) -> Result<BatchAppendStats, AssetsError> {
        let mut stats = BatchAppendStats::default();
        for event in events {
            match self.append(event).await {
                Ok(()) => stats.inserted += 1,
                Err(AssetsError::DuplicateEvent(_)) => stats.duplicates += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(stats)
    }

    /// Full event log for a asset, chronological (oldest first; one
    /// day's events in byte order of id — the order the projection
    /// folds them in). Returns an empty Vec if the asset is unknown.
    async fn events_for(&self, asset_id: &AssetId) -> Result<Vec<AssetEvent>, AssetsError>;

    /// Current state for a asset, or None if the asset is unknown.
    async fn current_state(
        &self,
        asset_id: &AssetId,
    ) -> Result<Option<AssetCurrentState>, AssetsError>;

    /// All known asset ids, unordered.
    async fn all_asset_ids(&self) -> Result<Vec<AssetId>, AssetsError>;

    /// Return a page of asset ids, in byte order, with total count. A
    /// negative limit or offset is refused `Invalid`.
    async fn list_asset_ids(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AssetId>, i64), AssetsError>;

    /// Return a page of asset summaries with total count, newest
    /// `last_event_at` first and one day's assets in byte order of id. A
    /// negative limit or offset is refused `Invalid`.
    ///
    /// `account_id` scopes the result to assets currently HELD by
    /// (holder_kind='account', holder_id=account_id) — Q5's typed
    /// pair; location-held equipment never matches. Owned by
    /// (or last associated with) one account — used by the unified
    /// account-detail view to render the Devices panel without
    /// pulling every assets row across the wire.
    async fn list_assets(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<AssetCurrentState>, i64), AssetsError>;

    /// Count open service Jobs associated with a given account: the
    /// open tickets of every asset the account HOLDS that is not
    /// decommissioned.
    ///
    /// Used by cross-service guards (e.g., the account delete path in
    /// boss-people) to reject destructive operations while assets at
    /// that account still have unresolved service work.
    async fn open_ticket_count_for_account(&self, account_id: &str) -> Result<u64, AssetsError>;

    /// Count assets in any active lifecycle phase (not
    /// decommissioned) that reference the given catalog SKU.
    ///
    /// Used by the boss-catalog asset model delete guard to refuse
    /// deleting a catalog entry while real assets in custody or in
    /// the field still depend on it.
    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsError>;

    /// Aggregated assets summary for dashboards. SQL-aggregated server-
    /// side so the Assets list kanban never has to download the full
    /// asset ids list to compute phase distribution.
    ///
    /// `today` anchors the warranty-expiring-30d count. The HTTP
    /// handler sources it from ClockClient so the count respects
    /// sim-time.
    ///
    /// `sku_counts` holds every model with an asset not decommissioned,
    /// most assets first and ties in byte order of sku; an unidentified
    /// asset counts under no model. `open_tickets_total` sums the open
    /// tickets of every asset not decommissioned; `warranty_expiring_30d`
    /// counts every asset whose warranty runs through a day in
    /// `[today, today + 30)`. Until the adapters-agree suite (backlog
    /// be459ab9) the double answered both of those 0, and Postgres
    /// failed the whole summary on an unidentified asset's NULL sku.
    async fn assets_summary(&self, today: chrono::NaiveDate) -> Result<AssetsSummary, AssetsError>;
}
