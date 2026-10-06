//! The deterministic fact id — pure, so it compiles without the
//! `postgres` feature. It lived in `events` (postgres-only) until the
//! products adapters-agree suite (backlog be459ab9) needed the in-memory
//! double to answer the same canonical fact id Postgres answers.

use uuid::Uuid;

/// Namespace UUID for deterministic fact ids — UUIDv5 over the natural
/// key `(kind, source_table, source_id)`, the same identity the unique
/// index enforces.
pub const FACT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x71004230_67f0_5fac_70ad_d11a51141a6c);

/// The one fact-id derivation, shared by every live writer (via
/// `record_fact_in_tx` and the cross-crate insert paths in
/// boss-inventory / boss-commerce / boss-products) AND the audit-log
/// rebuild. A rebuilt fact therefore carries the SAME id as its live
/// twin: journal entries keyed `(fact_id, rule_version_id)` compare
/// across live and replay, and any reference to a fact id survives a
/// rebuild. The previous split — live writers minting random v4, the
/// rebuild deriving v5 over `(event_id, fact_kind)` — made the deep
/// replay-check's entry diff structurally unmatchable for live-written
/// facts.
pub fn deterministic_fact_id(kind: &str, source_table: &str, source_id: &str) -> Uuid {
    let mut input = Vec::with_capacity(kind.len() + source_table.len() + source_id.len() + 2);
    input.extend_from_slice(kind.as_bytes());
    input.push(0);
    input.extend_from_slice(source_table.as_bytes());
    input.push(0);
    input.extend_from_slice(source_id.as_bytes());
    Uuid::new_v5(&FACT_ID_NAMESPACE, &input)
}
