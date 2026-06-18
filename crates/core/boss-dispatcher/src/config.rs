//! Runtime configuration for boss-dispatcher.

use serde::Deserialize;
use tracing::warn;

/// How the dispatcher distributes a ready Step across the active holders
/// of its required role. Selected by data (`BOSS_DISPATCH_STRATEGY`), not
/// baked in — per the registries/data-over-hardcoded-paths rule, the
/// work-dispatch *behavior* is config-selectable. New strategies are named
/// here and gated in `pick_employee`, never forked into a caller's `match`.
///
/// Both variants are deterministic: the same (strategy, sorted roster,
/// step id) always selects the same employee, so an assignment replays
/// identically across a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssignmentStrategy {
    /// Legacy behavior: index 0 of the id-sorted candidates (the
    /// lowest-id holder). Kept selectable for parity/debugging.
    LowestId,
    /// Default: spread deterministically across the role's holders by a
    /// stable hash of the step id, so load fans out instead of piling
    /// onto one employee.
    #[default]
    Spread,
}

impl AssignmentStrategy {
    /// Parse the `BOSS_DISPATCH_STRATEGY` value. `"lowest-id"` → `LowestId`,
    /// `"spread"` → `Spread`. An unknown or empty value falls back to the
    /// default (`Spread`) with a `warn!` — a typo must not hard-fail the
    /// dispatcher, only nudge the operator. Case/whitespace-insensitive.
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "lowest-id" | "lowest_id" => Self::LowestId,
            "spread" => Self::Spread,
            "" => Self::default(),
            other => {
                warn!(
                    value = %other,
                    "unknown BOSS_DISPATCH_STRATEGY; falling back to default `spread`"
                );
                Self::default()
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DispatcherConfig {
    pub nats_url: String,
    pub jobs_api_url: String,
    pub people_api_url: String,
    pub inventory_api_url: String,
    pub commerce_api_url: String,
    pub products_api_url: String,
    pub shipping_api_url: String,
    pub ledger_api_url: String,
    pub messages_api_url: String,
    pub http_bind: String,
    /// Path to the rule registry TOML file. Optional — when
    /// absent, the dispatcher only runs its legacy role-assignment
    /// loop, leaving the rules runner inert.
    pub rules_path: Option<String>,
    /// External webhook URL for the `webhook.notify` handler to forward
    /// matched events to. `None` (the normal deployment) makes
    /// `webhook.notify` a no-op; a regen sets it to the brewery-engine's
    /// callback server so its CounterpartyEngine (banks, suppliers,
    /// courier, tax authority) reacts to live events as an external party.
    pub webhook_url: Option<String>,
    /// Which step-assignment distribution strategy the dispatcher applies
    /// (data-selected via `BOSS_DISPATCH_STRATEGY`, default `spread`).
    /// Threaded into `DispatcherCtx` so `pick_employee` can gate the
    /// index it takes into the sorted candidate list. `#[serde(skip)]`:
    /// the field is sourced from env in `Default`, not from a serde
    /// document, and the strategy enum is intentionally not `Deserialize`.
    #[serde(skip)]
    pub assignment_strategy: AssignmentStrategy,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            nats_url: std::env::var("BOSS_NATS_URL")
                .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
            jobs_api_url: std::env::var("BOSS_JOBS_URL")
                .unwrap_or_else(|_| boss_ports::url("jobs")),
            people_api_url: std::env::var("BOSS_PEOPLE_URL")
                .unwrap_or_else(|_| boss_ports::url("people")),
            inventory_api_url: std::env::var("BOSS_INVENTORY_URL")
                .unwrap_or_else(|_| boss_ports::url("inventory")),
            commerce_api_url: std::env::var("BOSS_COMMERCE_URL")
                .unwrap_or_else(|_| boss_ports::url("commerce")),
            products_api_url: std::env::var("BOSS_PRODUCTS_URL")
                .unwrap_or_else(|_| boss_ports::url("products")),
            shipping_api_url: std::env::var("BOSS_SHIPPING_URL")
                .unwrap_or_else(|_| boss_ports::url("shipping")),
            ledger_api_url: std::env::var("BOSS_LEDGER_URL")
                .unwrap_or_else(|_| boss_ports::url("ledger")),
            messages_api_url: std::env::var("BOSS_MESSAGES_URL")
                .unwrap_or_else(|_| boss_ports::url("messages")),
            http_bind: format!("0.0.0.0:{}", boss_ports::prod("dispatcher")),
            rules_path: std::env::var("BOSS_DISPATCHER_RULES").ok(),
            webhook_url: std::env::var("BOSS_EVENT_WEBHOOK_URL").ok(),
            assignment_strategy: AssignmentStrategy::parse(
                &std::env::var("BOSS_DISPATCH_STRATEGY").unwrap_or_default(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AssignmentStrategy;

    /// The data-selected default IS `Spread` — both via the explicit
    /// `Default` impl (what `#[serde(skip)]` falls back to) and via parsing
    /// an empty `BOSS_DISPATCH_STRATEGY`, which is what an unset env var
    /// resolves to in `DispatcherConfig::default`.
    #[test]
    fn default_strategy_is_spread() {
        assert_eq!(AssignmentStrategy::default(), AssignmentStrategy::Spread);
        assert_eq!(AssignmentStrategy::parse(""), AssignmentStrategy::Spread);
        assert_eq!(AssignmentStrategy::parse("   "), AssignmentStrategy::Spread);
    }

    /// Known values parse to their variant, case- and whitespace-insensitive,
    /// accepting both the kebab and snake spelling.
    #[test]
    fn known_strategies_parse() {
        assert_eq!(
            AssignmentStrategy::parse("spread"),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("  SPREAD  "),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("lowest-id"),
            AssignmentStrategy::LowestId
        );
        assert_eq!(
            AssignmentStrategy::parse("Lowest_Id"),
            AssignmentStrategy::LowestId
        );
    }

    /// A typo must NOT hard-fail (no panic, no error type) — it falls back to
    /// the default. The `warn!` is a side effect; the value contract is that
    /// an unknown string still yields a usable strategy.
    #[test]
    fn unknown_strategy_falls_back_to_default() {
        assert_eq!(
            AssignmentStrategy::parse("round-robin"),
            AssignmentStrategy::Spread
        );
        assert_eq!(
            AssignmentStrategy::parse("sprad-typo"),
            AssignmentStrategy::Spread
        );
    }
}
