//! Re-export of the shared `AssetsClient` trait from the
//! `boss-assets-client` crate.
//!
//! The trait + its reqwest adapter live in `boss-assets-client` so
//! `boss-catalog` (for the system-model delete guard) can share the
//! definition without a `kb → people` dependency. This module
//! re-exports them under `boss_people::assets_client::*` for call
//! sites that reach for them there.

pub use boss_assets_client::{AssetsClient, AssetsClientError, ReqwestAssetsClient};
