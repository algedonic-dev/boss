//! `GET /api/yard/rule-firings` — every dispatcher rule's newest firing
//! and its dead-letters, for the rules list (backlog 43c4451a, found by
//! page-audit 08a444bc).
//!
//! WHY. `/it/registry/rules` listed each rule's trigger and version and
//! nothing about whether it RUNS, so a stalled `auto-park-on-gate-green`
//! and an idle one painted the same row — the question the world map's
//! borders answer for the one rule they name, asked of every rule.
//!
//! TWO RECORDS THAT ALREADY EXIST, NO NEW ONE. A firing is the
//! `dispatcher_firings` row the rules runner writes when every handler
//! succeeded (b14afc48); a failure past the redelivery budget is the
//! `dead_letter` annotation the runner lands on the packet it owed
//! (a9c498eb) — or, when the topic names no packet, the `dead-letter`
//! row it records in `dispatcher_firings` instead (4b175523). This
//! handler reads them and reduces the dead-letters per rule
//! ([`crate::dispatcher_firings::dead_letter_rollup`], pure and
//! unit-tested). It lives beside the borders because it is the same
//! record the borders read; the rules themselves are the dispatcher's,
//! and the page joins the two by rule name.
//!
//! UNREAD IS NOT EMPTY. Each half is `null` with its reason when it
//! cannot be read — no record wired, a failed read, a caller whose
//! policy does not read every packet (withheld, d0058c92), or more
//! dead-lettered packets than one page holds — because an empty list
//! would paint every rule "no firing" or
//! "no failures" on no evidence, which is how a dead machine reads
//! healthy.

use super::*;

use crate::dispatcher_firings::{
    DEAD_LETTER_KEY, DeadLetterRollup, RETENTION_DAYS, RuleLastFiring, UnroutedDeadLetters,
    dead_letter_rollup, with_unrouted,
};

pub(super) async fn yard_rule_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let since = now - chrono::Duration::days(RETENTION_DAYS);
    // ONE policy question gates every half (backlog e5f7b51e). Until
    // then only the packet dead-letters asked it, and the firing record
    // beside them answered every rule's name and newest instant to a
    // caller with no identity at all. A caller whose scope reads no
    // packets reads nothing about the machinery that moves them either.
    //
    // NOR DOES ONE WHOSE SCOPE READS SOME PACKETS BUT NOT ALL (backlog
    // d0058c92, review abebd39c B1). The firing record names when every
    // rule last fired, whoever's packets it moved — the record the map's
    // borders and the dispatcher's schedule withhold below a full scope —
    // so this door withholds it too. JUDGED ON THE TRANSLATED SCOPE
    // (`JobScope::from_predicate`): a department grant held outside its
    // department is a predicate, not `Predicate::None`, and reads no
    // packets; it was served the record here until then. The dead-letters
    // go with it, BOTH halves: the unrouted ones are that same record, and
    // serving only the packet half would break the both-or-neither rule
    // below (4b175523).
    let asked = state.policy.scope_of(&user, controls::READ_JOB).await;
    let scope = match &asked {
        Ok(p) => match JobScope::from_predicate(&user, p) {
            JobScope::All => Ok(JobScope::All),
            JobScope::None => Err(
                "this caller's policy scope reads no packets, so neither the firing record \
                 nor the dead-letters on them are read for it"
                    .to_string(),
            ),
            JobScope::OwnerIs(_) | JobScope::OwnerIn(_) | JobScope::AccountIn(_) => Err(
                "this caller's policy scope does not read every packet, and the firing record \
                 is not scoped by packet, so neither it nor the dead-letters beside it are \
                 read for it — withheld"
                    .to_string(),
            ),
        },
        Err(e) => Err(format!("policy check failed: {e}")),
    };
    // Withheld BY SCOPE, said as a flag (backlog 1805bac0): the web told
    // a refusal from a failed read by matching the reason's opening
    // words, one fact kept twice in prose (CLAUDE.md 9a). A policy check
    // that failed is a fault, not a refusal, and carries no flag.
    let withheld = asked.is_ok() && scope.is_err();
    let (firings, firings_error) = split(match &scope {
        Ok(_) => last_firings(&state).await,
        Err(why) => Err(why.clone()),
    });
    // Both kinds of dead-letter or neither: a count missing the half
    // that names no packet would say "none" of a rule failing only on
    // invoice topics (4b175523).
    let (dead_letters, dead_letters_error) = split(match &scope {
        Err(why) => Err(why.clone()),
        Ok(scope) => match (
            dead_letters(&state, scope.clone(), since).await,
            unrouted_dead_letters(&state, since).await,
        ) {
            (Ok(packets), Ok(unrouted)) => Ok(with_unrouted(packets, &unrouted)),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
    });
    let mut body = serde_json::json!({
        "now": now,
        // The window both halves answer: the firing record prunes past
        // it, and the dead-letters are cut to it, so "no firing" means
        // none in this many days and never "never".
        "retention_days": RETENTION_DAYS,
        "firings": firings,
        "firings_error": firings_error,
        "dead_letters": dead_letters,
        "dead_letters_error": dead_letters_error,
    });
    // Present only when withheld, so a full scope's payload is unchanged.
    if withheld && let Some(o) = body.as_object_mut() {
        o.insert("withheld".into(), serde_json::Value::Bool(true));
    }
    Json(body).into_response()
}

fn split<T>(r: Result<T, String>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e)),
    }
}

async fn last_firings<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
) -> Result<Vec<RuleLastFiring>, String> {
    let repo = state
        .dispatcher_firings
        .as_ref()
        .ok_or("the dispatcher firing record is not wired to this jobs API")?;
    repo.last_firings()
        .await
        .map_err(|e| format!("reading dispatcher_firings: {e}"))
}

/// The dead-letters that named no packet, per rule, from the firing
/// record (4b175523). Not scoped row by row — they are about no packet —
/// but read only for a caller whose scope reads every packet (d0058c92),
/// as the handler gates every half (e5f7b51e).
async fn unrouted_dead_letters<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<UnroutedDeadLetters>, String> {
    let repo = state.dispatcher_firings.as_ref().ok_or(
        "the dispatcher firing record, which holds the dead-letters that name no packet, \
         is not wired to this jobs API",
    )?;
    repo.unrouted_dead_letters(since)
        .await
        .map_err(|e| format!("reading unrouted dead-letters from dispatcher_firings: {e}"))
}

/// The dead-letter annotations the caller can see, rolled up per rule.
/// Scoped by the caller's read policy like every packet read here — the
/// scope the handler translated; since d0058c92 only a caller who reads
/// every packet reaches this, and any other is told the count was not
/// read for it, never that it is zero.
async fn dead_letters<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    scope: JobScope,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<DeadLetterRollup>, String> {
    let filter = JobFilter {
        metadata_has: Some(DEAD_LETTER_KEY.to_string()),
        scope,
        ..Default::default()
    };
    let (rows, total) = state
        .jobs
        .list_jobs(&filter, MAX_LIMIT, 0)
        .await
        .map_err(|e| format!("reading dead-lettered packets: {e}"))?;
    // A limit is not a filter: a page short of the total would count a
    // sample and call it the whole. Refused, never truncated.
    if total > rows.len() as i64 {
        return Err(format!(
            "{total} packets carry a dead-letter and one read holds {}; a per-rule count \
             of part of them would undercount",
            rows.len()
        ));
    }
    Ok(dead_letter_rollup(&rows, since))
}
