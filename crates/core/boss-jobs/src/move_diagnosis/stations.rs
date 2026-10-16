//! The car/run model consumes only the loading-dock predicate. Its historical
//! authority is a publication event, never today's row or a guessed birth date.
use super::HistoricalEvent;
use crate::{events, registry::WorkflowStatus, stations::StationSpec};
pub fn at(rows: &[HistoricalEvent], upto: i64) -> Result<Vec<StationSpec>, String> {
    let mut last = 0;
    let mut published = None;
    for row in rows {
        if row.seq <= last || row.seq > upto || row.event.source != "jobs" {
            return Err("station history order, cutoff or source mismatch".into());
        }
        last = row.seq;
        let spec: StationSpec =
            serde_json::from_value(row.event.payload.clone()).map_err(|e| e.to_string())?;
        if spec.name != "loading-dock" {
            return Err("station publication names another station".into());
        }
        match row.event.kind.as_str() {
            events::STATION_PUBLISHED if spec.status == WorkflowStatus::Active => {
                published = Some(spec)
            }
            events::STATION_RETIRED if spec.status == WorkflowStatus::Retired => published = None,
            _ => return Err("station publication kind/state mismatch".into()),
        }
    }
    published
        .map(|s| vec![s])
        .ok_or("original loading-dock publication unavailable".into())
}
