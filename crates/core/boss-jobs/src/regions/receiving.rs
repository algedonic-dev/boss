//! RECEIVING: inbound packets standing until an actor takes them in —
//! with the receiving yard's pipeline set and age bands, the second of
//! the two client-side predicates ported here once (`receiving.ts`) and
//! pinned against the TypeScript.

use super::*;
use crate::channels::{LaneBasis, lane_of};

/// The delivery pipeline's kinds — a packet of these is a car in
/// transit or the machinery moving it, never a request, so the
/// receiving yard leaves them to the train yard. `receiving.ts::
/// PIPELINE_KINDS`, ported; the test `the_pipeline_kinds_match_the_
/// receiving_yard` holds the two equal.
pub const PIPELINE_KINDS: [&str; 9] = [
    "pr-train",
    "gate-run",
    "ship-a-change",
    "ops-request",
    "park-a-job",
    "emergency-merge",
    "repair-a-train",
    "publish-request",
    "regenerate-deployment",
];

/// The receiving yard's age bands, in days from `opened_on`: triage
/// within 3 days, anything within 14 (`receiving.ts::AGE_THRESHOLDS`,
/// ported and pinned by the same test).
pub const AGING_DAYS: i64 = 3;
pub const STALE_DAYS: i64 = 14;

/// Which kinds are inbound: active platform-category kinds that are
/// neither chores (`maintenance-*`) nor the pipeline
/// (`receiving.ts::inboundKinds`). Derived from the registry, never
/// listed by kind.
pub fn inbound_kinds(workflows: &[WorkflowSpec]) -> Vec<String> {
    let mut kinds: Vec<String> = workflows
        .iter()
        .filter(|w| w.status == crate::registry::WorkflowStatus::Active)
        .filter(|w| w.category == "platform")
        .filter(|w| {
            !w.kind.starts_with("maintenance-") && !PIPELINE_KINDS.contains(&w.kind.as_str())
        })
        .map(|w| w.kind.clone())
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// RECEIVING: inbound packets standing — feedback, alarms, findings,
/// design questions — from the moment they open until an actor takes
/// them in: until the intake step completes ([`taken_in`]), after which
/// the packet is marshalling's, never both ([`members`]). The receiving
/// yard's own bands decide the state: any
/// packet older than [`STALE_DAYS`] is troubled, older than
/// [`AGING_DAYS`] attention. The trend is inbound arrivals per day.
pub(super) fn receiving(inputs: &RegionInputs<'_>, w: &Windows) -> Region {
    const UNIT: &str = "packets standing";
    let Some(inbound) = inputs.inbound else {
        return region(
            "receiving",
            None,
            None,
            UNIT,
            unread_settled(
                "the workflow registry that names the inbound kinds could not be read",
                inputs.now,
            ),
            Trend {
                metric: "inbound".to_string(),
                unit: "per day".to_string(),
                current: None,
                previous: None,
                samples: 0,
                previous_samples: 0,
            },
            vec![measure("oldest untriaged", None, "days")],
        );
    };
    let (cur, prev) = count_split(w, inbound.iter().filter_map(|(j, _)| opened_at(j)));
    let trend = rate_trend("inbound", w, cur, prev);
    let today = inputs.now.date_naive();
    // THE PARTITION (design 62de32ae decision 4): standing here means not
    // yet taken in, and held by no region of its own.
    let open: Vec<&Job> = receiving_standing(inputs).unwrap_or_default();
    let oldest_day = open.iter().map(|j| j.opened_on).min();
    let oldest = oldest_day.map_or(0, |d| (today - d).num_days());
    let n = open.len();
    // A band in whole days from `opened_on` is crossed at the midnight
    // that makes the age exceed it — which IS its onset, read off the
    // packet's own date.
    let crossed = |band_days: i64| {
        oldest_day
            .and_then(|d| d.checked_add_days(chrono::Days::new(u64::try_from(band_days + 1).ok()?)))
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|t| chrono::DateTime::from_naive_utc_and_offset(t, chrono::Utc))
    };
    let mut findings = Vec::new();
    if oldest > STALE_DAYS {
        findings.push(Finding::new(
            bands::RECEIVING_STALE,
            crossed(STALE_DAYS),
            format!("oldest {oldest}d"),
            format!(
                "{} standing, the oldest {oldest} days (past the {STALE_DAYS}-day band)",
                plural(n, "packet", "packets")
            ),
        ));
    }
    if oldest > AGING_DAYS {
        findings.push(Finding::new(
            bands::RECEIVING_AGING,
            crossed(AGING_DAYS),
            format!("oldest {oldest}d"),
            format!(
                "{} standing, the oldest {oldest} days (past the {AGING_DAYS}-day triage band)",
                plural(n, "packet", "packets")
            ),
        ));
    }
    let settled = settle(
        findings,
        format!("{} standing", plural(n, "packet", "packets")),
        inputs.now,
    );
    // THE KPI (decision 9): the oldest untriaged, and the share of what
    // stands whose filer recorded no lane — primary, so the map's one
    // line still reads them first; then where the untriaged items came
    // from, by source and by area (design 3036296f mechanism D).
    #[allow(clippy::cast_precision_loss)]
    let kpi: Vec<Measure> = vec![
        match oldest_day {
            Some(_) => measure("oldest untriaged", Some(oldest as f64), "days"),
            None => measure_said(
                "oldest untriaged",
                None,
                "days",
                "nothing untriaged".to_string(),
            ),
        },
        unrecorded_share(&open),
    ]
    .into_iter()
    .chain(origin_measures(&open))
    .collect();
    region("receiving", Some(n), None, UNIT, settled, trend, kpi)
}

/// The kind whose standing items are grouped by where they came from:
/// the one the filing door records a source on (`boss job file
/// --source`, backlog 9b473d4a).
const SOURCED_KIND: &str = "backlog-item";

/// WHERE THE UNTRIAGED ITEMS CAME FROM (design 3036296f mechanism D):
/// the standing backlog-items by recorded source and by area, each a
/// sentence from [`crate::origin`] — the rule the receiving board groups
/// its `origin=true` rows by, so the two cannot count differently.
/// Nothing standing is said, never a sentence of zeros.
fn origin_measures(standing: &[&Job]) -> [Measure; 2] {
    const BY_SOURCE: &str = "untriaged items by source";
    const BY_AREA: &str = "untriaged items by area";
    let items: Vec<crate::origin::Tallied<'_>> = standing
        .iter()
        .filter(|j| j.kind == SOURCED_KIND)
        .map(|j| crate::origin::Tallied {
            metadata: &j.metadata,
            urgent: j.priority == boss_core::job::Priority::Urgent,
        })
        .collect();
    if items.is_empty() {
        let none = |name: &str, unit: &str| {
            measure_said(
                name,
                None,
                unit,
                format!("{name}: no backlog item standing"),
            )
        };
        return [none(BY_SOURCE, "sources"), none(BY_AREA, "areas")];
    }
    let (sources, source_text) = crate::origin::by_source(&items);
    let (areas, area_text) = crate::origin::by_area(&items);
    [
        measure_said(BY_SOURCE, count_value(sources), "sources", source_text),
        measure_said(BY_AREA, count_value(areas), "areas", area_text),
    ]
}

/// THE SHARE WHOSE CHANNEL IS UNRECORDED — decision 9's second half,
/// named residue on backlog c3105b2a until the lane rule moved to the
/// server (backlog 1eea4554): it was the client's (`receiving.ts::
/// channelOf`), in a six-lane vocabulary of its own, so porting it would
/// have been a second copy to pin. Now both read [`lane_of`], the one
/// rule, and the board draws the lane the jobs list classified.
/// Measured over what STANDS, the region's own count, as a whole percent.
/// Nothing standing is no reading, never a 0% that says every filer
/// named their lane.
fn unrecorded_share(standing: &[&Job]) -> Measure {
    const NAME: &str = "channel unrecorded";
    let of = standing.len();
    if of == 0 {
        return measure_said(NAME, None, "%", format!("{NAME}: nothing standing"));
    }
    let unrecorded = standing
        .iter()
        .filter(|j| lane_of(&j.metadata).basis == LaneBasis::Unclassified)
        .count();
    #[allow(clippy::cast_precision_loss)]
    let pct = (unrecorded as f64 * 100.0 / of as f64).round();
    measure_said(
        NAME,
        Some(pct),
        "%",
        format!(
            "{NAME} {}% ({unrecorded} of {of} standing)",
            number_text(pct)
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// Receiving: the age bands from `receiving.ts`, and arrivals per
    /// day from the packets' own open stamps.
    #[test]
    fn receiving_reads_the_age_bands_and_inbound_arrivals() {
        let mut fresh = job(
            "user-feedback",
            "fresh",
            JobStatus::Open,
            json!({ "opened_at": "2026-09-19T09:00:00Z" }),
        );
        fresh.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
        let mut aging = job("backlog-item", "aging", JobStatus::Open, json!({}));
        aging.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let mut stale = job("backlog-item", "stale", JobStatus::Open, json!({}));
        stale.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let mut yesterday = job(
            "user-feedback",
            "done",
            JobStatus::Closed,
            json!({ "opened_at": "2026-09-18T09:00:00Z" }),
        );
        yesterday.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let status = empty_status();
        let inbound = untaken(vec![fresh.clone(), aging.clone()]);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(2));
        // Past the triage band: ATTENTION, with the band named against
        // the number beside it (design 62de32ae, decision 1) — and the
        // band was crossed the midnight the oldest turned four days old.
        assert_eq!(r.state, RegionState::Attention, "{}", r.why);
        let band = r.band.as_ref().unwrap();
        assert_eq!(band.id, "receiving-aging");
        assert_eq!(band.reads, "oldest 5d > the 3-day triage band");
        assert_eq!(band.since.as_deref(), Some("2026-09-18T00:00:00+00:00"));
        assert_eq!(band.held_minutes, Some(36 * 60));
        assert_eq!(r.unit, "packets standing");
        assert_eq!(r.kpi[0].text, "oldest untriaged 5 days");
        assert_eq!(r.trend.current, Some(1.0));
        assert_eq!(r.trend.previous, Some(0.0));

        let inbound = untaken(vec![fresh, aging, stale, yesterday]);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(3));
        assert_eq!(r.state, RegionState::Troubled, "{}", r.why);
        assert!(r.why.contains("30 days"), "{}", r.why);
        assert_eq!(r.trend.previous, Some(1.0));
    }

    /// THE KPI's SECOND HALF (design 62de32ae decision 9, ported by
    /// backlog 1eea4554): of the packets standing, the share whose filer
    /// recorded no lane — read through `channels::lane_of`, the one rule
    /// the board's rows are classified by too, so the region and the
    /// board cannot count two different things.
    #[test]
    fn receiving_measures_the_share_of_standing_packets_with_no_recorded_lane() {
        let recorded = job(
            "backlog-item",
            "said",
            JobStatus::Open,
            json!({ "input_channel": "review-finding" }),
        );
        // A key no filer writes, and a near miss: neither is a lane.
        let old_key = job(
            "backlog-item",
            "old key",
            JobStatus::Open,
            json!({ "channel": "monitoring" }),
        );
        let near_miss = job(
            "user-feedback",
            "near miss",
            JobStatus::Open,
            json!({ "input_channel": "telemetry" }),
        );
        let silent = job("design-doc", "silent", JobStatus::Open, json!({}));
        let status = empty_status();
        let inbound = untaken(vec![recorded, old_key, near_miss, silent]);
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&inbound),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        // Primary two, then the by-source and by-area sentences
        // (backlog 9b473d4a).
        assert_eq!(r.kpi.len(), 4, "{:?}", r.kpi);
        let share = &r.kpi[1];
        assert_eq!(share.name, "channel unrecorded");
        assert_eq!(share.unit, "%");
        assert_eq!(share.value, Some(75.0));
        assert_eq!(share.text, "channel unrecorded 75% (3 of 4 standing)");

        // Nothing standing: no share, said so — never a 0% that reads
        // as every filer saying where their work came from.
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&untaken(vec![])),
            Some(&[]),
        ));
        let share = &by_name(&out, "receiving").kpi[1];
        assert_eq!(share.value, None);
        assert_eq!(share.text, "channel unrecorded: nothing standing");
    }

    /// WHERE THE UNTRIAGED ITEMS CAME FROM (design 3036296f mechanism D,
    /// backlog 9b473d4a): of the backlog-items standing, grouped by the
    /// source the filer recorded and by area, through `crate::origin` —
    /// the rule the board's `origin=true` rows are grouped by too. Only
    /// backlog-items: the other inbound kinds are not filed with a
    /// source. The unrecorded are counted, always, never dropped.
    #[test]
    fn receiving_groups_its_standing_backlog_items_by_source_and_area() {
        let src = |kind: &str, branch: &str, urgent: bool, area: Option<&str>| {
            let mut j = job(
                "backlog-item",
                branch,
                JobStatus::Open,
                json!({ "source": { "kind": kind, "branch": branch }, "area": area }),
            );
            if urgent {
                j.priority = boss_core::job::Priority::Urgent;
            }
            j
        };
        let standing = vec![
            src("review", "fix/a", true, Some("jobs")),
            src("review", "fix/a", false, Some("jobs")),
            src("review", "fix/b", true, Some("web")),
            job(
                "backlog-item",
                "prose",
                JobStatus::Open,
                json!({ "source": "run 38eed035 handback", "area": "jobs" }),
            ),
            job(
                "backlog-item",
                "ad hoc",
                JobStatus::Open,
                json!({ "found_by": "the builder of e6b2066f (run e661eaac)" }),
            ),
            // Not a backlog-item: not tallied by source.
            job(
                "user-feedback",
                "feedback",
                JobStatus::Open,
                json!({ "area": "web" }),
            ),
        ];
        let status = empty_status();
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&untaken(standing)),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        assert_eq!(r.count, Some(6), "every inbound packet still stands");
        let by_source = r
            .kpi
            .iter()
            .find(|m| m.name == "untriaged items by source")
            .unwrap_or_else(|| panic!("no by-source measure: {:?}", r.kpi));
        assert_eq!(by_source.value, Some(2.0), "two distinct sources");
        assert_eq!(by_source.unit, "sources");
        assert_eq!(
            by_source.text,
            "3 items from 2 car reviews, 2 urgent; 2 items with no recorded source"
        );
        let by_area = r
            .kpi
            .iter()
            .find(|m| m.name == "untriaged items by area")
            .unwrap_or_else(|| panic!("no by-area measure: {:?}", r.kpi));
        assert_eq!(by_area.value, Some(2.0));
        assert_eq!(
            by_area.text,
            "4 items in 2 areas (jobs 3, web 1); 1 item with no area"
        );

        // Nothing standing: said, never a sentence of zeros that reads
        // as every item carrying its source.
        let out = regions(&inputs(
            &status,
            &[],
            &[],
            &[],
            &[],
            Some(&untaken(vec![])),
            Some(&[]),
        ));
        let r = by_name(&out, "receiving");
        let by_source = r
            .kpi
            .iter()
            .find(|m| m.name == "untriaged items by source")
            .unwrap_or_else(|| panic!("no by-source measure: {:?}", r.kpi));
        assert_eq!(by_source.value, None);
        assert_eq!(
            by_source.text,
            "untriaged items by source: no backlog item standing"
        );
    }

    /// The two facts ported from the receiving yard's TypeScript live
    /// twice; this holds them equal (CLAUDE.md §9a). Reads the source
    /// the client is built from, so a kind added to one side and not
    /// the other names itself here.
    #[test]
    fn the_pipeline_kinds_and_age_bands_match_the_receiving_yard() {
        let src = std::fs::read_to_string(
            boss_testing::repo_root().join("apps/web/src/it/receiving/receiving.ts"),
        )
        .expect("receiving.ts is in the tree");
        let set = src
            .split("PIPELINE_KINDS: ReadonlySet<string> = new Set([")
            .nth(1)
            .and_then(|rest| rest.split("]);").next())
            .expect("receiving.ts declares PIPELINE_KINDS as a Set literal");
        let ts_kinds: Vec<&str> = set
            .split(',')
            .map(|s| s.trim().trim_matches('\''))
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            ts_kinds, PIPELINE_KINDS,
            "receiving.ts::PIPELINE_KINDS drifted from regions.rs::PIPELINE_KINDS"
        );
        assert!(
            src.contains(&format!(
                "AGE_THRESHOLDS = {{ aging: {AGING_DAYS}, stale: {STALE_DAYS} }}"
            )),
            "receiving.ts::AGE_THRESHOLDS drifted from regions.rs::AGING_DAYS / STALE_DAYS"
        );
        // The train-gate keys the yard's `readTrainGate` reads.
        let yard =
            std::fs::read_to_string(boss_testing::repo_root().join("apps/web/src/it/yard/yard.ts"))
                .expect("yard.ts is in the tree");
        for key in [TRAIN_GATE_WAIT_REASON, TRAIN_GATE_FALLBACK] {
            assert!(
                yard.contains(&format!("md.{key}")),
                "yard.ts no longer reads {key}"
            );
        }
    }

    /// THE BOARD DRAWS THE SERVER'S LANE (backlog 1eea4554): the rule is
    /// [`lane_of`] alone, so the receiving yard's TypeScript asks the
    /// list for it (`lane=true`) and keeps no rule of its own — no read
    /// of the recorded key, none of the key no filer writes, no kind
    /// lists. A client that grows its own classification again names
    /// itself here, the way the pipeline kinds above do (CLAUDE.md §9a).
    #[test]
    fn the_receiving_yard_draws_the_lane_the_server_read() {
        let src = std::fs::read_to_string(
            boss_testing::repo_root().join("apps/web/src/it/receiving/receiving.ts"),
        )
        .expect("receiving.ts is in the tree");
        assert!(
            src.contains("&lane=true"),
            "receiving.ts must ask the jobs list for the server's lane"
        );
        for (word, why) in [
            (
                crate::channels::RECORDED_KEY,
                "reads the recorded key itself",
            ),
            ("md.channel", "reads a key no filer writes"),
            ("DESIGN_KINDS", "keeps a kind list"),
            ("PROTOCOL_KINDS", "keeps a kind list"),
            ("MONITORING_KINDS", "keeps a kind list"),
        ] {
            assert!(
                !src.contains(word),
                "receiving.ts {why} ({word}); the lane is the server's (channels::lane_of)"
            );
        }
        // The two bases a reading can have, spelled as the server spells them.
        for basis in [LaneBasis::Recorded, LaneBasis::Unclassified] {
            let word = serde_json::to_value(basis).unwrap();
            let word = word.as_str().unwrap();
            assert!(
                src.contains(&format!("'{word}'")),
                "receiving.ts no longer parses the basis '{word}'"
            );
        }
    }

    /// THE BOARD GROUPS BY THE SERVER'S ORIGIN (backlog 9b473d4a), the
    /// way it draws the server's lane: receiving.ts asks the list for
    /// `origin=true`, reads none of the ad-hoc keys itself, parses every
    /// basis in the server's spelling, tallies the same kind, and names
    /// each source kind with the nouns `origin::kind_noun` uses — so the
    /// board's "71 items from 14 car reviews" and the region's cannot
    /// read two ways (CLAUDE.md §9a).
    #[test]
    fn the_receiving_yard_groups_by_the_origin_the_server_read() {
        use crate::origin::{AD_HOC_KEYS, SOURCE_KINDS, SourceBasis, kind_noun};
        let src = std::fs::read_to_string(
            boss_testing::repo_root().join("apps/web/src/it/receiving/receiving.ts"),
        )
        .expect("receiving.ts is in the tree");
        assert!(
            src.contains("&origin=true"),
            "receiving.ts must ask the jobs list for the server's origin"
        );
        for key in AD_HOC_KEYS {
            assert!(
                !src.contains(key),
                "receiving.ts reads the ad-hoc key {key} itself; the source is the server's \
                 (origin::source_of)"
            );
        }
        for basis in SourceBasis::ALL {
            let word = serde_json::to_value(basis).unwrap();
            let word = word.as_str().unwrap().to_string();
            assert!(
                src.contains(&format!("'{word}'")),
                "receiving.ts no longer parses the source basis '{word}'"
            );
        }
        assert!(
            src.contains(&format!("SOURCED_KIND = '{SOURCED_KIND}'")),
            "receiving.ts::SOURCED_KIND drifted from regions/receiving.rs::SOURCED_KIND"
        );
        for kind in SOURCE_KINDS {
            let nouns = format!(
                "['{kind}', '{}', '{}']",
                kind_noun(kind, 1),
                kind_noun(kind, 2)
            );
            assert!(
                src.contains(&nouns),
                "receiving.ts::KIND_NOUNS drifted from origin::kind_noun — expected {nouns}"
            );
        }
    }

    #[test]
    fn inbound_kinds_are_active_platform_kinds_minus_chores_and_the_pipeline() {
        let spec = |kind: &str, category: &str| WorkflowSpec {
            kind: kind.into(),
            version: 1,
            status: crate::registry::WorkflowStatus::Active,
            label: kind.into(),
            description: None,
            category: category.into(),
            subject_kinds: vec![],
            steps: vec![],
            metadata_schema: json!({}),
            entitlements: json!({}),
            metadata: json!({}),
            on_complete_create: vec![],
            owning_team: "it".into(),
            authoring_job_id: None,
            created_at: t(NOW),
        };
        let specs = vec![
            spec("user-feedback", "platform"),
            spec("backlog-item", "platform"),
            spec("maintenance-nightly", "platform"),
            spec("pr-train", "platform"),
            spec("sale", "commerce"),
        ];
        assert_eq!(inbound_kinds(&specs), vec!["backlog-item", "user-feedback"]);
    }
}
