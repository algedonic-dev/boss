//! The owning PostgreSQL adapter is shared without depending on this service.
//! Historical service imports retain the same adapter type and write contract.

pub use boss_policy_client::postgres::{PgPolicy, StatementRefresh};
