//! Sales-tax-rate lookup for the invoicing generator.
//!
//! Data lives in `boss-sim/data/us_sales_tax_rates.toml`, bundled into
//! the binary via `include_str!`. The TOML is canonical — the
//! `sales_tax_rate_by_state` seed in `infra/postgres/schema/40-ledger.sql`
//! is a downstream copy used by services that don't link boss-sim, kept
//! aligned with this TOML by hand.
//!
//! Rates are in basis points (100 bps = 1.00%). Jurisdiction strings
//! match the `tax_filings.jurisdiction` column + the Ledger API's
//! tax_lines.jurisdiction field.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const RATES_TOML: &str = include_str!("../data/us_sales_tax_rates.toml");

#[derive(Deserialize)]
struct RatesBundle {
    rate: Vec<RateRow>,
}

#[derive(Deserialize)]
struct RateRow {
    state: String,
    bps: i64,
    jurisdiction: String,
}

fn rates_by_state() -> &'static HashMap<&'static str, (i64, &'static str)> {
    static CACHE: OnceLock<HashMap<&'static str, (i64, &'static str)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let bundle: RatesBundle = toml::from_str(RATES_TOML)
            .expect("us_sales_tax_rates.toml ships with the crate and must parse");
        let mut map: HashMap<&'static str, (i64, &'static str)> =
            HashMap::with_capacity(bundle.rate.len());
        for row in bundle.rate {
            // Leak the keys to get &'static str — we cache once for
            // process lifetime, so a 27-entry leak is fine and lets
            // the public API stay zero-allocation per call.
            let state: &'static str = Box::leak(row.state.into_boxed_str());
            let jur: &'static str = Box::leak(row.jurisdiction.into_boxed_str());
            map.insert(state, (row.bps, jur));
        }
        map
    })
}

/// Return `(rate_bps, jurisdiction)` for a two-letter US state. Unknown
/// states default to zero — the invoice still ships, just without tax.
/// Zero-tax states (OR/NH/MT/DE/AK) also return 0.
pub fn rate_for_state(state: &str) -> (i64, Option<&'static str>) {
    match rates_by_state().get(state).copied() {
        Some((bps, jur)) => (bps, Some(jur)),
        None => (0, None),
    }
}

/// Compute sales tax on a revenue amount (cents) for a account's
/// billing state. Returns `(tax_cents, jurisdiction)`. Tax is rounded
/// to the nearest cent via integer division — under-collection by up
/// to one cent per invoice is acceptable and matches how real POS
/// systems round.
pub fn compute_tax(revenue_cents: i64, state: &str) -> (i64, Option<&'static str>) {
    let (rate_bps, jurisdiction) = rate_for_state(state);
    if rate_bps == 0 || jurisdiction.is_none() {
        return (0, jurisdiction);
    }
    let tax = (revenue_cents * rate_bps) / 10_000;
    (tax, jurisdiction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn california_rate_is_725bps() {
        let (tax, jur) = compute_tax(100_000, "CA");
        assert_eq!(tax, 7_250);
        assert_eq!(jur, Some("US-CA"));
    }

    #[test]
    fn oregon_charges_no_tax_but_returns_jurisdiction() {
        let (tax, jur) = compute_tax(100_000, "OR");
        assert_eq!(tax, 0);
        assert_eq!(jur, Some("US-OR"));
    }

    #[test]
    fn unknown_state_returns_zero_and_no_jurisdiction() {
        let (tax, jur) = compute_tax(100_000, "ZZ");
        assert_eq!(tax, 0);
        assert_eq!(jur, None);
    }

    #[test]
    fn rounding_truncates_toward_zero() {
        // $100.03 * 7.25% = $7.2522 → rounds down to $7.25 (725 cents).
        let (tax, _) = compute_tax(10_003, "CA");
        assert_eq!(tax, 725);
    }
}
