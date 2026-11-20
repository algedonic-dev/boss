//! What the dispatcher carries to the next hop of a call it makes on an
//! event's behalf.
//!
//! The one helper lived in the matchmaker (`dispatcher.rs`) until that
//! was retired (backlog e1eaf72b); the rules runner's calls still need
//! it, so it moved here rather than going with it.

/// `x-sim-origin` for a downstream call, read from the task-local the
/// event loop set from the triggering event. Mirrors the handlers'
/// helper; kept local because this crate does not depend on them.
pub(crate) fn sim_origin_value() -> &'static str {
    if boss_core::sim_origin::is_in_sim_chain() {
        "true"
    } else {
        "false"
    }
}
