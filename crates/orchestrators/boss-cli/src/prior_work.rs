//! WHAT IS ALREADY NAMED (backlog 7562d7a0, and 65a753b7) — what
//! origin/main and the landed or parked cars already say about a backlog item,
//! read before its step is claimed and printed at the top of its brief.
//!
//! The landed-work door (a7837d81) refuses a packet a merged car names
//! as its `backlog_item`, and that is the only link it can refuse on.
//! Measured 2026-09-24/25, three runs went past it and were spent
//! re-deriving work that had already landed:
//!
//! - **1e973965** was fixed by two cars filed under its PARENT 4347a1af
//!   (trains #602 and #623). The code and its tests cite 1e973965 in
//!   eight lines on origin/main; no car names it as an item, so the door
//!   saw nothing and the run (60f5b083) closed it `stale`.
//! - **56727f95** had every buildable car of its plan landed as
//!   `partial_item` cars in train #612 — a key no door reads, by design,
//!   because a partial car must not close its item (65a753b7). Run
//!   b072e827 spent itself establishing that only the plan's `Later:`
//!   item remained.
//! - **86f32b7d** is cited three times on origin/main, as the measured
//!   case of a tenant venue.
//!
//! WHY A WARNING AND NOT A REFUSAL. Both halves were measured against
//! the whole open queue on 2026-09-25: origin/main cites 73 of the 293
//! open backlog-items by id, and 13 open items have a landed partial car
//! (one has seven — a plan in progress, not a finished one). Code
//! names the packets that MEASURED it as often as the ones it closes,
//! and a partial car is by definition not the whole. A refusal on either
//! would stop a quarter of the queue on a guess and teach `--force`; so
//! this prints the EVIDENCE — each cited line, each existing car — where
//! the one about to spend the run reads it first, and the judgement is
//! theirs (builder rule 15), made in the minute it takes to read the
//! lines rather than in the run it takes to rediscover them.

use std::path::Path;

use serde_json::{Value, json};

/// How many cited lines the brief prints; past this it names the
/// command that lists them all, rather than a digest of them.
pub(crate) const SHOWN: usize = 12;

/// A cited line is cut here: minified web bundles and TOML prose lines
/// run to thousands of characters, and the path:line is the evidence.
const LINE_CHARS: usize = 160;

/// The ref searched: the forge's main as this checkout last fetched it
/// (the dev pod's reclaim sidecar fetches it hourly), never the working
/// tree, which may be a branch that has not landed.
pub(crate) const MAIN: &str = "origin/main";

/// The lines of origin/main that name the packet, and the commit read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Citations {
    pub at: String,
    pub lines: Vec<String>,
}

/// Both halves, each either read or the reason it could not be — an
/// unread half is SAID in the brief, never rendered as "nothing found"
/// (backlog 7b7e0529: a door that could not look reads as a clean one).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PriorWork {
    pub item: String,
    pub citations: Result<Citations, String>,
    pub parts: Result<Vec<Value>, String>,
}

/// The item this searches for, when the packet is one: only a
/// backlog-item has a fix that can already be on main.
pub(crate) fn item_of(job: &Value) -> Option<&str> {
    (job.get("kind").and_then(Value::as_str) == Some("backlog-item"))
        .then(|| job.get("id").and_then(Value::as_str))
        .flatten()
}

/// The cars that name this packet as ONE PIECE of it — narrowed at the
/// server by the containment door, exactly as the landed-work door's
/// `cars_for_item_query` narrows on `backlog_item`.
pub(crate) fn partial_cars_query(item_id: &str) -> String {
    let doc = json!({ boss_jobs::car::PARTIAL_ITEM: item_id }).to_string();
    format!(
        "/api/jobs?kind=ship-a-change&metadata={}&limit=50",
        percent_encoding::utf8_percent_encode(&doc, crate::job::QUERY_VALUE)
    )
}

/// Read all existing car-to-item edges before describing prior work (17bcc877).
/// A dark, truncated or changing read is evidence we could not gather the set.
pub(crate) async fn read_cars<F, Fut>(item: &str, fetch: F) -> anyhow::Result<Option<Value>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Option<Value>>>,
{
    let mut out = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for query in [
        partial_cars_query(item),
        crate::dispatch::cars_for_item_query(item),
        // Containment accepts only flat strings (9afea697). Read every
        // array-bearing car through the native key-presence door, then
        // let landed_parts judge membership off each complete row.
        format!(
            "/api/jobs?kind=ship-a-change&metadata_has={}&limit=50",
            boss_jobs::car::ALSO_ANSWERS
        ),
    ] {
        let mut offset = 0;
        let mut expected = None;
        let mut page_ids = std::collections::BTreeSet::new();
        loop {
            let body = fetch(format!("{query}&full=true&offset={offset}"))
                .await?
                .ok_or_else(|| anyhow::anyhow!("car evidence read returned no body"))?;
            let total = crate::train::list_total(&body)?;
            if expected.is_some_and(|previous| previous != total) {
                anyhow::bail!("car evidence total changed during the read");
            }
            expected = Some(total);
            let page = crate::train::rows(Some(body))?;
            if page.is_empty() && offset < total {
                anyhow::bail!("car evidence read answered {offset} of {total} rows: unread tail");
            }
            offset += page.len();
            if offset > total {
                anyhow::bail!("car evidence carries more rows than its total");
            }
            for car in page {
                let id = car
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("car evidence row has no id"))?
                    .to_owned();
                if !page_ids.insert(id.clone()) {
                    anyhow::bail!("car evidence repeats id {id} across pages");
                }
                if ids.insert(id) {
                    out.push(car);
                }
            }
            if offset == total {
                break;
            }
        }
    }
    Ok(Some(json!({"total": out.len(), "data": out})))
}

/// The landed and still-open cars, re-read off each row rather than trusted
/// from the query: a server that ignored `metadata=` answers the
/// unfiltered page, and that must name nothing.
pub(crate) fn landed_parts(item_id: &str, cars: &[Value]) -> Vec<Value> {
    cars.iter()
        .filter(|c| {
            ([boss_jobs::car::PARTIAL_ITEM, boss_jobs::car::BACKLOG_ITEM]
                .iter()
                .any(|key| {
                    c.pointer(&format!("/metadata/{key}"))
                        .and_then(Value::as_str)
                        == Some(item_id)
                })
                || c.pointer(&format!("/metadata/{}", boss_jobs::car::ALSO_ANSWERS))
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(item_id))))
                && (boss_jobs::car::is_landed(c)
                    || c.get("status").and_then(Value::as_str) == Some("open"))
        })
        .cloned()
        .collect()
}

/// `git grep -n` output against `rev`, as `path:line  text` lines.
pub(crate) fn cited_lines(grep_stdout: &str, rev: &str) -> Vec<String> {
    let prefix = format!("{rev}:");
    grep_stdout
        .lines()
        .filter_map(|l| {
            let l = l.strip_prefix(&prefix).unwrap_or(l);
            let mut f = l.splitn(3, ':');
            let (path, n, text) = (f.next()?, f.next()?, f.next()?);
            let text = text.trim();
            let cut: String = text.chars().take(LINE_CHARS).collect();
            let more = if cut.len() < text.len() { " …" } else { "" };
            Some(format!("{path}:{n}  {cut}{more}"))
        })
        .collect()
}

/// Search origin/main in `repo` for the packet's short id. Local and
/// read-only — no fetch.
pub(crate) fn tree_citations(repo: &Path, short: &str) -> Result<Citations, String> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .map_err(|e| format!("git {} in {}: {e}", args.join(" "), repo.display()))
    };
    let first_line = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .next()
            .unwrap_or("no stderr")
            .trim()
            .to_string()
    };
    let at = git(&["rev-parse", "--short=12", "--verify", "--quiet", MAIN])?;
    if !at.status.success() {
        return Err(format!(
            "{} has no {MAIN} to search ({})",
            repo.display(),
            first_line(&at.stderr)
        ));
    }
    let at = String::from_utf8_lossy(&at.stdout).trim().to_string();
    let out = git(&["grep", "-n", "-I", "-F", "-e", short, MAIN, "--"])?;
    // `git grep` exits 1 for "no match", which is an answer; anything
    // else non-zero is git failing to look, which is not.
    match out.status.code() {
        Some(0) | Some(1) => Ok(Citations {
            at,
            lines: cited_lines(&String::from_utf8_lossy(&out.stdout), MAIN),
        }),
        _ => Err(format!(
            "git grep in {}: {}",
            repo.display(),
            first_line(&out.stderr)
        )),
    }
}

/// Both halves for `item_id`: the existing-car read the caller made with
/// its own signer, and origin/main in `repo`.
pub(crate) fn assemble(
    repo: &Path,
    item_id: &str,
    cars: anyhow::Result<Option<Value>>,
) -> PriorWork {
    let short = &item_id[..item_id.len().min(8)];
    PriorWork {
        item: item_id.to_string(),
        citations: tree_citations(repo, short),
        parts: cars
            .and_then(crate::train::rows)
            .map(|c| landed_parts(item_id, &c))
            .map_err(|e| format!("{e:#}")),
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// The brief's section. `None` only when BOTH halves were read and both
/// found nothing — an unread half is said.
pub(crate) fn section(prior: &PriorWork) -> Option<String> {
    let item = short(&prior.item);
    let cited = matches!(&prior.citations, Ok(c) if !c.lines.is_empty());
    let parted = matches!(&prior.parts, Ok(p) if !p.is_empty());
    if prior.citations.is_ok() && prior.parts.is_ok() && !cited && !parted {
        return None;
    }
    let mut out = format!(
        "== ALREADY NAMED — what origin/main and the landed cars already say about {item}; \
         read this before you build ==\n\n"
    );
    match &prior.citations {
        Ok(c) if c.lines.is_empty() => out.push_str(&format!(
            "origin/main ({}) does not cite {item} anywhere.\n\n",
            c.at
        )),
        Ok(c) => {
            out.push_str(&format!(
                "origin/main ({}, as this checkout last fetched it) cites {item} on {} line(s):\n",
                c.at,
                c.lines.len()
            ));
            for l in c.lines.iter().take(SHOWN) {
                out.push_str(&format!("  {l}\n"));
            }
            if c.lines.len() > SHOWN {
                out.push_str(&format!(
                    "  … and {} more: git grep -n {item} {MAIN}\n",
                    c.lines.len() - SHOWN
                ));
            }
            out.push('\n');
        }
        Err(why) => out.push_str(&format!(
            "origin/main was NOT searched for {item}: {why}\n\n"
        )),
    }
    match &prior.parts {
        Ok(p) if p.is_empty() => {
            out.push_str("No landed or parked car names this item on either car edge.\n\n")
        }
        Ok(p) => {
            out.push_str(&format!(
                "Cars already landed or parked for this item (partial or closing), {}:\n",
                p.len()
            ));
            for car in p {
                let id = car.get("id").and_then(Value::as_str).unwrap_or("?");
                let md = |k: &str| {
                    car.pointer(&format!("/metadata/{k}"))
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                };
                let summary: String = md("summary").chars().take(LINE_CHARS * 2).collect();
                out.push_str(&format!(
                    "  {} {} — {} at {}; train {}; recorded {}\n    {summary}\n",
                    short(id),
                    md("branch"),
                    if boss_jobs::car::is_landed(car) {
                        "landed"
                    } else {
                        "parked / in flight"
                    },
                    if boss_jobs::car::is_landed(car) {
                        md("merge_ref")
                    } else {
                        "not merged"
                    },
                    md("train"),
                    car.get("opened_at")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown")
                ));
            }
            out.push('\n');
        }
        Err(why) => out.push_str(&format!("The prior cars were NOT read: {why}\n\n")),
    }
    out.push_str(&format!(
        "A citation is not a fix: code names the packets that measured it as often as the \
         ones it closes, and a partial car is by definition not the whole. But each line above \
         is a place the fix may already be, and on 2026-09-25 two runs were spent finding out \
         that it was — 1e973965, fixed by cars filed under its parent, and 56727f95, whose \
         buildable plan had landed as partial cars (7562d7a0, 65a753b7). So read these FIRST. \
         If the fix is already on main, the report is the deliverable: name the lines that \
         prove it and build nothing (builder rule 15). If part of it is, build only what \
         remains and say which part. `boss dispatch {item}` refuses only on a car naming it \
         as its item; nothing else here was judged for you.\n"
    ));
    Some(out)
}

/// The one line `boss dispatch` prints to stderr BEFORE the claim.
pub(crate) fn warning(prior: &PriorWork) -> Option<String> {
    let item = short(&prior.item);
    let mut said = Vec::new();
    match &prior.citations {
        Ok(c) if !c.lines.is_empty() => said.push(format!(
            "origin/main ({}) cites it on {} line(s)",
            c.at,
            c.lines.len()
        )),
        Ok(_) => {}
        Err(why) => said.push(format!("origin/main was not searched ({why})")),
    }
    match &prior.parts {
        Ok(p) if !p.is_empty() => said.push(format!("{} landed or parked car(s) name it", p.len())),
        Ok(_) => {}
        Err(why) => said.push(format!("the partial cars were not read ({why})")),
    }
    (!said.is_empty()).then(|| {
        format!(
            "boss dispatch: {item} is already named — {}. The brief lists each under ALREADY \
             NAMED, above the step, and tells the builder to check them before building \
             (7562d7a0).",
            said.join("; ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEM: &str = "1e973965-392c-4208-9b27-233ae082f4ae";

    fn car(id: &str, key: &str, item: &str, landed: bool) -> Value {
        let mut c = json!({
            "id": id,
            "status": if landed { "closed" } else { "open" },
            "metadata": {
                key: item,
                "branch": format!("fix/{}", &id[..8]),
                "merge_ref": "fcdc6fb764ae",
                "summary": "The convert door now refuses a re-pin it cannot make true.",
            },
        });
        if landed {
            c["metadata"]["outcome"] = json!("merged");
        }
        c
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Only a backlog-item is searched: it is the only packet whose fix
    /// can already be on main.
    #[test]
    fn parked_partial_work_is_named_before_another_builder_starts() {
        let parked = car(
            "27073bd0-0000-4000-8000-000000000002",
            "partial_item",
            ITEM,
            false,
        );
        let parts = landed_parts(ITEM, &[parked]);
        assert_eq!(
            parts.len(),
            1,
            "parked partial work must not disappear from the brief"
        );
    }

    #[test]
    fn a_car_also_answering_this_item_is_not_hidden() {
        let mut related = car(
            "27073bd0-0000-4000-8000-000000000002",
            "backlog_item",
            "another-item",
            false,
        );
        related["metadata"][boss_jobs::car::ALSO_ANSWERS] = json!([ITEM]);
        assert_eq!(landed_parts(ITEM, &[related]).len(), 1);
    }

    #[tokio::test]
    async fn native_metadata_contract_keeps_all_three_edges_across_pages() {
        let closing = car(
            "aaaaaaaa-0000-4000-8000-000000000003",
            "backlog_item",
            ITEM,
            true,
        );
        let partial = car(
            "27073bd0-0000-4000-8000-000000000002",
            "partial_item",
            ITEM,
            false,
        );
        let mut related = car(
            "bbbbbbbb-0000-4000-8000-000000000004",
            "backlog_item",
            "other",
            false,
        );
        related["metadata"][boss_jobs::car::ALSO_ANSWERS] = json!(["other", ITEM]);
        let mut foreign = car(
            "cccccccc-0000-4000-8000-000000000005",
            "backlog_item",
            "other",
            true,
        );
        foreign["metadata"][boss_jobs::car::ALSO_ANSWERS] = json!(["other"]);
        let mut overlap = closing.clone();
        overlap["metadata"][boss_jobs::car::ALSO_ANSWERS] = json!([ITEM]);
        let rows = vec![overlap, partial, foreign, related];
        let body = read_cars(ITEM, |path| {
            let rows = rows.clone();
            async move {
                let params = path.split_once('?').unwrap().1.split('&')
                    .map(|p| p.split_once('=').unwrap()).collect::<std::collections::BTreeMap<_, _>>();
                // This is the server's actual shared wire parser, not a stub
                // accepting a query the native API refuses (9afea697).
                let contains = params.get("metadata").map(|text| {
                    let decoded = percent_encoding::percent_decode_str(text).decode_utf8().unwrap();
                    boss_jobs::metadata_containment::parse(&decoded).map_err(anyhow::Error::msg)
                }).transpose()?;
                let selected = rows.into_iter().filter(|row| {
                    contains.as_ref().is_none_or(|doc| doc.iter().all(|(key, value)| row["metadata"].get(key) == Some(value)))
                        && params.get("metadata_has").is_none_or(|key| row["metadata"].get(*key).is_some())
                }).collect::<Vec<_>>();
                let offset = params["offset"].parse::<usize>().unwrap();
                let total = selected.len();
                Ok(Some(json!({"total": total, "data": selected.into_iter().skip(offset).take(1).collect::<Vec<_>>() })))
            }
        }).await.expect("the native contract must accept every query");
        let rows = crate::train::rows(body).unwrap();
        assert_eq!(rows.len(), 4, "cross-query overlap is deduplicated");
        let related = landed_parts(ITEM, &rows);
        assert_eq!(
            related.len(),
            3,
            "foreign array edges must be filtered locally"
        );
        assert!(
            related
                .iter()
                .any(|row| row["id"] == "bbbbbbbb-0000-4000-8000-000000000004"),
            "the array-only edge on the last page must remain visible"
        );
    }

    #[tokio::test]
    async fn array_edge_read_failures_never_publish_a_partial_success() {
        for (fault, expected) in [
            ("failed", "native read failed"),
            ("body", "no body"),
            ("total", "total"),
            ("changed", "total changed"),
            ("duplicate", "repeats id"),
            ("tail", "unread tail"),
            ("id", "no id"),
        ] {
            let answer = read_cars(ITEM, |path| async move {
                let row = car(
                    "aaaaaaaa-0000-4000-8000-000000000003",
                    "backlog_item",
                    ITEM,
                    true,
                );
                // First two edges succeeded: failure of the broader array
                // read must still make the entire approach unknown.
                if !path.contains("metadata_has=") {
                    return Ok(Some(json!({"total": 1, "data": [row]})));
                }
                if path.contains("offset=0") {
                    return Ok(Some(json!({"total": 2, "data": [row]})));
                }
                match fault {
                    "failed" => anyhow::bail!("native read failed"),
                    "body" => Ok(None),
                    "total" => Ok(Some(json!({"data": [row]}))),
                    "changed" => Ok(Some(json!({"total": 3, "data": [row]}))),
                    "duplicate" => Ok(Some(json!({"total": 2, "data": [row]}))),
                    "tail" => Ok(Some(json!({"total": 2, "data": []}))),
                    "id" => Ok(Some(
                        json!({"total": 2, "data": [{"metadata": {"also_answers": [ITEM]}}]}),
                    )),
                    _ => unreachable!(),
                }
            })
            .await
            .expect_err("a failed tail must not become complete evidence");
            assert!(answer.to_string().contains(expected), "{fault}: {answer:#}");
            let prior = PriorWork {
                item: ITEM.into(),
                citations: Ok(Citations {
                    at: "control".into(),
                    lines: vec![],
                }),
                parts: Err(answer.to_string()),
            };
            assert!(
                section(&prior)
                    .unwrap()
                    .contains("The prior cars were NOT read")
            );
        }
    }

    #[tokio::test]
    async fn reads_closing_and_partial_edges_and_refuses_an_unread_tail() {
        let body = read_cars(ITEM, |path| async move {
            let closing = percent_encoding::percent_decode_str(&path).decode_utf8().unwrap().contains("backlog_item");
            Ok(Some(json!({ "total": 1, "data": [car(
                if closing { "aaaaaaaa-0000-4000-8000-000000000003" } else { "27073bd0-0000-4000-8000-000000000002" },
                if closing { "backlog_item" } else { "partial_item" }, ITEM, closing)] })))
        }).await.expect("both native edges read");
        assert_eq!(crate::train::rows(body).unwrap().len(), 2);
        let incomplete = read_cars(ITEM, |path| async move {
            Ok(Some(if path.contains("offset=0") {
                json!({ "total": 2, "data": [car("27073bd0-0000-4000-8000-000000000002", "partial_item", ITEM, false)] })
            } else { json!({ "total": 2, "data": [] }) }))
        }).await;
        assert!(
            incomplete.is_err(),
            "an unread tail must be said, not silently dropped"
        );
    }

    #[test]
    fn a_closing_car_is_named_with_its_train_and_recorded_date() {
        let mut closing = car(
            "0d9163d3-0000-4000-8000-000000000001",
            "backlog_item",
            ITEM,
            true,
        );
        closing["metadata"]["train"] = json!("7a1b2c3d-0000-4000-8000-000000000001");
        closing["opened_at"] = json!("2026-09-24T00:58:39Z");
        let parts = landed_parts(ITEM, &[closing]);
        assert_eq!(
            parts.len(),
            1,
            "closing work must remain visible on a forced brief"
        );
        let prior = PriorWork {
            item: ITEM.into(),
            citations: Ok(Citations {
                at: "03285637".into(),
                lines: vec![],
            }),
            parts: Ok(parts),
        };
        let section = section(&prior).expect("already performed work");
        assert!(
            section.contains("7a1b2c3d-0000-4000-8000-000000000001"),
            "{section}"
        );
        assert!(section.contains("2026-09-24T00:58:39Z"), "{section}");
    }

    #[test]
    fn only_a_backlog_item_is_searched() {
        assert_eq!(
            item_of(&json!({ "id": ITEM, "kind": "backlog-item" })),
            Some(ITEM)
        );
        assert_eq!(item_of(&json!({ "id": ITEM, "kind": "page-audit" })), None);
        assert_eq!(item_of(&json!({ "kind": "backlog-item" })), None);
    }

    /// The partial read is narrowed at the server on `partial_item`,
    /// the key a piece of a plan carries (65a753b7).
    #[test]
    fn the_partial_read_is_narrowed_at_the_server() {
        let q = partial_cars_query(ITEM);
        assert!(
            q.starts_with("/api/jobs?kind=ship-a-change&metadata="),
            "{q}"
        );
        let doc = percent_encoding::percent_decode_str(q.split("metadata=").nth(1).unwrap())
            .decode_utf8()
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(
            serde_json::from_str::<Value>(&doc).unwrap(),
            json!({ "partial_item": ITEM })
        );
    }

    /// Measured on 56727f95 (65a753b7): its plan's buildable car landed
    /// as partial cars. Those are named; a partial car still in flight,
    /// one naming another item, and a car naming it as its ITEM (the
    /// landed-work door's, which refuses) are not.
    #[test]
    fn the_landed_parts_are_read_off_each_row() {
        let rows = [
            car(
                "0d9163d3-0000-4000-8000-000000000001",
                "partial_item",
                ITEM,
                true,
            ),
            car(
                "27073bd0-0000-4000-8000-000000000002",
                "partial_item",
                ITEM,
                false,
            ),
            car(
                "aaaaaaaa-0000-4000-8000-000000000003",
                "partial_item",
                "other",
                true,
            ),
            car(
                "bbbbbbbb-0000-4000-8000-000000000004",
                "backlog_item",
                ITEM,
                true,
            ),
        ];
        let parts = landed_parts(ITEM, &rows);
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert_eq!(parts[0]["id"], "0d9163d3-0000-4000-8000-000000000001");
    }

    /// The tree half reads ORIGIN/MAIN, never the working tree: a branch
    /// citing the packet has not landed anything. Real git, in a scratch
    /// repo this process owns.
    #[test]
    fn the_tree_half_reads_origin_main_and_not_the_worktree() {
        let root = boss_testing::scratch::scratch_dir("prior-work-tree");
        boss_testing::scratch::create_dir(&root);
        git(&root, &["init", "-q", "-b", "main"]);
        boss_testing::scratch::write_file(
            &root.join("repin.rs"),
            "fn a() {}\n//! (backlog 1e973965). An interim car refused every such move\n",
        );
        boss_testing::scratch::write_file(&root.join("other.rs"), "fn b() {}\n");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "landed"]);

        // No origin/main yet: said, never read as "cites nothing".
        let unread = tree_citations(&root, "1e973965").expect_err("no origin/main");
        assert!(unread.contains("origin/main"), "{unread}");

        git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        // A working-tree edit that has not landed.
        boss_testing::scratch::write_file(&root.join("other.rs"), "// 1e973965 here too\n");

        let c = tree_citations(&root, "1e973965").expect("read");
        assert_eq!(c.at.len(), 12, "{c:?}");
        assert_eq!(
            c.lines,
            vec!["repin.rs:2  //! (backlog 1e973965). An interim car refused every such move"],
        );
        assert_eq!(
            tree_citations(&root, "56727f95").expect("read").lines,
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_cited_line_keeps_its_path_and_number_and_is_cut() {
        let long = "x".repeat(400);
        let lines = cited_lines(
            &format!("origin/main:a/b.rs:7:    // backlog 1e973965\norigin/main:c.js:1:{long}\n"),
            "origin/main",
        );
        assert_eq!(lines[0], "a/b.rs:7  // backlog 1e973965");
        assert!(lines[1].starts_with("c.js:1  xxx"), "{}", lines[1]);
        assert!(lines[1].ends_with(" …"), "{}", lines[1]);
        assert!(lines[1].len() < 200, "{}", lines[1].len());
    }

    /// The 1e973965 case, in the shape it had: origin/main citing it in
    /// the code that fixed it, no car naming it. The section names each
    /// line, says what to do with them, and cites why it exists.
    #[test]
    fn the_section_names_each_citation_and_each_landed_part() {
        let prior = PriorWork {
            item: ITEM.into(),
            citations: Ok(Citations {
                at: "faf5c47b1234".into(),
                lines: vec![
                    "crates/core/boss-jobs/src/repin.rs:16  //! (backlog 1e973965).".into(),
                    "crates/core/boss-jobs/src/registry.rs:1823  /// differs (1e973965).".into(),
                ],
            }),
            parts: Ok(vec![car(
                "0d9163d3-0000-4000-8000-000000000001",
                "partial_item",
                ITEM,
                true,
            )]),
        };
        let s = section(&prior).expect("something is named");
        assert!(s.starts_with("== ALREADY NAMED"), "{s}");
        assert!(s.contains("faf5c47b1234"), "{s}");
        assert!(
            s.contains("  crates/core/boss-jobs/src/repin.rs:16  "),
            "{s}"
        );
        assert!(s.contains("registry.rs:1823"), "{s}");
        assert!(
            s.contains("0d9163d3 fix/0d9163d3 — landed at fcdc6fb764ae"),
            "{s}"
        );
        assert!(s.contains("refuses a re-pin"), "the part's summary: {s}");
        assert!(s.contains("build nothing"), "{s}");
        assert!(s.contains("7562d7a0"), "{s}");

        let w = warning(&prior).expect("warned");
        assert!(w.contains("cites it on 2 line(s)"), "{w}");
        assert!(w.contains("1 landed or parked car(s) name it"), "{w}");
    }

    /// Past SHOWN lines the section names the command that lists them
    /// all, instead of a digest.
    #[test]
    fn a_long_list_names_the_command_for_the_rest() {
        let prior = PriorWork {
            item: ITEM.into(),
            citations: Ok(Citations {
                at: "faf5c47b1234".into(),
                lines: (0..SHOWN + 3)
                    .map(|n| format!("f.rs:{n}  1e973965"))
                    .collect(),
            }),
            parts: Ok(vec![]),
        };
        let s = section(&prior).expect("named");
        assert!(s.contains(&format!("f.rs:{}", SHOWN - 1)), "{s}");
        assert!(!s.contains(&format!("f.rs:{SHOWN} ")), "{s}");
        assert!(
            s.contains("… and 3 more: git grep -n 1e973965 origin/main"),
            "{s}"
        );
    }

    /// Nothing found anywhere is no section and no warning; a half that
    /// could not be read is SAID, in both.
    #[test]
    fn nothing_found_is_silent_and_an_unread_half_is_said() {
        let clean = PriorWork {
            item: ITEM.into(),
            citations: Ok(Citations {
                at: "faf5c47b1234".into(),
                lines: vec![],
            }),
            parts: Ok(vec![]),
        };
        assert_eq!(section(&clean), None);
        assert_eq!(warning(&clean), None);

        let blind = PriorWork {
            citations: Err("/w has no origin/main to search".into()),
            ..clean.clone()
        };
        let s = section(&blind).expect("the unread half is said");
        assert!(s.contains("NOT searched"), "{s}");
        assert!(s.contains("/w has no origin/main"), "{s}");
        assert!(warning(&blind).expect("said").contains("not searched"));

        let dark = PriorWork {
            parts: Err("a list read answered no JSON body".into()),
            ..clean
        };
        assert!(section(&dark).expect("said").contains("NOT read"));
    }
}
