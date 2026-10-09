//! A passkey ticket for a test that completes a step reserved for a
//! person.
//!
//! WHY THIS IS SHARED. Since 2026-10-07 a `human_only` step leaves its
//! open states only on a passkey (item 570c66e9,
//! `human_only_by_passkey_20261007`), so every fixture that walks a
//! protocol past a human-only step — a flight's `decide`, a deposit
//! key's `scope` — has to present what the gateway would: a ticket it
//! signed for that step, that shape and that person. One definition, so
//! a fixture cannot mint a ticket shaped differently from the one the
//! jobs API verifies.
//!
//! The key is a FIXTURE value, never the deployment's: the real one
//! lives in the boss-session-key Secret and appears in no repo,
//! transcript or test. A jobs API under test is handed it as
//! `PresenceKey::fixed(TEST_GATEWAY_KEY.to_vec())`.

use boss_core::job::Step;
use boss_core::presence::{HEADER, PresenceTicket, now_epoch};

/// The key the "gateway" of a test signs with.
pub const TEST_GATEWAY_KEY: &[u8] = b"boss-testing-gateway-key-0123456789abcdef";

/// The header a verified ticket rides to the jobs API in.
pub const PRESENCE_HEADER: &str = HEADER;

/// A ticket as the gateway's `assert_finish` mints it for `person` over
/// `step` AS IT STANDS — read the step after its last metadata write,
/// because the ticket binds its shape.
pub fn passkey_ticket(step: &Step, person: &str) -> String {
    // A nonce stamps its step once, so each ticket carries its own.
    static MINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = MINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    PresenceTicket {
        i: person.to_string(),
        s: step.id.to_string(),
        h: step.shape_hash(),
        n: format!("test-nonce-{}-{n}", std::process::id()),
        e: now_epoch() + 60,
    }
    .encode(TEST_GATEWAY_KEY)
    .unwrap_or_default()
}
