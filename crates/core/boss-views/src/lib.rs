//! Views — saved compositions over the Information API.
//!
//! A View is the personal rung of the extensibility ladder. Below
//! "author a Workflow" there used to be nothing: an operator who wanted
//! to *look at* the information a different way could ask for a
//! frontend change or keep a spreadsheet, and the spreadsheet is a
//! silo arriving by another door.
//!
//! A View is deliberately not a Cloudflare-OS gadget. It holds a query
//! and a layout, never records — so it stays a pure function of the
//! same projections everything else reads. It is scoped to whoever
//! runs it, not to its author: a narrower role sees its own rows, and
//! each result says which (`ViewResults::scope`, backlog 5392cf23).
//!
//! Design + decision history:
//! `docs/architecture-decisions.md §Step UX & frontend`.

//!
//! The os-map, flow, fleet and stage views (`/api/views/os-map`,
//! `/flow`, `/fleet/{kind}`, `/stage-durations/{kind}`,
//! `/stage-runs/{kind}`) went with the last pages that read them —
//! Bottlenecks and the Department Map's triage flow — in step 1 of
//! design 01fa5bdf (2026-11-17).

pub mod error;
pub mod filter;
pub mod in_memory;
pub mod port;
pub mod pushdown;
pub mod types;

#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod query;
#[cfg(feature = "postgres")]
pub mod rebuild_event_facts;

pub use error::ViewsError;
pub use in_memory::InMemoryViewsRepo;
pub use port::{ViewResolver, ViewsRepo};
pub use types::{
    ResultScope, View, ViewInput, ViewLayout, ViewResults, ViewSource, ViewSources, Visibility,
};

#[cfg(feature = "postgres")]
pub use postgres::PgViewsRepo;
#[cfg(feature = "postgres")]
pub use query::PgViewResolver;
#[cfg(feature = "postgres")]
pub use rebuild_event_facts::{RebuildEventFactsReport, catch_up_event_facts, rebuild_event_facts};
