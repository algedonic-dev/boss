//! The existing protected events read, with complete bounded inputs.
//! A snapshot is evidence for its captured interval, never permission
//! to change a gate mode or revoke a credential.

use boss_core::gate_evidence::Gate;
use boss_core::gate_window::{GateObservation, JoinedWindow};
use chrono::{DateTime, Duration, Utc};

pub(super) const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

pub(super) async fn read(
    client: &boss_core::machine_token::Client,
    url: &str,
    actor: &str,
) -> Result<JoinedWindow, String> {
    let mut response = client
        .get(url)
        .header("x-boss-user", actor)
        .header("x-sim-origin", super::common::sim_origin_value())
        .timeout(std::time::Duration::from_secs(4))
        .send()
        .await
        .map_err(|error| format!("GET {url}: {error:?}"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("GET {url} body: {error:?}"))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(format!(
                "GET {url} exceeds complete response bound {MAX_RESPONSE_BYTES}; no prefix is an observation"
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(format!(
            "GET {url} returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("GET {url} has no complete typed gate observation: {error}"))
}

pub(super) fn observation<'a>(
    snapshot: &'a JoinedWindow,
    gate: Gate,
    required: &[String],
    duration: Duration,
    before: DateTime<Utc>,
    after: DateTime<Utc>,
) -> Result<&'a GateObservation, String> {
    let observation = snapshot
        .observation
        .as_ref()
        .ok_or("producer supplies no complete gate observation")?;
    let mut expected = required.to_vec();
    expected.sort();
    // The producer requires what the launcher started and names the
    // rest as not launched (backlog 93e0814a); it still reads every
    // port. This consumer's roster is the whole of both — it decides
    // for itself, from those reads, which silent port it will excuse —
    // so the two lists together must be exactly what it asked about.
    let mut judged = observation.required_services.clone();
    judged.sort();
    let mut actual = judged.clone();
    actual.extend(observation.not_launched.iter().cloned());
    actual.sort();
    if expected.is_empty()
        || expected.windows(2).any(|pair| pair[0] == pair[1])
        || actual.windows(2).any(|pair| pair[0] == pair[1])
        || snapshot.gate != gate
        || observation.gate != gate
        || actual != expected
        || snapshot.required_services != judged
        || snapshot.from != observation.from
        || snapshot.now != observation.now
        || observation.now - observation.from != duration
        || before > after
        || observation.now < before
        || observation.now > after
    {
        return Err("gate observation does not bind the requested gate, complete roster, interval and wall-clock read".into());
    }
    let mut read_services = observation
        .reads
        .iter()
        .map(|read| read.service.clone())
        .collect::<Vec<_>>();
    read_services.sort();
    if read_services != expected {
        return Err(
            "gate observation live reads do not bind every required service exactly once".into(),
        );
    }
    if let Err(error) = &observation.facts {
        return Err(format!("gate observation log is unreadable: {error}"));
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::gate_window::{LiveRead, join_window};

    #[test]
    fn a_declared_roster_does_not_hide_missing_duplicate_or_foreign_live_reads() {
        let now = Utc::now();
        let required = vec!["policy".to_string()];
        for services in [vec![], vec!["policy", "policy"], vec!["policy", "jobs"]] {
            let reads = services
                .into_iter()
                .map(|service| LiveRead {
                    service: service.into(),
                    answer: Err("fixture unavailable".into()),
                })
                .collect();
            let snapshot = join_window(
                Gate::MachineGate,
                &required,
                now - Duration::hours(24),
                now,
                Ok(vec![]),
                reads,
            );
            assert!(
                observation(
                    &snapshot,
                    Gate::MachineGate,
                    &required,
                    Duration::hours(24),
                    now,
                    now
                )
                .is_err(),
                "every live answer must bind the entire roster before any permanent-down exception is projected"
            );
        }
    }
    #[test]
    fn a_producer_that_excuses_unlaunched_services_still_binds_the_whole_roster() {
        use boss_core::gate_window::{LaunchRoster, NotLaunched, PortProbe, join_launched_window};
        let now = Utc::now();
        let whole = vec!["policy".to_string(), "assets".to_string()];
        let excuse = |service: &str| NotLaunched {
            service: service.into(),
            binary: format!("boss-{service}-api"),
            reason: "fixture".into(),
        };
        let join = |roster: &LaunchRoster, services: &[&str]| {
            join_launched_window(
                Gate::MachineGate,
                roster,
                &[PortProbe {
                    service: "assets".into(),
                    listening: Ok(false),
                }],
                now - Duration::hours(24),
                now,
                Ok(vec![]),
                services
                    .iter()
                    .map(|service| LiveRead {
                        service: (*service).into(),
                        answer: Err("fixture unavailable".into()),
                    })
                    .collect(),
            )
        };
        let bind = |snapshot: &JoinedWindow| {
            observation(
                snapshot,
                Gate::MachineGate,
                &whole,
                Duration::hours(24),
                now,
                now,
            )
            .map(|observation| observation.reads.len())
        };
        let roster = LaunchRoster {
            required: vec!["policy".into()],
            not_launched: vec![excuse("assets")],
            errors: vec![],
        };
        // Required plus excused is the consumer's whole roster, and
        // every port of it was read.
        assert_eq!(bind(&join(&roster, &["policy", "assets"])), Ok(2));
        // The excused port's read may not be dropped from the observation.
        assert!(bind(&join(&roster, &["policy"])).is_err());
        // A producer may not excuse a name the consumer did not ask
        // about, nor name one service on both sides.
        let foreign = LaunchRoster {
            not_launched: vec![excuse("assets"), excuse("catalog")],
            ..roster.clone()
        };
        assert!(bind(&join(&foreign, &["policy", "assets", "catalog"])).is_err());
        let twice = LaunchRoster {
            required: whole.clone(),
            ..roster.clone()
        };
        assert!(bind(&join(&twice, &["policy", "assets"])).is_err());
    }

    #[test]
    fn an_older_producer_without_observation_is_not_a_clean_consumer_window() {
        let now = Utc::now();
        let required = vec!["policy".to_string()];
        let snapshot = join_window(
            Gate::MachineGate,
            &required,
            now - Duration::hours(24),
            now,
            Ok(vec![]),
            vec![LiveRead {
                service: "policy".into(),
                answer: Err("fixture".into()),
            }],
        );
        let mut wire = serde_json::to_value(snapshot).unwrap();
        wire.as_object_mut().unwrap().remove("observation");
        wire.as_object_mut().unwrap().remove("input_errors");
        wire["covers_requested_window"] = serde_json::json!(true);
        let old: JoinedWindow = serde_json::from_value(wire).unwrap();
        assert!(
            observation(
                &old,
                Gate::MachineGate,
                &required,
                Duration::hours(24),
                now,
                now
            )
            .is_err()
        );
    }
}
