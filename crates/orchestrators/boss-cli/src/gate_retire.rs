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
//! A SUCCESSOR IS NAMED, NEVER COMPUTED (backlog 1fb357f4, from review
//! 10cfcb8b). The first cut resolved any revision git would, so `main`,
//! `HEAD~400` or the red's own base each named a commit on main and
//! retired a red without naming what replaced it — reproduced on
//! 2026-10-19 by a dry run on gate-run b1e25027. So `--superseded-by`
//! is a landed car (its branch, or enough of its id to pick ONE) or a
//! commit spelled as 7-40 hex digits, and nothing else. A commit names
//! no car and no item, so the machine also proves it CARRIES the red's
//! work: merging the red's gated head into it (`git merge-tree`, the
//! whole-tree reading `boss car retire` uses) must write the commit's
//! own tree, or the files that differ are named and it is refused. A
//! GREEN is refused unless `--green-anyway '<why>'` asks: a green no car
//! carries is stranded, not superseded, and retiring it drops it from
//! the stranded-green list.
//!
//! THE RECORD. One metadata merge on the gate-run, through the job
//! metadata door, which records `jobs.job.updated` with the actor in the
//! same transaction: `superseded_by` (the retire door's own key, which
//! the stranded-green markers already read as SPENT), what the operator
//! typed beside it (`superseded_by_given`), `retire_evidence`,
//! `retired_by`, and `green_anyway` when a green was retired on purpose.
//! `yard::garage` and `yard::limbo` drop a branch whose latest run
//! carries `superseded_by`.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use boss_jobs::car;
use boss_jobs::car_retire::{RETIRE_EVIDENCE, SUPERSEDED_BY};

use crate::car_retire::git;

/// Who judged the successor to carry the red's work — the actor the
/// verb signed as.
pub(crate) const RETIRED_BY: &str = "retired_by";

/// What the operator typed for `--superseded-by`, verbatim, beside the
/// successor it resolved to — the record says what was NAMED, not only
/// what git made of it (backlog 1fb357f4).
pub(crate) const SUPERSEDED_BY_GIVEN: &str = "superseded_by_given";

/// Why a GREEN was retired anyway — the explicit ask, recorded.
pub(crate) const GREEN_ANYWAY: &str = "green_anyway";

/// PURE: is `given` a commit spelled as one — 7 to 40 hex digits and
/// nothing else? No ref, no `~`, `^` or `@{…}`: the text an operator
/// copies from a log line, never one git computes a commit from.
pub(crate) fn is_commit_name(given: &str) -> bool {
    (7..=40).contains(&given.len()) && given.chars().all(|c| c.is_ascii_hexdigit())
}

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
///
/// A GREEN is refused unless `green_anyway` says why (F2 of review
/// 10cfcb8b): a green no car carries is STRANDED — work ready to board
/// that nobody parked — and retiring it takes it off the stranded-green
/// list, so that is never a default. The reason is refused blank, and
/// refused on a run that is not green, where it would record an ask
/// nobody needed to make.
pub(crate) fn retire_refusal(
    job: &Value,
    cars: &[Value],
    green_anyway: Option<&str>,
) -> Option<String> {
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
        Some(v) => {
            let green = v == boss_jobs::stranded::VERDICT_GREEN;
            match green_anyway.map(str::trim) {
                Some("") => {
                    return Some(
                        "--green-anyway is blank — say why this green is superseded rather \
                         than stranded"
                            .into(),
                    );
                }
                None if green => {
                    return Some(format!(
                        "gate-run {id} is GREEN — a green no car carries is stranded, not \
                         superseded: park it, or rebase and re-gate it with its park flags. If a \
                         landed successor truly carries its work, say why with --green-anyway \
                         '<why>', which is recorded"
                    ));
                }
                Some(_) if !green => {
                    return Some(format!(
                        "--green-anyway is for a green, and gate-run {id} is {v} — drop it"
                    ));
                }
                _ => {}
            }
        }
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
    /// A commit this clone can read, spelled by the operator as hex
    /// (`given`) and resolved in full (`sha`).
    Commit { given: String, sha: String },
}

impl Successor {
    /// The value `superseded_by` records: the successor's branch and
    /// car, or the commit in full.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Car { id, branch, .. } => format!("{branch} (car {})", &id[..8.min(id.len())]),
            Self::Commit { sha, .. } => sha.clone(),
        }
    }
}

/// Whether a successor commit holds the red's work: the red's gated
/// `head` merged into it in memory, and the paths whose merged content
/// is not the commit's own (a conflict marked as one). `carried` only
/// when the merge is clean and writes the commit's own tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Carriage {
    pub head: String,
    pub carried: bool,
    pub files: Vec<String>,
}

impl Carriage {
    /// PURE: read a whole-tree merge of the red's head into the successor.
    pub(crate) fn of(head: &str, t: &crate::car_retire::WholeTree) -> Self {
        let conflicted = t.conflicted.iter().map(|f| format!("{f} (conflict)"));
        let differing = t
            .differing
            .iter()
            .filter(|f| !t.conflicted.contains(f))
            .cloned();
        Self {
            head: head.to_string(),
            carried: t.on_main(),
            files: conflicted.chain(differing).collect(),
        }
    }
}

/// PURE: the successor `given` names among every car, open and closed —
/// a LANDED car by branch, then by 8+ characters of its id (which must
/// pick ONE car: a prefix two ids share is refused, never the first hit,
/// F7); a car that has not landed is refused by name; otherwise a commit
/// spelled as 7-40 hex digits, which `sha` (what this clone resolved it
/// to) must begin with — a ref shadowing that spelling is refused. Any
/// other text is a revision expression and refused before git is asked:
/// `main`, `HEAD~N` and `<branch>~N` name a commit on main whatever the
/// red carried. `own` is the red's own branch: a branch cannot supersede
/// itself, and a landed car on it already settles the name.
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
    let by_id: Vec<&Value> = cars
        .iter()
        .filter(|c| {
            given.len() >= 8
                && c.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with(given))
        })
        .collect();
    if by_id.len() > 1 && car::landed_car_for(cars, given).is_none() {
        return Err(format!(
            "`{given}` begins {} cars' ids ({}) — name enough of the id to pick one",
            by_id.len(),
            // In full: two ids sharing the prefix share its short form too.
            by_id
                .iter()
                .map(|c| c.get("id").and_then(Value::as_str).unwrap_or("?"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let landed = car::landed_car_for(cars, given)
        .or_else(|| by_id.first().copied().filter(|c| car::is_landed(c)));
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
        .find(|c| md(c, "branch") == Some(given))
        .or_else(|| by_id.first().copied())
    {
        return Err(format!(
            "car {} ({}) has not landed ({}, outcome {}) — a successor must already be on main",
            short(c),
            md(c, "branch").unwrap_or("?"),
            c.get("status").and_then(Value::as_str).unwrap_or("?"),
            md(c, "outcome").unwrap_or("none recorded")
        ));
    }
    if !is_commit_name(given) {
        return Err(format!(
            "`{given}` names no car and is not a commit sha — a revision expression (`main`, \
             `HEAD~N`, `<branch>~N`) resolves to a commit on main whatever the red carried, so \
             it never names a successor. Name the landed car's branch or 8+ characters of its \
             id, or the 7-40 hex sha of the commit that carries the red's work"
        ));
    }
    match sha {
        Some(sha) if sha.starts_with(&given.to_ascii_lowercase()) => Ok(Successor::Commit {
            given: given.to_string(),
            sha: sha.to_string(),
        }),
        Some(sha) => Err(format!(
            "`{given}` resolved to {sha}, which is not the commit it spells — a ref of that name \
             shadows it; name more of the sha"
        )),
        None => Err(format!(
            "`{given}` is not a commit this clone can read (or more than one begins with it) — \
             fetch it, name more of it, or name the landed car's branch or id"
        )),
    }
}

/// PURE: the evidence the retirement records, or the refusal. `on_main`
/// is git's answer to "is the successor's merge (or the commit) an
/// ancestor of the forge's `main`" — `None` only for a landed car that
/// records no merge, which is accepted on its landed record and says so.
/// `carriage` is read for a commit, and a commit is accepted only when
/// it carries the red's work: it names no car or item, so landing alone
/// says nothing about what it replaced. A landed car's carriage stays
/// the operator's signed judgement, as `boss car retire` would prove it.
pub(crate) fn evidence(
    s: &Successor,
    main: &str,
    on_main: Option<bool>,
    carriage: Option<&Carriage>,
) -> Result<String, String> {
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
        (Successor::Commit { given, sha }, Some(true)) => match carriage {
            Some(Carriage {
                head,
                carried: true,
                ..
            }) => Ok(format!(
                "commit {sha} (named {given}) is an ancestor of the forge's main {main}, and \
                 merging the red's head {head} into it writes its own tree — it carries the \
                 red's work"
            )),
            Some(Carriage { head, files, .. }) => Err(format!(
                "commit {sha} is on the forge's main {main} but does not carry the red's work: \
                 merging the red's head {head} into it changes {} — name the commit or the \
                 landed car that carried it",
                if files.is_empty() {
                    "its tree".to_string()
                } else {
                    files.join(", ")
                }
            )),
            None => Err(format!(
                "commit {sha}: whether it carries the red's work was not read — refused rather \
                 than assumed"
            )),
        },
        (Successor::Commit { sha, .. }, _) => Err(format!(
            "commit {sha} is not an ancestor of the forge's main {main} — a successor must have \
             landed"
        )),
    }
}

/// PURE: the one metadata merge the retirement writes onto the gate-run.
/// `given` is what the operator typed, kept beside what it resolved to.
pub(crate) fn retire_patch(
    successor: &Successor,
    given: &str,
    evidence: &str,
    actor: &str,
    green_anyway: Option<&str>,
) -> Value {
    let mut patch = json!({
        SUPERSEDED_BY: successor.label(),
        SUPERSEDED_BY_GIVEN: given.trim(),
        RETIRE_EVIDENCE: evidence,
        RETIRED_BY: actor,
    });
    if let Some(why) = green_anyway {
        patch[GREEN_ANYWAY] = json!(why.trim());
    }
    patch
}

fn is_ancestor(repo: &Path, sha: &str, main: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", sha, main]).is_ok()
}

/// The head the red was gated at — its recorded `sha` — with its objects
/// here, fetched from the forge's branch once if the clone lacks them.
fn red_head(repo: &Path, job: &Value) -> Result<String, String> {
    let sha = md(job, "sha").ok_or(
        "the gate-run records no head `sha`, so nothing can show a commit carries its work \
         — name the landed car that did",
    )?;
    let have = |s: &str| git(repo, &["cat-file", "-e", &format!("{s}^{{commit}}")]).is_ok();
    if !have(sha)
        && let Some(branch) = md(job, "branch")
    {
        let _ = git(
            repo,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("refs/heads/{branch}"),
            ],
        );
    }
    if !have(sha) {
        return Err(format!(
            "the red's head {sha} is not readable in this clone, so nothing shows a commit \
             carries its work — name the landed car that did"
        ));
    }
    Ok(sha.to_string())
}

/// Whether `successor` holds the red's work, read by git.
fn carriage(repo: &Path, job: &Value, successor: &str) -> Result<Carriage, String> {
    let head = red_head(repo, job)?;
    let t = crate::car_retire::whole_tree(repo, successor, &head)?;
    Ok(Carriage::of(&head, &t))
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
pub(crate) async fn retire(
    packet_ref: &str,
    superseded_by: &str,
    green_anyway: Option<&str>,
    dry_run: bool,
) -> Result<()> {
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
    if let Some(why) = retire_refusal(&job, &cars, green_anyway) {
        bail!("boss gate --retire: REFUSED — {why}");
    }
    let branch = md(&job, "branch").unwrap_or("?").to_string();
    let short = &packet[..8.min(packet.len())];
    println!("boss gate --retire: gate-run {short} ({branch})");

    let repo = Path::new(".");
    let main = crate::car_retire::forge_main(repo)?;
    // Git is asked only about a name spelled as a commit: a revision
    // expression is refused before anything resolves it.
    let given = superseded_by.trim();
    let sha = if is_commit_name(given) {
        rev_parse(repo, given)
    } else {
        None
    };
    let successor = resolve_successor(&cars, superseded_by, sha.as_deref(), &branch)
        .map_err(|e| anyhow!("boss gate --retire: REFUSED — {e}"))?;
    let mut carried = None;
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
        Successor::Commit { sha, .. } => {
            let landed = is_ancestor(repo, sha, &main);
            // Carriage is read only for a landed commit: one off main is
            // refused on that alone.
            if landed {
                carried = Some(
                    carriage(repo, &job, sha)
                        .map_err(|e| anyhow!("boss gate --retire: REFUSED — {e}"))?,
                );
            }
            Some(landed)
        }
    };
    let proof = evidence(&successor, &main, on_main, carried.as_ref())
        .map_err(|e| anyhow!("boss gate --retire: REFUSED — {e}"))?;
    let patch = retire_patch(&successor, given, &proof, &actor, green_anyway);
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
        assert_eq!(retire_refusal(&red("feat/x"), &cars, None), None);
    }

    /// A GREEN no car carries is stranded, not superseded (F2 of review
    /// 10cfcb8b): refused unless --green-anyway says why, and the reason
    /// is refused blank and refused on a run that is not green.
    #[test]
    fn a_green_is_retired_only_when_asked_with_a_reason() {
        let mut green = red("feat/x");
        green["steps"] = json!([{"metadata": {"verdict": "green"}}]);
        let refused = retire_refusal(&green, &[], None).expect("a green is refused by default");
        assert!(
            refused.contains("stranded, not superseded") && refused.contains("--green-anyway"),
            "{refused}"
        );
        assert_eq!(
            retire_refusal(&green, &[], Some("its work rode car 1234abcd")),
            None
        );
        let blank = retire_refusal(&green, &[], Some("  ")).expect("a blank reason is refused");
        assert!(blank.contains("blank"), "{blank}");
        let on_red = retire_refusal(&red("feat/x"), &[], Some("why"))
            .expect("--green-anyway on a red is refused");
        assert!(on_red.contains("is for a green"), "{on_red}");
        // A lost run is not green, and is retired as a red is.
        let mut lost = red("feat/x");
        lost["steps"] = json!([{"metadata": {"verdict": "lost"}}]);
        assert_eq!(retire_refusal(&lost, &[], None), None);
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
            let refusal = retire_refusal(&job, &cars, None)
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
            ("feat/never", "names no car and is not a commit sha"),
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
                given: "05ccb83".into(),
                sha: "05ccb8383aaa".into()
            }
        );
    }

    /// A commit is 7-40 hex digits, what git resolved must BEGIN with it
    /// (a ref of that name shadowing the sha is refused), and a car-id
    /// prefix must pick one car, never the first hit (F7).
    #[test]
    fn a_commit_is_spelled_as_hex_and_a_car_prefix_picks_one() {
        let full = "05ccb8383aaabbbbccccddddeeeeffff00001111";
        assert!(is_commit_name("05ccb83") && is_commit_name(full));
        assert!(is_commit_name("05CCB83"), "case is the operator's");
        for not in [
            "05ccb8",
            "05ccb8383aaabbbbccccddddeeeeffff000011112",
            "05ccb8g",
            "",
        ] {
            assert!(!is_commit_name(not), "{not:?}");
        }
        let s = resolve_successor(&[], "05CCB83", Some(full), "feat/x").unwrap();
        assert_eq!(s.label(), full, "the record holds the full sha");
        let shadow = resolve_successor(&[], "deadbee", Some(full), "feat/x").unwrap_err();
        assert!(shadow.contains("not the commit it spells"), "{shadow}");
        let unread = resolve_successor(&[], "deadbee", None, "feat/x").unwrap_err();
        assert!(
            unread.contains("not a commit this clone can read"),
            "{unread}"
        );

        let twins = vec![
            landed("cafe0123-0000-4000-8000-000000000001", "feat/a"),
            landed("cafe0123-1111-4000-8000-000000000002", "feat/b"),
        ];
        let e = resolve_successor(&twins, "cafe0123", Some(full), "feat/x").unwrap_err();
        assert!(
            e.contains("begins 2 cars' ids") && e.contains("cafe0123-1111"),
            "{e}"
        );
        assert_eq!(
            resolve_successor(&twins, "cafe0123-1111", None, "feat/x")
                .unwrap()
                .label(),
            "feat/b (car cafe0123)"
        );
    }

    /// A REVISION EXPRESSION IS NOT A SUCCESSOR (backlog 1fb357f4, F1 of
    /// review 10cfcb8b). `main`, `origin/main`, `HEAD~N` and a landed
    /// branch's `~N` all resolve to commits on main whatever the red
    /// carried, so each retired a red while naming nothing that replaced
    /// it — reproduced 2026-10-19 by a dry run on gate-run b1e25027,
    /// which `main`, `HEAD~3` and `origin/main~400` each would have
    /// retired. Refused even when git resolved the text to a commit.
    #[test]
    fn a_revision_expression_is_refused_even_when_git_resolves_it() {
        let cars = vec![landed("cccccccc-0000-4000-8000-000000000003", "feat/x-2")];
        let resolved = Some("05ccb8383aaabbbbccccddddeeeeffff00001111");
        for given in [
            "main",
            "origin/main",
            "HEAD",
            "HEAD~3",
            "HEAD~400",
            "feat/x-2~1",
            "feat/x-2^",
            "05ccb83~1",
            "main@{1}",
        ] {
            let e = resolve_successor(&cars, given, resolved, "feat/x")
                .expect_err(&format!("{given} must be refused"));
            assert!(e.contains("revision expression"), "{given}: {e:?}");
        }
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
        let ok = evidence(&car_s, main, Some(true), None).unwrap();
        assert!(
            ok.contains("merged as 1a2b3c4d") && ok.contains("05ccb8383aaa"),
            "{ok}"
        );
        let off = evidence(&car_s, main, Some(false), None).unwrap_err();
        assert!(off.contains("not on the forge's main"), "{off}");
        let commit = Successor::Commit {
            given: "abc1234".into(),
            sha: "abc1234ffff".into(),
        };
        let carried = Carriage {
            head: "feedface".into(),
            carried: true,
            files: vec![],
        };
        assert!(
            evidence(&commit, main, Some(false), Some(&carried))
                .unwrap_err()
                .contains("not an ancestor")
        );
        let ok = evidence(&commit, main, Some(true), Some(&carried)).unwrap();
        assert!(
            ok.contains("is an ancestor")
                && ok.contains("named abc1234")
                && ok.contains("carries the red's work"),
            "{ok}"
        );
        // A landed commit that does not hold the red's work — main, its
        // own base, anything — is refused naming the files that differ;
        // and one whose carriage was never read is refused, not assumed.
        let not = Carriage {
            head: "feedface".into(),
            carried: false,
            files: vec!["a.rs (conflict)".into(), "b.rs".into()],
        };
        let e = evidence(&commit, main, Some(true), Some(&not)).unwrap_err();
        assert!(
            e.contains("does not carry the red's work") && e.contains("a.rs (conflict), b.rs"),
            "{e}"
        );
        assert!(
            evidence(&commit, main, Some(true), None)
                .unwrap_err()
                .contains("not read")
        );
        let unrecorded = Successor::Car {
            id: "cccccccc-0000-4000-8000-000000000003".into(),
            branch: "feat/x-2".into(),
            merge_ref: None,
        };
        assert!(
            evidence(&unrecorded, main, None, None)
                .unwrap()
                .contains("was not read")
        );
    }

    /// The record: by (who judged it), successor and evidence, under the
    /// key the yard and the stranded markers already read — and what the
    /// operator typed beside what it resolved to, and a green's reason.
    #[test]
    fn the_patch_records_by_successor_and_evidence() {
        let s = Successor::Commit {
            given: "abc1234".into(),
            sha: "abc1234ffff".into(),
        };
        let p = retire_patch(
            &s,
            " abc1234 ",
            "commit abc1234ffff is an ancestor",
            "agent-claude",
            None,
        );
        assert_eq!(p["superseded_by"], "abc1234ffff");
        assert_eq!(p[SUPERSEDED_BY_GIVEN], "abc1234");
        assert_eq!(p["retire_evidence"], "commit abc1234ffff is an ancestor");
        assert_eq!(p["retired_by"], "agent-claude");
        assert!(p.get(GREEN_ANYWAY).is_none(), "{p}");
        assert_eq!(
            boss_jobs::stranded::spent_reason(&p),
            Some("superseded_by"),
            "the stranded-green census reads the same marker as spent"
        );
        let p = retire_patch(&s, "abc1234", "e", "agent-claude", Some(" rode car 1234 "));
        assert_eq!(p[GREEN_ANYWAY], "rode car 1234");
    }

    fn fixture(name: &str) -> (std::path::PathBuf, impl Fn(&[&str]) -> String) {
        let root = boss_testing::scratch::scratch_dir(name);
        let at = root.clone();
        let g = move |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&at)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        (root, g)
    }

    /// A REAL REPOSITORY: a red gated at its head; a train that squashed
    /// the same work in beside another car's; main moving on. The train
    /// commit carries the red; the red's own base, and a later commit
    /// that is on main but before nothing of the red's, do not — the two
    /// shapes F1 accepted — and each names the file it is missing.
    #[test]
    fn a_commit_carries_the_red_only_when_merging_the_red_into_it_changes_nothing() {
        let (root, g) = fixture("gate-retire-carriage");
        let w = |rel: &str, body: &str| boss_testing::scratch::write_file(&root.join(rel), body);
        g(&["init", "-q", "-b", "main"]);
        w("steps.rs", "fn a() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);

        g(&["checkout", "-q", "-b", "red"]);
        w("steps.rs", "fn a() {}\nfn freeze() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "red: a terminal step freezes"]);
        let red_head = g(&["rev-parse", "HEAD"]);

        g(&["checkout", "-q", "main"]);
        w("unrelated.rs", "other car\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "train: an unrelated car"]);
        let unrelated = g(&["rev-parse", "HEAD"]);
        w("steps.rs", "fn a() {}\nfn freeze() {}\n");
        w("dispatcher.rs", "the successor's own change\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "train: the successor lands"]);
        let train = g(&["rev-parse", "HEAD"]);
        w("later.rs", "main moves on\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "train: later"]);

        let job = json!({"metadata": {"branch": "red", "sha": red_head}});
        let repo = root.as_path();
        let c = carriage(repo, &job, &train).unwrap();
        assert!(c.carried && c.files.is_empty(), "{c:?}");
        for shape in [&base, &unrelated] {
            let c = carriage(repo, &job, shape).unwrap();
            assert!(!c.carried, "{shape}: {c:?}");
            assert_eq!(c.files, vec!["steps.rs".to_string()], "{shape}: {c:?}");
        }
        let gone = json!({"metadata": {"sha": "0123456789abcdef0123456789abcdef01234567"}});
        let e = carriage(repo, &gone, &train).unwrap_err();
        assert!(e.contains("not readable in this clone"), "{e}");
    }
}
