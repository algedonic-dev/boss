#[cfg(feature = "postgres")]
pub mod audit_pg;
pub mod bus;
pub mod claude_dispatcher;
pub mod dispatcher;
#[cfg(feature = "events-api")]
pub mod events_api_config;
#[cfg(feature = "postgres")]
pub mod integrity;
pub mod ledger;
#[cfg(feature = "postgres")]
pub mod messages_events_pg;
#[cfg(feature = "postgres")]
pub mod outbox;
pub mod queue;
pub mod registry;
#[cfg(feature = "postgres")]
pub mod replay;
pub mod store;
#[cfg(feature = "postgres")]
pub mod tail_http;

#[cfg(feature = "postgres")]
pub use audit_pg::PgAuditWriter;
#[cfg(feature = "postgres")]
pub use integrity::{
    ChainBreak, CreatedAtRegression, IdGap, IntegrityReport, check_audit_log_integrity,
};
#[cfg(feature = "postgres")]
pub use messages_events_pg::PgMessagesEventWriter;
#[cfg(feature = "postgres")]
pub use tail_http::audit_tail_router;
