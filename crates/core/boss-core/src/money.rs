//! `Money` — integer cents + ISO-4217 currency code.
//!
//! Representation rules (`docs/architecture-decisions.md` §Finance
//! & ledger):
//! - Storage + wire unit is integer cents. No decimals, no floats.
//! - Currency travels with every value as a 3-letter ISO-4217 code,
//!   even while Boss is USD-only today.
//! - Arithmetic on `Money` requires matching currency at runtime.
//!   Cross-currency math is a domain error, not a silent conversion.
//! - Display formatting lives at the UI edge (`apps/web/src/ui/money.ts`),
//!   not on this type. Rust code that needs a string uses `to_string`
//!   which emits a locale-neutral `<cents> <ISO>` form.
//!
//! This module is intentionally thin — Session 1 of the monetary-units
//! migration lands the type so downstream services can start
//! importing it. Session 2 swaps the `*_usd INTEGER` schema columns
//! to `*_cents BIGINT` + `currency TEXT` and has every crate use
//! `Money` at API boundaries. Session 3 migrates the frontend.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A 3-letter ISO-4217 currency code, uppercased. Validated at
/// construction — cannot hold an invalid code.
///
/// We keep this as a newtype (not an enum) because the set of real
/// currencies is long, regional, and occasionally gains entries. The
/// validation we care about at this level is "3 ASCII letters" — any
/// further validation (e.g. "is this a currency we support for
/// settlement?") is a boundary check that belongs in the service
/// layer that owns the currency.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Currency(String);

impl Currency {
    /// The canonical single-currency code while Boss is USD-only.
    pub const USD: &'static str = "USD";

    /// Construct a validated `Currency` from a string. Accepts mixed
    /// case; stores as uppercase.
    pub fn new(code: impl AsRef<str>) -> Result<Self, MoneyError> {
        let s = code.as_ref().trim();
        if s.len() != 3 || !s.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(MoneyError::InvalidCurrency(s.to_string()));
        }
        Ok(Self(s.to_ascii_uppercase()))
    }

    /// Shorthand for the Boss single-currency default.
    pub fn usd() -> Self {
        Self(Self::USD.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A monetary amount in integer cents tagged with its currency.
///
/// Wire / storage shape is `{ amount_cents: i64, currency: "USD" }`.
/// All arithmetic goes through methods that validate currency match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Money {
    pub amount_cents: i64,
    pub currency: Currency,
}

impl Money {
    /// Construct from an explicit (cents, currency) pair.
    pub fn new(amount_cents: i64, currency: Currency) -> Self {
        Self {
            amount_cents,
            currency,
        }
    }

    /// Shorthand for USD-denominated values. The common case while
    /// Boss is single-currency — tests and sim generators lean on
    /// this, production domain code uses `Money::new` with an
    /// explicit currency.
    pub fn usd(amount_cents: i64) -> Self {
        Self {
            amount_cents,
            currency: Currency::usd(),
        }
    }

    /// Construct from a dollars-and-cents pair. Rejects negative
    /// cents on positive dollars (and vice versa) to avoid the
    /// "is this -1.50 or -1.-50" ambiguity.
    pub fn from_major_minor(
        major: i64,
        minor: u32,
        currency: Currency,
    ) -> Result<Self, MoneyError> {
        if minor >= 100 {
            return Err(MoneyError::InvalidMinor(minor));
        }
        let sign = if major < 0 { -1 } else { 1 };
        let cents = major
            .checked_mul(100)
            .ok_or(MoneyError::Overflow)?
            .checked_add(sign * minor as i64)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(cents, currency))
    }

    /// Zero of the given currency. Useful as an accumulator seed.
    pub fn zero(currency: Currency) -> Self {
        Self::new(0, currency)
    }

    pub fn is_zero(&self) -> bool {
        self.amount_cents == 0
    }

    /// Add two amounts. Both operands must share currency.
    pub fn add(&self, other: &Self) -> Result<Self, MoneyError> {
        if self.currency != other.currency {
            return Err(MoneyError::CurrencyMismatch {
                left: self.currency.clone(),
                right: other.currency.clone(),
            });
        }
        let sum = self
            .amount_cents
            .checked_add(other.amount_cents)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(sum, self.currency.clone()))
    }

    /// Subtract `other` from `self`. Both must share currency.
    pub fn sub(&self, other: &Self) -> Result<Self, MoneyError> {
        if self.currency != other.currency {
            return Err(MoneyError::CurrencyMismatch {
                left: self.currency.clone(),
                right: other.currency.clone(),
            });
        }
        let diff = self
            .amount_cents
            .checked_sub(other.amount_cents)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(diff, self.currency.clone()))
    }

    /// Multiply by an integer scalar (e.g. quantity of line items).
    /// Multiplying by another `Money` is intentionally not provided —
    /// money-times-money isn't a meaningful domain operation.
    pub fn mul_scalar(&self, n: i64) -> Result<Self, MoneyError> {
        let product = self
            .amount_cents
            .checked_mul(n)
            .ok_or(MoneyError::Overflow)?;
        Ok(Self::new(product, self.currency.clone()))
    }
}

/// Locale-neutral debug/logging display — `<cents> <ISO>`. UIs
/// should route through `formatMoney` on the frontend instead of
/// relying on this string.
impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.amount_cents, self.currency)
    }
}

/// Errors from `Money` / `Currency` construction and arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    #[error("invalid currency code `{0}` — expected 3 ASCII letters")]
    InvalidCurrency(String),
    #[error("minor units must be < 100, got {0}")]
    InvalidMinor(u32),
    #[error("currency mismatch: {left} vs {right}")]
    CurrencyMismatch { left: Currency, right: Currency },
    #[error("integer overflow in monetary arithmetic")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_validation() {
        assert_eq!(Currency::new("USD").unwrap().as_str(), "USD");
        assert_eq!(Currency::new("usd").unwrap().as_str(), "USD");
        assert_eq!(Currency::new(" EUR ").unwrap().as_str(), "EUR");
        assert!(matches!(
            Currency::new("US"),
            Err(MoneyError::InvalidCurrency(_))
        ));
        assert!(matches!(
            Currency::new("US1"),
            Err(MoneyError::InvalidCurrency(_))
        ));
        assert!(matches!(
            Currency::new("USDD"),
            Err(MoneyError::InvalidCurrency(_))
        ));
    }

    #[test]
    fn from_major_minor_round_trip() {
        let m = Money::from_major_minor(12, 34, Currency::usd()).unwrap();
        assert_eq!(m.amount_cents, 1234);
        assert_eq!(m.currency, Currency::usd());

        let neg = Money::from_major_minor(-5, 50, Currency::usd()).unwrap();
        assert_eq!(neg.amount_cents, -550);

        assert!(matches!(
            Money::from_major_minor(1, 100, Currency::usd()),
            Err(MoneyError::InvalidMinor(100))
        ));
    }

    #[test]
    fn arithmetic_same_currency() {
        let a = Money::usd(1_000);
        let b = Money::usd(250);
        assert_eq!(a.add(&b).unwrap().amount_cents, 1_250);
        assert_eq!(a.sub(&b).unwrap().amount_cents, 750);
        assert_eq!(a.mul_scalar(3).unwrap().amount_cents, 3_000);
    }

    #[test]
    fn arithmetic_rejects_currency_mismatch() {
        let usd = Money::usd(1_000);
        let eur = Money::new(1_000, Currency::new("EUR").unwrap());
        assert!(matches!(
            usd.add(&eur),
            Err(MoneyError::CurrencyMismatch { .. })
        ));
        assert!(matches!(
            usd.sub(&eur),
            Err(MoneyError::CurrencyMismatch { .. })
        ));
    }

    #[test]
    fn overflow_surfaces_as_error() {
        let big = Money::usd(i64::MAX);
        let one = Money::usd(1);
        assert!(matches!(big.add(&one), Err(MoneyError::Overflow)));
        assert!(matches!(big.mul_scalar(2), Err(MoneyError::Overflow)));
    }

    #[test]
    fn serde_round_trip_preserves_shape() {
        let m = Money::usd(1234);
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["amount_cents"], 1234);
        assert_eq!(j["currency"], "USD");

        let back: Money = serde_json::from_value(j).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn display_is_locale_neutral() {
        assert_eq!(Money::usd(1234).to_string(), "1234 USD");
        assert_eq!(
            Money::new(-50, Currency::new("CAD").unwrap()).to_string(),
            "-50 CAD"
        );
    }

    #[test]
    fn zero_accumulator_pattern() {
        let lines = [Money::usd(100), Money::usd(250), Money::usd(42)];
        let mut total = Money::zero(Currency::usd());
        for l in &lines {
            total = total.add(l).unwrap();
        }
        assert_eq!(total.amount_cents, 392);
    }
}
