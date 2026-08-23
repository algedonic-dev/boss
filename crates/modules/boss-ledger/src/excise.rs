//! Graduated excise-tax rate math over registry data.
//!
//! Rates live in the `excise_rate_schedules` table (registry data —
//! same reference-data posture as `tax_kinds`): jurisdiction-keyed,
//! effective-dated rows whose `tiers` column holds an ordered list of
//! `{up_to_bbl, rate_cents_per_bbl}` bands. The real TTB curve for a
//! small domestic brewer is the worked example: $3.50/bbl on the first
//! 60,000 bbl of the CALENDAR YEAR, $16.00/bbl above (26 USC 5051).
//!
//! This module is the pure half: tier parsing/validation and the
//! amount computation. It knows nothing about Postgres or HTTP — the
//! `http::tax` accrual endpoint resolves the schedule row + the
//! year-to-date taxed barrels and calls [`graduated_amount_cents`].
//!
//! Tier semantics: `up_to_bbl` is the tier's inclusive upper bound in
//! cumulative calendar-year barrels. Every tier except the last must
//! be bounded; the last tier may be bounded (a statutory band label,
//! e.g. TTB's 6,000,000) but is treated as unbounded for computation —
//! barrels past the last bound stay at the last rate rather than
//! falling off the schedule.

use serde::{Deserialize, Serialize};

/// One band of a graduated excise schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateTier {
    /// Inclusive cumulative-barrel upper bound. `None` = unbounded
    /// (legal only on the last tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to_bbl: Option<i64>,
    pub rate_cents_per_bbl: i64,
}

/// Why a tier list was rejected. Kept as data (not a bare string) so
/// the HTTP layer can surface the exact defect on a 400.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TierError {
    #[error("tiers must be non-empty")]
    Empty,
    #[error("tier {index}: rate_cents_per_bbl must be positive, got {rate}")]
    NonPositiveRate { index: usize, rate: i64 },
    #[error("tier {index}: up_to_bbl must be positive, got {bound}")]
    NonPositiveBound { index: usize, bound: i64 },
    #[error("tier {index}: only the last tier may omit up_to_bbl")]
    UnboundedBeforeLast { index: usize },
    #[error("tier {index}: up_to_bbl {bound} does not exceed the previous bound {previous}")]
    NonIncreasingBound {
        index: usize,
        bound: i64,
        previous: i64,
    },
}

/// Validate an ordered tier list: non-empty, positive rates, strictly
/// increasing bounds, unbounded only in last position.
pub fn validate_tiers(tiers: &[RateTier]) -> Result<(), TierError> {
    if tiers.is_empty() {
        return Err(TierError::Empty);
    }
    let mut previous: Option<i64> = None;
    for (index, tier) in tiers.iter().enumerate() {
        if tier.rate_cents_per_bbl <= 0 {
            return Err(TierError::NonPositiveRate {
                index,
                rate: tier.rate_cents_per_bbl,
            });
        }
        match tier.up_to_bbl {
            None => {
                if index + 1 != tiers.len() {
                    return Err(TierError::UnboundedBeforeLast { index });
                }
            }
            Some(bound) => {
                if bound <= 0 {
                    return Err(TierError::NonPositiveBound { index, bound });
                }
                if let Some(prev) = previous
                    && bound <= prev
                {
                    return Err(TierError::NonIncreasingBound {
                        index,
                        bound,
                        previous: prev,
                    });
                }
                previous = Some(bound);
            }
        }
    }
    Ok(())
}

/// Tax a batch of `batch_bbl` barrels that lands after `ytd_bbl_before`
/// calendar-year barrels have already been taxed in this jurisdiction.
///
/// Each barrel is charged at the rate of the tier its cumulative
/// position falls in, so a batch that straddles a boundary splits:
/// with the TTB curve, YTD 59,900 + a 200-bbl batch is
/// 100 × 350¢ + 100 × 1600¢. Barrels past the last tier's bound stay
/// at the last rate. Pure function of its arguments — same schedule +
/// same prior barrels ⇒ same accrual, which is what makes replay
/// deterministic.
///
/// `tiers` must have passed [`validate_tiers`]; a defensive empty list
/// yields 0. Negative inputs are clamped to 0.
pub fn graduated_amount_cents(tiers: &[RateTier], ytd_bbl_before: i64, batch_bbl: i64) -> i64 {
    let mut remaining = batch_bbl.max(0);
    let mut position = ytd_bbl_before.max(0);
    let mut amount: i64 = 0;

    for (index, tier) in tiers.iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let last = index + 1 == tiers.len();
        // The last tier absorbs everything left regardless of its bound.
        let taxed_here = match tier.up_to_bbl {
            Some(bound) if !last => {
                let room = (bound - position).max(0);
                remaining.min(room)
            }
            _ => remaining,
        };
        amount += taxed_here * tier.rate_cents_per_bbl;
        position += taxed_here;
        remaining -= taxed_here;
    }

    amount
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real TTB small-brewer curve the brewery seeds ship.
    fn ttb() -> Vec<RateTier> {
        vec![
            RateTier {
                up_to_bbl: Some(60_000),
                rate_cents_per_bbl: 350,
            },
            RateTier {
                up_to_bbl: Some(6_000_000),
                rate_cents_per_bbl: 1600,
            },
        ]
    }

    #[test]
    fn single_unbounded_tier_is_the_flat_rate() {
        let flat = vec![RateTier {
            up_to_bbl: None,
            rate_cents_per_bbl: 350,
        }];
        assert_eq!(graduated_amount_cents(&flat, 0, 105), 105 * 350);
        assert_eq!(graduated_amount_cents(&flat, 500_000, 105), 105 * 350);
    }

    #[test]
    fn batch_entirely_inside_first_tier() {
        assert_eq!(graduated_amount_cents(&ttb(), 0, 105), 105 * 350);
        assert_eq!(graduated_amount_cents(&ttb(), 59_895, 105), 105 * 350);
    }

    #[test]
    fn batch_entirely_inside_second_tier() {
        assert_eq!(graduated_amount_cents(&ttb(), 60_000, 105), 105 * 1600);
        assert_eq!(graduated_amount_cents(&ttb(), 200_000, 158), 158 * 1600);
    }

    #[test]
    fn batch_straddling_the_60k_boundary_splits_across_both_rates() {
        // YTD 59,900 + 200-bbl batch: 100 bbl at $3.50, 100 bbl at $16.00.
        assert_eq!(
            graduated_amount_cents(&ttb(), 59_900, 200),
            100 * 350 + 100 * 1600
        );
        // Off-by-one edges: barrel 60,000 is the last cheap barrel,
        // barrel 60,001 the first expensive one.
        assert_eq!(graduated_amount_cents(&ttb(), 59_999, 1), 350);
        assert_eq!(graduated_amount_cents(&ttb(), 60_000, 1), 1600);
        assert_eq!(graduated_amount_cents(&ttb(), 59_999, 2), 350 + 1600);
    }

    #[test]
    fn barrels_past_the_last_bound_stay_at_the_last_rate() {
        // 6M is a statutory band label, not a cliff — production past it
        // keeps accruing at the last rate instead of going untaxed.
        assert_eq!(graduated_amount_cents(&ttb(), 6_000_000, 100), 100 * 1600);
        assert_eq!(
            graduated_amount_cents(&ttb(), 5_999_950, 100),
            100 * 1600 // last tier absorbs the overflow at its own rate
        );
    }

    #[test]
    fn zero_and_negative_inputs_accrue_nothing() {
        assert_eq!(graduated_amount_cents(&ttb(), 0, 0), 0);
        assert_eq!(graduated_amount_cents(&ttb(), 1000, -5), 0);
        assert_eq!(graduated_amount_cents(&[], 0, 100), 0);
    }

    #[test]
    fn negative_ytd_clamps_to_zero() {
        assert_eq!(graduated_amount_cents(&ttb(), -50, 10), 10 * 350);
    }

    #[test]
    fn whole_year_at_the_tenants_stated_volume_matches_the_audit_math() {
        // The measured finding that bought this car: ~262k bbl/yr at a
        // flat $3.50 understates ~3.7×. 60k × $3.50 + 202k × $16.00.
        let total = graduated_amount_cents(&ttb(), 0, 262_000);
        assert_eq!(total, 60_000 * 350 + 202_000 * 1600);
        let flat = 262_000 * 350;
        assert!(
            (total as f64) / (flat as f64) > 3.6,
            "graduated {total} vs flat {flat} should be ~3.7x"
        );
    }

    // --- validation -------------------------------------------------------

    #[test]
    fn valid_tiers_pass() {
        assert_eq!(validate_tiers(&ttb()), Ok(()));
        assert_eq!(
            validate_tiers(&[RateTier {
                up_to_bbl: None,
                rate_cents_per_bbl: 350
            }]),
            Ok(())
        );
    }

    #[test]
    fn empty_tiers_rejected() {
        assert_eq!(validate_tiers(&[]), Err(TierError::Empty));
    }

    #[test]
    fn non_positive_rate_rejected() {
        let tiers = vec![RateTier {
            up_to_bbl: None,
            rate_cents_per_bbl: 0,
        }];
        assert_eq!(
            validate_tiers(&tiers),
            Err(TierError::NonPositiveRate { index: 0, rate: 0 })
        );
    }

    #[test]
    fn unbounded_tier_before_last_rejected() {
        let tiers = vec![
            RateTier {
                up_to_bbl: None,
                rate_cents_per_bbl: 350,
            },
            RateTier {
                up_to_bbl: Some(60_000),
                rate_cents_per_bbl: 1600,
            },
        ];
        assert_eq!(
            validate_tiers(&tiers),
            Err(TierError::UnboundedBeforeLast { index: 0 })
        );
    }

    #[test]
    fn non_increasing_bounds_rejected() {
        let tiers = vec![
            RateTier {
                up_to_bbl: Some(60_000),
                rate_cents_per_bbl: 350,
            },
            RateTier {
                up_to_bbl: Some(60_000),
                rate_cents_per_bbl: 1600,
            },
        ];
        assert_eq!(
            validate_tiers(&tiers),
            Err(TierError::NonIncreasingBound {
                index: 1,
                bound: 60_000,
                previous: 60_000
            })
        );
    }

    #[test]
    fn non_positive_bound_rejected() {
        let tiers = vec![RateTier {
            up_to_bbl: Some(0),
            rate_cents_per_bbl: 350,
        }];
        assert_eq!(
            validate_tiers(&tiers),
            Err(TierError::NonPositiveBound { index: 0, bound: 0 })
        );
    }
}
