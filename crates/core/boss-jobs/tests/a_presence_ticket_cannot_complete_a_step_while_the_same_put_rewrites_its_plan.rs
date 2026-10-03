//! A presence-door test. The defect it pins, the fix, and the fixture
//! it shares with the other presence tests are in `presence/mod.rs`.

mod presence;

use presence::*;

// ---------------------------------------------------------------------
// THE CONTENT JUDGED IS THE CONTENT COMPLETED (backlog c0b56fd9,
// adversarial review of car 5b30ccf9, 2026-09-25).
//
// A genuine ticket binds step, person and the step's shape hash, and
// `update_step` judged it against the step BEFORE the PUT's overlay. So
// on a presence step with no sign-off roles a valid ticket completed the
// step while the same PUT replaced the plan it had been minted over:
// 204, the stored plan replaced, and nobody's passkey ever saw the bytes
// that were completed. (On a sign-off step the same body was caught only
// incidentally, by the stale-stamp 409.) The test below is the real
// ticket the gateway would have issued — the forgery is not the point.
// ---------------------------------------------------------------------

/// THE BUG: the ceremony is over plan A, the completion carries plan B.
#[tokio::test]
async fn a_presence_ticket_cannot_complete_a_step_while_the_same_put_rewrites_its_plan() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = put(
        &app,
        GUARDED,
        Some(&t),
        r#"{"status":"completed","metadata":{"plan":"{\"verb\":\"wipe-the-system-disk\"}"}}"#,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a ticket minted over one plan must not complete another; body: {body}"
    );
    assert!(
        body.contains(&format!("/api/jobs/{JOB}/steps/{GUARDED}/metadata")),
        "the refusal names the merge door the content goes through first: {body}"
    );
    let after = stored(&jobs, GUARDED).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(
        after.metadata,
        step(GUARDED, None, false).metadata,
        "a refused completion writes nothing — the plan the ceremony saw stays"
    );

    // The same body is refused on the sign-off step too — by this rule,
    // not by the stale-stamp check it used to fall to by accident.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = put(
        &app,
        SIGNED,
        Some(&s),
        r#"{"status":"completed","metadata":{"plan":"{\"verb\":\"wipe-the-system-disk\"}"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(body.contains("/metadata"), "{body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // Scope control: a step that asks only for a session is not judged
    // by this rule, so its plan may be rewritten and the step completed
    // with no ticket at all. Since e39a9d2a (Stage 2's last car) no step
    // PUT writes metadata — the two bodies above now meet that refusal
    // before the presence rule, which still guards title, notes and
    // holder — so the rewrite goes through the merge door and the
    // completion is the status alone.
    let (status, body) = send(
        &app,
        "PATCH",
        format!("/api/jobs/{JOB}/steps/{ORDINARY}/metadata"),
        None,
        r#"{"plan":"rewritten at completion"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = put(&app, ORDINARY, None, r#"{"status":"completed"}"#).await;
    assert!(status.is_success(), "{status}: {body}");
    let done = stored(&jobs, ORDINARY).await;
    assert_eq!(done.status, StepStatus::Completed);
    assert_eq!(done.metadata["plan"], "rewritten at completion");
}
