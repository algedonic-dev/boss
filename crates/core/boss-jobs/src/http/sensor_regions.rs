//! The existing sensor telemetry and scoped alarm doors, read for the map.
use super::*;
use crate::regions::{
    Region,
    sensors::{self, Evidence},
};

pub(super) async fn read<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    now: chrono::DateTime<chrono::Utc>,
    hours: i64,
    reads_every_packet: bool,
) -> Region {
    // Same trust boundary as /api/sensors, before any telemetry-port read.
    if !crate::trust::can_read(user) {
        return sensors::unread(
            now,
            "sensor readings unavailable: Operator or Auditor access required",
        );
    }
    if !reads_every_packet {
        return sensors::unread(
            now,
            "sensor alarm reading unavailable within this packet scope",
        );
    }
    match evidence(state, now, hours).await {
        Ok(rows) => sensors::judge(&rows, now, hours),
        Err(why) => sensors::unread(now, &why),
    }
}

async fn evidence<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    now: chrono::DateTime<chrono::Utc>,
    hours: i64,
) -> Result<Vec<Evidence>, String> {
    let repo = state
        .sensors
        .as_ref()
        .ok_or("sensor registry and readings could not be read: no sensor port")?;
    let sensors = repo
        .list()
        .await
        .map_err(|e| format!("sensor registry could not be read: {e}"))?;
    let since = now - chrono::Duration::hours(hours);
    let mut out = Vec::new();
    for sensor in sensors {
        let id = &sensor.id;
        let current = repo
            .window(id, since, now)
            .await
            .map_err(|e| format!("sensor {id}: current window could not be read: {e}"))?;
        let previous = repo
            .window(id, since - chrono::Duration::hours(hours), since)
            .await
            .map_err(|e| format!("sensor {id}: previous window could not be read: {e}"))?;
        let owed = repo
            .unstamped(id)
            .await
            .map_err(|e| format!("sensor {id}: unstamped readings could not be read: {e}"))?;
        // Full packet scope was established above. Query the exact finding
        // identity and page to its total; an unread alarm tail cannot be clear.
        let alarms = crate::list_every::list_every(
            state.jobs.as_ref(),
            &JobFilter {
                kind: Some("backlog-item".into()),
                status: Some(JobStatus::Open),
                priority: Some(boss_core::job::Priority::Urgent),
                partition: Some(boss_core::partition::Partition::Real),
                metadata_contains: Some(
                    serde_json::json!({"estate_finding": format!("sensor_unreadable:{id}")}),
                ),
                scope: crate::port::JobScope::All,
                ..Default::default()
            },
            MAX_LIMIT,
        )
        .await
        .map_err(|e| format!("sensor {id}: alarms could not be read: {e}"))?;
        out.push(Evidence {
            sensor,
            current,
            previous,
            owed,
            alarms: alarms.into_iter().map(|j| j.id.to_string()).collect(),
        });
    }
    Ok(out)
}
