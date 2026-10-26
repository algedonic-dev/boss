//! AN EMPTY CONSIST DEPARTS NOTHING (backlog 81408a81).
//!
//! WHY. On 2026-10-18 trains b0e4ba33 (PR #1248) and 13cfa6ca each
//! carried ONE car whose content was already on main under another
//! branch — a landed twin. The assembled train head was TREESAME with
//! origin/main (b0e4ba33: both trees 13190d8a), and the train's own
//! `delivery_plan` recorded `changes []` when it opened. The conductor
//! still pushed the branch, opened the PR and launched the train gate;
//! `infra/gate.sh --auto` refused ("found no change at all against
//! origin/main"), the runner wrote no receipt, the receipt read
//! `unreadable`, the ci step recorded failing, and a red alert reading
//! "check names unavailable" was filed while forge CI was green on every
//! check. Each train held the one track until a hand cancelled it
//! (b0e4ba33 at 23:09Z; 13cfa6ca sat 86 minutes).
//!
//! An empty consist is not a code failure and not the car's fault, so it
//! departs nothing: no push, no packet, no PR, no gate, no alert. The
//! tree comparison is the same fact as `delivery_plan`'s `changes []` —
//! that list is `git diff --raw <base> <head>`, empty exactly when the
//! two trees are one — read before anything is spent rather than after.
//!
//! WHERE THE CARS GO. Each boarded car's work is already on main, which
//! is what the `landed-twin` terminal says, and `boss car retire
//! --carried-by` is its one writer: this module proves each car by that
//! verb's own whole-tree arm (`car_retire::judge` / `verdict` — merging
//! the car's head into main writes main's own tree, a proof the machine
//! can make without an operator's acceptance) and closes it through that
//! verb's own writes (`retire_writes`, then `apply_via` through the
//! conductor's door). A car it cannot prove or cannot close — its work
//! cancelled by another car's, a protocol version without the terminal —
//! is LEFT on the dock with the reason on its `skip_reason`, and the
//! board's refusal names it as needing a human (`refusal_persists`).

use super::*;
use crate::car_retire::{self as retire, Door};
use boss_jobs::car_retire::{LANDED_TWIN_SLUG, Retirement, retire_writes};

/// Does the assembled consist add nothing to `main`? The trees compared,
/// not the commits: assembly merges `--no-ff`, so the head is a new
/// commit even when it changes nothing. An unreadable tree is an error,
/// and the caller departs as before — a read that failed is not an
/// empty consist.
pub(crate) fn consist_adds_nothing(clone: &Path, main: &str, head: &str) -> Result<bool, String> {
    let tree = |rev: &str| retire::git(clone, &["rev-parse", &format!("{rev}^{{tree}}")]);
    Ok(tree(main)? == tree(head)?)
}

/// The machine's proof that `head`'s work is on `main`, in the evidence
/// sentence `boss car retire` records — or why it is not. No operator
/// acceptance is assumed: a replay or a net diff is refused here, and the
/// car stays for a person.
pub(crate) fn twin_proof(repo: &Path, main: &str, head: &str) -> Result<String, String> {
    let mine = retire::read_commits(repo, head, main)?;
    if mine.is_empty() {
        return Ok(format!("its head {head} is an ancestor of main {main}"));
    }
    let whole = retire::whole_tree(repo, main, head)?;
    let judged = retire::judge(&mine, &[], Some(&whole));
    retire::verdict(&judged, false, false)?;
    Ok(retire::evidence(
        &format!("main {main}"),
        &judged,
        None,
        None,
    ))
}

/// What became of one boarded car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fate {
    /// Closed through `landed-twin`, confirmed by reading it back.
    Retired { car: String, branch: String },
    /// Left on the dock; `reason` is on its `skip_reason`.
    Left {
        car: String,
        branch: String,
        reason: String,
    },
}

impl Fate {
    fn named(car: &str, branch: &str) -> String {
        format!("{} ({branch})", id8(car))
    }

    /// The journal line for this car.
    pub(crate) fn line(&self) -> String {
        match self {
            Fate::Retired { car, branch } => format!(
                "empty consist: car {} retired through `{LANDED_TWIN_SLUG}` — its work is on main",
                Self::named(car, branch)
            ),
            Fate::Left {
                car,
                branch,
                reason,
            } => format!(
                "empty consist: car {} LEFT on the dock — {reason}",
                Self::named(car, branch)
            ),
        }
    }
}

/// PURE: the board's refusal for an empty consist on `main`.
pub(crate) fn no_departure(main: &str, fates: &[Fate]) -> NoDeparture {
    let join = |want_retired: bool| {
        fates
            .iter()
            .filter_map(|f| match (f, want_retired) {
                (Fate::Retired { car, branch }, true) => Some(Fate::named(car, branch)),
                (
                    Fate::Left {
                        car,
                        branch,
                        reason,
                    },
                    false,
                ) => Some(format!(
                    "{}: {}",
                    Fate::named(car, branch),
                    reason.lines().next().unwrap_or_default()
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    NoDeparture::EmptyConsist {
        main: main[..12.min(main.len())].to_string(),
        retired: join(true),
        left: join(false),
    }
}

/// Retire every boarded car of an empty consist on `main` — `(car id,
/// branch, boarded head)` each — through `boss car retire`'s writer, or
/// leave it on the dock with its reason. Never fatal: one car that
/// cannot be closed costs that car, never the others.
pub(crate) async fn retire_twins(
    door: &dyn Door,
    repo: &Path,
    main: &str,
    boarded: &[(String, String, String)],
) -> Vec<Fate> {
    let mut fates = Vec::with_capacity(boarded.len());
    for (car, branch, head) in boarded {
        let fate = match retire_one(door, repo, main, car, head).await {
            Ok(()) => Fate::Retired {
                car: car.clone(),
                branch: branch.clone(),
            },
            Err(reason) => {
                let reason = format!(
                    "the consist it boarded adds nothing to main {}, but the conductor could \
                     not retire it as a landed twin: {reason}",
                    &main[..12.min(main.len())]
                );
                if let Err(e) = door
                    .send(
                        Method::PATCH,
                        &format!("/api/jobs/{car}/metadata"),
                        Some(json!({"skip_reason": reason})),
                    )
                    .await
                {
                    log(format!(
                        "empty consist: car {} — its skip_reason could not be written ({e:#})",
                        id8(car)
                    ));
                }
                Fate::Left {
                    car: car.clone(),
                    branch: branch.clone(),
                    reason,
                }
            }
        };
        log(fate.line());
        fates.push(fate);
    }
    fates
}

async fn retire_one(
    door: &dyn Door,
    repo: &Path,
    main: &str,
    car: &str,
    head: &str,
) -> Result<(), String> {
    let proof = twin_proof(repo, main, head)?;
    let current = retire::read_via(door, car)
        .await
        .map_err(|e| format!("the car could not be read: {e:#}"))?;
    let retirement = Retirement::CarriedBy {
        by: main.to_string(),
        evidence: format!(
            "retired by the train conductor: the consist it boarded was treesame with main \
             {main} (backlog 81408a81). {proof}"
        ),
    };
    let writes = retire_writes(&current, &retirement)?;
    retire::apply_via(
        door,
        car,
        &current,
        &writes,
        LANDED_TWIN_SLUG,
        "boss train (empty consist)",
    )
    .await
    .map_err(|e| format!("the landed-twin writes did not close it: {e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::car_unland::tests::git_in;

    fn admin() -> Option<crate::identity::Caller> {
        Some(crate::identity::Caller {
            id: "emp-bootstrap-admin".into(),
            source: crate::identity::Source::Env,
        })
    }

    /// A parked car on the real router: scope, build and gate completed,
    /// review open — the shape a car has when it boards.
    async fn parked(door: &dyn Door, branch: &str) -> String {
        let made = door
            .send(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "ship-a-change",
                    "subject": {"subject_kind": "custom", "id": branch},
                    "title": "A full gate binds its dropped empty mode",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "tags": [],
                    "metadata": {"branch": branch, "summary": "binds the mode"},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        for (slug, md) in [
            (
                "scope",
                json!({"summary": "binds the mode", "excludes": "the UI"}),
            ),
            ("build", json!({"test": "1 test"})),
            ("gate", json!({"gates": "auto", "verified": "green"})),
        ] {
            let car = retire::read_via(door, &id).await.unwrap();
            let sid = car::find_step(&car, slug, "").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            door.send(
                Method::PATCH,
                &format!("/api/jobs/{id}/steps/{sid}/metadata"),
                Some(md),
            )
            .await
            .unwrap();
            door.send(
                Method::PUT,
                &format!("/api/jobs/{id}/steps/{sid}"),
                Some(json!({"status": "completed"})),
            )
            .await
            .unwrap();
        }
        id
    }

    /// The b0e4ba33 shape, in a real repository: a car cut from main adds
    /// a test; a DIFFERENT branch carrying the same lines lands on main
    /// (squashed, as a train lands it); the car then boards alone. The
    /// assembled consist's tree is main's own. A second car whose line
    /// main lacks is the control: its consist is not empty, and it cannot
    /// be proven a twin.
    fn twin_repo(name: &str) -> (std::path::PathBuf, String, String, String) {
        let root = boss_testing::scratch::scratch_dir(name);
        let w = |rel: &str, body: &str| boss_testing::scratch::write_file(&root.join(rel), body);
        git_in(&root, &["init", "-q", "-b", "main"]);
        w("reservation.rs", "fn reserve() {}\n");
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "base"]);

        git_in(&root, &["checkout", "-q", "-b", "twin"]);
        w(
            "reservation.rs",
            "fn reserve() {}\nfn a_full_gate_binds_the_job_whose_empty_mode_the_server_dropped() {}\n",
        );
        git_in(
            &root,
            &["commit", "-qam", "a full gate binds its dropped empty mode"],
        );
        let twin = git_in(&root, &["rev-parse", "HEAD"]);

        // The landing: the same lines under another branch, squashed.
        git_in(&root, &["checkout", "-q", "-b", "landed", "main"]);
        w(
            "reservation.rs",
            "fn reserve() {}\nfn a_full_gate_binds_the_job_whose_empty_mode_the_server_dropped() {}\n",
        );
        git_in(
            &root,
            &["commit", "-qam", "the same fix, under its first branch"],
        );
        git_in(&root, &["checkout", "-q", "main"]);
        git_in(&root, &["merge", "-q", "--squash", "landed"]);
        git_in(&root, &["commit", "-qm", "train: #1243"]);
        let main = git_in(&root, &["rev-parse", "HEAD"]);

        git_in(&root, &["checkout", "-q", "-b", "orphan", "main"]);
        w("orphan.rs", "fn only_here() {}\n");
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "work main lacks"]);
        let orphan = git_in(&root, &["rev-parse", "HEAD"]);
        git_in(&root, &["checkout", "-q", "main"]);
        (root, main, twin, orphan)
    }

    #[test]
    fn a_consist_of_a_landed_twin_is_main_s_own_tree_and_one_with_work_is_not() {
        let (root, main, twin, orphan) = twin_repo("empty-consist-tree");
        git_in(&root, &["checkout", "-q", "-B", "train/t", &main]);
        git_in(
            &root,
            &["merge", "-q", "--no-ff", "-m", "train: merge twin", &twin],
        );
        let head = git_in(&root, &["rev-parse", "HEAD"]);
        assert_ne!(head, main, "assembly made a new commit");
        assert_eq!(consist_adds_nothing(&root, &main, &head), Ok(true));
        let proof = twin_proof(&root, &main, &twin).expect("the twin is proven");
        assert!(proof.contains("main's own tree"), "{proof}");

        git_in(
            &root,
            &[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "train: merge orphan",
                &orphan,
            ],
        );
        let head = git_in(&root, &["rev-parse", "HEAD"]);
        assert_eq!(consist_adds_nothing(&root, &main, &head), Ok(false));
        let e = twin_proof(&root, &main, &orphan).unwrap_err();
        assert!(e.contains("orphan.rs"), "{e}");

        // A ref that is not there is an error, never "empty".
        assert!(
            consist_adds_nothing(&root, &main, "0000000000000000000000000000000000000000").is_err()
        );
    }

    /// End to end against the REAL jobs router and the real platform
    /// bundle: the twin closes through `landed-twin` with main named as
    /// its carrier and the whole-tree proof as its evidence; the car whose
    /// work main lacks is left OPEN on the dock, its `skip_reason` naming
    /// the file — and the board's refusal pages for it alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_consist_retires_each_proven_twin_and_leaves_the_rest_named() {
        let (root, main, twin, orphan) = twin_repo("empty-consist-retire");
        let wire =
            crate::steps::Wire::at(crate::car_unland::tests::serve().await, admin()).unwrap();
        let twin_car = parked(&wire, "fix/a-full-gate-binds-its-dropped-empty-mode-2").await;
        let orphan_car = parked(&wire, "fix/work-main-lacks").await;

        let fates = retire_twins(
            &wire,
            &root,
            &main,
            &[
                (
                    twin_car.clone(),
                    "fix/a-full-gate-binds-its-dropped-empty-mode-2".into(),
                    twin.clone(),
                ),
                (
                    orphan_car.clone(),
                    "fix/work-main-lacks".into(),
                    orphan.clone(),
                ),
            ],
        )
        .await;
        assert!(
            matches!(&fates[0], Fate::Retired { car, .. } if *car == twin_car),
            "{fates:?}"
        );
        assert!(
            matches!(&fates[1], Fate::Left { car, .. } if *car == orphan_car),
            "{fates:?}"
        );

        let closed = retire::read_via(&wire, &twin_car).await.unwrap();
        assert_eq!(closed["status"], "closed", "{closed:#}");
        assert_eq!(closed["metadata"]["outcome"], LANDED_TWIN_SLUG);
        assert_eq!(closed["metadata"]["carried_by"], main.as_str());
        let step = car::find_step(&closed, LANDED_TWIN_SLUG, "").unwrap();
        let evidence = step["metadata"]["evidence"].as_str().unwrap();
        assert!(evidence.contains("treesame with main"), "{evidence}");
        assert!(evidence.contains("main's own tree"), "{evidence}");

        let left = retire::read_via(&wire, &orphan_car).await.unwrap();
        assert_eq!(left["status"], "open", "{left:#}");
        let skip = left["metadata"]["skip_reason"].as_str().unwrap();
        assert!(
            skip.contains("could not retire it as a landed twin"),
            "{skip}"
        );
        assert!(skip.contains("orphan.rs"), "{skip}");

        let refusal = no_departure(&main, &fates);
        assert!(boss_jobs::board_decision::refusal_persists(&refusal));
        let line = no_departure_line(&refusal);
        assert!(
            line.contains(&format!("retired as landed twins: {}", id8(&twin_car))),
            "{line}"
        );
        assert!(
            line.contains(&format!("A HUMAN IS NEEDED for {}", id8(&orphan_car))),
            "{line}"
        );

        // Every car retired: the window clears itself, nobody is paged.
        let all = no_departure(&main, &fates[..1]);
        assert!(!boss_jobs::board_decision::refusal_persists(&all));
    }
}
