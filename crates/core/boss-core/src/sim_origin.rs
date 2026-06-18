//! Sim-chain context propagation.
//!
//! When an event is part of a simulated chain (kicked off by the
//! simulator), every downstream event triggered by it must inherit
//! the `_simulated=true` marker — data-integrity invariant the
//! correctness protocol requires so sim chains can't accidentally
//! produce wall-tagged events on a service that happens to be in
//! wall-clock mode.
//!
//! The propagation:
//! 1. The simulator stamps `x-sim-origin: true` on every outbound
//!    HTTP call.
//! 2. Receiving services have a middleware layer that extracts the
//!    header and sets [`IN_SIM_CHAIN`] for the duration of the
//!    request handler.
//! 3. The DomainPublisher's [`SimulatedProbe`] checks
//!    [`is_in_sim_chain`] before falling back to clock-mode.
//! 4. Outbound HTTP calls from handlers reading
//!    [`is_in_sim_chain`] include the header in their requests so
//!    the chain continues into downstream services.
//!
//! [`SimulatedProbe`]: crate::publisher::SimulatedProbe

/// HTTP header that marks a request as part of a simulated event
/// chain. Receivers set the task-local [`IN_SIM_CHAIN`] for the
/// duration of the handler.
pub const SIM_ORIGIN_HEADER: &str = "x-sim-origin";

tokio::task_local! {
    /// Set to `true` by the SimOrigin middleware when the current
    /// request carried the `x-sim-origin: true` header.
    static IN_SIM_CHAIN: bool;
}

/// Returns `true` if the current task is running inside a request
/// marked as part of a sim chain. Returns `false` outside the
/// middleware scope (e.g. startup tasks, background jobs, tests
/// without a request).
pub fn is_in_sim_chain() -> bool {
    IN_SIM_CHAIN.try_with(|v| *v).unwrap_or(false)
}

/// Run `fut` with [`IN_SIM_CHAIN`] set to `flag` for the duration
/// of the future. Used by the axum SimOrigin middleware to scope
/// the flag to a single request.
pub async fn with_sim_chain<F: std::future::Future>(flag: bool, fut: F) -> F::Output {
    IN_SIM_CHAIN.scope(flag, fut).await
}

/// Parse the `x-sim-origin` header value into a boolean.
///
/// Accepts `"true"` and `"1"` as true. Anything else (including the
/// header being absent) is false.
///
/// Header lookup is case-insensitive so callers pass either
/// `headers.get("x-sim-origin")` or a typed `HeaderName::from_static`
/// — both resolve.
pub fn header_is_truthy(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1"))
}
