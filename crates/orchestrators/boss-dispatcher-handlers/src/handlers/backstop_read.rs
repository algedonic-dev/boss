//! A transient unreadable backstop gets one bounded second observation.
use boss_dispatcher::rules::handler::HandlerError;
use std::future::Future;

pub(super) async fn retry_once<T, F, Fut>(mut read: F) -> Result<T, HandlerError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, HandlerError>>,
{
    match read().await {
        Ok(value) => Ok(value),
        Err(first) => {
            tracing::warn!(error = %first, "unreadable backstop read; retrying once after 250ms");
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            read().await.map_err(|last| {
                HandlerError::Downstream(format!("first read: {first}; retry: {last}"))
            })
        }
    }
}

use super::alarm_item::{self, Occurrence};
use super::common::{RECOVERED_AT, write_json};
use boss_dispatcher::rules::handler::InvocationContext;
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

const STATE: &str = "backstop_unreadable";

// The existing alarm is the persistence port, so a dispatcher restart cannot
// erase a failed tick. Count observations, never retries or replays of a tick.
fn failure_state(
    alarm: Option<&Json>,
    ctx: &InvocationContext,
) -> Result<Option<Json>, HandlerError> {
    let now = ctx.firing_instant()?;
    let previous = alarm
        .filter(|alarm| {
            alarm
                .pointer(&format!("/metadata/{RECOVERED_AT}"))
                .is_none_or(Json::is_null)
        })
        .and_then(|alarm| alarm.pointer(&format!("/metadata/{STATE}")))
        .filter(|state| state["rule"].as_str() == Some(ctx.rule_name.as_str()))
        .and_then(|state| {
            let count = state["consecutive_failed_ticks"]
                .as_u64()
                .filter(|count| (1..=2).contains(count))?;
            let at = DateTime::parse_from_rfc3339(state["last_failed_tick"].as_str()?)
                .ok()?
                .with_timezone(&Utc);
            Some((count, at))
        });
    let count = match previous {
        Some((_, last)) if now < last => return Ok(None),
        Some((count, last)) if now == last => count,
        Some((count, _)) => (count + 1).min(2),
        None => 1,
    };
    Ok(Some(
        json!({"rule":ctx.rule_name,"last_failed_tick":now.to_rfc3339(),"consecutive_failed_ticks":count}),
    ))
}

pub(super) async fn raise_unreadable(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    alarm: Option<&Json>,
    mut body: Json,
    ctx: &InvocationContext,
) -> Result<(), HandlerError> {
    let Some(state) = failure_state(alarm, ctx)? else {
        tracing::warn!(rule = %ctx.rule_name, "older unreadable tick cannot overwrite newer evidence");
        return Ok(());
    };
    let priority = if state["consecutive_failed_ticks"] == 2 {
        "urgent"
    } else {
        "standard"
    };
    let metadata = body
        .get_mut("metadata")
        .and_then(Json::as_object_mut)
        .ok_or_else(|| HandlerError::Permanent("unreadable alarm has no metadata object".into()))?;
    metadata.insert(STATE.into(), state.clone());
    // The whole reading refreshes an open alarm; the helper writes only
    // the keys that changed, and a relapse after a recovery note is an
    // occurrence on it (backlog f4b0ad41) rather than a hand-merged
    // `relapse_patch`.
    let refresh = metadata.clone();
    body["priority"] = json!(priority);
    let occ = Occurrence::new(
        state["last_failed_tick"].as_str().unwrap_or_default(),
        format!(
            "{}: the read could not be judged on {} consecutive observed tick(s)",
            ctx.rule_name, state["consecutive_failed_ticks"]
        ),
    );
    if let Some(alarm) = alarm
        && alarm["id"].as_str().is_none_or(str::is_empty)
    {
        return Err(HandlerError::Permanent("unreadable alarm has no id".into()));
    }
    alarm_item::raise(
        client,
        jobs_base,
        alarm,
        body,
        &occ,
        refresh,
        &ctx.rule_name,
    )
    .await?;
    if let Some(alarm) = alarm
        && alarm["priority"].as_str() != Some(priority)
    {
        let id = alarm["id"].as_str().unwrap_or_default();
        write_json(client, reqwest::Method::PUT, &format!("{jobs_base}/api/jobs/{id}/priority"), &json!({"priority":priority,"why":format!("{}: {} consecutive failed observed tick(s), ending {}; each read retried once. First tick is standard; two are urgent.", ctx.rule_name, state["consecutive_failed_ticks"], state["last_failed_tick"])}), &ctx.rule_name).await?;
    }
    Ok(())
}

pub(super) async fn recovered(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    alarm: Option<&Json>,
    ctx: &InvocationContext,
) -> Result<bool, HandlerError> {
    let Some(alarm) = alarm.filter(|alarm| {
        alarm
            .pointer(&format!("/metadata/{STATE}"))
            .is_some_and(|value| !value.is_null())
    }) else {
        return Ok(true);
    };
    let now = ctx.firing_instant()?;
    let newer = alarm
        .pointer(&format!("/metadata/{STATE}"))
        .filter(|state| state["rule"].as_str() == Some(ctx.rule_name.as_str()))
        .and_then(|state| state["last_failed_tick"].as_str())
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|at| at.with_timezone(&Utc) > now);
    if newer {
        return Ok(false);
    }
    let id = alarm["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| HandlerError::Permanent("unreadable alarm has no id".into()))?;
    write_json(
        client,
        reqwest::Method::PATCH,
        &format!("{jobs_base}/api/jobs/{id}/metadata"),
        &json!({STATE:Json::Null}),
        &ctx.rule_name,
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx(at: &str) -> InvocationContext {
        InvocationContext {
            rule_name: "fixture".into(),
            triggering_event_id: at.into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_at":at}),
            event_timestamp: None,
        }
    }
    #[test]
    fn repeated_and_older_ticks_never_manufacture_a_second_failure() {
        let first = ctx("2026-10-31T01:00:00Z");
        let state = failure_state(None, &first).unwrap().unwrap();
        let alarm = json!({"metadata":{STATE:state}});
        assert_eq!(
            failure_state(Some(&alarm), &first).unwrap().unwrap()["consecutive_failed_ticks"],
            1
        );
        assert!(
            failure_state(Some(&alarm), &ctx("2026-10-31T00:00:00Z"))
                .unwrap()
                .is_none()
        );
        let next = failure_state(Some(&alarm), &ctx("2026-10-31T02:00:00Z"))
            .unwrap()
            .unwrap();
        assert_eq!(next["consecutive_failed_ticks"], 2);
        let alarm = json!({"metadata":{STATE:next}});
        assert_eq!(
            failure_state(Some(&alarm), &ctx("2026-10-31T03:00:00Z"))
                .unwrap()
                .unwrap()["consecutive_failed_ticks"],
            2
        );
    }
    #[test]
    fn recovered_malformed_and_foreign_state_cannot_certify_continuity() {
        let now = ctx("2026-10-31T02:00:00Z");
        for metadata in [
            json!({STATE:{"rule":"fixture","last_failed_tick":"2026-10-31T01:00:00Z","consecutive_failed_ticks":2},RECOVERED_AT:"2026-10-31T01:30:00Z"}),
            json!({STATE:{"rule":"other","last_failed_tick":"2026-10-31T01:00:00Z","consecutive_failed_ticks":2}}),
            json!({STATE:{"rule":"fixture","last_failed_tick":"bad","consecutive_failed_ticks":2}}),
            json!({STATE:{"rule":"fixture","last_failed_tick":"2026-10-31T01:00:00Z","consecutive_failed_ticks":99}}),
        ] {
            assert_eq!(
                failure_state(Some(&json!({"metadata":metadata})), &now)
                    .unwrap()
                    .unwrap()["consecutive_failed_ticks"],
                1
            );
        }
    }
}
