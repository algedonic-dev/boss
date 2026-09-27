//! `boss job backfill-source [--apply]` — the ONE-SHOT map from the three
//! ad-hoc provenance keys an open backlog-item may carry (`source_car`,
//! `source_packet`, `found_by`: `boss_jobs::origin::AD_HOC_KEYS`) to the
//! structured `metadata.source` the filing door has recorded since
//! backlog 9b473d4a car 1, so the receiving yard can group what was filed
//! before it.
//!
//! WHY (design 3036296f mechanism D, car 2). Measured 2026-09-27 on the
//! 359 open backlog-items: none carried a structured source, and 178
//! carried at least one ad-hoc key — 25 `source_car`, 83
//! `source_packet`, 70 `found_by` — each spelled its filer's own way.
//! Without this, every one of them stands in the yard's "no recorded
//! source" group until it closes.
//!
//! EVERY MAPPING IS CHECKED, NONE IS GUESSED. A reference is resolved
//! against the system of record exactly the way the door checks one: an
//! id through `GET /api/jobs/{id}` (a prefix resolves there; absent and
//! ambiguous are both refused, not picked), and its KIND decides the
//! source kind — so a `source_car` that names a backlog item records a
//! `packet`, not a car. A branch must be carried by a car. From
//! `found_by` — a sentence — only a run id is taken, and only when the
//! sentence names exactly ONE and it resolves to an agent-run; anything
//! else is left unmapped, named, and stays visible in the yard. A read
//! that fails for any reason but "not found" / "ambiguous" STOPS the
//! run: an outage is not an unmapped item.
//!
//! DRY RUN BY DEFAULT. Without `--apply` it writes nothing and prints
//! every item's plan and a summary. With it, each mapped item gets ONE
//! metadata PATCH — `source`, and `source_as_filed` carrying a prose
//! `source` it replaces, so no sentence is lost — read back and confirmed
//! the way `boss job patch` confirms. The ad-hoc keys are left in place:
//! the write adds a fact and removes none. Running it twice maps nothing
//! the second time, because a recorded source is skipped.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use boss_jobs::origin::{AD_HOC_KEYS, SOURCE_KEY, SourceBasis, source_of};

/// Where a replaced prose `source` is kept.
pub(crate) const AS_FILED_KEY: &str = "source_as_filed";

/// The lane whose car references record as a review of that car
/// (`item_source::SourceKind::Review`).
const REVIEW_LANE: &str = "review-finding";

/// A reference one ad-hoc key names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reference {
    /// A car's branch (`source_car: "fix/…"`).
    Branch(String),
    /// A packet id, full or a prefix of 8+ (`source_car`, `source_packet`).
    Id(String),
    /// The ONE run a `found_by` sentence names.
    Run(String),
}

/// Which ad-hoc key a reference came from, and the reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) key: &'static str,
    pub(crate) reference: Reference,
}

fn id_shaped(s: &str) -> bool {
    s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The run ids a sentence names: a hex token of 8+ right after the word
/// `run` or `agent-run` ("the builder of e6b2066f (run e661eaac)",
/// "triage of 49391a67 (agent-run c1699751)"). Distinct, in order.
pub(crate) fn runs_named(sentence: &str) -> Vec<String> {
    let words: Vec<&str> = sentence
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | ':'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    for pair in words.windows(2) {
        let (word, next) = (pair[0].to_ascii_lowercase(), pair[1]);
        let next = next.trim_end_matches('.').to_ascii_lowercase();
        if (word == "run" || word == "agent-run") && id_shaped(&next) && !out.contains(&next) {
            out.push(next);
        }
    }
    out
}

/// What an item's ad-hoc keys name, tried in [`AD_HOC_KEYS`] order —
/// the car first, then the packet, then the run a sentence names — or,
/// when none can be read, why each could not.
pub(crate) fn candidate(md: &Value) -> Result<Candidate, String> {
    let mut why: Vec<String> = Vec::new();
    for key in AD_HOC_KEYS {
        let Some(v) = md.get(key).and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if v.is_empty() {
            continue;
        }
        let reference = match key {
            "found_by" => match runs_named(v).as_slice() {
                [one] => Some(Reference::Run(one.clone())),
                [] => {
                    why.push("found_by names no run".into());
                    None
                }
                many => {
                    why.push(format!("found_by names {} runs", many.len()));
                    None
                }
            },
            _ if key == "source_car" && v.contains('/') && !v.contains(char::is_whitespace) => {
                Some(Reference::Branch(v.to_string()))
            }
            _ if id_shaped(v) => Some(Reference::Id(v.to_ascii_lowercase())),
            _ => {
                why.push(format!("{key} {v:?} is neither an id nor a car's branch"));
                None
            }
        };
        if let Some(reference) = reference {
            return Ok(Candidate { key, reference });
        }
    }
    if why.is_empty() {
        why.push("no ad-hoc key".into());
    }
    Err(why.join("; "))
}

/// What the system of record answered for a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The packet an id resolved to.
    Packet {
        id: String,
        kind: String,
        branch: Option<String>,
    },
    /// A car carries the branch.
    Carried,
    /// Not found, ambiguous, or no car carries it — named.
    Refused(String),
}

/// The recorded source a checked reference maps to, or why it does not.
/// A car in the review-finding lane is a review OF that car.
pub(crate) fn mapped(lane: Option<&str>, c: &Candidate, answer: &Answer) -> Result<Value, String> {
    let car_kind = if lane == Some(REVIEW_LANE) {
        "review"
    } else {
        "car"
    };
    match (&c.reference, answer) {
        (_, Answer::Refused(why)) => Err(why.clone()),
        (Reference::Branch(b), Answer::Carried) => Ok(json!({"kind": car_kind, "branch": b})),
        (Reference::Run(_), Answer::Packet { id, kind, .. }) => {
            if kind == "agent-run" {
                Ok(json!({"kind": "agent-run", "id": id}))
            } else {
                Err(format!(
                    "found_by's run {} is kind {kind}",
                    &id[..8.min(id.len())]
                ))
            }
        }
        (Reference::Id(_), Answer::Packet { id, kind, branch }) => {
            let kind = match kind.as_str() {
                "ship-a-change" => car_kind,
                "gate-run" => "gate-run",
                "agent-run" => "agent-run",
                _ => "packet",
            };
            let mut o = serde_json::Map::new();
            o.insert("kind".into(), json!(kind));
            o.insert("id".into(), json!(id));
            // A car's branch rides along, as the door stamps it, so a
            // car named by id and by branch group as one.
            if let (Some(b), "car" | "review") = (branch, kind) {
                o.insert("branch".into(), json!(b));
            }
            Ok(Value::Object(o))
        }
        (r, a) => Err(format!("{r:?} cannot be read from {a:?}")),
    }
}

/// The metadata PATCH one mapped item receives: the source, and the
/// prose (or unreadable object) it replaces kept under [`AS_FILED_KEY`].
pub(crate) fn patch_for(md: &Value, source: Value) -> Value {
    let mut o = serde_json::Map::new();
    if let Some(old) = md.get(SOURCE_KEY).filter(|v| !v.is_null())
        && md.get(AS_FILED_KEY).is_none()
    {
        o.insert(AS_FILED_KEY.into(), old.clone());
    }
    o.insert(SOURCE_KEY.into(), source);
    Value::Object(o)
}

/// One item's plan.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    Map { from: &'static str, patch: Value },
    Unmapped(String),
}

/// A 404 (absent) or 409 (ambiguous prefix) is an answer about the
/// reference; anything else is a failed read, and stops the run.
fn refused_by_the_api(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("-> 404") || s.contains("-> 409")
}

/// Resolve one reference against the system of record through `get`
/// (one GET path → body; the seam the tests stub).
async fn answer<G, Fut>(get: &G, r: &Reference) -> Result<Answer>
where
    G: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    match r {
        Reference::Branch(branch) => {
            let doc = json!({ "branch": branch }).to_string();
            let path = format!(
                "/api/jobs?kind=ship-a-change&limit=1&metadata={}",
                percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
            );
            let rows = crate::train::rows(get(path).await?)?;
            // The row is read, not the total: a server that ignored the
            // filter would answer some other car.
            let carried = rows.iter().any(|r| {
                r.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    == Some(branch.as_str())
            });
            Ok(if carried {
                Answer::Carried
            } else {
                Answer::Refused(format!("no car carries branch {branch}"))
            })
        }
        Reference::Id(id) | Reference::Run(id) => match get(format!("/api/jobs/{id}")).await {
            Ok(Some(job)) => {
                let s = |k: &str| job.get(k).and_then(Value::as_str).map(str::to_string);
                match (s("id"), s("kind")) {
                    (Some(id), Some(kind)) => Ok(Answer::Packet {
                        id,
                        kind,
                        branch: job
                            .get("metadata")
                            .and_then(|m| m.get("branch"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }),
                    _ => bail!("GET /api/jobs/{id} answered a body with no id or kind"),
                }
            }
            Ok(None) => bail!("GET /api/jobs/{id} answered no JSON body"),
            Err(e) if refused_by_the_api(&e) => Ok(Answer::Refused(format!(
                "{id} resolves to no one packet ({})",
                if e.to_string().contains("-> 409") {
                    "ambiguous"
                } else {
                    "not found"
                }
            ))),
            Err(e) => Err(e),
        },
    }
}

/// The plan for every item that carries an ad-hoc key and no recorded
/// source — each reference read once, however many items name it.
/// Items with a recorded source, or with neither, are not this verb's
/// and get no plan.
pub(crate) async fn plan<G, Fut>(get: G, items: &[Value]) -> Result<Vec<(String, Plan)>>
where
    G: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let mut seen: BTreeMap<String, Answer> = BTreeMap::new();
    let mut out = Vec::new();
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let md = item.get("metadata").cloned().unwrap_or(Value::Null);
        if source_of(&md).basis != SourceBasis::AdHoc {
            continue;
        }
        let lane = md
            .get(crate::channels::RECORDED_KEY)
            .and_then(Value::as_str);
        let c = match candidate(&md) {
            Ok(c) => c,
            Err(why) => {
                out.push((id, Plan::Unmapped(why)));
                continue;
            }
        };
        let cache_key = format!("{:?}", c.reference);
        let a = match seen.get(&cache_key) {
            Some(a) => a.clone(),
            None => {
                let a = answer(&get, &c.reference).await?;
                seen.insert(cache_key, a.clone());
                a
            }
        };
        out.push((
            id,
            match mapped(lane, &c, &a) {
                Ok(source) => Plan::Map {
                    from: c.key,
                    patch: patch_for(&md, source),
                },
                Err(why) => Plan::Unmapped(format!("{} — {why}", c.key)),
            },
        ));
    }
    Ok(out)
}

/// Every open backlog-item, page after page until the rows agree with
/// the server's `total` — a limit is not a filter.
async fn open_items(http: &reqwest::Client) -> Result<Vec<Value>> {
    const PAGE: usize = 200;
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let path = format!(
            "/api/jobs?kind=backlog-item&status=open&limit={PAGE}&offset={}",
            rows.len()
        );
        let body = crate::gate::api(http, reqwest::Method::GET, &path, None).await?;
        let total = body
            .as_ref()
            .and_then(|b| b.get("total"))
            .and_then(Value::as_u64);
        let page = crate::train::rows(body)?;
        let n = page.len();
        rows.extend(page);
        let Some(total) = total else {
            bail!("the open backlog-item list answered no total, so its rows cannot be counted");
        };
        if n == 0 || rows.len() as u64 >= total {
            if (rows.len() as u64) < total {
                bail!(
                    "the open backlog-item list stopped at {} of {total} rows",
                    rows.len()
                );
            }
            return Ok(rows);
        }
    }
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

pub async fn run(apply: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let items = open_items(&http).await?;
    let get = |path: String| {
        let http = &http;
        async move { crate::gate::api(http, reqwest::Method::GET, &path, None).await }
    };
    let plans = plan(get, &items).await?;
    let recorded = items
        .iter()
        .filter(|i| {
            source_of(i.get("metadata").unwrap_or(&Value::Null)).basis == SourceBasis::Recorded
        })
        .count();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut mapped_n = 0usize;
    for (id, p) in &plans {
        match p {
            Plan::Map { from, patch } => {
                mapped_n += 1;
                let s = &patch[SOURCE_KEY];
                let kind = s["kind"].as_str().unwrap_or("?");
                *by_kind.entry(kind.to_string()).or_default() += 1;
                let named = s["branch"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| s["id"].as_str().map(|i| short(i).to_string()))
                    .unwrap_or_default();
                let kept = if patch.get(AS_FILED_KEY).is_some() {
                    "  (prose source kept as source_as_filed)"
                } else {
                    ""
                };
                println!("  map      {}  {from} -> {kind}:{named}{kept}", short(id));
            }
            Plan::Unmapped(why) => println!("  unmapped {}  {why}", short(id)),
        }
    }
    let kinds: Vec<String> = by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
    println!(
        "boss job backfill-source: {} open backlog-items read; {recorded} already record a \
         source; {} carry an ad-hoc key and none — {mapped_n} map ({}), {} stay unmapped and \
         visible in the yard's no-recorded-source group",
        items.len(),
        plans.len(),
        if kinds.is_empty() {
            "none".to_string()
        } else {
            kinds.join(", ")
        },
        plans.len() - mapped_n
    );
    if !apply {
        println!("boss job backfill-source: DRY RUN — nothing written; --apply writes the maps");
        return Ok(());
    }
    let mut failed: Vec<String> = Vec::new();
    for (id, p) in &plans {
        let Plan::Map { patch, .. } = p else {
            continue;
        };
        let wrote = crate::gate::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(patch.clone()),
        )
        .await;
        let confirmed = match wrote {
            Ok(_) => crate::gate::api(
                &http,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await
            .map(|job| {
                let now = job
                    .as_ref()
                    .and_then(|j| j.get("metadata"))
                    .cloned()
                    .unwrap_or(Value::Null);
                crate::job::confirm_patch(&now, patch).1
            }),
            Err(e) => Err(e),
        };
        match confirmed {
            Ok(true) => {}
            Ok(false) => failed.push(format!("{}: the write did not read back", short(id))),
            Err(e) => failed.push(format!("{}: {e}", short(id))),
        }
    }
    if !failed.is_empty() {
        bail!(
            "{} of {mapped_n} writes did not land:\n  {}",
            failed.len(),
            failed.join("\n  ")
        );
    }
    println!(
        "boss job backfill-source: {mapped_n} items now record a structured source — each \
         confirmed by reading it back"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAR: &str = "ed84acc8-90ac-4403-9dc9-2cff0e537474";
    const RUN: &str = "e661eaac-0000-4000-8000-000000000001";
    const ITEM: &str = "1d9b7db7-0000-4000-8000-000000000002";

    #[test]
    fn a_found_by_sentence_yields_a_run_only_when_it_names_exactly_one() {
        assert_eq!(
            runs_named("the builder of e6b2066f (run e661eaac), 2026-09-24"),
            vec!["e661eaac"]
        );
        assert_eq!(
            runs_named("triage of 49391a67 (agent-run c1699751)"),
            vec!["c1699751"]
        );
        assert_eq!(
            runs_named("page-audit test builders, 2026-09-23 (runs 404619cc, ab963744)"),
            Vec::<String>::new(),
            "`runs` introduces two — not one run"
        );
        assert_eq!(
            runs_named("post-mortem 3c3b202c, 2026-09-23"),
            Vec::<String>::new()
        );
        let md = json!({"found_by": "run 11111111 then agent-run 22222222"});
        assert_eq!(candidate(&md).unwrap_err(), "found_by names 2 runs");
    }

    #[test]
    fn the_car_key_is_read_first_then_the_packet_then_the_sentence() {
        let md = json!({"source_car": "fix/x", "source_packet": "8cd38edd",
                        "found_by": "run e661eaac"});
        assert_eq!(
            candidate(&md).unwrap(),
            Candidate {
                key: "source_car",
                reference: Reference::Branch("fix/x".into())
            }
        );
        // An unreadable car key falls through to the next key, and the
        // reasons are all named when nothing reads.
        let md = json!({"source_car": "the six doors car", "found_by": "run E661EAAC."});
        assert_eq!(
            candidate(&md).unwrap(),
            Candidate {
                key: "found_by",
                reference: Reference::Run("e661eaac".into())
            }
        );
        let md = json!({"source_car": "six doors", "source_packet": "x"});
        let why = candidate(&md).unwrap_err();
        assert!(
            why.contains("source_car") && why.contains("source_packet"),
            "{why}"
        );
        // A branch is only a car's: a slash in source_packet is not one.
        assert!(candidate(&json!({"source_packet": "fix/x"})).is_err());
    }

    fn packet(id: &str, kind: &str, branch: Option<&str>) -> Answer {
        Answer::Packet {
            id: id.into(),
            kind: kind.into(),
            branch: branch.map(str::to_string),
        }
    }

    #[test]
    fn the_packet_the_system_of_record_names_decides_the_source_kind() {
        let by_id = |key| Candidate {
            key,
            reference: Reference::Id("ed84acc8".into()),
        };
        // A car, and a car in the review lane is a review of it — with
        // its branch, as the door stamps it.
        assert_eq!(
            mapped(
                None,
                &by_id("source_car"),
                &packet(CAR, "ship-a-change", Some("fix/x"))
            ),
            Ok(json!({"kind": "car", "id": CAR, "branch": "fix/x"}))
        );
        assert_eq!(
            mapped(
                Some("review-finding"),
                &by_id("source_packet"),
                &packet(CAR, "ship-a-change", Some("fix/x"))
            ),
            Ok(json!({"kind": "review", "id": CAR, "branch": "fix/x"}))
        );
        // A source_car that names a backlog item records a packet: the
        // key's word is not the fact, the kind is.
        assert_eq!(
            mapped(
                None,
                &by_id("source_car"),
                &packet(ITEM, "backlog-item", None)
            ),
            Ok(json!({"kind": "packet", "id": ITEM}))
        );
        assert_eq!(
            mapped(
                None,
                &by_id("source_packet"),
                &packet(RUN, "gate-run", Some("fix/x"))
            ),
            Ok(json!({"kind": "gate-run", "id": RUN}))
        );
        // A run the sentence names must BE a run.
        let run = Candidate {
            key: "found_by",
            reference: Reference::Run("e661eaac".into()),
        };
        assert_eq!(
            mapped(None, &run, &packet(RUN, "agent-run", None)),
            Ok(json!({"kind": "agent-run", "id": RUN}))
        );
        assert!(
            mapped(None, &run, &packet(ITEM, "backlog-item", None))
                .unwrap_err()
                .contains("kind backlog-item")
        );
        let branch = Candidate {
            key: "source_car",
            reference: Reference::Branch("fix/x".into()),
        };
        assert_eq!(
            mapped(Some("review-finding"), &branch, &Answer::Carried),
            Ok(json!({"kind": "review", "branch": "fix/x"}))
        );
        assert_eq!(
            mapped(
                None,
                &branch,
                &Answer::Refused("no car carries branch fix/x".into())
            ),
            Err("no car carries branch fix/x".into())
        );
    }

    #[test]
    fn a_prose_source_it_replaces_is_kept_not_lost() {
        let src = json!({"kind": "car", "branch": "fix/x"});
        assert_eq!(
            patch_for(
                &json!({"source": "review of car fix/x", "source_car": "fix/x"}),
                src.clone()
            ),
            json!({"source": src, "source_as_filed": "review of car fix/x"})
        );
        assert_eq!(
            patch_for(&json!({"source_car": "fix/x"}), src.clone()),
            json!({"source": src})
        );
    }

    type Calls = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

    /// A stub system of record: path → body; `null` answers a 404 and
    /// `false` a 502, the two failures the plan must tell apart.
    fn reader(
        map: &BTreeMap<String, Value>,
        calls: &Calls,
    ) -> impl Fn(String) -> std::future::Ready<Result<Option<Value>>> {
        let map = map.clone();
        let calls = calls.clone();
        move |path: String| {
            calls.borrow_mut().push(path.clone());
            std::future::ready(match map.get(&path) {
                Some(Value::Null) => Err(anyhow::anyhow!(
                    "jobs api GET {path} -> 404 Not Found: job not found"
                )),
                Some(Value::Bool(false)) => {
                    Err(anyhow::anyhow!("jobs api GET {path} -> 502 Bad Gateway"))
                }
                Some(v) => Ok(Some(v.clone())),
                None => Err(anyhow::anyhow!("unexpected read {path}")),
            })
        }
    }

    #[tokio::test]
    async fn the_plan_maps_what_resolves_names_what_does_not_and_reads_each_reference_once() {
        let items = vec![
            json!({"id": "a", "metadata": {"source_car": "ed84acc8"}}),
            json!({"id": "b", "metadata": {"source_car": "ed84acc8",
                                            "input_channel": "review-finding",
                                            "source": "review of car ed84acc8"}}),
            json!({"id": "c", "metadata": {"found_by": "the builder of x (run e661eaac)"}}),
            json!({"id": "d", "metadata": {"source_packet": "deadbeef"}}),
            json!({"id": "e", "metadata": {"found_by": "post-mortem 3c3b202c"}}),
            // Not this verb's: already recorded, prose only, nothing.
            json!({"id": "f", "metadata": {"source": {"kind": "car", "branch": "fix/x"},
                                            "source_car": "ed84acc8"}}),
            json!({"id": "g", "metadata": {"source": "run 38eed035 handback"}}),
            json!({"id": "h", "metadata": {}}),
        ];
        let map: BTreeMap<String, Value> = [
            (
                "/api/jobs/ed84acc8".to_string(),
                json!({"id": CAR, "kind": "ship-a-change", "metadata": {"branch": "fix/x"}}),
            ),
            (
                "/api/jobs/e661eaac".to_string(),
                json!({"id": RUN, "kind": "agent-run", "metadata": {}}),
            ),
            ("/api/jobs/deadbeef".to_string(), Value::Null),
        ]
        .into_iter()
        .collect();
        let calls: Calls = Default::default();
        let got = plan(reader(&map, &calls), &items).await.unwrap();
        let ids: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d", "e"]);
        assert_eq!(
            got[0].1,
            Plan::Map {
                from: "source_car",
                patch: json!({"source": {"kind": "car", "id": CAR, "branch": "fix/x"}})
            }
        );
        assert_eq!(
            got[1].1,
            Plan::Map {
                from: "source_car",
                patch: json!({"source": {"kind": "review", "id": CAR, "branch": "fix/x"},
                              "source_as_filed": "review of car ed84acc8"})
            }
        );
        assert_eq!(
            got[2].1,
            Plan::Map {
                from: "found_by",
                patch: json!({"source": {"kind": "agent-run", "id": RUN}})
            }
        );
        assert_eq!(
            got[3].1,
            Plan::Unmapped("source_packet — deadbeef resolves to no one packet (not found)".into())
        );
        assert_eq!(got[4].1, Plan::Unmapped("found_by names no run".into()));
        // The car two items name was read once.
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|p| p.ends_with("ed84acc8"))
                .count(),
            1
        );
    }

    /// An outage is not an unmapped item: a failed read that is not a
    /// 404 or 409 stops the plan, naming the read.
    #[tokio::test]
    async fn a_failed_read_stops_the_plan_rather_than_reading_as_unmapped() {
        let items = vec![json!({"id": "a", "metadata": {"source_packet": "deadbeef"}})];
        let map: BTreeMap<String, Value> = [("/api/jobs/deadbeef".to_string(), Value::Bool(false))]
            .into_iter()
            .collect();
        let calls: Calls = Default::default();
        let err = plan(reader(&map, &calls), &items).await.unwrap_err();
        assert!(err.to_string().contains("502"), "{err}");
    }
}
