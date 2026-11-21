//! SUPERSEDED AREAS REFUSE NEW ITEMS (backlog f4b0ad41 rule 1; design
//! 25511015 Q4, decided by David 2026-11-17).
//!
//! WHY. The backlog grew every week since W40, and reconstruction onto
//! the station tree closes only about 5% of what is open, so intake is
//! one of the two levers. A new backlog item about an area a
//! reconstruction step deletes — the v1 map, regions, departments as an
//! entity, peer nominations and pane nudges, systemd timers — is work
//! filed against code that will not exist. Admission refuses it and
//! names the step that retires the area, so the filer learns where the
//! work went instead of adding to the pile.
//!
//! THE LIST IS DATA: `infra/platform/superseded-areas.toml`, compiled in
//! here, so adding an area is adding a row (its header says how). The
//! refusal is at ADMISSION, not in the CLI, so `boss job file` and every
//! other caller of `POST /api/jobs` meet it from one place — the CLI
//! posts through this door and prints its 422 (pinned through the real
//! router by boss-cli's job.rs test
//! `a_backlog_item_about_a_superseded_area_is_refused_naming_its_step`).
//!
//! WHAT IS JUDGED: only the recorded `metadata.area`, trimmed, matched
//! exactly against an entry's `areas`. A title's words are not parsed
//! into an area — that would be a guess drawn as a fact, the rule
//! `crate::origin` keeps for the source.

use serde::Deserialize;
use serde_json::{Value, json};

/// The superseded areas, compiled in from the one file that holds them.
pub const SUPERSEDED_AREAS_TOML: &str =
    include_str!("../../../../infra/platform/superseded-areas.toml");

/// Where [`SUPERSEDED_AREAS_TOML`] lives, for a refusal to name.
pub const SUPERSEDED_AREAS_PATH: &str = "infra/platform/superseded-areas.toml";

/// The one kind this refuses: intake is the backlog's.
pub const REFUSED_KIND: &str = "backlog-item";

/// The design whose §4 numbers the reconstruction steps.
pub const DESIGN: &str = "25511015";

/// One superseded area, as its row declares it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupersededArea {
    /// The entry's own slug.
    pub name: String,
    /// The sentence a filer matches a change against.
    pub what: String,
    /// The `metadata.area` slugs refused.
    pub areas: Vec<String>,
    /// The reconstruction step's number in design 25511015 §4.
    pub step: u32,
    /// That step's heading there.
    pub step_title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    area: Vec<SupersededArea>,
}

/// Every superseded area, parsed from [`SUPERSEDED_AREAS_TOML`].
pub fn superseded_areas() -> Result<Vec<SupersededArea>, toml::de::Error> {
    parse(SUPERSEDED_AREAS_TOML)
}

fn parse(text: &str) -> Result<Vec<SupersededArea>, toml::de::Error> {
    toml::from_str::<File>(text).map(|f| f.area)
}

/// The entry that supersedes `area`, if one does.
pub fn superseding<'a>(entries: &'a [SupersededArea], area: &str) -> Option<&'a SupersededArea> {
    let area = area.trim();
    entries.iter().find(|e| e.areas.iter().any(|a| a == area))
}

/// The 422 body admission answers a create of `kind` carrying
/// `metadata`, or `None` when it is not refused here. A list that does
/// not parse refuses every backlog-item create, loudly and naming the
/// file — never admits one unjudged; `superseded_areas_parse_and_each_names_its_step`
/// keeps that from reaching main.
pub fn refusal(kind: &str, metadata: &Value) -> Option<Value> {
    if kind != REFUSED_KIND {
        return None;
    }
    let area = crate::origin::area_of(metadata)?;
    let entries = match superseded_areas() {
        Ok(e) => e,
        Err(e) => {
            return Some(json!({
                "error": format!("the superseded-area list does not parse, so no backlog item can be judged against it: {e}"),
                "refused_keys": [crate::origin::AREA_KEY],
                "file": SUPERSEDED_AREAS_PATH,
            }));
        }
    };
    let hit = superseding(&entries, &area)?;
    Some(json!({
        "error": format!(
            "a new backlog item about area {area:?} is refused: {} is superseded, and \
             reconstruction step {} of design {DESIGN} ({}) retires it",
            hit.name, hit.step, hit.step_title
        ),
        "refused_keys": [crate::origin::AREA_KEY],
        "area": area,
        "superseded_area": hit.name,
        "what": hit.what,
        "step": hit.step,
        "step_title": hit.step_title,
        "design": DESIGN,
        "file": SUPERSEDED_AREAS_PATH,
        "hint": format!(
            "the work goes to reconstruction step {}, not to the backlog; an item that BUILDS \
             that step is filed under the area it builds in (station-tree), never the area it deletes",
            hit.step
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped list parses, holds the five areas the decision names,
    /// and every entry names a step and at least one slug a filer could
    /// record — a row that names no step is a refusal that points
    /// nowhere.
    #[test]
    fn superseded_areas_parse_and_each_names_its_step() {
        let entries = superseded_areas().expect("the shipped list parses");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "v1-map",
                "regions",
                "departments",
                "peer-nominations",
                "systemd-timers"
            ],
            "design 25511015 Q4 names these five"
        );
        let mut seen = std::collections::BTreeSet::new();
        for e in &entries {
            assert!(
                (1..=7).contains(&e.step),
                "{} names step {}",
                e.name,
                e.step
            );
            assert!(
                !e.step_title.trim().is_empty(),
                "{} has no step title",
                e.name
            );
            assert!(!e.areas.is_empty(), "{} refuses no area", e.name);
            for a in &e.areas {
                let slug = !a.is_empty()
                    && a.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/'
                    });
                assert!(
                    slug,
                    "{a:?} is not an area slug `boss job file --area` takes"
                );
                assert!(seen.insert(a.clone()), "{a:?} is listed twice");
            }
        }
    }

    #[test]
    fn a_backlog_item_in_a_listed_area_is_refused_naming_its_step() {
        let body = refusal(REFUSED_KIND, &json!({"area": " it-map "})).expect("refused");
        assert_eq!(body["superseded_area"], "v1-map");
        assert_eq!(body["step"], 2);
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("reconstruction step 2"), "{error}");
        assert!(error.contains("converge the visuals"), "{error}");
        let body = refusal(REFUSED_KIND, &json!({"area": "timers"})).expect("refused");
        assert_eq!(body["step"], 6);
    }

    #[test]
    fn another_area_another_kind_or_no_area_is_not_judged_here() {
        assert_eq!(
            refusal(REFUSED_KIND, &json!({"area": "station-tree"})),
            None
        );
        assert_eq!(refusal(REFUSED_KIND, &json!({"area": "it-map/x"})), None);
        assert_eq!(refusal(REFUSED_KIND, &json!({})), None);
        assert_eq!(refusal("agent-run", &json!({"area": "it-map"})), None);
    }

    #[test]
    fn an_unknown_key_in_a_row_does_not_parse() {
        let text =
            "[[area]]\nname='x'\nwhat='w'\nareas=['x']\nstep=1\nstep_title='t'\nretired=true\n";
        assert!(
            parse(text).is_err(),
            "a misspelt key would be read as absent"
        );
    }
}
