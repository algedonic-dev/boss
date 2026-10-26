//! The record a hand-release leaves on a step, and the one question the
//! dispatcher asks of it: who was the step taken FROM?
//!
//! WHY (backlog ff0f0ff5, found by review 7adb1d44 of car b2a077eb). The
//! capability pick chooses among the agents serving a step's
//! `(role, model)` by the configured strategy over the step id; live is
//! `Spread`, `stable_hash(step_id) % n`. `boss step release` leaves the
//! step `ready` and unassigned, the dispatcher re-nominates any such
//! step, and the hash names the same agent every time — so a step its
//! holder handed back to the pool went straight back to that holder,
//! and handing it to the other agent needed `boss step nominate`, which
//! loses the race to the re-nomination when it runs after the release.
//!
//! THE RULE RIDES THE STEP, NOT AN AGENT'S MEMORY. The release already
//! writes one evidence object under [`KEY`] (`why`, `by`, `at`,
//! `from_run`); it now also writes [`FROM`], the holder the step was
//! taken from, and the dispatcher reads it through [`released_from`] to
//! leave that agent out of the pick while another agent serves the same
//! pair. One spelling here; two writers — `boss step release` and a
//! page's Release (`apps/web/src/steps/holder.ts`, pinned equal to
//! [`KEY`] and [`FROM`] by holder.test.ts) — and one reader (§9a).
//!
//! `from`, NOT `by`. `by` is whoever ran the verb, and a coordinator
//! releasing another agent's step is the ordinary case of that: the
//! agent the step must not bounce back to is the one that HELD it.

use serde_json::Value;

/// Where a hand-release's evidence lands on the freed step —
/// deliberately not the clock rule's `reclaimed`, so a reader can tell
/// which one freed the step.
pub const KEY: &str = "released";

/// The field of the [`KEY`] object naming the actor the step was held
/// by when it was released.
pub const FROM: &str = "from";

/// The jobs API's conditional matchmaking refusal, shared with the
/// dispatcher so a settled nomination cannot become a retry loop.
pub const AUTOMATIC_ASSIGNMENT_SUPERSEDED: &str = "automatic_assignment_superseded";

/// The actor a released step was taken from, read off the step's
/// metadata: `released.from`, trimmed, or `None` when the step was
/// never hand-released or the release predates the field.
pub fn released_from(metadata: &Value) -> Option<&str> {
    metadata
        .get(KEY)?
        .get(FROM)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_holder_a_step_was_released_from_is_read_off_its_metadata() {
        let md = json!({
            "authority_role": "platform-admin",
            KEY: { "why": "handing it to the other agent", "by": "agent-claude",
                   "at": "2026-10-20T09:40:00Z", "from_run": null,
                   FROM: "agent-claude-b" },
        });
        assert_eq!(released_from(&md), Some("agent-claude-b"));
        // `by` is the verb's caller, never the holder: a release that
        // predates `from` names nobody to leave out.
        let legacy = json!({ KEY: { "why": "w", "by": "agent-claude" } });
        assert_eq!(released_from(&legacy), None);
        assert_eq!(released_from(&json!({})), None);
        assert_eq!(released_from(&json!({ KEY: { FROM: "  " } })), None);
        assert_eq!(released_from(&json!({ KEY: { FROM: null } })), None);
    }
}
