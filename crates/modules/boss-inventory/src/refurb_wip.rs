//! Tenant-shape projection of the generic phase-distribution into
//! warehouse-stage counts for the used-device-shop refurb pipeline.
//!
//! Lives here rather than in Tier 1 `boss-jobs-client`: the
//! `refurb-used` / `refurb-oem-new` step graphs are device-shop
//! JobKinds, not generic state-machine OS concepts, so the
//! projection sits next to its sole consumer (`warehouse_status.rs`).

use serde::{Deserialize, Serialize};

use boss_jobs_client::PhaseDistribution;

/// Refurb-pipeline WIP summary keyed by warehouse stage.
/// Populated by mapping a `PhaseDistribution`'s per-tier counts
/// onto the stage labels used by the warehouse dashboard.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RefurbWipSummary {
    pub total_in_flight: i64,
    /// Stage counts in pipeline order — intake → triage → refurb → qa → ready.
    pub by_stage: Vec<RefurbStageCount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefurbStageCount {
    pub stage: String,
    pub count: i64,
}

/// Pipeline order for the warehouse-status dashboard. Matches the
/// refurb sim generator's STAGE_ORDER so operators see the same labels
/// across surfaces.
pub const STAGE_ORDER: &[&str] = &["intake", "triage", "refurb", "qa", "ready"];

/// Project the generic `PhaseDistribution` into refurb-pipeline stage
/// counts. Non-refurb kinds are ignored; counts roll up across both
/// shipped refurb JobKinds.
pub fn summarise_refurb_wip(dist: &PhaseDistribution) -> RefurbWipSummary {
    let mut counts = std::collections::HashMap::<&'static str, i64>::new();
    for stage in STAGE_ORDER {
        counts.insert(stage, 0);
    }

    let mut total = 0i64;
    for (kind, kind_counts) in &dist.by_kind {
        if !kind.starts_with("refurb") {
            continue;
        }
        for (tier_str, count) in &kind_counts.tiers {
            let Ok(tier) = tier_str.parse::<i32>() else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            let stage = tier_to_stage(kind, tier);
            total += count;
            if let Some(slot) = counts.get_mut(stage) {
                *slot += count;
            }
        }
    }

    let by_stage: Vec<RefurbStageCount> = STAGE_ORDER
        .iter()
        .map(|s| RefurbStageCount {
            stage: s.to_string(),
            count: counts.get(s).copied().unwrap_or(0),
        })
        .collect();

    RefurbWipSummary {
        total_in_flight: total,
        by_stage,
    }
}

/// Map (JobKind, tier) to a warehouse-status stage. Encoded from the
/// seeded `refurb-used` (tiers 0–5) and `refurb-oem-new` (tiers 0–3)
/// step graphs. `tier == -1` means every step is terminal but the Job
/// isn't closed yet — treated as "ready" since the device is sitting
/// in put-away waiting for a close event.
fn tier_to_stage(kind: &str, tier: i32) -> &'static str {
    match (kind, tier) {
        // refurb-used: 0 intake / 1 triage / 2 repair / 3 QA insp / 4 QA cert / 5 put-away
        ("refurb-used", 0) => "intake",
        ("refurb-used", 1) => "triage",
        ("refurb-used", 2) => "refurb",
        ("refurb-used", 3 | 4) => "qa",
        ("refurb-used", 5) => "ready",
        // refurb-oem-new: 0 intake / 1 QA insp / 2 QA cert / 3 put-away
        ("refurb-oem-new", 0) => "intake",
        ("refurb-oem-new", 1 | 2) => "qa",
        ("refurb-oem-new", 3) => "ready",
        // Any-kind tier -1 = steps all terminal, job not closed.
        (_, -1) => "ready",
        // Unknown kind/tier combo — fall back to intake so the count is
        // still visible rather than silently dropped.
        _ => "intake",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs_client::PhaseKindCounts;
    use std::collections::BTreeMap;

    fn dist(kinds: &[(&str, &[(&str, i64)])]) -> PhaseDistribution {
        let mut by_kind = BTreeMap::new();
        for (k, tiers) in kinds {
            let mut t = BTreeMap::new();
            for (tier, count) in *tiers {
                t.insert(tier.to_string(), *count);
            }
            by_kind.insert(k.to_string(), PhaseKindCounts { tiers: t });
        }
        PhaseDistribution { by_kind }
    }

    #[test]
    fn maps_refurb_used_tiers_to_stages() {
        let input = dist(&[(
            "refurb-used",
            &[("0", 3), ("1", 5), ("2", 2), ("3", 1), ("4", 1), ("5", 4)],
        )]);
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 16);
        let by: std::collections::HashMap<_, _> = summary
            .by_stage
            .iter()
            .map(|r| (r.stage.as_str(), r.count))
            .collect();
        assert_eq!(by["intake"], 3);
        assert_eq!(by["triage"], 5);
        assert_eq!(by["refurb"], 2);
        assert_eq!(by["qa"], 2);
        assert_eq!(by["ready"], 4);
    }

    #[test]
    fn maps_refurb_oem_new_tiers_to_stages() {
        let input = dist(&[("refurb-oem-new", &[("0", 2), ("1", 3), ("2", 1), ("3", 5)])]);
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 11);
        let by: std::collections::HashMap<_, _> = summary
            .by_stage
            .iter()
            .map(|r| (r.stage.as_str(), r.count))
            .collect();
        assert_eq!(by["intake"], 2);
        assert_eq!(by["qa"], 4);
        assert_eq!(by["ready"], 5);
        assert_eq!(by["triage"], 0);
        assert_eq!(by["refurb"], 0);
    }

    #[test]
    fn negative_one_tier_counts_as_ready() {
        let input = dist(&[("refurb-used", &[("-1", 7)])]);
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 7);
        let ready = summary
            .by_stage
            .iter()
            .find(|r| r.stage == "ready")
            .unwrap();
        assert_eq!(ready.count, 7);
    }

    #[test]
    fn non_refurb_kinds_are_ignored() {
        let input = dist(&[
            ("refurb-used", &[("0", 2)]),
            ("sale", &[("0", 50)]),
            ("field-service", &[("0", 12)]),
        ]);
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 2);
    }

    #[test]
    fn empty_input_returns_zeroed_stages_in_order() {
        let input = PhaseDistribution::default();
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 0);
        let stages: Vec<&str> = summary.by_stage.iter().map(|r| r.stage.as_str()).collect();
        assert_eq!(stages, STAGE_ORDER);
        for row in &summary.by_stage {
            assert_eq!(row.count, 0);
        }
    }

    #[test]
    fn summary_aggregates_across_refurb_kinds() {
        let input = dist(&[
            ("refurb-used", &[("5", 3)]),
            ("refurb-oem-new", &[("3", 2)]),
        ]);
        let summary = summarise_refurb_wip(&input);
        assert_eq!(summary.total_in_flight, 5);
        let ready = summary
            .by_stage
            .iter()
            .find(|r| r.stage == "ready")
            .unwrap();
        assert_eq!(ready.count, 5);
    }
}
