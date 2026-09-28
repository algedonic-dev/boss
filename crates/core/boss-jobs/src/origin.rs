//! WHERE A FILED ITEM CAME FROM, AS A READER GROUPS IT — the one rule the
//! receiving board (`apps/web/src/it/receiving/receiving.ts`, through the
//! jobs list's `origin=true`) and the receiving REGION both read, the way
//! both read [`crate::channels::lane_of`] for the lane.
//!
//! WHY (design 3036296f mechanism D, backlog 9b473d4a, car 2 of 3). The
//! filing door now records `metadata.source` as `{kind, id}` or
//! `{kind, branch}` (`boss job file --source`, car 1, in
//! `boss-cli/src/item_source.rs`) and `metadata.area` as a slug. The
//! receiving yard showed one flat pile: measured 2026-09-27, 359 open
//! backlog-items, 105 of them untriaged, NONE with a structured source,
//! 74 with a prose one and 178 with one of three ad-hoc keys each filer
//! spelled its own way. A pile cannot be grouped by a sentence.
//!
//! THE KEY. A recorded source groups by `kind` + `branch` when it carries
//! one, else `kind` + `id` — so a car filed by its branch and the same
//! car filed by its id (which the door resolves and stamps WITH the
//! branch) land in one group.
//!
//! NEVER DROPPED. Anything that is not a readable structured source —
//! prose, only an ad-hoc key, an object this vocabulary cannot read, or
//! nothing — reads as unrecorded, with a basis that says which, so the
//! board draws a visible "no recorded source" group and the region counts
//! it. A prose sentence is never parsed into a source here: that would be
//! a guess drawn as a fact. The ad-hoc keys are mapped once, CHECKED
//! against the system of record, by `boss job backfill-source`, which
//! reads [`AD_HOC_KEYS`] from here.

use serde::Serialize;
use serde_json::Value;

/// The metadata key a structured source is recorded under.
pub const SOURCE_KEY: &str = "source";
/// The metadata key the area is recorded under.
pub const AREA_KEY: &str = "area";

/// Every source kind a filer may record, in the filing door's order —
/// `boss-cli`'s `item_source::SourceKind::ALL` is pinned equal to this
/// list (CLAUDE.md §9a), so a kind the door can write is a kind this
/// reader can read.
pub const SOURCE_KINDS: [&str; 5] = ["car", "gate-run", "agent-run", "packet", "review"];

/// The three keys filers used for provenance before the structured
/// source existed, each spelled differently by each filer: a car's
/// branch or id, a packet id, and a sentence naming who found it.
/// Measured 2026-09-27 on the 359 open backlog-items: 25, 83 and 70.
pub const AD_HOC_KEYS: [&str; 3] = ["source_car", "source_packet", "found_by"];

/// Why a source reads the way it does. Only `Recorded` groups by source;
/// every other basis is the "no recorded source" group, named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceBasis {
    /// A structured `source` of a known kind naming an id or a branch.
    Recorded,
    /// No readable structured source, but one of [`AD_HOC_KEYS`] — what
    /// `boss job backfill-source` can map.
    AdHoc,
    /// `source` is a sentence.
    Prose,
    /// `source` is an object this vocabulary cannot read.
    Unreadable,
    /// Nothing about where it came from.
    Missing,
}

/// A packet's source as a reader groups it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReading {
    pub basis: SourceBasis,
    /// The recorded kind, one of [`SOURCE_KINDS`]; `None` unless recorded.
    pub kind: Option<&'static str>,
    /// The grouping key — `<kind>:<branch>` when a branch is recorded,
    /// else `<kind>:<id>`; `None` unless recorded.
    pub key: Option<String>,
    pub id: Option<String>,
    pub branch: Option<String>,
}

impl SourceBasis {
    /// Every basis, so a reader's vocabulary can be pinned to this one.
    pub const ALL: [SourceBasis; 5] = [
        SourceBasis::Recorded,
        SourceBasis::AdHoc,
        SourceBasis::Prose,
        SourceBasis::Unreadable,
        SourceBasis::Missing,
    ];
}

impl SourceReading {
    fn unrecorded(basis: SourceBasis) -> Self {
        SourceReading {
            basis,
            kind: None,
            key: None,
            id: None,
            branch: None,
        }
    }
}

/// What the jobs list puts on a row under `origin=true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OriginReading {
    pub source: SourceReading,
    /// The recorded area, or `None` when the filer named none.
    pub area: Option<String>,
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// THE SOURCE A READER GROUPS BY — see the module doc for the rule.
pub fn source_of(metadata: &Value) -> SourceReading {
    let has_ad_hoc = || AD_HOC_KEYS.iter().any(|k| text(metadata.get(*k)).is_some());
    match metadata.get(SOURCE_KEY) {
        Some(Value::Object(o)) => {
            let kind = o
                .get("kind")
                .and_then(Value::as_str)
                .and_then(|k| SOURCE_KINDS.into_iter().find(|s| *s == k));
            let id = text(o.get("id"));
            let branch = text(o.get("branch"));
            match (kind, branch.as_ref().or(id.as_ref())) {
                (Some(kind), Some(named)) => SourceReading {
                    basis: SourceBasis::Recorded,
                    kind: Some(kind),
                    key: Some(format!("{kind}:{named}")),
                    id,
                    branch,
                },
                _ if has_ad_hoc() => SourceReading::unrecorded(SourceBasis::AdHoc),
                _ => SourceReading::unrecorded(SourceBasis::Unreadable),
            }
        }
        _ if has_ad_hoc() => SourceReading::unrecorded(SourceBasis::AdHoc),
        Some(v) if text(Some(v)).is_some() => SourceReading::unrecorded(SourceBasis::Prose),
        _ => SourceReading::unrecorded(SourceBasis::Missing),
    }
}

/// The area a filer recorded, trimmed; `None` for none or an empty one.
pub fn area_of(metadata: &Value) -> Option<String> {
    text(metadata.get(AREA_KEY))
}

pub fn origin_of(metadata: &Value) -> OriginReading {
    OriginReading {
        source: source_of(metadata),
        area: area_of(metadata),
    }
}

/// How `n` sources of a kind are named in a sentence — "71 items from
/// 14 car reviews", "1 item from 1 agent run".
pub fn kind_noun(kind: &str, n: usize) -> &'static str {
    let (one, many) = match kind {
        "car" => ("car", "cars"),
        "gate-run" => ("gate run", "gate runs"),
        "agent-run" => ("agent run", "agent runs"),
        "review" => ("car review", "car reviews"),
        _ => ("packet", "packets"),
    };
    if n == 1 { one } else { many }
}

fn items(n: usize) -> String {
    format!("{n} item{}", if n == 1 { "" } else { "s" })
}

/// One tallied item: its metadata and whether it is urgent.
pub struct Tallied<'a> {
    pub metadata: &'a Value,
    pub urgent: bool,
}

/// THE BY-SOURCE SENTENCE (design 3036296f's own example: "71 findings
/// from 14 car reviews, 9 blocking"): per recorded kind, most items
/// first, how many items came from how many distinct sources and how
/// many of them are urgent; then the unrecorded count, ALWAYS, because
/// an empty "no recorded source" is an answer too. Urgent is the
/// priority the item carries — the one severity every item records.
/// Returns the number of distinct recorded sources and the sentence.
pub fn by_source(items_: &[Tallied<'_>]) -> (usize, String) {
    use std::collections::{BTreeMap, BTreeSet};
    let mut kinds: BTreeMap<&'static str, (usize, BTreeSet<String>, usize)> = BTreeMap::new();
    let mut unrecorded = 0usize;
    for it in items_ {
        let s = source_of(it.metadata);
        match (s.kind, s.key) {
            (Some(kind), Some(key)) => {
                let e = kinds.entry(kind).or_default();
                e.0 += 1;
                e.1.insert(key);
                e.2 += usize::from(it.urgent);
            }
            _ => unrecorded += 1,
        }
    }
    let mut rows: Vec<_> = kinds.into_iter().collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    let sources = rows.iter().map(|(_, (_, s, _))| s.len()).sum();
    let mut parts: Vec<String> = rows
        .iter()
        .map(|(kind, (n, s, urgent))| {
            let head = format!(
                "{} from {} {}",
                items(*n),
                s.len(),
                kind_noun(kind, s.len())
            );
            if *urgent > 0 {
                format!("{head}, {urgent} urgent")
            } else {
                head
            }
        })
        .collect();
    parts.push(format!("{} with no recorded source", items(unrecorded)));
    (sources, parts.join("; "))
}

/// THE BY-AREA SENTENCE: how many items in how many areas, the three
/// largest named, and the items with no area, always. Returns the
/// number of distinct areas and the sentence.
pub fn by_area(items_: &[Tallied<'_>]) -> (usize, String) {
    use std::collections::BTreeMap;
    let mut areas: BTreeMap<String, usize> = BTreeMap::new();
    let mut none = 0usize;
    for it in items_ {
        match area_of(it.metadata) {
            Some(a) => *areas.entry(a).or_default() += 1,
            None => none += 1,
        }
    }
    let mut rows: Vec<_> = areas.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let with = items_.len() - none;
    let top: Vec<String> = rows
        .iter()
        .take(3)
        .map(|(a, n)| format!("{a} {n}"))
        .collect();
    let head = if rows.is_empty() {
        "no item names an area".to_string()
    } else {
        format!(
            "{} in {} area{} ({})",
            items(with),
            rows.len(),
            if rows.len() == 1 { "" } else { "s" },
            top.join(", ")
        )
    };
    (rows.len(), format!("{head}; {} with no area", items(none)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_structured_source_groups_by_its_branch_else_its_id() {
        let by_branch = source_of(&json!({"source": {"kind": "review", "branch": "fix/x"}}));
        assert_eq!(by_branch.basis, SourceBasis::Recorded);
        assert_eq!(by_branch.key.as_deref(), Some("review:fix/x"));
        // The door stamps a car named by id WITH its branch: one group.
        let both = source_of(&json!({"source": {
            "kind": "review", "id": "ed84acc8-90ac-4403-9dc9-2cff0e537474", "branch": "fix/x"}}));
        assert_eq!(both.key, by_branch.key);
        assert_eq!(
            both.id.as_deref(),
            Some("ed84acc8-90ac-4403-9dc9-2cff0e537474")
        );
        let by_id = source_of(&json!({"source": {"kind": "agent-run", "id": "665c7419"}}));
        assert_eq!(by_id.key.as_deref(), Some("agent-run:665c7419"));
        assert_eq!(by_id.kind, Some("agent-run"));
    }

    /// Prose, ad-hoc keys, an unreadable object and silence each read as
    /// unrecorded, named — never guessed into a source, never dropped.
    #[test]
    fn every_other_shape_reads_unrecorded_with_the_reason() {
        let cases = [
            (
                json!({"source": "review of car fix/x (42da8bd2)"}),
                SourceBasis::Prose,
            ),
            (json!({"source_car": "fix/x"}), SourceBasis::AdHoc),
            (
                json!({"source": "prose", "source_packet": "8cd38edd"}),
                SourceBasis::AdHoc,
            ),
            (
                json!({"found_by": "the builder of e6b2066f (run e661eaac)"}),
                SourceBasis::AdHoc,
            ),
            (
                json!({"source": {"kind": "vibes", "id": "1a2b3c4d"}}),
                SourceBasis::Unreadable,
            ),
            (json!({"source": {"kind": "car"}}), SourceBasis::Unreadable),
            (
                json!({"source": {"kind": "car", "id": "  "}, "found_by": "x"}),
                SourceBasis::AdHoc,
            ),
            (json!({"source": ""}), SourceBasis::Missing),
            (json!({"found_by": ""}), SourceBasis::Missing),
            (json!({}), SourceBasis::Missing),
        ];
        for (md, want) in cases {
            let got = source_of(&md);
            assert_eq!(got.basis, want, "{md}");
            assert_eq!(got.key, None, "{md}");
            assert_eq!(got.kind, None, "{md}");
        }
    }

    #[test]
    fn the_reading_serialises_in_the_words_the_board_parses() {
        assert_eq!(
            serde_json::to_value(origin_of(&json!({
                "area": " jobs ", "source": {"kind": "car", "branch": "fix/x"}})))
            .unwrap(),
            json!({"source": {"basis": "recorded", "kind": "car", "key": "car:fix/x",
                              "id": null, "branch": "fix/x"},
                   "area": "jobs"})
        );
        assert_eq!(
            serde_json::to_value(origin_of(&json!({"source_car": "fix/x", "area": ""}))).unwrap(),
            json!({"source": {"basis": "ad-hoc", "kind": null, "key": null,
                              "id": null, "branch": null},
                   "area": null})
        );
    }

    fn t(md: &Value, urgent: bool) -> Tallied<'_> {
        Tallied {
            metadata: md,
            urgent,
        }
    }

    /// Design 3036296f's own example, "71 findings from 14 car reviews,
    /// 9 blocking", in the small: counts per kind, distinct sources,
    /// urgent — and the unrecorded group always said.
    #[test]
    fn the_by_source_sentence_counts_items_sources_and_urgent_per_kind() {
        let r1 = json!({"source": {"kind": "review", "branch": "fix/a"}});
        let r2 = json!({"source": {"kind": "review", "branch": "fix/b"}});
        let run = json!({"source": {"kind": "agent-run", "id": "665c7419"}});
        let prose = json!({"source": "run 38eed035 handback"});
        let none = json!({});
        let all = [
            t(&r1, true),
            t(&r1, false),
            t(&r2, true),
            t(&run, false),
            t(&prose, true),
            t(&none, false),
        ];
        assert_eq!(
            by_source(&all),
            (
                3,
                "3 items from 2 car reviews, 2 urgent; 1 item from 1 agent run; \
                 2 items with no recorded source"
                    .to_string()
            )
        );
        assert_eq!(
            by_source(&[]),
            (0, "0 items with no recorded source".to_string())
        );
    }

    #[test]
    fn the_by_area_sentence_names_the_largest_three_and_the_rest() {
        let md: Vec<Value> = ["jobs", "jobs", "web", "forge", "forge", "forge", "cli"]
            .iter()
            .map(|a| json!({"area": a}))
            .chain([json!({}), json!({"area": ""})])
            .collect();
        let all: Vec<Tallied<'_>> = md.iter().map(|m| t(m, false)).collect();
        assert_eq!(
            by_area(&all),
            (
                4,
                "7 items in 4 areas (forge 3, jobs 2, cli 1); 2 items with no area".to_string()
            )
        );
        let none = json!({});
        assert_eq!(
            by_area(&[t(&none, false)]),
            (0, "no item names an area; 1 item with no area".to_string())
        );
    }
}
