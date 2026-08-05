//! Views — saved compositions over the Information API.
//!
//! A View is the personal rung of the extensibility ladder. Below
//! "author a JobKind" there used to be nothing: an operator who wanted
//! to *look at* the information a different way could ask for a
//! frontend change or keep a spreadsheet, and the spreadsheet is a
//! silo arriving by another door.
//!
//! A View is deliberately not a Cloudflare-OS gadget. It holds a query
//! and a layout, never records — so it stays a pure function of the
//! same projections everything else reads, and two people running the
//! same View see the same numbers because there is only one set of
//! numbers.
//!
//! Design + decision history:
//! `docs/design/home-workspace-and-department-apps.md`.

pub mod error;
pub mod filter;
pub mod in_memory;
pub mod port;
pub mod types;

#[cfg(feature = "postgres")]
pub mod http;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "postgres")]
pub mod query;

pub use error::ViewsError;
pub use in_memory::InMemoryViewsRepo;
pub use port::{ViewResolver, ViewsRepo};
pub use types::{View, ViewInput, ViewLayout, ViewResults, ViewSource, Visibility};

#[cfg(feature = "postgres")]
pub use postgres::PgViewsRepo;
#[cfg(feature = "postgres")]
pub use query::PgViewResolver;
