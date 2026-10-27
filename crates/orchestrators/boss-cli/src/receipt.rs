//! `boss receipt <car>` — what does this car's receipt actually vouch for?
//!
//! WHY THIS IS A VERB. The third of the three Class A questions in
//! backlog-item 26b3d203. A gate receipt is the only evidence a car
//! carries, and it vouches for something narrower than people read it
//! as. Two gaps, both of which have cost real work:
//!
//!   THE HEAD MOVED. A receipt names the sha it gated. Rebase or
//!   force-push the branch afterwards and the receipt still reads
//!   "green" while describing a tree that is no longer there. Three
//!   cars' stale receipts were "repaired" on 2026-08-27, the API
//!   answered 204 three times, nothing was written, and the cars were
//!   reported fixed while staying unboardable (09576fab).
//!
//!   THE MODE IS NOT THE SAME GATE. `--auto` derives its scope from the
//!   tree and skips cargo entirely when nothing implies a crate. That
//!   is correct and it is NOT a full gate: a green receipt in auto mode
//!   vouches for the lints, not the suites. "A green gate only covers
//!   what it runs" is a whole separate finding, and reading a mode-auto
//!   receipt as a full one is how a ci.yml change gated green and broke
//!   CI on the train.
//!
//! So the verb answers three things a reader needs and a shell pipeline
//! keeps re-deriving: what sha the receipt covers, whether the branch
//! still points there, and how much of the gate actually ran.

use anyhow::Result;

/// What a receipt is worth right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The receipt describes the branch's current head.
    Current,
    /// The branch has moved since it was gated. The receipt is honest
    /// about a tree that no longer exists.
    Stale { gated: String, now: String },
    /// The branch is gone. For an arrived car that is expected — the
    /// train deletes what it merged — so this is not a fault.
    BranchGone,
    /// Could not tell. Never reported as Current.
    Unknown(String),
}

/// Everything the check needs, so the decision is pure and testable.
#[derive(Debug, Clone, Default)]
pub struct ReceiptFacts {
    /// `receipt.head` — the sha the gate actually ran against.
    pub gated_head: Option<String>,
    /// The branch's head on the forge now. `None` = the ref is gone.
    pub branch_head: Option<String>,
    /// Did the ref lookup itself succeed? Distinguishes "the branch is
    /// gone" from "I could not ask", which look identical otherwise and
    /// must not.
    pub remote_readable: bool,
    /// When the lookup could NOT be made: the command, where it ran, and
    /// what git said. "Could not read" without the reason sent its
    /// reader to re-derive it with `git ls-remote` by hand (backlog
    /// 2770eba8, secondary).
    pub remote_said: Option<String>,
    /// `receipt.verdict`.
    pub verdict: Option<String>,
    /// `receipt.mode` — full, auto, or a crate-scoped run.
    pub mode: Option<String>,
}

fn same_commit(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len());
    n >= 7 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// The decision, pure.
pub fn standing(f: &ReceiptFacts) -> Standing {
    let Some(gated) = &f.gated_head else {
        return Standing::Unknown(
            "the car carries no gate receipt — there is nothing vouching for it".into(),
        );
    };
    if !f.remote_readable {
        return Standing::Unknown(format!(
            "could not read the branch from the forge ({}). An unreadable ref is \
             not a missing one, and a receipt cannot be called current against a \
             head nobody looked up",
            f.remote_said
                .as_deref()
                .unwrap_or("the lookup gave no reason")
        ));
    }
    let Some(now) = &f.branch_head else {
        return Standing::BranchGone;
    };
    if same_commit(gated, now) {
        Standing::Current
    } else {
        Standing::Stale {
            gated: gated.clone(),
            now: now.clone(),
        }
    }
}

/// How much of the gate a receipt's mode actually covers, in words.
/// `None` mode is treated as unknown coverage rather than assumed full —
/// the assumption is the defect.
pub fn coverage(mode: Option<&str>) -> &'static str {
    match mode {
        Some("full") => "the full gate: fmt, every lint, clippy, build and the suites",
        Some("auto") => {
            "AUTO — scope derived from the tree. Cargo is skipped entirely when \
             nothing changed implies a crate, so this vouches for the lints and \
             not necessarily for any suite"
        }
        Some(_) => {
            "a SCOPED run — cargo phases limited to the named crates. Lints and \
             fmt ran repo-wide; the suites did not"
        }
        None => "unrecorded — coverage cannot be read off this receipt",
    }
}

pub async fn run(branch: &str, clone: Option<String>, remote: Option<String>) -> Result<()> {
    let clone = clone.unwrap_or_else(|| ".".to_string());
    let remote = remote.unwrap_or_else(|| "origin".to_string());

    // The receipt lives on the car's gate step; this reads it from the
    // packet rather than re-deriving it, because a receipt that is
    // retyped is not a receipt (that is what boss park fixed).
    //
    // SPEAKS TO THE API DIRECTLY. The first version shelled out to
    // ~/bin/boss-api, which made the verb work on exactly one laptop —
    // caught by the no-session-paths lint before it reached a gate.
    // Going through gate.rs's client also inherits the rule that
    // BOSS_JOBS_URL has no default: a wrong instance does not error
    // here, it answers, which is worse.
    let http = crate::sor_client::machine_client()?;
    // Read EVERY open car, not just page one. A limit is not a filter
    // (a-limit-is-not-a-filter): with more than a page of open cars, the
    // one we want can sit past the cap, and reading only page one called
    // an existing car's receipt absent. `crate::sor_client::all_open_cars` pages on
    // `total`.
    let cars = crate::sor_client::all_open_cars(&http).await?;
    let facts = facts_from(&cars, branch, &clone, &remote);

    println!("boss receipt: {branch}");
    println!("  verdict  {}", facts.verdict.as_deref().unwrap_or("none"));
    println!(
        "  gated    {}",
        facts.gated_head.as_deref().unwrap_or("none")
    );
    // "absent" is a reading; a lookup that failed read nothing, and
    // printing "absent" for it is the line 2770eba8 measured.
    println!(
        "  now      {}",
        match (&facts.branch_head, facts.remote_readable) {
            (Some(head), _) => head.as_str(),
            (None, true) => "absent",
            (None, false) => "unread — the lookup failed (see below)",
        }
    );
    println!("  covers   {}", coverage(facts.mode.as_deref()));
    match standing(&facts) {
        Standing::Current => println!("  CURRENT — the receipt describes the branch's head"),
        Standing::Stale { gated, now } => println!(
            "  STALE — gated {} but the branch is now {}. The receipt is honest \n\
             \x20   about a tree that no longer exists; re-gate before boarding.",
            &gated[..gated.len().min(12)],
            &now[..now.len().min(12)]
        ),
        Standing::BranchGone => println!(
            "  BRANCH GONE — expected for an arrived car, since a train deletes \n\
             \x20   what it merged. Not a fault."
        ),
        Standing::Unknown(why) => println!("  UNKNOWN — {why}"),
    }
    Ok(())
}

/// Split out so the JSON walk is testable without a live API. Takes the
/// FULLY-GATHERED open cars (see `crate::sor_client::all_open_cars`), not one page, so
/// a car past the first page is not silently missed.
fn facts_from(cars: &[serde_json::Value], branch: &str, clone: &str, remote: &str) -> ReceiptFacts {
    let ls = look_up_branch(clone, remote, branch);
    let mut out = ReceiptFacts {
        remote_readable: ls.readable,
        branch_head: ls.head,
        remote_said: ls.said,
        ..Default::default()
    };
    if let Some(r) = receipt_for_branch(cars, branch) {
        out.gated_head = r.get("head").and_then(|x| x.as_str()).map(str::to_string);
        out.verdict = r
            .get("verdict")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        out.mode = r.get("mode").and_then(|x| x.as_str()).map(str::to_string);
    }
    out
}

/// What one `git ls-remote` answered about the car's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchLookup {
    /// The command ran and answered — a head, or a clean absence.
    pub readable: bool,
    /// The branch's head, when the remote carries it.
    pub head: Option<String>,
    /// Why the lookup could not be made, when it could not.
    pub said: Option<String>,
}

/// Look the branch up, reading git's EXIT STATUS rather than its output.
///
/// Two defects in one line, both measured 2026-10-17 (backlog 2770eba8,
/// secondary). `sh` answers `None` for a failed command AND for empty
/// output, and `git ls-remote` answers a ref the remote lacks with exit
/// 0 and nothing printed — so a deleted branch read as an unreadable
/// one, and `BranchGone` could never come out of a real lookup. And a
/// lookup that did fail dropped git's own reason: run outside the forge
/// checkout (`--clone` defaults to the working directory, and remote
/// names are per-clone) the verb printed `now absent` for a branch
/// `git ls-remote origin` showed at its head, with nothing saying the
/// clone it asked had no `origin`.
pub(crate) fn look_up_branch(clone: &str, remote: &str, branch: &str) -> BranchLookup {
    let refname = format!("refs/heads/{branch}");
    let command = format!("git -C {clone} ls-remote {remote} {refname}");
    let ran = std::process::Command::new("git")
        .args(["-C", clone, "ls-remote", remote, &refname])
        .output();
    read_lookup(&command, &refname, ran)
}

/// What one `git ls-remote <remote> refs/heads/<branch>` run answered,
/// read off its exit status — the one reading, shared by `boss receipt`
/// and `boss gate`'s launch (backlog ec0ca63d), so the two cannot
/// disagree about what an empty answer means. `command` is the command
/// as the operator would type it, quoted in the reason when it failed.
///
/// THE HEAD IS THE LINE WHOSE REF IS `refname`, not the first line
/// (backlog 78d7a0c1). `git ls-remote` matches its pattern as a glob and
/// against the TAIL of a ref, so `refs/heads/fix/real-*` and
/// `refs/heads/ghost` both exit 0 with a head on a forge holding
/// `fix/real-one` and `other/refs/heads/ghost` (reviewer run db548695).
/// The runner fetches `refs/heads/<branch>` exactly, so only that ref is
/// the branch; a near match is a clean absence.
pub(crate) fn read_lookup(
    command: &str,
    refname: &str,
    ran: std::io::Result<std::process::Output>,
) -> BranchLookup {
    match ran {
        Ok(out) if out.status.success() => BranchLookup {
            readable: true,
            head: String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.split_once('\t'))
                .find(|(_, name)| name.trim() == refname)
                .map(|(head, _)| head.trim().to_string()),
            said: None,
        },
        Ok(out) => BranchLookup {
            readable: false,
            head: None,
            said: Some(format!(
                "`{command}` exited {}: {}",
                out.status
                    .code()
                    .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        },
        Err(e) => BranchLookup {
            readable: false,
            head: None,
            said: Some(format!("`{command}` could not start: {e}")),
        },
    }
}

/// The receipt the car on `branch` carries, chosen from the gathered
/// open cars — pure, so the walk (and that it reads a car past page one,
/// not just the first page) is testable without git or a live API.
///
/// PARSED, NOT SCANNED. The first version of this walked the raw body
/// for `"head":` inside a window after the branch name, and it reported
/// NO RECEIPT for a car that had one — the receipt lives on the gate
/// STEP, past the window, and field order is not a contract. `select_receipt`
/// parses the packet (and prefers `regate_receipt` over the stale gate
/// step); writing an ad-hoc string scan inside the verb built to retire
/// ad-hoc string scans is the joke this comment exists to prevent
/// repeating.
fn receipt_for_branch(cars: &[serde_json::Value], branch: &str) -> Option<serde_json::Value> {
    cars.iter()
        .find(|job| {
            job.pointer("/metadata/branch")
                .and_then(|b| b.as_str())
                .is_some_and(|b| b == branch)
        })
        .and_then(select_receipt)
}

/// The one receipt that describes the head this car would board, chosen
/// from a `ship-a-change` job the same way the boarding logic chooses it
/// (train.rs `receipt_skip_reason`) — split out so the JSON walk is
/// testable without a live API.
///
/// PREFER `regate_receipt` over the gate STEP receipt. A re-rail rebases
/// the car onto a moved main and writes the fresh receipt to
/// `job.metadata.regate_receipt`, leaving the gate step's receipt
/// describing the OLD head. Reading only the step made `boss receipt`
/// call a correctly re-gated car STALE and disagree with the head the
/// conductor would board (7f20fb19 — the mirror of 64cae7e9's repair).
pub(crate) use boss_jobs::car::select_receipt;

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> ReceiptFacts {
        ReceiptFacts {
            gated_head: Some("c8ccb133770f6507422a7d2438261213e9e897ce".into()),
            branch_head: Some("c8ccb133770f6507422a7d2438261213e9e897ce".into()),
            remote_readable: true,
            remote_said: None,
            verdict: Some("green".into()),
            mode: Some("full".into()),
        }
    }

    /// A fresh repository with one commit on `main`, so a lookup has a
    /// real answer to give — the ref, or a clean absence. No network:
    /// the "remote" is the repository's own path.
    fn repo_with_main(name: &str) -> (std::path::PathBuf, String) {
        let dir = boss_testing::scratch::scratch_dir(name);
        let d = dir.to_str().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "t",
            ][..],
        ] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(d)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} in {d}");
        }
        let out = std::process::Command::new("git")
            .args(["-C", d, "rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success(), "git rev-parse HEAD in {d}");
        let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (dir, head)
    }

    /// THE CONTROL: a branch the remote carries is read at its head.
    #[test]
    fn a_branch_the_remote_carries_is_read_at_its_head() {
        let (dir, head) = repo_with_main("receipt-lookup-found");
        let d = dir.to_str().unwrap();
        let found = look_up_branch(d, d, "main");
        assert!(found.readable, "{found:?}");
        assert_eq!(found.head.as_deref(), Some(head.as_str()));
        assert_eq!(found.said, None);
    }

    /// A BRANCH THE REMOTE LACKS IS GONE, NOT UNREADABLE (backlog
    /// 2770eba8, secondary). `git ls-remote` answers a missing ref with
    /// exit 0 and NO output; the old lookup read empty output as a
    /// failed command, so `BranchGone` was unreachable from a real read
    /// and every arrived car printed "could not read the branch".
    #[test]
    fn a_branch_the_remote_lacks_is_gone_not_unreadable() {
        let (dir, _) = repo_with_main("receipt-lookup-gone");
        let d = dir.to_str().unwrap();
        let gone = look_up_branch(d, d, "fix/never-pushed");
        assert!(gone.readable, "a clean absence is a read: {gone:?}");
        assert_eq!(gone.head, None);
        let facts = ReceiptFacts {
            branch_head: gone.head,
            remote_readable: gone.readable,
            remote_said: gone.said,
            ..f()
        };
        assert_eq!(standing(&facts), Standing::BranchGone);
    }

    /// ONLY THE EXACT REF IS THE BRANCH (backlog 78d7a0c1). `git
    /// ls-remote` matches its pattern as a glob and against the TAIL of
    /// a ref, so a remote holding `fix/real-one` and
    /// `other/refs/heads/ghost` answers `fix/real-*` and `ghost` with
    /// exit 0 and a head — and the first line was taken as the branch.
    /// The runner fetches `refs/heads/<name>` exactly and fails, so
    /// neither name has a head; the exact name still does.
    #[test]
    fn only_the_exact_ref_is_the_branch() {
        let (dir, head) = repo_with_main("receipt-lookup-exact");
        let d = dir.to_str().unwrap();
        for branch in ["fix/real-one", "other/refs/heads/ghost"] {
            let ok = std::process::Command::new("git")
                .args(["-C", d, "branch", branch])
                .status()
                .unwrap()
                .success();
            assert!(ok, "git branch {branch}");
        }
        for name in ["fix/real-*", "ghost"] {
            let near = look_up_branch(d, d, name);
            assert!(near.readable, "the remote answered: {near:?}");
            assert_eq!(near.head, None, "{name} is not a branch here");
        }
        let exact = look_up_branch(d, d, "fix/real-one");
        assert_eq!(exact.head.as_deref(), Some(head.as_str()));
    }

    /// A LOOKUP THAT CANNOT RUN SAYS WHAT GIT SAID (backlog 2770eba8,
    /// secondary). Measured 2026-10-17: `boss receipt` run outside the
    /// forge checkout printed `now absent` for a branch `git ls-remote
    /// origin` showed at its head, because `--clone` defaults to the
    /// working directory and the clone there has no such remote — and
    /// the verb dropped git's own sentence saying so. Remote names are
    /// per-clone, so this is the shape: a clone without the remote.
    #[test]
    fn a_lookup_that_cannot_run_says_what_git_said() {
        let (dir, _) = repo_with_main("receipt-lookup-blind");
        let d = dir.to_str().unwrap();
        let blind = look_up_branch(d, "no-such-remote", "main");
        assert!(!blind.readable, "{blind:?}");
        assert_eq!(blind.head, None);
        let said = blind.said.clone().expect("the refusal is carried");
        assert!(said.contains("ls-remote no-such-remote"), "{said}");
        assert!(said.contains(d), "names the clone it ran in: {said}");
        assert!(said.contains("no-such-remote' does not appear"), "{said}");
        let facts = ReceiptFacts {
            branch_head: blind.head,
            remote_readable: blind.readable,
            remote_said: blind.said,
            ..f()
        };
        let Standing::Unknown(why) = standing(&facts) else {
            panic!("an unreadable ref is Unknown");
        };
        assert!(why.contains(&said), "{why}");
    }

    #[test]
    fn a_receipt_matching_the_head_is_current() {
        assert_eq!(standing(&f()), Standing::Current);
    }

    /// THE 09576fab CASE. A rebase or force-push leaves the receipt
    /// green and describing a tree that is gone. Three cars were
    /// reported repaired on that basis while staying unboardable.
    #[test]
    fn a_moved_branch_makes_the_receipt_stale() {
        let moved = ReceiptFacts {
            branch_head: Some("ffffffffffffffffffffffffffffffffffffffff".into()),
            ..f()
        };
        assert!(matches!(standing(&moved), Standing::Stale { .. }));
    }

    /// An arrived train deletes the branches it merged, so a missing
    /// ref is expected rather than a fault — the same trap `boss
    /// merged` encodes.
    #[test]
    fn a_gone_branch_is_expected_not_a_fault() {
        let gone = ReceiptFacts {
            branch_head: None,
            ..f()
        };
        assert_eq!(standing(&gone), Standing::BranchGone);
    }

    /// ...but only when the lookup SUCCEEDED and found nothing. An
    /// unreadable remote is a different fact and must not be reported
    /// as a deleted branch.
    #[test]
    fn an_unreadable_remote_is_not_a_deleted_branch() {
        let blind = ReceiptFacts {
            branch_head: None,
            remote_readable: false,
            ..f()
        };
        assert!(matches!(standing(&blind), Standing::Unknown(_)));
    }

    #[test]
    fn no_receipt_is_unknown_rather_than_current() {
        let none = ReceiptFacts {
            gated_head: None,
            ..f()
        };
        assert!(matches!(standing(&none), Standing::Unknown(_)));
    }

    /// The mode is the half people read past. A green auto receipt is
    /// green about the lints, not the suites, and saying so is the
    /// whole point of surfacing coverage.
    #[test]
    fn coverage_distinguishes_auto_from_full() {
        assert!(coverage(Some("full")).contains("suites"));
        assert!(coverage(Some("auto")).contains("not necessarily"));
        assert!(coverage(None).contains("cannot be read"));
    }

    /// Abbreviated and full spellings of one sha are the same commit —
    /// the bug that `boss running` shipped with until a live run caught
    /// it, so it is pinned here rather than rediscovered.
    #[test]
    fn an_abbreviated_head_still_matches() {
        let short = ReceiptFacts {
            branch_head: Some("c8ccb133770f".into()),
            ..f()
        };
        assert_eq!(standing(&short), Standing::Current);
    }

    /// A car with only a gate-step receipt reads it — the common case.
    #[test]
    fn the_gate_step_receipt_is_used_when_there_is_no_regate() {
        let job = serde_json::json!({
            "steps": [{ "title": "Green, and observed working",
                        "metadata": { "receipt": "{\"head\":\"oldhead\",\"verdict\":\"green\",\"mode\":\"full\"}" } }]
        });
        let r = select_receipt(&job).expect("gate step receipt");
        assert_eq!(r.get("head").and_then(|h| h.as_str()), Some("oldhead"));
    }

    /// BOTH RECEIPT SHAPES ANSWER THE VERB'S THREE QUESTIONS.
    ///
    /// The gate runner reduced `infra/gate.sh`'s account of a run to
    /// `{verdict, head, mode, fails}` before reporting it until
    /// 2026-09-09, so every landed car carries that four-field shape and
    /// nothing will rewrite them; new ones carry the whole receipt. This
    /// verb reads `head`, `verdict` and `mode` and must find all three in
    /// either — the extra keys ignored, the missing `fails` unmissed.
    #[test]
    fn a_wide_receipt_and_a_four_field_one_both_answer_the_verb() {
        let old = serde_json::json!({
            "steps": [{ "title": "Green, and observed working", "metadata": { "receipt":
                "{\"verdict\":\"green\",\"head\":\"oldhead\",\"mode\":\"full\",\"fails\":[]}" } }]
        });
        let wide = serde_json::json!({
            "steps": [{ "title": "Green, and observed working", "metadata": { "receipt":
                "{\"verdict\":\"green\",\"mode\":\"auto\",\"scope\":\"\",\"head\":\"newhead\",\
                  \"dirty\":false,\"host\":\"gate-runner-abc\",\"ci\":true,\"free_gb\":91,\
                  \"unverifiable\":[],\"report\":{\"attempts\":1,\"waited_s\":0,\"sor_unreachable\":false},\
                  \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\",\"seconds\":3}]}" } }]
        });
        for (job, head, mode) in [(old, "oldhead", "full"), (wide, "newhead", "auto")] {
            let r = select_receipt(&job).expect("a receipt in either shape");
            assert_eq!(r.get("head").and_then(|h| h.as_str()), Some(head));
            assert_eq!(r.get("verdict").and_then(|v| v.as_str()), Some("green"));
            assert_eq!(r.get("mode").and_then(|m| m.as_str()), Some(mode));
            assert!(
                !coverage(r.get("mode").and_then(|m| m.as_str())).contains("cannot be read"),
                "coverage must still be readable off either shape"
            );
        }
    }

    /// THE 7f20fb19 CASE. After a re-rail the gate step still holds the
    /// OLD head's receipt while `regate_receipt` holds the fresh one; the
    /// boarding logic prefers regate, so `boss receipt` must too or it
    /// calls a correctly re-gated car STALE. Preferred as an OBJECT...
    #[test]
    fn a_regate_receipt_object_is_preferred_over_the_stale_gate_step() {
        let job = serde_json::json!({
            "metadata": { "regate_receipt": { "head": "freshhead", "verdict": "green", "mode": "full" } },
            "steps": [{ "title": "Green, and observed working",
                        "metadata": { "receipt": "{\"head\":\"oldhead\",\"verdict\":\"green\",\"mode\":\"full\"}" } }]
        });
        let r = select_receipt(&job).expect("regate receipt");
        assert_eq!(r.get("head").and_then(|h| h.as_str()), Some("freshhead"));
    }

    /// ...and as a JSON STRING, the shape a re-rail copies verbatim.
    #[test]
    fn a_regate_receipt_string_is_parsed_and_preferred() {
        let job = serde_json::json!({
            "metadata": { "regate_receipt": "{\"head\":\"freshhead\",\"verdict\":\"green\",\"mode\":\"auto\"}" },
            "steps": [{ "title": "Green, and observed working",
                        "metadata": { "receipt": "{\"head\":\"oldhead\",\"verdict\":\"green\",\"mode\":\"full\"}" } }]
        });
        let r = select_receipt(&job).expect("regate receipt");
        assert_eq!(r.get("head").and_then(|h| h.as_str()), Some("freshhead"));
        assert_eq!(r.get("mode").and_then(|m| m.as_str()), Some("auto"));
    }

    /// A null regate_receipt is not a receipt — fall back to the step.
    #[test]
    fn a_null_regate_receipt_falls_back_to_the_gate_step() {
        let job = serde_json::json!({
            "metadata": { "regate_receipt": null },
            "steps": [{ "title": "Green, and observed working",
                        "metadata": { "receipt": "{\"head\":\"oldhead\",\"verdict\":\"green\",\"mode\":\"full\"}" } }]
        });
        let r = select_receipt(&job).expect("gate step receipt");
        assert_eq!(r.get("head").and_then(|h| h.as_str()), Some("oldhead"));
    }

    /// A LIMIT IS NOT A FILTER (memory: a-limit-is-not-a-filter). Once
    /// open cars fill more than a page, the car sorts to the tail of
    /// `opened_on DESC`; the old bare `limit=100` read left it off page
    /// one and `boss receipt` reported an existing car as carrying no
    /// receipt. The read now pages on `total` (via
    /// `train::list_all_pages`, whose page-two behaviour is pinned
    /// there), so the packet walk must find a car gathered from page two.
    #[tokio::test]
    async fn a_receipt_on_page_two_is_found_not_left_off_page_one() {
        use serde_json::json;
        let mut all: Vec<serde_json::Value> = (0..crate::train::PAGE_LIMIT + 40)
            .map(|i| json!({ "metadata": { "branch": format!("decoy-{i}") } }))
            .collect();
        all.push(json!({
            "metadata": { "branch": "fix/tail-car" },
            "steps": [{
                "title": "Green, and observed working",
                "metadata": { "receipt": "{\"head\":\"deadbeefcafe\",\"verdict\":\"green\",\"mode\":\"full\"}" }
            }],
        }));
        let all_ref = &all;
        let gathered = crate::train::list_all_pages(|offset| async move {
            let page: Vec<serde_json::Value> = all_ref
                .iter()
                .skip(offset)
                .take(crate::train::PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({ "data": page, "total": all_ref.len() })))
        })
        .await
        .unwrap();
        let r = receipt_for_branch(&gathered, "fix/tail-car")
            .expect("the car on page two must be found, not left off page one");
        assert_eq!(r.get("head").and_then(|h| h.as_str()), Some("deadbeefcafe"));
    }
}

/// Top-level registration for `boss receipt` — see `merged::Cmd` for
/// why the variant lives in its verb's module (84f9fbc0).
#[derive(clap::Subcommand)]
pub enum Cmd {
    /// What does this car's gate receipt actually vouch for?
    ///
    /// A receipt names the sha it gated and the MODE it ran in. A
    /// rebase leaves it green about a tree that is gone, and an auto
    /// receipt is green about the lints rather than the suites.
    Receipt {
        /// Car branch to inspect.
        branch: String,
        /// Clone to read refs from. Defaults to the working directory.
        #[arg(long)]
        clone: Option<String>,
        /// Remote holding the car branches.
        #[arg(long)]
        remote: Option<String>,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Receipt {
            branch,
            clone,
            remote,
        } => run(&branch, clone, remote).await,
    }
}
