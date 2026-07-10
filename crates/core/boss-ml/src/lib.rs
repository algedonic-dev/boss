//! boss-ml — ML platform infrastructure.
//!
//! Models and predictions are append-only rows. Storage-only models
//! receive predictions POSTed by code outside this crate; inference-
//! driven models have their scores computed by the `InferenceDispatcher`.
//! The CTO dashboard reads the model list and recent prediction counts.
//!
//! Decision record: `docs/architecture-decisions.md` §ML platform.

pub mod bootstrap;
pub mod config;
#[cfg(feature = "postgres")]
pub mod http;
pub mod in_memory;
pub mod inference;
pub mod port;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod types;

pub use in_memory::InMemoryMlRepo;
pub use inference::{BatchInferReport, InferError, InferOutput};
#[cfg(feature = "postgres")]
pub use inference::{InferContext, InferenceDispatcher, InferencePlugin};
pub use port::{MlError, MlRepository};
#[cfg(feature = "postgres")]
pub use postgres::PgMlRepo;
pub use types::*;
