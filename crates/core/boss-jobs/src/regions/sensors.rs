//! Sensor telemetry at the IT-map station (approved design eb008249 D1-D3).
use super::*;
use crate::sensors::{Reading as SensorReading, ReadingsWindow, SensorRow};

pub(crate) struct Evidence {
    pub sensor: SensorRow,
    pub current: ReadingsWindow,
    pub previous: ReadingsWindow,
    pub owed: Vec<SensorReading>,
    pub alarms: Vec<String>,
}

pub(crate) fn judge(rows: &[Evidence], now: Instant, hours: i64) -> Region {
    match checked(rows, now, hours) {
        Ok(r) => r,
        Err(why) => unread(now, &why),
    }
}

fn checked(rows: &[Evidence], now: Instant, hours: i64) -> Result<Region, String> {
    let mut ids = std::collections::BTreeSet::new();
    let mut current = 0_u64;
    let mut previous = 0_u64;
    let mut findings = Vec::new();
    for e in rows {
        let s = &e.sensor;
        if s.id.is_empty()
            || !ids.insert(s.id.clone())
            || s.every_minutes < 0
            || (!s.is_push_only() && s.every_minutes == 0)
        {
            return Err("sensor registry identity or interval is ambiguous".into());
        }
        for (window, until) in [
            (&e.current, now),
            (&e.previous, now - chrono::Duration::hours(hours)),
        ] {
            let packets: std::collections::BTreeSet<_> = window.packets.iter().collect();
            if window.sensor_id != s.id
                || window.since != until - chrono::Duration::hours(hours)
                || window.until != until
                || window.stamped.checked_add(window.unstamped) != Some(window.arrived)
                || packets.len() != window.packets.len()
                || u64::try_from(packets.len())
                    .ok()
                    .is_none_or(|n| n > window.stamped)
            {
                return Err(format!(
                    "sensor {}: readings window or counts contradict the request",
                    s.id
                ));
            }
        }
        current = current
            .checked_add(e.current.arrived)
            .ok_or("sensor count overflow")?;
        previous = previous
            .checked_add(e.previous.arrived)
            .ok_or("sensor count overflow")?;
        let mut owed_ids = std::collections::BTreeSet::new();
        for reading in &e.owed {
            if reading.sensor_id != s.id
                || reading.packet_id.is_some()
                || reading.external_id.is_empty()
                || !owed_ids.insert(reading.external_id.clone())
                || reading.observed_at > now
            {
                return Err(format!("sensor {}: unstamped readings are ambiguous", s.id));
            }
            if !s.is_push_only()
                && reading.observed_at < now - chrono::Duration::minutes(i64::from(s.every_minutes))
            {
                findings.push(Finding::new(
                    bands::SENSOR_OVERDUE,
                    Some(reading.observed_at),
                    format!("older than {} minutes", s.every_minutes),
                    format!(
                        "sensor {}: unstamped reading {} is past its {} minute poll interval",
                        s.id, reading.external_id, s.every_minutes
                    ),
                ));
            }
        }
        if !e.alarms.is_empty() {
            findings.push(Finding::new(
                bands::SENSOR_UNREADABLE,
                None,
                format!("{} open findings", e.alarms.len()),
                format!(
                    "sensor {}: open sensor_unreadable findings {}",
                    s.id,
                    e.alarms.join(", ")
                ),
            ));
        }
    }
    let current = usize::try_from(current).map_err(|_| "sensor count exceeds this platform")?;
    let previous = usize::try_from(previous).map_err(|_| "sensor count exceeds this platform")?;
    Ok(region(
        "sensors",
        Some(rows.len()),
        None,
        "declared sensors",
        settle(
            findings,
            format!(
                "{} declared sensors; {} readings arrived in the window",
                rows.len(),
                current
            ),
            now,
        ),
        Trend {
            metric: "arriving readings".into(),
            unit: "readings".into(),
            current: count_value(current),
            previous: count_value(previous),
            samples: current,
            previous_samples: previous,
        },
        vec![measure("arriving", count_value(current), "readings")],
    ))
}

pub(crate) fn unread(now: Instant, why: &str) -> Region {
    region(
        "sensors",
        None,
        None,
        "declared sensors",
        unread_settled(why, now),
        Trend {
            metric: "arriving readings".into(),
            unit: "readings".into(),
            current: None,
            previous: None,
            samples: 0,
            previous_samples: 0,
        },
        vec![measure("arriving", None, "readings")],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(s: &str) -> Instant {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }
    fn now() -> Instant {
        at("2026-10-02T12:00:00Z")
    }
    fn sample() -> Evidence {
        let sensor = SensorRow {
            id: "inbox".into(),
            source: "mail".into(),
            credential: "shared-credential".into(),
            every_minutes: 5,
            opens_kind: "receive-message".into(),
            subject_kind: "message".into(),
            enabled: true,
            selector: Some("support@example.test".into()),
            tenant_id: "test".into(),
            published_at: now(),
            last_polled_at: None,
            cursor_at: None,
        };
        let window = |until| ReadingsWindow {
            sensor_id: "inbox".into(),
            since: until - chrono::Duration::hours(24),
            until,
            arrived: 3,
            stamped: 2,
            unstamped: 1,
            packets: vec!["a".into(), "b".into()],
        };
        Evidence {
            sensor,
            current: window(now()),
            previous: window(now() - chrono::Duration::hours(24)),
            owed: vec![],
            alarms: vec![],
        }
    }
    #[test]
    fn malformed_or_ambiguous_readings_refuse_the_whole_station() {
        let changes: [fn(&mut Evidence); 7] = [
            |e| e.current.sensor_id = "other".into(),
            |e| e.current.since += chrono::Duration::nanoseconds(1),
            |e| e.previous.until += chrono::Duration::seconds(1),
            |e| e.current.arrived = 4,
            |e| e.previous.packets.push("a".into()),
            |e| e.sensor.every_minutes = -1,
            |e| e.sensor.every_minutes = 0,
        ];
        for change in changes {
            let mut e = sample();
            change(&mut e);
            let r = judge(&[e], now(), 24);
            assert_eq!(r.count, None);
            assert_eq!(r.state, RegionState::Troubled);
        }
        assert_eq!(judge(&[sample(), sample()], now(), 24).count, None);
        let mut a = sample();
        let mut b = sample();
        b.sensor.id = "other".into();
        b.current.sensor_id = "other".into();
        b.previous.sensor_id = "other".into();
        a.current.arrived = u64::MAX;
        a.current.stamped = u64::MAX;
        a.current.unstamped = 0;
        assert_eq!(judge(&[a, b], now(), 24).count, None);
    }
    #[test]
    fn read_counts_and_existing_alarm_receipts_are_the_judgement() {
        let r = judge(&[sample()], now(), 24);
        assert_eq!(r.count, Some(1));
        assert_eq!(r.state, RegionState::Clear);
        assert_eq!(r.trend.current, Some(3.0));
        assert_eq!(r.trend.previous, Some(3.0));
        let mut e = sample();
        e.alarms = vec!["alarm-packet".into()];
        let r = judge(&[e], now(), 24);
        assert_eq!(r.state, RegionState::Troubled);
        assert!(r.why.contains("alarm-packet"));
        assert_eq!(r.band.unwrap().id, "sensor-unreadable");
        let r = judge(&[], now(), 24);
        assert_eq!(r.count, Some(0));
        assert_eq!(r.state, RegionState::Clear);
    }
}
