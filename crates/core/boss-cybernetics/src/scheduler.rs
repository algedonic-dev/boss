//! Scheduled agent dispatch — runs agents on configured intervals.
//!
//! Parses simple schedule strings like "every 6h", "every 30m", "every 24h"
//! and submits synthetic messages to the cybernetics loop at each interval.

use std::sync::Arc;
use std::time::Duration;

use boss_core::agent::{AgentId, Message};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::Cybernetics;
use crate::config::AgentEntry;

/// Parse a schedule string into a Duration.
///
/// Supported formats: "every Nh", "every Nm", "every Ns"
fn parse_interval(schedule: &str) -> Option<Duration> {
    let s = schedule.trim().to_lowercase();
    let rest = s.strip_prefix("every ")?;
    let rest = rest.trim();

    if let Some(hours) = rest.strip_suffix('h') {
        let n: u64 = hours.trim().parse().ok()?;
        Some(Duration::from_secs(n * 3600))
    } else if let Some(mins) = rest.strip_suffix('m') {
        let n: u64 = mins.trim().parse().ok()?;
        Some(Duration::from_secs(n * 60))
    } else if let Some(secs) = rest.strip_suffix('s') {
        let n: u64 = secs.trim().parse().ok()?;
        Some(Duration::from_secs(n))
    } else {
        None
    }
}

/// Spawn a scheduler task for each agent that has a schedule configured.
/// Returns the join handles.
pub fn spawn_schedulers(
    agents: &[AgentEntry],
    cyb: Arc<Cybernetics>,
    cancel: watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    for agent in agents {
        let schedule = match &agent.schedule {
            Some(s) => s.clone(),
            None => continue,
        };

        let interval = match parse_interval(&schedule) {
            Some(d) => d,
            None => {
                warn!(
                    agent = %agent.id,
                    schedule = %schedule,
                    "invalid schedule format, skipping"
                );
                continue;
            }
        };

        let agent_id = match AgentId::try_new(agent.id.clone()) {
            Ok(id) => id,
            Err(_) => continue,
        };

        info!(
            agent = %agent.id,
            schedule = %schedule,
            interval_secs = interval.as_secs(),
            "scheduling agent"
        );

        let cyb = cyb.clone();
        let mut cancel = cancel.clone();

        handles.push(tokio::spawn(async move {
            // Initial delay: wait one interval before first run.
            tokio::time::sleep(interval).await;

            let mut tick = tokio::time::interval(interval);
            tick.tick().await; // consume the immediate tick

            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let message = Message::new(
                            agent_id.clone(),
                            "system.scheduled",
                            serde_json::json!({ "trigger": "cron", "schedule": schedule }),
                        );
                        info!(agent = %agent_id, "scheduled dispatch");
                        if let Err(e) = cyb.submit(message).await {
                            error!(agent = %agent_id, error = %e, "scheduled dispatch failed");
                        }
                    }
                    _ = cancel.changed() => {
                        if *cancel.borrow() {
                            info!(agent = %agent_id, "scheduler shutting down");
                            return;
                        }
                    }
                }
            }
        }));
    }

    handles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hours() {
        assert_eq!(
            parse_interval("every 6h"),
            Some(Duration::from_secs(6 * 3600))
        );
        assert_eq!(
            parse_interval("every 24h"),
            Some(Duration::from_secs(24 * 3600))
        );
    }

    #[test]
    fn parse_minutes() {
        assert_eq!(
            parse_interval("every 30m"),
            Some(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn parse_seconds() {
        assert_eq!(parse_interval("every 60s"), Some(Duration::from_secs(60)));
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_interval("daily 06:00"), None);
        assert_eq!(parse_interval(""), None);
        assert_eq!(parse_interval("every"), None);
    }
}
