//! `boss gate --retire <gate-run> --superseded-by <landed branch | car |
//! merge sha>` — retire a gate-run no car ever claimed, because another
//! branch carrying the same work has LANDED (backlog c9c48222).
//!
//! WHY. Measured 2026-10-17 by a read-only garage triage: 14 of the 17
//! reds awaiting rework were superseded — a successor branch carrying
//! the same work was in a train commit on origin/main — and nothing
//! could take them out. `boss gate --withdraw` refuses once a runner
//! pod existed (true of all 14: they ran and judged); `boss car retire`
//! needs a car, and a red never parks; `yard::garage` drops a branch
//! only on a newer run under the SAME name, a settled car on it, or a
//! `rerail_origins` mention. So the garage read TROUBLED on residue, and
//! an operator could not tell its one live red from fourteen landed ones.
//!
//! WHAT IT PROVES, AND WHAT IT DOES NOT. The machine proves the
//! successor LANDED: a landed car whose recorded merge is an ancestor of
//! the forge's main, or a commit that is. That the successor carries
//! the same work as the red is the operator's judgement — a red never
//! became a car, so there is no item edge or commit set to compare —
//! and it goes on the record as one, signed: `retired_by` beside the
//! evidence. An unlanded successor, a car-carried branch, a run still
//! owed its verdict and a train's own gate are refused by name.
//!
//! THE RECORD. One metadata merge on the gate-run, through the job
//! metadata door, which records `jobs.job.updated` with the actor in the
//! same transaction: `superseded_by` (the retire door's own key, which
//! the stranded-green markers already read as SPENT), `retire_evidence`
//! and `retired_by`. `yard::garage` and `yard::limbo` drop a branch
//! whose latest run carries `superseded_by`.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use boss_jobs::car;
use boss_jobs::car_retire::{RETIRE_EVIDENCE, SUPERSEDED_BY};

use crate::car_retire::git;

/// Who judged the successor to carry the red's work — the actor the
/// verb signed as.
pub(crate) const RETIRED_BY: &str = "retired_by";

fn md<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.pointer(&format!("/metadata/{k}"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn short(v: &Value) -> &str {
    let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
    &id[..8.min(id.len())]
}

/// The verdict a gate-run recorded, read the way the yard reads it: the
/// first step metadata carrying a non-empty `verdict`.
fn verdict(job: &Value) -> Option<&str> {
    job.get("steps")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|s| s.pointer("/metadata/verdict").and_then(Value::as_str))
        .filter(|v| !v.is_empty())
}

/// PURE: why this gate-run cannot be retired as superseded, or `None`.
///
/// Only a CLOSED gate-run that recorded a verdict, on a branch no car
/// carries: an open run is owed its verdict (let it report, or withdraw
/// it before a pod exists); a withdrawn run is already out of every
/// lane; a train's or main's own gate is that train's verdict, never a
/// car; and a branch a car carries is retired through the car's own
/// door, `boss car retire`, where the car's commits can be proven.
pub(crate) fn retire_refusal(job: &Value, cars: &[Value]) -> Option<String> {
    let id = short(job);
    let kind = job.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "gate-run" {
        return Some(format!(
            "{id} is a {kind:?}, not a gate-run — this door retires a gate-run and nothing else"
        ));
    }
    let status = job.get("status").and_then(Value::as_str).unwrap_or("?");
    if status != "closed" {
        return Some(format!(
            "gate-run {id} is still {status}: its verdict is owed. Let it report, or `boss \
             gate --withdraw` it while no runner pod exists"
        ));
    }
    let meta = job.get("metadata").unwrap_or(&Value::Null);
    if boss_jobs::stranded::is_train_gate(meta) || boss_jobs::full_gate_on_main::is_main_gate(meta)
    {
        return Some(format!(
            "gate-run {id} is a train's or main's own gate — its verdict belongs to that train, \
             and no lane draws it as a car"
        ));
    }
    match verdict(job) {
        None => {
            return Some(format!(
                "gate-run {id} is closed with no verdict recorded — there is nothing it said \
                 about its branch to retire"
            ));
        }
        Some(v) if v == boss_jobs::yard::WITHDRAWN_VERDICT => {
            return Some(format!(
                "gate-run {id} was withdrawn — it never ran, and is already out of every lane"
            ));
        }
        Some(_) => {}
    }
    if let Some(by) = boss_jobs::stranded::marked(meta, SUPERSEDED_BY) {
        return Some(format!(
            "gate-run {id} is already retired as superseded by {by}"
        ));
    }
    let Some(branch) = md(job, "branch") else {
        return Some(format!(
            "gate-run {id} names no branch, so no lane groups it under one"
        ));
    };
    if let Some(c) = cars.iter().find(|c| md(c, "branch") == Some(branch)) {
        return Some(format!(
            "car {} carries {branch} ({}) — a car's work is retired through its own door, \
             `boss car retire {}`, where its commits are proven; a settled car already takes \
             the branch out of the garage",
            short(c),
            c.get("status").and_then(Value::as_str).unwrap_or("?"),
            short(c)
        ));
    }
    None
}

/// What `--superseded-by` named, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Successor {
    /// A landed car — by branch or by id — and the merge it records.
    Car {
        id: String,
        branch: String,
        merge_ref: Option<String>,
    },
    /// A commit this clone can read.
    Commit { sha: String },
}

impl Successor {
    /// The value `superseded_by` records: the successor's branch and
    /// car, or the commit.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Car { id, branch, .. } => format!("{branch} (car {})", &id[..8.min(id.len())]),
            Self::Commit { sha } => sha.clone(),
        }
    }
}

/// PURE: the successor `given` names among every car, open and closed —
/// a LANDED car by branch, then by 8+ characters of its id; a car that
/// has not landed is refused by name; otherwise the commit `given`
/// resolved to in this clone (`sha`). `own` is the red's own branch: a
/// branch cannot supersede itself, and a landed car on it already
/// settles the name.
pub(crate) fn resolve_successor(
    cars: &[Value],
    given: &str,
    sha: Option<&str>,
    own: &str,
) -> Result<Successor, String> {
    let given = given.trim();
    if given.is_empty() {
        return Err("--superseded-by is blank — name the successor that landed".into());
    }
    if given == own {
        return Err(format!(
            "{own} cannot supersede itself — a landed car on it already settles the name"
        ));
    }
    let by_id = |c: &&Value| {
        given.len() >= 8
            && c.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with(given))
    };
    let landed = car::landed_car_for(cars, given)
        .or_else(|| cars.iter().filter(by_id).find(|c| car::is_landed(c)));
    if let Some(c) = landed {
        return Ok(Successor::Car {
            id: c
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            branch: md(c, "branch").unwrap_or("?").to_string(),
            merge_ref: md(c, "merge_ref").map(str::to_string),
        });
    }
    if let Some(c) = cars
        .iter()
        .find(|c| md(c, "branch") == Some(given) || by_id(c))
    {
        return Err(format!(
            "car {} ({}) has not landed ({}, outcome {}) — a successor must already be on main",
            short(c),
            md(c, "branch").unwrap_or("?"),
            c.get("status").and_then(Value::as_str).unwrap_or("?"),
            md(c, "outcome").unwrap_or("none recorded")
        ));
    }
    match sha {
        Some(sha) => Ok(Successor::Commit {
            sha: sha.to_string(),
        }),
        None => Err(format!(
            "`{given}` is neither a landed car's branch or id nor a commit this clone can read \
             — name the successor's branch, its car, or the merge sha that landed it"
        )),
    }
}

/// PURE: the evidence the retirement records, or the refusal. `on_main`
/// is git's answer to "is the successor's merge (or the commit) an
/// ancestor of the forge's `main`" — `None` only for a landed car that
/// records no merge, which is accepted on its landed record and says so.
pub(crate) fn evidence(s: &Successor, main: &str, on_main: Option<bool>) -> Result<String, String> {
    let main = &main[..12.min(main.len())];
    match (s, on_main) {
        (
            Successor::Car {
                id,
                branch,
                merge_ref: Some(merge),
            },
            Some(true),
        ) => Ok(format!(
            "car {} ({branch}) landed, merged as {merge}, an ancestor of the forge's main {main}",
            &id[..8.min(id.len())]
        )),
        (
            Successor::Car {
                id,
                branch,
                merge_ref: Some(merge),
            },
            _,
        ) => Err(format!(
            "car {} ({branch}) records its merge as {merge}, which is not on the forge's main \
             {main} — a successor must have landed",
            &id[..8.min(id.len())]
        )),
        (
            Successor::Car {
                id,
                branch,
                merge_ref: None,
            },
            _,
        ) => Ok(format!(
            "car {} ({branch}) is recorded landed; it records no merge_ref, so its ancestry of \
             the forge's main {main} was not read",
            &id[..8.min(id.len())]
        )),
        (Successor::Commit { sha }, Some(true)) => Ok(format!(
            "commit {sha} is an ancestor of the forge's main {main}"
        )),
        (Successor::Commit { sha }, _) => Err(format!(
            "commit {sha} is not an ancestor of the forge's main {main} — a successor must have \
             landed"
        )),
    }
}

/// PURE: the one metadata merge the retirement writes onto the gate-run.
pub(crate) fn retire_patch(successor: &Successor, evidence: &str, actor: &str) -> Value {
    json!({
        SUPERSEDED_BY: successor.label(),
        RETIRE_EVIDENCE: evidence,
        RETIRED_BY: actor,
    })
}

fn is_ancestor(repo: &Path, sha: &str, main: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", sha, main]).is_ok()
}

fn rev_parse(repo: &Path, given: &str) -> Option<String> {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{given}^{{commit}}"),
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

/// `boss gate --retire`.
pub(crate) async fn retire(packet_ref: &str, superseded_by: &str, dry_run: bool) -> Result<()> {
    let http = crate::sor_client::machine_client()?;
    // The actor FIRST: a write nobody names is refused, and the refusal
    // costs a line rather than a half-judged retirement (5083d6f5).
    let actor = if dry_run {
        "dry-run".to_string()
    } else {
        crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?
    };
    let packet = crate::job::fetch_and_resolve(&http, packet_ref).await?;
    let path = format!("/api/jobs/{packet}");
    let job = crate::sor_client::api(&http, reqwest::Method::GET, &path, None)
        .await?
        .ok_or_else(|| anyhow!("gate-run {packet} read returned no body"))?;
    let cars = crate::sor_client::all_cars(&http).await?;
    if let Some(why) = retire_refusal(&job, &cars) {
        bail!("boss gate --retire: REFUSED — {why}");
    }
    let branch = md(&job, "branch").unwrap_or("?").to_string();
    let short = &packet[..8.min(packet.len())];
    println!("boss gate --retire: gate-run {short} ({branch})");

    let repo = Path::new(".");
    let main = crate::car_retire::forge_main(repo)?;
    let successor = resolve_successor(
        &cars,
        superseded_by,
        rev_parse(repo, superseded_by.trim()).as_deref(),
        &branch,
    )
    .map_err(|e| anyhow!("boss gate --retire: REFUSED — {e}"))?;
    let on_main = match &successor {
        Successor::Car {
            merge_ref: Some(merge),
            id,
            ..
        } => {
            let sha = rev_parse(repo, merge).ok_or_else(|| {
                anyhow!(
                    "boss gate --retire: REFUSED — car {}'s merge {merge} is not readable in this \
                     clone, so its landing cannot be read",
                    &id[..8.min(id.len())]
                )
            })?;
            Some(is_ancestor(repo, &sha, &main))
        }
        Successor::Car {
            merge_ref: None, ..
        } => None,
        Successor::Commit { sha } => Some(is_ancestor(repo, sha, &main)),
    };
    let proof = evidence(&successor, &main, on_main)
        .map_err(|e| anyhow!("boss gate --retire: REFUSED — {e}"))?;
    let patch = retire_patch(&successor, &proof, &actor);
    println!("  superseded by: {}", successor.label());
    println!("  evidence: {proof}");
    if dry_run {
        println!("boss gate --retire: DRY — nothing written; would merge {patch} onto {short}");
        return Ok(());
    }
    crate::sor_client::api(
        &http,
        reqwest::Method::PATCH,
        &format!("{path}/metadata"),
        Some(patch),
    )
    .await?;
    let back = crate::sor_client::api(&http, reqwest::Method::GET, &path, None)
        .await?
        .ok_or_else(|| anyhow!("gate-run {packet} read-back returned no body"))?;
    if md(&back, SUPERSEDED_BY) != Some(successor.label().as_str()) {
        bail!(
            "boss gate --retire: the write answered, but gate-run {short} reads back without \
             `{SUPERSEDED_BY}` = {:?} — the record says what it says; nothing more was written",
            successor.label()
        );
    }
    println!(
        "boss gate --retire: gate-run {short} ({branch}) retired as superseded by {}, signed \
         {actor}; the garage no longer draws it",
        successor.label()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red(branch: &str) -> Value {
        json!({
            "id": "02686d0a-2093-42cf-8ea9-7bcc330ccb80",
            "kind": "gate-run",
            "status": "closed",
            "metadata": {"branch": branch, "outcome": "failed"},
            "steps": [
                {"slug": "launch", "metadata": {}},
                {"slug": "record-verdict", "metadata": {"verdict": "failed"}}
            ]
        })
    }

    fn car_row(id: &str, branch: &str, status: &str, extra: Value) -> Value {
        let mut md = json!({"branch": branch});
        if let (Some(m), Some(e)) = (md.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        json!({"id": id, "kind": "ship-a-change", "status": status, "metadata": md})
    }

    fn landed(id: &str, branch: &str) -> Value {
        car_row(
            id,
            branch,
            "closed",
            json!({"outcome": "merged", "merge_ref": "05ccb8383"}),
        )
    }

    /// The ordinary case: a closed red on a branch no car ever carried.
    #[test]
    fn a_closed_carless_red_can_be_retired() {
        let cars = vec![landed("aaaaaaaa-0000-4000-8000-000000000001", "feat/other")];
        assert_eq!(retire_refusal(&red("feat/x"), &cars), None);
    }

    /// Every refusal names why — a gate-run whose record cannot be
    /// retired is never retired by default.
    #[test]
    fn a_run_that_is_not_a_carless_judged_gate_is_refused_by_name() {
        let cases: Vec<(Value, Vec<Value>, &str)> = vec![
            (
                {
                    let mut j = red("feat/x");
                    j["kind"] = json!("ship-a-change");
                    j
                },
                vec![],
                "not a gate-run",
            ),
            (
                {
                    let mut j = red("feat/x");
                    j["status"] = json!("open");
                    j
                },
                vec![],
                "verdict is owed",
            ),
            (
                {
                    let mut j = red("train/2026-10-17-1501");
                    j["metadata"]["train_gate"] = json!(true);
                    j
                },
                vec![],
                "own gate",
            ),
            (
                {
                    let mut j = red("feat/x");
                    j["steps"] = json!([{"metadata": {}}]);
                    j
                },
                vec![],
                "no verdict",
            ),
            (
                {
                    let mut j = red("feat/x");
                    j["steps"] = json!([{"metadata": {"verdict": "withdrawn"}}]);
                    j
                },
                vec![],
                "withdrawn",
            ),
            (
                {
                    let mut j = red("feat/x");
                    j["metadata"]["superseded_by"] = json!("feat/x-2 (car 11111111)");
                    j
                },
                vec![],
                "already retired",
            ),
            (
                red("feat/x"),
                vec![car_row(
                    "bbbbbbbb-0000-4000-8000-000000000002",
                    "feat/x",
                    "open",
                    json!({}),
                )],
                "boss car retire bbbbbbbb",
            ),
        ];
        for (job, cars, why) in cases {
            let refusal = retire_refusal(&job, &cars)
                .unwrap_or_else(|| panic!("{job} must be refused ({why})"));
            assert!(refusal.contains(why), "{refusal:?} should say {why:?}");
        }
    }

    /// The successor resolves to a LANDED car by branch or id, else to a
    /// commit this clone read; a car that has not landed is refused, and
    /// so is a name that is neither.
    #[test]
    fn a_successor_must_be_a_landed_car_or_a_commit() {
        let cars = vec![
            landed("cccccccc-0000-4000-8000-000000000003", "feat/x-2"),
            car_row(
                "dddddddd-0000-4000-8000-000000000004",
                "feat/x-3",
                "open",
                json!({}),
            ),
            car_row(
                "eeeeeeee-0000-4000-8000-000000000005",
                "feat/x-4",
                "closed",
                json!({"outcome": "abandoned"}),
            ),
        ];
        let by_branch = resolve_successor(&cars, "feat/x-2", None, "feat/x").unwrap();
        assert_eq!(by_branch.label(), "feat/x-2 (car cccccccc)");
        assert_eq!(
            resolve_successor(&cars, "cccccccc", None, "feat/x").unwrap(),
            by_branch,
            "a landed car by its id is the same successor"
        );
        for (given, why) in [
            ("feat/x-3", "has not landed (open"),
            ("feat/x-4", "outcome abandoned"),
            ("dddddddd", "has not landed"),
            ("feat/never", "neither a landed car"),
            ("feat/x", "cannot supersede itself"),
            ("  ", "blank"),
        ] {
            let e = resolve_successor(&cars, given, None, "feat/x")
                .expect_err(&format!("{given} must be refused"));
            assert!(e.contains(why), "{e:?} should say {why:?}");
        }
        assert_eq!(
            resolve_successor(&cars, "05ccb83", Some("05ccb8383aaa"), "feat/x").unwrap(),
            Successor::Commit {
                sha: "05ccb8383aaa".into()
            }
        );
    }

    /// Landed is OBSERVED on the forge's main: a merge or a commit that
    /// is not an ancestor of it is refused; a landed car with no merge
    /// recorded is accepted on its record and the evidence says main was
    /// not read.
    #[test]
    fn a_successor_off_main_is_refused_and_the_evidence_says_what_was_read() {
        let main = "05ccb8383aaabbbbccccdddd";
        let car_s = Successor::Car {
            id: "cccccccc-0000-4000-8000-000000000003".into(),
            branch: "feat/x-2".into(),
            merge_ref: Some("1a2b3c4d".into()),
        };
        let ok = evidence(&car_s, main, Some(true)).unwrap();
        assert!(
            ok.contains("merged as 1a2b3c4d") && ok.contains("05ccb8383aaa"),
            "{ok}"
        );
        let off = evidence(&car_s, main, Some(false)).unwrap_err();
        assert!(off.contains("not on the forge's main"), "{off}");
        let commit = Successor::Commit {
            sha: "abc123".into(),
        };
        assert!(
            evidence(&commit, main, Some(false))
                .unwrap_err()
                .contains("not an ancestor")
        );
        assert!(
            evidence(&commit, main, Some(true))
                .unwrap()
                .contains("is an ancestor")
        );
        let unrecorded = Successor::Car {
            id: "cccccccc-0000-4000-8000-000000000003".into(),
            branch: "feat/x-2".into(),
            merge_ref: None,
        };
        assert!(
            evidence(&unrecorded, main, None)
                .unwrap()
                .contains("was not read")
        );
    }

    /// The record: by (who judged it), successor and evidence, under the
    /// key the yard and the stranded markers already read.
    #[test]
    fn the_patch_records_by_successor_and_evidence() {
        let s = Successor::Commit {
            sha: "abc123".into(),
        };
        let p = retire_patch(&s, "commit abc123 is an ancestor", "agent-claude");
        assert_eq!(p["superseded_by"], "abc123");
        assert_eq!(p["retire_evidence"], "commit abc123 is an ancestor");
        assert_eq!(p["retired_by"], "agent-claude");
        assert_eq!(
            boss_jobs::stranded::spent_reason(&p),
            Some("superseded_by"),
            "the stranded-green census reads the same marker as spent"
        );
    }
}
