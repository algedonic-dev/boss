//! Ambient request-actor propagation.
//!
//! Every audit_log event must name the authority that fired it (see
//! [`crate::actor::ActorId`] — there is no anonymous "system" actor).
//! Most domain writes happen inside an HTTP request whose authenticated
//! identity IS that authority, but the handler emits its event through
//! [`DomainPublisher`] without threading the actor down by hand.
//!
//! The propagation mirrors [`crate::sim_origin`]:
//! 1. Callers stamp `x-boss-user` on every outbound HTTP call.
//! 2. Receiving services have a middleware layer that parses the header
//!    into an [`ActorId`] and sets [`REQUEST_ACTOR`] for the duration
//!    of the request handler.
//! 3. The DomainPublisher reads [`current_actor`] as the default actor
//!    for an emit that didn't pass one explicitly, falling back to the
//!    emitting service's own automation identity outside any request.
//!
//! [`DomainPublisher`]: crate::publisher::DomainPublisher

use crate::actor::ActorId;

tokio::task_local! {
    /// Set by the request-context middleware to the authenticated
    /// actor of the current request.
    static REQUEST_ACTOR: ActorId;
}

/// The actor of the current request, or `None` outside a request
/// scope (startup tasks, background jobs, CLI tools, tests).
pub fn current_actor() -> Option<ActorId> {
    REQUEST_ACTOR.try_with(|a| a.clone()).ok()
}

/// Run `fut` with [`REQUEST_ACTOR`] set to `actor` for the duration of
/// the future. Used by the request-context middleware to scope the
/// actor to a single request.
pub async fn with_actor<F: std::future::Future>(actor: ActorId, fut: F) -> F::Output {
    REQUEST_ACTOR.scope(actor, fut).await
}
