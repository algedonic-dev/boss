//! `boss retire` (backlog 7506fced) — driven against the stub the
//! release and completion tests use, which judges `metadata=` by the
//! server's own parser and pages as the server does but never applies
//! `status=`, so every row the verb acts on is re-read off the row
//! itself.

use super::*;
use crate::steps::retire::{
    Answer, answer_of, car_refusal, landed_commit, retirable, retire, retire_writes, run_refusal,
};

/// The landing commit of UI step 1 as the nine items name it — full,
/// because the verb records only a sha it resolved.
const LANDED: &str = "1f3a9725e0000000000000000000000000000000";
const OPEN_CAR: &str = "c4a7ba0b-0000-4000-8000-000000000001";

/// backlog-item v22's `build`, as the live board holds one of the nine:
/// the row's `disposition` enum and completion contract on top of the
/// claimed shape `claimed` builds.
fn build(status: &str, edge: Option<&str>) -> Value {
    let mut step = claimed(status, edge);
    step["fields"] = json!([
        { "name": "disposition", "field_type": "stale|duplicate|decline|delivered", "required": false }
    ]);
    step["metadata"]["completion_contract"] = json!("item-awaits-its-car-v1");
    step
}

fn item(step: Value) -> Value {
    let mut p = held_packet(step);
    p["steps"].as_array_mut().unwrap().push(
        json!({ "id": "s-stale", "spec_slug": "stale", "kind": "outcome",
                      "status": "pending", "metadata": {} }),
    );
    p
}

fn run(id: &str, status: &str, packet: &str) -> Value {
    json!({ "id": id, "kind": "agent-run", "status": status, "title": "a run",
            "metadata": { "packet": packet, "step": "build" }, "steps": [] })
}

fn car(status: &str, md: Value) -> Value {
    json!({ "id": OPEN_CAR, "kind": "ship-a-change", "status": status,
            "title": "a car", "metadata": md, "steps": [] })
}

fn stored_build(s: &Stub) -> Value {
    s.packets.lock().unwrap()[0]["steps"][1].clone()
}

/// REVIEW 956f882f FINDING 1, Codex's regression ported: a new executor
/// claims the step and opens a run on the item AFTER every retirement
/// read and BEFORE its first write. The retirement must refuse, and the
/// new holder, run edge and resource claim must stand — no `released`
/// record naming the old holder, no disposition or evidence written.
#[tokio::test]
async fn review_retire_preserves_a_run_claimed_after_its_read() {
    let s = stub_with_protocol(
        vec![
            item(build("active", Some(HELD_RUN))),
            run(HELD_RUN, "closed", HELD_JOB),
        ],
        json!({"review_retire_race": true}),
    )
    .await;
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    let result = retire(&wire, "bd93d2be", "stale", "old page removed", None, None).await;
    let step = stored_build(&s);
    assert!(
        result.is_err(),
        "a retirement judged on a row that moved must refuse"
    );
    assert_eq!(step["status"], "active");
    assert_eq!(step["assignee_id"], "agent-new-worker");
    assert_eq!(
        step["metadata"][boss_jobs::agent_runs::EDGE_KEY],
        RACE_RUN,
        "a refused retirement must not erase the newly admitted run edge: {step}"
    );
    assert!(
        step["metadata"].get(RELEASED_KEY).is_none(),
        "no release occurred: {step}"
    );
    assert!(
        step["metadata"].get("disposition").is_none() && step["metadata"].get("evidence").is_none(),
        "nothing of the refused retirement is on the record: {step}"
    );
    assert_eq!(
        step["metadata"][boss_jobs::flow_resources::TOKEN_KEY]["id"],
        "bbbbbbbb-0000-4000-8000-000000000099"
    );
}

/// REVIEW 956f882f FINDING 2, Codex's regression ported: a duplicate of
/// ITSELF names no original to carry the claim, by abbreviated or full id.
#[tokio::test]
async fn review_retire_refuses_a_duplicate_of_itself() {
    for of in ["bd93d2be", HELD_JOB] {
        let s = stub(vec![item(build("active", None))]).await;
        let wire = Wire::at(s.base.clone(), named()).unwrap();
        let result = retire(&wire, "bd93d2be", "duplicate", "same claim", Some(of), None).await;
        let err = result.expect_err("the original of a duplicate must be a different packet");
        assert!(err.to_string().contains("itself"), "{of}: {err:#}");
        assert!(s.puts.lock().unwrap().is_empty(), "{of}: nothing written");
    }
}

/// THE DEFECT, AS THE GENERIC DOOR MEETS IT: `build` declares no
/// `evidence`, so `boss step complete --field-file evidence=...` is
/// refused as an undeclared name — the only supported completion of the
/// step could withdraw the item with nothing behind it.
#[test]
fn the_generic_door_cannot_record_evidence_on_a_build() {
    let step = build("active", None);
    let refused = field_writes_against(
        &step,
        &[Given {
            name: "evidence".into(),
            value: "measured".into(),
        }],
        Vec::new(),
    )
    .expect_err("build declares no evidence");
    assert!(refused.contains("evidence"), "{refused}");
}

/// THE WHOLE VERB: ONE conditional completion carrying the disposition,
/// the evidence, the landing commit and the release record with the run
/// edge cleared, expecting the status, holder and version it judged —
/// and no other write; the step read back so.
#[tokio::test]
async fn an_item_at_build_retires_with_its_evidence_and_its_claim_released() {
    let s = stub(vec![
        item(build("active", Some(HELD_RUN))),
        run(HELD_RUN, "closed", HELD_JOB),
    ])
    .await;
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    retire(
        &wire,
        "bd93d2be",
        "stale",
        "  the page this item names was deleted by UI step 1  ",
        None,
        Some(LANDED),
    )
    .await
    .expect("retired");

    let puts = s.puts.lock().unwrap().clone();
    assert_eq!(
        puts.len(),
        1,
        "one guarded write and nothing else: {puts:?}"
    );
    assert!(puts[0].0.ends_with("/complete-if"), "{puts:?}");
    let sent = &puts[0].1;
    assert_eq!(sent["expected"]["status"], "active");
    assert_eq!(sent["expected"]["assignee_id"], "agent-claude");
    assert!(
        sent["expected"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    );
    let merged = &sent["evidence"];
    assert_eq!(merged["disposition"], "stale");
    assert_eq!(
        merged["evidence"],
        "the page this item names was deleted by UI step 1"
    );
    assert_eq!(merged["landed_in"], LANDED);
    assert_eq!(merged[boss_jobs::agent_runs::EDGE_KEY], Value::Null);

    let step = stored_build(&s);
    assert_eq!(step["status"], "completed");
    assert_eq!(
        step["assignee_id"], "agent-claude",
        "the door completes without reassigning; `released` says it was taken: {step}"
    );
    assert!(
        step["metadata"]
            .get(boss_jobs::agent_runs::EDGE_KEY)
            .is_none(),
        "no run is left named, so no run is delivered by the completion: {step}"
    );
    let released = &step["metadata"][RELEASED_KEY];
    assert_eq!(released["from"], "agent-claude");
    assert_eq!(released["from_run"], HELD_RUN);
    assert!(
        released["why"]
            .as_str()
            .unwrap()
            .contains("retired `stale`"),
        "{released}"
    );
    assert_eq!(
        step["metadata"]["completion_contract"], "item-awaits-its-car-v1",
        "a merge, not a replace: the step keeps what it carried"
    );
}

/// A READY step nobody holds and no run names takes no release record —
/// there is nothing to hand back, and writing one would record a
/// takeover of work nobody held.
#[tokio::test]
async fn a_ready_unheld_build_retires_with_no_release_record() {
    let mut step = build("ready", None);
    step["assignee_id"] = Value::Null;
    let s = stub(vec![item(step)]).await;
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    retire(&wire, "bd93d2be", "decline", "not worth a car", None, None)
        .await
        .expect("retired");
    let sent = s.puts.lock().unwrap()[0].1.clone();
    assert_eq!(sent["expected"]["assignee_id"], Value::Null, "{sent}");
    let merged = &sent["evidence"];
    assert!(merged.get(RELEASED_KEY).is_none(), "{merged}");
    assert!(merged.get("landed_in").is_none(), "{merged}");
    assert_eq!(stored_build(&s)["status"], "completed");
}

/// AN ANSWER THAT SAYS NOTHING IS ASKED AGAIN AS ITSELF (review 956f882f
/// finding 1's unknown result): one retry, the SAME operation id, and —
/// still unanswered — an UNKNOWN naming that operation, never a success
/// and never a second, different write.
#[tokio::test]
async fn an_unanswered_completion_is_retried_once_as_the_same_operation() {
    let s = stub(vec![item(build("active", None))]).await;
    s.drop_puts.store(true, std::sync::atomic::Ordering::SeqCst);
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    let err = retire(&wire, "bd93d2be", "stale", "gone", None, None)
        .await
        .expect_err("no answer is no success");
    let puts = s.puts.lock().unwrap().clone();
    assert_eq!(puts.len(), 2, "asked twice: {puts:?}");
    assert_eq!(puts[0].1["operation_id"], puts[1].1["operation_id"]);
    let op = puts[0].1["operation_id"].as_str().unwrap().to_string();
    assert!(err.to_string().contains("UNKNOWN"), "{err:#}");
    assert!(err.to_string().contains(&op), "{err:#}");
    assert_eq!(stored_build(&s)["status"], "active", "nothing landed");
}

#[test]
fn the_doors_answer_is_read_off_its_outcome() {
    let done = answer_of(&Ok(Some(
        json!({"outcome": "replayed", "receipt": {"event_id": "e1"}}),
    )));
    assert_eq!(done, Answer::Done(json!({"event_id": "e1"})));
    let moved = answer_of(&Err(anyhow!(
        "jobs POST /api/jobs/x/steps/y/complete-if -> 409 Conflict: {{\"outcome\":\"precondition_failed\",\"step_id\":\"y\"}}"
    )));
    assert!(matches!(moved, Answer::Refused(w) if w.contains("moved after it was judged")));
    assert!(matches!(
        answer_of(&Err(anyhow!("jobs POST /p -> 403 Forbidden: nope"))),
        Answer::Refused(_)
    ));
    assert!(matches!(
        answer_of(&Err(anyhow!("jobs POST /p -> 502 Bad Gateway: proxy"))),
        Answer::Unknown(_)
    ));
    assert!(matches!(
        answer_of(&Err(anyhow!("connection reset"))),
        Answer::Unknown(_)
    ));
    assert!(matches!(answer_of(&Ok(None)), Answer::Unknown(_)));
}

/// A duplicate names its original by the FULL id it resolved to, never
/// the eight characters typed.
#[tokio::test]
async fn a_duplicate_records_the_original_it_resolved() {
    let original = json!({ "id": "0a1b2c3d-0000-4000-8000-00000000000e", "kind": "backlog-item",
                           "status": "open", "title": "the older item", "metadata": {}, "steps": [] });
    let s = stub(vec![item(build("active", None)), original]).await;
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    retire(
        &wire,
        "bd93d2be",
        "duplicate",
        "same claim",
        Some("0a1b2c3d"),
        None,
    )
    .await
    .expect("retired");
    assert_eq!(
        stored_build(&s)["metadata"]["duplicate_of"],
        "0a1b2c3d-0000-4000-8000-00000000000e"
    );
}

/// WORK STILL RUNNING REFUSES BEFORE ANY WRITE: an open run naming the
/// item, a run on the step's edge that is still open (though it names
/// the item nowhere the list could find), and an edge run that cannot
/// be read.
#[tokio::test]
async fn work_still_running_on_the_item_refuses_before_any_write() {
    let other = "9e9e9e9e-0000-4000-8000-000000000009";
    for (packets, says) in [
        (
            vec![item(build("active", None)), run(HELD_RUN, "open", HELD_JOB)],
            "open agent-run",
        ),
        (
            vec![
                item(build("active", Some(HELD_RUN))),
                run(HELD_RUN, "open", other),
            ],
            "whose status is open",
        ),
        (
            vec![item(build("active", Some(HELD_RUN)))],
            "could not be read",
        ),
    ] {
        let s = stub(packets).await;
        let wire = Wire::at(s.base.clone(), named()).unwrap();
        let err = retire(&wire, "bd93d2be", "stale", "gone", None, None)
            .await
            .expect_err("refused");
        assert!(err.to_string().contains(says), "{err:#}");
        assert!(s.puts.lock().unwrap().is_empty(), "nothing was written");
    }
}

/// AN OPEN CAR THAT ANSWERS THE ITEM REFUSES — by `backlog_item` or by
/// `also_answers`; a closed car does not, and neither does an open car
/// whose `also_answers` lists only other items. The stub judges
/// `metadata=` by the server's own parser, so a read the live API
/// refuses fails here too (backlog 559b37a4: the array document
/// `{"also_answers":[id]}` was a 400 on every retirement). The refusal
/// asserted is the verb's own sentence, never a read's context line,
/// which also says "open cars".
#[tokio::test]
async fn an_open_car_answering_the_item_refuses_and_a_closed_one_does_not() {
    for md in [
        json!({ "backlog_item": HELD_JOB, "branch": "feat/x" }),
        json!({ "also_answers": [HELD_JOB], "branch": "feat/y" }),
        json!({ "also_answers": ["0a1b2c3d-0000-4000-8000-00000000000e", HELD_JOB],
                "branch": "feat/z" }),
    ] {
        let s = stub(vec![item(build("active", None)), car("open", md)]).await;
        let wire = Wire::at(s.base.clone(), named()).unwrap();
        let err = retire(&wire, "bd93d2be", "stale", "gone", None, None)
            .await
            .expect_err("refused");
        assert!(err.to_string().contains("open car(s) answer"), "{err:#}");
        assert!(s.puts.lock().unwrap().is_empty());
    }
    for (status, md) in [
        (
            "closed",
            json!({ "backlog_item": HELD_JOB, "also_answers": [HELD_JOB] }),
        ),
        (
            "open",
            json!({ "also_answers": ["0a1b2c3d-0000-4000-8000-00000000000e"] }),
        ),
    ] {
        let s = stub(vec![item(build("active", None)), car(status, md.clone())]).await;
        let wire = Wire::at(s.base.clone(), named()).unwrap();
        retire(&wire, "bd93d2be", "stale", "gone", None, None)
            .await
            .unwrap_or_else(|e| panic!("a {status} car {md} holds nothing: {e:#}"));
    }
}

/// `n` unrelated open cars, ids under `prefix`; `answering_another`
/// gives each an `also_answers` naming another item.
fn unrelated_cars(prefix: &str, n: usize, answering_another: bool) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let mut md = json!({ "branch": format!("feat/{prefix}-{i}") });
            if answering_another {
                md["also_answers"] = json!(["0a1b2c3d-0000-4000-8000-00000000000e"]);
            }
            json!({ "id": format!("{prefix}-0000-4000-8000-{i:012}"), "kind": "ship-a-change",
                    "status": "open", "title": "an unrelated car", "steps": [], "metadata": md })
        })
        .collect()
}

fn ids(cars: &[Value]) -> Value {
    json!(cars.iter().map(|c| c["id"].clone()).collect::<Vec<_>>())
}

/// Retire the item against `packets` under `fixture` (merged over the
/// stub's protocol row); answer the result and the stub.
async fn retire_under(packets: Vec<Value>, fixture: Value) -> (anyhow::Result<()>, Stub) {
    let mut protocol = json!({ "kind": "ship-a-change", "version": 7, "status": "active",
                               "steps": [{ "title": "review" }] });
    for (k, v) in fixture.as_object().unwrap() {
        protocol[k] = v.clone();
    }
    let s = stub_with_protocol(packets, protocol).await;
    let wire = Wire::at(s.base.clone(), named()).unwrap();
    let result = retire(&wire, "bd93d2be", "stale", "gone", None, None).await;
    (result, s)
}

/// REVIEWS 24ab880a and ac23854d [P2], Codex's whole-verb regressions:
/// absence of an answering car is what permits the write, and no read
/// paged on offset can prove an absence — the rows before the offset
/// can change between pages. 100 newer unrelated open cars carry
/// `also_answers`, then the car whose list names the item (index 100).
/// After page one (the 100, `total` 101):
/// - 24ab880a: the 100 leave the set, page two at offset 100 is empty
///   with `total` 1, and the answering car (now index 0) is never read;
/// - ac23854d: the 100 leave AND 100 older open cars take an
///   `also_answers` naming another item — an ordinary metadata merge —
///   so `total` stays 101, every page is full, every id unique, and
///   offset 100 serves the last older car instead of the answering one.
///
/// Either way the retirement must refuse before any write. The read is
/// therefore ONE statement, which is one snapshot; control: unraced,
/// the answering car refuses it by name.
#[tokio::test]
async fn a_census_raced_between_pages_cannot_hide_the_answering_car() {
    let newer = unrelated_cars("c0000000", crate::train::PAGE_LIMIT, true);
    let older = unrelated_cars("d0000000", crate::train::PAGE_LIMIT, false);
    let answering = car(
        "open",
        json!({ "also_answers": [HELD_JOB], "branch": "feat/answers" }),
    );
    let mut packets = vec![item(build("active", None))];
    packets.extend(newer.clone());
    packets.push(answering);
    packets.extend(older.clone());

    for (race, fixture) in [
        (
            "an equal-count swap (ac23854d)",
            json!({ "leave_after_a_page": ids(&newer),
                    "gain_also_answers_after_a_page": ids(&older) }),
        ),
        (
            "the set shrinks (24ab880a)",
            json!({ "leave_after_a_page": ids(&newer) }),
        ),
        ("unraced control", json!({})),
    ] {
        let (result, s) = retire_under(packets.clone(), fixture).await;
        let err = result.expect_err(&format!("{race}: an open car answers the item"));
        assert!(
            err.to_string().contains("open car(s) answer")
                && err.to_string().contains("feat/answers"),
            "{race}: refused by the answering car: {err:#}"
        );
        assert!(
            s.puts.lock().unwrap().is_empty(),
            "{race}: nothing was written"
        );
        assert_eq!(
            stored_build(&s)["status"],
            "active",
            "{race}: the build stays open"
        );
    }
}

/// A SET LARGER THAN ONE STATEMENT MAY RETURN IS NOT READ AS AN ABSENCE:
/// the server clamps `limit` (1000 in `http/mod.rs`; 100 here), and a
/// census that would need a second page refuses before any write —
/// even when, as here, no car on either page answers the item.
#[tokio::test]
async fn a_census_past_the_one_statement_limit_refuses_before_any_write() {
    let mut packets = vec![item(build("active", None))];
    packets.extend(unrelated_cars("c0000000", 101, true));
    let (result, s) = retire_under(packets, json!({ "max_limit": 100 })).await;
    let err = result.expect_err("101 cars cannot be read in one 100-row statement");
    assert!(format!("{err:#}").contains("in one statement"), "{err:#}");
    assert!(s.puts.lock().unwrap().is_empty(), "nothing was written");
    assert_eq!(stored_build(&s)["status"], "active", "the build stays open");
}

#[test]
fn retirable_takes_the_one_open_step_whose_row_admits_the_withdrawal() {
    let p = item(build("active", None));
    assert_eq!(retirable(&p, "stale").unwrap()["id"], HELD_STEP);
    assert_eq!(retirable(&p, "decline").unwrap()["id"], HELD_STEP);

    for not_one in ["delivered", "build", "design", "verify", "superseded"] {
        let err = retirable(&p, not_one).unwrap_err();
        assert!(err.contains("not a retirement"), "{not_one}: {err}");
    }

    let mut at_triage = p.clone();
    at_triage["steps"][0]["status"] = json!("ready");
    let err = retirable(&at_triage, "stale").unwrap_err();
    assert!(err.contains("boss triage"), "{err}");

    let mut closed = p.clone();
    closed["status"] = json!("closed");
    assert!(retirable(&closed, "stale").unwrap_err().contains("closed"));

    // A step whose row declares no withdrawal: nothing to write.
    let mut review = p.clone();
    review["steps"][1]["fields"] =
        json!([{ "name": "verdict", "field_type": "approved|declined" }]);
    assert!(
        retirable(&review, "stale")
            .unwrap_err()
            .contains("no open step")
    );

    // A done build is no door either.
    let mut done = p.clone();
    done["steps"][1]["status"] = json!("completed");
    assert!(
        retirable(&done, "stale")
            .unwrap_err()
            .contains("no open step")
    );
}

#[test]
fn retire_writes_refuse_a_withdrawal_with_nothing_behind_it() {
    assert!(
        retire_writes("stale", "   ", None, None)
            .unwrap_err()
            .contains("blank")
    );
    assert!(
        retire_writes("duplicate", "x", None, None)
            .unwrap_err()
            .contains("--of")
    );
    assert!(
        retire_writes("stale", "x", Some("abc"), None)
            .unwrap_err()
            .contains("only accompanies")
    );
    let w = retire_writes("stale", "x", None, Some(LANDED)).unwrap();
    assert_eq!(w["landed_in"], LANDED);
    assert_eq!(w["disposition"], "stale");
}

#[test]
fn run_and_car_refusals_read_each_row_not_the_filter() {
    // A row for another item, and a closed run, are not this item's.
    let runs = vec![
        run(HELD_RUN, "open", "someone-else"),
        run("r2", "closed", HELD_JOB),
    ];
    assert_eq!(run_refusal(HELD_JOB, &runs, None), None);
    assert_eq!(
        run_refusal(
            HELD_JOB,
            &[],
            Some((HELD_RUN, Ok(run(HELD_RUN, "cancelled", HELD_JOB))))
        ),
        None
    );
    assert_eq!(
        car_refusal(HELD_JOB, &[car("open", json!({ "backlog_item": "other" }))]),
        None
    );
}

/// THE LANDING COMMIT IS PROVED, NOT TYPED: on main it resolves to its
/// full sha; a commit main does not hold, and a name that is no commit,
/// are refused.
#[test]
fn a_landing_commit_must_be_on_main() {
    let root = boss_testing::scratch::scratch_dir("retire-landed-in");
    let g = |args: &[&str]| -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
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
    let w = |rel: &str, body: &str| boss_testing::scratch::write_file(&root.join(rel), body);
    g(&["init", "-q", "-b", "main"]);
    w("page.svelte", "old\n");
    g(&["add", "."]);
    g(&["commit", "-qm", "base"]);
    w("page.svelte", "removed\n");
    g(&["commit", "-qam", "train: UI step 1"]);
    let landed = g(&["rev-parse", "HEAD"]);
    g(&["checkout", "-q", "-b", "side"]);
    w("side.rs", "never landed\n");
    g(&["add", "."]);
    g(&["commit", "-qm", "side"]);
    let side = g(&["rev-parse", "HEAD"]);
    g(&["checkout", "-q", "main"]);
    let main = g(&["rev-parse", "main"]);

    assert_eq!(landed_commit(&root, &landed[..9], &main).unwrap(), landed);
    assert!(
        landed_commit(&root, &side, &main)
            .unwrap_err()
            .contains("not on the forge's main")
    );
    assert!(
        landed_commit(&root, "nosuchref", &main)
            .unwrap_err()
            .contains("not a commit")
    );
}
