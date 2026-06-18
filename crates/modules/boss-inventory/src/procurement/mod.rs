//! Procurement — vendor CRM + intelligence, namespaced under
//! `boss-inventory` per D3 of `examples/used-device-shop/design/procurement-team-needs.md`.
//!
//! Session 1 scope: contacts, interactions, account-team, contracts
//! — the Vendor Knowledge Base four-section plumbing. Jobs + plugins
//! + crawl intelligence land in subsequent sessions.

pub mod http;
pub mod types;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use types::{
    NewVendorAccountTeamMember, NewVendorContact, NewVendorContract, NewVendorInteraction,
    VendorAccountTeamMember, VendorContact, VendorContract, VendorInteraction,
};
