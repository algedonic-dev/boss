//! `jobs.reoffer_waiting` — a pool that frees re-offers the step that
//! has waited longest in its line (backlog 587f395a, allocation item 1
//! of design 012822ea).
//!
//! Fires on `jobs.step.released`: a claimed step left `active` and its
//! allocation was returned (completed, skipped, released, or its packet
//! closed or cancelled). Measured on origin/main 3a7be357, nothing read
//! that event — a claim refused `capacity-exhausted` wrote
//! `metadata.waiting_on` and waited for nobody. David's steer: queues are
//! everywhere, so an exhausted pool must mean wait.
//!
//! For each pool the release names (`claim.allocations[].pool`), it reads
//! the pool's line through the jobs API (`GET /api/claims/waiting`, oldest
//! wait first) and re-offers its oldest step not yet re-offered: it marks
//! that step's `waiting_on` with `reoffered_at` (the release's instant)
//! and `reoffered_by` (the release's event id) through the step merge
//! door, judged against the `waiting_on` it read. That is a WAKE, not a
//! claim: the step is READY before and after, claim-next's head offers it
//! again (it passes over a step holding its place until this mark is on
//! it), the `jobs.step.updated` the merge records reaches the dispatcher's
//! assignment loop, which nominates an unplaced step, and a nominee's
//! queue read shows it. The claim door is still the one act that takes
//! capacity, and it may refuse again — which drops the mark and keeps the
//! step's place, because a refusal on the same pool keeps `since`.
//!
//! One step per pool per release: a release frees one holder's amount,
//! and offering every waiter on one release would hand the same unit to
//! the whole line. Idempotent: a redelivery finds a step already marked
//! with this release's id and writes nothing. A step that moved between
//! the read and the write (claimed, or refused again, or offered by a
//! concurrent release) answers 409 `step_changed` and is left as it is,
//! and the release reads the line again and offers the next waiter: a
//! lost write does not spend its wake (review 4ed9395a R1). A line that
//! keeps moving past `MOVED_BOUND` hands the release back for redelivery.
//!
//! A release's wake is spent once, and the line cannot say so after the
//! offered step claims: the claim ends its wait and takes the mark with
//! it (review 4ed9395a R2). Two records make it one act (review R3, Codex
//! run c143d322: a mark and an account written as two independent acts
//! let an interrupted second write spend one release twice):
//! - the releasing packet's `reoffer_consumed_<event id>` names, per pool,
//!   the step this release is offering (`{job, step}`), written BEFORE the
//!   mark, so a replay always knows which step to ask;
//! - the mark carries `reoffer_receipt_<event id>` on that step beside its
//!   `waiting_on`, in the SAME write. A claim removes only `waiting_on`,
//!   so the receipt outlives the waiter leaving the line.
//! A replay reads the account's step: its receipt means the wake is spent;
//! no receipt means the mark never landed, and the step is offered again
//! if it still waits unoffered, or the next waiter is (and the account
//! moves to it). An interrupted account write leaves nothing offered; an
//! interrupted mark, landed or not, is told apart by the receipt.

use super::common::{
    Judged, dispatcher_reader_header, get_json, sim_origin_value, write_json, write_json_expecting,
};
use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::flow_resources::{REOFFERED_AT, REOFFERED_BY, WAITING_KEY};
use serde_json::json;
use std::sync::Arc;

pub const HANDLER: &str = "jobs.reoffer_waiting";

pub struct JobsReofferWaiting {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsReofferWaiting {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Self::with_client(crate::handlers::common::api_client(), jobs_base)
    }

    /// Construct with a custom client (tests point it at a local server).
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }
}

/// The pools a release returned, in its own order, each once. A release
/// that names none is a broken contract, refused rather than read as
/// "nothing waits" (no evidence is not a pass).
pub fn released_pools(payload: &serde_json::Value) -> Result<Vec<String>, String> {
    let allocations = payload
        .pointer("/claim/allocations")
        .and_then(serde_json::Value::as_array)
        .ok_or("the release carries no claim.allocations")?;
    let mut pools: Vec<String> = Vec::new();
    for allocation in allocations {
        let pool = allocation
            .get("pool")
            .and_then(serde_json::Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or("a released allocation names no pool")?;
        if !pools.iter().any(|p| p == pool) {
            pools.push(pool.to_string());
        }
    }
    if pools.is_empty() {
        return Err("the release returned no allocation".into());
    }
    Ok(pools)
}

/// The step a release re-offers from a pool's `line`: `None` when this
/// release already re-offered one (a redelivery) or when every waiter is
/// already re-offered; otherwise the oldest not yet re-offered.
pub fn to_reoffer<'a>(
    line: &'a [serde_json::Value],
    release: &str,
) -> Option<&'a serde_json::Value> {
    let mark = |w: &serde_json::Value, key: &str| {
        w.get(WAITING_KEY)
            .and_then(|wait| wait.get(key))
            .filter(|v| !v.is_null())
            .cloned()
    };
    if line
        .iter()
        .any(|w| mark(w, REOFFERED_BY) == Some(json!(release)))
    {
        return None;
    }
    line.iter().find(|w| mark(w, REOFFERED_AT).is_none())
}

/// The packet metadata key that records what release `release` offers.
pub fn consumed_key(release: &str) -> String {
    format!("{CONSUMED_PREFIX}{release}")
}

/// The prefix of [`consumed_key`].
pub const CONSUMED_PREFIX: &str = "reoffer_consumed_";

/// The step metadata key a mark carries beside `waiting_on`, in the same
/// write: the receipt that this release offered this step, which outlives
/// the claim that ends the wait.
pub fn receipt_key(release: &str) -> String {
    format!("{RECEIPT_PREFIX}{release}")
}

/// The prefix of [`receipt_key`].
pub const RECEIPT_PREFIX: &str = "reoffer_receipt_";

/// The account this release already wrote on its packet: pool -> the
/// `{job, step}` it offers. A key that is not an object is a broken record,
/// refused rather than read as nothing spent.
pub fn consumed(
    packet_metadata: &serde_json::Value,
    release: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match packet_metadata.get(consumed_key(release)) {
        None | Some(serde_json::Value::Null) => Ok(serde_json::Map::new()),
        Some(serde_json::Value::Object(m)) => Ok(m.clone()),
        Some(other) => Err(format!(
            "{} is not an object of pool to step: {other}",
            consumed_key(release)
        )),
    }
}

/// The `(job, step)` one pool's account entry names; a malformed entry is
/// refused rather than read as nothing offered.
pub fn account_target(entry: &serde_json::Value) -> Result<(String, String), String> {
    let field = |k: &str| {
        entry
            .get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
    };
    match (field("job"), field("step")) {
        (Some(job), Some(step)) => Ok((job, step)),
        _ => Err(format!("an account entry names no job and step: {entry}")),
    }
}

/// Whether a step's metadata carries this release's receipt.
pub fn has_receipt(step_metadata: &serde_json::Value, release: &str) -> bool {
    step_metadata
        .get(receipt_key(release))
        .is_some_and(|r| !r.is_null())
}

/// How many judged writes in a row may lose to a moving line before a
/// release is handed back for redelivery rather than spun on.
const MOVED_BOUND: usize = 4;

impl JobsReofferWaiting {
    /// The pool's waiting line, oldest wait first. A body with no `line`
    /// is no answer, never an empty line: read as nobody waiting it would
    /// ACK the one release that could have woken them.
    async fn read_line(
        &self,
        base: &str,
        pool: &str,
    ) -> Result<Vec<serde_json::Value>, HandlerError> {
        let url = reqwest::Url::parse_with_params(
            &format!("{base}/api/claims/waiting"),
            &[("pool", pool)],
        )
        .map_err(|e| HandlerError::Permanent(format!("the waiting-line URL: {e}")))?;
        let resp = self
            .client
            .get(url.as_str())
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))?;
        body.get("line")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .ok_or_else(|| HandlerError::Downstream(format!("GET {url} answered no line: {body}")))
    }

    /// The metadata of step `step` on packet `job`, as the jobs API serves
    /// the packet; `None` when the packet no longer lists the step.
    async fn step_metadata(
        &self,
        base: &str,
        job: &str,
        step: &str,
        ctx: &InvocationContext,
    ) -> Result<Option<serde_json::Value>, HandlerError> {
        let packet = get_json(
            &self.client,
            &format!("{base}/api/jobs/{job}"),
            &ctx.rule_name,
        )
        .await?;
        let steps = packet
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                HandlerError::Downstream(format!("GET /api/jobs/{job} answered no steps"))
            })?;
        Ok(steps
            .iter()
            .find(|s| s.get("id").and_then(serde_json::Value::as_str) == Some(step))
            .map(|s| {
                s.get("metadata")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            }))
    }

    /// Mark one waiter re-offered by this release, judged against the
    /// `waiting_on` it was read with. [`Judged::Moved`]: claimed or refused
    /// again between the read and the write.
    async fn mark(
        &self,
        base: &str,
        head: &serde_json::Value,
        pool: &str,
        at: &serde_json::Value,
        ctx: &InvocationContext,
    ) -> Result<Judged, HandlerError> {
        let (Some(job), Some(step), Some(wait)) = (
            head.get("job_id").and_then(serde_json::Value::as_str),
            head.get("step_id").and_then(serde_json::Value::as_str),
            head.get(WAITING_KEY).filter(|w| w.is_object()),
        ) else {
            return Err(HandlerError::Downstream(format!(
                "the waiting line holds a row without its job, step or wait: {head}"
            )));
        };
        let mut marked = wait.clone();
        marked[REOFFERED_AT] = at.clone();
        marked[REOFFERED_BY] = json!(ctx.triggering_event_id);
        let mut expect = serde_json::Map::new();
        expect.insert(WAITING_KEY.into(), wait.clone());
        write_json_expecting(
            &self.client,
            reqwest::Method::PATCH,
            &format!("{base}/api/jobs/{job}/steps/{step}/metadata"),
            &json!({
                WAITING_KEY: marked,
                receipt_key(&ctx.triggering_event_id): {"pool": pool, "at": at},
            }),
            &expect,
            &ctx.rule_name,
        )
        .await
    }
}

#[async_trait]
impl Handler for JobsReofferWaiting {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let pools = released_pools(&ctx.event_payload).map_err(|why| {
            HandlerError::Permanent(format!(
                "{HANDLER} on event {}: {why}",
                ctx.triggering_event_id
            ))
        })?;
        let at = json!(ctx.firing_instant()?);
        let base = self.jobs_base.trim_end_matches('/');
        let release = ctx.triggering_event_id.as_str();
        let packet = ctx
            .event_payload
            .get("job_id")
            .and_then(serde_json::Value::as_str)
            .filter(|j| !j.trim().is_empty())
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "{HANDLER} on event {release}: the release names no job_id"
                ))
            })?;
        let packet_url = format!("{base}/api/jobs/{packet}");
        let read = get_json(&self.client, &packet_url, &ctx.rule_name).await?;
        let mut spent = consumed(
            read.get("metadata").unwrap_or(&serde_json::Value::Null),
            release,
        )
        .map_err(|why| HandlerError::Permanent(format!("{HANDLER} on event {release}: {why}")))?;
        for pool in pools {
            // The step this release offers on this pool, by its account.
            let mut target = match spent.get(&pool) {
                Some(entry) => Some(account_target(entry).map_err(|why| {
                    HandlerError::Permanent(format!("{HANDLER} on event {release}: {why}"))
                })?),
                None => None,
            };
            // Review 4ed9395a R1: a judged mark that loses is not a spent
            // wake; the line is read again, bounded.
            let mut moved = 0;
            loop {
                // Review R3: the receipt rides the mark's own write, so it
                // says whether the offer landed, though the waiter claimed.
                if let Some((job, step)) = &target
                    && self
                        .step_metadata(base, job, step, ctx)
                        .await?
                        .is_some_and(|m| has_receipt(&m, release))
                {
                    break;
                }
                let line = self.read_line(base, &pool).await?;
                let unoffered = |w: &&serde_json::Value| {
                    w.get(WAITING_KEY)
                        .and_then(|wait| wait.get(REOFFERED_AT))
                        .is_none_or(serde_json::Value::is_null)
                };
                let still = target.as_ref().and_then(|(_, step)| {
                    line.iter().filter(unoffered).find(|w| {
                        w.get("step_id").and_then(serde_json::Value::as_str) == Some(step)
                    })
                });
                let head = match still {
                    Some(head) => head,
                    None => {
                        // No account yet, or its step left the line or was
                        // offered by another release without this one's
                        // receipt: the wake is unspent, so the next waiter.
                        let Some(head) = to_reoffer(&line, release) else {
                            break;
                        };
                        let (Some(job), Some(step)) = (
                            head.get("job_id").and_then(serde_json::Value::as_str),
                            head.get("step_id").and_then(serde_json::Value::as_str),
                        ) else {
                            return Err(HandlerError::Downstream(format!(
                                "the waiting line holds a row without its job or step: {head}"
                            )));
                        };
                        spent.insert(pool.clone(), json!({"job": job, "step": step}));
                        write_json(
                            &self.client,
                            reqwest::Method::PATCH,
                            &format!("{packet_url}/metadata"),
                            &json!({ consumed_key(release): spent }),
                            &ctx.rule_name,
                        )
                        .await?;
                        target = Some((job.to_string(), step.to_string()));
                        head
                    }
                };
                match self.mark(base, head, &pool, &at, ctx).await? {
                    Judged::Written => break,
                    Judged::Moved => {
                        moved += 1;
                        if moved >= MOVED_BOUND {
                            return Err(HandlerError::Downstream(format!(
                                "{HANDLER} on event {release}: the {pool} line moved under \
                                 {moved} judged writes in a row; redelivered rather than \
                                 acknowledged with its wake unspent"
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_names_each_pool_it_returned_once() {
        let payload = json!({"claim":{"allocations":[
            {"pool":"vcpu","version":1,"amount":"1"},
            {"pool":"ram-gb","version":1,"amount":"4"},
            {"pool":"vcpu","version":1,"amount":"1"},
        ]}});
        assert_eq!(released_pools(&payload).unwrap(), vec!["vcpu", "ram-gb"]);
        assert!(
            released_pools(&json!({})).is_err(),
            "a release naming no pool is a broken contract, not an empty line"
        );
        assert!(released_pools(&json!({"claim":{"allocations":[]}})).is_err());
    }

    #[test]
    fn the_oldest_waiter_not_yet_reoffered_is_offered_once_per_release() {
        let w = |id: &str, extra: serde_json::Value| {
            let mut wait = json!({"resource":"vcpu","reason":"capacity-exhausted"});
            for (k, v) in extra.as_object().unwrap() {
                wait[k] = v.clone();
            }
            json!({"step_id": id, "job_id": "j", "waiting_on": wait})
        };
        let line = vec![
            w(
                "a",
                json!({"reoffered_at":"2026-11-06T15:00:00Z","reoffered_by":"evt-1"}),
            ),
            w("b", json!({})),
            w("c", json!({})),
        ];
        assert_eq!(
            to_reoffer(&line, "evt-2").map(|h| h["step_id"].clone()),
            Some(json!("b")),
            "a still re-offered from an earlier release keeps its offer; the next waiter gets this one"
        );
        assert_eq!(
            to_reoffer(&line, "evt-1"),
            None,
            "a redelivered release finds its own mark and offers nobody else"
        );
        assert_eq!(to_reoffer(&[], "evt-3"), None);
    }

    #[test]
    fn a_release_reads_its_own_spent_account_and_refuses_a_broken_one() {
        let meta = json!({
            "reoffer_consumed_evt-1": {"vcpu": {"job": "j", "step": "step-a"}},
            "reoffer_consumed_evt-2": "not an account",
        });
        assert_eq!(
            consumed(&meta, "evt-1").unwrap().get("vcpu"),
            Some(&json!({"job": "j", "step": "step-a"})),
            "the pool this release already spent is read back"
        );
        assert!(
            consumed(&meta, "evt-3").unwrap().is_empty(),
            "a release with no account has spent nothing"
        );
        assert!(
            consumed(&meta, "evt-2").is_err(),
            "a broken account is refused, never read as nothing spent"
        );
        assert!(
            consumed(&serde_json::Value::Null, "evt-1")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_account_names_the_step_a_release_offers_and_its_receipt_says_it_landed() {
        assert_eq!(
            account_target(&json!({"job": "j", "step": "s"})).unwrap(),
            ("j".to_string(), "s".to_string())
        );
        assert!(
            account_target(&json!("s")).is_err(),
            "an entry with no job and step is refused, never read as nothing offered"
        );
        let meta = json!({"waiting_on": null, "reoffer_receipt_evt-1": {"pool": "vcpu"}});
        assert!(has_receipt(&meta, "evt-1"), "a claim keeps the receipt");
        assert!(!has_receipt(&meta, "evt-2"));
        assert!(!has_receipt(
            &json!({"reoffer_receipt_evt-1": null}),
            "evt-1"
        ));
    }
}
