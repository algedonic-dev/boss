//! Where a filed packet came from, RECORDED as data at the moment it is
//! known — the car, gate-run, agent-run, packet or review that produced
//! it, and the area it touches. Who filed it is admission's to record
//! (`boss_jobs::opened_by`; backlog 443eedc9), never this module's.
//!
//! WHY (design 3036296f mechanism D, backlog 9b473d4a). Measured
//! 2026-09-27: of the first 200 open backlog items, 166 were untriaged,
//! 139 of those were one session's own review findings and discoveries,
//! 36 carried an `area`, and NONE recorded what produced it except in
//! prose — `source: "review of car fix/… (42da8bd2), 2026-09-27"`, or one
//! of three ad-hoc keys (`source_car`, `source_packet`, `found_by`)
//! spelled differently by each filer. The receiving yard cannot group a
//! pile by a sentence, so it showed one flat pile. And every one of the
//! 200 was owned by `emp-david`: admission resolves an agent filer to a
//! human owner (owner_resolution.rs, subject-model Q7), so who FILED the
//! item survived only as the create event's actor, never on the packet.
//!
//! THE SHAPE. `metadata.source` is an object: `{kind, id}` for a packet
//! named by id, `{kind, branch}` for a car or review named by branch,
//! and both when the id names a packet that carries a branch. The kind
//! is one of [`SourceKind::ALL`]. Every reference is CHECKED against the
//! system of record before the POST — an id resolves to a full uuid of
//! the kind it claims, a branch to a car that carries it — so a typo
//! is refused rather than filed as a group of one that nothing matches.
//!
//! WHAT IS REFUSED. A backlog-item whose lane is a finding —
//! review-finding, discovery-while-working, pipeline-failure — is always
//! produced BY something, and its filer is holding that something at the
//! moment of filing; it is refused without a source, naming the flag. A
//! prose `source` does not satisfy the rule, because prose is the shape
//! this module exists to retire. Other lanes (roadmap, user-feedback,
//! design-resolution…) may carry a source and are not refused without.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::channels::InputChannel;

/// The metadata key admission records the filer under — read back and
/// reported, never written here (backlog 443eedc9).
pub(crate) use boss_jobs::opened_by::OPENED_BY_KEY;
/// The metadata keys the structured source and the area are recorded
/// under — the key 51 of the first 200 open items already used for the
/// area, so the existing ones group too. Spelled ONCE, in the reader the
/// receiving yard groups by (`boss_jobs::origin`, backlog 9b473d4a car
/// 2), so the door cannot write a key the yard does not read.
pub(crate) use boss_jobs::origin::{AREA_KEY, SOURCE_KEY};

/// The lanes whose items are refused without a source: each is a
/// finding, and a finding was found BY something.
const SOURCE_IS_REQUIRED_OF: [InputChannel; 3] = [
    InputChannel::Review,
    InputChannel::Discovery,
    InputChannel::PipelineFailure,
];

/// What produced a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// A car — a `ship-a-change` packet — named by its branch or id.
    Car,
    /// A gate-run packet, by id (a branch names every gate run on it).
    GateRun,
    /// An agent-run packet, by id.
    AgentRun,
    /// Any other packet, by id.
    Packet,
    /// A review of a car, named by the car reviewed (branch or id) — so
    /// "71 findings from 14 car reviews" groups by the car.
    Review,
}

impl SourceKind {
    pub(crate) const ALL: [SourceKind; 5] = [
        SourceKind::Car,
        SourceKind::GateRun,
        SourceKind::AgentRun,
        SourceKind::Packet,
        SourceKind::Review,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            SourceKind::Car => "car",
            SourceKind::GateRun => "gate-run",
            SourceKind::AgentRun => "agent-run",
            SourceKind::Packet => "packet",
            SourceKind::Review => "review",
        }
    }

    fn parse(label: &str) -> Option<SourceKind> {
        SourceKind::ALL.into_iter().find(|k| k.label() == label)
    }

    /// The packet kind an id of this source must name; `None` = any.
    fn packet_kind(self) -> Option<&'static str> {
        match self {
            SourceKind::Car | SourceKind::Review => Some("ship-a-change"),
            SourceKind::GateRun => Some("gate-run"),
            SourceKind::AgentRun => Some("agent-run"),
            SourceKind::Packet => None,
        }
    }

    /// Whether a branch names ONE of these. A car does; a gate-run does
    /// not — every gate ever run on the branch carries it.
    fn takes_branch(self) -> bool {
        matches!(self, SourceKind::Car | SourceKind::Review)
    }
}

fn vocabulary() -> String {
    SourceKind::ALL
        .iter()
        .map(|k| k.label())
        .collect::<Vec<_>>()
        .join("|")
}

/// How a source is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    /// A packet id: the full uuid, or a prefix of at least 8 characters
    /// that [`pin`] resolves to one.
    Id(String),
    /// A branch — it carries a `/` (`fix/…`, `feat/…`).
    Branch(String),
}

/// A source as the filer stated it, before it is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) kind: SourceKind,
    pub(crate) reference: Reference,
}

fn reference(kind: SourceKind, value: &str) -> Result<Reference> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        bail!(
            "a {} source needs an id or a branch after the colon, with no spaces — got {value:?}",
            kind.label()
        );
    }
    if value.contains('/') {
        if !kind.takes_branch() {
            bail!(
                "a {} is named by its id, not a branch: a branch names every {} ever run on it",
                kind.label(),
                kind.label()
            );
        }
        return Ok(Reference::Branch(value.to_string()));
    }
    let id_shaped = value.len() >= 8 && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if !id_shaped {
        bail!(
            "{value:?} names neither a packet id (8 or more hex characters) nor a branch \
             (it would carry a /, like fix/…)"
        );
    }
    Ok(Reference::Id(value.to_ascii_lowercase()))
}

/// Parse `--source <kind>:<id or branch>`.
pub(crate) fn parse(text: &str) -> Result<Source> {
    let Some((label, value)) = text.split_once(':') else {
        bail!(
            "--source takes <{}>:<id or branch>, e.g. car:fix/x or gate-run:1a2b3c4d — got {text:?}",
            vocabulary()
        );
    };
    let Some(kind) = SourceKind::parse(label.trim()) else {
        bail!(
            "{label:?} is not a source kind — one of: {}",
            vocabulary().replace('|', ", ")
        );
    };
    Ok(Source {
        kind,
        reference: reference(kind, value)?,
    })
}

/// Read a structured source a metadata file already carries — the same
/// field, written through the other door, checked the same way.
fn from_metadata(v: &Value) -> Result<Source> {
    let o = v
        .as_object()
        .context("the metadata's `source` is not an object")?;
    let label = o
        .get("kind")
        .and_then(Value::as_str)
        .context("the metadata's `source` has no `kind`")?;
    let kind = SourceKind::parse(label).with_context(|| {
        format!(
            "the metadata's source kind {label:?} is not one of: {}",
            vocabulary().replace('|', ", ")
        )
    })?;
    let value = o
        .get("id")
        .or_else(|| o.get("branch"))
        .and_then(Value::as_str)
        .context("the metadata's `source` names neither an `id` nor a `branch`")?;
    Ok(Source {
        kind,
        reference: reference(kind, value)?,
    })
}

fn required_of(kind: &str, lane: Option<&str>) -> Option<InputChannel> {
    if kind != "backlog-item" {
        return None;
    }
    lane.and_then(InputChannel::parse)
        .filter(|l| SOURCE_IS_REQUIRED_OF.contains(l))
}

/// The source this filing states, or a refusal. `lane` is the input
/// lane the filing records (flag or metadata). `Ok(None)` means no
/// source to write: none was given and none is required, or a prose one
/// rides in the metadata of a lane that does not require one.
pub(crate) fn requested(
    kind: &str,
    lane: Option<&str>,
    flag: Option<&str>,
    md: &Option<Value>,
) -> Result<Option<Source>> {
    let in_md = md
        .as_ref()
        .and_then(|m| m.get(SOURCE_KEY))
        .filter(|v| !v.is_null());
    if let Some(text) = flag {
        if in_md.is_some() {
            bail!(
                "--source and the metadata's `{SOURCE_KEY}` both name a source — say it once \
                 (prose about the origin belongs in `description`)"
            );
        }
        return parse(text).map(Some);
    }
    let required = required_of(kind, lane);
    match in_md {
        Some(v) if v.is_object() => from_metadata(v).map(Some),
        Some(_) => match required {
            Some(lane) => bail!(
                "a {} {kind}'s `{SOURCE_KEY}` is prose, and a pile cannot be grouped by a \
                 sentence: pass --source <{}>:<id or branch> and move the prose to `description`",
                lane.label(),
                vocabulary()
            ),
            None => Ok(None),
        },
        None => match required {
            Some(lane) => bail!(
                "a {} {kind} must record what produced it: --source <{}>:<id or branch> \
                 (e.g. --source car:fix/x, --source gate-run:1a2b3c4d, --source review:fix/x)",
                lane.label(),
                vocabulary()
            ),
            None => Ok(None),
        },
    }
}

/// The area a filing records: the flag, checked against what the
/// metadata already says. A slug — lowercase, no spaces — because it is
/// a grouping key, and `Jobs` and `jobs ` would be two groups.
pub(crate) fn area(flag: Option<&str>, md: &Option<Value>) -> Result<Option<String>> {
    let Some(area) = flag else {
        return Ok(None);
    };
    let slug = !area.is_empty()
        && area
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '/');
    if !slug {
        bail!("--area is a grouping key: lowercase letters, digits, - and / only — got {area:?}");
    }
    if let Some(said) = md
        .as_ref()
        .and_then(|m| m.get(AREA_KEY))
        .and_then(Value::as_str)
        && said != area
    {
        bail!("--area {area} disagrees with the metadata's {AREA_KEY} {said} — say it once");
    }
    Ok(Some(area.to_string()))
}

/// Merge what this filing recorded into its metadata: the pinned source
/// and the area when given.
///
/// NOT THE FILER (backlog 443eedc9). This used to stamp `opened_by`
/// from BOSS_ACTOR and refuse a metadata file naming anyone else. Then
/// admission began stamping it from the signed caller (958edca6,
/// `boss_jobs::opened_by`) and refusing a create that names another
/// filer — and on the live stack the login door rewrites the login
/// (`claude@algedonic.dev`) to the agent it maps to (`agent-claude`)
/// before admission reads it, so the CLI's value named "someone else"
/// and every filing through this verb answered 422. The CLI cannot
/// know that spelling; admission can. So the filer is admission's
/// alone, and a metadata file naming one is passed through for
/// admission to judge — its 422 names both actors and the fix.
pub(crate) fn merge(
    md: Option<Value>,
    source: Option<Value>,
    area: Option<String>,
) -> Result<Value> {
    let mut md = md.unwrap_or_else(|| json!({}));
    let Some(o) = md.as_object_mut() else {
        bail!("--metadata must be a JSON object to carry the filing's record");
    };
    if let Some(s) = source {
        o.insert(SOURCE_KEY.to_string(), s);
    }
    if let Some(a) = area {
        o.insert(AREA_KEY.to_string(), json!(a));
    }
    Ok(md)
}

/// The recorded form of a checked source.
fn recorded(kind: SourceKind, id: Option<&str>, branch: Option<&str>) -> Value {
    let mut o = Map::new();
    o.insert("kind".into(), json!(kind.label()));
    if let Some(id) = id {
        o.insert("id".into(), json!(id));
    }
    if let Some(b) = branch {
        o.insert("branch".into(), json!(b));
    }
    Value::Object(o)
}

/// Check a source against the system of record and return its recorded
/// form. `get` reads one API path (`None` for an empty body) — the seam
/// the tests go through, so the checks are pinned without a socket.
///
/// An id resolves to the full uuid (a prefix through the same paging
/// `boss job get` uses) and must name a packet of the kind the source
/// claims; a branch must be carried by at least one car. A reference
/// that answers nothing is refused, never recorded: a wrong target
/// answers instead of erroring, and a source nothing matches would
/// file as a group of one.
pub(crate) async fn pin<G, Fut>(get: G, source: &Source) -> Result<Value>
where
    G: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    match &source.reference {
        Reference::Branch(branch) => {
            let doc = json!({ "branch": branch }).to_string();
            let path = format!(
                "/api/jobs?kind=ship-a-change&limit=1&metadata={}",
                percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
            );
            let rows = crate::train::rows(get(path).await?)?;
            // The row itself is read, not the total: a server that
            // ignored `metadata=` would answer every car there is.
            let carried = rows.iter().any(|r| {
                r.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    == Some(branch)
            });
            if !carried {
                bail!(
                    "no car carries branch {branch} — check the spelling, or name the \
                     source by its id"
                );
            }
            Ok(recorded(source.kind, None, Some(branch)))
        }
        Reference::Id(given) => {
            let id = if crate::job::looks_like_uuid(given) {
                given.clone()
            } else {
                let page = |status: &'static str, offset: usize| {
                    get(format!(
                        "/api/jobs?status={status}&limit={}&offset={offset}",
                        crate::job::RESOLVE_PAGE
                    ))
                };
                crate::job::resolve_through(page, given).await?
            };
            let job = get(format!("/api/jobs/{id}"))
                .await?
                .with_context(|| format!("{id} answered no packet"))?;
            let is = job.get("kind").and_then(Value::as_str).unwrap_or("?");
            if let Some(want) = source.kind.packet_kind()
                && is != want
            {
                bail!(
                    "{id} is kind {is}, but a source of kind {} must name kind {want}",
                    source.kind.label()
                );
            }
            let branch = job
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str);
            Ok(recorded(source.kind, Some(&id), branch))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const CAR: &str = "ed84acc8-90ac-4403-9dc9-2cff0e537474";
    const GATE: &str = "1a2b3c4d-0000-4000-8000-000000000001";

    fn say(r: Result<impl std::fmt::Debug>) -> String {
        r.unwrap_err().to_string()
    }

    /// 9b473d4a: a finding's lane is refused without a source, and the
    /// refusal names the flag and the vocabulary; other lanes are not.
    #[test]
    fn a_finding_is_refused_without_a_source_and_the_refusal_names_the_flag() {
        for lane in [
            "review-finding",
            "discovery-while-working",
            "pipeline-failure",
        ] {
            let said = say(requested("backlog-item", Some(lane), None, &None));
            assert!(said.contains("--source"), "{lane}: {said}");
            assert!(said.contains(lane), "{lane}: {said}");
            for k in SourceKind::ALL {
                assert!(said.contains(k.label()), "{} missing: {said}", k.label());
            }
        }
        for lane in ["roadmap", "user-feedback", "design-resolution"] {
            assert_eq!(
                requested("backlog-item", Some(lane), None, &None).unwrap(),
                None
            );
        }
        // Only a backlog-item: an ops-request has no finding lane.
        assert_eq!(
            requested("ops-request", Some("review-finding"), None, &None).unwrap(),
            None
        );
    }

    /// Prose is the shape being retired: it does not satisfy a finding's
    /// requirement, and it is left alone on a lane that has none.
    #[test]
    fn a_prose_source_does_not_satisfy_a_finding() {
        let md = Some(json!({"source": "review of car fix/x (42da8bd2)"}));
        let said = say(requested("backlog-item", Some("review-finding"), None, &md));
        assert!(
            said.contains("prose") && said.contains("--source"),
            "{said}"
        );
        assert_eq!(
            requested("backlog-item", Some("design-resolution"), None, &md).unwrap(),
            None
        );
        // And a flag beside any metadata source is said twice.
        let said = say(requested(
            "backlog-item",
            Some("review-finding"),
            Some("car:fix/x"),
            &md,
        ));
        assert!(said.contains("say it once"), "{said}");
    }

    #[test]
    fn a_structured_metadata_source_is_read_and_checked_like_the_flag() {
        let md = Some(json!({"source": {"kind": "gate-run", "id": "1A2B3C4D"}}));
        assert_eq!(
            requested("backlog-item", Some("pipeline-failure"), None, &md).unwrap(),
            Some(Source {
                kind: SourceKind::GateRun,
                reference: Reference::Id("1a2b3c4d".into())
            })
        );
        let md = Some(json!({"source": {"kind": "vibes", "id": "1a2b3c4d"}}));
        assert!(say(requested("backlog-item", None, None, &md)).contains("vibes"));
    }

    /// The kinds this door can WRITE are the kinds the receiving yard
    /// can READ (`boss_jobs::origin::SOURCE_KINDS`, backlog 9b473d4a car
    /// 2): a kind added here and not there would file items the yard
    /// draws as unreadable (CLAUDE.md §9a).
    #[test]
    fn every_kind_the_door_writes_is_one_the_yard_reads() {
        let door: Vec<&str> = SourceKind::ALL.iter().map(|k| k.label()).collect();
        assert_eq!(door, boss_jobs::origin::SOURCE_KINDS);
    }

    #[test]
    fn the_flag_parses_every_kind_and_refuses_the_shapes_it_cannot_group() {
        assert_eq!(
            parse("car:fix/a-thing").unwrap(),
            Source {
                kind: SourceKind::Car,
                reference: Reference::Branch("fix/a-thing".into())
            }
        );
        assert_eq!(
            parse("review:983696b5").unwrap().reference,
            Reference::Id("983696b5".into())
        );
        assert_eq!(
            parse("agent-run:665c7419").unwrap().kind,
            SourceKind::AgentRun
        );
        assert_eq!(
            parse(&format!("packet:{GATE}")).unwrap().kind,
            SourceKind::Packet
        );
        // No colon, an unknown kind, an empty or spaced value.
        assert!(say(parse("fix/a-thing")).contains("--source takes"));
        assert!(say(parse("commit:abc12345")).contains("not a source kind"));
        assert!(say(parse("car:")).contains("no spaces"));
        assert!(say(parse("car:fix/a thing")).contains("no spaces"));
        // A gate-run is one run: a branch names all of them.
        assert!(say(parse("gate-run:fix/a-thing")).contains("named by its id"));
        // Too short to be an id, and not a branch.
        assert!(say(parse("car:94469")).contains("neither"));
        assert!(say(parse("packet:the-design")).contains("neither"));
    }

    #[test]
    fn the_area_is_a_slug_and_is_said_once() {
        assert_eq!(area(None, &None).unwrap(), None);
        assert_eq!(
            area(Some("infra/forge"), &None).unwrap(),
            Some("infra/forge".into())
        );
        assert!(say(area(Some("Jobs"), &None)).contains("--area"));
        assert!(say(area(Some("boss-jobs stations"), &None)).contains("--area"));
        let md = Some(json!({"area": "estate"}));
        assert!(say(area(Some("jobs"), &md)).contains("say it once"));
        assert_eq!(area(Some("estate"), &md).unwrap(), Some("estate".into()));
    }

    /// The filer is admission's to stamp (backlog 443eedc9): the merge
    /// records the source and the area and never writes `opened_by` —
    /// the whole-verb pin against the real admission is in job.rs
    /// (`filing_meets_admission`).
    #[test]
    fn the_merge_records_the_origin_and_leaves_the_filer_to_admission() {
        let md = merge(None, None, None).unwrap();
        assert_eq!(md, json!({}));

        let md = merge(
            Some(json!({"description": "d"})),
            Some(json!({"kind": "car", "branch": "fix/x"})),
            Some("jobs".into()),
        )
        .unwrap();
        assert_eq!(md["description"], "d");
        assert_eq!(md["source"], json!({"kind": "car", "branch": "fix/x"}));
        assert_eq!(md["area"], "jobs");
        assert!(md.get(OPENED_BY_KEY).is_none(), "{md}");

        assert!(say(merge(Some(json!([1])), None, None)).contains("JSON object"));
    }

    /// A stub system of record: path → body.
    fn sor(bodies: Vec<(String, Value)>) -> BTreeMap<String, Value> {
        bodies.into_iter().collect()
    }

    fn reader(
        map: &BTreeMap<String, Value>,
    ) -> impl Fn(String) -> std::future::Ready<Result<Option<Value>>> + '_ {
        move |path: String| {
            std::future::ready(match map.get(&path) {
                Some(v) => Ok(Some(v.clone())),
                None => Err(anyhow::anyhow!("unexpected read {path}")),
            })
        }
    }

    fn branch_path(branch: &str) -> String {
        let doc = json!({ "branch": branch }).to_string();
        format!(
            "/api/jobs?kind=ship-a-change&limit=1&metadata={}",
            percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
        )
    }

    #[tokio::test]
    async fn a_branch_is_recorded_only_when_a_car_carries_it() {
        let map = sor(vec![
            (
                branch_path("fix/x"),
                json!({"data": [{"id": CAR, "metadata": {"branch": "fix/x"}}], "total": 1}),
            ),
            (branch_path("fix/typo"), json!({"data": [], "total": 0})),
            // A server that ignored the filter answers some other car.
            (
                branch_path("fix/ignored"),
                json!({"data": [{"id": CAR, "metadata": {"branch": "fix/x"}}], "total": 9}),
            ),
        ]);
        let got = pin(reader(&map), &parse("review:fix/x").unwrap())
            .await
            .unwrap();
        assert_eq!(got, json!({"kind": "review", "branch": "fix/x"}));
        let said = pin(reader(&map), &parse("car:fix/typo").unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(said.contains("no car carries branch fix/typo"), "{said}");
        assert!(
            pin(reader(&map), &parse("car:fix/ignored").unwrap())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_id_resolves_to_the_full_uuid_of_the_kind_it_claims() {
        let map = sor(vec![
            (
                format!(
                    "/api/jobs?status=open&limit={}&offset=0",
                    crate::job::RESOLVE_PAGE
                ),
                json!({"data": [{"id": CAR, "kind": "ship-a-change",
                                  "metadata": {"branch": "fix/x"}}], "total": 1}),
            ),
            (
                format!("/api/jobs/{CAR}"),
                json!({"id": CAR, "kind": "ship-a-change", "metadata": {"branch": "fix/x"}}),
            ),
            (
                format!("/api/jobs/{GATE}"),
                json!({"id": GATE, "kind": "gate-run", "metadata": {"branch": "fix/x"}}),
            ),
        ]);
        // A prefix resolves, and the car's branch rides along so the
        // yard can group by-id and by-branch filings as one car.
        let got = pin(reader(&map), &parse("car:ed84acc8").unwrap())
            .await
            .unwrap();
        assert_eq!(got, json!({"kind": "car", "id": CAR, "branch": "fix/x"}));
        // A full uuid is read directly.
        let got = pin(reader(&map), &parse(&format!("gate-run:{GATE}")).unwrap())
            .await
            .unwrap();
        assert_eq!(got["id"], GATE);
        // `packet` names any kind.
        assert!(
            pin(reader(&map), &parse(&format!("packet:{GATE}")).unwrap())
                .await
                .is_ok()
        );
        // The wrong kind is refused, naming both.
        let said = pin(reader(&map), &parse(&format!("agent-run:{GATE}")).unwrap())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            said.contains("gate-run") && said.contains("agent-run"),
            "{said}"
        );
    }
}
